//! macOS AXUIElement-based accessibility provider.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use xa11y_core::handle_cache::{BoundedHandleCache, MAX_CACHED_ELEMENTS};

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use rayon::prelude::*;

#[cfg(test)]
use xa11y_core::Selector;
use xa11y_core::{
    CancelHandle, ElementData, ElementParts, Error, Event, EventKind, EventParts, HiddenReason,
    Provider, Rect, Result, Role, ShellSurfaceKind, StateFlag, StateParts, StateSet, Subscription,
    Toggled,
};

// ── FFI Declarations ──────────────────────────────────────────────────────────

type AXUIElementRef = *const c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFIndex = isize;

const AX_ERROR_SUCCESS: i32 = 0;
const AX_ERROR_INVALID_UI_ELEMENT: i32 = -25202;
const AX_ERROR_CANNOT_COMPLETE: i32 = -25204;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
const AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;
const AX_ERROR_NOT_IMPLEMENTED: i32 = -25208;
const AX_ERROR_NO_VALUE: i32 = -25212;
const AX_VALUE_CGPOINT: i32 = 1;
const AX_VALUE_CGSIZE: i32 = 2;
const CF_NUMBER_FLOAT64: i32 = 13;
const CF_NUMBER_SINT32: i32 = 3;
#[allow(dead_code)]
const CF_NUMBER_SINT64: i32 = 4;

#[repr(C)]
#[derive(Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Default)]
struct CGSize {
    width: f64,
    height: f64,
}

// All CF / AX interop in this file goes through the `safe_*` wrappers defined
// in `exception_safe.m`. Each wrapper runs its underlying call inside an
// Objective-C `@try`/`@catch` so a misbehaving AX value's `-release` /
// `-getTypeID` can't unwind through `extern "C"` frames and abort the
// process. Raw CF / AX symbols (CFRelease, CFRetain, CFGetTypeID,
// CFNumberGetValue, CFBooleanGetValue, CFArrayGetCount,
// CFArrayGetValueAtIndex, CFDictionaryGetValue, CFArrayCreate,
// CFStringGetTypeID, CFNumberGetTypeID, CFBooleanGetTypeID,
// CFArrayGetTypeID, AXIsProcessTrusted) are intentionally NOT declared here
// - if you need a new one, add a `safe_*` wrapper to `exception_safe.m`.
// Enforced by `cargo xtask check-macos-ffi`.
extern "C" {
    fn safe_ax_copy_attribute_value(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn safe_ax_copy_multiple_attribute_values(
        element: AXUIElementRef,
        attributes: CFArrayRef,
        values: *mut CFArrayRef,
    ) -> i32;
    fn safe_ax_copy_action_names(element: AXUIElementRef, names: *mut CFArrayRef) -> i32;
    fn safe_ax_perform_action(element: AXUIElementRef, action: CFStringRef) -> i32;
    fn safe_ax_set_attribute_value(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn safe_ax_is_attribute_settable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut bool,
    ) -> i32;
    fn safe_ax_create_application(pid: i32) -> AXUIElementRef;
    fn safe_ax_create_system_wide() -> AXUIElementRef;
    fn safe_ax_is_element(value: CFTypeRef) -> bool;
    fn safe_ax_get_pid(element: AXUIElementRef, out_pid: *mut i32) -> i32;
    fn safe_ax_set_messaging_timeout(element: AXUIElementRef, timeout_seconds: f32) -> i32;
    fn safe_ax_value_get_value(value: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
    fn safe_cg_window_list_copy(option: u32, relative_to: u32) -> CFArrayRef;
    fn safe_cg_preflight_screen_capture_access() -> bool;
    fn safe_ax_observer_create_with_info(
        pid: i32,
        callback: unsafe extern "C" fn(
            CFTypeRef,
            AXUIElementRef,
            CFTypeRef,
            CFTypeRef,
            *mut c_void,
        ),
        observer: *mut CFTypeRef,
    ) -> i32;
    fn safe_ax_observer_add_notification(
        observer: CFTypeRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> i32;
    fn safe_ax_observer_remove_notification(
        observer: CFTypeRef,
        element: AXUIElementRef,
        notification: CFStringRef,
    ) -> i32;
    fn safe_ax_observer_get_run_loop_source(observer: CFTypeRef) -> CFTypeRef;
    fn safe_cf_run_loop_add_source(source: CFTypeRef);
    fn safe_cf_run_loop_get_current() -> CFTypeRef;
    fn safe_cf_run_loop_run();
    fn safe_cf_run_loop_stop(run_loop: CFTypeRef);
    fn safe_ax_value_create_cf_range(location: isize, length: isize) -> CFTypeRef;
    fn safe_ax_value_create_cg_point(x: f64, y: f64) -> CFTypeRef;
    fn safe_ax_value_create_cg_size(width: f64, height: f64) -> CFTypeRef;

    // CoreFoundation helpers - all calls from ax.rs go through these.
    fn safe_cf_retain(cf: CFTypeRef) -> CFTypeRef;
    fn safe_cf_release(cf: CFTypeRef);
    fn safe_cf_get_type_id(cf: CFTypeRef) -> u64;
    fn safe_cf_array_get_count(arr: CFArrayRef) -> CFIndex;
    fn safe_cf_array_get_value(arr: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn safe_cf_boolean_get_value(b: CFTypeRef) -> bool;
    fn safe_cf_equal(a: CFTypeRef, b: CFTypeRef) -> bool;
    fn safe_cf_hash(cf: CFTypeRef) -> u64;
    fn safe_cf_number_get_value(num: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
    fn safe_cf_dict_get_value(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn safe_cf_array_create(values: *const CFTypeRef, num_values: CFIndex) -> CFArrayRef;
    fn safe_cf_string_get_type_id() -> u64;
    fn safe_cf_number_get_type_id() -> u64;
    fn safe_cf_boolean_get_type_id() -> u64;
    fn safe_cf_array_get_type_id() -> u64;
    fn safe_cf_url_get_type_id() -> u64;
    fn safe_cf_url_get_string(url: CFTypeRef) -> CFTypeRef;
    fn safe_cf_string_copy_utf16(value: CFTypeRef, buffer: *mut u16, capacity: CFIndex) -> CFIndex;

    fn safe_ax_is_process_trusted() -> bool;

    #[cfg(test)]
    fn test_throw_and_catch_nsexception() -> i32;
}

// ── AXElement RAII Wrapper ────────────────────────────────────────────────────

struct AXElement(AXUIElementRef);

unsafe impl Send for AXElement {}
unsafe impl Sync for AXElement {}

impl AXElement {
    fn from_owned(ptr: AXUIElementRef) -> Self {
        Self(ptr)
    }

    fn from_borrowed(ptr: AXUIElementRef) -> Self {
        if !ptr.is_null() {
            unsafe { safe_cf_retain(ptr) };
        }
        Self(ptr)
    }

    fn as_ptr(&self) -> AXUIElementRef {
        self.0
    }

    fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

/// The process's system-wide element, created once and never released.
///
/// [`ax_bounded`] must recognise this element: a messaging timeout set on it
/// is the *process-global* value, not a per-element one. Keeping a single
/// instance alive for the life of the process makes pointer equality a sound
/// identity for it (the pointer is never freed, so never reused), with no AX
/// or CF call on the hot path.
static SYSTEM_WIDE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// A retained reference to the process's system-wide element, or a NULL
/// [`AXElement`] when `AXUIElementCreateSystemWide` returned NULL (callers
/// check [`AXElement::is_null`], as they did for a fresh element).
fn system_wide_element() -> AXElement {
    if let Some(&ptr) = SYSTEM_WIDE.get() {
        return AXElement::from_borrowed(ptr as AXUIElementRef);
    }
    let created = unsafe { safe_ax_create_system_wide() };
    if created.is_null() {
        return AXElement::from_owned(created);
    }
    // The static keeps the create-rule reference. A thread that lost the
    // race releases its own and uses the winner's.
    let ptr = *SYSTEM_WIDE.get_or_init(|| created as usize);
    if ptr != created as usize {
        unsafe { safe_cf_release(created) };
    }
    AXElement::from_borrowed(ptr as AXUIElementRef)
}

/// Whether `element` is the process's system-wide element (see
/// [`system_wide_element`]).
fn is_system_wide(element: AXUIElementRef) -> bool {
    SYSTEM_WIDE.get().is_some_and(|&p| p == element as usize)
}

impl Clone for AXElement {
    fn clone(&self) -> Self {
        Self::from_borrowed(self.0)
    }
}

impl Drop for AXElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { safe_cf_release(self.0) };
        }
    }
}

/// An `AXUIElementRef` as an identity: `CFEqual` compares the underlying
/// object (two `AXChildren` reads vend different pointers for one element),
/// and `CFHash` is consistent with it.
#[derive(Debug)]
struct AxIdentity(AXElement);

impl std::fmt::Debug for AXElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AXElement({:p})", self.0)
    }
}

impl xa11y_core::NativeObject for AxIdentity {
    fn object_hash(&self) -> u64 {
        unsafe { safe_cf_hash(self.0.as_ptr() as CFTypeRef) }
    }
    fn same_object(&self, other: &dyn xa11y_core::NativeObject) -> bool {
        other
            .as_any()
            .downcast_ref::<AxIdentity>()
            .is_some_and(|o| unsafe {
                safe_cf_equal(self.0.as_ptr() as CFTypeRef, o.0.as_ptr() as CFTypeRef)
            })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Whether process `pid` still answers AX at all: its application element
/// reads `AXRole`. A dead process answers `kAXErrorInvalidUIElement` (or
/// cannot be messaged).
fn process_answers_ax(pid: i32) -> bool {
    let app = AXElement::from_owned(unsafe { safe_ax_create_application(pid) });
    if app.is_null() {
        return false;
    }
    match read_raw_attr(app.as_ptr(), "AXRole") {
        RawAttr::Value(v) => {
            unsafe { safe_cf_release(v) };
            true
        }
        RawAttr::Absent => true,
        RawAttr::Unanswered(code) => code != AX_ERROR_INVALID_UI_ELEMENT,
    }
}

mod rich;

// ── AX Call Counters (test-only) ──────────────────────────────────────────────

/// Atomic counters tracking AX IPC calls. Only compiled in test builds.
/// Used by integration tests to assert that selector optimizations don't
/// regress — call counts should only go down over time.
#[cfg(test)]
pub mod ax_counters {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Individual attribute fetch (AXUIElementCopyAttributeValue).
    pub static COPY_ATTR: AtomicU64 = AtomicU64::new(0);
    /// Batch attribute fetch (AXUIElementCopyMultipleAttributeValues).
    pub static COPY_MULTI_ATTR: AtomicU64 = AtomicU64::new(0);
    /// Action names fetch (AXUIElementCopyActionNames).
    pub static COPY_ACTIONS: AtomicU64 = AtomicU64::new(0);

    /// Serializes counter-based tests (global counters are shared state).
    pub static LOCK: Mutex<()> = Mutex::new(());

    pub fn reset_all() {
        COPY_ATTR.store(0, Ordering::SeqCst);
        COPY_MULTI_ATTR.store(0, Ordering::SeqCst);
        COPY_ACTIONS.store(0, Ordering::SeqCst);
    }

    pub fn total() -> u64 {
        COPY_ATTR.load(Ordering::SeqCst)
            + COPY_MULTI_ATTR.load(Ordering::SeqCst)
            + COPY_ACTIONS.load(Ordering::SeqCst)
    }

    pub fn snapshot() -> (u64, u64, u64) {
        (
            COPY_ATTR.load(Ordering::SeqCst),
            COPY_MULTI_ATTR.load(Ordering::SeqCst),
            COPY_ACTIONS.load(Ordering::SeqCst),
        )
    }
}

#[cfg(test)]
mod grant_instruction_tests {
    use super::instructions_for_major;

    #[test]
    fn the_grant_instruction_names_the_correct_settings_entry_per_version() {
        // macOS 27 renamed the entry under Privacy & Security: Accessibility
        // is gone as its own entry, and the grant lives in Device Control and
        // Data Access. The instruction a user follows must match the pane
        // they see.
        let old = instructions_for_major(26);
        assert!(
            old.contains("Privacy & Security → Accessibility"),
            "pre-27 must name the old entry: {old}"
        );

        for new in [27u32, 28, 99] {
            let next = instructions_for_major(new);
            assert!(
                next.contains("Privacy & Security → Device Control and Data Access"),
                "macOS {new} must name the new entry: {next}"
            );
            assert!(
                !next.contains("→ Accessibility"),
                "macOS {new} must not name the removed entry: {next}"
            );
        }
    }
}

// ── Deadline-bounded AX messages (#531) ─────────────────────────────────────
//
// Every AX message waits up to the process-wide messaging timeout
// ([`AX_MESSAGING_TIMEOUT_SECS`]), so a read made late in a 1 s wait could
// still block for 2 s. [`ax_bounded`] lowers the timeout on the element the
// message goes to, to the time left on the caller's open
// [`Deadline`](xa11y_core::Deadline), for that one message, then releases it.
// With no deadline open, or one further away than the element's own bound,
// the message is sent exactly as before.
//
// `AXUIElementSetMessagingTimeout` is state on the element *object*, and the
// handle cache hands every thread the same object for one handle. Unguarded,
// one thread's tight bound would cut another thread's unrelated read of that
// element, and one thread's release would unbind a message another thread
// had just bounded. So every message to an element object other than the
// system-wide one holds that object's [`ElementGate`] for the whole
// set / send / release sequence, and a bound can never be observed by a
// message it was not set for.

thread_local! {
    /// The element whose messaging timeout a shell-surface probe has bounded
    /// on this thread, and that bound in seconds (see [`ShellProbeBound`]).
    /// [`ax_bounded`] releases an element back to this bound rather than to
    /// the global one, so a deadline does not undo the probe's tighter
    /// bound.
    static PROBE_BOUND: std::cell::Cell<Option<(usize, f32)>> =
        const { std::cell::Cell::new(None) };
}

/// The messaging timeout `element` has when no deadline lowers it.
fn element_bound(element: AXUIElementRef) -> f32 {
    match PROBE_BOUND.with(|b| b.get()) {
        Some((probed, secs)) if probed == element as usize => secs,
        _ => AX_MESSAGING_TIMEOUT_SECS,
    }
}

/// An AX message as a deadline diagnosis names it: what was asked and the
/// application it was asked of. The pid comes from the element itself, with
/// no IPC to the application that did not answer.
fn describe_ax_call(element: AXUIElementRef, call: &str) -> String {
    let mut pid = 0;
    if !element.is_null()
        && unsafe { safe_ax_get_pid(element, &mut pid) } == AX_ERROR_SUCCESS
        && pid > 0
    {
        format!("{call} on an element of the application with pid {pid}")
    } else {
        format!("{call} on an element of an unknown application")
    }
}

/// Mutual exclusion for the messages sent to one AX element object (see the
/// section comment above). Keyed by the object's address: a pointer is only
/// reused once its object is freed, and a stale key can at worst make two
/// unrelated objects share a gate for a moment, which costs a wait and never
/// a wrong bound.
struct ElementGate {
    busy: Mutex<bool>,
    freed: std::sync::Condvar,
}

/// The live gates. An entry is removed when the last holder or waiter lets
/// go of it, so the map holds only elements a message is in flight to.
fn element_gates() -> &'static Mutex<HashMap<usize, std::sync::Arc<ElementGate>>> {
    static GATES: std::sync::OnceLock<Mutex<HashMap<usize, std::sync::Arc<ElementGate>>>> =
        std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A claim on an [`ElementGate`]: held (`held == true`) or given up after a
/// timed-out wait. Either way dropping it removes the map entry once nobody
/// else holds or waits on the gate.
struct ElementGateGuard {
    key: usize,
    gate: std::sync::Arc<ElementGate>,
    held: bool,
}

impl ElementGateGuard {
    /// Wait for `element`'s gate and hold it. `limit` caps the wait (the
    /// time left on the caller's deadline); `None` waits until the holder
    /// finishes, which its own messaging bound limits. Returns the unheld
    /// claim when the limit ran out first.
    fn acquire(
        element: AXUIElementRef,
        limit: Option<Duration>,
    ) -> std::result::Result<Self, Self> {
        let key = element as usize;
        let gate = element_gates()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_insert_with(|| {
                std::sync::Arc::new(ElementGate {
                    busy: Mutex::new(false),
                    freed: std::sync::Condvar::new(),
                })
            })
            .clone();
        let mut claim = Self {
            key,
            gate,
            held: false,
        };
        let gate = claim.gate.clone();
        let start = Instant::now();
        let mut busy = gate.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            busy = match limit {
                None => gate.freed.wait(busy).unwrap_or_else(|e| e.into_inner()),
                Some(limit) => {
                    let left = limit.saturating_sub(start.elapsed());
                    if left.is_zero() {
                        drop(busy);
                        return Err(claim);
                    }
                    gate.freed
                        .wait_timeout(busy, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
        *busy = true;
        drop(busy);
        claim.held = true;
        Ok(claim)
    }
}

impl Drop for ElementGateGuard {
    fn drop(&mut self) {
        if self.held {
            *self.gate.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
            // Every waiter re-checks; one that timed out meanwhile must not
            // swallow the only wake-up.
            self.gate.freed.notify_all();
        }
        let mut gates = element_gates().lock().unwrap_or_else(|e| e.into_inner());
        // The map's reference and this one: nobody else holds or waits.
        if std::sync::Arc::strong_count(&self.gate) == 2
            && gates
                .get(&self.key)
                .is_some_and(|g| std::sync::Arc::ptr_eq(g, &self.gate))
        {
            gates.remove(&self.key);
        }
    }
}

/// Send one AX message to `element`, bounded by the caller's deadline.
///
/// `call` names the message for the diagnosis and is built only when the
/// deadline cuts it. When the deadline has already passed the message is not
/// sent and the result is `kAXErrorCannotComplete`, the code an expired
/// messaging timeout returns, so every call site's existing handling still
/// applies. A cut message is also reported to the deadline, which is how
/// core names it as pending whatever error the call site builds from it.
fn ax_bounded(
    element: AXUIElementRef,
    call: impl Fn() -> String,
    send: impl FnOnce() -> i32,
) -> i32 {
    let bound = element_bound(element);
    // A messaging timeout set on the system-wide element is the process's
    // *global* value, not a per-element one: lowering it would cut every
    // other thread's AX message to this caller's deadline, and the release
    // below (`0`) would reset the global to the system default rather than
    // the bound `set_global_messaging_timeout` installed. So a message to the
    // system-wide element runs at the global bound even under a nearer
    // deadline; it can overrun the deadline by at most that bound. Nothing
    // here sets a bound on it, so it needs no gate either.
    if is_system_wide(element) || element.is_null() {
        return match xa11y_core::call_timeout(Duration::from_secs_f32(bound), || {
            describe_ax_call(element, &call())
        }) {
            Ok(_) => send(),
            // The deadline passed before the message was sent, and recorded it.
            Err(_) => AX_ERROR_CANNOT_COMPLETE,
        };
    }
    // Hold the element's gate for the whole set / send / release, so no
    // other thread's message runs under this one's bound (or this one under
    // theirs). Waiting for it counts against the caller's deadline.
    let wait = xa11y_core::Deadline::current().map(|d| d.remaining());
    let _gate = match ElementGateGuard::acquire(element, wait) {
        Ok(g) => g,
        Err(_) => {
            if let Some(d) = xa11y_core::Deadline::current() {
                // Recording the cut is the point; the error is built again
                // by core with the wait's own context.
                let _ = d.expired(Some(format!(
                    "waiting for another thread's message to finish before {}",
                    describe_ax_call(element, &call())
                )));
            }
            return AX_ERROR_CANNOT_COMPLETE;
        }
    };
    let timeout = match xa11y_core::call_timeout(Duration::from_secs_f32(bound), || {
        describe_ax_call(element, &call())
    }) {
        Ok(t) => t,
        // The deadline passed before the message was sent, and recorded it.
        Err(_) => return AX_ERROR_CANNOT_COMPLETE,
    };
    if !timeout.is_deadline() {
        return send();
    }
    // `call_timeout` never hands back a zero duration, which AX would read
    // as "use the global value".
    let set = unsafe { safe_ax_set_messaging_timeout(element, timeout.duration.as_secs_f32()) };
    if set != AX_ERROR_SUCCESS {
        // An element whose cost cannot be bounded is not one this message
        // can be sent to inside the caller's deadline. The code is what AX
        // returned for the element, so the call site reports it as such.
        return set;
    }
    let code = send();
    // Release the element to the bound it had: `0` means "use the global
    // value". The result does not matter for this message, which has
    // already been answered; a failure leaves the element on the tighter
    // bound, which is a faster failure for a later one, never a wrong answer.
    let release = if bound == AX_MESSAGING_TIMEOUT_SECS {
        0.0
    } else {
        bound
    };
    let _err = unsafe { safe_ax_set_messaging_timeout(element, release) };
    // The deadline set the bound this message ran under, so a
    // `kAXErrorCannotComplete` is the deadline cutting it. Recorded without
    // re-checking `is_expired`: AX takes the bound as an `f32`, and rounding
    // can return the timeout a moment before the deadline reads as passed,
    // which would lose the cut. This mirrors the AT-SPI backend.
    if code == AX_ERROR_CANNOT_COMPLETE {
        // Recording the cut is the point of this call; the error it returns
        // is built again by core with the wait's own context.
        let _ = timeout.expired(describe_ax_call(element, &call()));
    }
    code
}

/// The name of an attribute passed as a `CFStringRef`, for a diagnosis.
fn attribute_name(attribute: CFStringRef) -> String {
    if attribute.is_null() {
        return "an attribute".to_string();
    }
    cf_string(attribute).unwrap_or_else(|| "an attribute that is not a string".to_string())
}

/// Wrap a rayon closure so each item runs under the caller's deadline:
/// the deadline is thread-local, and rayon runs items on its own threads.
fn carry_deadline<T, R>(f: impl Fn(T) -> R + Sync + Send) -> impl Fn(T) -> R + Sync + Send {
    let deadline = xa11y_core::Deadline::current();
    move |item| xa11y_core::Deadline::scope_opt(deadline.clone(), || f(item))
}

// ── FFI Wrappers (instrumented in test builds) ──────────────────────────────

#[inline(always)]
fn ffi_copy_attribute_value(
    element: AXUIElementRef,
    attribute: CFStringRef,
    value: *mut CFTypeRef,
) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_ATTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ax_bounded(
        element,
        || format!("reading {}", attribute_name(attribute)),
        || unsafe { safe_ax_copy_attribute_value(element, attribute, value) },
    )
}

#[inline(always)]
fn ffi_copy_multiple_attribute_values(
    element: AXUIElementRef,
    attributes: CFArrayRef,
    values: *mut CFArrayRef,
) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_MULTI_ATTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ax_bounded(
        element,
        || "reading an element's attributes".to_string(),
        || unsafe { safe_ax_copy_multiple_attribute_values(element, attributes, values) },
    )
}

#[inline(always)]
fn ffi_copy_action_names(element: AXUIElementRef, names: *mut CFArrayRef) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_ACTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ax_bounded(
        element,
        || "reading an element's actions".to_string(),
        || unsafe { safe_ax_copy_action_names(element, names) },
    )
}

// ── Attribute Helpers ─────────────────────────────────────────────────────────

/// A borrowed CFString's UTF-16 code units. `None` for null or any other
/// type. Read inside `safe_cf_string_copy_utf16`'s `@try`, so a misbehaving
/// object cannot unwind into Rust, and with no retain or release of `v`:
/// `core-foundation`'s `CFString::wrap_under_*` would `CFRetain` /
/// `CFRelease` it outside the exception-safe wrappers.
fn cf_utf16(v: CFTypeRef) -> Option<Vec<u16>> {
    let n = unsafe { safe_cf_string_copy_utf16(v, std::ptr::null_mut(), 0) };
    if n < 0 {
        return None;
    }
    let mut buf = vec![0u16; n as usize];
    let copied = unsafe { safe_cf_string_copy_utf16(v, buf.as_mut_ptr(), n) };
    (copied == n).then_some(buf)
}

/// A borrowed CFString as a `String` (lone surrogates replaced, as
/// `CFString`'s `Display` does). `None` for null or any other type.
fn cf_string(v: CFTypeRef) -> Option<String> {
    cf_utf16(v).map(|u| String::from_utf16_lossy(&u))
}

/// An owned (+1) CF value as a `String`, releasing it whatever its type.
fn cf_string_owned(v: CFTypeRef) -> Option<String> {
    let s = cf_string(v);
    if !v.is_null() {
        unsafe { safe_cf_release(v) };
    }
    s
}

fn ax_attr(element: AXUIElementRef, attribute: &str) -> Option<CFTypeRef> {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value);
    if err == AX_ERROR_SUCCESS && !value.is_null() {
        Some(value)
    } else {
        None
    }
}

/// The string of a borrowed `CFURL` value, or `None` for anything else.
fn cf_url_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() {
        return None;
    }
    unsafe {
        if safe_cf_get_type_id(v) != safe_cf_url_get_type_id() {
            return None;
        }
        let s = safe_cf_url_get_string(v);
        if s.is_null() || safe_cf_get_type_id(s) != safe_cf_string_get_type_id() {
            return None;
        }
        // Borrowed from the URL (Get rule).
        cf_string(s)
    }
}

fn ax_string(element: AXUIElementRef, attribute: &str) -> Option<String> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_string_get_type_id() {
            cf_string_owned(value)
        } else {
            safe_cf_release(value);
            None
        }
    }
}

/// The accessible name: a non-empty `AXTitle`, else a static text's non-empty
/// `AXValue`, else `AXDescription`. An empty string is no name: Chromium
/// answers "" on its text leaves, and a Calendar event is a static text with
/// no `AXValue` whose title is its `AXDescription`.
fn accessible_name(
    role: Option<Role>,
    title: Option<String>,
    value: impl FnOnce() -> Option<String>,
    description: impl FnOnce() -> Option<String>,
) -> Option<String> {
    title
        .filter(|t| !t.is_empty())
        .or_else(|| {
            (role == Some(Role::StaticText))
                .then(value)
                .flatten()
                .filter(|v| !v.is_empty())
        })
        .or_else(description)
}

/// The accessible description: `AXHelp`, else `AXDescription` when the name
/// did not take it.
fn accessible_description(
    name: Option<&String>,
    description: Option<String>,
    help: Option<String>,
) -> Option<String> {
    help.or_else(|| description.filter(|d| name != Some(d)))
}

/// The accessible name of `ax` as `build_snapshot_data` derives it. The
/// fast-path matcher and the window-chrome filter use this so they agree with
/// the snapshot walk.
fn derived_ax_name(ax: AXUIElementRef, role: Option<Role>) -> Option<String> {
    accessible_name(
        role,
        ax_string(ax, "AXTitle"),
        || ax_value_string(ax),
        || ax_string(ax, "AXDescription"),
    )
}

/// Whether `target` names itself from a non-empty `AXTitle`. A static text
/// that does keeps a title-derived name, so a value change there is only a
/// value change; an empty `AXTitle` (Chromium) is no title, and the name
/// comes from `AXValue`.
fn has_title_derived_name(target: Option<&ElementData>) -> bool {
    target.is_some_and(|t| {
        t.raw
            .get("AXTitle")
            .and_then(|v| v.as_str())
            .is_some_and(|title| !title.is_empty())
    })
}

fn ax_bool(element: AXUIElementRef, attribute: &str) -> Option<bool> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_boolean_get_type_id() {
            let b = safe_cf_boolean_get_value(value);
            safe_cf_release(value);
            Some(b)
        } else {
            safe_cf_release(value);
            None
        }
    }
}

/// Whether `window` is the owning process's focused (key) window.
fn focused_window_is(window: AXUIElementRef, pid: u32) -> bool {
    let system = system_wide_element();
    if system.is_null() {
        return false;
    }
    let focused_app_attribute = CFString::new("AXFocusedApplication");
    let mut focused_app: CFTypeRef = std::ptr::null();
    if ffi_copy_attribute_value(
        system.as_ptr(),
        focused_app_attribute.as_concrete_TypeRef() as CFTypeRef,
        &mut focused_app,
    ) != AX_ERROR_SUCCESS
        || focused_app.is_null()
    {
        return false;
    }
    let frontmost_app = AXElement::from_owned(focused_app as AXUIElementRef);
    let mut focused_pid = 0;
    if unsafe { safe_ax_get_pid(frontmost_app.as_ptr(), &mut focused_pid) } != AX_ERROR_SUCCESS
        || focused_pid != pid as i32
    {
        return false;
    }
    // Query the canonical per-process application element. The object
    // returned by AXFocusedApplication identifies the frontmost process, but
    // some accessibility bridges do not vend its application attributes on
    // that particular proxy. AXUIElementCreateApplication is the stable root
    // the rest of this provider uses for AXWindows and app-level attributes.
    let app = AXElement::from_owned(unsafe { safe_ax_create_application(focused_pid) });
    if app.is_null() {
        return false;
    }
    let attribute = CFString::new("AXFocusedWindow");
    let mut focused: CFTypeRef = std::ptr::null();
    let err = ffi_copy_attribute_value(
        app.as_ptr(),
        attribute.as_concrete_TypeRef() as CFTypeRef,
        &mut focused,
    );
    if err != AX_ERROR_SUCCESS || focused.is_null() {
        return false;
    }
    let focused = AXElement::from_owned(focused as AXUIElementRef);
    unsafe { safe_cf_equal(window as CFTypeRef, focused.as_ptr() as CFTypeRef) }
}

fn ax_number_f64(element: AXUIElementRef, attribute: &str) -> Option<f64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        let type_id = safe_cf_get_type_id(value);
        // Each branch owns the CFRelease of `value` exactly once: the number
        // path releases explicitly after the copy; the string path releases
        // through `cf_string_owned`; the fall-through releases before
        // returning `None`. The
        // previous control flow released in the number branch and then read
        // `value` again in the string branch (use-after-free) and released
        // it a second time on the fall-through path (double-release).
        if type_id == safe_cf_number_get_type_id() {
            let mut result: f64 = 0.0;
            let ok = safe_cf_number_get_value(
                value,
                CF_NUMBER_FLOAT64,
                &mut result as *mut _ as *mut c_void,
            );
            safe_cf_release(value);
            return if ok { Some(result) } else { None };
        }
        if type_id == safe_cf_string_get_type_id() {
            // `cf_string_owned` releases the +1 copy.
            return cf_string_owned(value)?.trim().parse::<f64>().ok();
        }
        safe_cf_release(value);
        None
    }
}

#[allow(dead_code)]
fn ax_number_i32(element: AXUIElementRef, attribute: &str) -> Option<i32> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_number_get_type_id() {
            let mut result: i32 = 0;
            let ok = safe_cf_number_get_value(
                value,
                CF_NUMBER_SINT32,
                &mut result as *mut _ as *mut c_void,
            );
            safe_cf_release(value);
            if ok {
                Some(result)
            } else {
                None
            }
        } else {
            safe_cf_release(value);
            None
        }
    }
}

#[allow(dead_code)]
fn ax_number_i64(element: AXUIElementRef, attribute: &str) -> Option<i64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_number_get_type_id() {
            let mut result: i64 = 0;
            let ok = safe_cf_number_get_value(
                value,
                CF_NUMBER_SINT64,
                &mut result as *mut _ as *mut c_void,
            );
            safe_cf_release(value);
            if ok {
                Some(result)
            } else {
                None
            }
        } else {
            safe_cf_release(value);
            None
        }
    }
}

fn ax_element_array(element: AXUIElementRef, attribute: &str) -> Vec<AXElement> {
    let value = match ax_attr(element, attribute) {
        Some(v) => v,
        None => return vec![],
    };
    unsafe {
        if safe_cf_get_type_id(value) != safe_cf_array_get_type_id() {
            safe_cf_release(value);
            return vec![];
        }
        let count = safe_cf_array_get_count(value);
        let mut children = Vec::with_capacity(count as usize);
        for i in 0..count {
            let child = safe_cf_array_get_value(value, i);
            if !child.is_null() {
                children.push(AXElement::from_borrowed(child));
            }
        }
        safe_cf_release(value);
        children
    }
}

fn ax_parent(element: AXUIElementRef) -> Option<AXElement> {
    let value = ax_attr(element, "AXParent")?;
    // AXParent returns an AXUIElement, which we own via copy attribute
    Some(AXElement::from_owned(value as AXUIElementRef))
}

/// Outcome of reading an element-valued AX attribute from a process that may
/// not answer.
///
/// `ax_attr` collapses every AXError into `None`, which is fine for the tree
/// walk (the element is right there) but not for the shell-surface scan: that
/// fans out over every GUI process, and "this app has no status items" and
/// "this app never answered" arrive as the same missing value under different
/// AXError codes. Keeping them apart is tenet 1 — a wedged process is skipped
/// for a reason the code names, never folded into "no match".
enum ElementProbe {
    /// The attribute holds an element.
    Found(AXElement),
    /// The process answered, and the attribute is absent, unsupported, or
    /// NULL. This is the common case for `AXExtrasMenuBar`: most apps vend no
    /// status items at all.
    Absent,
    /// The process did not answer within its messaging timeout, or the
    /// element / process is gone. Carries the AXError code so a caller that
    /// treats this as a failure can report which.
    Unanswered(i32),
}

fn ax_children(element: AXUIElementRef) -> Vec<AXElement> {
    ax_element_array(element, "AXChildren")
}

/// Outcome of reading an AX attribute containing one or more accessibility
/// elements.
enum ElementListProbe {
    Found(Vec<AXElement>),
    Absent,
    Unanswered,
}

/// Read an attribute whose value is an `AXUIElement`, distinguishing "absent"
/// from "unanswered" by AXError code.
///
/// The returned element is owned: `AXUIElementCopyAttributeValue` hands back a
/// +1-retained value, and [`AXElement::from_owned`] adopts that reference, the
/// same rule [`ax_parent`] follows for `AXParent`.
fn probe_element_attr(element: AXUIElementRef, attribute: &str) -> ElementProbe {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value);
    match err {
        AX_ERROR_SUCCESS => {
            if value.is_null() {
                ElementProbe::Absent
            } else {
                ElementProbe::Found(AXElement::from_owned(value as AXUIElementRef))
            }
        }
        // The process answered and said it has no such attribute / no value.
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => ElementProbe::Absent,
        // Everything else — kAXErrorCannotComplete (the code a messaging
        // timeout surfaces as), kAXErrorInvalidUIElement, an ObjC exception
        // caught by the wrapper — means we did not get an answer.
        _ => ElementProbe::Unanswered(err),
    }
}

/// Read an AX attribute containing an accessibility element or an array of
/// accessibility elements.
///
/// AppKit's `AXShownMenu` is element-valued, while the Carbon
/// `AXShownMenuUIElement` contract is array-valued. Each returned value is
/// owned independently of the copied attribute value.
fn probe_element_list_attr(element: AXUIElementRef, attribute: &str) -> ElementListProbe {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value);
    match err {
        AX_ERROR_SUCCESS => {
            if value.is_null() {
                return ElementListProbe::Absent;
            }
            if unsafe { safe_cf_get_type_id(value) } != unsafe { safe_cf_array_get_type_id() } {
                return ElementListProbe::Found(vec![AXElement::from_owned(
                    value as AXUIElementRef,
                )]);
            }
            let elements = unsafe {
                let count = safe_cf_array_get_count(value);
                let mut elements = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let element = safe_cf_array_get_value(value, i);
                    if !element.is_null() {
                        elements.push(AXElement::from_borrowed(element));
                    }
                }
                safe_cf_release(value);
                elements
            };
            if elements.is_empty() {
                ElementListProbe::Absent
            } else {
                ElementListProbe::Found(elements)
            }
        }
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => ElementListProbe::Absent,
        _ => ElementListProbe::Unanswered,
    }
}

/// Outcome of reading a raw CFTypeRef-valued AX attribute, preserving the
/// AXError code so a caller can distinguish "the element has no such
/// attribute" from "the process did not answer".
enum RawAttr {
    /// The copy succeeded and returned a value, owned (+1 retained).
    Value(CFTypeRef),
    /// The process answered, and the attribute is absent, unsupported, NULL,
    /// or null on success. No value is owned.
    Absent,
    /// The process did not answer within its messaging timeout, or the
    /// element / process is gone. Carries the AXError code.
    Unanswered(i32),
}

/// Read an attribute, preserving its AXError. The value is returned raw so a
/// caller can type-check before casting — `AXUIElementCopyAttributeValue` can
/// hand back `kCFNull` (or another non-element) with `AX_ERROR_SUCCESS`, and
/// casting that to `AXUIElementRef` is undefined behavior (see `close`).
fn read_raw_attr(element: AXUIElementRef, attribute: &str) -> RawAttr {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value);
    match err {
        AX_ERROR_SUCCESS => {
            if value.is_null() {
                RawAttr::Absent
            } else {
                RawAttr::Value(value)
            }
        }
        // The process answered and said it has no such attribute / no value.
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => RawAttr::Absent,
        // Everything else — kAXErrorCannotComplete (the code a messaging
        // timeout surfaces as), kAXErrorInvalidUIElement, an ObjC exception
        // caught by the wrapper — means we did not get an answer.
        _ => RawAttr::Unanswered(err),
    }
}

/// Top-level windows of the application (`AXWindows` attribute), in window
/// order. Layout mirrors [`ax_children`]: the attribute value is owned and
/// released here, and each element is retained via `from_borrowed`.
///
/// Deliberately does **not** go through [`ax_attr`], which collapses every
/// `AXError` into `None`: `App::windows()` on a specific application must
/// distinguish "this app has no windows" (`AX_ERROR_ATTRIBUTE_UNSUPPORTED` /
/// `AX_ERROR_NO_VALUE` → `Ok(vec![])`) from "the app did not answer"
/// (`kAXErrorCannotComplete`, a dead element, ...) — the latter is a real
/// platform failure and propagates (tenet 1), never an empty success that
/// reads as "no windows".
fn ax_windows(element: AXUIElementRef) -> Result<Vec<AXElement>> {
    ax_element_list_strict(element, "AXWindows")
}

/// The top-level children of an application element: its `AXWindows`, then
/// any app-level `AXMenu` from `AXChildren` that is not one of them.
///
/// An open context menu is an `AXMenu` child of the application, never an
/// entry in `AXWindows`. Search walks `AXChildren`, so a listing built from
/// `AXWindows` alone missed a menu that a selector found (issue #518). The
/// menu bar stays out, as it does for search (`should_filter_child_with_role`).
/// Both reads are error-preserving, like [`ax_windows`]: a child that vanished
/// between the reads is dropped, any other unreadable role is an error.
fn app_top_level_children(app: AXUIElementRef) -> Result<Vec<AXElement>> {
    let mut out = ax_windows(app)?;
    let windows = out.len();
    for child in ax_element_list_strict(app, "AXChildren")? {
        let is_menu = match read_raw_attr(child.as_ptr(), "AXRole") {
            RawAttr::Value(v) => {
                if unsafe { safe_cf_get_type_id(v) } == unsafe { safe_cf_string_get_type_id() } {
                    // `cf_string_owned` releases the +1 copy.
                    cf_string_owned(v).as_deref() == Some("AXMenu")
                } else {
                    // Not a role string, so not a menu; release the copy.
                    unsafe { safe_cf_release(v) };
                    false
                }
            }
            RawAttr::Absent => false,
            RawAttr::Unanswered(code) => match classify_element_churn_code(i64::from(code)) {
                ElementChurn::Gone => false,
                ElementChurn::Unreachable | ElementChurn::Fatal => {
                    return Err(Error::platform(
                        i64::from(code),
                        format!(
                            "AXRole read of an application child failed (AXError {code}); \
                             cannot tell whether it is a menu"
                        ),
                    ))
                }
            },
        };
        let already_listed = out[..windows].iter().any(|w| unsafe {
            safe_cf_equal(w.as_ptr() as CFTypeRef, child.as_ptr() as CFTypeRef)
        });
        if is_menu && !already_listed {
            out.push(child);
        }
    }
    Ok(out)
}

/// Read an attribute holding an array of accessibility elements, preserving
/// its AXError: absent / no value is an empty list, anything unanswered or
/// malformed is an error. Each element is retained via `from_borrowed`.
fn ax_element_list_strict(element: AXUIElementRef, attribute: &str) -> Result<Vec<AXElement>> {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err =
        ffi_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value);
    match err {
        AX_ERROR_SUCCESS => {
            if value.is_null() {
                return Ok(vec![]);
            }
            unsafe {
                if safe_cf_get_type_id(value) != safe_cf_array_get_type_id() {
                    safe_cf_release(value);
                    return Err(Error::platform(
                        -1,
                        format!(
                            "{attribute} returned a non-array value; the provider answered \
                             malformed, not empty"
                        ),
                    ));
                }
                let count = safe_cf_array_get_count(value);
                let mut windows = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let w = safe_cf_array_get_value(value, i);
                    // AX calls on a non-element are undefined, and a dropped
                    // item would read as a shorter list (tenet 1).
                    if !safe_ax_is_element(w) {
                        safe_cf_release(value);
                        return Err(Error::platform(
                            -1,
                            format!(
                                "{attribute} item {i} is not an AXUIElement; the provider \
                                 answered malformed"
                            ),
                        ));
                    }
                    windows.push(AXElement::from_borrowed(w));
                }
                safe_cf_release(value);
                Ok(windows)
            }
        }
        // The process answered and said it has no such attribute / no value:
        // e.g. an application without windows, honestly an empty list.
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(vec![]),
        // Everything else — kAXErrorCannotComplete (the code a messaging
        // timeout surfaces as), kAXErrorInvalidUIElement, an ObjC exception
        // caught by the wrapper — means we did not get an answer.
        code => Err(Error::platform(
            code as i64,
            format!("AXUIElementCopyAttributeValue({attribute}) failed (AXError {code})"),
        )),
    }
}

/// What a caller does with a per-element AX read failure.
///
/// The *classification* is shared — a new AX code is decided once, here — and
/// each caller's action follows from its contract, matched at the call site:
///
/// - `Gone`: the object was invalidated (`kAXErrorInvalidUIElement`) — a
///   window closed, or one recreated during a fullscreen transition. It is
///   not part of any surface any more, so an enumeration drops it (that is
///   what keeps a listing or a selector walk from failing wholesale on
///   churn), a retrying sample re-reads, and the retry callers read it as
///   "not reachable right now" too.
/// - `Unreachable`: the process did not answer *right now*
///   (`kAXErrorCannotComplete`, `kAXErrorNotImplemented`) — still launching,
///   busy, or without an accessibility bridge. The answer is unknown, not
///   absent: a discovery listing may skip the process and re-enumerate, while
///   a content walk fails on it, because a silently smaller tree is
///   indistinguishable from a genuine one (tenet 1).
/// - `Fatal`: every other AXError, and every non-platform error, is a real
///   failure and propagates with its original code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementChurn {
    Gone,
    Unreachable,
    Fatal,
}

/// Classify an [`Error`] for the callers above.
fn classify_element_churn(err: &Error) -> ElementChurn {
    match err {
        Error::Platform { code, .. } => classify_element_churn_code(*code),
        _ => ElementChurn::Fatal,
    }
}

/// The raw-code form for callers that still hold the `AXError` rather than an
/// [`Error::Platform`] (`ElementProbe::Unanswered`, the attach probe): one
/// list, so the two shapes cannot drift. A caller that also reads another
/// code as "not reachable yet" — the attach probe's `kAXErrorNoValue` — adds
/// it at the call site instead of forking the list.
fn classify_element_churn_code(code: i64) -> ElementChurn {
    if code == AX_ERROR_INVALID_UI_ELEMENT as i64 {
        ElementChurn::Gone
    } else if code == AX_ERROR_CANNOT_COMPLETE as i64 || code == AX_ERROR_NOT_IMPLEMENTED as i64 {
        ElementChurn::Unreachable
    } else {
        ElementChurn::Fatal
    }
}

/// Fold [`ElementChurn::Gone`] into `Ok(None)` for a read whose caller
/// retries the whole sample (see `poll_target`): an invalidated object is
/// churn, not a state. Every other failure — a wedged
/// app (`Unreachable`), a malformed answer — stays a real platform error and
/// propagates (tenet 1).
fn tolerate_churn<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) => match classify_element_churn(&err) {
            ElementChurn::Gone => Ok(None),
            ElementChurn::Unreachable | ElementChurn::Fatal => Err(err),
        },
    }
}

/// Drop repeat sightings of the same accessibility object from `items`,
/// keeping order, then apply `limit`. Each item carries its walk position
/// (ignored here), its `AXUIElementRef`, and its payload.
///
/// Pointer identity is not element identity: every `AXChildren` read vends
/// fresh `AXUIElementRef`s, so an element reached along two paths (`group
/// button` with nested groups finds the button under each group) arrives as
/// two different pointers. `CFEqual` compares the underlying object, and
/// `CFHash` is consistent with it, so hashing buckets the candidates and
/// keeps this linear for distinct results. Each item owns a retained
/// reference, so the pointers compared stay valid for the whole call — the
/// handle cache is bounded and evicts, so it cannot be what keeps them alive.
fn dedup_by_ax_identity<T>(items: Vec<(usize, AXElement, T)>, limit: Option<usize>) -> Vec<T> {
    let mut buckets: HashMap<u64, Vec<AXElement>> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    for (_, ax, item) in items {
        let ptr = ax.as_ptr();
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
        let bucket = buckets
            .entry(unsafe { safe_cf_hash(ptr as CFTypeRef) })
            .or_default();
        if bucket
            .iter()
            .any(|seen| unsafe { safe_cf_equal(seen.as_ptr() as CFTypeRef, ptr as CFTypeRef) })
        {
            continue;
        }
        bucket.push(ax);
        out.push(item);
    }
    out
}

/// The close button of a window, or `None` when there is none.
///
/// The canonical AX close path is pressing the close button: prefer the
/// direct `AXCloseButton` attribute, then the child scan (both are the same
/// button; the attribute is one IPC call and the scan is the fallback for
/// bridges that only expose it as a child with the `AXCloseButton` subrole).
/// Shared by `close()` (which presses the result) and the `actions`
/// advertisement, so the advertised surface always matches the dispatch
/// (tenet 3): a bridge that exposes the button only as a child advertises
/// `close` and can press it.
///
/// The value must be an `AXUIElement` before it is cast and handed to
/// `AXPress`. `AXUIElementCopyAttributeValue` can return `kCFNull` (or
/// another non-element!) for a window with no close button instead of an
/// error, and casting that to `AXUIElementRef` is undefined behavior.
/// Comparing type ids against the element's own (also an `AXUIElement`) is
/// the same guard `ax_string`/`ax_bool` apply, and needs no new safe
/// wrapper.
///
/// Error-preserving: only a genuinely absent attribute falls back to the
/// child scan, and only a genuinely absent subrole means "not the close
/// button". A dead or wedged element answers `kAXErrorCannotComplete` /
/// `kAXErrorInvalidUIElement`, and letting that read as "no close button"
/// hides the real failure as an `ActionNotSupported` (tenet 1) — the same
/// distinction `probe_element_attr` / `ax_windows` make for their
/// attributes.
fn find_close_button(element: AXUIElementRef, role: Role) -> Result<Option<AXElement>> {
    let raw_attr_close = match read_raw_attr(element, "AXCloseButton") {
        RawAttr::Value(v) => {
            // Both `v` and `element` carry the AXUIElement type id if `v`
            // is really an element; anything else (kCFNull, etc.) is
            // treated like a missing attribute.
            let is_element =
                unsafe { safe_cf_get_type_id(v) == safe_cf_get_type_id(element as CFTypeRef) };
            if is_element {
                Some(AXElement::from_owned(v as AXUIElementRef))
            } else {
                unsafe { safe_cf_release(v) };
                None
            }
        }
        RawAttr::Absent => None,
        RawAttr::Unanswered(code) => {
            return Err(Error::platform(
                code as i64,
                format!(
                    "AXCloseButton read failed for a {} (AXError {code}); the close button \
                     is unknown, not absent",
                    role
                ),
            ));
        }
    };
    match raw_attr_close {
        Some(b) => Ok(Some(b)),
        None => match read_raw_attr(element, "AXChildren") {
            RawAttr::Value(v) => {
                let found = unsafe {
                    if safe_cf_get_type_id(v) != safe_cf_array_get_type_id() {
                        None
                    } else {
                        let count = safe_cf_array_get_count(v);
                        let mut found = None;
                        for i in 0..count {
                            let child = safe_cf_array_get_value(v, i);
                            if child.is_null() {
                                continue;
                            }
                            // Same structure as the AXCloseButton read
                            // above: only a genuinely absent subrole means
                            // "not the close button". `ax_string` would
                            // collapse every read failure
                            // (CannotComplete, InvalidUIElement) into
                            // None — turning a real platform error into
                            // "no close button" (tenet 1).
                            let is_close_button = match read_raw_attr(child, "AXSubrole") {
                                RawAttr::Value(subrole) => {
                                    if safe_cf_get_type_id(subrole) == safe_cf_string_get_type_id()
                                    {
                                        // Releases the +1 copy.
                                        cf_string_owned(subrole).as_deref() == Some("AXCloseButton")
                                    } else {
                                        safe_cf_release(subrole);
                                        false
                                    }
                                }
                                RawAttr::Absent => false,
                                RawAttr::Unanswered(code) => {
                                    // `v` (AXChildren) is owned by this
                                    // unsafe block; the release below is
                                    // skipped by this early return, so
                                    // release here or every failed
                                    // subrole read leaks the array.
                                    safe_cf_release(v);
                                    return Err(Error::platform(
                                        code as i64,
                                        format!(
                                            "AXSubrole read failed while scanning for the \
                                             close button of a {} (AXError {code}); the \
                                             close button is unknown, not absent",
                                            role
                                        ),
                                    ));
                                }
                            };
                            if is_close_button {
                                found = Some(AXElement::from_borrowed(child));
                                break;
                            }
                        }
                        found
                    }
                };
                unsafe { safe_cf_release(v) };
                Ok(found)
            }
            RawAttr::Absent => Ok(None),
            RawAttr::Unanswered(code) => Err(Error::platform(
                code as i64,
                format!(
                    "AXChildren read failed while scanning for the close button of a {} \
                     (AXError {code}); the close button is unknown, not absent",
                    role
                ),
            )),
        },
    }
}

/// Roles whose selection state may live on the container rather than on the
/// element itself. Kept narrow so the container probe never fires for
/// elements that simply have no selection concept.
fn selection_can_come_from_container(role: Role) -> bool {
    matches!(role, Role::TableCell | Role::TableRow | Role::ListItem)
}

/// Resolve selection for an element with no `AXSelected` attribute by
/// membership in the nearest ancestor's `AXSelectedChildren`.
///
/// AppKit sets `AXSelected` directly on rows/cells, but some bridges expose
/// selection only container-side: Qt's Cocoa bridge implements no
/// per-element `AXSelected` at all and surfaces `QAccessibleSelectionInterface`
/// as `AXSelectedChildren` on the table — two hops above a cell
/// (cell → row → table). Membership is the platform's canonical reading of
/// per-item selection in that case, not a substitute for it.
///
/// Walks at most `max_hops` ancestors and stops at the first that exposes
/// `AXSelectedChildren`; an empty list there is a definitive "not selected".
/// A chain with no such container yields `false`, matching how the other
/// state attributes degrade when absent.
fn container_selection_contains(element: AXUIElementRef, max_hops: usize) -> bool {
    let mut current = match ax_parent(element) {
        Some(p) => p,
        None => return false,
    };
    for _ in 0..max_hops {
        if let Some(list) = ax_attr(current.as_ptr(), "AXSelectedChildren") {
            let contained = unsafe {
                if safe_cf_get_type_id(list) == safe_cf_array_get_type_id() {
                    let count = safe_cf_array_get_count(list);
                    (0..count).any(|i| {
                        let item = safe_cf_array_get_value(list, i);
                        safe_cf_equal(item, element as CFTypeRef)
                    })
                } else {
                    false
                }
            };
            unsafe { safe_cf_release(list) };
            return contained;
        }
        current = match ax_parent(current.as_ptr()) {
            Some(p) => p,
            None => return false,
        };
    }
    false
}

/// The element's native action names, with every failure read as "none".
/// For snapshots, where a missing action list only narrows `actions`.
/// Dispatch decisions use [`try_ax_action_names`].
fn ax_action_names(element: AXUIElementRef) -> Vec<String> {
    // The error is discarded, so it is built in the cheap snapshot form.
    action_names_in(element, CannotCompleteContext::Snapshot).unwrap_or_default()
}

/// The element's native action names, error-preserving: an element that
/// answered with no actions is `Ok(vec![])`, while an unanswered or malformed
/// reply is `Error::Platform`. A dispatch that picks its AX action from this
/// list must not mistake a failed read for an empty one (tenet 1).
fn try_ax_action_names(element: AXUIElementRef) -> Result<Vec<String>> {
    action_names_in(element, CannotCompleteContext::PreAction)
}

/// [`try_ax_action_names`], with `context` deciding how much a
/// `kAXErrorCannotComplete` costs to describe (see [`cannot_complete_error`]).
fn action_names_in(element: AXUIElementRef, context: CannotCompleteContext) -> Result<Vec<String>> {
    let mut names: CFArrayRef = std::ptr::null();
    let call = AxCall::timed(|| ffi_copy_action_names(element, &mut names));
    match call.code {
        AX_ERROR_SUCCESS => {}
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => return Ok(vec![]),
        // Say which process did not answer, and whether it timed out.
        AX_ERROR_CANNOT_COMPLETE => {
            return Err(cannot_complete_error(
                element,
                "AXUIElementCopyActionNames failed",
                call.elapsed,
                context,
            ))
        }
        code => {
            return Err(Error::platform(
                i64::from(code),
                format!("AXUIElementCopyActionNames failed (AXError {code})"),
            ))
        }
    }
    if names.is_null() {
        return Ok(vec![]);
    }
    if unsafe { safe_cf_get_type_id(names) } != unsafe { safe_cf_array_get_type_id() } {
        unsafe { safe_cf_release(names) };
        return Err(Error::platform(
            -1,
            "AXUIElementCopyActionNames returned a non-array value",
        ));
    }
    Ok(unsafe {
        let count = safe_cf_array_get_count(names);
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            let name = safe_cf_array_get_value(names, i);
            if !name.is_null() && safe_cf_get_type_id(name) == safe_cf_string_get_type_id() {
                result.extend(cf_string(name));
            }
        }
        safe_cf_release(names);
        result
    })
}

fn ax_position(element: AXUIElementRef) -> Option<(f64, f64)> {
    let value = ax_attr(element, "AXPosition")?;
    let mut point = CGPoint::default();
    let ok = unsafe {
        safe_ax_value_get_value(value, AX_VALUE_CGPOINT, &mut point as *mut _ as *mut c_void)
    };
    unsafe { safe_cf_release(value) };
    if ok {
        Some((point.x, point.y))
    } else {
        None
    }
}

fn ax_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    let value = ax_attr(element, "AXSize")?;
    let mut size = CGSize::default();
    let ok = unsafe {
        safe_ax_value_get_value(value, AX_VALUE_CGSIZE, &mut size as *mut _ as *mut c_void)
    };
    unsafe { safe_cf_release(value) };
    if ok {
        Some((size.width, size.height))
    } else {
        None
    }
}

fn ax_value_string(element: AXUIElementRef) -> Option<String> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        let tid = safe_cf_get_type_id(value);
        if tid == safe_cf_string_get_type_id() {
            return cf_string_owned(value);
        }
        if tid == safe_cf_number_get_type_id() {
            let mut f: f64 = 0.0;
            if safe_cf_number_get_value(value, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
                safe_cf_release(value);
                return Some(f.to_string());
            }
        }
        safe_cf_release(value);
        None
    }
}

fn ax_value_number(element: AXUIElementRef) -> Option<f64> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_number_get_type_id() {
            let mut f: f64 = 0.0;
            let ok =
                safe_cf_number_get_value(value, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void);
            safe_cf_release(value);
            if ok {
                return Some(f);
            }
        }
        if safe_cf_get_type_id(value) == safe_cf_string_get_type_id() {
            return cf_string_owned(value)?.trim().parse::<f64>().ok();
        }
        safe_cf_release(value);
        None
    }
}

fn ax_value_int(element: AXUIElementRef) -> Option<i32> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_number_get_type_id() {
            let mut i: i32 = 0;
            let ok =
                safe_cf_number_get_value(value, CF_NUMBER_SINT32, &mut i as *mut _ as *mut c_void);
            safe_cf_release(value);
            if ok {
                return Some(i);
            }
        }
        safe_cf_release(value);
        None
    }
}

// ── Batch Attribute Fetch ────────────────────────────────────────────────────

/// Attribute indices into the batch fetch result array.
/// Order must match `BATCH_ATTRS` below.
mod attr_idx {
    pub const ROLE: usize = 0;
    pub const SUBROLE: usize = 1;
    pub const TITLE: usize = 2;
    pub const DESCRIPTION: usize = 3;
    pub const HELP: usize = 4;
    pub const VALUE: usize = 5;
    pub const ENABLED: usize = 6;
    pub const FOCUSED: usize = 7;
    pub const SELECTED: usize = 8;
    pub const HIDDEN: usize = 9;
    pub const EXPANDED: usize = 10;
    pub const MODAL: usize = 11;
    pub const POSITION: usize = 12;
    pub const SIZE: usize = 13;
    pub const IDENTIFIER: usize = 14;
    pub const MINIMIZED: usize = 15;
    pub const FULLSCREEN: usize = 16;
    // Table / collection position (#479). In the one batch so a tree read
    // pays no extra round trip for them; an element without one gets an
    // error object in its slot, which reads as absent.
    pub const ROW_COUNT: usize = 17;
    pub const COLUMN_COUNT: usize = 18;
    pub const ROW_INDEX_RANGE: usize = 19;
    pub const COLUMN_INDEX_RANGE: usize = 20;
    pub const INDEX: usize = 21;
    pub const DISCLOSURE_LEVEL: usize = 22;
    pub const ARIA_POS_IN_SET: usize = 23;
    pub const ARIA_SET_SIZE: usize = 24;
    // Document semantics (#476), in the same batch.
    pub const ARIA_LIVE: usize = 25;
    pub const ARIA_ATOMIC: usize = 26;
    pub const ARIA_RELEVANT: usize = 27;
    pub const URL: usize = 28;
    pub const COUNT: usize = 29;
}

/// Raw values returned by a single batch AX fetch. Values are borrowed
/// CFTypeRefs (owned by the CFArray) — valid only while `_values_array`
/// is alive.
struct BatchAttrs {
    /// Owning CFArray — values are only valid while this is alive.
    _values_array: CFArrayRef,
    /// Borrowed pointers into the array (may be null or AXValueIllegalType).
    vals: [CFTypeRef; attr_idx::COUNT],
}

impl BatchAttrs {
    /// Fetch all element attributes in a single Mach IPC round-trip.
    fn fetch(element: AXUIElementRef) -> Option<Self> {
        // Build CFArray of attribute name CFStrings.
        let attr_names: [CFString; attr_idx::COUNT] = [
            CFString::new("AXRole"),
            CFString::new("AXSubrole"),
            CFString::new("AXTitle"),
            CFString::new("AXDescription"),
            CFString::new("AXHelp"),
            CFString::new("AXValue"),
            CFString::new("AXEnabled"),
            CFString::new("AXFocused"),
            CFString::new("AXSelected"),
            CFString::new("AXHidden"),
            CFString::new("AXExpanded"),
            CFString::new("AXModal"),
            CFString::new("AXPosition"),
            CFString::new("AXSize"),
            CFString::new("AXIdentifier"),
            CFString::new("AXMinimized"),
            CFString::new("AXFullScreen"),
            CFString::new("AXRowCount"),
            CFString::new("AXColumnCount"),
            CFString::new("AXRowIndexRange"),
            CFString::new("AXColumnIndexRange"),
            CFString::new("AXIndex"),
            CFString::new("AXDisclosureLevel"),
            CFString::new("AXARIAPosInSet"),
            CFString::new("AXARIASetSize"),
            CFString::new("AXARIALive"),
            CFString::new("AXARIAAtomic"),
            CFString::new("AXARIARelevant"),
            CFString::new("AXURL"),
        ];
        let ptrs: Vec<CFTypeRef> = attr_names
            .iter()
            .map(|s| s.as_concrete_TypeRef() as CFTypeRef)
            .collect();

        let cf_attrs = unsafe { safe_cf_array_create(ptrs.as_ptr(), ptrs.len() as CFIndex) };
        if cf_attrs.is_null() {
            return None;
        }

        let mut values: CFArrayRef = std::ptr::null();
        let err = ffi_copy_multiple_attribute_values(element, cf_attrs, &mut values);
        unsafe { safe_cf_release(cf_attrs) };

        if err != AX_ERROR_SUCCESS || values.is_null() {
            return None;
        }
        // The reply is indexed as a CFArray below; a misbehaving app can hand
        // back some other CF type with `kAXErrorSuccess`, and indexing that is
        // undefined behavior. Treat it as a failed batch, so the caller re-reads
        // through the individual attribute path.
        if unsafe { safe_cf_get_type_id(values) } != unsafe { safe_cf_array_get_type_id() } {
            unsafe { safe_cf_release(values) };
            return None;
        }

        let count = unsafe { safe_cf_array_get_count(values) } as usize;
        let mut vals = [std::ptr::null(); attr_idx::COUNT];
        for (i, slot) in vals.iter_mut().enumerate().take(count.min(attr_idx::COUNT)) {
            let v = unsafe { safe_cf_array_get_value(values, i as CFIndex) };
            *slot = v;
        }

        Some(BatchAttrs {
            _values_array: values,
            vals,
        })
    }

    /// Read a value as a String (CFString).
    fn string(&self, idx: usize) -> Option<String> {
        let v = self.vals[idx];
        if v.is_null() {
            return None;
        }
        unsafe {
            if safe_cf_get_type_id(v) == safe_cf_string_get_type_id() {
                cf_string(v)
            } else {
                None
            }
        }
    }

    /// Read a value as a URL string (CFURL).
    fn url(&self, idx: usize) -> Option<String> {
        cf_url_string(self.vals[idx])
    }

    /// Read a value as a bool (CFBoolean).
    fn boolean(&self, idx: usize) -> Option<bool> {
        let v = self.vals[idx];
        if v.is_null() {
            return None;
        }
        unsafe {
            if safe_cf_get_type_id(v) == safe_cf_boolean_get_type_id() {
                Some(safe_cf_boolean_get_value(v))
            } else {
                None
            }
        }
    }

    /// Read an integer-valued slot (a CFNumber).
    fn integer(&self, idx: usize) -> Option<i64> {
        rich::cf_i64(self.vals[idx])
    }

    /// Read a CFRange-valued slot (an AXValue).
    fn range(&self, idx: usize) -> Option<(i64, i64)> {
        rich::cf_range(self.vals[idx])
    }

    /// Read AXValue as a string (handles CFString and CFNumber).
    fn value_string(&self) -> Option<String> {
        let v = self.vals[attr_idx::VALUE];
        if v.is_null() {
            return None;
        }
        unsafe {
            let tid = safe_cf_get_type_id(v);
            if tid == safe_cf_string_get_type_id() {
                return cf_string(v);
            }
            if tid == safe_cf_number_get_type_id() {
                let mut f: f64 = 0.0;
                if safe_cf_number_get_value(v, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
                    return Some(f.to_string());
                }
            }
            None
        }
    }

    /// Read AXValue as an f64 number.
    fn value_number(&self) -> Option<f64> {
        let v = self.vals[attr_idx::VALUE];
        if v.is_null() {
            return None;
        }
        unsafe {
            if safe_cf_get_type_id(v) == safe_cf_number_get_type_id() {
                let mut f: f64 = 0.0;
                if safe_cf_number_get_value(v, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
                    return Some(f);
                }
            }
            if safe_cf_get_type_id(v) == safe_cf_string_get_type_id() {
                return cf_string(v)?.trim().parse::<f64>().ok();
            }
            None
        }
    }

    /// Read AXValue as an i32 integer.
    fn value_int(&self) -> Option<i32> {
        let v = self.vals[attr_idx::VALUE];
        if v.is_null() {
            return None;
        }
        unsafe {
            if safe_cf_get_type_id(v) == safe_cf_number_get_type_id() {
                let mut i: i32 = 0;
                if safe_cf_number_get_value(v, CF_NUMBER_SINT32, &mut i as *mut _ as *mut c_void) {
                    return Some(i);
                }
            }
            None
        }
    }

    /// Read AXPosition as (x, y).
    fn position(&self) -> Option<(f64, f64)> {
        let v = self.vals[attr_idx::POSITION];
        if v.is_null() {
            return None;
        }
        let mut point = CGPoint::default();
        let ok = unsafe {
            safe_ax_value_get_value(v, AX_VALUE_CGPOINT, &mut point as *mut _ as *mut c_void)
        };
        if ok {
            Some((point.x, point.y))
        } else {
            None
        }
    }

    /// Read AXSize as (width, height).
    fn size(&self) -> Option<(f64, f64)> {
        let v = self.vals[attr_idx::SIZE];
        if v.is_null() {
            return None;
        }
        let mut size = CGSize::default();
        let ok = unsafe {
            safe_ax_value_get_value(v, AX_VALUE_CGSIZE, &mut size as *mut _ as *mut c_void)
        };
        if ok {
            Some((size.width, size.height))
        } else {
            None
        }
    }
}

impl Drop for BatchAttrs {
    fn drop(&mut self) {
        if !self._values_array.is_null() {
            unsafe { safe_cf_release(self._values_array) };
        }
    }
}

// ── Resolved Attributes ──────────────────────────────────────────────────────

/// Platform-independent snapshot of all AX attributes needed to build an
/// ElementData. Populated either from a BatchAttrs (1 IPC call) or from
/// individual ax_* helpers (fallback path).
struct ResolvedAttrs {
    role_str: String,
    subrole_str: Option<String>,
    ax_title: Option<String>,
    ax_description: Option<String>,
    ax_help: Option<String>,
    value_string: Option<String>,
    value_int: Option<i32>,
    value_number: Option<f64>,
    /// Is the raw AXValue a CFBoolean? Used for checkbox toggle fallback.
    value_is_bool: Option<bool>,
    enabled: Option<bool>,
    focused: Option<bool>,
    selected: Option<bool>,
    hidden: Option<bool>,
    expanded: Option<bool>,
    modal: Option<bool>,
    /// Window minimized state (`AXMinimized`); `None` on non-window elements.
    minimized: Option<bool>,
    /// Window fullscreen state (`AXFullScreen`); `None` on non-window elements.
    fullscreen: Option<bool>,
    position: Option<(f64, f64)>,
    size: Option<(f64, f64)>,
    identifier: Option<String>,
    /// Table / collection position attributes (#479).
    rich: rich::PositionAttrs,
    /// Live-region settings and link target (#476).
    aria_live: Option<String>,
    aria_atomic: Option<bool>,
    aria_relevant: Option<String>,
    url: Option<String>,
}

impl ResolvedAttrs {
    /// Populate from a BatchAttrs (1 Mach IPC round-trip).
    ///
    /// Returns `None` when the role slot does not hold a string. The batch
    /// call never fails per attribute; it puts an `AXValue` error object in
    /// the slot of each attribute it could not read. For most attributes
    /// that is an absent value, but an unreadable role would build a
    /// `Role::Unknown` element with no sign that the read failed. The caller
    /// re-reads the element through [`from_individual`](Self::from_individual),
    /// which tells a role-less element apart from an unreadable one and
    /// propagates the latter.
    fn from_batch(batch: &BatchAttrs) -> Option<Self> {
        let role_str = batch.string(attr_idx::ROLE)?;
        let value_is_bool = {
            let v = batch.vals[attr_idx::VALUE];
            if v.is_null() {
                None
            } else {
                unsafe {
                    if safe_cf_get_type_id(v) == safe_cf_boolean_get_type_id() {
                        Some(safe_cf_boolean_get_value(v))
                    } else {
                        None
                    }
                }
            }
        };

        Some(Self {
            role_str,
            subrole_str: batch.string(attr_idx::SUBROLE),
            ax_title: batch.string(attr_idx::TITLE),
            ax_description: batch.string(attr_idx::DESCRIPTION),
            ax_help: batch.string(attr_idx::HELP),
            value_string: batch.value_string(),
            value_int: batch.value_int(),
            value_number: batch.value_number(),
            value_is_bool,
            enabled: batch.boolean(attr_idx::ENABLED),
            focused: batch.boolean(attr_idx::FOCUSED),
            selected: batch.boolean(attr_idx::SELECTED),
            hidden: batch.boolean(attr_idx::HIDDEN),
            expanded: batch.boolean(attr_idx::EXPANDED),
            modal: batch.boolean(attr_idx::MODAL),
            minimized: batch.boolean(attr_idx::MINIMIZED),
            fullscreen: batch.boolean(attr_idx::FULLSCREEN),
            position: batch.position(),
            size: batch.size(),
            identifier: batch.string(attr_idx::IDENTIFIER),
            rich: rich::PositionAttrs {
                row_count: batch.integer(attr_idx::ROW_COUNT),
                column_count: batch.integer(attr_idx::COLUMN_COUNT),
                row_range: batch.range(attr_idx::ROW_INDEX_RANGE),
                column_range: batch.range(attr_idx::COLUMN_INDEX_RANGE),
                index: batch.integer(attr_idx::INDEX),
                disclosure_level: batch.integer(attr_idx::DISCLOSURE_LEVEL),
                pos_in_set: batch.integer(attr_idx::ARIA_POS_IN_SET),
                set_size: batch.integer(attr_idx::ARIA_SET_SIZE),
            },
            aria_live: batch.string(attr_idx::ARIA_LIVE),
            aria_atomic: batch.boolean(attr_idx::ARIA_ATOMIC),
            aria_relevant: batch.string(attr_idx::ARIA_RELEVANT),
            url: batch.url(attr_idx::URL),
        })
    }

    /// Populate from individual AX API calls (fallback path).
    ///
    /// The batch call is an optimisation, not a requirement, so a failed
    /// batch fetch falls back here. The read is error-preserving for
    /// `AXRole`, the attribute that decides whether the element is surfaced
    /// at all: an unanswered read — the object can be invalidated between
    /// the batch fetch and this fallback, which is the transition churn the
    /// callers handle — propagates as the platform failure instead of
    /// becoming an empty role, because an empty role builds a
    /// `Role::Unknown` ghost the enumerating callers can neither drop by
    /// code nor report (tenet 1). A definitive "no role" answer
    /// (`RawAttr::Absent`, the process answered) still builds the role-less
    /// element, and the remaining attributes keep the tree-walk tolerance
    /// `ax_attr` documents.
    fn from_individual(element: AXUIElementRef) -> Result<Self> {
        let role_read_start = Instant::now();
        let role_read = read_raw_attr(element, "AXRole");
        let role_read_elapsed = role_read_start.elapsed();
        let role_str = match role_read {
            RawAttr::Value(v) => {
                if unsafe { safe_cf_get_type_id(v) } == unsafe { safe_cf_string_get_type_id() } {
                    // `cf_string_owned` releases the +1 copy; a string it
                    // cannot read (the wrapper caught an exception) is as
                    // malformed as a non-string.
                    cf_string_owned(v).ok_or_else(|| {
                        Error::platform(-1, "AXRole returned a string that could not be read")
                    })?
                } else {
                    // A value of another type is a malformed answer, not the
                    // definitive "no role" the `Absent` arm covers: turning
                    // it into an empty role would rebuild the ghost this
                    // error-preserving read exists to prevent (tenet 1),
                    // the same distinction `ax_windows` makes for a
                    // non-array `AXWindows`.
                    unsafe { safe_cf_release(v) };
                    return Err(Error::platform(
                        -1,
                        "AXRole returned a non-string value; the provider answered \
                                  malformed, not role-less"
                            .to_string(),
                    ));
                }
            }
            RawAttr::Absent => String::new(),
            // Say which process did not answer, and whether it timed out.
            // PID only: callers such as `list_apps` and attach read this as a
            // retry signal and may discard it.
            RawAttr::Unanswered(AX_ERROR_CANNOT_COMPLETE) => {
                return Err(cannot_complete_error(
                    element,
                    "AXRole read failed while building an element snapshot",
                    role_read_elapsed,
                    CannotCompleteContext::Snapshot,
                ));
            }
            RawAttr::Unanswered(code) => {
                return Err(Error::platform(
                    code as i64,
                    format!(
                        "AXRole read failed while building an element snapshot (AXError \
                         {code}); the element is unreadable, not role-less"
                    ),
                ));
            }
        };
        let subrole_str = ax_string(element, "AXSubrole");
        let rich =
            rich::PositionAttrs::read_individually(element, &role_str, subrole_str.as_deref());
        Ok(Self {
            role_str,
            subrole_str,
            ax_title: ax_string(element, "AXTitle"),
            ax_description: ax_string(element, "AXDescription"),
            ax_help: ax_string(element, "AXHelp"),
            value_string: ax_value_string(element),
            value_int: ax_value_int(element),
            value_number: ax_value_number(element),
            value_is_bool: ax_bool(element, "AXValue"),
            enabled: ax_bool(element, "AXEnabled"),
            focused: ax_bool(element, "AXFocused"),
            selected: ax_bool(element, "AXSelected"),
            hidden: ax_bool(element, "AXHidden"),
            expanded: ax_bool(element, "AXExpanded"),
            modal: ax_bool(element, "AXModal"),
            minimized: ax_bool(element, "AXMinimized"),
            fullscreen: ax_bool(element, "AXFullScreen"),
            position: ax_position(element),
            size: ax_size(element),
            identifier: ax_string(element, "AXIdentifier"),
            rich,
            aria_live: ax_string(element, "AXARIALive"),
            aria_atomic: ax_bool(element, "AXARIAAtomic"),
            aria_relevant: ax_string(element, "AXARIARelevant"),
            url: ax_attr(element, "AXURL").and_then(|v| {
                let s = cf_url_string(v);
                unsafe { safe_cf_release(v) };
                s
            }),
        })
    }
}

// ── Safe FFI Wrappers ────────────────────────────────────────────────────────

/// The result of one AX message: its AXError code and how long it took.
///
/// The duration is what tells a messaging timeout apart from an app that
/// answered `kAXErrorCannotComplete` straight away (see
/// [`cannot_complete_error`]). It compares equal to a bare code, so callers
/// test `call != AX_ERROR_SUCCESS` as they would the `i32`.
#[derive(Debug, Clone, Copy)]
struct AxCall {
    code: i32,
    elapsed: Duration,
}

impl AxCall {
    fn timed(f: impl FnOnce() -> i32) -> Self {
        let start = Instant::now();
        let code = f();
        Self {
            code,
            elapsed: start.elapsed(),
        }
    }
}

impl PartialEq<i32> for AxCall {
    fn eq(&self, code: &i32) -> bool {
        self.code == *code
    }
}

fn do_perform_action(element: AXUIElementRef, action: &CFString) -> AxCall {
    AxCall::timed(|| {
        ax_bounded(
            element,
            || format!("performing {action}"),
            || unsafe {
                safe_ax_perform_action(element, action.as_concrete_TypeRef() as CFTypeRef)
            },
        )
    })
}

fn do_set_attribute(element: AXUIElementRef, attribute: &CFString, value: CFTypeRef) -> AxCall {
    AxCall::timed(|| {
        ax_bounded(
            element,
            || format!("setting {attribute}"),
            || unsafe {
                safe_ax_set_attribute_value(
                    element,
                    attribute.as_concrete_TypeRef() as CFTypeRef,
                    value,
                )
            },
        )
    })
}

/// Re-classify an error from a step that ran after `done` had already changed
/// the window.
///
/// Verbs such as `minimize`, `enter_fullscreen` and `restore` mutate in
/// several steps. A pre-dispatch-class error (`ActionNotSupported`,
/// `ElementStale`, ...) from a later step describes that step only. Reporting
/// it as `NotDelivered` would invite a retry on a window that has already
/// changed, so it becomes a `Platform` error, which reads as `Unknown`. An
/// error that is already `Unknown` passes through.
fn unknown_after_step(e: Error, done: &str) -> Error {
    if e.delivery() == xa11y_core::Delivery::Unknown {
        return e;
    }
    Error::platform(
        -1,
        format!("{e}, after {done} had already changed the window"),
    )
}

/// Convert a failed AX action call into an appropriate `Error`. Returns
/// `ActionNotSupported` for -25206 (kAXErrorActionUnsupported) so callers get
/// a clear, structured error instead of a raw platform code, and describes a
/// `kAXErrorCannotComplete` with the application it was sent to.
fn action_error(
    el_ptr: AXUIElementRef,
    call: AxCall,
    action: &str,
    role: Role,
    fallback_msg: &str,
) -> Error {
    if call == AX_ERROR_ACTION_UNSUPPORTED {
        Error::ActionNotSupported {
            action: action.to_string(),
            role,
        }
    } else if call == AX_ERROR_CANNOT_COMPLETE {
        // An action is a terminal call, not a retry signal, so the app name
        // lookup is affordable here.
        cannot_complete_error(
            el_ptr,
            &format!("{action}: {fallback_msg}"),
            call.elapsed,
            CannotCompleteContext::Action,
        )
    } else {
        Error::platform(call.code as i64, fallback_msg.to_string())
    }
}

// ── Messaging Timeout ───────────────────────────────────────────────────────

/// Upper bound on each accessibility message this process sends, in seconds.
///
/// Without it every message waits for the system default (about six
/// seconds), and a walk over a busy or hung app pays that per attribute
/// read. Set once, process-wide, by [`set_global_messaging_timeout`]. A
/// caller's deadline nearer than this lowers it per message (`ax_bounded`).
const AX_MESSAGING_TIMEOUT_SECS: f32 = 2.0;

/// Apply [`AX_MESSAGING_TIMEOUT_SECS`] to every AX message from this process.
///
/// Apple documents a timeout set on the system-wide element as the global
/// value for the calling process, and a `0` set on any other element as
/// "use the current global value". That is what the shell scan's release
/// (`unbound_shell_probe`) writes, so a released probe returns to this bound.
fn set_global_messaging_timeout() -> Result<()> {
    let system_wide = system_wide_element();
    if system_wide.is_null() {
        return Err(Error::platform(
            -1,
            "AXUIElementCreateSystemWide returned NULL",
        ));
    }
    let err =
        unsafe { safe_ax_set_messaging_timeout(system_wide.as_ptr(), AX_MESSAGING_TIMEOUT_SECS) };
    if err != AX_ERROR_SUCCESS {
        return Err(Error::platform(
            err as i64,
            format!(
                "AXUIElementSetMessagingTimeout({AX_MESSAGING_TIMEOUT_SECS}s) on the \
                 system-wide element failed"
            ),
        ));
    }
    Ok(())
}

/// Where a `kAXErrorCannotComplete` came from, which decides how much the
/// error may cost to describe and what it must warn about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CannotCompleteContext {
    /// A terminal action or attribute write. The app name is looked up (one
    /// window-list copy), and the message warns that the action may already
    /// have been delivered.
    Action,
    /// A read an action makes before sending anything: the stale-handle
    /// check and the action-name lookup that picks the AX action. Terminal
    /// like [`Action`](Self::Action), so the app name is looked up, but
    /// nothing has been sent yet, so there is no delivery warning.
    PreAction,
    /// A snapshot read. Enumeration, attach and churn handling read this
    /// error as a retry signal and often discard it, so it carries the PID
    /// only (tenet 6: no expensive diagnosis on a retry-signal path).
    Snapshot,
}

/// A call that took at least this fraction of [`AX_MESSAGING_TIMEOUT_SECS`]
/// before failing with `kAXErrorCannotComplete` is reported as a timeout;
/// a faster one is the app (or the AX server) refusing outright.
const TIMEOUT_FRACTION: f32 = 0.9;

/// Whether a `kAXErrorCannotComplete` after `elapsed` was the messaging
/// timeout expiring.
fn hit_messaging_timeout(elapsed: Duration) -> bool {
    elapsed.as_secs_f32() >= AX_MESSAGING_TIMEOUT_SECS * TIMEOUT_FRACTION
}

/// The `kAXErrorCannotComplete` error for a message sent to `el_ptr`.
///
/// `kAXErrorCannotComplete` is also what an expired messaging timeout
/// surfaces as, but not only that, so the message claims a timeout only when
/// the call actually ran for (about) the whole timeout. The PID comes from
/// the element itself, with no IPC to the unresponsive app. The name lookup
/// copies the window server's list, so it happens only for
/// [`CannotCompleteContext::Action`]. The code stays
/// `kAXErrorCannotComplete`, so `classify_element_churn` still reads it as
/// `Unreachable`.
fn cannot_complete_error(
    el_ptr: AXUIElementRef,
    what: &str,
    elapsed: Duration,
    context: CannotCompleteContext,
) -> Error {
    let mut pid = 0;
    let pid_known = !el_ptr.is_null()
        && unsafe { safe_ax_get_pid(el_ptr, &mut pid) } == AX_ERROR_SUCCESS
        && pid > 0;
    let name = if pid_known
        && matches!(
            context,
            CannotCompleteContext::Action | CannotCompleteContext::PreAction
        ) {
        MacOSProvider::list_gui_apps()
            .into_iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, name)| name)
    } else {
        None
    };
    let app = match (pid_known, name) {
        (true, Some(name)) => format!("application {name:?} (pid {pid})"),
        (true, None) => format!("the application with pid {pid}"),
        (false, _) => "the owning application (pid unknown)".to_string(),
    };
    Error::platform(
        AX_ERROR_CANNOT_COMPLETE as i64,
        cannot_complete_message(
            what,
            &app,
            elapsed,
            context,
            // An expired caller's deadline is what bounded this call
            // (`ax_bounded`), so the global timeout's arithmetic does not
            // apply to it.
            xa11y_core::Deadline::current().is_some_and(|d| d.is_expired()),
        ),
    )
}

/// The text of [`cannot_complete_error`], split out so its wording is
/// unit-testable without an unresponsive app.
fn cannot_complete_message(
    what: &str,
    app: &str,
    elapsed: Duration,
    context: CannotCompleteContext,
    deadline_cut: bool,
) -> String {
    let ms = elapsed.as_millis();
    let mut message = if deadline_cut {
        format!(
            "{what}: {app} did not respond before the caller's deadline \
             (kAXErrorCannotComplete after {ms} ms); it may be busy or hung"
        )
    } else if hit_messaging_timeout(elapsed) {
        format!(
            "{what}: {app} did not respond within the {AX_MESSAGING_TIMEOUT_SECS}s accessibility \
             messaging timeout (kAXErrorCannotComplete after {ms} ms); it may be busy or hung"
        )
    } else {
        format!(
            "{what}: {app} returned kAXErrorCannotComplete after {ms} ms (not a timeout: the \
             {AX_MESSAGING_TIMEOUT_SECS}s messaging timeout was not reached, so the app or the \
             accessibility server refused the request)"
        )
    };
    if context == CannotCompleteContext::Action {
        message.push_str(
            "; the request may still have been delivered, so the action may already have \
             taken effect",
        );
    }
    message
}

// ── Action Helpers ──────────────────────────────────────────────────────────
//
// Small functions that reduce repetition in `perform_action`. Each wraps a
// single AX API call pattern and converts the result to `Result<()>`.

/// Invoke an AX action by name. Used for Press, ShowMenu, Increment, Decrement.
fn perform_ax_action(
    el_ptr: AXUIElementRef,
    ax_name: &str,
    action: &str,
    role: Role,
) -> Result<()> {
    let cf = CFString::new(ax_name);
    let err = do_perform_action(el_ptr, &cf);
    if err != AX_ERROR_SUCCESS {
        return Err(action_error(
            el_ptr,
            err,
            action,
            role,
            &format!("{ax_name} failed"),
        ));
    }
    Ok(())
}

/// Set a boolean attribute. Used for Focus, Blur, Select, Expand, Collapse.
/// Check whether `attr_name` is settable on the element.
///
/// Used to surface `ActionNotSupported` for attribute-backed semantic verbs
/// (`expand` / `collapse`): every AX bridge (AppKit, AccessKit, Qt) accepts
/// `AXUIElementSetAttributeValue` for an attribute the element does not
/// support and silently no-ops, so a plain "set and check the error code"
/// can never report unsupported. Matches the AT-SPI2 backend (missing action
/// index) and the UIA backend (missing ExpandCollapse pattern).
fn is_attr_settable(el_ptr: AXUIElementRef, attr_name: &str) -> Result<bool> {
    let attr = CFString::new(attr_name);
    let mut settable = false;
    let err = ax_bounded(
        el_ptr,
        || format!("asking whether {attr_name} is settable"),
        || unsafe {
            safe_ax_is_attribute_settable(
                el_ptr,
                attr.as_concrete_TypeRef() as CFTypeRef,
                &mut settable,
            )
        },
    );
    match err {
        AX_ERROR_SUCCESS => Ok(settable),
        // "Is this attribute settable?" answered as "the attribute is not
        // supported / has no value" is a definitive `false`, not a platform
        // failure: `restore` checks both `AXMinimized` and `AXFullScreen`,
        // and a window without the fullscreen state failing the whole restore
        // on `AXFullScreen`-unsupported hides that it restored fine (tenet 1
        // is about real failures — an explicit "not supported" is the
        // answer).
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(false),
        code => Err(Error::platform(
            code as i64,
            format!("IsAttributeSettable({attr_name}) failed"),
        )),
    }
}

fn set_bool_attr(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    value: bool,
    action: &str,
    role: Role,
) -> Result<()> {
    let attr = CFString::new(attr_name);
    let val = if value {
        core_foundation::boolean::CFBoolean::true_value()
    } else {
        core_foundation::boolean::CFBoolean::false_value()
    };
    let err = do_set_attribute(el_ptr, &attr, val.as_CFTypeRef());
    if err != AX_ERROR_SUCCESS {
        return Err(action_error(
            el_ptr,
            err,
            action,
            role,
            &format!("Set {attr_name}={value} failed"),
        ));
    }
    Ok(())
}

/// The application element owning `el_ptr`, resolved through the element's
/// PID (`AXUIElementGetPid` / `AXUIElementCreateApplication`) — the same
/// addressing `App::by_pid` uses.
fn owning_app_element(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<AXElement> {
    let mut pid = 0;
    let pid_err = unsafe { safe_ax_get_pid(el_ptr, &mut pid) };
    if pid_err != AX_ERROR_SUCCESS {
        return Err(Error::platform(
            pid_err as i64,
            format!(
                "AXUIElementGetPid failed (AXError {pid_err}) while running {action} on a {role}"
            ),
        ));
    }
    if pid == 0 {
        return Err(Error::platform(
            -1,
            format!("AXUIElementGetPid returned pid 0 while running {action} on a {role}"),
        ));
    }
    let app = AXElement::from_owned(unsafe { safe_ax_create_application(pid) });
    if app.is_null() {
        return Err(Error::platform(
            -9999,
            format!("AXUIElementCreateApplication failed while running {action}"),
        ));
    }
    Ok(app)
}

/// Bring the process owning `element` to the foreground via the accessibility
/// API: the application element's `AXFrontmost` attribute.
///
/// `AXRaise` on a window only re-raises it within the window list of its own
/// app and answers success while the app sits in the background, so activating
/// a background app's window appears to do nothing. The application
/// element is built from the element's owning PID (`AXUIElementGetPid`), the
/// same addressing `App::by_pid` uses, and the attribute is the one
/// `osascript`'s "set frontmost of process" sets — activation without input
/// simulation (tenet 2).
fn activate_owning_app(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    set_bool_attr(app.as_ptr(), "AXFrontmost", true, action, role)
}

/// The application-element attribute that asks a Chromium-based app to build
/// its web content's accessibility tree. Electron answers it, and it has no
/// side effects on the app's native UI.
///
/// `AXEnhancedUserInterface` (what VoiceOver sets) is deliberately not tried
/// when this one is refused: it changes how the app behaves for every client
/// (it is known to disable window animations and to confuse window managers),
/// and trying it after a refusal would be the silent fallback tenet 1 rules
/// out. An app that ignores this attribute is launched with
/// `--force-renderer-accessibility` instead.
const WEB_ACCESSIBILITY_ATTR: &str = "AXManualAccessibility";

/// What reading a web-accessibility switch back returned.
#[derive(Debug, PartialEq)]
enum SwitchReadBack {
    /// A boolean, or a number (0 is false).
    Flag(bool),
    /// The app has no such attribute.
    Absent,
    /// A value of another type.
    Other,
    /// The read failed with this AXError.
    Unanswered(i32),
}

impl std::fmt::Display for SwitchReadBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag(b) => write!(f, "{b}"),
            Self::Absent => f.write_str("no such attribute"),
            Self::Other => f.write_str("a non-boolean value"),
            Self::Unanswered(code) => write!(f, "no answer (AXError {code})"),
        }
    }
}

fn read_switch(el_ptr: AXUIElementRef, attr_name: &str) -> SwitchReadBack {
    match read_raw_attr(el_ptr, attr_name) {
        RawAttr::Value(v) => unsafe {
            let tid = safe_cf_get_type_id(v);
            let read = if tid == safe_cf_boolean_get_type_id() {
                SwitchReadBack::Flag(safe_cf_boolean_get_value(v))
            } else if tid == safe_cf_number_get_type_id() {
                let mut n: i64 = 0;
                if safe_cf_number_get_value(v, CF_NUMBER_SINT64, &mut n as *mut _ as *mut c_void) {
                    SwitchReadBack::Flag(n != 0)
                } else {
                    SwitchReadBack::Other
                }
            } else {
                SwitchReadBack::Other
            };
            safe_cf_release(v);
            read
        },
        RawAttr::Absent => SwitchReadBack::Absent,
        RawAttr::Unanswered(code) => SwitchReadBack::Unanswered(code),
    }
}

/// Ask the application `app` to build its web content's accessibility tree.
///
/// [`WEB_ACCESSIBILITY_ATTR`] is set and then read back. Either a successful
/// write or a `true` read-back is the success, because apps prove it
/// differently: Chromium has answered `kAXErrorNotImplemented` to a set it
/// carried out, and Electron accepts the set and reads back `false`. The
/// attribute stays set: it is not restored. When neither holds, the error
/// names what the write and the read returned and what to do instead.
fn enable_web_accessibility_for(app: &ElementData) -> Result<()> {
    const ACTION: &str = "enable_web_accessibility";
    if app.role != Role::Application {
        return Err(Error::ActionNotSupported {
            action: ACTION.to_string(),
            role: app.role,
        });
    }
    let Some(pid) = app.pid else {
        return Err(Error::platform(
            -1,
            format!("{ACTION}: the application element has no pid"),
        ));
    };
    let app_el = AXElement::from_owned(unsafe { safe_ax_create_application(pid as i32) });
    if app_el.is_null() {
        return Err(Error::platform(
            -9999,
            format!("AXUIElementCreateApplication({pid}) failed while running {ACTION}"),
        ));
    }
    let attr = CFString::new(WEB_ACCESSIBILITY_ATTR);
    let set = do_set_attribute(
        app_el.as_ptr(),
        &attr,
        core_foundation::boolean::CFBoolean::true_value().as_CFTypeRef(),
    );
    // One request, two pieces of evidence that the app took it, because apps
    // disagree on which they give: Chromium can answer the write with an
    // error and still read back `true`, while Electron accepts the write and
    // reads back `false`. Either one means the request was taken; neither is
    // a second mechanism.
    let read = read_switch(app_el.as_ptr(), WEB_ACCESSIBILITY_ATTR);
    if set.code == 0 || read == SwitchReadBack::Flag(true) {
        return Ok(());
    }
    let name = app.name.clone().unwrap_or_default();
    Err(Error::AccessibilityNotEnabled {
        app: format!("{name} (pid={pid})"),
        instructions: web_accessibility_refused(set.code, &read),
    })
}

/// The guidance of the error `enable_web_accessibility` returns when the app
/// neither accepted the [`WEB_ACCESSIBILITY_ATTR`] write nor reads it back as
/// set.
fn web_accessibility_refused(set_code: i32, read: &SwitchReadBack) -> String {
    format!(
        "the app refused {WEB_ACCESSIBILITY_ATTR} (the write returned AXError \
         {set_code}, and it reads back {read}). Electron and Chromium take it. Relaunch an app that \
         does not with --force-renderer-accessibility. xa11y does not set \
         AXEnhancedUserInterface instead, because that changes the app's behaviour \
         for every client."
    )
}

/// Clear a boolean attribute when it currently reads `true`.
///
/// The deminiaturize half of `activate` / `enter_fullscreen`: neither
/// `AXRaise` nor setting `AXFullScreen` clears `AXMinimized`, so a minimized
/// window must
/// have its minimized flag cleared first or the verb returns success while
/// the window stays in the Dock. Error-preserving through [`read_bool_attr`]:
/// only a definitive unsupported / no-value answer means "not set"; a failed
/// or malformed read is a platform error (tenet 1), the same distinction
/// `restore()` makes.
fn clear_bool_attr_if_true(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    action: &str,
    role: Role,
) -> Result<()> {
    if read_bool_attr(el_ptr, attr_name, action, role)?.unwrap_or(false) {
        set_bool_attr(el_ptr, attr_name, false, action, role)?;
    }
    Ok(())
}

/// How long `enter_fullscreen` / `restore` (and the fullscreen exit half of
/// `minimize`) wait for the fullscreen transition to commit, and the poll
/// cadence during the wait.
///
/// Native fullscreen (`AXFullScreen`) is the state these verbs drive, and it
/// is readable *and* writable. `AXMinimized` is equally readable and
/// writable, but it is the minimize state; the zoom state has no attribute
/// at all. The green button's `AXPress` and `AXZoomWindow` actions are toggles
/// whose state cannot be read back. The transition is asynchronous and
/// hostile to observation on macOS 26:
///
/// * A window *entering* fullscreen transiently reports
///   `AXFullScreen=false` while the transition runs, so a bare `false` read
///   is not evidence of a restored window. On the AppKit windows measured on
///   macOS 26.4, `IsAttributeSettable(AXFullScreen)` stayed `true` through
///   every entry/exit sample — the transient withdraws the *value*, not the
///   capability — so a single sample cannot tell a transient from a settled
///   window (see [`WINDOW_FULLSCREEN_SETTLE_GRACE`]).
/// * A set issued while a transition is in flight is discarded, not queued: a
///   `true` set followed 10 ms later by a `false` set leaves the window
///   fullscreen, so waiting alone never recovers it. The settle loop
///   re-issues the set.
/// * The window object can be recreated mid-transition (transient `AXUnknown`
///   windows appear and disappear from `AXWindows`), so the settle follows
///   the target into the freshly enumerated set and falls back to its cached
///   handle while it is off the set.
///
/// The budget covers the observed ~0.5s AppKit transition plus retries while
/// keeping a wedged window from hanging the verb for long.
const WINDOW_FULLSCREEN_SETTLE_BUDGET: Duration = Duration::from_secs(5);
const WINDOW_FULLSCREEN_SETTLE_INTERVAL: Duration = Duration::from_millis(50);
/// How long the promised state must hold, without interruption, before the
/// wait reports success.
///
/// A transition AppKit has queued but not started reports exactly what a
/// settled window reports — an entering window transiently reads
/// `AXFullScreen=false` with `IsAttributeSettable=true`, which *is* the
/// restore promise — so a confirmation shorter than the transition can
/// confirm the pre-commit value. Measured on macOS 26.4, the transition takes
/// ~0.5 s and the window is absent from `AXWindows` for 450–650 ms once under
/// way. The hold counts only while the target is enumerated, so the absence
/// itself never counts toward it, however long it runs. Any unreadable sample,
/// absence or state change restarts the run (see [`SettleRun`]).
const WINDOW_FULLSCREEN_SETTLE_GRACE: Duration = Duration::from_millis(700);

/// How long `minimize` / `restore` wait for the `AXMinimized` state to commit,
/// and the poll cadence during the wait.
///
/// AppKit applies a minimized set asynchronously on native windows: the call
/// returns while the Dock animation still runs, so a caller reading the state
/// right after `minimize` / `restore` observes the previous value. Measured on
/// macOS 26.4, deminiaturizing a native AppKit window takes ~0.7 s before
/// `AXMinimized` reads `false` again. Unlike the fullscreen transition the set
/// is not discarded, but re-issuing an absolute set is a no-op and recovers
/// one that was.
const WINDOW_MINIMIZED_SETTLE_BUDGET: Duration = Duration::from_secs(5);
const WINDOW_MINIMIZED_SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// How long `move_to` and `resize_to` wait for an `AXPosition` / `AXSize` write
/// to become observable, and the poll cadence during the wait.
///
/// Some toolkit bridges acknowledge a geometry write before their event loop has
/// applied it, and can drop that first write while the window is settling
/// from another transition. Re-issuing the absolute value is idempotent
/// and turns the accepted write into the promise `move_to` and `resize_to` expose.
const WINDOW_GEOMETRY_SETTLE_BUDGET: Duration = Duration::from_secs(5);
const WINDOW_GEOMETRY_SETTLE_INTERVAL: Duration = Duration::from_millis(50);
/// AX bridges can round logical coordinates while crossing the native
/// boundary. This matches the tolerance used by the cross-binding integration
/// tests and is small enough to distinguish their 10-point move.
const WINDOW_GEOMETRY_TOLERANCE: f64 = 2.0;

/// Read a boolean AX attribute with the tenet-1 distinction: a failed read is
/// an error (the state is unknown, not false), an absent / unsupported
/// attribute is `Ok(None)`.
///
/// The one strict reader behind [`read_fullscreen_state`], the
/// enter_fullscreen/restore probes, [`clear_bool_attr_if_true`] and the `actions`
/// advertisement, so a malformed answer cannot be accepted by one surface and
/// rejected by another (tenet 3). `action` names the operation for the error
/// text.
fn read_bool_attr(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    action: &str,
    role: Role,
) -> Result<Option<bool>> {
    match read_raw_attr(el_ptr, attr_name) {
        RawAttr::Value(v) => {
            let is_boolean = unsafe { safe_cf_get_type_id(v) == safe_cf_boolean_get_type_id() };
            if !is_boolean {
                // A value of another type is a malformed answer: the state
                // is unknown, not false, and the doc above promises not to
                // collapse that (tenet 1).
                unsafe { safe_cf_release(v) };
                return Err(Error::platform(
                    -1,
                    format!(
                        "{attr_name} returned a non-boolean value while handling {action} on a \
                         {role}; the state is unknown, not false"
                    ),
                ));
            }
            let b = unsafe { safe_cf_boolean_get_value(v) };
            unsafe { safe_cf_release(v) };
            Ok(Some(b))
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::platform(
            code as i64,
            format!(
                "{attr_name} read failed while handling {action} on a {role} (AXError {code}); \
                 the state is unknown, not false"
            ),
        )),
    }
}

/// Read the window's `AXFullScreen` state: `Ok(Some(true/false))` when the
/// attribute is readable, `Ok(None)` when the window has no fullscreen
/// surface (unsupported), `Err` when the read fails.
fn read_fullscreen_state(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<Option<bool>> {
    read_bool_attr(el_ptr, "AXFullScreen", action, role)
}

/// The four window-state readings `minimize` / `enter_fullscreen` / `restore`
/// and the `actions` advertisement decide from: the two state booleans and
/// their settability.
///
/// The settability flags answer "would a write be honored?", not "what is the
/// state" (see [`is_attr_settable`]). [`WindowState::probe`] is the one
/// construction all three sites use, so an advertised verb and its dispatch
/// decide from the same readings and cannot disagree (tenet 3).
#[derive(Debug)]
struct WindowState {
    minimized: Option<bool>,
    fullscreen: Option<bool>,
    minimized_settable: bool,
    fullscreen_settable: bool,
}

impl WindowState {
    /// Probe all four readings strictly: a malformed or failed read is a
    /// platform error (tenet 1), an absent attribute is `None` / not settable.
    fn probe(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<Self> {
        Ok(Self {
            minimized: read_bool_attr(el_ptr, "AXMinimized", action, role)?,
            fullscreen: read_fullscreen_state(el_ptr, action, role)?,
            minimized_settable: is_attr_settable(el_ptr, "AXMinimized")?,
            fullscreen_settable: is_attr_settable(el_ptr, "AXFullScreen")?,
        })
    }
}

/// Whether `minimize` can act on a window: `AXMinimized` is writable, and a
/// window that is currently fullscreen can be taken out of fullscreen first —
/// AppKit silently ignores an `AXMinimized` set while the window is
/// fullscreen, so the verb would otherwise report success while the window
/// stayed fullscreen (the `max, min, max, min` sequence would never
/// minimize). Shared by the verb implementation and the `actions`
/// advertisement, so a window that would honor the call never disappears
/// from `actions` (tenet 3).
fn minimize_supported(state: &WindowState) -> bool {
    state.minimized_settable && (state.fullscreen != Some(true) || state.fullscreen_settable)
}

/// Whether `enter_fullscreen` can act on a window: it can set the native
/// fullscreen state (`true` is already committed, or the attribute is
/// writable) and it can clear `AXMinimized` when that flag is set — an
/// unsupported `AXMinimized` set no-ops silently (see [`is_attr_settable`]),
/// and the fullscreen settle only checks `AXFullScreen`, so the verb would
/// report success while the window stays in the Dock. Shared by the verb
/// implementation and the `actions` advertisement, so a window that would
/// honor the call never disappears from `actions` (tenet 3).
fn enter_fullscreen_supported(state: &WindowState) -> bool {
    (state.fullscreen == Some(true) || state.fullscreen_settable)
        && (state.minimized != Some(true) || state.minimized_settable)
}

/// Whether `restore` can act on a window: every state that reads `true` must
/// be clearable — an unsupported set no-ops silently, so a window reading
/// `AXMinimized=true` or `AXFullScreen=true` without the matching settable
/// attribute would otherwise be advertised and then time out (or report a
/// restore that never happened) — and at least one state must be reachable.
/// Shared by the verb implementation and the `actions` advertisement, so an
/// advertised `restore` always matches the dispatch (tenet 3).
fn restore_supported(state: &WindowState) -> bool {
    (state.minimized != Some(true) || state.minimized_settable)
        && (state.fullscreen != Some(true) || state.fullscreen_settable)
        && (state.minimized_settable || state.fullscreen_settable)
}

/// The answer one attribute read gave on a single settle poll.
#[derive(Debug)]
enum AttrProbe {
    /// The attribute answered a boolean.
    Value(bool),
    /// The attribute does not exist on this window (no such surface).
    Absent,
    /// The object was invalidated mid-transition (`kAXErrorInvalidUIElement`):
    /// churn to retry, not a state. Carries the error for the timeout
    /// diagnosis.
    Churn(String),
}

impl AttrProbe {
    /// The boolean the probe answered, if any.
    fn value(&self) -> Option<bool> {
        match self {
            AttrProbe::Value(v) => Some(*v),
            AttrProbe::Absent | AttrProbe::Churn(_) => None,
        }
    }
}

/// Fold a boolean-attribute read into a probe: the invalidated-object code is
/// transition churn, every other failure propagates (tenet 1).
fn probe_bool(result: Result<Option<bool>>) -> Result<AttrProbe> {
    match result {
        Ok(Some(v)) => Ok(AttrProbe::Value(v)),
        Ok(None) => Ok(AttrProbe::Absent),
        Err(err) => match classify_element_churn(&err) {
            ElementChurn::Gone => Ok(AttrProbe::Churn(err.to_string())),
            ElementChurn::Unreachable | ElementChurn::Fatal => Err(err),
        },
    }
}

/// Fold a settability read into a probe. The probe answers a bare bool, so it
/// cannot be absent.
fn probe_settable(result: Result<bool>) -> Result<AttrProbe> {
    match result {
        Ok(v) => Ok(AttrProbe::Value(v)),
        Err(err) => match classify_element_churn(&err) {
            ElementChurn::Gone => Ok(AttrProbe::Churn(err.to_string())),
            ElementChurn::Unreachable | ElementChurn::Fatal => Err(err),
        },
    }
}

/// The target window's state for one settle poll.
///
/// The target is the window the verb was invoked on. While the transition
/// holds it in the app's enumerated window set the sample is read from the
/// fresh object (the transition can recreate the window); while it is missing
/// from `AXWindows` the cached handle answers instead. The two shapes are
/// kept apart because only an enumerated sample can confirm the promise (see
/// [`SettleRun`]); a cached one still decides whether to re-issue the set and
/// feeds the timeout diagnosis.
enum TargetSample {
    /// The target's own sample from the app's enumerated window set. An
    /// absent `AXFullScreen` reads as `false`: a window without the surface
    /// is by definition not fullscreen.
    Enumerated { fullscreen: bool, settable: bool },
    /// The target was not in the enumerated set; its cached handle answered.
    /// `settable` is always probed: `is_attr_settable` answers a bare bool, so
    /// `AttrProbe::Absent` cannot occur for it.
    Cached {
        fullscreen: AttrProbe,
        settable: AttrProbe,
    },
}

impl TargetSample {
    /// Evaluate the sample against the verb's promise: whether the promised
    /// state holds, and the settability to consult before re-issuing the set
    /// (only meaningful when the promise holds).
    fn evaluate(&self, want: bool) -> (bool, Option<bool>) {
        match self {
            TargetSample::Enumerated {
                fullscreen,
                settable,
            } => {
                let reached = if want {
                    *fullscreen
                } else {
                    // The restore promise is the pair: a bare `false` is also
                    // what the entry transient reports.
                    !*fullscreen && *settable
                };
                (reached, Some(*settable))
            }
            TargetSample::Cached {
                fullscreen,
                settable,
            } => {
                let state = fullscreen.value();
                let settable = settable.value();
                let reached = if want {
                    // A churned settability probe has to be retried: it decides
                    // whether the no-op write may be skipped.
                    state == Some(true) && settable.is_some()
                } else {
                    state == Some(false) && settable == Some(true)
                };
                (reached, settable)
            }
        }
    }
}

/// The live AX object for `target` in the app's enumerated window set, when a
/// fullscreen transition recreated it: [`safe_cf_equal`] matches the fresh
/// object to the cached reference.
///
/// `Ok(None)` when the target is not currently enumerable; an invalidated
/// object under us is churn and answers `Ok(None)` too, while every other
/// failure propagates like every other window-list read (tenet 1). Shared by
/// [`poll_target`] and the `minimize` hand-off after leaving fullscreen, so
/// the two resolve the recreated window the same way.
fn fresh_target_in_app(app: AXUIElementRef, target: AXUIElementRef) -> Result<Option<AXElement>> {
    let Some(windows) = tolerate_churn(ax_windows(app))? else {
        return Ok(None);
    };
    Ok(windows.into_iter().find(|window| {
        !target.is_null()
            && unsafe { safe_cf_equal(window.as_ptr() as CFTypeRef, target as CFTypeRef) }
    }))
}

/// Poll the target window once: its fresh sample when it is in the app's
/// enumerated window set, otherwise its cached handle.
///
/// `Ok(None)` when a window object was invalidated under us (churn — the
/// caller polls again) and `Err` for every other failure: a wedged app or a
/// malformed answer must not be retried into a generic timeout (tenet 1).
/// The enumeration doubles as the target lookup: [`safe_cf_equal`] matches
/// the freshly enumerated window to the cached reference even after the
/// transition recreates it.
fn poll_target(
    app: AXUIElementRef,
    target: AXUIElementRef,
    action: &str,
    role: Role,
) -> Result<Option<TargetSample>> {
    let window = fresh_target_in_app(app, target)?;
    let Some(window) = window else {
        return Ok(Some(TargetSample::Cached {
            fullscreen: probe_bool(read_fullscreen_state(target, action, role))?,
            settable: probe_settable(is_attr_settable(target, "AXFullScreen"))?,
        }));
    };
    let Some(fullscreen) = tolerate_churn(read_fullscreen_state(window.as_ptr(), action, role))?
    else {
        return Ok(None);
    };
    let Some(settable) = tolerate_churn(is_attr_settable(window.as_ptr(), "AXFullScreen"))? else {
        return Ok(None);
    };
    Ok(Some(TargetSample::Enumerated {
        // Absent `AXFullScreen` reads as `false` on an enumerated window.
        fullscreen: fullscreen.unwrap_or(false),
        settable,
    }))
}

/// The continuous-hold bookkeeping behind [`settle_window_fullscreen`]: the
/// promise must hold on the *enumerated* target, without interruption, for
/// [`WINDOW_FULLSCREEN_SETTLE_GRACE`].
///
/// A sample answered by the cached handle never confirms, and it breaks a
/// running hold. The verb's promise is what the next read observes, and that
/// read goes through `AXWindows` (`App::windows`, a re-resolved locator): a
/// settle that returned while the target was still off the list handed the
/// caller a window it could not find (#591: the cached handle used to
/// confirm on its own grace run, sized to outlast the absence measured on an
/// idle machine, 450–650 ms; on a loaded CI runner the absence ran longer and
/// `enter_fullscreen` returned into it).
#[derive(Default)]
struct SettleRun {
    /// When the current run of promise-holding enumerated samples started.
    held_since: Option<Instant>,
    /// Whether any poll found the target in the enumerated window set.
    ever_enumerated: bool,
    /// The most recent poll that found the target off the enumerated set.
    last_absence: Option<Instant>,
    /// The most recent poll that broke (or prevented) a hold, and why.
    last_break: Option<(HoldBreak, Instant)>,
    /// How many running holds were broken before the grace: a target that
    /// flickers in and out of the promise shows up here.
    holds_broken: u32,
}

/// Why a poll did not extend the hold. Diagnosis only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldBreak {
    /// The target was off the enumerated window set.
    Absent,
    /// The target was enumerated but read a state away from the promise.
    WrongState,
    /// No readable sample of the target.
    Unreadable,
}

impl HoldBreak {
    fn describe(self) -> &'static str {
        match self {
            HoldBreak::Absent => "the target was off AXWindows",
            HoldBreak::WrongState => "the enumerated target read a state away from the promise",
            HoldBreak::Unreadable => "the target gave no readable sample",
        }
    }
}

impl SettleRun {
    /// Fold one poll in. `reached` is [`TargetSample::evaluate`]'s verdict.
    fn observe(&mut self, sample: Option<&TargetSample>, reached: bool, now: Instant) {
        // A target off the enumerated set, an unreadable sample, or a state
        // away from the promise is not evidence the caller's next read will
        // see the promise: the hold starts over.
        let cause = match sample {
            Some(TargetSample::Enumerated { .. }) => {
                self.ever_enumerated = true;
                if reached {
                    self.held_since.get_or_insert(now);
                    return;
                }
                HoldBreak::WrongState
            }
            Some(TargetSample::Cached { .. }) => {
                self.last_absence = Some(now);
                HoldBreak::Absent
            }
            None => HoldBreak::Unreadable,
        };
        if self.held_since.take().is_some() {
            self.holds_broken += 1;
        }
        self.last_break = Some((cause, now));
    }

    /// Whether the promise has held on the enumerated target for the grace,
    /// uninterrupted.
    fn confirmed(&self, now: Instant) -> bool {
        self.held_since
            .is_some_and(|since| now.duration_since(since) >= WINDOW_FULLSCREEN_SETTLE_GRACE)
    }

    /// Which half of the confirmation was missing at the deadline, for the
    /// timeout diagnosis: the target was not back in `AXWindows`, or it was
    /// listed but the promised state did not hold there. Every arm carries
    /// the run's history, so a churned last sample keeps the context.
    fn describe_failure(&self, last: Option<&TargetSample>, now: Instant) -> String {
        let state = describe_target(last);
        let headline = match last {
            Some(TargetSample::Cached { .. }) if self.ever_enumerated => format!(
                "the target window left the app's window list (AXWindows) during the \
                 transition and was not re-enumerated before the deadline; {state}"
            ),
            Some(TargetSample::Cached { .. }) => format!(
                "the target window was never enumerated in the app's window list \
                 (AXWindows) during the wait; {state}"
            ),
            Some(TargetSample::Enumerated { .. }) => format!(
                "the target window is enumerated, but the promised state did not hold \
                 there for {} ms; {state}",
                WINDOW_FULLSCREEN_SETTLE_GRACE.as_millis()
            ),
            None => state,
        };
        let ago = |at: Instant| now.saturating_duration_since(at).as_millis();
        let absence = match self.last_absence {
            Some(at) => format!("last off AXWindows {} ms before the deadline", ago(at)),
            None => "never off AXWindows".to_string(),
        };
        let interruption = match self.last_break {
            Some((cause, at)) => format!(
                "last interruption {} ms before the deadline: {}",
                ago(at),
                cause.describe()
            ),
            None => "no interruption".to_string(),
        };
        format!(
            "{headline} [ever enumerated: {}; {absence}; {interruption}; holds broken \
             before the grace: {}]",
            self.ever_enumerated, self.holds_broken
        )
    }
}

/// Render one poll for the timeout diagnosis (tenet 6: only the failure path
/// formats it).
fn describe_target(sample: Option<&TargetSample>) -> String {
    match sample {
        None => "the app did not answer a readable sample of the target window".to_string(),
        Some(TargetSample::Enumerated {
            fullscreen,
            settable,
        }) => format!("the target window reads AXFullScreen={fullscreen} (settable={settable})"),
        Some(TargetSample::Cached {
            fullscreen,
            settable,
        }) => format!(
            "the target window is not enumerable; its cached handle {}",
            describe_cached_target(fullscreen, settable)
        ),
    }
}

/// The cached handle's clause of the timeout diagnosis.
fn describe_cached_target(fullscreen: &AttrProbe, settable: &AttrProbe) -> String {
    match (fullscreen, settable) {
        (AttrProbe::Churn(err), _) => format!("could not be read: {err}"),
        (AttrProbe::Absent, _) => "has no AXFullScreen attribute".to_string(),
        (AttrProbe::Value(state), AttrProbe::Churn(err)) => {
            format!("reads AXFullScreen={state} but its settability probe was invalidated: {err}")
        }
        (AttrProbe::Value(state), AttrProbe::Value(settable)) => {
            format!("reads AXFullScreen={state} (settable={settable})")
        }
        // A settability probe answers a bare bool, so an absent one is
        // unreachable; report the state rather than dropping the clause.
        (AttrProbe::Value(state), _) => format!("reads AXFullScreen={state}"),
    }
}

/// Wait for the fullscreen transition to commit or clear, re-issuing the
/// absolute set while the promise is unconfirmed.
///
/// `want` is the state the verb promises: `true` after `enter_fullscreen`,
/// `false` after `restore` (and after the fullscreen exit `minimize` performs
/// before it iconifies). The wait succeeds once the *target* window's own state
/// holds the promise for [`WINDOW_FULLSCREEN_SETTLE_GRACE`] without
/// interruption *while enumerated in `AXWindows`*, so a caller's next
/// `App::windows()` finds it. The target is followed across the transition's
/// window recreation through the app's enumerated window set
/// ([`safe_cf_equal`]). While it is missing from `AXWindows` its cached handle
/// is read for the re-issue decision and the diagnosis, but cannot confirm
/// (see [`SettleRun`]).
///
/// Every unconfirmed iteration re-issues `AXFullScreen = want`: a set that
/// lands mid-transition is discarded rather than queued (see the constants),
/// and absolute sets make the repetition safe — with one exception. A desired
/// state the window refuses to write (`AXFullScreen=true` with
/// `IsAttributeSettable=false`) has nothing a set could change, and issuing
/// it anyway can be rejected, turning an advertised no-op `enter_fullscreen`
/// into a failure. For `want=false` the promise is `AXFullScreen=false` *and*
/// `IsAttributeSettable=true`; the hold must outlast the transition because
/// the entry transient reports exactly that pair (see
/// [`WINDOW_FULLSCREEN_SETTLE_GRACE`]).
///
/// An invalidated object mid-transition (`kAXErrorInvalidUIElement`) is
/// retried on the next tick and recorded for the deadline diagnosis; every
/// other failure returns with its own error, and an unmet promise fails at
/// the deadline with a [`xa11y_core::Diagnosis`] naming what was last
/// observed (tenet 1, tenet 6).
fn settle_window_fullscreen(
    el_ptr: AXUIElementRef,
    want: bool,
    action: &str,
    role: Role,
) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    let deadline = Instant::now() + WINDOW_FULLSCREEN_SETTLE_BUDGET;
    let mut run = SettleRun::default();
    // The last expected (invalidated-object) retry-set failure, recorded for
    // the deadline diagnosis. Kept apart from the poll so the diagnosis stays
    // bounded across ~100 retries (tenet 6: context is collected on the
    // failure path only).
    let mut last_set_error: Option<String> = None;
    loop {
        let now = Instant::now();
        let sample = poll_target(app.as_ptr(), el_ptr, action, role)?;
        let (reached, settable) = match &sample {
            Some(sample) => sample.evaluate(want),
            None => (false, None),
        };
        run.observe(sample.as_ref(), reached, now);
        if run.confirmed(now) {
            return Ok(());
        }
        // Re-issue the absolute set while the promise is unconfirmed — not
        // only when the state still reads the wrong value: the entry
        // transient reads the desired pair for a while (see the constants),
        // and a set that lands mid-transition is discarded, so the loop keeps
        // issuing until the confirmation proves the state committed. Setting
        // the desired value on a window that already holds it is a no-op. An
        // already-desired state on an attribute the window refuses to write
        // (`AXFullScreen=true` with `IsAttributeSettable=false`) has nothing
        // to set: issuing the write can be rejected and turn an advertised
        // no-op `enter_fullscreen` into a failure, while the confirmation above
        // still
        // validates the read. The one expected failure is the invalidated
        // object (`kAXErrorInvalidUIElement`): the set is retried on the next
        // tick and recorded for the deadline diagnosis, superseded by a later
        // successful retry. Every other failure is real — the settability
        // probe and the snapshot reads propagate theirs — so it must not be
        // retried into a generic timeout; it returns with its original code
        // (tenet 1, tenet 6).
        if !(reached && settable == Some(false)) {
            match set_bool_attr(el_ptr, "AXFullScreen", want, action, role) {
                Ok(()) => {
                    // A later successful retry supersedes the recorded
                    // failure: the diagnosis must describe the last attempt,
                    // not any attempt.
                    last_set_error = None;
                }
                Err(err) => match classify_element_churn(&err) {
                    // The invalidated object is the one expected retry: the
                    // next tick re-issues the set, and the deadline diagnosis
                    // records the last attempt.
                    ElementChurn::Gone => last_set_error = Some(err.to_string()),
                    ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
                },
            }
        }
        if Instant::now() >= deadline {
            let mut last = run.describe_failure(sample.as_ref(), Instant::now());
            if let Some(err) = last_set_error {
                last = format!("{last}; the last retry set failed: {err}");
            }
            let condition = if want {
                "window enters fullscreen (AXFullScreen=true)"
            } else {
                // The restore promise is the pair (see `TargetSample::evaluate`):
                // a bare `false` is also the entry transient.
                "window leaves fullscreen (AXFullScreen=false with IsAttributeSettable=true)"
            };
            return Err(Error::timeout(WINDOW_FULLSCREEN_SETTLE_BUDGET).diagnose(
                xa11y_core::Diagnosis::new()
                    .condition(condition)
                    .last_observed(last),
            ));
        }
        std::thread::sleep(WINDOW_FULLSCREEN_SETTLE_INTERVAL);
    }
}

/// Wait until a boolean AX attribute reads `want`, re-issuing the absolute set
/// while it does not.
///
/// The minimized state is applied asynchronously by AppKit on native windows
/// (see [`WINDOW_MINIMIZED_SETTLE_BUDGET`]): the set returns while the Dock
/// animation still runs, so a caller reading the state right after `minimize`
/// / `restore` would observe the previous value. This is the same promise the
/// fullscreen settle makes, for the state whose transition is the miniaturize
/// animation. Re-issuing the absolute set each unconfirmed iteration is a
/// no-op when the set is already queued, and recovers one that was lost. An
/// invalidated object is retried against the app's enumerated window set; a
/// wedged process or a malformed answer propagates instead of being retried
/// into a generic timeout, and an unmet promise fails at the deadline with the
/// last observed reading (tenet 1, tenet 6).
fn settle_bool_attr(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    want: bool,
    action: &str,
    role: Role,
) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    let deadline = Instant::now() + WINDOW_MINIMIZED_SETTLE_BUDGET;
    let mut last_observed = format!("{attr_name} never answered a boolean");
    // The object can be recreated while the transition runs; when that happens
    // the freshly enumerated window takes over (see `fresh_target_in_app`).
    let mut fresh: Option<AXElement> = None;
    loop {
        let target = fresh.as_ref().map_or(el_ptr, |el| el.as_ptr());
        // The read and the set it may trigger can both hit the invalidated
        // object, so both outcomes are collected and classified once below:
        // the set races the same recreation the read does, and a `Gone` set
        // must be retried against the freshly enumerated window rather than
        // surfaced as a platform error (tenet 1, tenet 6).
        let failure = match read_bool_attr(target, attr_name, action, role) {
            Ok(Some(value)) => {
                last_observed = format!("{attr_name} reads {value}");
                if value == want {
                    return Ok(());
                }
                set_bool_attr(target, attr_name, want, action, role).err()
            }
            Ok(None) => {
                last_observed = format!("the window has no {attr_name} attribute");
                None
            }
            Err(err) => Some(err),
        };
        if let Some(err) = failure {
            match classify_element_churn(&err) {
                ElementChurn::Gone => {
                    last_observed = err.to_string();
                    if let Some(found) = fresh_target_in_app(app.as_ptr(), el_ptr)? {
                        fresh = Some(found);
                    }
                }
                ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
            }
        }
        if Instant::now() >= deadline {
            let condition = if want {
                "window enters minimized state (AXMinimized=true)"
            } else {
                "window leaves minimized state (AXMinimized=false)"
            };
            return Err(Error::timeout(WINDOW_MINIMIZED_SETTLE_BUDGET).diagnose(
                xa11y_core::Diagnosis::new()
                    .condition(condition)
                    .last_observed(last_observed),
            ));
        }
        std::thread::sleep(WINDOW_MINIMIZED_SETTLE_INTERVAL);
    }
}

// ── Window Position Settling ──────────────────────────────────────────────────

/// Read `AXPosition` without collapsing a failed or malformed response into
/// "no position". Snapshot construction may leave optional bounds absent,
/// but an action that is verifying its own write must surface why the promise
/// could not be checked (tenet 1, tenet 6).
fn read_window_position(
    el_ptr: AXUIElementRef,
    action: &str,
    role: Role,
) -> Result<Option<(f64, f64)>> {
    match read_raw_attr(el_ptr, "AXPosition") {
        RawAttr::Value(value) => {
            let mut point = CGPoint::default();
            let ok = unsafe {
                safe_ax_value_get_value(
                    value,
                    AX_VALUE_CGPOINT,
                    &mut point as *mut _ as *mut c_void,
                )
            };
            unsafe { safe_cf_release(value) };
            if ok {
                Ok(Some((point.x, point.y)))
            } else {
                Err(Error::platform(
                    -1,
                    format!(
                        "AXPosition returned a non-point value while handling {action} on a \
                         {role}; the position is unknown"
                    ),
                ))
            }
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::platform(
            code as i64,
            format!(
                "AXPosition read failed while handling {action} on a {role} (AXError {code}); \
                 the position is unknown"
            ),
        )),
    }
}

/// Set the window's absolute AX position once.
fn set_window_position(
    el_ptr: AXUIElementRef,
    x: i32,
    y: i32,
    action: &str,
    role: Role,
) -> Result<()> {
    let value = unsafe { safe_ax_value_create_cg_point(f64::from(x), f64::from(y)) };
    if value.is_null() {
        return Err(Error::platform(
            -1,
            "Failed to create CGPoint AXValue for window move",
        ));
    }
    let attr = CFString::new("AXPosition");
    let err = do_set_attribute(el_ptr, &attr, value);
    unsafe { safe_cf_release(value) };
    if err != AX_ERROR_SUCCESS {
        return Err(action_error(
            el_ptr,
            err,
            action,
            role,
            "Set AXPosition failed",
        ));
    }
    Ok(())
}

fn geometry_near(actual: (f64, f64), expected: (f64, f64)) -> bool {
    (actual.0 - expected.0).abs() <= WINDOW_GEOMETRY_TOLERANCE
        && (actual.1 - expected.1).abs() <= WINDOW_GEOMETRY_TOLERANCE
}

/// Wait until `AXPosition` reflects the requested logical coordinates,
/// re-issuing the absolute write while it does not.
///
/// macOS can return success before a toolkit bridge applies the write, and an
/// egui window settling from an earlier transition can discard that accepted
/// first write. A caller-side sleep cannot recover a dropped write; retrying
/// the idempotent absolute set can. Invalidated elements are refreshed from
/// the owning application's window list, while every non-churn failure keeps
/// its original error instead of turning into a generic timeout (tenet 1).
fn settle_window_position(
    el_ptr: AXUIElementRef,
    x: i32,
    y: i32,
    action: &str,
    role: Role,
) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    let deadline = Instant::now() + WINDOW_GEOMETRY_SETTLE_BUDGET;
    let expected = (f64::from(x), f64::from(y));
    let mut last_observed: Option<String> = None;
    let mut fresh: Option<AXElement> = None;
    loop {
        let target = fresh.as_ref().map_or(el_ptr, |el| el.as_ptr());
        let failure = match read_window_position(target, action, role) {
            Ok(Some(actual)) => {
                last_observed = Some(format!("AXPosition reads ({}, {})", actual.0, actual.1));
                if geometry_near(actual, expected) {
                    return Ok(());
                }
                set_window_position(target, x, y, action, role).err()
            }
            Ok(None) => {
                last_observed = Some("the window has no AXPosition attribute".to_string());
                set_window_position(target, x, y, action, role).err()
            }
            Err(err) => Some(err),
        };
        if let Some(err) = failure {
            match classify_element_churn(&err) {
                ElementChurn::Gone => {
                    last_observed = Some(err.to_string());
                    if let Some(found) = fresh_target_in_app(app.as_ptr(), el_ptr)? {
                        fresh = Some(found);
                    }
                }
                ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::timeout(WINDOW_GEOMETRY_SETTLE_BUDGET).diagnose(
                xa11y_core::Diagnosis::new()
                    .condition(format!(
                        "window reaches position ({x}, {y}) within {} points",
                        WINDOW_GEOMETRY_TOLERANCE
                    ))
                    .last_observed(last_observed.unwrap_or_else(|| {
                        "AXPosition could not be observed before the deadline".to_string()
                    })),
            ));
        }
        std::thread::sleep(WINDOW_GEOMETRY_SETTLE_INTERVAL);
    }
}

// ── Window Size Settling ──────────────────────────────────────────────────────

/// Read `AXSize` with the same failure distinction as
/// [`read_window_position`]: a failed or malformed read is an error, and only
/// a genuinely absent attribute is `None`.
fn read_window_size(
    el_ptr: AXUIElementRef,
    action: &str,
    role: Role,
) -> Result<Option<(f64, f64)>> {
    match read_raw_attr(el_ptr, "AXSize") {
        RawAttr::Value(value) => {
            let mut size = CGSize::default();
            let ok = unsafe {
                safe_ax_value_get_value(value, AX_VALUE_CGSIZE, &mut size as *mut _ as *mut c_void)
            };
            unsafe { safe_cf_release(value) };
            if ok {
                Ok(Some((size.width, size.height)))
            } else {
                Err(Error::platform(
                    -1,
                    format!(
                        "AXSize returned a non-size value while handling {action} on a \
                         {role}; the size is unknown"
                    ),
                ))
            }
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::platform(
            code as i64,
            format!(
                "AXSize read failed while handling {action} on a {role} (AXError {code}); \
                 the size is unknown"
            ),
        )),
    }
}

/// Set the window's absolute AX size once.
fn set_window_size(
    el_ptr: AXUIElementRef,
    width: u32,
    height: u32,
    action: &str,
    role: Role,
) -> Result<()> {
    let value = unsafe { safe_ax_value_create_cg_size(f64::from(width), f64::from(height)) };
    if value.is_null() {
        return Err(Error::platform(
            -1,
            "Failed to create CGSize AXValue for window resize",
        ));
    }
    let attr = CFString::new("AXSize");
    let err = do_set_attribute(el_ptr, &attr, value);
    unsafe { safe_cf_release(value) };
    if err != AX_ERROR_SUCCESS {
        return Err(action_error(el_ptr, err, action, role, "Set AXSize failed"));
    }
    Ok(())
}

/// What `resize_to` saw when the window would not take the requested size:
/// the requested rect and the one the window actually reports, so the error
/// says which way it was refused without a re-run.
fn describe_refused_resize(
    width: u32,
    height: u32,
    observed_size: (f64, f64),
    origin: &str,
) -> String {
    let (ow, oh) = observed_size;
    format!(
        "requested {width}x{height} at origin {origin} but the window reports {ow}x{oh}; \
         AXSize accepted the write and the window refused it. The window server \
         constrains windows to the screen's visible frame (and the app to its own \
         minimum/maximum size), so a size that does not fit there is refused"
    )
}

/// Wait until `AXSize` reflects the requested size, re-issuing the absolute
/// write while it does not — the `resize_to` counterpart of
/// [`settle_window_position`].
///
/// `AXUIElementSetAttributeValue(AXSize)` returns success when the window
/// *received* the request, not when it applied it, and macOS silently keeps
/// the old frame for a size the window cannot take (one already filling the
/// visible height cannot grow taller). Without this read-back `resize_to`
/// reported `ok` for a resize that never happened (tenet 1). Element churn is
/// handled exactly as in the move settle.
fn settle_window_size(
    el_ptr: AXUIElementRef,
    width: u32,
    height: u32,
    action: &str,
    role: Role,
) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    let deadline = Instant::now() + WINDOW_GEOMETRY_SETTLE_BUDGET;
    let expected = (f64::from(width), f64::from(height));
    // Only raw observations are kept while polling; the diagnosis text (and
    // the extra origin read it needs) is built once, at the deadline.
    let mut last_size: Option<(f64, f64)> = None;
    let mut last_other: Option<String> = None;
    let mut fresh: Option<AXElement> = None;
    loop {
        let target = fresh.as_ref().map_or(el_ptr, |el| el.as_ptr());
        let failure = match read_window_size(target, action, role) {
            Ok(Some(actual)) => {
                if geometry_near(actual, expected) {
                    return Ok(());
                }
                last_size = Some(actual);
                last_other = None;
                set_window_size(target, width, height, action, role).err()
            }
            Ok(None) => {
                last_size = None;
                last_other = Some("the window has no AXSize attribute".to_string());
                set_window_size(target, width, height, action, role).err()
            }
            Err(err) => Some(err),
        };
        if let Some(err) = failure {
            match classify_element_churn(&err) {
                ElementChurn::Gone => {
                    last_size = None;
                    last_other = Some(err.to_string());
                    if let Some(found) = fresh_target_in_app(app.as_ptr(), el_ptr)? {
                        fresh = Some(found);
                    }
                }
                ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
            }
        }
        if Instant::now() >= deadline {
            let observed = match (last_size, last_other) {
                (Some(actual), _) => {
                    // The origin is context for the error only, so a failed
                    // origin read is reported in that context rather than
                    // replacing the size the window did report.
                    let origin = match read_window_position(target, action, role) {
                        Ok(Some((x, y))) => format!("({x}, {y})"),
                        Ok(None) => "(no AXPosition)".to_string(),
                        Err(err) => format!("(origin unreadable: {err})"),
                    };
                    describe_refused_resize(width, height, actual, &origin)
                }
                (None, Some(other)) => other,
                (None, None) => "AXSize could not be observed before the deadline".to_string(),
            };
            return Err(Error::timeout(WINDOW_GEOMETRY_SETTLE_BUDGET).diagnose(
                xa11y_core::Diagnosis::new()
                    .condition(format!(
                        "window reaches size {width}x{height} within {} points",
                        WINDOW_GEOMETRY_TOLERANCE
                    ))
                    .last_observed(observed),
            ));
        }
        std::thread::sleep(WINDOW_GEOMETRY_SETTLE_INTERVAL);
    }
}

// ── Document semantics (#476) ─────────────────────────────────────────────────

struct AxSemantics {
    heading_level: Option<u32>,
    landmark_type: Option<xa11y_core::Landmark>,
    link_url: Option<String>,
    live_region: Option<xa11y_core::LiveRegionInfo>,
}

/// The document semantics in one element's batch read: an `AXHeading`'s
/// level is its `AXValue`, a landmark's kind its `AXLandmark*` subrole, a
/// link's target its `AXURL`, and the `AXARIA*` live attributes.
fn document_semantics(attrs: &ResolvedAttrs, role: Role) -> AxSemantics {
    let live: xa11y_core::LiveRegionInfo = xa11y_core::LiveRegionParts {
        politeness: attrs
            .aria_live
            .as_deref()
            .and_then(|v| xa11y_core::Politeness::from_snake_case(&v.trim().to_ascii_lowercase())),
        atomic: attrs.aria_atomic,
        relevant: attrs.aria_relevant.clone().filter(|r| !r.is_empty()),
    }
    .into();
    AxSemantics {
        heading_level: (role == Role::Heading)
            .then_some(attrs.value_int)
            .flatten()
            .and_then(|level| u32::try_from(level).ok())
            .filter(|level| *level > 0),
        landmark_type: (role == Role::Landmark).then(|| {
            attrs
                .subrole_str
                .as_deref()
                .and_then(landmark_from_subrole)
                .unwrap_or(xa11y_core::Landmark::Unknown)
        }),
        link_url: (role == Role::Link).then(|| attrs.url.clone()).flatten(),
        live_region: (!live.is_empty()).then_some(live),
    }
}

fn landmark_from_subrole(subrole: &str) -> Option<xa11y_core::Landmark> {
    use xa11y_core::Landmark;
    Some(match subrole {
        "AXLandmarkBanner" => Landmark::Banner,
        "AXLandmarkComplementary" => Landmark::Complementary,
        "AXLandmarkContentInfo" => Landmark::ContentInfo,
        "AXLandmarkForm" => Landmark::Form,
        "AXLandmarkMain" => Landmark::Main,
        "AXLandmarkNavigation" => Landmark::Navigation,
        "AXLandmarkRegion" => Landmark::Region,
        "AXLandmarkSearch" => Landmark::Search,
        _ => return None,
    })
}

/// The AX attribute holding a relation, and whether it holds one element
/// (`AXTitleUIElement`) rather than an array. `None` for a kind AX does not
/// report (see the capability table).
fn ax_relation_attribute(kind: xa11y_core::RelationKind) -> Option<(&'static str, bool)> {
    use xa11y_core::RelationKind as K;
    Some(match kind {
        K::LabelledBy => ("AXTitleUIElement", true),
        K::LabelFor => ("AXServesAsTitleForUIElements", false),
        K::Details => ("AXDetailsElements", false),
        K::ErrorMessage => ("AXErrorMessageElements", false),
        K::Owns => ("AXOwns", false),
        K::Linked => ("AXLinkedUIElements", false),
        K::DescribedBy
        | K::DescriptionFor
        | K::DetailsFor
        | K::Controls
        | K::ControlledBy
        | K::FlowsTo
        | K::FlowsFrom
        | K::ErrorFor
        | K::MemberOf => return None,
    })
}

/// One element-valued attribute: `Ok(None)` when absent, an error when the
/// app did not answer or answered with something other than an element
/// (never read as "no element", tenet 1).
fn ax_single_element(element: AXUIElementRef, attribute: &str) -> Result<Option<AXElement>> {
    match read_raw_attr(element, attribute) {
        RawAttr::Value(v) => {
            if unsafe { safe_ax_is_element(v) } {
                Ok(Some(AXElement::from_owned(v as AXUIElementRef)))
            } else {
                unsafe { safe_cf_release(v) };
                Err(Error::platform(
                    -1,
                    format!(
                        "{attribute} returned a non-element value; the provider answered \
                         malformed, not empty"
                    ),
                ))
            }
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::platform(
            code as i64,
            format!("{attribute} read failed (AXError {code}); the relation is unknown, not empty"),
        )),
    }
}

// ── Role Mapping ──────────────────────────────────────────────────────────────

/// Whether an AX role/subrole pair is a secure (password) text field.
///
/// AppKit's `NSSecureTextField` reports `AXSecureTextField` as its role;
/// WebKit and AccessKit report `AXTextField` with `AXSecureTextField` as the
/// subrole. Both map to `Role::TextField` in [`map_ax_role`], so this is the
/// only place the distinction survives.
fn is_secure_text_field(role: &str, subrole: Option<&str>) -> bool {
    role == "AXSecureTextField" || subrole == Some("AXSecureTextField")
}

fn map_ax_role(role: &str, subrole: Option<&str>) -> Role {
    match subrole {
        Some("AXDialog") => return Role::Dialog,
        Some("AXApplicationAlert") | Some("AXSystemAlert") => return Role::Alert,
        Some("AXTabButton") => return Role::Tab,
        Some("AXOutlineRow") => return Role::TreeItem,
        Some("AXHeading") => return Role::Heading,
        Some("AXSwitch") => return Role::Switch,
        // WebKit exposes <nav>/role="navigation" as AXGroup with this
        // subrole; matches AT-SPI's "navigation" landmark mapping.
        Some(sub) if sub.starts_with("AXLandmark") => return Role::Landmark,
        _ => {}
    }

    match role {
        "AXApplication" => Role::Application,
        "AXWindow" | "AXSheet" | "AXDrawer" => {
            if role == "AXSheet" {
                Role::Dialog
            } else {
                Role::Window
            }
        }
        "AXButton" => match subrole {
            Some("AXDisclosureTriangle") => Role::TreeItem,
            _ => Role::Button,
        },
        "AXRadioButton" => Role::RadioButton,
        "AXCheckBox" => Role::CheckBox,
        "AXTextField" | "AXSecureTextField" => Role::TextField,
        "AXTextArea" => Role::TextArea,
        "AXStaticText" => Role::StaticText,
        "AXComboBox" | "AXPopUpButton" => Role::ComboBox,
        "AXMenuButton" => match subrole {
            Some("AXSegment") => Role::Button,
            _ => Role::ComboBox,
        },
        "AXList" => Role::List,
        "AXTable" => Role::Table,
        "AXOutline" => Role::List,
        "AXRow" => Role::TableRow,
        "AXCell" => Role::TableCell,
        "AXMenu" => Role::Menu,
        "AXMenuItem" | "AXMenuBarItem" => Role::MenuItem,
        "AXMenuBar" | "AXMenuBarExtra" => Role::MenuBar,
        "AXTabGroup" => Role::TabGroup,
        "AXToolbar" => Role::Toolbar,
        "AXScrollBar" => Role::ScrollBar,
        "AXSlider" => Role::Slider,
        "AXImage" => Role::Image,
        "AXLink" => Role::Link,
        "AXGroup" | "AXScrollArea" | "AXLayoutArea" | "AXRadioGroup" | "AXBrowser" | "AXColumn" => {
            Role::Group
        }
        "AXDialog" => Role::Dialog,
        "AXProgressIndicator" | "AXBusyIndicator" | "AXLevelIndicator" => Role::ProgressBar,
        "AXDisclosureTriangle" => Role::TreeItem,
        "AXHeading" | "Heading" => Role::Heading,
        "AXSplitGroup" => Role::SplitGroup,
        "AXSplitter" => Role::Separator,
        "AXWebArea" => Role::WebArea,
        "AXIncrementor" => Role::SpinButton,
        "AXToolTip" => Role::Tooltip,
        "AXStatusBar" => Role::Status,
        "AXValueIndicator" => Role::ScrollThumb,
        "AXGrid" => Role::Table,
        // Table-header sort buttons (view-based NSTableView headers).
        "AXSortButton" => Role::Button,
        // AppKit's help-tag tooltip role (AXToolTip above is the WebKit one).
        "AXHelpTag" => Role::Tooltip,
        // Transient floating container (NSPopover); a content group like its
        // GTK popover counterpart, not a dialog.
        "AXPopover" => Role::Group,
        "AXDockItem" => Role::Button,
        "AXGrowArea" => Role::ScrollThumb,
        "AXColorWell" | "AXRuler" | "AXMatte" => Role::Unknown,
        // Elements with no role or the explicit AXUnknown placeholder.
        "" | "AXUnknown" => Role::Unknown,
        _ => xa11y_core::unknown_role(role),
    }
}

// ── Action Mapping ───────────────────────────────────────────────────────────
//
// The macOS action system has two kinds of operations:
//
// 1. **AX actions** — invoked via `AXUIElementPerformAction`. These are
//    freeform strings like "AXPress", "AXShowMenu", "AXCustomThing".
//
// 2. **Attribute-based actions** — performed by setting an attribute via
//    `AXUIElementSetAttributeValue` (e.g. `AXFocused = true` for Focus).
//
// The mapping table below covers only AX actions (type 1). Attribute-based
// actions are handled directly in `perform_action`.
//
// For **reading** which actions an element supports:
//   - `AXUIElementCopyActionNames` returns the element's AX action list
//   - Known names (e.g. "AXPress") map to well-known action name strings
//   - Unknown names following `AXFooBar` convention become `snake_case` custom
//     actions (e.g. "AXCustomThing" → "custom_thing")
//   - Implicit actions are added from settable attributes (Focus, SetValue)
//
// For **performing** a custom action by name:
//   1. Convert `snake_case` → `AXPascalCase` (e.g. "custom_thing" → "AXCustomThing")
//   2. Check if the element's action list contains that name
//   3. If not, try the literal `snake_case` name
//   4. If neither matches, return error

/// Map an AX action name to a well-known xa11y action name string.
///
/// Returns `None` for unrecognized names (which may be custom actions).
fn ax_action_to_name(ax_name: &str) -> Option<&'static str> {
    match ax_name {
        "AXPress" | "AXConfirm" => Some("press"),
        "AXShowMenu" => Some("show_menu"),
        "AXIncrement" => Some("increment"),
        "AXDecrement" => Some("decrement"),
        "AXRaise" => Some("activate"),
        "AXScrollToVisible" => Some("scroll_into_view"),
        _ => None,
    }
}

/// The AX action `press` dispatches to, given the element's native action
/// names.
///
/// `AXPress` when the element has it. Otherwise `AXOpen` when that is the
/// element's activation action: Finder sidebar items, for example, advertise
/// `AXOpen` and no `AXPress`. This is the primary-activation reading of
/// tenet 3, the same way Windows `press` dispatches to the element's
/// activation pattern. Nothing is tried after a failure (tenet 1).
///
/// Failing both, `AXConfirm` when the element advertises it: the snapshot
/// lists `press` for an `AXConfirm`-only element (`ax_action_to_name`), and
/// tenet 3 requires that the advertised action is the one invoked rather
/// than an `AXPress` the element never offered. Every other shape keeps
/// `AXPress`, which is what `press` has always sent.
fn press_ax_action(ax_actions: &[String]) -> &'static str {
    let has = |name: &str| ax_actions.iter().any(|a| a == name);
    if has("AXPress") {
        "AXPress"
    } else if has("AXOpen") {
        "AXOpen"
    } else if has("AXConfirm") {
        "AXConfirm"
    } else {
        "AXPress"
    }
}

/// Convert an `AXPascalCase` name to `snake_case`, stripping the `AX` prefix.
///
/// `"AXCustomThing"` → `"custom_thing"`
/// `"AXPress"` → `"press"`
/// `"NoPrefix"` → `"no_prefix"`
fn ax_pascal_to_snake(ax_name: &str) -> String {
    let name = ax_name.strip_prefix("AX").unwrap_or(ax_name);
    let mut result = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}

/// Convert a `snake_case` name to `AXPascalCase`.
///
/// `"custom_thing"` → `"AXCustomThing"`
/// `"press"` → `"AXPress"`
fn snake_to_ax_pascal(snake: &str) -> String {
    let mut result = String::with_capacity(snake.len() + 2);
    result.push_str("AX");
    let mut capitalize_next = true;
    for ch in snake.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

// ── Lightweight selector matching (no ElementData) ───────────────────────────

use xa11y_core::selector::{match_op, SimpleSelector};

/// Test whether a raw AXElement matches a SimpleSelector, fetching only the
/// attributes the selector actually inspects. This avoids building a full
/// ElementData (15-20 AX API calls) for elements that will be discarded.
#[cfg(test)]
fn matches_ax(ax: AXUIElementRef, simple: &SimpleSelector) -> bool {
    matches_ax_with_role(ax, simple, None, None)
}

/// Attributes the lightweight `matches_ax_with_role` fast path knows how to
/// resolve without building a full `ElementData`. Any selector filter whose
/// attr is *not* in this list forces a fall-through to `build_snapshot_data`
/// combined with `xa11y_core::selector::matches_simple`, so normalized state
/// attrs (`enabled`, `checked`, `focused`, …) and raw platform-attr map keys
/// still match correctly.
const FAST_PATH_ATTRS: &[&str] = &["role", "name", "value", "description"];

/// Like `matches_ax` but accepts a pre-resolved role to avoid redundant AX
/// API calls and the owning pid needed to resolve process-scoped state.
fn matches_ax_with_role(
    ax: AXUIElementRef,
    simple: &SimpleSelector,
    precomputed_role: Option<Role>,
    pid: Option<u32>,
) -> bool {
    // If any filter targets an attr the fast path can't resolve, fall through
    // to a full snapshot + canonical core matcher. This keeps selectors like
    // `[enabled="true"]`, `[checked="on"]`, `[focused="true"]` (and raw AX
    // platform keys) correct.
    if simple
        .filters
        .iter()
        .any(|f| !FAST_PATH_ATTRS.contains(&f.attr.as_str()))
    {
        if ax.is_null() {
            return false;
        }
        // Snapshot handle is 0 — this path is only used to decide whether to
        // keep a candidate; callers re-resolve via the provider cache after
        // the match set is assembled. Preserve `pid`: state such as `active`
        // depends on the owning process and must match the final snapshot. A
        // snapshot that cannot be built is a candidate that does not match
        // (the selector engines treat an unreadable node as absent, not as a
        // hard failure).
        let data = match build_snapshot_data(ax, pid, 0) {
            Ok(d) => d,
            Err(_) => return false,
        };
        return xa11y_core::selector::matches_simple(&data, simple);
    }

    // Resolve role only if the selector cares about it and it wasn't pre-computed.
    let needs_role = simple.role.is_some() || simple.filters.iter().any(|f| f.attr == "role");

    let role = if needs_role {
        match precomputed_role {
            Some(r) => Some(r),
            None => {
                let role_str = match ax_string(ax, "AXRole") {
                    Some(s) => s,
                    None => return false,
                };
                let subrole_str = ax_string(ax, "AXSubrole");
                Some(map_ax_role(&role_str, subrole_str.as_deref()))
            }
        }
    } else {
        precomputed_role
    };

    if let Some(ref role_match) = simple.role {
        match role_match {
            xa11y_core::selector::RoleMatch::Normalized(expected) => {
                if role != Some(*expected) {
                    return false;
                }
            }
            xa11y_core::selector::RoleMatch::Platform(platform_role) => {
                // Match against the original AX role string
                let ax_role = ax_string(ax, "AXRole").unwrap_or_default();
                if ax_role != *platform_role {
                    return false;
                }
            }
        }
    }

    for filter in &simple.filters {
        let attr_value: Option<String> = match filter.attr.as_str() {
            "role" => role.map(|r| r.to_snake_case().to_string()),
            "name" => {
                // Mirror build_element_data name logic.
                derived_ax_name(ax, role)
            }
            "value" => ax_value_string(ax),
            "description" => {
                let description = ax_string(ax, "AXDescription");
                let name = accessible_name(
                    role,
                    ax_string(ax, "AXTitle"),
                    || ax_value_string(ax),
                    || description.clone(),
                );
                accessible_description(name.as_ref(), description, ax_string(ax, "AXHelp"))
            }
            // Unreachable: we bailed to the full-snapshot path above for any
            // filter whose attr isn't in FAST_PATH_ATTRS.
            _ => unreachable!("non-fast-path attr should have taken the fallback above"),
        };

        if !match_op(&filter.op, &filter.value, attr_value.as_deref()) {
            return false;
        }
    }

    true
}

// ── MacOS Provider ────────────────────────────────────────────────────────────

/// The TCC grant hint for the Accessibility permission, as carried in
/// [`Error::PermissionDenied`].
///
/// The System Settings entry that holds the grant was renamed on macOS 27:
/// `Privacy & Security → Accessibility` became `Privacy & Security →
/// Device Control and Data Access` (the `Accessibility` entry no longer
/// exists), so the instruction names the pane for the running macOS version.
/// The CLI recognizes this exact denial and offers to open the matching pane —
/// see `cli::offer_accessibility_settings_hint` — against the same builder,
/// so the error message and the prompt can never disagree about where to
/// click.
pub fn accessibility_grant_instructions() -> String {
    match macos_major_version() {
        Ok(major) => instructions_for_major(major),
        // Naming one pane on a guess would send the user to an entry that
        // may not exist on their system; name both, by version.
        Err(_) => "Enable Accessibility in System Settings → Privacy & Security → \
                   Device Control and Data Access (macOS 27 and later) or \
                   Privacy & Security → Accessibility (macOS 26 and earlier)"
            .to_string(),
    }
}

/// The instruction text for a given macOS major version. Split out so the
/// version-dependent pane name is unit-testable without a sysctl round-trip.
///
/// The second-level entry under `Privacy & Security` was renamed on macOS 27:
/// `Accessibility` as its own entry is gone, and the grant now lives in
/// `Device Control and Data Access`.
fn instructions_for_major(major: u32) -> String {
    let entry = if major >= 27 {
        "Privacy & Security → Device Control and Data Access"
    } else {
        "Privacy & Security → Accessibility"
    };
    format!("Enable Accessibility in System Settings → {entry}")
}

extern "C" {
    // libSystem's sysctl by name; declared here rather than through a libc
    // crate dependency for this one call.
    fn sysctlbyname(
        name: *const std::ffi::c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> std::ffi::c_int;
}

/// `kern.osproductversion` (e.g. `"15.4.1"`), read in-process with
/// `sysctlbyname`. The error names what failed.
fn read_os_product_version() -> std::result::Result<String, String> {
    const NAME: &std::ffi::CStr = c"kern.osproductversion";
    let mut len: usize = 0;
    // SAFETY: a null `oldp` with a valid `oldlenp` asks for the size only.
    let rc = unsafe {
        sysctlbyname(
            NAME.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(format!(
            "sysctlbyname(kern.osproductversion) size query failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` holds `len` writable bytes, and `len` says so.
    let rc = unsafe {
        sysctlbyname(
            NAME.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(format!(
            "sysctlbyname(kern.osproductversion) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    buf.truncate(len.min(buf.len()));
    // The value is NUL-terminated.
    let text = std::ffi::CStr::from_bytes_until_nul(&buf)
        .map_err(|_| "kern.osproductversion is not NUL-terminated".to_string())?;
    text.to_str()
        .map(str::to_string)
        .map_err(|e| format!("kern.osproductversion is not UTF-8: {e}"))
}

/// The major version of a `kern.osproductversion` string.
fn parse_major_version(version: &str) -> std::result::Result<u32, String> {
    let version = version.trim();
    version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .filter(|major| *major > 0)
        .ok_or_else(|| format!("kern.osproductversion is {version:?}, not a macOS version"))
}

/// macOS major version from `kern.osproductversion`, cached after the first
/// read, or why it could not be read. An unknown version has two consumers:
/// the grant instruction names both settings panes, and
/// `screen_recording_gates_ax` treats it as "requires Screen Recording", so
/// `MacOSProvider::new` demands the grant (and says why) rather than skip it.
fn macos_major_version() -> std::result::Result<u32, String> {
    static VERSION: std::sync::LazyLock<std::result::Result<u32, String>> =
        std::sync::LazyLock::new(|| {
            read_os_product_version().and_then(|v| parse_major_version(&v))
        });
    VERSION.clone()
}

/// The provider's Screen Recording denial, naming the detected version (or
/// why it could not be detected, which is why the grant is demanded).
fn screen_recording_instructions(major: &std::result::Result<u32, String>) -> String {
    let why = match major {
        Ok(major) => format!(
            "This is macOS {major}; on macOS 26+ it is required to read window content via \
             the accessibility API."
        ),
        Err(reason) => format!(
            "The macOS version could not be detected ({reason}), so xa11y requires it as it \
             does on macOS 26+, where it is needed to read window content via the \
             accessibility API."
        ),
    };
    format!(
        "Enable Screen Recording in System Settings → Privacy & Security → \
         Screen & System Audio Recording, then restart this process (or the terminal \
         that launched it): the permission is read once per process.\n{why}"
    )
}

/// Whether the accessibility API needs Screen Recording to read window
/// content on macOS `major` (26 and later). An unknown version is treated as
/// needing it: demanding a grant the system may not need fails loudly, while
/// skipping one it does need returns window-less trees with no error.
fn screen_recording_gates_ax(major: &std::result::Result<u32, String>) -> bool {
    major.as_ref().map_or(true, |major| *major >= 26)
}

/// Global handle counter for mapping ElementData back to AXElements.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub struct MacOSProvider {
    /// Handle table: each handle's retained `AXUIElementRef`. Bounded (see
    /// `xa11y_core::handle_cache`), so a long session cannot grow it without
    /// limit; an evicted handle fails as `ElementStale` (`evicted`).
    handle_cache: Mutex<BoundedHandleCache<AXElement>>,
}

/// Holds a shell-probe messaging-timeout bound, releasing it on drop.
///
/// The bound is taken on an **application** element. Apple's header scopes
/// `AXUIElementSetMessagingTimeout` on a non-system-wide object to that object
/// alone, but the scan's elements can outlive it (an `AXMenuBar` surface hands
/// out the frontmost app's element), so the scan does not leave one pinned at
/// a quarter-second timeout: a caller who then walked the menu bar or the
/// desktop would see spurious `kAXErrorCannotComplete`. Releasing with `0`
/// returns the element to the process-wide value `MacOSProvider::new` set.
///
/// A guard rather than a call at the end of each probe, because every one of
/// those probes can leave early — `?` on a missing child, an unanswered
/// attribute — and a release that is skipped on the failure path is the case
/// that matters.
struct ShellProbeBound<'a> {
    element: &'a AXElement,
    /// The probe bound this one replaced in [`PROBE_BOUND`], restored on
    /// drop so a probe nested on the same thread by rayon's work stealing
    /// unwinds in order.
    outer: Option<(usize, f32)>,
}

impl Drop for ShellProbeBound<'_> {
    fn drop(&mut self) {
        PROBE_BOUND.with(|b| b.set(self.outer));
        MacOSProvider::unbound_shell_probe(self.element);
    }
}

impl MacOSProvider {
    pub fn new() -> Result<Self> {
        if !unsafe { safe_ax_is_process_trusted() } {
            return Err(Error::PermissionDenied {
                instructions: accessibility_grant_instructions(),
            });
        }

        // On macOS 26+, Screen Recording permission is required to read
        // window content via the accessibility API. Without it, AXChildren
        // returns only self-referencing AXApplication wrappers and menu bars.
        // Earlier releases read window content without it, so the provider
        // does not demand it there (the screenshot backend always does).
        let version = macos_major_version();
        if screen_recording_gates_ax(&version) && !Self::has_screen_recording_permission() {
            return Err(Error::PermissionDenied {
                instructions: screen_recording_instructions(&version),
            });
        }

        // Bound every AX message, so one busy or hung app cannot stall a
        // walk or an action for the system default per call (issue #512).
        set_global_messaging_timeout()?;

        Ok(Self {
            handle_cache: Mutex::new(BoundedHandleCache::new(MAX_CACHED_ELEMENTS)),
        })
    }

    /// List running GUI apps using CGWindowListCopyWindowInfo.
    fn list_gui_apps() -> Vec<(i32, String)> {
        let info = unsafe { safe_cg_window_list_copy(0, 0) };
        if info.is_null() {
            return vec![];
        }

        let pid_key = CFString::new("kCGWindowOwnerPID");
        let name_key = CFString::new("kCGWindowOwnerName");

        let mut seen = HashSet::new();
        let mut apps = Vec::new();

        unsafe {
            let count = safe_cf_array_get_count(info);
            for i in 0..count {
                let dict = safe_cf_array_get_value(info, i);
                if dict.is_null() {
                    continue;
                }

                let pid_val =
                    safe_cf_dict_get_value(dict, pid_key.as_concrete_TypeRef() as CFTypeRef);
                let name_val =
                    safe_cf_dict_get_value(dict, name_key.as_concrete_TypeRef() as CFTypeRef);

                if pid_val.is_null() {
                    continue;
                }

                let mut pid: i32 = 0;
                if safe_cf_get_type_id(pid_val) == safe_cf_number_get_type_id() {
                    safe_cf_number_get_value(
                        pid_val,
                        CF_NUMBER_SINT32,
                        &mut pid as *mut _ as *mut c_void,
                    );
                }

                if pid <= 0 || !seen.insert(pid) {
                    continue;
                }

                let name = if !name_val.is_null()
                    && safe_cf_get_type_id(name_val) == safe_cf_string_get_type_id()
                {
                    cf_string(name_val).unwrap_or_default()
                } else {
                    String::new()
                };

                if !name.is_empty() {
                    apps.push((pid, name));
                }
            }
            safe_cf_release(info);
        }

        apps
    }

    /// Whether this process has Screen Recording permission, from
    /// `CGPreflightScreenCaptureAccess` (macOS 10.15+), which reads the grant
    /// itself and never prompts.
    ///
    /// This used to be inferred from `CGWindowListCopyWindowInfo` listing a
    /// layer-0 (app) window, which read as "denied" whenever no app window
    /// was open — a fresh CI runner, a cleared desktop — however the
    /// permission was set (issue #516).
    pub(crate) fn has_screen_recording_permission() -> bool {
        unsafe { safe_cg_preflight_screen_capture_access() }
    }

    /// Cache an AXElement and return a new handle ID.
    fn cache_element(&self, ax: AXElement) -> u64 {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, ax);
        handle
    }

    /// Look up a cached AXElement by handle.
    fn get_cached(&self, handle: u64) -> Result<AXElement> {
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .cloned()
            .ok_or(Error::stale(
                xa11y_core::StaleReason::Evicted,
                format!("handle:{}", handle),
            ))
    }

    /// Build an ElementData from an AXElement, caching the AX handle.
    /// Tries batch fetch (1 IPC call for 15 attributes) first, falls back
    /// to individual calls if the batch API fails.
    fn build_element_data(&self, ax: &AXElement, pid: Option<u32>) -> Result<ElementData> {
        let handle = self.cache_element(ax.clone());
        build_snapshot_data(ax.as_ptr(), pid, handle)
    }
}

/// Build a snapshot `ElementData` from a raw `AXUIElementRef` without touching
/// the provider's handle cache. `handle` is stored verbatim — callers that
/// want the snapshot to be navigable later must supply one from
/// `MacOSProvider::cache_element`. For read-only snapshots (e.g. event
/// targets), pass `0`.
fn build_snapshot_data(
    element: AXUIElementRef,
    pid: Option<u32>,
    handle: u64,
) -> Result<ElementData> {
    if element.is_null() {
        // A null AXUIElementRef reports nothing, but this is still a
        // production path that returns an element to consumers — so it goes
        // through ElementParts rather than `for_role`. A new property needs a
        // decision here too, even if the decision is almost always "absent".
        return Ok(ElementParts {
            heading_level: None,
            landmark_type: None,
            web_role: None,
            link_url: None,
            live_region: None,
            role: Role::Unknown,
            name: None,
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            table: None,
            cell: None,
            table_row: None,
            collection: None,
            stable_id: None,
            pid,
            raw: std::collections::HashMap::new(),
            handle,
        }
        .into());
    }

    let body = move || -> Result<ElementData> {
        let batched =
            BatchAttrs::fetch(element).and_then(|batch| ResolvedAttrs::from_batch(&batch));
        let attrs = match batched {
            Some(attrs) => attrs,
            // The batch call is an optimisation, not a requirement. When it
            // fails, or answers without a readable role, re-read the same
            // attributes one at a time. That re-read is error-preserving for
            // `AXRole`, so an element invalidated between the batch fetch and
            // the individual reads carries its platform error out instead of
            // being built as a role-less ghost (tenet 1). A genuinely
            // role-less element also lands here (its role slot holds an
            // error object too), paying the individual reads; that is rare,
            // and the cost buys telling it apart from an unreadable one.
            None => ResolvedAttrs::from_individual(element)?,
        };

        let role = map_ax_role(&attrs.role_str, attrs.subrole_str.as_deref());
        let semantics = document_semantics(&attrs, role);
        let password = is_secure_text_field(&attrs.role_str, attrs.subrole_str.as_deref());

        // Build raw platform data map before consuming attrs fields.
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "ax_role".into(),
            serde_json::Value::String(attrs.role_str.clone()),
        );
        if let Some(ref sr) = attrs.subrole_str {
            raw.insert("ax_subrole".into(), serde_json::Value::String(sr.clone()));
        }
        if let Some(ref id) = attrs.identifier {
            raw.insert(
                "ax_identifier".into(),
                serde_json::Value::String(id.clone()),
            );
        }
        if let Some(ref t) = attrs.ax_title {
            raw.insert("AXTitle".into(), serde_json::Value::String(t.clone()));
        }
        if let Some(ref d) = attrs.ax_description {
            raw.insert("AXDescription".into(), serde_json::Value::String(d.clone()));
        }
        if let Some(ref h) = attrs.ax_help {
            raw.insert("AXHelp".into(), serde_json::Value::String(h.clone()));
        }
        if let Some(e) = attrs.enabled {
            raw.insert("AXEnabled".into(), serde_json::Value::Bool(e));
        }
        if let Some(f) = attrs.focused {
            raw.insert("AXFocused".into(), serde_json::Value::Bool(f));
        }
        if let Some(s) = attrs.selected {
            raw.insert("AXSelected".into(), serde_json::Value::Bool(s));
        }
        if let Some(h) = attrs.hidden {
            raw.insert("AXHidden".into(), serde_json::Value::Bool(h));
        }
        if let Some(e) = attrs.expanded {
            raw.insert("AXExpanded".into(), serde_json::Value::Bool(e));
        }
        if let Some(m) = attrs.modal {
            raw.insert("AXModal".into(), serde_json::Value::Bool(m));
        }
        if let Some((x, y)) = attrs.position {
            raw.insert("AXPosition".into(), serde_json::json!({"x": x, "y": y}));
        }
        if let Some((w, h)) = attrs.size {
            raw.insert(
                "AXSize".into(),
                serde_json::json!({"width": w, "height": h}),
            );
        }
        if let Some(n) = attrs.value_number {
            raw.insert(
                "AXValue".into(),
                serde_json::Value::Number(
                    serde_json::Number::from_f64(n).unwrap_or_else(|| serde_json::Number::from(0)),
                ),
            );
        } else if let Some(ref v) = attrs.value_string {
            raw.insert("AXValue".into(), serde_json::Value::String(v.clone()));
        }

        let name = accessible_name(
            Some(role),
            attrs.ax_title,
            || attrs.value_string.clone(),
            || attrs.ax_description.clone(),
        );
        let description =
            accessible_description(name.as_ref(), attrs.ax_description, attrs.ax_help);

        let value = match role {
            Role::CheckBox | Role::RadioButton => None,
            _ => attrs.value_string,
        };

        // States
        let checked = match role {
            Role::CheckBox | Role::RadioButton => {
                if let Some(i) = attrs.value_int {
                    match i {
                        0 => Some(Toggled::Off),
                        1 => Some(Toggled::On),
                        2 => Some(Toggled::Mixed),
                        _ => Some(Toggled::Off),
                    }
                } else if let Some(b) = attrs.value_is_bool {
                    Some(if b { Toggled::On } else { Toggled::Off })
                } else if let Some(f) = attrs.value_number {
                    Some(if f > 0.5 { Toggled::On } else { Toggled::Off })
                } else {
                    Some(Toggled::Off)
                }
            }
            _ => None,
        };

        let focusable = matches!(
            role,
            Role::Button
                | Role::TextField
                | Role::TextArea
                | Role::CheckBox
                | Role::RadioButton
                | Role::ComboBox
                | Role::Slider
                | Role::Link
                | Role::Tab
                | Role::MenuItem
                | Role::ListItem
                | Role::TreeItem
                | Role::SpinButton
                | Role::Switch
        ) || attrs.focused.is_some();

        // "Active" means the window currently receiving input, not the main
        // document window. Cocoa deliberately distinguishes key and main
        // windows (a floating panel can be key while the document stays main),
        // so compare against the owning application's AXFocusedWindow.
        let active = matches!(role, Role::Window | Role::Dialog)
            && pid.is_some_and(|pid| focused_window_is(element, pid));

        let states: StateSet = StateParts {
            enabled: attrs.enabled.unwrap_or(true),
            visible: !attrs.hidden.unwrap_or(false),
            // `AXHidden` is the only visibility AX reports, and it says
            // nothing about why: AX has no scrolled-out or minimized-content
            // signal on the element itself.
            hidden_reason: attrs
                .hidden
                .unwrap_or(false)
                .then_some(HiddenReason::NotShowing),
            focused: attrs.focused.unwrap_or(false),
            active,
            focusable,
            modal: attrs.modal.unwrap_or(false),
            minimized: if matches!(role, Role::Window | Role::Dialog) {
                attrs.minimized
            } else {
                None
            },
            // Native fullscreen is the state behind `enter_fullscreen`, and
            // macOS has no readable "zoomed" attribute (`AXZoomed` is not in
            // the SDK headers and live windows answer unsupported for it), so
            // `maximized` stays `None` and the state surfaces as `fullscreen`
            // (AXFullScreen, see [`WINDOW_FULLSCREEN_SETTLE_BUDGET`]).
            maximized: None,
            fullscreen: if matches!(role, Role::Window | Role::Dialog) {
                attrs.fullscreen
            } else {
                None
            },
            checked,
            // `AXSelected` is the per-element attribute, but bridges like
            // Qt's implement selection only container-side
            // (AXSelectedChildren on the table). Probe the container only
            // when the attribute is entirely absent AND the role is a
            // selectable item — AppKit elements carry AXSelected directly, so
            // they never pay for the probe. `raw` keeps only genuinely
            // present platform attributes, so a derived value shows up in
            // `states.selected` but adds no fake "AXSelected" key.
            selected: match attrs.selected {
                Some(s) => s,
                None if selection_can_come_from_container(role) => {
                    container_selection_contains(element, 2)
                }
                None => false,
            },
            expanded: attrs.expanded,
            editable: matches!(role, Role::TextField | Role::TextArea),
            required: false,
            busy: false,
            password,
        }
        .into();

        let bounds = match (attrs.position, attrs.size) {
            (Some((x, y)), Some((w, h))) if w > 0.0 || h > 0.0 => Some(Rect {
                x: x as i32,
                y: y as i32,
                width: w.max(0.0) as u32,
                height: h.max(0.0) as u32,
            }),
            _ => None,
        };

        // Discover actions via AXUIElementCopyActionNames + implicit attributes.
        let ax_actions = ax_action_names(element);
        let mut actions: Vec<String> = Vec::new();

        for ax_name in &ax_actions {
            if let Some(known) = ax_action_to_name(ax_name) {
                let s = known.to_string();
                if !actions.contains(&s) {
                    actions.push(s);
                }
            } else {
                let snake = ax_pascal_to_snake(ax_name);
                if !actions.contains(&snake) {
                    actions.push(snake);
                }
            }
        }

        // `press` on an element whose only activation action is `AXOpen`
        // dispatches to it (see `press_ax_action`), so it is advertised too.
        // `open` stays listed: it is the element's native action.
        let press_str = "press".to_string();
        if press_ax_action(&ax_actions) == "AXOpen" && !actions.contains(&press_str) {
            actions.push(press_str);
        }

        // Implicit actions from settable attributes.
        let focus_str = "focus".to_string();
        if attrs.focused.is_some() && !actions.contains(&focus_str) {
            actions.push(focus_str);
        }
        let set_value_str = "set_value".to_string();
        if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
            && !actions.contains(&set_value_str)
        {
            actions.push(set_value_str);
        }
        // `toggle` is a cross-platform semantic verb; macOS implements it via
        // AXPress for toggleable roles. Advertise it alongside `press` when
        // the element both reports AXPress natively and is one of the known
        // toggleable roles.
        let toggle_str = "toggle".to_string();
        if matches!(role, Role::CheckBox | Role::Switch | Role::RadioButton)
            && ax_actions.iter().any(|a| a == "AXPress")
            && !actions.contains(&toggle_str)
        {
            actions.push(toggle_str);
        }

        // Window verbs: `activate` is advertised from the native action list
        // (AXRaise is a real AX action for windows); the attribute-backed
        // verbs are advertised from settability, the same way `expand` /
        // `collapse` decide theirs. The probes preserve their errors: a
        // transient `CannotComplete` is a platform failure, not "the window
        // cannot be minimized" — `is_attr_settable` already answers
        // unsupported/no-value as `false` (the honest absence case), so a
        // real failure propagates out of element construction instead of
        // silently dropping the capability from `actions`.
        if matches!(role, Role::Window | Role::Dialog) {
            let push = |actions: &mut Vec<String>, name: &str| {
                let n = name.to_string();
                if !actions.contains(&n) {
                    actions.push(n);
                }
            };
            if ax_actions.iter().any(|a| a == "AXRaise") {
                push(&mut actions, "activate");
            }
            // Probe the window-state capabilities once each — every probe is
            // an AX FFI round-trip — and advertise each verb from the exact
            // predicate its implementation accepts, so a caller that
            // re-enumerates after a transition never loses a verb the window
            // would still honor (tenet 3). The state reads are the strict
            // ones the verbs dispatch on, not the snapshot's lossy batch
            // values: a malformed boolean is a platform error in both, so
            // `actions` cannot promise a call that rejects on the same read.
            //
            // `enter_fullscreen` is the native fullscreen state, which is
            // readable *and* writable, and what the green button does on a
            // fullscreen-capable window. It is deliberately not advertised
            // from the presence of a zoom button — the button's `AXPress` /
            // `AXZoomWindow` actions are toggles with no readable state, and
            // on a window that cannot fullscreen (System Settings, for
            // example) the button only classic-zooms, which is not what
            // `enter_fullscreen` promises (tenet 3). `maximize` is never
            // advertised on macOS: there is no readable or writable zoom
            // state to back it (see the dispatch in `maximize()`).
            let state = WindowState::probe(element, "the window actions", role)?;
            // `minimize` also needs a way out of fullscreen: AppKit ignores a
            // minimized set while the window is fullscreen, so the verb exits
            // fullscreen first and is advertised only when it can (see
            // `minimize_supported`).
            if minimize_supported(&state) {
                push(&mut actions, "minimize");
            }
            // An already-committed fullscreen window satisfies
            // `enter_fullscreen` whatever the settability probe answers (see
            // `enter_fullscreen()`), and a minimized flag that cannot be
            // cleared makes the verb a silent no-op, so both states are part
            // of the shared predicate: a repeated call, and one that brings
            // the window back on-screen, stay advertised.
            if enter_fullscreen_supported(&state) {
                push(&mut actions, "enter_fullscreen");
            }
            // `restore()` accepts a window only when every state that reads
            // `true` can be cleared (a true state with a non-settable
            // attribute is refused before any mutation, so it must not be
            // advertised) and at least one of the two attributes is settable.
            if restore_supported(&state) {
                push(&mut actions, "restore");
            }
            // `close` is advertised when the window exposes a close button —
            // the direct AXCloseButton attribute *or* the AXCloseButton-subrole
            // child scan, the exact lookup `close()` dispatches (tenet 3: the
            // advertised surface must match the implementation, and a bridge
            // that exposes the button only as a child must advertise it).
            if find_close_button(element, role)?.is_some() {
                push(&mut actions, "close");
            }
            if is_attr_settable(element, "AXPosition")? {
                push(&mut actions, "move_to");
            }
            if is_attr_settable(element, "AXSize")? {
                push(&mut actions, "resize_to");
            }
        }

        let (table, cell, table_row, collection) =
            attrs
                .rich
                .to_metadata(element, role, &attrs.role_str, attrs.subrole_str.as_deref())?;

        let numeric_value = match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton | Role::ScrollBar => {
                attrs.value_number
            }
            _ => None,
        };

        // Min/max still require individual calls (not in the batch set).
        // Role list mirrors xa11y-linux (atspi.rs) and xa11y-windows (uia.rs)
        // which both populate min/max for Slider | ProgressBar | ScrollBar |
        // SpinButton. The previous list only covered Slider, which silently
        // dropped min/max for the other three roles on macOS — egui's DragValue
        // (Role::SpinButton) surfaced the gap because AccessKit's macOS bridge
        // exposes kAXMinValue/kAXMaxValue unconditionally, we just never asked
        // for them.
        let (min_value, max_value) = match role {
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::SpinButton => (
                ax_number_f64(element, "AXMinValue"),
                ax_number_f64(element, "AXMaxValue"),
            ),
            _ => (None, None),
        };

        // Strip Unicode bidi format controls (LRM, RLM, embeddings, isolates)
        // from text fields. macOS inserts these for presentation; they break
        // equality assertions like `el.value == "5"`. Originals remain in
        // `raw` (`AXTitle`, `AXValue`, `AXDescription`, `AXHelp`).
        let name = xa11y_core::text::strip_bidi_opt(name);
        let value = xa11y_core::text::strip_bidi_opt(value);
        let description = xa11y_core::text::strip_bidi_opt(description);

        Ok(ElementParts {
            heading_level: semantics.heading_level,
            landmark_type: semantics.landmark_type,
            web_role: None,
            link_url: semantics.link_url,
            live_region: semantics.live_region,
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            numeric_value,
            min_value,
            max_value,
            table,
            cell,
            table_row,
            collection,
            stable_id: attrs.identifier,
            pid,
            raw,
            handle,
        }
        .into())
    };
    body()
}

impl MacOSProvider {
    /// Should this child be filtered out (macOS system chrome)?
    /// Accepts pre-fetched role/subrole to avoid redundant AX calls when
    /// the caller already has them.
    fn should_filter_child_with_role(
        parent_role: Role,
        parent_name: Option<&str>,
        child_role: &str,
        child_subrole: Option<&str>,
        child: &AXElement,
    ) -> bool {
        if parent_role == Role::Application && child_role == "AXMenuBar" {
            return true;
        }

        if parent_role == Role::Window {
            let sr = child_subrole.unwrap_or("");
            if matches!(
                sr,
                "AXCloseButton" | "AXMinimizeButton" | "AXFullScreenButton" | "AXZoomButton"
            ) {
                return true;
            }
            if child_role == "AXStaticText" && (sr.is_empty() || sr == "AXUnknown") {
                if let Some(v) = ax_string(child.as_ptr(), "AXValue") {
                    if parent_name == Some(v.as_str()) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Snapshot `children` of `parent`, dropping system chrome.
    ///
    /// Filters first (cheap string checks), then builds ElementData in
    /// parallel (each build is an IPC round-trip). A child that vanished
    /// between the children read and its snapshot (macOS invalidates a
    /// window's AX object during fullscreen transitions and on close) is no
    /// longer part of this parent's live surface, so it is dropped. A child
    /// the process did not answer for is *not* a fact about the tree —
    /// reporting a smaller one would be indistinguishable from a genuine
    /// subtree, so the walk fails with the platform error (tenet 1).
    fn snapshot_children(
        &self,
        parent: &ElementData,
        children: &[AXElement],
    ) -> Result<Vec<ElementData>> {
        let role = parent.role;
        let name = parent.name.as_deref();
        let filtered: Vec<&AXElement> = children
            .iter()
            .filter(|child| !Self::should_filter_child(role, name, child))
            .collect();
        let built: Vec<Result<ElementData>> = filtered
            .par_iter()
            .map(carry_deadline(|child: &&AXElement| {
                self.build_element_data(child, parent.pid)
            }))
            .collect();
        let mut results: Vec<ElementData> = Vec::with_capacity(built.len());
        for data in built {
            match data {
                Ok(data) => results.push(data),
                Err(err) => match classify_element_churn(&err) {
                    ElementChurn::Gone => {}
                    ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
                },
            }
        }
        Ok(results)
    }

    /// Convenience wrapper that fetches role/subrole before filtering.
    fn should_filter_child(
        parent_role: Role,
        parent_name: Option<&str>,
        child: &AXElement,
    ) -> bool {
        let child_role = ax_string(child.as_ptr(), "AXRole").unwrap_or_default();
        let child_subrole = ax_string(child.as_ptr(), "AXSubrole");
        Self::should_filter_child_with_role(
            parent_role,
            parent_name,
            &child_role,
            child_subrole.as_deref(),
            child,
        )
    }

    /// Parallel DFS over the AX subtree, evaluating every clause's first
    /// `SimpleSelector` against each visited element. Emits
    /// `(clause_idx, AXElement)` pairs in document order. An element that
    /// matches multiple clauses is emitted once per matching clause — the
    /// caller deduplicates by `AXUIElement` pointer identity at merge time.
    ///
    /// At each level, children are processed in parallel using rayon —
    /// each child's role check and recursive subtree search happen
    /// concurrently across threads.
    ///
    /// `limit` here is the *outer* limit; for groups with more than one
    /// clause it must be passed as `None` because cross-clause doc-order
    /// can promote later-clause hits ahead of earlier ones.
    #[allow(clippy::too_many_arguments)] // recursive DFS with parent context
    fn collect_matching_ax_group(
        &self,
        parent: &AXElement,
        parent_role: Role,
        parent_name: Option<&str>,
        clauses: &[&SimpleSelector],
        pid: Option<u32>,
        depth: u32,
        max_depth: u32,
        limit: Option<usize>,
    ) -> Vec<(usize, AXElement)> {
        if depth > max_depth {
            return vec![];
        }

        let children = ax_children(parent.as_ptr());

        // Process children in parallel: check each clause + recurse.
        let per_child_results: Vec<Vec<(usize, AXElement)>> = children
            .par_iter()
            .map(carry_deadline(|child: &AXElement| {
                let mut child_results: Vec<(usize, AXElement)> = Vec::new();

                // Fetch role+subrole once — used for filter, match, and recursion.
                let role_str = ax_string(child.as_ptr(), "AXRole").unwrap_or_default();
                let subrole_str = ax_string(child.as_ptr(), "AXSubrole");

                if Self::should_filter_child_with_role(
                    parent_role,
                    parent_name,
                    &role_str,
                    subrole_str.as_deref(),
                    child,
                ) {
                    return child_results;
                }

                let child_role = map_ax_role(&role_str, subrole_str.as_deref());

                for (idx, simple) in clauses.iter().enumerate() {
                    if matches_ax_with_role(child.as_ptr(), simple, Some(child_role), pid) {
                        child_results.push((idx, child.clone()));
                    }
                }

                // Recurse into subtree.
                let child_name = derived_ax_name(child.as_ptr(), Some(child_role));
                let sub = self.collect_matching_ax_group(
                    child,
                    child_role,
                    child_name.as_deref(),
                    clauses,
                    pid,
                    depth + 1,
                    max_depth,
                    limit,
                );
                child_results.extend(sub);

                child_results
            }))
            .collect();

        // Merge results, respecting limit.
        let mut results = Vec::new();
        for batch in per_child_results {
            for elem in batch {
                results.push(elem);
                if let Some(limit) = limit {
                    if results.len() >= limit {
                        return results;
                    }
                }
            }
        }
        results
    }

    // ── Shell surfaces ───────────────────────────────────────────────────
    //
    // `design/shell-surfaces/PROPOSAL.md` §4 is the contract these three
    // helpers implement: the frontmost app's `AXMenuBar`, one surface per
    // process owning a live `AXExtrasMenuBar`, the Dock's application
    // element, and Finder's desktop scroll area. Every root is an element
    // the platform itself vends — xa11y adds the tag, never the node.
    //
    // None of them touches `should_filter_child_with_role`'s `AXMenuBar`
    // drop (PROPOSAL §5): app trees stay exactly as they are, and these
    // surfaces reach the same elements through a root the caller asked for
    // by name.

    /// Messaging timeout for one shell-surface probe, in seconds.
    ///
    /// The process-wide bound ([`AX_MESSAGING_TIMEOUT_SECS`]) applies *per
    /// attribute*, so a handful of wedged processes otherwise dominate the
    /// whole scan (PROPOSAL §4, measured). `AXUIElementSetMessagingTimeout`
    /// on a non-system-wide object applies to that object alone — so it is
    /// set on the very element each probe then reads.
    ///
    /// It bounds **every** application element the scan reads, not only the
    /// status-item fan-out: the frontmost app for `AXMenuBar`, the Dock, and
    /// Finder for its desktop scroll area. `ShellSurface::by_kind` polls at
    /// 100ms, and a listing that can block for seconds per attribute makes
    /// that contract false — a wedged Finder is enough.
    const SHELL_PROBE_TIMEOUT_SECS: f32 = 0.25;

    /// Bound `element`'s messaging timeout to
    /// [`SHELL_PROBE_TIMEOUT_SECS`](Self::SHELL_PROBE_TIMEOUT_SECS).
    ///
    /// Returns `false` when the bound could not be established, which is the
    /// scan's cue to skip the element rather than read it at the
    /// process-wide bound: an element whose cost cannot be bounded is not one this scan
    /// can query inside its contract.
    fn bound_shell_probe(element: &AXElement) -> bool {
        let err = unsafe {
            safe_ax_set_messaging_timeout(element.as_ptr(), Self::SHELL_PROBE_TIMEOUT_SECS)
        };
        err == AX_ERROR_SUCCESS
    }

    /// Release `element` back to the system-wide default messaging timeout.
    ///
    /// `0` means "no element-specific timeout", so the global default applies
    /// again. Every bound the scan takes is released, on every path — see
    /// [`ShellProbeBound`].
    fn unbound_shell_probe(element: &AXElement) {
        // The result genuinely does not matter: a failure here leaves the
        // element on the scan's tighter bound, which is a slower walk and
        // never a wrong answer. Failing the listing over it would trade a
        // correct result for none.
        let _err = unsafe { safe_ax_set_messaging_timeout(element.as_ptr(), 0.0_f32) };
    }

    /// Bound `element` for the duration of a probe, releasing it on every exit
    /// path.
    ///
    /// Returns `None` when the bound could not be established, which is the
    /// scan's cue to skip the element (see [`bound_shell_probe`]).
    ///
    /// [`bound_shell_probe`]: Self::bound_shell_probe
    fn shell_probe_bound(element: &AXElement) -> Option<ShellProbeBound<'_>> {
        Self::bound_shell_probe(element).then(|| ShellProbeBound {
            element,
            outer: PROBE_BOUND.with(|b| {
                b.replace(Some((
                    element.as_ptr() as usize,
                    Self::SHELL_PROBE_TIMEOUT_SECS,
                )))
            }),
        })
    }

    /// The frontmost application's `AXMenuBar`, tagged `MenuBar`.
    ///
    /// The macOS menu bar is per-application — there is no single system
    /// object — so the surface is whichever app is frontmost, named and
    /// pid-attributed to that app. When nothing is frontmost, the system-wide
    /// element vends no `AXFocusedApplication`; that means "no menu bar right
    /// now", so the surface is omitted rather than failing the listing.
    ///
    /// The frontmost application element is read from the system-wide element
    /// here rather than through [`focused_app`](Provider::focused_app),
    /// because this scan must **bound** that element's messaging timeout
    /// before touching it. `focused_app` builds a full `ElementData` at the
    /// AX default, and it is on the app-discovery path where that default is
    /// the right one — bounding it there would change a contract this scan
    /// has no business changing. The two calls read the same attribute of the
    /// same system-wide element; only the cost bound differs. `apps` supplies
    /// the CGWindowList name, exactly as `focused_app` looks it up.
    fn menu_bar_surface(
        &self,
        apps: &[(i32, String)],
    ) -> Result<Option<(ShellSurfaceKind, ElementData)>> {
        let system_wide = system_wide_element();
        if system_wide.is_null() {
            return Err(Error::platform(
                -1,
                "AXUIElementCreateSystemWide returned NULL",
            ));
        }
        let app_ax = match probe_element_attr(system_wide.as_ptr(), "AXFocusedApplication") {
            ElementProbe::Found(app) => app,
            // Nothing is frontmost (login window, screen saver), or the
            // system-wide element did not answer. `focused_app` reads both the
            // same way — "nothing is foreground" — and for a live listing that
            // is "no menu_bar surface", not a failure of the listing.
            ElementProbe::Absent | ElementProbe::Unanswered(_) => return Ok(None),
        };
        // Bound the probe *before* making it, as the status-item fan-out does.
        // The guard releases it however this function exits.
        let Some(_bound) = Self::shell_probe_bound(&app_ax) else {
            return Ok(None);
        };

        let mut pid: i32 = 0;
        let pid_err = unsafe { safe_ax_get_pid(app_ax.as_ptr(), &mut pid) };
        let app_pid = (pid_err == AX_ERROR_SUCCESS && pid > 0).then_some(pid as u32);

        match probe_element_attr(app_ax.as_ptr(), "AXMenuBar") {
            ElementProbe::Found(menu_bar) => {
                // The bound was set on the app element alone, so `menu_bar` —
                // the root handed to the caller — keeps the system-wide
                // default and walking the menus is not crippled by the scan's
                // quarter-second budget.
                let mut data = self.build_element_data(&menu_bar, app_pid)?;
                // An `AXMenuBar` carries no title of its own; the surface is
                // named for the app whose menus it holds ("Safari"), which is
                // the CGWindowList name `focused_app` also resolves. When the
                // app owns no listed window the AX-reported name stands, the
                // same policy `focused_app` applies.
                if let Some((_, app_name)) =
                    app_pid.and_then(|p| apps.iter().find(|(gp, _)| *gp == p as i32))
                {
                    data.name = Some(app_name.clone());
                }
                Ok(Some((ShellSurfaceKind::MenuBar, data)))
            }
            // The frontmost app answered and has no menu bar (a full-screen
            // game, a process with no AppKit menu). Honest absence.
            ElementProbe::Absent => Ok(None),
            ElementProbe::Unanswered(code) => match classify_element_churn_code(i64::from(code)) {
                // The same codes `app_by_pid` reads as "not reachable right
                // now": the app went away between the focused-app read and
                // this one, its accessibility bridge is not answering, or it
                // did not answer inside the bounded probe. For a live listing
                // that is "no menu_bar surface", not a failure of the listing.
                ElementChurn::Gone | ElementChurn::Unreachable => Ok(None),
                // Anything else is a genuine platform failure and propagates
                // with the AXError code that produced it (tenet 1, tenet 6).
                ElementChurn::Fatal => Err(Error::platform(
                    code as i64,
                    format!(
                        "AXUIElementCopyAttributeValue(AXMenuBar) failed on the frontmost \
                         application{}",
                        app_pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                    ),
                )),
            },
        }
    }

    /// One `StatusItems` surface per process owning a live `AXExtrasMenuBar`,
    /// ordered by ascending pid.
    ///
    /// The candidate set is the crate's existing CGWindowList enumeration
    /// (`list_gui_apps`), which sees every process owning a window — the
    /// status item an accessory app puts in the menu bar is such a window, so
    /// the common accessory app is covered. A status-item owner that vends no
    /// window entry at all is missed: that is documented narrowing, not a
    /// silent gap. PROPOSAL §4 allows either this fan-out or new NSWorkspace
    /// FFI, and closing the remainder is the `list_gui_apps()` union
    /// follow-up the proposal defers in §10.
    ///
    /// A failure on one process never fails the scan — it contributes no
    /// surface and the fan-out continues, which is the policy the scan-cost
    /// measurement demanded. Enumerating the processes at all is the caller's
    /// job, and that failure does propagate.
    fn status_item_surfaces(&self, apps: &[(i32, String)]) -> Vec<(ShellSurfaceKind, ElementData)> {
        let mut surfaces: Vec<(ShellSurfaceKind, ElementData)> = apps
            .par_iter()
            .filter_map(carry_deadline(|(pid, app_name): &(i32, String)| {
                let app_element =
                    AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
                if app_element.is_null() {
                    return None;
                }

                // Bound the probe *before* making it. If the bound cannot be
                // established the element is not one we can query within the
                // scan's cost contract, so it is skipped rather than probed
                // at the process-wide bound.
                // Bound for the rest of this closure; the guard releases it.
                let _bound = Self::shell_probe_bound(&app_element)?;

                match probe_element_attr(app_element.as_ptr(), "AXExtrasMenuBar") {
                    ElementProbe::Found(extras) => {
                        // `extras` is a child of the app element, and the
                        // guard releases the bound on that app element when
                        // this closure returns — so the root handed to the
                        // caller is walked at the system-wide default, not the
                        // scan's quarter-second budget. Building its `ElementData`
                        // costs one unbounded round-trip, to a process that
                        // just answered inside the bound.
                        let mut data = match self.build_element_data(&extras, Some(*pid as u32)) {
                            Ok(d) => d,
                            Err(e) => {
                                // Same policy as `Unanswered` below: one
                                // process never fails the scan, but the error
                                // is diagnosed rather than silent.
                                eprintln!(
                                    "status-item surface snapshot failed for pid {}: {e:?}",
                                    pid
                                );
                                return None;
                            }
                        };
                        // The extras menu bar has no title; the surface is
                        // named for its owner, as `list_apps` names apps.
                        data.name = Some(app_name.clone());
                        Some((ShellSurfaceKind::StatusItems, data))
                    }
                    // No status items — the common case — or the process did
                    // not answer inside the bounded probe. Both contribute
                    // nothing, and `probe_element_attr` keeps them apart by
                    // AXError code so a wedged app is skipped deliberately
                    // rather than mistaken for an app with nothing to show.
                    ElementProbe::Absent | ElementProbe::Unanswered(_) => None,
                }
            }))
            .collect();
        // Deterministic order: the fan-out is parallel, so the pid sort is
        // what makes repeated listings comparable.
        surfaces.sort_by_key(|(_, data)| data.pid);
        surfaces
    }

    /// Native menus currently shown by status-item surfaces, as transient
    /// `Flyout` roots.
    ///
    /// AppKit may keep a status-item menu permanently in `AXChildren`, even
    /// while it is closed. An open attached menu either enters
    /// `AXVisibleChildren` or gains its non-empty on-screen frame. Other
    /// implementations expose a detached menu through AppKit's `AXShownMenu`
    /// or Carbon's `AXShownMenuUIElement`. Probe the owning application,
    /// extras bar, and each direct status-item child because those provider
    /// shapes vary across implementations.
    ///
    /// Each probe carries the same per-element timeout as the rest of shell
    /// discovery. An app that does not answer contributes no flyout, matching
    /// the documented failure policy of the status-item fan-out above.
    fn shown_status_menu_surfaces(
        &self,
        status_items: &[(ShellSurfaceKind, ElementData)],
    ) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        let mut surfaces = Vec::new();
        let mut seen_menus: Vec<AXElement> = Vec::new();

        for (_, status_data) in status_items {
            let extras = self.get_cached(status_data.handle)?;
            let mut providers = vec![extras.clone()];
            if let Some(pid) = status_data.pid {
                providers.push(AXElement::from_owned(unsafe {
                    safe_ax_create_application(pid as i32)
                }));
            }
            providers.extend(ax_children(extras.as_ptr()));

            for provider in providers {
                let Some(_bound) = Self::shell_probe_bound(&provider) else {
                    continue;
                };
                let visible_children = ax_element_array(provider.as_ptr(), "AXVisibleChildren")
                    .into_iter()
                    .filter(|child| {
                        ax_string(child.as_ptr(), "AXRole").as_deref() == Some("AXMenu")
                    })
                    .collect::<Vec<_>>();
                let mut shown_menus = visible_children;
                for attached_menu in ax_children(provider.as_ptr()).into_iter().filter(|child| {
                    ax_string(child.as_ptr(), "AXRole").as_deref() == Some("AXMenu")
                }) {
                    let data = self.build_element_data(&attached_menu, status_data.pid)?;
                    if data.states.visible
                        && data
                            .bounds
                            .is_some_and(|bounds| bounds.width > 0 && bounds.height > 0)
                    {
                        shown_menus.push(attached_menu);
                    }
                }
                for attribute in ["AXShownMenu", "AXShownMenuUIElement"] {
                    match probe_element_list_attr(provider.as_ptr(), attribute) {
                        ElementListProbe::Found(menus) => shown_menus.extend(menus),
                        ElementListProbe::Absent | ElementListProbe::Unanswered => {}
                    }
                }

                for shown_menu in shown_menus {
                    if seen_menus.iter().any(|menu| unsafe {
                        safe_cf_equal(menu.as_ptr() as CFTypeRef, shown_menu.as_ptr() as CFTypeRef)
                    }) {
                        continue;
                    }

                    let mut data = self.build_element_data(&shown_menu, status_data.pid)?;
                    data.name = status_data.name.clone();
                    seen_menus.push(shown_menu);
                    surfaces.push((ShellSurfaceKind::Flyout, data));
                }
            }
        }

        surfaces.sort_by_key(|(_, data)| data.pid);
        Ok(surfaces)
    }

    /// The Dock's application element, tagged `Dock`.
    ///
    /// Identified by the CGWindowList owner name, which is what the existing
    /// enumeration reports for `com.apple.dock`. Reading the bundle
    /// identifier instead would mean new NSWorkspace FFI for a process whose
    /// owner name Apple has not changed; the name match is the conservative
    /// choice, and a miss simply omits the surface.
    ///
    /// Reading it costs one `build_element_data` round-trip, bounded like
    /// every other probe in this scan — a wedged Dock must not make the
    /// listing's 100ms poll contract a fiction. The bound is released again
    /// before the element is handed out, because here the application element
    /// *is* the surface root: walking the Dock at a quarter-second per
    /// attribute is not what bounding the scan was for.
    fn dock_surface(
        &self,
        apps: &[(i32, String)],
    ) -> Result<Option<(ShellSurfaceKind, ElementData)>> {
        let Some((pid, name)) = apps.iter().find(|(_, name)| name.as_str() == "Dock") else {
            return Ok(None);
        };
        let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
        if app_element.is_null() {
            return Ok(None);
        }
        if !Self::bound_shell_probe(&app_element) {
            return Ok(None);
        }
        let mut data = self.build_element_data(&app_element, Some(*pid as u32))?;
        Self::unbound_shell_probe(&app_element);
        // Same name policy as `list_apps`: the CGWindowList owner name wins.
        data.name = Some(name.clone());
        Ok(Some((ShellSurfaceKind::Dock, data)))
    }

    /// Finder's desktop scroll area, tagged `Desktop`: the `AXScrollArea`
    /// child of Finder's application element whose `AXDescription` is
    /// "desktop".
    ///
    /// It already sits in Finder's `AXChildren`, so nothing has to be
    /// unfiltered to reach it. The root keeps whatever name the platform
    /// gives the scroll area (usually none), which `ShellSurface::list_with`
    /// then renders as the kind's own spelling — the desktop is the
    /// platform's surface, not Finder's window.
    ///
    /// Finder's application element is bounded before its `AXChildren` are
    /// read: a wedged Finder is the case that makes the listing's 100ms poll
    /// contract false, and it is the one process this scan cannot route
    /// around. No release afterwards — the root handed to the caller is the
    /// scroll area, a child element that never carried the bound.
    fn desktop_surface(
        &self,
        apps: &[(i32, String)],
    ) -> Result<Option<(ShellSurfaceKind, ElementData)>> {
        let Some((pid, _)) = apps.iter().find(|(_, name)| name.as_str() == "Finder") else {
            return Ok(None);
        };
        let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
        if app_element.is_null() {
            return Ok(None);
        }
        if !Self::bound_shell_probe(&app_element) {
            return Ok(None);
        }
        let Some(desktop) = ax_children(app_element.as_ptr()).into_iter().find(|child| {
            ax_string(child.as_ptr(), "AXRole").as_deref() == Some("AXScrollArea")
                && ax_string(child.as_ptr(), "AXDescription")
                    .is_some_and(|d| d.eq_ignore_ascii_case("desktop"))
        }) else {
            return Ok(None);
        };
        Ok(Some((
            ShellSurfaceKind::Desktop,
            self.build_element_data(&desktop, Some(*pid as u32))?,
        )))
    }
}

impl Provider for MacOSProvider {
    fn capabilities(&self) -> Result<xa11y_core::Capabilities> {
        xa11y_core::Capabilities::current()
    }

    fn relations(
        &self,
        element: &ElementData,
        kinds: &[xa11y_core::RelationKind],
    ) -> Result<Vec<(xa11y_core::RelationKind, Vec<ElementData>)>> {
        let attributes = kinds
            .iter()
            .map(|k| {
                ax_relation_attribute(*k)
                    .map(|a| (*k, a))
                    .ok_or_else(|| Error::Unsupported {
                        feature: format!(
                            "relation {k} on macOS: AX does not report it (controls and flows-to \
                         arrive together as `linked`)"
                        ),
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let ax = self.get_cached(element.handle)?;
        let mut out = Vec::with_capacity(attributes.len());
        for (kind, (attribute, single)) in attributes {
            let targets = if single {
                ax_single_element(ax.as_ptr(), attribute)?
                    .into_iter()
                    .collect()
            } else {
                ax_element_list_strict(ax.as_ptr(), attribute)?
            };
            let built = targets
                .iter()
                .map(|t| self.build_element_data(t, element.pid))
                .collect::<Result<Vec<_>>>()?;
            out.push((kind, built));
        }
        Ok(out)
    }

    fn identity(&self, element: &ElementData) -> Result<xa11y_core::NodeIdentity> {
        let ax = self.get_cached(element.handle)?;
        let process = element
            .pid
            .map_or_else(|| "-".to_string(), |pid| pid.to_string());
        Ok(xa11y_core::NodeIdentity::from_object(
            "ax",
            process,
            std::sync::Arc::new(AxIdentity(ax)),
        ))
    }

    fn revalidate(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let subject = xa11y_core::error::stale_subject(element);
        let read_start = Instant::now();
        let read = read_raw_attr(ax.as_ptr(), "AXRole");
        let read_elapsed = read_start.elapsed();
        let live_role = match read {
            RawAttr::Value(v) => {
                let is_string = unsafe { safe_cf_get_type_id(v) == safe_cf_string_get_type_id() };
                if is_string {
                    // `cf_string_owned` releases the +1 copy.
                    cf_string_owned(v)
                } else {
                    unsafe { safe_cf_release(v) };
                    None
                }
            }
            RawAttr::Absent => None,
            RawAttr::Unanswered(code) => {
                return Err(match classify_element_churn_code(i64::from(code)) {
                    ElementChurn::Gone => {
                        let exited = element
                            .pid
                            .and_then(|pid| i32::try_from(pid).ok())
                            .is_some_and(|pid| !process_answers_ax(pid));
                        let reason = if exited {
                            xa11y_core::StaleReason::ProcessExited
                        } else {
                            xa11y_core::StaleReason::Removed
                        };
                        Error::stale(reason, subject)
                    }
                    // A busy or wedged app is not a stale element: report it
                    // as the platform failure it is, naming the app and
                    // whether it timed out as every other path does.
                    ElementChurn::Unreachable | ElementChurn::Fatal
                        if code == AX_ERROR_CANNOT_COMPLETE =>
                    {
                        cannot_complete_error(
                            ax.as_ptr(),
                            &format!("AXRole read for {subject} failed"),
                            read_elapsed,
                            CannotCompleteContext::PreAction,
                        )
                    }
                    ElementChurn::Unreachable | ElementChurn::Fatal => Error::platform(
                        i64::from(code),
                        format!("AXRole read for {subject} failed (AXError {code})"),
                    ),
                });
            }
        };
        // The guard compares the raw `AXRole` the snapshot stored
        // (`ax_role`), not the mapped role, so a subrole refinement cannot
        // make a live element look replaced.
        match (
            live_role,
            element.raw.get("ax_role").and_then(|v| v.as_str()),
        ) {
            (Some(live), Some(captured)) if live != captured => Err(Error::stale(
                xa11y_core::StaleReason::IdentityChanged,
                format!("{subject}; the element now reports {live}, not {captured}"),
            )),
            _ => Ok(()),
        }
    }

    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: list all GUI apps as application elements.
                // Delegated to `list_apps()` so the discovery primitive has
                // a single canonical implementation.
                self.list_apps()
            }
            Some(element_data) => {
                let ax = self.get_cached(element_data.handle)?;

                // An Application element's children are its top-level
                // windows (`AXWindows`) followed by any app-level `AXMenu`,
                // such as an open context menu, which search reaches through
                // `AXChildren` and `AXWindows` never lists (see
                // `app_top_level_children`). Every other role keeps the raw
                // `AXChildren` walk. The application read is error-preserving
                // on purpose: a dead app must report its failure, not an
                // empty list (tenet 1).
                let ax_children_list = match element_data.role {
                    Role::Application => app_top_level_children(ax.as_ptr())?,
                    _ => ax_children(ax.as_ptr()),
                };
                self.snapshot_children(element_data, &ax_children_list)
            }
        }
    }

    /// `App::windows` reads `AXWindows` alone: windows are all it keeps, so
    /// the app-level menu read `get_children` adds would only cost an IPC
    /// round-trip and a failure mode.
    fn app_windows(&self, app: &ElementData) -> Result<Vec<ElementData>> {
        if app.role != Role::Application {
            return self.get_children(Some(app));
        }
        let ax = self.get_cached(app.handle)?;
        let windows = ax_windows(ax.as_ptr())?;
        self.snapshot_children(app, &windows)
    }

    fn find_elements_group(
        &self,
        root: &ElementData,
        group: &xa11y_core::selector::SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        // The AST's fields are public, so a hand-built group can break the
        // invariants the walk below indexes on (`clause.segments[0]`, Root
        // only first, 1-based `:nth`). Reject it before traversal.
        group.validate()?;

        // ONE AX walk that evaluates every clause's first SimpleSelector
        // inline. Each match is tagged with its clause index; per-clause
        // phase-2 narrowing follows. The cross-clause merge deduplicates by
        // AXUIElement pointer identity — that's the only identifier stable
        // across narrowings within a single call (handles are minted fresh
        // on every `build_element_data`).
        //
        // App discovery is handled separately by `list_apps()` — `root` is
        // always present here.
        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        let root_data = root;

        let root_ax = self.get_cached(root_data.handle)?;

        let firsts: Vec<&SimpleSelector> = group
            .clauses
            .iter()
            .map(|c| &c.segments[0].simple)
            .collect();

        // N=1 phase-1 limit short-circuit: when there's exactly one clause,
        // propagate the user's `limit` (adjusted for `:nth`) to the AX walk
        // so e.g. `app.locator("button").first()` stops at the first match.
        // For N>=2, phase-1 must collect the full union before truncating
        // because cross-clause doc-order can promote later-clause hits
        // ahead of earlier ones.
        let phase1_walk_limit = if group.clauses.len() == 1 {
            let clause = &group.clauses[0];
            let first = firsts[0];
            let outer = if clause.segments.len() == 1 {
                limit
            } else {
                None
            };
            match (outer, first.nth) {
                (Some(l), Some(n)) => Some(l.max(n)),
                (_, Some(n)) => Some(n),
                (l, None) => l,
            }
        } else {
            None
        };

        // Phase 1 walks and snapshots in one pass. A hit that died between
        // the AX walk and its snapshot is dropped — it is not a match any
        // more, and a bare miss is exactly the retry signal auto-wait polls
        // on (`ElementChurn::Gone`); a hit the process did not answer for
        // fails the search rather than shrinking the match set (tenet 1).
        // A bounded walk (`phase1_walk_limit`, the
        // `first()` / `nth()` short-circuit) stops before matches past the
        // bound, so a dropped hit can have consumed the bound before a live
        // match was ever examined. Re-walk unbounded when that happens: a
        // second pass has no bound left to hide a match, and a drop there is
        // the ordinary concurrent-mutation case the retry signal covers.
        let mut walk_limit = phase1_walk_limit;
        let phase1_data_by_clause: Vec<Vec<(usize, AXElement, ElementData)>> = loop {
            let phase1: Vec<(usize, AXElement)> = self.collect_matching_ax_group(
                &root_ax,
                root_data.role,
                root_data.name.as_deref(),
                &firsts,
                root_data.pid,
                0,
                max_depth_val,
                walk_limit,
            );

            // Snapshot phase-1 hits in walk order, bucketed by the clause each
            // belongs to. The per-clause entries keep their original walk
            // positions, so bucketing as we snapshot cannot affect the merge.
            let mut dropped = false;
            let mut data_by_clause: Vec<Vec<(usize, AXElement, ElementData)>> =
                (0..group.clauses.len()).map(|_| Vec::new()).collect();
            for (pos, (clause_idx, ax)) in phase1.into_iter().enumerate() {
                match self.build_element_data(&ax, root_data.pid) {
                    Ok(data) => data_by_clause[clause_idx].push((pos, ax, data)),
                    Err(err) => match classify_element_churn(&err) {
                        ElementChurn::Gone => dropped = true,
                        ElementChurn::Unreachable | ElementChurn::Fatal => return Err(err),
                    },
                }
            }

            if !dropped || walk_limit.is_none() {
                break data_by_clause;
            }
            // The bounded pass is discarded and re-walked. Its snapshots stay
            // in the handle cache (see `MacOSProvider::handle_cache`); they
            // are a deliberate casualty of the retry, not a leak this pass
            // can plug.
            walk_limit = None;
        };

        let mut merged: Vec<(usize, AXElement, ElementData)> = Vec::new();
        for (clause_idx, mut phase1_data) in phase1_data_by_clause.into_iter().enumerate() {
            if phase1_data.is_empty() {
                continue;
            }
            let clause = &group.clauses[clause_idx];
            let first = &clause.segments[0].simple;

            if clause.segments.len() == 1 {
                // Per-clause `:nth` and limit handling — but we can't apply
                // outer limit here (cross-clause merge can re-order).
                if let Some(nth) = first.nth {
                    if nth <= phase1_data.len() {
                        let kept = phase1_data.remove(nth - 1);
                        phase1_data.clear();
                        phase1_data.push(kept);
                    } else {
                        phase1_data.clear();
                    }
                }
                merged.extend(phase1_data);
                continue;
            }

            // Multi-segment narrowing per clause. We anchor each narrowed
            // descendant at its phase-1 ancestor's walk_pos so the doc-order
            // sort puts cross-clause results in the right global order.
            for (anchor_pos, _anchor_ax, head) in phase1_data {
                let narrowed = self.narrow_multi_segment(
                    vec![head],
                    &clause.segments[1..],
                    max_depth_val,
                    None,
                )?;
                for n in narrowed {
                    // Anchor identity is for dedup vs other clauses' phase-1
                    // hits; for phase-2 outputs we use the narrowed element's
                    // own AXUIElement, resolved via the cache and held as a
                    // retained reference: the cache evicts, so it cannot be
                    // what keeps the pointer alive through the merge below.
                    // The handle was minted by this call; a miss is a bug,
                    // not an element to keep without identity.
                    let ax = self.get_cached(n.handle)?;
                    merged.push((anchor_pos, ax, n));
                }
            }
        }

        // Stable sort by walk position keeps doc-order; the dedup keeps the
        // first of each accessibility object.
        merged.sort_by_key(|(pos, _, _)| *pos);
        Ok(dedup_by_ax_identity(merged, limit))
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let ax = self.get_cached(element.handle)?;
        match ax_parent(ax.as_ptr()) {
            Some(parent_ax) => {
                if parent_ax.is_null() {
                    return Ok(None);
                }
                // Check if parent is an application — if so, still return it
                Ok(Some(self.build_element_data(&parent_ax, element.pid)?))
            }
            None => Ok(None),
        }
    }

    fn enable_web_accessibility(&self, app: &ElementData) -> Result<()> {
        enable_web_accessibility_for(app)
    }

    /// Enumerate the macOS shell surfaces: the frontmost application's menu
    /// bar, each process's status items, the Dock, Finder's desktop, and
    /// native status-item menus while they are open.
    ///
    /// Order is fixed — `menu_bar`, `status_items` by ascending pid, `dock`,
    /// `desktop`, then open `flyout` menus by ascending pid — so repeated
    /// listings are comparable even though the status-item scan fans out in
    /// parallel.
    ///
    /// Reading is the whole of it: nothing here opens, closes, focuses, or
    /// presses anything, and the app-tree `AXMenuBar` filter (PROPOSAL §5) is
    /// untouched — these surfaces reach those elements through their own
    /// roots.
    ///
    /// `Flyout` covers native status-item menus through the platform's
    /// `AXShownMenuUIElement` relationship. Control Center and Notification
    /// Center remain out of scope: those are shell processes'
    /// `AXSystemDialog` windows, whose enumeration contract differs from the
    /// status-item roots already available here.
    ///
    /// # Errors
    ///
    /// Failing to enumerate the processes at all propagates as
    /// [`Error::Platform`]. A failure on one process does not: it contributes
    /// no surface, and the rest of the listing stands.
    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        let apps = Self::list_gui_apps();
        if apps.is_empty() {
            // `list_gui_apps` returns an empty vec both when CGWindowList is
            // empty and when the call failed outright. On a live session it
            // is never legitimately empty — the Dock and Finder always own
            // windows — and the one other cause, missing Screen Recording
            // permission, `MacOSProvider::new` already rejects. So this is
            // "process enumeration failed", which propagates.
            return Err(Error::platform(
                -1,
                "CGWindowListCopyWindowInfo listed no processes; \
                          cannot enumerate shell surfaces"
                    .to_string(),
            ));
        }

        let mut surfaces: Vec<(ShellSurfaceKind, ElementData)> = Vec::new();
        if let Some(menu_bar) = self.menu_bar_surface(&apps)? {
            surfaces.push(menu_bar);
        }
        let status_items = self.status_item_surfaces(&apps);
        let shown_menus = self.shown_status_menu_surfaces(&status_items)?;
        surfaces.extend(status_items);
        if let Some(dock) = self.dock_surface(&apps)? {
            surfaces.push(dock);
        }
        if let Some(desktop) = self.desktop_surface(&apps)? {
            surfaces.push(desktop);
        }
        surfaces.extend(shown_menus);
        Ok(surfaces)
    }

    /// Enumerate top-level applications via CGWindowList — the canonical
    /// macOS app discovery primitive. For each GUI app we synthesise an
    /// `AXUIElement` via `AXUIElementCreateApplication(pid)` and build
    /// full `ElementData`, overriding the AX-reported name with the
    /// CGWindowList name (which is more consistent across launches).
    fn list_apps(&self) -> Result<Vec<ElementData>> {
        let apps = Self::list_gui_apps();
        let mut results: Vec<ElementData> = Vec::new();
        for (pid, app_name) in &apps {
            let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
            if app_element.is_null() {
                continue;
            }
            let mut data = match self.build_element_data(&app_element, Some(*pid as u32)) {
                Ok(d) => d,
                // The process is not AX-reachable right now (still launching,
                // busy, or without an accessibility bridge) or its object was
                // invalidated between the scan and the snapshot: it
                // contributes no entry to this listing, and the caller's poll
                // re-enumerates — the same codes `app_by_pid` maps to
                // `SelectorNotMatched`. The `Role::Unknown` filter below is
                // the other half of the same classification. Every other
                // failure propagates (tenet 1).
                Err(err) => match classify_element_churn(&err) {
                    ElementChurn::Gone | ElementChurn::Unreachable => continue,
                    ElementChurn::Fatal => return Err(err),
                },
            };
            data.name = Some(app_name.clone());
            // A node built from `AXUIElementCreateApplication(pid)` is a
            // process node, so its role is `Role::Application` whatever
            // AXRole reads — with one exception: a process that answers
            // AXRole with the `AXUnknown` placeholder (Window Server) is not
            // an accessibility application at all. Its AXWindows read answers
            // `kAXErrorCannotComplete` (-25204), so a synthesized
            // Application node for it would make every unfiltered
            // `xa11y windows` / MCP windows listing fail with a permanent,
            // unactionable error on every macOS desktop. It is classified
            // out of the listing — inspected, not silently skipped (tenet 1);
            // a user who attaches to it by pid still gets the node and an
            // honest error on windows().
            if data.role == Role::Unknown {
                continue;
            }
            data.role = Role::Application;
            results.push(data);
        }
        Ok(results)
    }

    /// Attach to an application directly by pid via
    /// `AXUIElementCreateApplication` — no window enumeration involved.
    ///
    /// `list_apps()` discovers apps through CGWindowList, which only sees
    /// processes that already own a window with a non-empty owner name (and
    /// needs Screen Recording permission to read names). During app startup
    /// none of that holds yet, so a freshly launched process can be
    /// AX-reachable while still invisible to enumeration. Direct attach
    /// avoids that blind spot entirely.
    ///
    /// `AXUIElementCreateApplication` succeeds for *any* pid without checking
    /// the process, so reachability is probed by reading `AXRole`. AXError
    /// codes that mean "not reachable (yet)" — the process is still
    /// launching, has exited, or doesn't implement the AX API — map to
    /// `SelectorNotMatched` so the core poll loop keeps retrying until its
    /// deadline; anything else is a genuine platform failure and
    /// short-circuits.
    fn app_by_pid(&self, pid: u32) -> Result<ElementData> {
        let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(pid as i32) });
        if app_element.is_null() {
            return Err(
                Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
                    xa11y_core::Diagnosis::new()
                        .last_observed("AXUIElementCreateApplication returned NULL"),
                ),
            );
        }
        let attr = CFString::new("AXRole");
        let mut value: CFTypeRef = std::ptr::null();
        let err = ffi_copy_attribute_value(
            app_element.as_ptr(),
            attr.as_concrete_TypeRef() as CFTypeRef,
            &mut value,
        );
        match err {
            AX_ERROR_SUCCESS => {
                if !value.is_null() {
                    unsafe { safe_cf_release(value) };
                }
                let mut data = match self.build_element_data(&app_element, Some(pid)) {
                    Ok(d) => d,
                    Err(err) => match classify_element_churn(&err) {
                        // The process became unreachable between the attach
                        // probe above and the snapshot (it exited, or its
                        // accessibility server stopped answering): the same
                        // "not reachable (yet)" reading the probe maps to
                        // `SelectorNotMatched`, so the core's poll loop keeps
                        // retrying until its deadline.
                        ElementChurn::Gone | ElementChurn::Unreachable => {
                            let diagnosis = xa11y_core::Diagnosis::new().last_observed(format!(
                                "the application stopped answering between the AX attach probe \
                                 and its snapshot: {err}"
                            ));
                            return Err(Error::selector_not_matched(format!(
                                "application[pid={pid}]"
                            ))
                            .diagnose(diagnosis));
                        }
                        ElementChurn::Fatal => return Err(err),
                    },
                };
                // Keep `name` consistent with `list_apps()`, which overrides
                // the AX-reported name with the CGWindowList owner name. A
                // pre-window process has no CGWindowList entry yet; the
                // AX-reported name from build_element_data then stands.
                if let Some((_, name)) = Self::list_gui_apps()
                    .into_iter()
                    .find(|(p, _)| *p == pid as i32)
                {
                    data.name = Some(name);
                }
                // `list_apps` classifies non-AX-interactive processes (AXRole
                // `AXUnknown`, e.g. Window Server) out of its listing; a
                // direct pid attach keeps the role AXRole reported, so
                // `windows()` on such a node fails with an honest
                // ActionNotSupported rather than pretending the process has
                // no windows.
                Ok(data)
            }
            // `kAXErrorNoValue` joins the retryable list only here: the probe
            // asked for the role, and a process that answers "no value" while
            // it is still attaching reads as "not reachable yet" like the
            // codes the classifier folds.
            err if err == AX_ERROR_NO_VALUE
                || matches!(
                    classify_element_churn_code(i64::from(err)),
                    ElementChurn::Gone | ElementChurn::Unreachable
                ) =>
            {
                Err(
                    Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
                        xa11y_core::Diagnosis::new().last_observed(format!(
                        "AX attach probe returned AXError {err}: process not yet AX-reachable, \
                     exited, or has no accessibility bridge"
                    )),
                    ),
                )
            }
            _ => Err(Error::platform(
                err as i64,
                format!(
                    "AXUIElementCopyAttributeValue(AXRole) failed while attaching to pid {pid}"
                ),
            )),
        }
    }

    /// Identify the frontmost application via the system-wide AX element's
    /// `AXFocusedApplication` attribute — the canonical macOS foreground-app
    /// query, equivalent to `NSWorkspace.frontmostApplication` but staying
    /// entirely within the AX API we already use.
    ///
    /// We read the focused application element, resolve its pid via
    /// `AXUIElementGetPid` (so the core can tag the matching `list_apps`
    /// entry), and override the name with the CGWindowList owner name for
    /// consistency with `list_apps`.
    ///
    /// When nothing is frontmost the attribute is absent / NULL — that maps to
    /// [`Error::SelectorNotMatched`] ("nothing focused"), which the core reads
    /// as "no app is foreground" rather than a failure. A NULL system-wide
    /// element is a genuine platform failure and propagates.
    fn focused_app(&self) -> Result<ElementData> {
        let system_wide = system_wide_element();
        if system_wide.is_null() {
            return Err(Error::platform(
                -1,
                "AXUIElementCreateSystemWide returned NULL",
            ));
        }

        let attr = CFString::new("AXFocusedApplication");
        let mut value: CFTypeRef = std::ptr::null();
        let err = ffi_copy_attribute_value(
            system_wide.as_ptr(),
            attr.as_concrete_TypeRef() as CFTypeRef,
            &mut value,
        );
        if err != AX_ERROR_SUCCESS || value.is_null() {
            // No frontmost application (or the attribute is unavailable, e.g.
            // focus rests on the login window / screen saver). Treat as
            // "nothing focused" so the core leaves apps untagged.
            return Err(Error::selector_not_matched("focused application"));
        }

        // `AXFocusedApplication` returns a +1-retained AXUIElement; take
        // ownership so it's released on drop.
        let app_element = AXElement::from_owned(value as AXUIElementRef);

        let mut pid: i32 = 0;
        let pid_err = unsafe { safe_ax_get_pid(app_element.as_ptr(), &mut pid) };
        let pid_opt = if pid_err == AX_ERROR_SUCCESS && pid > 0 {
            Some(pid as u32)
        } else {
            None
        };

        let mut data = self.build_element_data(&app_element, pid_opt)?;
        if let Some(p) = pid_opt {
            if let Some((_, name)) = Self::list_gui_apps()
                .into_iter()
                .find(|(gp, _)| *gp == p as i32)
            {
                data.name = Some(name);
            }
        }
        Ok(data)
    }

    // ── Common actions ──────────────────────────────────────────────

    fn press(&self, element: &ElementData) -> Result<()> {
        // The `actions` list is built from `AXUIElementCopyActionNames` at
        // snapshot time — macOS's counterpart of the AT-SPI action-index lookup
        // (Linux) and the activation-pattern probe (Windows), both of which
        // refuse the verb up front. AppKit answers `kAXErrorSuccess` for AXPress
        // on elements that do not support it (an AccessKit static text no-ops,
        // for example), so dispatching blindly would report a press that never
        // happened. A control whose press really works advertises it (a native
        // NSPopUpButton lists `press` and opens its menu), so gating here keeps
        // the advertised and the dispatchable surfaces the same (tenet 3).
        if !element.actions.iter().any(|a| a == "press") {
            return Err(Error::ActionNotSupported {
                action: "press".to_string(),
                role: element.role,
            });
        }
        let ax = self.get_cached(element.handle)?;
        // The snapshot's `press` cannot say which AX action it came from
        // (`AXPress` + `AXOpen` and `AXOpen` alone both list `press` and
        // `open`), so the dispatch target is read from the live action list.
        // That read must not fail open: an empty list would pick AXPress,
        // which AppKit acknowledges on an AXOpen-only element without doing
        // anything.
        let ax_name = press_ax_action(&try_ax_action_names(ax.as_ptr())?);
        perform_ax_action(ax.as_ptr(), ax_name, "press", element.role)
    }

    fn toggle(&self, element: &ElementData) -> Result<()> {
        if !matches!(
            element.role,
            Role::CheckBox | Role::Switch | Role::RadioButton
        ) {
            return Err(Error::ActionNotSupported {
                action: "toggle".to_string(),
                role: element.role,
            });
        }
        let ax = self.get_cached(element.handle)?;
        perform_ax_action(ax.as_ptr(), "AXPress", "toggle", element.role)
    }

    fn show_menu(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        perform_ax_action(ax.as_ptr(), "AXShowMenu", "show_menu", element.role)
    }

    fn increment(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        perform_ax_action(ax.as_ptr(), "AXIncrement", "increment", element.role)
    }

    fn decrement(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        perform_ax_action(ax.as_ptr(), "AXDecrement", "decrement", element.role)
    }

    fn focus(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        set_bool_attr(ax.as_ptr(), "AXFocused", true, "focus", element.role)
    }

    fn blur(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        set_bool_attr(ax.as_ptr(), "AXFocused", false, "blur", element.role)
    }

    fn select(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        set_bool_attr(ax.as_ptr(), "AXSelected", true, "select", element.role)
    }

    fn expand(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Setting AXExpanded on an element that doesn't support it succeeds
        // and silently no-ops in every bridge — check settability first so
        // unsupported expand surfaces as ActionNotSupported (tenet 1).
        if !is_attr_settable(ax.as_ptr(), "AXExpanded")? {
            return Err(Error::ActionNotSupported {
                action: "expand".to_string(),
                role: element.role,
            });
        }
        set_bool_attr(ax.as_ptr(), "AXExpanded", true, "expand", element.role)
    }

    fn collapse(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // See expand(): unsupported AXExpanded sets are silent no-ops.
        if !is_attr_settable(ax.as_ptr(), "AXExpanded")? {
            return Err(Error::ActionNotSupported {
                action: "collapse".to_string(),
                role: element.role,
            });
        }
        set_bool_attr(ax.as_ptr(), "AXExpanded", false, "collapse", element.role)
    }

    fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
        // `AXScrollToVisible` is the AX counterpart of AT-SPI
        // `Component.ScrollTo` and UIA `ScrollItemPattern.ScrollIntoView`.
        // Gate on the advertised list: AppKit answers `kAXErrorSuccess` for
        // actions an element does not implement, so a blind dispatch would
        // report a scroll that never happened (tenet 1). The list is read
        // live and error-preserving, as `press` reads its dispatch target:
        // the snapshot's `actions` were read with failures folded into an
        // empty list, so a read that failed then would refuse here as
        // "not supported". Setting the enclosing scroll bar's `AXValue`
        // would be a second mechanism, so it is deliberately not tried.
        let ax = self.get_cached(element.handle)?;
        if !try_ax_action_names(ax.as_ptr())?
            .iter()
            .any(|a| a == "AXScrollToVisible")
        {
            return Err(Error::ActionNotSupported {
                action: "scroll_into_view".to_string(),
                role: element.role,
            });
        }
        perform_ax_action(
            ax.as_ptr(),
            "AXScrollToVisible",
            "scroll_into_view",
            element.role,
        )
    }

    // ── Window management ──────────────────────────────────────────

    fn activate(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // AXRaise does not deminiaturize, so a minimized window would answer
        // Ok while staying in the Dock — the fidelity gap the Windows backend
        // avoids by restoring first. Clear AXMinimized when it is true,
        // error-preserving (see `clear_bool_attr_if_true`).
        clear_bool_attr_if_true(ax.as_ptr(), "AXMinimized", "activate", element.role)?;
        // AXRaise alone only re-raises the window within its own app's window
        // list and answers Ok while the app stays in the background, so a
        // background app's window never appears on screen — the report that
        // `xa11y action activate` "does nothing". Activate the app first through
        // the accessibility API (the application element's AXFrontmost), then
        // activate the window.
        // Conservative: the clear above is a no-op on a window that was not
        // minimized, but it cannot be told apart here, so a failure from this
        // step is `unknown`.
        activate_owning_app(ax.as_ptr(), "activate", element.role)
            .map_err(|e| unknown_after_step(e, "clearing AXMinimized"))?;
        // The window or its app has changed by now, so an AXRaise refusal
        // must not read as "not delivered".
        let raise = CFString::new("AXRaise");
        let call = do_perform_action(ax.as_ptr(), &raise);
        if call == AX_ERROR_ATTRIBUTE_UNSUPPORTED {
            // SwiftUI scene windows (and Calculator on macOS 26) list AXRaise
            // and then answer it with kAXErrorAttributeUnsupported, even when
            // they are already the app's main window. Nothing visible tells
            // such a window apart before the call, so the refusal is reported
            // here. AXMain is not taken as proof the window is in front: it
            // can be stale right after AXFrontmost, and skipping the raise on
            // it would report success for a raise that never ran (tenet 1).
            return Err(Error::platform(
                call.code as i64,
                "activate: the app was brought to the front, but the window \
                          refused AXRaise (kAXErrorAttributeUnsupported) although it \
                          lists AXRaise among its actions, so it may not be the front \
                          window"
                    .to_string(),
            ));
        }
        if call != AX_ERROR_SUCCESS {
            return Err(unknown_after_step(
                action_error(
                    ax.as_ptr(),
                    call,
                    "activate",
                    element.role,
                    "AXRaise failed",
                ),
                "activating the owning app",
            ));
        }
        Ok(())
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Resolve the capability before any mutation, so a refused verb makes
        // no partial change (tenet 1). `minimize_supported` refuses a
        // fullscreen window whose fullscreen state cannot be cleared: AppKit
        // silently ignores an AXMinimized set while the window is fullscreen,
        // so the verb would report success while the window stayed
        // fullscreen. The settability probe also stands in for the old
        // "unsettable AXMinimized sets would silently no-op in every bridge"
        // check.
        let state = WindowState::probe(ax.as_ptr(), "minimize", element.role)?;
        if !minimize_supported(&state) {
            return Err(Error::ActionNotSupported {
                action: "minimize".to_string(),
                role: element.role,
            });
        }
        let mut target = ax;
        let left_fullscreen = state.fullscreen == Some(true);
        if state.fullscreen == Some(true) {
            // Leave fullscreen first: the window server suppresses minimize
            // while the window is fullscreen, so a minimize issued now would
            // no-op. Settle the exit like `restore` does — the minimized set
            // is issued only once the state has really committed.
            let app = owning_app_element(target.as_ptr(), "minimize", element.role)?;
            settle_window_fullscreen(target.as_ptr(), false, "minimize", element.role)?;
            // The exit transition can recreate the window object; the cached
            // handle would then answer kAXErrorInvalidUIElement to the
            // minimize set. Prefer the fresh object from the app's enumerated
            // window set — the same lookup the settle follows the target
            // with. The settle only returns once the target is enumerated
            // (see `SettleRun`), so the cached handle is used only if the
            // window drops off the list again in the moment between the
            // settle's last poll and this lookup.
            if let Some(fresh) = fresh_target_in_app(app.as_ptr(), target.as_ptr())
                .map_err(|e| unknown_after_step(e, "leaving fullscreen"))?
            {
                target = fresh;
            }
        }
        // After leaving fullscreen the window has already changed, so a later
        // failure must not read as "not delivered".
        let after_exit = |e: Error| {
            if left_fullscreen {
                unknown_after_step(e, "leaving fullscreen")
            } else {
                e
            }
        };
        set_bool_attr(
            target.as_ptr(),
            "AXMinimized",
            true,
            "minimize",
            element.role,
        )
        .map_err(after_exit)?;
        // The iconify is applied asynchronously on native windows (see
        // [`WINDOW_MINIMIZED_SETTLE_BUDGET`]); settle the read-back so the
        // caller never has to discover the Dock animation.
        settle_bool_attr(
            target.as_ptr(),
            "AXMinimized",
            true,
            "minimize",
            element.role,
        )
        .map_err(|e| unknown_after_step(e, "setting AXMinimized"))
    }

    fn maximize(&self, element: &ElementData) -> Result<()> {
        // macOS has no accessible maximize. The classic zoom state has no
        // readable or writable accessibility surface (`AXZoomed` is not
        // declared in the SDK and live windows answer unsupported for it),
        // and the green button's `AXPress` / `AXZoomWindow` are toggles that
        // enter fullscreen on a fullscreen-capable window — so there is no
        // absolute set to make repeated calls idempotent, and substituting
        // fullscreen would blur two distinct operations (see
        // `enter_fullscreen`). This is a platform-wide missing capability,
        // not an action this particular element lacks, so classify it as
        // `Unsupported` consistently with Windows/Linux `enter_fullscreen`
        // and Linux `maximize`. The `actions` advertisement never lists
        // `maximize` here (tenet 1, tenet 3).
        Err(Error::Unsupported {
            feature: format!(
                "maximize on {}: macOS Accessibility has no readable or writable zoom state",
                element.role.to_snake_case()
            ),
        })
    }

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Resolve the capability before any mutation, so a refused verb makes
        // no partial change (tenet 1). `enter_fullscreen_supported` accepts a
        // committed fullscreen state whatever the settability probe answers,
        // and refuses a minimized flag that cannot be cleared. The settle
        // loop still confirms the state, so a call racing an exit cannot
        // no-op on a stale `true` read.
        let state = WindowState::probe(ax.as_ptr(), "enter_fullscreen", element.role)?;
        if !enter_fullscreen_supported(&state) {
            return Err(Error::ActionNotSupported {
                action: "enter_fullscreen".to_string(),
                role: element.role,
            });
        }
        // AXMinimized and fullscreen are independent states: setting
        // fullscreen on a minimized window returns success while the window
        // stays in the Dock. The window-state contract (see the shared mock)
        // has the verb clear `minimized` and bring the window back on-screen,
        // so clear it first — error-preserving, the same `activate()`
        // handling (tenet 1).
        clear_bool_attr_if_true(ax.as_ptr(), "AXMinimized", "enter_fullscreen", element.role)?;
        // Set the absolute state, never a toggle: setting `true` on an
        // already-fullscreen window is a no-op, and skipping the set on a
        // `true` read would let an enter-fullscreen that races an exit
        // transition return success on a window that ends up restored. The
        // settle loop issues the set and re-issues it until the state
        // commits. Conservative: the AXMinimized clear above is a no-op on a
        // window that was not minimized and that cannot be told apart here,
        // and the settle loop itself sets AXFullScreen, so any error from it
        // is `unknown`.
        settle_window_fullscreen(ax.as_ptr(), true, "enter_fullscreen", element.role)
            .map_err(|e| unknown_after_step(e, "clearing AXMinimized or setting AXFullScreen"))
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Restore clears the minimized state and the fullscreen state. A
        // state that reads `true` must also be clearable: an unsupported AX
        // set no-ops silently (see `is_attr_settable`), so accepting it would
        // only time out in the settle, or report success after clearing the
        // other state. At least one state must be reachable for the verb to
        // do anything. A failed probe propagates as a platform error rather
        // than being read as unsupported (tenet 1).
        //
        // `restore_supported` is the same predicate the `actions`
        // advertisement uses, so an advertised `restore` cannot
        // deterministically reject here (tenet 3).
        let state = WindowState::probe(ax.as_ptr(), "restore", element.role)?;
        if !restore_supported(&state) {
            return Err(Error::ActionNotSupported {
                action: "restore".to_string(),
                role: element.role,
            });
        }
        if state.minimized_settable {
            set_bool_attr(ax.as_ptr(), "AXMinimized", false, "restore", element.role)?;
            // Deminiaturizing is applied asynchronously too (see
            // [`WINDOW_MINIMIZED_SETTLE_BUDGET`]); settle before the caller
            // can read the state back. A window that was not minimized reads
            // `false` on the first poll, so this costs one AX read.
            settle_bool_attr(ax.as_ptr(), "AXMinimized", false, "restore", element.role)
                .map_err(|e| unknown_after_step(e, "setting AXMinimized"))?;
        }
        // Never press the green button here: the press toggles, so a window
        // that is not fullscreen would be *entered* fullscreen by its own
        // restore. `AXFullScreen=false` is the absolute state and is safe to
        // set when the window is already restored; the settle loop re-issues
        // the clear until the confirmation has ruled out an entry transient
        // that reports `false` with the attribute still settable (see
        // [`WINDOW_FULLSCREEN_SETTLE_GRACE`]). A window that already reads
        // fullscreen is settable once `restore_supported` has passed — the
        // predicate refuses that state otherwise — so the settability flag
        // alone says whether there is a fullscreen state to clear.
        if state.fullscreen_settable {
            settle_window_fullscreen(ax.as_ptr(), false, "restore", element.role).map_err(|e| {
                if state.minimized_settable {
                    unknown_after_step(e, "clearing AXMinimized")
                } else {
                    e
                }
            })?;
        }
        Ok(())
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let btn = find_close_button(ax.as_ptr(), element.role)?;
        let Some(btn) = btn else {
            return Err(Error::ActionNotSupported {
                action: "close".to_string(),
                role: element.role,
            });
        };
        perform_ax_action(btn.as_ptr(), "AXPress", "close", element.role)
    }

    fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        if !is_attr_settable(ax.as_ptr(), "AXPosition")? {
            return Err(Error::ActionNotSupported {
                action: "move_to".to_string(),
                role: element.role,
            });
        }
        settle_window_position(ax.as_ptr(), x, y, "move_to", element.role)
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        if !is_attr_settable(ax.as_ptr(), "AXSize")? {
            return Err(Error::ActionNotSupported {
                action: "resize_to".to_string(),
                role: element.role,
            });
        }
        set_window_size(ax.as_ptr(), width, height, "resize_to", element.role)?;
        settle_window_size(ax.as_ptr(), width, height, "resize_to", element.role)
            .map_err(|e| unknown_after_step(e, "setting AXSize"))
    }

    // ── Typed operations ────────────────────────────────────────────

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let attr = CFString::new("AXValue");
        let cf_value = CFString::new(value);
        let err = do_set_attribute(
            ax.as_ptr(),
            &attr,
            cf_value.as_concrete_TypeRef() as CFTypeRef,
        );
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                ax.as_ptr(),
                err,
                "set_value",
                element.role,
                "Set AXValue (string) failed",
            ));
        }
        Ok(())
    }

    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let attr = CFString::new("AXValue");
        let cf_num = CFNumber::from(value);
        let err = do_set_attribute(
            ax.as_ptr(),
            &attr,
            cf_num.as_concrete_TypeRef() as CFTypeRef,
        );
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                ax.as_ptr(),
                err,
                "set_numeric_value",
                element.role,
                "Set AXValue (number) failed",
            ));
        }
        Ok(())
    }

    fn type_text(&self, element: &ElementData, text: &str) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let attr = CFString::new("AXSelectedText");
        let cf_text = CFString::new(text);
        let err = do_set_attribute(
            ax.as_ptr(),
            &attr,
            cf_text.as_concrete_TypeRef() as CFTypeRef,
        );
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                ax.as_ptr(),
                err,
                "type_text",
                element.role,
                "Set AXSelectedText failed",
            ));
        }
        Ok(())
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        // Out of bounds or splitting a surrogate pair is refused before the
        // set: AX would clamp the first and select half a character for the
        // second (#646).
        self.rich_validate_selection(element, start, end)
            .map_err(|e| self.rich_stale(element, e))?;
        let ax = self.get_cached(element.handle)?;
        let location = start as isize;
        let length = (end as isize) - (start as isize);
        let range_value = unsafe { safe_ax_value_create_cf_range(location, length) };
        if range_value.is_null() {
            return Err(Error::platform(
                -1,
                "Failed to create CFRange value for text selection",
            ));
        }
        let attr = CFString::new("AXSelectedTextRange");
        let err = do_set_attribute(ax.as_ptr(), &attr, range_value);
        unsafe { safe_cf_release(range_value) };
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                ax.as_ptr(),
                err,
                "set_text_selection",
                element.role,
                "Set AXSelectedTextRange failed",
            ));
        }
        Ok(())
    }

    // ── Tables, collections and text by range (ax/rich.rs) ─────────

    fn table_cell_at(
        &self,
        table: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        self.rich_table_cell_at(table, row, column)
            .map_err(|e| self.rich_stale(table, e))
    }

    fn table_headers(
        &self,
        table: &ElementData,
        kind: xa11y_core::table::HeaderKind,
    ) -> Result<Vec<ElementData>> {
        self.rich_table_headers(table, kind)
            .map_err(|e| self.rich_stale(table, e))
    }

    fn cell_headers(
        &self,
        cell: &ElementData,
        kind: xa11y_core::table::HeaderKind,
    ) -> Result<Vec<ElementData>> {
        self.rich_cell_headers(cell, kind)
            .map_err(|e| self.rich_stale(cell, e))
    }

    fn selected_items(&self, container: &ElementData) -> Result<Vec<ElementData>> {
        self.rich_selected_items(container)
            .map_err(|e| self.rich_stale(container, e))
    }

    fn text_length(&self, element: &ElementData) -> Result<u32> {
        self.rich_text_length(element)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_caret(&self, element: &ElementData) -> Result<Option<u32>> {
        self.rich_text_caret(element)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_selections(&self, element: &ElementData) -> Result<Vec<xa11y_core::TextRange>> {
        self.rich_text_selections(element)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_read(&self, element: &ElementData, range: xa11y_core::TextRange) -> Result<String> {
        self.rich_text_read(element, range)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_range_at(
        &self,
        element: &ElementData,
        offset: u32,
        unit: xa11y_core::text_api::TextUnit,
    ) -> Result<xa11y_core::TextRange> {
        self.rich_text_range_at(element, offset, unit)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_range_bounds(
        &self,
        element: &ElementData,
        range: xa11y_core::TextRange,
    ) -> Result<Option<Rect>> {
        self.rich_text_range_bounds(element, range)
            .map_err(|e| self.rich_stale(element, e))
    }

    fn text_attributes_at(
        &self,
        element: &ElementData,
        offset: u32,
    ) -> Result<xa11y_core::text_api::TextRun> {
        self.rich_text_attributes_at(element, offset)
            .map_err(|e| self.rich_stale(element, e))
    }

    // ── Generic action escape hatch ─────────────────────────────────

    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()> {
        match action {
            "press" => self.press(element),
            "focus" => self.focus(element),
            "blur" => self.blur(element),
            "toggle" => self.toggle(element),
            "select" => self.select(element),
            "expand" => self.expand(element),
            "collapse" => self.collapse(element),
            "show_menu" => self.show_menu(element),
            "increment" => self.increment(element),
            "decrement" => self.decrement(element),
            "scroll_into_view" => self.scroll_into_view(element),
            "activate" => self.activate(element),
            "minimize" => self.minimize(element),
            "maximize" => self.maximize(element),
            "enter_fullscreen" => self.enter_fullscreen(element),
            "restore" => self.restore(element),
            "close" => self.close(element),
            // Payload verbs have no arguments on the generic escape hatch;
            // fail surfaceably with how to call them instead of guessing
            // (tenet 1: no silent fallback).
            "move_to" => Err(Error::invalid_action_data(
                "perform_action(\"move_to\") requires coordinates; call move_to(x, y)",
            )),
            "resize_to" => Err(Error::invalid_action_data(
                "perform_action(\"resize_to\") requires dimensions; call \
                           resize_to(width, height)",
            )),
            _ => {
                // Custom action resolution: snake_case → AXPascalCase
                let ax = self.get_cached(element.handle)?;
                let el_ptr = ax.as_ptr();
                let available = ax_action_names(el_ptr);

                // Strategy 1: snake_case → AXPascalCase
                let ax_name = snake_to_ax_pascal(action);
                if available.iter().any(|a| a == &ax_name) {
                    let cf_action = CFString::new(&ax_name);
                    let err = do_perform_action(el_ptr, &cf_action);
                    if err != AX_ERROR_SUCCESS {
                        return Err(action_error(el_ptr, err, action, element.role, &ax_name));
                    }
                    return Ok(());
                }

                // Strategy 2: literal name
                if available.iter().any(|a| a == action) {
                    let cf_action = CFString::new(action);
                    let err = do_perform_action(el_ptr, &cf_action);
                    if err != AX_ERROR_SUCCESS {
                        return Err(action_error(el_ptr, err, action, element.role, action));
                    }
                    return Ok(());
                }

                Err(Error::ActionNotSupported {
                    action: action.to_string(),
                    role: element.role,
                })
            }
        }
    }

    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        let pid = element
            .pid
            .ok_or(Error::platform(-1, "Element has no PID for subscribe"))?;
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(pid as i32, app_name)
    }
}

// ── Event subscription ──────────────────────────────────────────────────────

struct ObserverContext {
    sender: xa11y_core::EventSender,
    app_name: String,
    app_pid: u32,
    /// Set by the subscription's close. A callback that sees it returns
    /// without reading anything, so a close waits for at most the one
    /// callback already running.
    closing: std::sync::atomic::AtomicBool,
}

/// The time one notification callback may spend reading its target.
///
/// A window notification snapshots the window, and against a hung app each
/// read waits for the process-wide messaging timeout: unbounded, one
/// callback ran for many of those, and closing the subscription (which
/// joins the run-loop thread) blocked for all of it. One messaging timeout
/// for the whole callback keeps a responsive app's snapshot intact and puts
/// a hung one's at "target unknown", the documented `None`.
const OBSERVER_CALLBACK_BUDGET: Duration =
    Duration::from_millis((AX_MESSAGING_TIMEOUT_SECS * 1000.0) as u64);

/// How long closing a subscription waits for its run-loop thread before
/// handing the rest of the teardown to a background thread.
const SUBSCRIPTION_CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// `NSAccessibilityPriorityHigh`; `Low` is 10 and `Medium` 50.
const AX_PRIORITY_HIGH: i64 = 90;

/// A string value of a borrowed `userInfo` dictionary, or `None`.
fn info_string(info: CFTypeRef, key: &str) -> Option<String> {
    if info.is_null() {
        return None;
    }
    let key = CFString::new(key);
    unsafe {
        let v = safe_cf_dict_get_value(info, key.as_concrete_TypeRef() as CFTypeRef);
        if v.is_null() || safe_cf_get_type_id(v) != safe_cf_string_get_type_id() {
            return None;
        }
        // Borrowed from the dictionary (Get rule).
        cf_string(v)
    }
}

/// An integer value of a borrowed `userInfo` dictionary, or `None`.
fn info_i64(info: CFTypeRef, key: &str) -> Option<i64> {
    if info.is_null() {
        return None;
    }
    let key = CFString::new(key);
    unsafe {
        let v = safe_cf_dict_get_value(info, key.as_concrete_TypeRef() as CFTypeRef);
        if v.is_null() || safe_cf_get_type_id(v) != safe_cf_number_get_type_id() {
            return None;
        }
        let mut out: i64 = 0;
        safe_cf_number_get_value(v, CF_NUMBER_SINT64, &mut out as *mut _ as *mut c_void)
            .then_some(out)
    }
}

/// The payload of an AX announcement-like notification.
fn ax_announcement(
    notification: &str,
    info: CFTypeRef,
    target: Option<&ElementData>,
) -> Option<xa11y_core::AnnouncementInfo> {
    use xa11y_core::{AnnouncementOrigin, AnnouncementParts, Politeness};
    match notification {
        "AXAnnouncementRequested" => {
            let priority = info_i64(info, "AXPriorityKey");
            Some(
                AnnouncementParts {
                    origin: AnnouncementOrigin::Request,
                    text: info_string(info, "AXAnnouncementKey").filter(|t| !t.is_empty()),
                    politeness: priority.map(|p| {
                        if p >= AX_PRIORITY_HIGH {
                            Politeness::Assertive
                        } else {
                            Politeness::Polite
                        }
                    }),
                    platform_priority: priority.map(|p| p.to_string()),
                    activity_id: None,
                }
                .into(),
            )
        }
        // The region changed; its own `AXARIALive` says how urgently.
        "AXLiveRegionChanged" => Some(
            AnnouncementParts {
                origin: AnnouncementOrigin::LiveRegion,
                text: None,
                politeness: target
                    .and_then(|t| t.live_region.as_ref())
                    .and_then(|l| l.politeness),
                platform_priority: None,
                activity_id: None,
            }
            .into(),
        ),
        _ => None,
    }
}

unsafe extern "C" fn ax_observer_callback(
    _observer: CFTypeRef,
    element: AXUIElementRef,
    notification: CFTypeRef,
    info: CFTypeRef,
    refcon: *mut c_void,
) {
    // SAFETY: `refcon` is the `ObserverContext` the subscription registered,
    // freed only after this run loop's thread has exited.
    let ctx = unsafe { &*(refcon as *const ObserverContext) };
    if ctx.closing.load(Ordering::SeqCst) {
        return;
    }
    xa11y_core::Deadline::after(OBSERVER_CALLBACK_BUDGET)
        .scope(|| handle_ax_notification(ctx, element, notification, info));
}

/// The body of [`ax_observer_callback`], run under its read budget.
fn handle_ax_notification(
    ctx: &ObserverContext,
    element: AXUIElementRef,
    notification: CFTypeRef,
    info: CFTypeRef,
) {
    let Some(notif_str) = cf_string(notification) else {
        eprintln!(
            "AX notification for pid {} carried no notification name; dropped",
            ctx.app_pid
        );
        return;
    };

    // Read the raw role string once — used for both kind dispatch and target building.
    // AXUIElementRef is only valid during this callback; snapshot all attributes now.
    let raw_role = if !element.is_null() {
        ax_string(element, "AXRole").unwrap_or_default()
    } else {
        String::new()
    };

    // Build the target element snapshot using the full batch attribute reader
    // so tests can assert on name, value, states, numeric_value, etc. An
    // AX notification callback is fire-and-forget: a failed snapshot still
    // delivers the event (the target is honestly unknown — the documented
    // `None` case), with the error diagnosed on stderr rather than silently
    // dropped (tenet 1).
    let target = if element.is_null() {
        None
    } else {
        match build_snapshot_data(element, Some(ctx.app_pid), 0) {
            Ok(data) => Some(data),
            Err(e) => {
                eprintln!(
                    "AX notification target snapshot failed ({}): {e:?}",
                    ctx.app_pid
                );
                None
            }
        }
    };

    // Alias for notification dispatch logic below.
    let role_str = raw_role.as_str();

    // Determine which event kind(s) to emit. Some notifications produce more
    // than one event (e.g. AXValueChanged on a checkbox also emits StateChanged).
    let kinds: Vec<EventKind> = match notif_str.as_str() {
        "AXFocusedUIElementChanged" => vec![EventKind::FocusChanged],

        "AXValueChanged" => {
            // Checkbox and radio toggles fire AXValueChanged — also emit
            // StateChanged { Checked } so consumers can filter on state.
            let mut ks = vec![EventKind::ValueChanged];
            match role_str {
                "AXCheckBox" | "AXRadioButton" => {
                    // Source of truth is the target snapshot, which already
                    // resolves the AXValue across CFBoolean / CFNumber shapes
                    // (AccessKit's macOS bridge uses CFBoolean for checkbox
                    // values, not CFNumber — ax_number_f64 would miss it).
                    let checked = target
                        .as_ref()
                        .and_then(|t| t.states.checked)
                        .map(|c| matches!(c, Toggled::On))
                        .unwrap_or(false);
                    ks.push(EventKind::StateChanged {
                        flag: StateFlag::Checked,
                        value: checked,
                    });
                }
                // Text fields: also emit TextChanged so consumers can filter
                // specifically on text content changes.
                "AXTextField" | "AXTextArea" | "AXSearchField" => {
                    ks.push(EventKind::TextChanged);
                }
                // Static text has no (non-empty) AXTitle, so its name is derived from
                // AXValue (see `build_snapshot_data`): a value change is also
                // a name change, and the Linux (AT-SPI `accessible-name`) and
                // Windows (UIA NameProperty) backends emit NameChanged for the
                // same label update. A static text that carries an AXTitle
                // keeps a title-derived name, so there a value change really is
                // only a value change.
                "AXStaticText" if !has_title_derived_name(target.as_ref()) => {
                    ks.push(EventKind::NameChanged);
                }
                // Sliders, spinners, progress bars, etc.: just ValueChanged.
                _ => {}
            }
            ks
        }

        "AXTitleChanged" => vec![EventKind::NameChanged],

        "AXElementBusyChanged" => {
            let busy = ax_bool(element, "AXElementBusy").unwrap_or(false);
            vec![EventKind::StateChanged {
                flag: StateFlag::Busy,
                value: busy,
            }]
        }

        "AXWindowCreated" => vec![EventKind::WindowOpened],

        "AXUIElementDestroyed" => {
            // Determine whether the destroyed element was a window.
            if matches!(role_str, "AXWindow") {
                vec![EventKind::WindowClosed]
            } else {
                vec![EventKind::StructureChanged]
            }
        }

        "AXFocusedWindowChanged" => vec![EventKind::WindowActivated],

        // Minimize/restore are a window *state* change, not an activation
        // change: a background window can be miniaturized and deminiaturized
        // without ever becoming the key window, so emitting
        // WindowDeactivated/WindowActivated here would report focus
        // transitions that never happened. AXFocusedWindowChanged above is
        // the only real activation signal. StateChanged{Minimized} is still
        // what makes the public StateFlag::Minimized observable on macOS.
        "AXWindowMiniaturized" => vec![EventKind::StateChanged {
            flag: StateFlag::Minimized,
            value: true,
        }],
        "AXWindowDeminiaturized" => vec![EventKind::StateChanged {
            flag: StateFlag::Minimized,
            value: false,
        }],

        // The caret moved or the text selection changed: its own kind, as on
        // Linux and Windows, not the container-selection one below.
        "AXSelectedTextChanged" => vec![EventKind::TextSelectionChanged],

        "AXSelectedRowsChanged" | "AXSelectedCellsChanged" | "AXSelectedChildrenChanged" => {
            vec![EventKind::SelectionChanged]
        }

        "AXMenuOpened" => vec![EventKind::MenuOpened],
        "AXMenuClosed" => vec![EventKind::MenuClosed],

        "AXAnnouncementRequested" | "AXLiveRegionChanged" => vec![EventKind::Announcement],

        _ => return,
    };

    let announcement = ax_announcement(notif_str.as_str(), info, target.as_ref());
    for kind in kinds {
        let event: Event = EventParts {
            announcement: (kind == EventKind::Announcement)
                .then(|| announcement.clone())
                .flatten(),
            kind,
            target: target.clone(),
            app_name: ctx.app_name.clone(),
            app_pid: ctx.app_pid,
            timestamp: std::time::Instant::now(),
            // AXValueChanged carries no delta (#480).
            text_change: None,
            sequence: 0,
        }
        .into();
        // Fails only once the subscription is dropped; the cancel path
        // stops this run loop.
        let _ = ctx.sender.send(event);
    }
}

/// How long `subscribe()` keeps retrying transient `AXObserverAddNotification`
/// failures while the target app finishes launching, and the pause between
/// attempts. Cold CI runners can enumerate an app that still answers
/// `kAXErrorCannotComplete` for a moment; 2s comfortably covers that window
/// without masking a genuinely broken target for long.
const SUBSCRIBE_REGISTER_RETRY_BUDGET: Duration = Duration::from_secs(2);
const SUBSCRIBE_REGISTER_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// One pass of `AXObserverAddNotification` over `notifications`. On the first
/// failure, rolls back the registrations made so far — so the observer holds
/// no registrations referencing `ctx_ptr` when the caller frees it — and
/// returns the failing AXError together with the notification name. Removal
/// results during rollback are deliberately ignored: this is already an error
/// path and the registration failure is the error the caller acts on.
fn register_notifications(
    observer: CFTypeRef,
    app_element: AXUIElementRef,
    ctx_ptr: *mut c_void,
    notifications: &[&'static str],
) -> std::result::Result<(), (i32, &'static str)> {
    for (idx, notif) in notifications.iter().enumerate() {
        let name = CFString::new(notif);
        let err = unsafe {
            safe_ax_observer_add_notification(
                observer,
                app_element,
                name.as_concrete_TypeRef() as CFTypeRef,
                ctx_ptr,
            )
        };
        if err != AX_ERROR_SUCCESS {
            for added in &notifications[..idx] {
                let added_name = CFString::new(added);
                let _ = unsafe {
                    safe_ax_observer_remove_notification(
                        observer,
                        app_element,
                        added_name.as_concrete_TypeRef() as CFTypeRef,
                    )
                };
            }
            return Err((err, notif));
        }
    }
    Ok(())
}

/// Stop a subscription's run loop until its thread has exited, or `limit`
/// has passed. True once the thread has exited.
///
/// CFRunLoopStop only stops a loop that is already running. The worker
/// reports its run loop before it calls CFRunLoopRun, so a cancel right after
/// subscribe can land in between; a single stop is then lost. So the stop is
/// repeated until the worker has exited. The run loop is retained by the
/// caller, so a stop that races the worker's exit is harmless.
fn stop_run_loop_thread(
    handle: &std::thread::JoinHandle<()>,
    run_loop: usize,
    limit: Option<Duration>,
) -> bool {
    let start = Instant::now();
    while !handle.is_finished() {
        if limit.is_some_and(|l| start.elapsed() >= l) {
            return false;
        }
        unsafe { safe_cf_run_loop_stop(run_loop as CFTypeRef) };
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

impl MacOSProvider {
    fn subscribe_impl(&self, pid: i32, app_name: String) -> Result<Subscription> {
        let (tx, rx) = xa11y_core::event_channel();

        let ctx = Box::new(ObserverContext {
            sender: tx,
            app_name,
            app_pid: pid as u32,
            closing: std::sync::atomic::AtomicBool::new(false),
        });
        let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

        let mut observer: CFTypeRef = std::ptr::null();
        let err =
            unsafe { safe_ax_observer_create_with_info(pid, ax_observer_callback, &mut observer) };
        if err != AX_ERROR_SUCCESS || observer.is_null() {
            unsafe { drop(Box::from_raw(ctx_ptr as *mut ObserverContext)) };
            return Err(Error::platform(
                err as i64,
                "AXObserverCreateWithInfoCallback failed",
            ));
        }

        let app_element = unsafe { safe_ax_create_application(pid) };
        if app_element.is_null() {
            unsafe {
                safe_cf_release(observer);
                drop(Box::from_raw(ctx_ptr as *mut ObserverContext));
            }
            return Err(
                Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
                    xa11y_core::Diagnosis::new().last_observed(
                        "AXUIElementCreateApplication returned NULL while subscribing",
                    ),
                ),
            );
        }

        let notifications = [
            "AXFocusedUIElementChanged",
            "AXValueChanged",
            "AXTitleChanged",
            "AXElementBusyChanged",
            "AXWindowCreated",
            "AXUIElementDestroyed",
            "AXFocusedWindowChanged",
            "AXWindowMiniaturized",
            "AXWindowDeminiaturized",
            "AXSelectedTextChanged",
            "AXSelectedRowsChanged",
            "AXSelectedCellsChanged",
            "AXSelectedChildrenChanged",
            "AXMenuOpened",
            "AXMenuClosed",
            "AXAnnouncementRequested",
            "AXLiveRegionChanged",
        ];

        // Register every notification, failing the whole subscribe on the
        // first persistent error (tenet 1): a partially-registered observer
        // would silently never deliver the missing event kinds. Mirrors the
        // Windows backend's AddAutomationEventHandler rollback.
        //
        // A freshly launched app can be enumerable yet still answer
        // kAXErrorCannotComplete / kAXErrorInvalidUIElement while its AX
        // bridge finishes initialising (cold CI runners especially). Those
        // codes are transient "not ready yet" signals, so registration is
        // retried as a whole — each failed attempt rolls back its partial
        // registrations inside `register_notifications` — until
        // SUBSCRIBE_REGISTER_RETRY_BUDGET elapses. This is an explicit,
        // bounded retry of the same mechanism, not a fallback: on exhaustion
        // the original AXError is surfaced unchanged.
        let retry_start = Instant::now();
        loop {
            match register_notifications(observer, app_element, ctx_ptr, &notifications) {
                Ok(()) => break,
                Err((err, _))
                    if matches!(err, AX_ERROR_CANNOT_COMPLETE | AX_ERROR_INVALID_UI_ELEMENT)
                        && retry_start.elapsed() < SUBSCRIBE_REGISTER_RETRY_BUDGET =>
                {
                    std::thread::sleep(SUBSCRIBE_REGISTER_RETRY_INTERVAL);
                }
                Err((err, notif)) => {
                    // Release everything this function owns so far:
                    // `app_element` (released after the loop on the success
                    // path), `observer`, and the leaked `ObserverContext`
                    // box. We return before the success-path release /
                    // CancelHandle ownership transfer, so nothing is
                    // double-released.
                    unsafe {
                        safe_cf_release(app_element);
                        safe_cf_release(observer);
                        drop(Box::from_raw(ctx_ptr as *mut ObserverContext));
                    }
                    return Err(Error::platform(
                        err as i64,
                        format!("AXObserverAddNotification({notif}) failed"),
                    ));
                }
            }
        }

        unsafe { safe_cf_release(app_element) };

        let (rl_tx, rl_rx) = std::sync::mpsc::sync_channel::<usize>(1);
        let observer_usize = observer as usize;

        let handle = std::thread::spawn(move || {
            let obs = observer_usize as CFTypeRef;
            unsafe {
                let source = safe_ax_observer_get_run_loop_source(obs);
                if source.is_null() {
                    return;
                }
                safe_cf_run_loop_add_source(source);
                // Retained for the cancel handle, which may still call
                // CFRunLoopStop on it after this thread (and its run loop's
                // last owner) has exited. Released after the join.
                let rl = safe_cf_retain(safe_cf_run_loop_get_current());
                if rl_tx.send(rl as usize).is_err() {
                    // subscribe gave up waiting and will never cancel us.
                    safe_cf_release(rl);
                    return;
                }
                safe_cf_run_loop_run();
            }
        });

        let run_loop_usize = match rl_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(rl) => rl,
            Err(_) => {
                // Either the worker thread exited before reporting its
                // RunLoop pointer (source-null case: the sender drops and
                // `recv_timeout` returns `Disconnected` almost immediately),
                // or the thread is genuinely stuck (should not happen —
                // `rl_tx.send` precedes `safe_cf_run_loop_run`). Either way,
                // clean up rather than leaking `observer`, the
                // `ObserverContext` box, and the thread handle, which the
                // original code dropped on the ground when `?` fired.
                //
                // `handle.is_finished()` lets us join without risk of hang
                // in the common (source-null) case; in the unlikely
                // still-running case we release the observer but abandon
                // the thread handle — releasing the observer tears down
                // the run-loop source the thread is polling, so it will
                // wake up and exit on its own soon after.
                if handle.is_finished() {
                    let _ = handle.join();
                }
                unsafe {
                    safe_cf_release(observer);
                    drop(Box::from_raw(ctx_ptr as *mut ObserverContext));
                }
                return Err(Error::platform(-1, "Failed to start observer RunLoop"));
            }
        };

        let ctx_usize = ctx_ptr as usize;

        let cancel = CancelHandle::new(move || {
            // No callback that starts from here on reads anything, and the
            // one already running (if any) is bounded by
            // `OBSERVER_CALLBACK_BUDGET`.
            // SAFETY: the context is freed only after the thread has exited,
            // below or in the reaper.
            unsafe { &*(ctx_usize as *const ObserverContext) }
                .closing
                .store(true, Ordering::SeqCst);
            let teardown = move |handle: std::thread::JoinHandle<()>| {
                // The thread has finished; join only collects its result. A
                // panic in it was already reported on stderr by the runtime.
                let _ = handle.join();
                unsafe {
                    safe_cf_release(run_loop_usize as CFTypeRef);
                    drop(Box::from_raw(ctx_usize as *mut ObserverContext));
                    safe_cf_release(observer_usize as CFTypeRef);
                }
            };
            if stop_run_loop_thread(&handle, run_loop_usize, Some(SUBSCRIPTION_CLOSE_BUDGET)) {
                teardown(handle);
                return;
            }
            // The run-loop thread is still inside a callback (an app that
            // stopped answering mid-read). Close returns now rather than
            // block on it; a background thread finishes stopping the loop
            // and frees the observer and its context once the thread has
            // exited, never before (the callback still holds the context).
            eprintln!(
                "xa11y: closing the event subscription for pid {pid}: its notification \
                 thread did not stop within {SUBSCRIPTION_CLOSE_BUDGET:?}; finishing the \
                 teardown in the background"
            );
            std::thread::spawn(move || {
                stop_run_loop_thread(&handle, run_loop_usize, None);
                teardown(handle);
            });
        });

        Ok(Subscription::new(rx, cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objc_exception_is_caught_by_c_wrapper() {
        let result = unsafe { test_throw_and_catch_nsexception() };
        assert_eq!(result, 1, "C wrapper should have caught the NSException");
    }

    #[test]
    fn a_failure_after_a_completed_step_is_never_not_delivered() {
        let refused = Error::ActionNotSupported {
            action: "minimize".to_string(),
            role: Role::Window,
        };
        assert_eq!(refused.delivery(), xa11y_core::Delivery::NotDelivered);
        let e = unknown_after_step(refused, "leaving fullscreen");
        assert_eq!(e.delivery(), xa11y_core::Delivery::Unknown);
    }

    #[test]
    fn maximize_is_platform_unsupported() {
        let provider = MacOSProvider::new().expect("provider construction must succeed");
        let window = ElementData::for_role(Role::Window);
        let err = provider
            .maximize(&window)
            .expect_err("macOS has no accessible maximize operation");
        assert!(
            matches!(
                err,
                Error::Unsupported { ref feature }
                    if feature.starts_with("maximize on window:")
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn safe_ax_copy_attribute_returns_error_on_null_element() {
        let attr = CFString::new("AXRole");
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe {
            safe_ax_copy_attribute_value(
                std::ptr::null(),
                attr.as_concrete_TypeRef() as CFTypeRef,
                &mut value,
            )
        };
        assert_ne!(err, AX_ERROR_SUCCESS);
    }

    #[test]
    fn ax_attr_returns_none_for_null_element() {
        let result = ax_attr(std::ptr::null(), "AXRole");
        assert!(result.is_none());
    }

    #[test]
    fn find_close_button_propagates_read_errors_for_null_element() {
        // A null element answers with an error, not "no close button": the
        // attribute is unknown, so the error must propagate (the close
        // advertisement and `close()` must not silently drop the capability
        // on a transient AX failure). A null AXUIElementRef fails the
        // underlying call with kAXErrorInvalidUIElement.
        let result = find_close_button(std::ptr::null(), Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn settle_window_fullscreen_propagates_unreadable_element() {
        // A null element cannot resolve its owning application, so the settle
        // must not report a committed state: the verb's promise is
        // unverifiable, and a silent success would be exactly the bug
        // enter-fullscreen/restore idempotency is about (tenet 1).
        let result =
            settle_window_fullscreen(std::ptr::null(), true, "enter_fullscreen", Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn settle_bool_attr_propagates_unreadable_element() {
        // The minimized settle has the same contract: a null element cannot
        // resolve its owning application, so the promise is unverifiable and
        // must not report a committed state (tenet 1).
        let result = settle_bool_attr(
            std::ptr::null(),
            "AXMinimized",
            true,
            "minimize",
            Role::Window,
        );
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn settle_window_position_propagates_unreadable_element() {
        // The move settle resolves the owning application before polling, so
        // an unreadable target must surface its platform failure rather than
        // being retried into a timeout.
        let result = settle_window_position(std::ptr::null(), 10, 20, "move_to", Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn settle_window_size_propagates_unreadable_element() {
        // Same contract as the move settle: an unreadable target surfaces its
        // platform failure instead of a resize reported as applied.
        let result = settle_window_size(std::ptr::null(), 900, 700, "resize_to", Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn refused_resize_names_the_requested_and_observed_rects() {
        let msg = describe_refused_resize(900, 723, (850.0, 673.0), "(0, 31)");
        assert!(msg.contains("requested 900x723 at origin (0, 31)"), "{msg}");
        assert!(msg.contains("window reports 850x673"), "{msg}");
        assert!(msg.contains("visible frame"), "{msg}");
    }

    #[test]
    fn geometry_near_allows_only_the_documented_rounding_tolerance() {
        assert!(geometry_near((12.0, 18.0), (10.0, 20.0)));
        assert!(!geometry_near((12.1, 18.0), (10.0, 20.0)));
        assert!(!geometry_near((12.0, 17.9), (10.0, 20.0)));
    }

    /// An enumerated target sample holding `fullscreen` / `settable`.
    fn settle_poll(fullscreen: bool, settable: bool) -> Option<TargetSample> {
        Some(TargetSample::Enumerated {
            fullscreen,
            settable,
        })
    }

    /// A poll answered by the cached handle.
    fn cached_poll(fullscreen: AttrProbe, settable: AttrProbe) -> Option<TargetSample> {
        Some(TargetSample::Cached {
            fullscreen,
            settable,
        })
    }

    #[test]
    fn settle_run_confirms_the_promise_only_after_the_grace() {
        let t0 = Instant::now();
        let promise = settle_poll(true, true);
        let mut run = SettleRun::default();

        // Reaching the promise starts the hold; it is not confirmation by
        // itself, because a queued transition reports the same value.
        run.observe(promise.as_ref(), true, t0);
        assert!(!run.confirmed(t0));
        assert!(!run.confirmed(t0 + WINDOW_FULLSCREEN_SETTLE_GRACE - Duration::from_millis(1)));

        // Still holding once the grace has passed: confirmed.
        run.observe(promise.as_ref(), true, t0 + WINDOW_FULLSCREEN_SETTLE_GRACE);
        assert!(run.confirmed(t0 + WINDOW_FULLSCREEN_SETTLE_GRACE));
    }

    #[test]
    fn settle_run_restarts_the_hold_on_a_state_change() {
        let t0 = Instant::now();
        let promise = settle_poll(true, true);
        let mut run = SettleRun::default();

        run.observe(promise.as_ref(), true, t0);
        // A state away from the promise restarts the hold: the time already
        // spent holding must not count toward the new run.
        run.observe(
            settle_poll(false, true).as_ref(),
            false,
            t0 + Duration::from_millis(50),
        );
        run.observe(promise.as_ref(), true, t0 + Duration::from_millis(100));
        assert!(!run.confirmed(
            t0 + Duration::from_millis(100) + WINDOW_FULLSCREEN_SETTLE_GRACE
                - Duration::from_millis(1)
        ));
        assert!(run.confirmed(t0 + Duration::from_millis(100) + WINDOW_FULLSCREEN_SETTLE_GRACE));
    }

    #[test]
    fn settle_run_restarts_the_hold_on_an_unreadable_poll() {
        let t0 = Instant::now();
        let promise = settle_poll(true, true);
        let mut run = SettleRun::default();

        run.observe(promise.as_ref(), true, t0);
        // An unreadable sample is unknown, not evidence the promise holds:
        // there is no boundary shortcut, so the hold starts over.
        run.observe(None, false, t0 + Duration::from_millis(50));
        run.observe(promise.as_ref(), true, t0 + Duration::from_millis(100));
        assert!(!run.confirmed(
            t0 + Duration::from_millis(100) + WINDOW_FULLSCREEN_SETTLE_GRACE
                - Duration::from_millis(1)
        ));
        assert!(run.confirmed(t0 + Duration::from_millis(100) + WINDOW_FULLSCREEN_SETTLE_GRACE));
    }

    #[test]
    fn settle_run_never_confirms_on_the_cached_handle() {
        // The target off `AXWindows` with its cached handle reading the
        // promise is not confirmation, however long it lasts: the caller's
        // next read goes through `AXWindows` and would not find the window.
        // This is the "no window advertises enter_fullscreen" flake: the old
        // run confirmed the cached handle after the grace.
        let t0 = Instant::now();
        let cached = cached_poll(AttrProbe::Value(true), AttrProbe::Value(true));
        let mut run = SettleRun::default();

        let mut now = t0;
        while now < t0 + WINDOW_FULLSCREEN_SETTLE_BUDGET {
            run.observe(cached.as_ref(), true, now);
            assert!(
                !run.confirmed(now),
                "confirmed off AXWindows at {:?}",
                now - t0
            );
            now += WINDOW_FULLSCREEN_SETTLE_INTERVAL;
        }
    }

    #[test]
    fn settle_run_restarts_the_enumerated_hold_after_an_absence() {
        let t0 = Instant::now();
        let promise = settle_poll(true, true);
        let cached = cached_poll(AttrProbe::Value(true), AttrProbe::Value(true));
        let mut run = SettleRun::default();

        // Held while enumerated, then off the list, then back: neither the
        // time before the absence nor the absence itself counts.
        run.observe(promise.as_ref(), true, t0);
        run.observe(cached.as_ref(), true, t0 + Duration::from_millis(100));
        assert!(!run.confirmed(t0 + Duration::from_millis(100) + WINDOW_FULLSCREEN_SETTLE_GRACE));
        let back = t0 + Duration::from_millis(1500);
        run.observe(promise.as_ref(), true, back);
        assert!(!run.confirmed(back + WINDOW_FULLSCREEN_SETTLE_GRACE - Duration::from_millis(1)));
        assert!(run.confirmed(back + WINDOW_FULLSCREEN_SETTLE_GRACE));
    }

    #[test]
    fn settle_run_diagnosis_names_the_missing_half() {
        let t0 = Instant::now();
        let cached = cached_poll(AttrProbe::Value(true), AttrProbe::Value(true));

        // Never in the enumerated set.
        let mut run = SettleRun::default();
        run.observe(cached.as_ref(), true, t0);
        let described = run.describe_failure(cached.as_ref(), t0);
        assert!(described.contains("was never enumerated"), "{described}");
        assert!(described.contains("reads AXFullScreen=true"), "{described}");
        assert!(described.contains("ever enumerated: false"), "{described}");

        // Listed and holding, then gone at the deadline.
        let mut run = SettleRun::default();
        run.observe(settle_poll(true, true).as_ref(), true, t0);
        run.observe(cached.as_ref(), true, t0 + Duration::from_millis(50));
        let deadline = t0 + Duration::from_millis(250);
        let described = run.describe_failure(cached.as_ref(), deadline);
        assert!(
            described.contains("was not re-enumerated before the deadline"),
            "{described}"
        );
        assert!(
            described.contains("last off AXWindows 200 ms before the deadline"),
            "{described}"
        );
        assert!(
            described.contains("the target was off AXWindows"),
            "{described}"
        );
        assert!(
            described.contains("holds broken before the grace: 1"),
            "{described}"
        );

        // Listed at the deadline, but the state is wrong there.
        let wrong = settle_poll(false, true);
        run.observe(wrong.as_ref(), false, deadline);
        let described = run.describe_failure(wrong.as_ref(), deadline);
        assert!(
            described.contains("is enumerated, but the promised state did not hold"),
            "{described}"
        );
        assert!(described.contains("AXFullScreen=false"), "{described}");
        assert!(described.contains("away from the promise"), "{described}");

        // A churned last sample keeps the run's history.
        run.observe(None, false, deadline);
        let described = run.describe_failure(None, deadline);
        assert!(
            described.contains("did not answer a readable sample"),
            "{described}"
        );
        assert!(described.contains("ever enumerated: true"), "{described}");
        assert!(described.contains("last off AXWindows"), "{described}");
        assert!(described.contains("no readable sample"), "{described}");
    }

    #[test]
    fn target_sample_evaluates_the_verb_promise() {
        let enumerated = |fullscreen, settable| TargetSample::Enumerated {
            fullscreen,
            settable,
        };
        let cached = |fullscreen, settable| TargetSample::Cached {
            fullscreen,
            settable,
        };
        let churn = || AttrProbe::Churn("invalidated".to_string());

        // Enter fullscreen: the promise is `fullscreen=true`. An absent
        // attribute on an enumerated window reads as false (it has no
        // fullscreen surface), and a cached `true` whose settability churned
        // is unknown, not reached.
        assert!(enumerated(true, false).evaluate(true).0);
        assert!(!enumerated(false, true).evaluate(true).0);
        assert!(
            cached(AttrProbe::Value(true), AttrProbe::Value(false))
                .evaluate(true)
                .0
        );
        assert!(!cached(churn(), AttrProbe::Value(true)).evaluate(true).0);
        assert!(
            !cached(AttrProbe::Value(false), AttrProbe::Value(true))
                .evaluate(true)
                .0
        );

        // Restore: the promise is the pair `fullscreen=false` + settable, so
        // a bare `false` on a non-settable attribute is not it.
        assert!(enumerated(false, true).evaluate(false).0);
        assert!(!enumerated(false, false).evaluate(false).0);
        assert!(!enumerated(true, true).evaluate(false).0);
        assert!(
            cached(AttrProbe::Value(false), AttrProbe::Value(true))
                .evaluate(false)
                .0
        );
        assert!(!cached(AttrProbe::Value(false), churn()).evaluate(false).0);
        assert!(
            !cached(AttrProbe::Value(true), AttrProbe::Value(true))
                .evaluate(false)
                .0
        );

        // The settability side is what the caller consults before re-issuing
        // the set: an enumerated sample always answers it, a cached one may
        // have churned.
        assert_eq!(enumerated(true, false).evaluate(true).1, Some(false));
        assert_eq!(
            cached(AttrProbe::Value(true), churn()).evaluate(true).1,
            None
        );
    }

    #[test]
    fn describe_target_names_the_target_state() {
        // An enumerated target names both halves of the restore promise.
        let described = describe_target(Some(&TargetSample::Enumerated {
            fullscreen: false,
            settable: true,
        }));
        assert!(
            described.contains("the target window reads AXFullScreen=false (settable=true)"),
            "{described}"
        );
        // The target can be transiently absent from `AXWindows`; the
        // diagnosis must say so and carry the cached handle's answer.
        let described = describe_target(Some(&TargetSample::Cached {
            fullscreen: AttrProbe::Value(true),
            settable: AttrProbe::Value(false),
        }));
        assert!(
            described.contains("the target window is not enumerable"),
            "{described}"
        );
        assert!(
            described.contains("reads AXFullScreen=true (settable=false)"),
            "{described}"
        );
        // Every read of the poll failed: the diagnosis says the app did not
        // answer rather than implying a state.
        let described = describe_target(None);
        assert!(
            described.contains("did not answer a readable sample"),
            "{described}"
        );
    }

    #[test]
    fn classify_element_churn_folds_only_the_retryable_ax_codes() {
        let platform = |code: i32| Error::platform(code as i64, String::new());
        // The one code that means "this AX object no longer exists": the
        // enumerating callers drop the element instead of failing the whole
        // listing/search.
        assert_eq!(
            classify_element_churn(&platform(AX_ERROR_INVALID_UI_ELEMENT)),
            ElementChurn::Gone
        );
        // A freshly launched process owns its window before its accessibility
        // server answers; a busy process and one without a bridge answer the
        // same way. A listing skips these and re-enumerates — the app was
        // about to become discoverable — while a content walk propagates
        // them, because the answer is unknown, not absent.
        assert_eq!(
            classify_element_churn(&platform(AX_ERROR_CANNOT_COMPLETE)),
            ElementChurn::Unreachable
        );
        assert_eq!(
            classify_element_churn(&platform(AX_ERROR_NOT_IMPLEMENTED)),
            ElementChurn::Unreachable
        );
        // A malformed answer or any other platform failure is a real error,
        // not an absence to retry.
        assert_eq!(
            classify_element_churn(&platform(AX_ERROR_ACTION_UNSUPPORTED)),
            ElementChurn::Fatal
        );
        // Non-platform errors are never churn.
        assert_eq!(
            classify_element_churn(&Error::stale(
                xa11y_core::StaleReason::Removed,
                "handle:1".to_string()
            )),
            ElementChurn::Fatal
        );
        // `kAXErrorNoValue` is not retryable on its own: the process
        // answered. The attach probe adds it at its call site.
        assert_eq!(
            classify_element_churn(&platform(AX_ERROR_NO_VALUE)),
            ElementChurn::Fatal
        );
    }

    #[test]
    fn classify_element_churn_code_draws_the_same_lines_as_the_error_form() {
        // The raw-code form backs the AX probes that never build an `Error`
        // (`ElementProbe::Unanswered`, the attach probe); it must fold exactly
        // the codes the `Error` form folds, or the two shapes have drifted.
        for code in [
            AX_ERROR_INVALID_UI_ELEMENT,
            AX_ERROR_CANNOT_COMPLETE,
            AX_ERROR_NOT_IMPLEMENTED,
            AX_ERROR_ACTION_UNSUPPORTED,
            AX_ERROR_NO_VALUE,
        ] {
            assert_eq!(
                classify_element_churn_code(i64::from(code)),
                classify_element_churn(&Error::platform(i64::from(code), String::new())),
                "AXError {code}"
            );
        }
    }

    #[test]
    fn individual_fallback_propagates_an_unreadable_role() {
        // The batch read can fail for an AXUIElement that is invalidated
        // mid-transition, and the element can be invalidated again before the
        // individual fallback reaches AXRole. That read must carry the
        // platform error out so the enumerating callers drop the element,
        // instead of answering an empty role that builds a ghost (tenet 1).
        // A null AXUIElementRef fails the underlying call with
        // kAXErrorInvalidUIElement.
        let result = ResolvedAttrs::from_individual(std::ptr::null());
        assert!(
            matches!(result, Err(Error::Platform { .. })),
            "an unreadable role must propagate as a platform error"
        );
    }

    #[test]
    fn window_verb_predicates_cover_the_advertised_combinations() {
        // The predicates are the contract shared by the `actions`
        // advertisement and the verb dispatch, so each combination of the
        // four probed readings that decides them is pinned as one row.
        //
        // A case is (minimized, minimized_settable, fullscreen,
        // fullscreen_settable, expected).
        type Case = (Option<bool>, bool, Option<bool>, bool, bool);
        let window_state = |minimized: Option<bool>,
                            minimized_settable: bool,
                            fullscreen: Option<bool>,
                            fullscreen_settable: bool| WindowState {
            minimized,
            minimized_settable,
            fullscreen,
            fullscreen_settable,
        };

        // `enter_fullscreen`: a committed fullscreen window is accepted
        // whatever the settability probe answers — the repeated-call case the
        // advertisement must not drop. A minimized window whose flag cannot
        // be cleared stays in the Dock while the settle only checks
        // fullscreen, so the verb is refused before mutating; not fullscreen
        // and not writable is `ActionNotSupported`.
        let enter_fullscreen_cases: &[Case] = &[
            (None, false, Some(true), false, true),
            (Some(false), false, Some(true), true, true),
            (Some(false), false, Some(false), true, true),
            (None, false, None, true, true),
            (Some(true), true, Some(false), true, true),
            (Some(true), false, Some(false), true, false),
            (Some(true), false, Some(true), false, false),
            (Some(false), false, Some(false), false, false),
            (None, false, None, false, false),
        ];
        for &(minimized, minimized_settable, fullscreen, fullscreen_settable, expected) in
            enter_fullscreen_cases
        {
            let state = window_state(
                minimized,
                minimized_settable,
                fullscreen,
                fullscreen_settable,
            );
            assert_eq!(
                enter_fullscreen_supported(&state),
                expected,
                "enter_fullscreen: {state:?}"
            );
        }

        // `minimize`: `AXMinimized` must be writable, and a fullscreen window
        // must be leavable first — AppKit ignores the minimized set until the
        // window is out of fullscreen, so the verb would otherwise report
        // success without minimizing.
        let minimize_cases: &[Case] = &[
            (None, true, None, false, true),
            (Some(false), true, Some(false), false, true),
            (Some(true), true, None, true, true),
            (Some(true), true, Some(true), true, true),
            (None, false, None, false, false),
            (Some(true), true, Some(true), false, false),
            (Some(false), true, Some(true), false, false),
        ];
        for &(minimized, minimized_settable, fullscreen, fullscreen_settable, expected) in
            minimize_cases
        {
            let state = window_state(
                minimized,
                minimized_settable,
                fullscreen,
                fullscreen_settable,
            );
            assert_eq!(minimize_supported(&state), expected, "minimize: {state:?}");
        }

        // `restore`: at least one state must be reachable, and every state
        // that reads `true` must be clearable — an unsupported AX set no-ops
        // silently, so the verb is refused before mutating anything.
        let restore_cases: &[Case] = &[
            (Some(false), true, Some(true), true, true),
            (Some(false), true, Some(false), false, true),
            (Some(false), false, Some(false), true, true),
            (None, false, None, true, true),
            (Some(true), true, Some(false), false, true),
            (Some(true), true, Some(true), true, true),
            (Some(true), false, Some(false), true, false),
            (Some(true), false, Some(true), false, false),
            (Some(true), false, Some(true), true, false),
            (Some(false), true, Some(true), false, false),
            (Some(false), false, Some(true), false, false),
            (Some(false), false, Some(false), false, false),
        ];
        for &(minimized, minimized_settable, fullscreen, fullscreen_settable, expected) in
            restore_cases
        {
            let state = window_state(
                minimized,
                minimized_settable,
                fullscreen,
                fullscreen_settable,
            );
            assert_eq!(restore_supported(&state), expected, "restore: {state:?}");
        }
    }

    #[test]
    fn an_empty_ax_title_is_not_a_title_derived_name() {
        let mut data = ElementData::for_role(Role::StaticText);
        assert!(!has_title_derived_name(Some(&data)), "no AXTitle at all");
        assert!(!has_title_derived_name(None), "no target");
        data.raw.insert("AXTitle".into(), serde_json::json!(""));
        assert!(
            !has_title_derived_name(Some(&data)),
            "Chromium text leaves answer an empty AXTitle"
        );
        data.raw
            .insert("AXTitle".into(), serde_json::json!("Label"));
        assert!(has_title_derived_name(Some(&data)));
    }

    #[test]
    fn a_static_text_without_a_value_is_named_by_its_ax_description() {
        let title = "Diwali (Deepawali). 8 November 2026, All-Day".to_string();
        let help = "This event belongs to calendar “India Holidays”.".to_string();
        for value in [None, Some(String::new())] {
            let name = accessible_name(
                Some(Role::StaticText),
                None,
                || value,
                || Some(title.clone()),
            );
            assert_eq!(name.as_ref(), Some(&title));
            let description =
                accessible_description(name.as_ref(), Some(title.clone()), Some(help.clone()));
            assert_eq!(description, Some(help.clone()));
        }
        let name = accessible_name(
            Some(Role::StaticText),
            Some(String::new()),
            || Some("Kesariya".into()),
            || Some(title.clone()),
        );
        assert_eq!(
            name.as_deref(),
            Some("Kesariya"),
            "a static text with a value keeps it as its name"
        );
        assert_eq!(
            accessible_description(name.as_ref(), Some(title.clone()), None),
            Some(title)
        );
    }

    #[test]
    fn ax_string_returns_none_for_null_element() {
        let result = ax_string(std::ptr::null(), "AXTitle");
        assert!(result.is_none());
    }

    #[test]
    fn probe_element_attr_reports_unanswered_for_null_element() {
        // A NULL element never answers, so the probe must not report the
        // attribute as absent — that reading is reserved for a process that
        // replied "I have no such attribute".
        match probe_element_attr(std::ptr::null(), "AXMenuBar") {
            ElementProbe::Unanswered(code) => assert_ne!(code, AX_ERROR_SUCCESS),
            ElementProbe::Found(_) => panic!("null element cannot vend an AXMenuBar"),
            ElementProbe::Absent => panic!("null element must not read as an honest absence"),
        }
    }

    #[test]
    fn global_messaging_timeout_is_accepted() {
        set_global_messaging_timeout().expect("system-wide messaging timeout");
    }

    fn call(code: i32, ms: u64) -> AxCall {
        AxCall {
            code,
            elapsed: Duration::from_millis(ms),
        }
    }

    #[test]
    fn cannot_complete_says_timeout_only_when_the_call_ran_that_long() {
        let timeout_ms = (AX_MESSAGING_TIMEOUT_SECS * 1000.0) as u64;
        // A call that ran the full timeout is reported as one, keeps its
        // kAXErrorCannotComplete code (so churn classification still sees
        // `Unreachable`), and warns that the action may have landed.
        let err = action_error(
            std::ptr::null(),
            call(AX_ERROR_CANNOT_COMPLETE, timeout_ms),
            "press",
            Role::Button,
            "AXPress failed",
        );
        match err {
            Error::Platform {
                code, ref message, ..
            } => {
                assert_eq!(code, AX_ERROR_CANNOT_COMPLETE as i64);
                assert!(message.contains("press: AXPress failed"), "{message}");
                assert!(message.contains("did not respond within"), "{message}");
                assert!(message.contains("pid unknown"), "{message}");
                assert!(
                    message.contains("may already have taken effect"),
                    "{message}"
                );
            }
            other => panic!("expected Platform, got {other:?}"),
        }
        assert_eq!(
            classify_element_churn(&err),
            ElementChurn::Unreachable,
            "the enriched error must still classify as unreachable"
        );

        // A fast refusal is not a timeout and must not be called one.
        let Error::Platform { message, .. } = action_error(
            std::ptr::null(),
            call(AX_ERROR_CANNOT_COMPLETE, 5),
            "press",
            Role::Button,
            "AXPress failed",
        ) else {
            panic!("expected Platform");
        };
        assert!(!message.contains("did not respond"), "{message}");
        assert!(message.contains("after 5 ms (not a timeout"), "{message}");

        // Other codes are untouched.
        assert!(matches!(
            action_error(
                std::ptr::null(),
                call(AX_ERROR_ACTION_UNSUPPORTED, 0),
                "press",
                Role::Button,
                "x"
            ),
            Error::ActionNotSupported { .. }
        ));
        assert!(matches!(
            action_error(std::ptr::null(), call(-1, 0), "press", Role::Button, "x"),
            Error::Platform { code: -1, ref message, .. } if message == "x"
        ));
    }

    #[test]
    fn a_message_after_the_deadline_is_not_sent_and_is_recorded() {
        let deadline = xa11y_core::Deadline::after(Duration::ZERO);
        let probe = deadline.clone();
        let mut sent = false;
        let code = deadline.scope(|| {
            ax_bounded(
                std::ptr::null(),
                || "reading AXRole".to_string(),
                || {
                    sent = true;
                    AX_ERROR_SUCCESS
                },
            )
        });
        assert_eq!(code, AX_ERROR_CANNOT_COMPLETE);
        assert!(!sent, "a message past the deadline must not be sent");
        assert_eq!(
            probe.cut_short(),
            Some("reading AXRole on an element of an unknown application")
        );
    }

    #[test]
    fn an_element_gate_excludes_other_threads_and_is_released() {
        // An address no real element has; the gate never dereferences it.
        let element = 0x10usize as AXUIElementRef;
        let Ok(held) = ElementGateGuard::acquire(element, None) else {
            panic!("an idle gate is free");
        };
        let key = element as usize;
        let waited = std::thread::spawn(move || {
            ElementGateGuard::acquire(key as AXUIElementRef, Some(Duration::from_millis(50)))
                .is_ok()
        })
        .join()
        .expect("the waiting thread does not panic");
        assert!(!waited, "a held gate must refuse a bounded wait");
        let waiter = std::thread::spawn(move || {
            ElementGateGuard::acquire(key as AXUIElementRef, None).is_ok()
        });
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
        assert!(waiter.join().expect("the waiting thread does not panic"));
        assert!(
            !element_gates()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&key),
            "a gate nobody holds or waits on is removed"
        );
    }

    #[test]
    fn a_message_with_no_deadline_is_sent_unchanged() {
        let code = ax_bounded(std::ptr::null(), || unreachable!(), || -7);
        assert_eq!(code, -7);
    }

    #[test]
    fn a_deadline_cut_is_not_called_a_refusal() {
        let message = cannot_complete_message(
            "press: AXPress failed",
            "the application with pid 1",
            Duration::from_millis(300),
            CannotCompleteContext::Action,
            true,
        );
        assert!(
            message.contains("before the caller's deadline"),
            "{message}"
        );
        assert!(!message.contains("not a timeout"), "{message}");
    }

    #[test]
    fn timeout_threshold_is_ninety_percent_of_the_bound() {
        let bound = Duration::from_secs_f32(AX_MESSAGING_TIMEOUT_SECS);
        assert!(hit_messaging_timeout(bound));
        assert!(hit_messaging_timeout(bound.mul_f32(0.95)));
        assert!(!hit_messaging_timeout(bound.mul_f32(0.5)));
    }

    #[test]
    fn snapshot_context_names_the_pid_without_an_action_warning() {
        let app =
            AXElement::from_owned(unsafe { safe_ax_create_application(std::process::id() as i32) });
        let err = cannot_complete_error(
            app.as_ptr(),
            "probe",
            Duration::from_secs_f32(AX_MESSAGING_TIMEOUT_SECS),
            CannotCompleteContext::Snapshot,
        );
        let Error::Platform { message, .. } = err else {
            panic!("expected Platform");
        };
        assert!(
            message.contains(&format!("the application with pid {}", std::process::id())),
            "{message}"
        );
        assert!(!message.contains("taken effect"), "{message}");
    }

    #[test]
    fn screen_recording_gates_ax_from_macos_26_and_when_unknown() {
        assert!(!screen_recording_gates_ax(&Ok(15)));
        assert!(screen_recording_gates_ax(&Ok(26)));
        assert!(screen_recording_gates_ax(&Ok(27)));
        assert!(screen_recording_gates_ax(&Err("probe failed".to_string())));
    }

    #[test]
    fn screen_recording_instructions_name_the_version_and_the_restart() {
        let known = screen_recording_instructions(&Ok(26));
        assert!(known.contains("This is macOS 26"), "{known}");
        assert!(known.contains("restart this process"), "{known}");
        let unknown = screen_recording_instructions(&Err("sysctl said no".to_string()));
        assert!(unknown.contains("could not be detected"), "{unknown}");
        assert!(unknown.contains("sysctl said no"), "{unknown}");
        assert!(unknown.contains("restart this process"), "{unknown}");
    }

    #[test]
    fn the_os_product_version_is_read_in_process() {
        let version = read_os_product_version().expect("kern.osproductversion is readable");
        assert!(parse_major_version(&version).is_ok(), "{version:?}");
        assert!(macos_major_version().is_ok());
        assert_eq!(parse_major_version("15.4.1"), Ok(15));
        assert_eq!(parse_major_version("26\n"), Ok(26));
        assert!(parse_major_version("").is_err());
        assert!(parse_major_version("x.1").is_err());
    }

    #[test]
    fn screen_recording_preflight_is_callable() {
        // The answer depends on the host's grant; the call must not prompt,
        // throw, or crash. Whatever it says, it agrees with itself.
        let first = MacOSProvider::has_screen_recording_permission();
        assert_eq!(first, MacOSProvider::has_screen_recording_permission());
    }

    #[test]
    fn dedup_by_ax_identity_compares_objects_not_pointers() {
        // Two refs to one application are distinct pointers but the same
        // accessibility object; a ref to another process is not (issue #514).
        // AXUIElementCreateApplication sends no message, so this needs no
        // running app or permission.
        let me = std::process::id() as i32;
        let a = AXElement::from_owned(unsafe { safe_ax_create_application(me) });
        let b = AXElement::from_owned(unsafe { safe_ax_create_application(me) });
        let other = AXElement::from_owned(unsafe { safe_ax_create_application(1) });
        assert_ne!(a.as_ptr(), b.as_ptr(), "fixture needs two distinct refs");

        let items = vec![(0, a, "a"), (1, other, "other"), (2, b, "b")];
        assert_eq!(
            dedup_by_ax_identity(items.clone(), None),
            vec!["a", "other"]
        );
        assert_eq!(dedup_by_ax_identity(items, Some(1)), vec!["a"]);
    }

    #[test]
    fn safe_ax_set_messaging_timeout_is_callable_on_null_element() {
        // Exercises the wrapper: it must return an AXError rather than let an
        // ObjC exception unwind into Rust. The code Apple picks for a NULL
        // element is Apple's to choose, so only the call itself is asserted.
        let _err = unsafe { safe_ax_set_messaging_timeout(std::ptr::null(), 0.25_f32) };
    }

    #[test]
    fn ax_children_returns_empty_for_null_element() {
        let result = ax_children(std::ptr::null());
        assert!(result.is_empty());
    }

    #[test]
    fn ax_action_names_returns_empty_for_null_element() {
        let result = ax_action_names(std::ptr::null());
        assert!(result.is_empty());
    }

    #[test]
    fn try_ax_action_names_reports_a_failed_read() {
        // A null element is an AX error, not "no actions".
        assert!(matches!(
            try_ax_action_names(std::ptr::null()),
            Err(Error::Platform { .. })
        ));
    }

    #[test]
    fn container_selection_probe_gated_to_selectable_item_roles() {
        // Only roles that live inside AXSelectedChildren-style containers
        // may trigger the (IPC-costly) ancestor probe.
        for role in [Role::TableCell, Role::TableRow, Role::ListItem] {
            assert!(selection_can_come_from_container(role), "{role:?}");
        }
        for role in [
            Role::Button,
            Role::Table,
            Role::StaticText,
            Role::Window,
            Role::CheckBox,
        ] {
            assert!(!selection_can_come_from_container(role), "{role:?}");
        }
    }

    #[test]
    fn container_selection_contains_is_false_for_null_element() {
        // A null element has no parent chain; the probe must degrade to
        // "not selected" without touching AX APIs.
        assert!(!container_selection_contains(std::ptr::null(), 2));
    }

    #[test]
    fn ax_bool_returns_none_for_null_element() {
        let result = ax_bool(std::ptr::null(), "AXEnabled");
        assert!(result.is_none());
    }

    #[test]
    fn ax_position_returns_none_for_null_element() {
        let result = ax_position(std::ptr::null());
        assert!(result.is_none());
    }

    #[test]
    fn ax_size_returns_none_for_null_element() {
        let result = ax_size(std::ptr::null());
        assert!(result.is_none());
    }

    #[test]
    fn do_perform_action_returns_error_for_null_element() {
        let action = CFString::new("AXPress");
        let err = do_perform_action(std::ptr::null(), &action);
        assert_ne!(err, AX_ERROR_SUCCESS);
    }

    #[test]
    fn do_set_attribute_returns_error_for_null_element() {
        let attr = CFString::new("AXFocused");
        let val = core_foundation::boolean::CFBoolean::true_value();
        let err = do_set_attribute(std::ptr::null(), &attr, val.as_CFTypeRef());
        assert_ne!(err, AX_ERROR_SUCCESS);
    }

    /// A refused `AXManualAccessibility` names what the set and the read
    /// returned and the relaunch flag, and says why there is no
    /// `AXEnhancedUserInterface` fallback.
    #[test]
    fn web_accessibility_refusal_names_the_attempt_and_the_remedy() {
        let message = web_accessibility_refused(-25205, &SwitchReadBack::Absent);
        assert!(message.contains("AXManualAccessibility"), "{message}");
        assert!(message.contains("AXError -25205"), "{message}");
        assert!(message.contains("no such attribute"), "{message}");
        assert!(
            message.contains("--force-renderer-accessibility"),
            "{message}"
        );
        assert!(message.contains("AXEnhancedUserInterface"), "{message}");
    }

    /// Both spellings of a password field are flagged, and nothing else
    /// is (#522). The role itself stays `TextField`; see `map_ax_role`.
    #[test]
    fn secure_text_field_is_flagged_as_role_or_subrole() {
        assert!(is_secure_text_field("AXSecureTextField", None));
        assert!(is_secure_text_field(
            "AXTextField",
            Some("AXSecureTextField")
        ));
        assert!(!is_secure_text_field("AXTextField", None));
        assert!(!is_secure_text_field("AXTextField", Some("AXSearchField")));
        assert!(!is_secure_text_field("AXTextArea", None));
    }

    #[test]
    fn map_ax_role_covers_all_known_roles() {
        assert_eq!(map_ax_role("AXSortButton", None), Role::Button);
        assert_eq!(map_ax_role("AXHelpTag", None), Role::Tooltip);
        assert_eq!(map_ax_role("AXPopover", None), Role::Group);
        assert_eq!(
            map_ax_role("AXGroup", Some("AXLandmarkNavigation")),
            Role::Landmark
        );
        assert_eq!(map_ax_role("AXWindow", Some("AXDialog")), Role::Dialog);
        assert_eq!(
            map_ax_role("AXGroup", Some("AXApplicationAlert")),
            Role::Alert
        );
        assert_eq!(map_ax_role("AXGroup", Some("AXSystemAlert")), Role::Alert);
        assert_eq!(map_ax_role("AXButton", Some("AXTabButton")), Role::Tab);
        assert_eq!(map_ax_role("AXRow", Some("AXOutlineRow")), Role::TreeItem);
        assert_eq!(
            map_ax_role("AXStaticText", Some("AXHeading")),
            Role::Heading
        );
        assert_eq!(map_ax_role("AXApplication", None), Role::Application);
        assert_eq!(map_ax_role("AXWindow", None), Role::Window);
        assert_eq!(map_ax_role("AXSheet", None), Role::Dialog);
        assert_eq!(map_ax_role("AXDrawer", None), Role::Window);
        assert_eq!(map_ax_role("AXButton", None), Role::Button);
        assert_eq!(
            map_ax_role("AXButton", Some("AXDisclosureTriangle")),
            Role::TreeItem
        );
        assert_eq!(map_ax_role("AXRadioButton", None), Role::RadioButton);
        assert_eq!(map_ax_role("AXCheckBox", None), Role::CheckBox);
        assert_eq!(map_ax_role("AXTextField", None), Role::TextField);
        assert_eq!(map_ax_role("AXSecureTextField", None), Role::TextField);
        assert_eq!(
            map_ax_role("AXTextField", Some("AXSecureTextField")),
            Role::TextField
        );
        assert_eq!(map_ax_role("AXTextArea", None), Role::TextArea);
        assert_eq!(map_ax_role("AXStaticText", None), Role::StaticText);
        assert_eq!(map_ax_role("AXComboBox", None), Role::ComboBox);
        assert_eq!(map_ax_role("AXPopUpButton", None), Role::ComboBox);
        assert_eq!(map_ax_role("AXList", None), Role::List);
        assert_eq!(map_ax_role("AXTable", None), Role::Table);
        assert_eq!(map_ax_role("AXOutline", None), Role::List);
        assert_eq!(map_ax_role("AXRow", None), Role::TableRow);
        assert_eq!(map_ax_role("AXCell", None), Role::TableCell);
        assert_eq!(map_ax_role("AXMenu", None), Role::Menu);
        assert_eq!(map_ax_role("AXMenuItem", None), Role::MenuItem);
        assert_eq!(map_ax_role("AXMenuBarItem", None), Role::MenuItem);
        assert_eq!(map_ax_role("AXMenuBar", None), Role::MenuBar);
        assert_eq!(map_ax_role("AXMenuBarExtra", None), Role::MenuBar);
        assert_eq!(map_ax_role("AXTabGroup", None), Role::TabGroup);
        assert_eq!(map_ax_role("AXToolbar", None), Role::Toolbar);
        assert_eq!(map_ax_role("AXScrollBar", None), Role::ScrollBar);
        assert_eq!(map_ax_role("AXSlider", None), Role::Slider);
        assert_eq!(map_ax_role("AXImage", None), Role::Image);
        assert_eq!(map_ax_role("AXLink", None), Role::Link);
        assert_eq!(map_ax_role("AXGroup", None), Role::Group);
        assert_eq!(map_ax_role("AXScrollArea", None), Role::Group);
        assert_eq!(map_ax_role("AXLayoutArea", None), Role::Group);
        assert_eq!(map_ax_role("AXRadioGroup", None), Role::Group);
        assert_eq!(map_ax_role("AXDialog", None), Role::Dialog);
        assert_eq!(map_ax_role("AXProgressIndicator", None), Role::ProgressBar);
        assert_eq!(map_ax_role("AXBusyIndicator", None), Role::ProgressBar);
        assert_eq!(map_ax_role("AXLevelIndicator", None), Role::ProgressBar);
        assert_eq!(map_ax_role("AXDisclosureTriangle", None), Role::TreeItem);
        assert_eq!(map_ax_role("AXHeading", None), Role::Heading);
        assert_eq!(map_ax_role("Heading", None), Role::Heading);
        assert_eq!(map_ax_role("AXSplitGroup", None), Role::SplitGroup);
        assert_eq!(map_ax_role("AXSplitter", None), Role::Separator);
        assert_eq!(map_ax_role("AXWebArea", None), Role::WebArea);
        assert_eq!(map_ax_role("AXIncrementor", None), Role::SpinButton);
        assert_eq!(map_ax_role("AXColorWell", None), Role::Unknown);
        // AXGrid (NSGridView) maps to Table — it is a 2-D grid of cells
        assert_eq!(map_ax_role("AXGrid", None), Role::Table);
        // AXDockItem (macOS Dock icon) maps to Button — it is activatable
        assert_eq!(map_ax_role("AXDockItem", None), Role::Button);
        // AXGrowArea (window resize grip) maps to ScrollThumb — it is a draggable handle
        assert_eq!(map_ax_role("AXGrowArea", None), Role::ScrollThumb);
        assert_eq!(map_ax_role("TotallyUnknownRole", None), Role::Unknown);
        // PySide6/Qt exposes QComboBox as AXMenuButton on macOS
        assert_eq!(map_ax_role("AXMenuButton", None), Role::ComboBox);
        // AXBrowser (Finder column view) and AXColumn (table columns) map to Group
        assert_eq!(map_ax_role("AXBrowser", None), Role::Group);
        assert_eq!(map_ax_role("AXColumn", None), Role::Group);
        // AXValueIndicator is the scroll thumb inside a scroll bar
        assert_eq!(map_ax_role("AXValueIndicator", None), Role::ScrollThumb);
        // AXMenuButton with AXSegment subrole is a segmented control button
        assert_eq!(map_ax_role("AXMenuButton", Some("AXSegment")), Role::Button);
    }

    #[test]
    fn press_dispatches_to_axopen_only_when_it_is_the_activation_action() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(press_ax_action(&names(&["AXOpen"])), "AXOpen");
        assert_eq!(press_ax_action(&names(&["AXShowMenu", "AXOpen"])), "AXOpen");
        // AXPress wins whenever the element has it (issue #517 decision).
        assert_eq!(press_ax_action(&names(&["AXOpen", "AXPress"])), "AXPress");
        assert_eq!(press_ax_action(&names(&["AXPress"])), "AXPress");
        // AXConfirm alone is what `press` advertises, so it is what it sends.
        assert_eq!(press_ax_action(&names(&["AXConfirm"])), "AXConfirm");
        assert_eq!(
            press_ax_action(&names(&["AXConfirm", "AXPress"])),
            "AXPress"
        );
        // AXConfirm does not count as AXPress: an element with AXConfirm and
        // AXOpen but no AXPress opens.
        assert_eq!(press_ax_action(&names(&["AXConfirm", "AXOpen"])), "AXOpen");
        assert_eq!(press_ax_action(&[]), "AXPress");
    }

    #[test]
    fn ax_action_to_name_covers_known() {
        assert_eq!(ax_action_to_name("AXPress"), Some("press"));
        assert_eq!(ax_action_to_name("AXConfirm"), Some("press"));
        assert_eq!(ax_action_to_name("AXShowMenu"), Some("show_menu"));
        assert_eq!(ax_action_to_name("AXIncrement"), Some("increment"));
        assert_eq!(ax_action_to_name("AXDecrement"), Some("decrement"));
        assert_eq!(
            ax_action_to_name("AXScrollToVisible"),
            Some("scroll_into_view")
        );
        assert_eq!(ax_action_to_name("AXRaise"), Some("activate"));
    }

    #[test]
    fn ax_action_to_name_returns_none_for_unknown() {
        // Unknown AX actions get converted via ax_pascal_to_snake instead
        assert_eq!(ax_action_to_name("AXCancel"), None);
        assert_eq!(ax_action_to_name("AXCustomThing"), None);
        assert_eq!(ax_action_to_name("UnknownAction"), None);
    }

    // ── Name conversion tests ───────────────────────────────────────

    #[test]
    fn ax_pascal_to_snake_basic() {
        assert_eq!(ax_pascal_to_snake("AXPress"), "press");
        assert_eq!(ax_pascal_to_snake("AXShowMenu"), "show_menu");
        assert_eq!(ax_pascal_to_snake("AXCustomThing"), "custom_thing");
        assert_eq!(ax_pascal_to_snake("AXIncrement"), "increment");
    }

    #[test]
    fn ax_pascal_to_snake_no_prefix() {
        assert_eq!(ax_pascal_to_snake("NoPrefix"), "no_prefix");
    }

    #[test]
    fn snake_to_ax_pascal_basic() {
        assert_eq!(snake_to_ax_pascal("press"), "AXPress");
        assert_eq!(snake_to_ax_pascal("show_menu"), "AXShowMenu");
        assert_eq!(snake_to_ax_pascal("custom_thing"), "AXCustomThing");
        assert_eq!(snake_to_ax_pascal("increment"), "AXIncrement");
    }

    #[test]
    fn name_conversion_roundtrips() {
        let names = ["custom_thing", "my_action", "foo_bar_baz", "press"];
        for name in names {
            let ax = snake_to_ax_pascal(name);
            let back = ax_pascal_to_snake(&ax);
            assert_eq!(name, back, "round-trip failed: {name} → {ax} → {back}");
        }
    }

    #[test]
    fn provider_new_succeeds() {
        let provider = MacOSProvider::new();
        assert!(provider.is_ok());
    }

    #[test]
    fn batch_attrs_returns_none_for_null_element() {
        let result = BatchAttrs::fetch(std::ptr::null());
        assert!(result.is_none());
    }

    #[test]
    fn batch_attrs_string_returns_none_for_empty() {
        // Construct a BatchAttrs with all-null values to test accessors.
        let batch = BatchAttrs {
            _values_array: std::ptr::null(),
            vals: [std::ptr::null(); attr_idx::COUNT],
        };
        assert!(batch.string(attr_idx::ROLE).is_none());
        assert!(batch.string(attr_idx::TITLE).is_none());
        assert!(batch.boolean(attr_idx::ENABLED).is_none());
        assert!(batch.value_string().is_none());
        assert!(batch.value_number().is_none());
        assert!(batch.value_int().is_none());
        assert!(batch.position().is_none());
        assert!(batch.size().is_none());
        // Don't drop — _values_array is null so Drop is a no-op.
    }

    #[test]
    fn from_batch_requires_a_readable_role() {
        // A role slot holding anything but a string (the batch call's
        // per-attribute error object, kCFNull, ...) must not become an
        // empty role: `from_batch` declines, and the caller re-reads the
        // element individually (issue #513).
        let role = CFString::new("AXButton");
        let not_a_string = core_foundation::boolean::CFBoolean::true_value();

        let mut vals = [std::ptr::null(); attr_idx::COUNT];
        let missing = BatchAttrs {
            _values_array: std::ptr::null(),
            vals,
        };
        assert!(ResolvedAttrs::from_batch(&missing).is_none());

        vals[attr_idx::ROLE] = not_a_string.as_CFTypeRef();
        let malformed = BatchAttrs {
            _values_array: std::ptr::null(),
            vals,
        };
        assert!(ResolvedAttrs::from_batch(&malformed).is_none());

        vals[attr_idx::ROLE] = role.as_concrete_TypeRef() as CFTypeRef;
        let readable = BatchAttrs {
            _values_array: std::ptr::null(),
            vals,
        };
        let attrs = ResolvedAttrs::from_batch(&readable).expect("string role is readable");
        assert_eq!(attrs.role_str, "AXButton");
        // The borrowed values outlive the batches: `role` and `not_a_string`
        // drop after them, and each `_values_array` is null so Drop is a no-op.
    }

    #[test]
    fn matches_ax_returns_false_for_null_element() {
        use xa11y_core::selector::{RoleMatch, SimpleSelector};
        // Role-only selector should not match a null element
        let simple = SimpleSelector {
            role: Some(RoleMatch::Normalized(Role::Button)),
            filters: vec![],
            nth: None,
        };
        assert!(!matches_ax(std::ptr::null(), &simple));
    }

    #[test]
    fn matches_ax_matches_role_only() {
        use xa11y_core::selector::SimpleSelector;
        // No role constraint — should match anything (even null, since no
        // attribute to check). But null has no role, so no-role selector matches.
        let simple = SimpleSelector {
            role: None,
            filters: vec![],
            nth: None,
        };
        // A null element can't report a role, but the selector has no role
        // constraint, so it should match.
        assert!(matches_ax(std::ptr::null(), &simple));
    }

    #[test]
    fn matches_ax_rejects_wrong_role() {
        use xa11y_core::selector::{RoleMatch, SimpleSelector};
        let simple = SimpleSelector {
            role: Some(RoleMatch::Normalized(Role::CheckBox)),
            filters: vec![],
            nth: None,
        };
        // Null element has no role — should not match CheckBox
        assert!(!matches_ax(std::ptr::null(), &simple));
    }

    #[test]
    fn matches_ax_with_name_filter_rejects_null() {
        use xa11y_core::selector::{AttrFilter, MatchOp, SimpleSelector};
        let simple = SimpleSelector {
            role: None,
            filters: vec![AttrFilter {
                attr: "name".to_string(),
                op: MatchOp::Exact,
                value: "Submit".to_string(),
            }],
            nth: None,
        };
        // Null element has no name — filter should fail
        assert!(!matches_ax(std::ptr::null(), &simple));
    }

    #[test]
    fn matches_ax_non_fast_path_attr_takes_full_fallback() {
        // Filters on normalized state attributes (`enabled`, `checked`,
        // `focused`, …) aren't in FAST_PATH_ATTRS. The fast path must bail
        // out to the full-snapshot matcher rather than silently returning
        // `None` from the filter-value switch (which would make every
        // element fail the filter and break these selectors entirely). For
        // a null AXUIElementRef the fallback should return `false` without
        // panicking.
        use xa11y_core::selector::{AttrFilter, MatchOp, SimpleSelector};
        for attr in ["enabled", "checked", "focused", "selected"] {
            let simple = SimpleSelector {
                role: None,
                filters: vec![AttrFilter {
                    attr: attr.to_string(),
                    op: MatchOp::Exact,
                    value: "true".to_string(),
                }],
                nth: None,
            };
            assert!(
                !matches_ax(std::ptr::null(), &simple),
                "non-fast-path attr `{attr}` should fall through to the full matcher \
                 without panicking, and a null element should not match"
            );
        }
    }

    // ════════════════════════════════════════════════════════════════
    // AX Call Count Regression Tests
    //
    // These tests assert exact AX IPC call counts for selector queries
    // against the running test app. Counts should ONLY GO DOWN as we
    // optimize — if you've improved performance, lower the expected
    // count. Never raise it on a one-off high: the bounds were last
    // re-measured on macOS 26.4 (Tart VM) and macOS 27.0 (physical), where
    // the walks cost 304 / 298 / 288 calls, stable across runs. A raise must
    // come with the same stable re-measurement and an explanation of what
    // grew (the current numbers reflect the test app's tree growth since the
    // bounds were first tuned).
    //
    // Requires: xa11y-test-app running with accessibility permissions.
    // Run via: cargo xtask test-integ (which also runs these)
    // ════════════════════════════════════════════════════════════════

    /// Find the test app's root ElementData via `list_apps` — same
    /// discovery path as `App::by_name`, which is known to work.
    fn find_test_app(provider: &MacOSProvider) -> ElementData {
        let apps = provider.list_apps().unwrap();
        apps.into_iter()
            .find(|d| d.name.as_deref() == Some("xa11y-test-app"))
            .expect("xa11y-test-app not found — is it running?")
    }

    /// Get the first window ElementData under the test app.
    fn find_test_window(provider: &MacOSProvider, app: &ElementData) -> ElementData {
        let children = provider.get_children(Some(app)).unwrap();
        children
            .into_iter()
            .find(|c| c.role == Role::Window)
            .expect("No window found under test app")
    }

    /// Poll `find_elements` until `selector` matches under `root` (or the
    /// deadline passes), so the measured query starts from a tree the app has
    /// finished publishing.
    ///
    /// The harness only sleeps a fixed two seconds after launching the test
    /// app; a cold launch can still be publishing its window when the first
    /// query lands, and the count test then fails with "no results" instead of
    /// a count. The probe runs before `ax_counters::reset_all()`, so it cannot
    /// affect the measured calls.
    fn wait_for_match(
        provider: &MacOSProvider,
        root: &ElementData,
        selector: &str,
        limit: Option<usize>,
    ) {
        let selector = Selector::parse(selector).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let found = provider
                .find_elements(root, &selector, limit, None)
                .unwrap();
            if !found.is_empty() || std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore]
    fn ax_calls_find_button_by_name() {
        // Searching for Button[name="Submit"] from the app root.
        // Lightweight matching checks only role+name per node; batch
        // fetch builds full ElementData only for the single match.
        let _lock = ax_counters::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let provider = MacOSProvider::new().unwrap();
        let app = find_test_app(&provider);
        wait_for_match(&provider, &app, "button[name=\"Submit\"]", Some(1));

        ax_counters::reset_all();
        let selector = Selector::parse("button[name=\"Submit\"]").unwrap();
        let results = provider
            .find_elements(&app, &selector, Some(1), None)
            .unwrap();
        let (copy_attr, copy_multi, copy_actions) = ax_counters::snapshot();
        let total = ax_counters::total();

        assert!(
            !results.is_empty(),
            "Should find Submit button. Got no results."
        );

        // Upper bound — this counts AX IPC calls. Reducing this number is good;
        // increasing it is a regression. Update the bound if a deliberate feature
        // addition raises it.
        //
        // Breakdown: lightweight DFS fetches AXRole+AXSubrole per node (~2 calls
        // each) plus AXTitle/AXValue for name matching. 1 batch + 1 action-names
        // call for the single match's full ElementData.
        // 304 measured on macOS 26.4/27.0 (stable; see the module note above).
        const MAX_CALLS: u64 = 304;
        assert!(
            total <= MAX_CALLS,
            "AX call count regression: button[name=\"Submit\"] from app root.\n\
             got {total}, expected <= {MAX_CALLS}\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}",
        );
    }

    #[test]
    #[ignore]
    fn ax_calls_find_button_by_name_from_window() {
        // Searching from window (one level deeper) should need fewer calls
        // since we skip the app→window traversal.
        let _lock = ax_counters::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let provider = MacOSProvider::new().unwrap();
        let app = find_test_app(&provider);
        let window = find_test_window(&provider, &app);
        wait_for_match(&provider, &window, "button[name=\"Submit\"]", Some(1));

        ax_counters::reset_all();
        let selector = Selector::parse("button[name=\"Submit\"]").unwrap();
        let results = provider
            .find_elements(&window, &selector, Some(1), None)
            .unwrap();
        let (copy_attr, copy_multi, copy_actions) = ax_counters::snapshot();
        let total = ax_counters::total();

        assert!(
            !results.is_empty(),
            "Should find Submit button from window."
        );

        // Upper bound — this counts AX IPC calls. Reducing this number is good;
        // increasing it is a regression. Update the bound if a deliberate feature
        // addition raises it.
        // 298 measured on macOS 26.4/27.0 (stable; see the module note above).
        const MAX_CALLS: u64 = 298;
        assert!(
            total <= MAX_CALLS,
            "AX call count regression: button[name=\"Submit\"] from window.\n\
             got {total}, expected <= {MAX_CALLS}\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}",
        );
    }

    #[test]
    #[ignore]
    fn ax_calls_find_by_role_only() {
        // Searching for all checkboxes — role-only selector means
        // lightweight matching only checks AXRole+AXSubrole per node.
        let _lock = ax_counters::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let provider = MacOSProvider::new().unwrap();
        let app = find_test_app(&provider);
        let window = find_test_window(&provider, &app);
        wait_for_match(&provider, &window, "check_box", None);

        ax_counters::reset_all();
        let selector = Selector::parse("check_box").unwrap();
        let results = provider
            .find_elements(&window, &selector, None, None)
            .unwrap();
        let (copy_attr, copy_multi, copy_actions) = ax_counters::snapshot();
        let total = ax_counters::total();

        assert!(!results.is_empty(), "Should find at least one checkbox.");

        // Upper bound — this counts AX IPC calls. Reducing this number is good;
        // increasing it is a regression. Update the bound if a deliberate feature
        // addition raises it.
        // 288 measured on macOS 26.4/27.0 (stable; see the module note above).
        const MAX_CALLS: u64 = 288;
        assert!(
            total <= MAX_CALLS,
            "AX call count regression: check_box from window.\n\
             got {total}, expected <= {MAX_CALLS}\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}",
        );
    }

    #[test]
    #[ignore]
    fn ax_calls_get_children_uses_batch() {
        // Getting children of the window should use batch fetch (1 IPC
        // per child for attributes) rather than individual calls.
        let _lock = ax_counters::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let provider = MacOSProvider::new().unwrap();
        let app = find_test_app(&provider);
        let window = find_test_window(&provider, &app);

        ax_counters::reset_all();
        let _children = provider.get_children(Some(&window)).unwrap();
        let (copy_attr, copy_multi, copy_actions) = ax_counters::snapshot();

        // Structural invariant: when batch fetch is in effect, copy_multi
        // (AXUIElementCopyMultipleAttributeValues calls) should equal
        // copy_actions (1 per child). This is not a count bound — it's
        // testing that we go through the batch code path rather than falling
        // back to per-attribute fetches. If batch were bypassed, copy_multi
        // would be ~0 and copy_attr would spike instead.
        assert_eq!(
            copy_multi, copy_actions,
            "Batch fetches should equal action-name fetches (1 per child).\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}",
        );
    }
}
