//! macOS AXUIElement-based accessibility provider.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use rayon::prelude::*;

#[cfg(test)]
use xa11y_core::Selector;
use xa11y_core::{
    CancelHandle, ElementData, ElementParts, Error, Event, EventKind, EventParts, EventReceiver,
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
    fn safe_ax_get_pid(element: AXUIElementRef, out_pid: *mut i32) -> i32;
    fn safe_ax_set_messaging_timeout(element: AXUIElementRef, timeout_seconds: f32) -> i32;
    fn safe_ax_value_get_value(value: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
    fn safe_cg_window_list_copy(option: u32, relative_to: u32) -> CFArrayRef;
    fn safe_ax_observer_create(
        pid: i32,
        callback: unsafe extern "C" fn(CFTypeRef, AXUIElementRef, CFTypeRef, *mut c_void),
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
    fn safe_cf_number_get_value(num: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
    fn safe_cf_dict_get_value(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn safe_cf_array_create(values: *const CFTypeRef, num_values: CFIndex) -> CFArrayRef;
    fn safe_cf_string_get_type_id() -> u64;
    fn safe_cf_number_get_type_id() -> u64;
    fn safe_cf_boolean_get_type_id() -> u64;
    fn safe_cf_array_get_type_id() -> u64;

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

// ── FFI Wrappers (instrumented in test builds) ──────────────────────────────

#[inline(always)]
fn ffi_copy_attribute_value(
    element: AXUIElementRef,
    attribute: CFStringRef,
    value: *mut CFTypeRef,
) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_ATTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { safe_ax_copy_attribute_value(element, attribute, value) }
}

#[inline(always)]
fn ffi_copy_multiple_attribute_values(
    element: AXUIElementRef,
    attributes: CFArrayRef,
    values: *mut CFArrayRef,
) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_MULTI_ATTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { safe_ax_copy_multiple_attribute_values(element, attributes, values) }
}

#[inline(always)]
fn ffi_copy_action_names(element: AXUIElementRef, names: *mut CFArrayRef) -> i32 {
    #[cfg(test)]
    ax_counters::COPY_ACTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { safe_ax_copy_action_names(element, names) }
}

// ── Attribute Helpers ─────────────────────────────────────────────────────────

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

fn ax_string(element: AXUIElementRef, attribute: &str) -> Option<String> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if safe_cf_get_type_id(value) == safe_cf_string_get_type_id() {
            let s = CFString::wrap_under_create_rule(value as *const _);
            Some(s.to_string())
        } else {
            safe_cf_release(value);
            None
        }
    }
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

fn ax_number_f64(element: AXUIElementRef, attribute: &str) -> Option<f64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        let type_id = safe_cf_get_type_id(value);
        // Each branch owns the CFRelease of `value` exactly once: the number
        // path releases explicitly after the copy; the string path transfers
        // ownership to `CFString::wrap_under_create_rule`, which releases on
        // drop; the fall-through releases before returning `None`. The
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
            // `wrap_under_create_rule` adopts the existing +1 retain; the
            // resulting `CFString` releases on drop.
            let s = CFString::wrap_under_create_rule(value as *const _);
            return s.to_string().trim().parse::<f64>().ok();
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

fn ax_children(element: AXUIElementRef) -> Vec<AXElement> {
    let value = match ax_attr(element, "AXChildren") {
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
    let attr = CFString::new("AXWindows");
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
                    return Err(Error::Platform {
                        code: -1,
                        message: "AXWindows returned a non-array value; the provider answered \
                                  malformed, not empty"
                            .to_string(),
                    });
                }
                let count = safe_cf_array_get_count(value);
                let mut windows = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let w = safe_cf_array_get_value(value, i);
                    if !w.is_null() {
                        windows.push(AXElement::from_borrowed(w));
                    }
                }
                safe_cf_release(value);
                Ok(windows)
            }
        }
        // The process answered and said it has no such attribute / no value:
        // an application without windows, honestly reported as an empty list.
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(vec![]),
        // Everything else — kAXErrorCannotComplete (the code a messaging
        // timeout surfaces as), kAXErrorInvalidUIElement, an ObjC exception
        // caught by the wrapper — means we did not get an answer.
        code => Err(Error::Platform {
            code: code as i64,
            message: format!("AXUIElementCopyAttributeValue(AXWindows) failed (AXError {code})"),
        }),
    }
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
            return Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "AXCloseButton read failed for a {} (AXError {code}); the close button \
                     is unknown, not absent",
                    role
                ),
            });
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
                                        // CFString compares to `&str`
                                        // without an owned allocation.
                                        CFString::wrap_under_create_rule(subrole as *const _)
                                            == "AXCloseButton"
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
                                    return Err(Error::Platform {
                                        code: code as i64,
                                        message: format!(
                                            "AXSubrole read failed while scanning for the \
                                             close button of a {} (AXError {code}); the \
                                             close button is unknown, not absent",
                                            role
                                        ),
                                    });
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
            RawAttr::Unanswered(code) => Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "AXChildren read failed while scanning for the close button of a {} \
                     (AXError {code}); the close button is unknown, not absent",
                    role
                ),
            }),
        },
    }
}

/// How a window's maximize/restore verbs are exposed on this macOS.
///
/// There is no accessibility attribute for the zoom **state**: the zoom
/// surface is the window's zoom button (`AXZoomButton`, the documented
/// convenience attribute, "required for all window elements that have a zoom
/// button"). The `AXZoomed` string is not that attribute — it matches the
/// AppKit *property* `NSWindow.isZoomed`, is not declared in the SDK headers,
/// and live windows answer `kAXErrorAttributeUnsupported` for it. The only
/// state a zoom press leaves behind is `AXFullScreen` (pressing the button on
/// TextEdit, Chrome, and Firefox sets `AXFullScreen=true` and ends the frame
/// at the display's bounds), so `restore` clears that.
enum WindowZoom {
    /// The window exposes a zoom button. maximize presses it — the original
    /// platform action, not a substitute (tenet 3).
    ZoomButton(AXElement),
    /// The window has no zoom button. The verbs are `ActionNotSupported`,
    /// never faked (tenet 2).
    None,
}

/// Resolve a window's maximize/restore capability, error-preserving: only a
/// genuinely absent zoom button reads as "no zoom"; a dead or wedged element
/// propagates (tenet 1).
fn window_zoom(element: AXUIElementRef, role: Role) -> Result<WindowZoom> {
    Ok(match find_zoom_button(element, role)? {
        Some(btn) => WindowZoom::ZoomButton(btn),
        None => WindowZoom::None,
    })
}

/// Resolve a window's zoom button: the `AXZoomButton` convenience attribute,
/// with a child scan by `AXZoomButton` subrole as the fallback for bridges
/// that expose the button only as a child — the same two lookup paths as
/// [`find_close_button`], and the same error discipline: only a genuinely
/// absent attribute/subrole means "no zoom button"; a dead or wedged element
/// propagates (tenet 1).
fn find_zoom_button(element: AXUIElementRef, role: Role) -> Result<Option<AXElement>> {
    let raw_attr_zoom = match read_raw_attr(element, "AXZoomButton") {
        RawAttr::Value(v) => {
            // Same guard as `find_close_button`: only a real AXUIElement
            // value (same type id as the window itself) is a button; anything
            // else — kCFNull, a malformed answer — is treated as absent.
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
            return Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "AXZoomButton read failed for a {} (AXError {code}); the zoom button \
                     is unknown, not absent",
                    role
                ),
            });
        }
    };
    match raw_attr_zoom {
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
                            let is_zoom_button = match read_raw_attr(child, "AXSubrole") {
                                RawAttr::Value(subrole) => {
                                    if safe_cf_get_type_id(subrole) == safe_cf_string_get_type_id()
                                    {
                                        CFString::wrap_under_create_rule(subrole as *const _)
                                            == "AXZoomButton"
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
                                    return Err(Error::Platform {
                                        code: code as i64,
                                        message: format!(
                                            "AXSubrole read failed while scanning for the \
                                             zoom button of a {} (AXError {code}); the zoom \
                                             button is unknown, not absent",
                                            role
                                        ),
                                    });
                                }
                            };
                            if is_zoom_button {
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
            RawAttr::Unanswered(code) => Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "AXChildren read failed while scanning for the zoom button of a {} \
                     (AXError {code}); the zoom button is unknown, not absent",
                    role
                ),
            }),
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

fn ax_action_names(element: AXUIElementRef) -> Vec<String> {
    let mut names: CFArrayRef = std::ptr::null();
    let err = ffi_copy_action_names(element, &mut names);
    if err != AX_ERROR_SUCCESS || names.is_null() {
        return vec![];
    }
    unsafe {
        let count = safe_cf_array_get_count(names);
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            let name = safe_cf_array_get_value(names, i);
            if !name.is_null() && safe_cf_get_type_id(name) == safe_cf_string_get_type_id() {
                let s = CFString::wrap_under_get_rule(name as *const _);
                result.push(s.to_string());
            }
        }
        safe_cf_release(names);
        result
    }
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
            let s = CFString::wrap_under_create_rule(value as *const _);
            return Some(s.to_string());
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
            let s = CFString::wrap_under_create_rule(value as *const _);
            return s.to_string().trim().parse::<f64>().ok();
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
    pub const COUNT: usize = 17;
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
                let s = CFString::wrap_under_get_rule(v as *const _);
                Some(s.to_string())
            } else {
                None
            }
        }
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

    /// Read AXValue as a string (handles CFString and CFNumber).
    fn value_string(&self) -> Option<String> {
        let v = self.vals[attr_idx::VALUE];
        if v.is_null() {
            return None;
        }
        unsafe {
            let tid = safe_cf_get_type_id(v);
            if tid == safe_cf_string_get_type_id() {
                let s = CFString::wrap_under_get_rule(v as *const _);
                return Some(s.to_string());
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
                let s = CFString::wrap_under_get_rule(v as *const _);
                return s.to_string().trim().parse::<f64>().ok();
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
}

impl ResolvedAttrs {
    /// Populate from a BatchAttrs (1 Mach IPC round-trip).
    fn from_batch(batch: &BatchAttrs) -> Self {
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

        Self {
            role_str: batch.string(attr_idx::ROLE).unwrap_or_default(),
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
        }
    }

    /// Populate from individual AX API calls (fallback path).
    fn from_individual(element: AXUIElementRef) -> Self {
        Self {
            role_str: ax_string(element, "AXRole").unwrap_or_default(),
            subrole_str: ax_string(element, "AXSubrole"),
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
        }
    }
}

// ── Safe FFI Wrappers ────────────────────────────────────────────────────────

fn do_perform_action(element: AXUIElementRef, action: &CFString) -> i32 {
    unsafe { safe_ax_perform_action(element, action.as_concrete_TypeRef() as CFTypeRef) }
}

fn do_set_attribute(element: AXUIElementRef, attribute: &CFString, value: CFTypeRef) -> i32 {
    unsafe {
        safe_ax_set_attribute_value(element, attribute.as_concrete_TypeRef() as CFTypeRef, value)
    }
}

/// Convert an AX error code from `do_perform_action` into an appropriate
/// `Error`.  Returns `ActionNotSupported` for -25206 (kAXErrorActionUnsupported)
/// so callers get a clear, structured error instead of a raw platform code.
fn action_error(err: i32, action: &str, role: Role, fallback_msg: &str) -> Error {
    if err == AX_ERROR_ACTION_UNSUPPORTED {
        Error::ActionNotSupported {
            action: action.to_string(),
            role,
        }
    } else {
        Error::Platform {
            code: err as i64,
            message: fallback_msg.to_string(),
        }
    }
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
    let err = unsafe {
        safe_ax_is_attribute_settable(
            el_ptr,
            attr.as_concrete_TypeRef() as CFTypeRef,
            &mut settable,
        )
    };
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
        code => Err(Error::Platform {
            code: code as i64,
            message: format!("IsAttributeSettable({attr_name}) failed"),
        }),
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
        return Err(Error::Platform {
            code: pid_err as i64,
            message: format!(
                "AXUIElementGetPid failed (AXError {pid_err}) while running {action} on a {role}"
            ),
        });
    }
    if pid == 0 {
        return Err(Error::Platform {
            code: -1,
            message: format!("AXUIElementGetPid returned pid 0 while running {action} on a {role}"),
        });
    }
    let app = AXElement::from_owned(unsafe { safe_ax_create_application(pid) });
    if app.is_null() {
        return Err(Error::Platform {
            code: -9999,
            message: format!("AXUIElementCreateApplication failed while running {action}"),
        });
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

/// Clear a boolean attribute when it currently reads `true`.
///
/// The deminiaturize half of `activate` / `maximize`: neither `AXRaise` nor
/// pressing the zoom button clears `AXMinimized`, so a minimized window must
/// have its minimized flag cleared first or the verb returns success while
/// the window stays in the Dock. Error-preserving: only a definitive
/// unsupported / no-value answer (`RawAttr::Absent`) means "not set"; a
/// failed read is a platform error (tenet 1), the same distinction
/// `restore()` makes.
fn clear_bool_attr_if_true(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    action: &str,
    role: Role,
) -> Result<bool> {
    let was_true = match read_raw_attr(el_ptr, attr_name) {
        RawAttr::Value(v) => {
            let is_boolean = unsafe { safe_cf_get_type_id(v) == safe_cf_boolean_get_type_id() };
            let b = is_boolean && unsafe { safe_cf_boolean_get_value(v) };
            unsafe { safe_cf_release(v) };
            b
        }
        RawAttr::Absent => false,
        RawAttr::Unanswered(code) => {
            return Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "{attr_name} read failed while running {action} on a {} (AXError {code}); \
                     the state is unknown, not false",
                    role
                ),
            });
        }
    };
    if was_true {
        set_bool_attr(el_ptr, attr_name, false, action, role)?;
    }
    Ok(was_true)
}

/// How long `maximize` / `restore` wait for the zoom/fullscreen transition —
/// the zoom-button press (or the `AXFullScreen` clear) starts — before giving
/// up, and the read cadence / consecutive-confirm count during the wait.
///
/// The press is asynchronous, and on macOS 26 a WebKit-backed window
/// *recreates* its window object during the transition (the title empties and
/// comes back, a transient shell window appears and disappears, `AXWindows`
/// momentarily returns nothing). Immediately after the press the window is
/// already at the display's bounds while `AXFullScreen` still reads the
/// *previous* state, and a window still inside the transition answers
/// `kAXErrorIllegalArgument` (-25200) to an `AXSize` set. `restore`'s
/// clear-if-true read during that stale window no-ops, so `restore` returns
/// success with the window still zoomed and the next `resize_to` fails — the
/// observed macOS-26 failure. These bounds cover the observed ~2s transition
/// (including the transient shell window) while keeping a wedged window from
/// hanging the verb for long.
const WINDOW_ZOOM_SETTLE_BUDGET: Duration = Duration::from_secs(5);
const WINDOW_ZOOM_SETTLE_INTERVAL: Duration = Duration::from_millis(50);
const WINDOW_ZOOM_SETTLE_SAMPLES: usize = 3;

/// Read a boolean AX attribute with the tenet-1 distinction: a failed read is
/// an error (the state is unknown, not false), an absent / unsupported
/// attribute is `Ok(None)` — the same distinction `clear_bool_attr_if_true`
/// draws, and what `read_fullscreen_state` is made of.
fn read_bool_attr(
    el_ptr: AXUIElementRef,
    attr_name: &str,
    action: &str,
    role: Role,
) -> Result<Option<bool>> {
    match read_raw_attr(el_ptr, attr_name) {
        RawAttr::Value(v) => {
            let is_boolean = unsafe { safe_cf_get_type_id(v) == safe_cf_boolean_get_type_id() };
            let b = is_boolean && unsafe { safe_cf_boolean_get_value(v) };
            unsafe { safe_cf_release(v) };
            Ok(Some(b))
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::Platform {
            code: code as i64,
            message: format!(
                "{attr_name} read failed while {action} was settling the window on a {role} \
                 (AXError {code}); the state is unknown, not false"
            ),
        }),
    }
}

/// Read the window's `AXFullScreen` state: `Ok(Some(true/false))` when the
/// attribute is readable, `Ok(None)` when the window has no fullscreen
/// surface (unsupported), `Err` when the read fails.
fn read_fullscreen_state(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<Option<bool>> {
    read_bool_attr(el_ptr, "AXFullScreen", action, role)
}

/// Read the window's `AXSize` as `(width, height)`; the same error
/// distinction as [`read_fullscreen_state`].
fn read_size_attr(el_ptr: AXUIElementRef, action: &str, role: Role) -> Result<Option<(f64, f64)>> {
    match read_raw_attr(el_ptr, "AXSize") {
        RawAttr::Value(v) => {
            let mut size = CGSize::default();
            let ok = unsafe {
                safe_ax_value_get_value(v, AX_VALUE_CGSIZE, &mut size as *mut _ as *mut c_void)
            };
            unsafe { safe_cf_release(v) };
            Ok(ok.then_some((size.width, size.height)))
        }
        RawAttr::Absent => Ok(None),
        RawAttr::Unanswered(code) => Err(Error::Platform {
            code: code as i64,
            message: format!(
                "AXSize read failed while {action} was settling the window on a {role} \
                 (AXError {code}); the state is unknown, not unset"
            ),
        }),
    }
}

/// The window data one settle sample is built from.
#[derive(Clone, PartialEq, Debug)]
struct WindowZoomSample {
    size: (f64, f64),
    fullscreen: bool,
    main: bool,
}

/// Snapshot the app's windows once, for one settle sample.
///
/// Returns `Ok(Some(..))` when every window's read succeeded, `Ok(None)` when
/// the app did not answer or some window's attribute read failed — the
/// transition is in flight, so the snapshot is not a reliable sample and the
/// caller keeps waiting (the failure surfaces at the budget deadline via the
/// tenet-1 error). Absent `AXFullScreen` reads as `false` (a window without a
/// fullscreen surface is by definition not fullscreen); an absent `AXSize` /
/// `AXMain` makes the sample unreadable.
fn app_window_zoom_snapshot(
    app: AXUIElementRef,
    action: &str,
    role: Role,
) -> Result<Option<Vec<WindowZoomSample>>> {
    // A transient `kAXErrorCannotComplete` from the app mid-transition is part
    // of the churn the caller wants to wait out, not a verdict: an unreadable
    // sample resets the settle streak and the deadline error governs.
    let windows = match ax_windows(app) {
        Ok(w) => w,
        Err(_) => return Ok(None),
    };
    let mut samples = Vec::with_capacity(windows.len());
    for window in &windows {
        let fullscreen = match read_fullscreen_state(window.as_ptr(), action, role) {
            Ok(v) => v.unwrap_or(false),
            Err(_) => return Ok(None),
        };
        let size = match read_size_attr(window.as_ptr(), action, role) {
            Ok(v) => match v {
                Some(s) => s,
                None => return Ok(None),
            },
            Err(_) => return Ok(None),
        };
        let main = match read_bool_attr(window.as_ptr(), "AXMain", action, role) {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => return Ok(None),
        };
        samples.push(WindowZoomSample {
            size,
            fullscreen,
            main,
        });
    }
    // Round the sizes so a half-point wobble during a resize does not count as
    // instability. The window's *geometry* is what changes on zoom; sub-pixel
    // jitter is not.
    for sample in &mut samples {
        sample.size = (sample.size.0.round(), sample.size.1.round());
    }
    Ok(Some(samples))
}

/// Wait for the zoom/fullscreen transition to commit or clear.
///
/// `require` is the state the verb promises: `true` after `maximize`'s press
/// (the zoomed state committed), `false` after `restore`'s `AXFullScreen`
/// clear (the window actually un-zoomed). The wait succeeds once the app's
/// window set is stable for [`WINDOW_ZOOM_SETTLE_SAMPLES`] consecutive
/// samples and satisfies the promise. The wait is application-level, not
/// element-level, on purpose: the transition *recreates* the window object
/// (see [`WINDOW_ZOOM_SETTLE_BUDGET`]), so a cached element handle keeps
/// reading stale state while the app itself has already settled.
///
/// Timeout semantics differ per promise (tenet 1: no silent fallback, but no
/// false failure either):
/// - `require=true`: for the whole budget no window ever reported
///   `AXFullScreen=true`. That is the documented "app's zoom has no
///   `AXFullScreen` readout" case — the press was still the platform's own
///   action, so `maximize` reports it as performed rather than failing on an
///   unobservable state.
/// - `require=false`: `restore` promised a restored window; returning success
///   while the window is still zoomed would hide a broken restore and make
///   the next `AXSize` set fail elsewhere, so budget expiry is a platform
///   error.
fn wait_window_zoom_settled(
    el_ptr: AXUIElementRef,
    require: bool,
    action: &str,
    role: Role,
) -> Result<()> {
    let app = owning_app_element(el_ptr, action, role)?;
    let deadline = Instant::now() + WINDOW_ZOOM_SETTLE_BUDGET;
    let mut previous: Option<Vec<WindowZoomSample>> = None;
    let mut streak = 0usize;
    loop {
        match app_window_zoom_snapshot(app.as_ptr(), action, role)? {
            Some(set) => {
                let reached = if require {
                    set.iter().any(|w| w.fullscreen)
                } else {
                    !set.iter().any(|w| w.fullscreen)
                };
                let stable = previous.as_deref() == Some(set.as_slice());
                if reached && stable {
                    streak += 1;
                    if streak >= WINDOW_ZOOM_SETTLE_SAMPLES {
                        return Ok(());
                    }
                } else {
                    streak = 0;
                }
                previous = Some(set);
            }
            // An unreadable window is the transition churn, not a settled
            // state; note it so a *stable* all-false set after it can never
            // be mistaken for persistence.
            None => {
                streak = 0;
                previous = None;
            }
        }
        if Instant::now() >= deadline {
            if require {
                return Ok(());
            }
            return Err(Error::Platform {
                code: -9998,
                message: format!(
                    "restore did not settle within {:.0}s on a {role}: the window set never \
                     reported a stable non-fullscreen state after the clear, so the window is \
                     still zoomed",
                    WINDOW_ZOOM_SETTLE_BUDGET.as_secs(),
                ),
            });
        }
        std::thread::sleep(WINDOW_ZOOM_SETTLE_INTERVAL);
    }
}

// ── Role Mapping ──────────────────────────────────────────────────────────────

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
        Some("AXLandmarkNavigation") => return Role::Navigation,
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
        _ => None,
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
    matches_ax_with_role(ax, simple, None)
}

/// Attributes the lightweight `matches_ax_with_role` fast path knows how to
/// resolve without building a full `ElementData`. Any selector filter whose
/// attr is *not* in this list forces a fall-through to `build_snapshot_data`
/// combined with `xa11y_core::selector::matches_simple`, so normalized state
/// attrs (`enabled`, `checked`, `focused`, …) and raw platform-attr map keys
/// still match correctly.
const FAST_PATH_ATTRS: &[&str] = &["role", "name", "value", "description"];

/// Like `matches_ax` but accepts a pre-resolved role to avoid redundant
/// AX API calls when the caller already fetched the role.
fn matches_ax_with_role(
    ax: AXUIElementRef,
    simple: &SimpleSelector,
    precomputed_role: Option<Role>,
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
        // the match set is assembled. A snapshot that cannot be built is a
        // candidate that does not match (the selector engines treat an
        // unreadable node as absent, not as a hard failure).
        let data = match build_snapshot_data(ax, None, 0) {
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
                let ax_title = ax_string(ax, "AXTitle");
                ax_title.or_else(|| {
                    if role == Some(Role::StaticText) {
                        ax_value_string(ax)
                    } else {
                        ax_string(ax, "AXDescription")
                    }
                })
            }
            "value" => ax_value_string(ax),
            "description" => {
                // Mirror build_element_data description logic.
                let ax_title = ax_string(ax, "AXTitle");
                let ax_description = ax_string(ax, "AXDescription");
                let name = ax_title.or_else(|| {
                    if role == Some(Role::StaticText) {
                        ax_value_string(ax)
                    } else {
                        ax_description.clone()
                    }
                });
                ax_string(ax, "AXHelp").or_else(|| {
                    if name.as_ref() != ax_description.as_ref() {
                        ax_description
                    } else {
                        None
                    }
                })
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
    instructions_for_major(macos_major_version())
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

/// macOS major version from `kern.osproductversion`, cached after the first
/// read. On a probe failure it reports 0, which yields the pre-27 section
/// name: the probe is a local `sysctl` and should not fail, and a wrong
/// section name is only a hint, but this keeps the behavior stable.
fn macos_major_version() -> u32 {
    static VERSION: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        std::process::Command::new("sysctl")
            .args(["-n", "kern.osproductversion"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .split('.')
                    .next()
                    .and_then(|major| major.parse().ok())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    });
    *VERSION
}

/// Global handle counter for mapping ElementData back to AXElements.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub struct MacOSProvider {
    /// Cached AXElement refs keyed by handle ID.
    handle_cache: Mutex<HashMap<u64, AXElement>>,
}

/// Holds a shell-probe messaging-timeout bound, releasing it on drop.
///
/// The bound is taken on an **application** element, and Apple documents
/// `AXUIElementSetMessagingTimeout` on an application element as applying to
/// every message sent to that application — not to that element alone. The
/// scan therefore cannot leave one in place: the frontmost app, each
/// status-item host and Finder would stay pinned at a quarter-second timeout
/// for the life of the process, and a caller who then walked the menu bar or
/// the desktop would see spurious `kAXErrorCannotComplete`.
///
/// A guard rather than a call at the end of each probe, because every one of
/// those probes can leave early — `?` on a missing child, an unanswered
/// attribute — and a release that is skipped on the failure path is the case
/// that matters.
struct ShellProbeBound<'a> {
    element: &'a AXElement,
}

impl Drop for ShellProbeBound<'_> {
    fn drop(&mut self) {
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
        if !Self::has_screen_recording_permission() {
            return Err(Error::PermissionDenied {
                instructions:
                    "Enable Screen Recording in System Settings → Privacy & Security → \
                     Screen & System Audio Recording.\n\
                     On macOS 26+, this is required to read window content via the accessibility API."
                        .to_string(),
            });
        }

        Ok(Self {
            handle_cache: Mutex::new(HashMap::new()),
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
                    CFString::wrap_under_get_rule(name_val as *const _).to_string()
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

    /// Check if Screen Recording permission is granted by inspecting
    /// CGWindowListCopyWindowInfo. Without this permission, the list
    /// contains only system chrome (layer != 0). With it, app windows
    /// (layer 0) are included.
    pub(crate) fn has_screen_recording_permission() -> bool {
        let info = unsafe { safe_cg_window_list_copy(0, 0) };
        if info.is_null() {
            return false;
        }
        let layer_key = CFString::new("kCGWindowLayer");
        let mut has_app_window = false;
        unsafe {
            let count = safe_cf_array_get_count(info);
            for i in 0..count {
                let dict = safe_cf_array_get_value(info, i);
                if dict.is_null() {
                    continue;
                }
                let layer_val =
                    safe_cf_dict_get_value(dict, layer_key.as_concrete_TypeRef() as CFTypeRef);
                if !layer_val.is_null()
                    && safe_cf_get_type_id(layer_val) == safe_cf_number_get_type_id()
                {
                    let mut layer: i32 = -1;
                    safe_cf_number_get_value(
                        layer_val,
                        CF_NUMBER_SINT32,
                        &mut layer as *mut _ as *mut c_void,
                    );
                    if layer == 0 {
                        has_app_window = true;
                        break;
                    }
                }
            }
            safe_cf_release(info);
        }
        has_app_window
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
            .ok_or(Error::ElementStale {
                selector: format!("handle:{}", handle),
            })
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
            stable_id: None,
            pid,
            raw: std::collections::HashMap::new(),
            handle,
        }
        .into());
    }

    let body = move || -> Result<ElementData> {
        let attrs = if let Some(batch) = BatchAttrs::fetch(element) {
            ResolvedAttrs::from_batch(&batch)
        } else {
            ResolvedAttrs::from_individual(element)
        };

        let role = map_ax_role(&attrs.role_str, attrs.subrole_str.as_deref());

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

        // Name: prefer AXTitle, fall back to AXDescription only if no title
        let name = attrs.ax_title.or_else(|| {
            if role == Role::StaticText {
                attrs.value_string.clone()
            } else {
                attrs.ax_description.clone()
            }
        });

        // Description: AXHelp first, then AXDescription (if not already used as name)
        let description = attrs.ax_help.or_else(|| {
            if name.as_ref() != attrs.ax_description.as_ref() {
                attrs.ax_description
            } else {
                None
            }
        });

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

        // `AXMain` marks the app's main (active) window. Only window-like
        // elements (Window / Dialog / Sheet — the latter maps to `Role::Dialog`)
        // carry it, so gate on role to avoid an extra AX IPC round-trip for
        // every non-window element. `ax_bool` routes through the exception-safe
        // wrappers and owns its CFRelease; a missing / error / non-boolean
        // attribute yields `false`, matching how the other state reads degrade.
        let active = matches!(role, Role::Window | Role::Dialog)
            && ax_bool(element, "AXMain").unwrap_or(false);

        let states: StateSet = StateParts {
            enabled: attrs.enabled.unwrap_or(true),
            visible: !attrs.hidden.unwrap_or(false),
            focused: attrs.focused.unwrap_or(false),
            active,
            focusable,
            modal: attrs.modal.unwrap_or(false),
            minimized: if matches!(role, Role::Window | Role::Dialog) {
                attrs.minimized
            } else {
                None
            },
            // No AX attribute reports the zoom state (see `WindowZoom`): the
            // zoomed state surfaces as `fullscreen` (from AXFullScreen).
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
            // an AX FFI round-trip, and the same results feed both the verb
            // advertisement and the verb implementations below. Zoom is
            // button-based (there is no zoom-state attribute; see
            // `WindowZoom`), and the zoom press's only observable state is
            // `AXFullScreen`, so restore is additionally available when
            // `AXFullScreen` can be cleared.
            let minimized_settable = is_attr_settable(element, "AXMinimized")?;
            let zoom = window_zoom(element, role)?;
            let fullscreen_restorable = is_attr_settable(element, "AXFullScreen")?;
            if minimized_settable {
                push(&mut actions, "minimize");
            }
            if !matches!(zoom, WindowZoom::None) {
                push(&mut actions, "maximize");
            }
            if minimized_settable || !matches!(zoom, WindowZoom::None) || fullscreen_restorable {
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
            .map(|child| {
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
                    if matches_ax_with_role(child.as_ptr(), simple, Some(child_role)) {
                        child_results.push((idx, child.clone()));
                    }
                }

                // Recurse into subtree.
                let child_name = ax_string(child.as_ptr(), "AXTitle");
                let sub = self.collect_matching_ax_group(
                    child,
                    child_role,
                    child_name.as_deref(),
                    clauses,
                    depth + 1,
                    max_depth,
                    limit,
                );
                child_results.extend(sub);

                child_results
            })
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
    /// The AX default is ~1.5s *per attribute*, so a handful of wedged
    /// processes otherwise dominate the whole scan (PROPOSAL §4, measured).
    /// `AXUIElementSetMessagingTimeout` applies to the element it is set on
    /// and to nothing else from the same process — so it is set on the very
    /// element each probe then reads.
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
    /// scan's cue to skip the element rather than read it at the ~1.5s
    /// default: an element whose cost cannot be bounded is not one this scan
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
        Self::bound_shell_probe(element).then(|| ShellProbeBound { element })
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
        let system_wide = AXElement::from_owned(unsafe { safe_ax_create_system_wide() });
        if system_wide.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "AXUIElementCreateSystemWide returned NULL".to_string(),
            });
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
            // The same codes `app_by_pid` reads as "not reachable right now":
            // the app went away between the focused-app read and this one, its
            // accessibility bridge is not answering, or it did not answer
            // inside the bounded probe. For a live listing that is "no
            // menu_bar surface", not a failure of the listing.
            ElementProbe::Unanswered(
                AX_ERROR_CANNOT_COMPLETE | AX_ERROR_INVALID_UI_ELEMENT | AX_ERROR_NOT_IMPLEMENTED,
            ) => Ok(None),
            // Anything else is a genuine platform failure and propagates with
            // the AXError code that produced it (tenet 1, tenet 6).
            ElementProbe::Unanswered(code) => Err(Error::Platform {
                code: code as i64,
                message: format!(
                    "AXUIElementCopyAttributeValue(AXMenuBar) failed on the frontmost \
                     application{}",
                    app_pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                ),
            }),
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
            .filter_map(|(pid, app_name)| {
                let app_element =
                    AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
                if app_element.is_null() {
                    return None;
                }

                // Bound the probe *before* making it. If the bound cannot be
                // established the element is not one we can query within the
                // scan's cost contract, so it is skipped rather than probed
                // at the ~1.5s default.
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
            })
            .collect();
        // Deterministic order: the fan-out is parallel, so the pid sort is
        // what makes repeated listings comparable.
        surfaces.sort_by_key(|(_, data)| data.pid);
        surfaces
    }

    /// Native menus currently shown by status-item surfaces, as transient
    /// `Flyout` roots.
    ///
    /// AppKit does not put an open status-item menu in `AXChildren`, which is
    /// why walking either the owning app or its `AXExtrasMenuBar` cannot find
    /// it. The accessibility API exposes that detached menu through AppKit's
    /// `AXShownMenu` or Carbon's `AXShownMenuUIElement` on the object providing
    /// it. Probe the owning application, extras bar, and each direct
    /// status-item child because those provider shapes vary across AppKit and
    /// Carbon implementations.
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
                let mut shown_menus = Vec::new();
                for attribute in ["AXShownMenu", "AXShownMenuUIElement"] {
                    let Some(_bound) = Self::shell_probe_bound(&provider) else {
                        continue;
                    };
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

                    let mut data = self.build_element_data(&shown_menu, status_data.pid);
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
                let role = element_data.role;
                let name = element_data.name.as_deref();

                // An Application element's children are its top-level windows:
                // `AXWindows` is the single canonical window-discovery source
                // (the same answer `App::windows` now reads on every platform).
                // Every other role keeps the raw `AXChildren` walk. The read is
                // error-preserving on purpose — a dead app must report its
                // failure, not an empty window list (tenet 1).
                let ax_children_list = match role {
                    Role::Application => ax_windows(ax.as_ptr())?,
                    _ => ax_children(ax.as_ptr()),
                };

                // Filter first (cheap string checks), then build ElementData
                // in parallel (each build_element_data is an IPC round-trip).
                let filtered: Vec<&AXElement> = ax_children_list
                    .iter()
                    .filter(|child| !Self::should_filter_child(role, name, child))
                    .collect();

                let results: Vec<ElementData> = filtered
                    .par_iter()
                    .map(|child| self.build_element_data(child, element_data.pid))
                    .collect::<Result<Vec<_>>>()?;

                Ok(results)
            }
        }
    }

    fn find_elements_group(
        &self,
        root: &ElementData,
        group: &xa11y_core::selector::SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        if group.clauses.is_empty() {
            return Ok(vec![]);
        }
        // Reject any clause with zero segments early — `clause.segments[0]`
        // below would otherwise panic.
        if group.clauses.iter().any(|c| c.segments.is_empty()) {
            return Ok(vec![]);
        }

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

        let phase1: Vec<(usize, AXElement)> = self.collect_matching_ax_group(
            &root_ax,
            root_data.role,
            root_data.name.as_deref(),
            &firsts,
            0,
            max_depth_val,
            phase1_walk_limit,
        );

        // Bucket phase-1 hits by clause + their doc-order walk position.
        let mut by_clause: Vec<Vec<(usize, AXElement)>> =
            (0..group.clauses.len()).map(|_| Vec::new()).collect();
        for (walk_pos, (clause_idx, ax)) in phase1.into_iter().enumerate() {
            by_clause[clause_idx].push((walk_pos, ax));
        }

        let mut merged: Vec<(usize, AXUIElementRef, ElementData)> = Vec::new();
        for (clause_idx, hits) in by_clause.into_iter().enumerate() {
            if hits.is_empty() {
                continue;
            }
            let clause = &group.clauses[clause_idx];
            let first = &clause.segments[0].simple;

            // Build ElementData for this clause's phase-1 hits.
            let mut phase1_data: Vec<(usize, AXUIElementRef, ElementData)> = hits
                .iter()
                .map(|(pos, ax)| {
                    let data = self.build_element_data(ax, root_data.pid)?;
                    Ok((*pos, ax.as_ptr(), data))
                })
                .collect::<Result<Vec<_>>>()?;

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
            for (anchor_pos, _anchor_ptr, head) in phase1_data {
                let narrowed = self.narrow_multi_segment(
                    vec![head],
                    &clause.segments[1..],
                    max_depth_val,
                    None,
                )?;
                for n in narrowed {
                    // Anchor identity is for dedup vs other clauses' phase-1
                    // hits; for phase-2 outputs we use the narrowed element's
                    // own AXUIElement pointer (resolved via the cache).
                    let ptr = self
                        .get_cached(n.handle)
                        .map(|ax| ax.as_ptr())
                        .unwrap_or(std::ptr::null());
                    merged.push((anchor_pos, ptr, n));
                }
            }
        }

        // Stable sort by walk position keeps doc-order; dedup by
        // AXUIElement pointer identity ensures `X, X` collapses correctly.
        merged.sort_by_key(|(pos, _, _)| *pos);
        let mut seen: HashSet<usize> = HashSet::new();
        let mut out: Vec<ElementData> = Vec::with_capacity(merged.len());
        for (_, ptr, data) in merged {
            // Null pointers (resolution failures) can't be sensibly deduped;
            // treat each null as its own key so we keep the element.
            let key = ptr as usize;
            if key != 0 && !seen.insert(key) {
                continue;
            }
            out.push(data);
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let ax = self.get_cached(element.handle)?;
        match ax_parent(ax.as_ptr()) {
            Some(parent_ax) => {
                if parent_ax.is_null() {
                    return Ok(None);
                }
                // Check if parent is an application — if so, still return it
                let data = self.build_element_data(&parent_ax, element.pid)?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
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
            return Err(Error::Platform {
                code: -1,
                message: "CGWindowListCopyWindowInfo listed no processes; \
                          cannot enumerate shell surfaces"
                    .to_string(),
            });
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
        let mut results = Vec::new();
        for (pid, app_name) in &apps {
            let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
            if app_element.is_null() {
                continue;
            }
            let mut data = self.build_element_data(&app_element, Some(*pid as u32))?;
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
                let mut data = self.build_element_data(&app_element, Some(pid))?;
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
            AX_ERROR_CANNOT_COMPLETE
            | AX_ERROR_INVALID_UI_ELEMENT
            | AX_ERROR_NOT_IMPLEMENTED
            | AX_ERROR_NO_VALUE => Err(Error::selector_not_matched(format!(
                "application[pid={pid}]"
            ))
            .diagnose(xa11y_core::Diagnosis::new().last_observed(format!(
                "AX attach probe returned AXError {err}: process not yet AX-reachable, \
                 exited, or has no accessibility bridge"
            )))),
            _ => Err(Error::Platform {
                code: err as i64,
                message: format!(
                    "AXUIElementCopyAttributeValue(AXRole) failed while attaching to pid {pid}"
                ),
            }),
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
        let system_wide = AXElement::from_owned(unsafe { safe_ax_create_system_wide() });
        if system_wide.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "AXUIElementCreateSystemWide returned NULL".to_string(),
            });
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
        let ax = self.get_cached(element.handle)?;
        perform_ax_action(ax.as_ptr(), "AXPress", "press", element.role)
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

    fn scroll_into_view(&self, _element: &ElementData) -> Result<()> {
        // macOS has no accessibility API equivalent for scroll-into-view.
        Ok(())
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
        activate_owning_app(ax.as_ptr(), "activate", element.role)?;
        perform_ax_action(ax.as_ptr(), "AXRaise", "activate", element.role)
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // See expand(): unsettable AXMinimized sets would silently no-op in
        // every bridge — surface unsupported as ActionNotSupported instead.
        if !is_attr_settable(ax.as_ptr(), "AXMinimized")? {
            return Err(Error::ActionNotSupported {
                action: "minimize".to_string(),
                role: element.role,
            });
        }
        set_bool_attr(ax.as_ptr(), "AXMinimized", true, "minimize", element.role)
    }

    fn maximize(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Resolve the capability before touching the window: an unsupported
        // `maximize` must not deminiaturize and then report failure — no
        // partial state change on an action that is going to be refused.
        let zoom = window_zoom(ax.as_ptr(), element.role)?;
        // AXMinimized and the zoom button are independent: zooming a minimized
        // window would return success while the window stays in the Dock. The
        // window-state contract (see the shared mock) has maximize clear
        // `minimized` and bring the window back on-screen, so clear it first —
        // error-preserving, the same `activate()` handling (tenet 1).
        match zoom {
            WindowZoom::None => Err(Error::ActionNotSupported {
                action: "maximize".to_string(),
                role: element.role,
            }),
            WindowZoom::ZoomButton(btn) => {
                clear_bool_attr_if_true(ax.as_ptr(), "AXMinimized", "maximize", element.role)?;
                // Pressing the window's zoom button is the platform's own
                // maximization action, not a substitute (tenet 3) — the zoom
                // state has no AX attribute (see `WindowZoom`), so the button
                // is the surface. The state it sets is `AXFullScreen`, which
                // `restore()` reads back.
                perform_ax_action(btn.as_ptr(), "AXPress", "maximize", element.role)?;
                // The press is asynchronous: returning while the window is
                // mid-transition makes an immediate `restore()` read the stale
                // (previous) `AXFullScreen`, skip its clear, and report
                // success on a window that is still zoomed — which is how the
                // next resize answered kAXErrorIllegalArgument (-25200).
                // Wait for the zoomed state to commit.
                wait_window_zoom_settled(ax.as_ptr(), true, "maximize", element.role)
            }
        }
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        // Restore clears the minimized state and the zoomed state. A window
        // supports restore if either state is reachable: `AXMinimized`
        // settable, a zoom button, or a settable `AXFullScreen` — the state
        // the zoom button's press leaves behind. Each clear is attempted only
        // when applicable (a missing attribute would no-op).
        let min_settable = is_attr_settable(ax.as_ptr(), "AXMinimized")?;
        let zoom = window_zoom(ax.as_ptr(), element.role)?;
        let fullscreen_settable = is_attr_settable(ax.as_ptr(), "AXFullScreen")?;
        if !min_settable && matches!(zoom, WindowZoom::None) && !fullscreen_settable {
            return Err(Error::ActionNotSupported {
                action: "restore".to_string(),
                role: element.role,
            });
        }
        if min_settable {
            set_bool_attr(ax.as_ptr(), "AXMinimized", false, "restore", element.role)?;
        }
        // The zoomed state has no AX attribute (see `WindowZoom`); the only
        // observable leftover of the zoom button's press is AXFullScreen, and
        // restore leaves it. Misreading that as a no-op because the read
        // "safely" returned false would be a silent failure (tenet 1), so the
        // clear is error-preserving. Never press the zoom button here:
        // pressing toggles, so a window that is not zoomed would be zoomed by
        // its own restore. `AXFullScreen` is what a non-zoomed window reports
        // (false or absent), so a definitive read decides.
        if fullscreen_settable {
            clear_bool_attr_if_true(ax.as_ptr(), "AXFullScreen", "restore", element.role)?;
            // Clearing starts an asynchronous exit transition; wait until the
            // window actually reports un-zoomed, or fail (tenet 1) rather than
            // return success on a window that is still zoomed — which is what
            // used to leave the next AXSize set answering -25200.
            wait_window_zoom_settled(ax.as_ptr(), false, "restore", element.role)?;
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
        let value = unsafe { safe_ax_value_create_cg_point(f64::from(x), f64::from(y)) };
        if value.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "Failed to create CGPoint AXValue for window move".to_string(),
            });
        }
        let attr = CFString::new("AXPosition");
        let err = do_set_attribute(ax.as_ptr(), &attr, value);
        unsafe { safe_cf_release(value) };
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                err,
                "move_to",
                element.role,
                "Set AXPosition failed",
            ));
        }
        Ok(())
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        if !is_attr_settable(ax.as_ptr(), "AXSize")? {
            return Err(Error::ActionNotSupported {
                action: "resize_to".to_string(),
                role: element.role,
            });
        }
        let value = unsafe { safe_ax_value_create_cg_size(f64::from(width), f64::from(height)) };
        if value.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "Failed to create CGSize AXValue for window resize".to_string(),
            });
        }
        let attr = CFString::new("AXSize");
        let err = do_set_attribute(ax.as_ptr(), &attr, value);
        unsafe { safe_cf_release(value) };
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                err,
                "resize_to",
                element.role,
                "Set AXSize failed",
            ));
        }
        Ok(())
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
                err,
                "type_text",
                element.role,
                "Set AXSelectedText failed",
            ));
        }
        Ok(())
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let location = start as isize;
        let length = (end as isize) - (start as isize);
        let range_value = unsafe { safe_ax_value_create_cf_range(location, length) };
        if range_value.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "Failed to create CFRange value for text selection".to_string(),
            });
        }
        let attr = CFString::new("AXSelectedTextRange");
        let err = do_set_attribute(ax.as_ptr(), &attr, range_value);
        unsafe { safe_cf_release(range_value) };
        if err != AX_ERROR_SUCCESS {
            return Err(action_error(
                err,
                "set_text_selection",
                element.role,
                "Set AXSelectedTextRange failed",
            ));
        }
        Ok(())
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
            "restore" => self.restore(element),
            "close" => self.close(element),
            // Payload verbs have no arguments on the generic escape hatch;
            // fail surfaceably with how to call them instead of guessing
            // (tenet 1: no silent fallback).
            "move_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"move_to\") requires coordinates; call move_to(x, y)"
                    .to_string(),
            }),
            "resize_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"resize_to\") requires dimensions; call \
                           resize_to(width, height)"
                    .to_string(),
            }),
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
                        return Err(action_error(err, action, element.role, &ax_name));
                    }
                    return Ok(());
                }

                // Strategy 2: literal name
                if available.iter().any(|a| a == action) {
                    let cf_action = CFString::new(action);
                    let err = do_perform_action(el_ptr, &cf_action);
                    if err != AX_ERROR_SUCCESS {
                        return Err(action_error(err, action, element.role, action));
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
        let pid = element.pid.ok_or(Error::Platform {
            code: -1,
            message: "Element has no PID for subscribe".to_string(),
        })?;
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(pid as i32, app_name)
    }
}

// ── Event subscription ──────────────────────────────────────────────────────

struct ObserverContext {
    sender: std::sync::mpsc::Sender<Event>,
    app_name: String,
    app_pid: u32,
}

unsafe extern "C" fn ax_observer_callback(
    _observer: CFTypeRef,
    element: AXUIElementRef,
    notification: CFTypeRef,
    refcon: *mut c_void,
) {
    let ctx = &*(refcon as *const ObserverContext);

    let notif_str = {
        let cf = CFString::wrap_under_get_rule(notification as *const _);
        cf.to_string()
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

        "AXSelectedTextChanged"
        | "AXSelectedRowsChanged"
        | "AXSelectedCellsChanged"
        | "AXSelectedChildrenChanged" => vec![EventKind::SelectionChanged],

        "AXMenuOpened" => vec![EventKind::MenuOpened],
        "AXMenuClosed" => vec![EventKind::MenuClosed],

        "AXAnnouncementRequested" => vec![EventKind::Announcement],

        _ => return,
    };

    for kind in kinds {
        let event: Event = EventParts {
            kind,
            target: target.clone(),
            app_name: ctx.app_name.clone(),
            app_pid: ctx.app_pid,
            timestamp: std::time::Instant::now(),
        }
        .into();
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

impl MacOSProvider {
    fn subscribe_impl(&self, pid: i32, app_name: String) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel();

        let ctx = Box::new(ObserverContext {
            sender: tx,
            app_name,
            app_pid: pid as u32,
        });
        let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

        let mut observer: CFTypeRef = std::ptr::null();
        let err = unsafe { safe_ax_observer_create(pid, ax_observer_callback, &mut observer) };
        if err != AX_ERROR_SUCCESS || observer.is_null() {
            unsafe { drop(Box::from_raw(ctx_ptr as *mut ObserverContext)) };
            return Err(Error::Platform {
                code: err as i64,
                message: "AXObserverCreate failed".to_string(),
            });
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
                    return Err(Error::Platform {
                        code: err as i64,
                        message: format!("AXObserverAddNotification({notif}) failed"),
                    });
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
                let rl = safe_cf_run_loop_get_current();
                let _ = rl_tx.send(rl as usize);
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
                return Err(Error::Platform {
                    code: -1,
                    message: "Failed to start observer RunLoop".to_string(),
                });
            }
        };

        let ctx_usize = ctx_ptr as usize;

        let cancel = CancelHandle::new(move || {
            unsafe {
                safe_cf_run_loop_stop(run_loop_usize as CFTypeRef);
            }
            let _ = handle.join();
            unsafe {
                drop(Box::from_raw(ctx_usize as *mut ObserverContext));
                safe_cf_release(observer_usize as CFTypeRef);
            }
        });

        Ok(Subscription::new(EventReceiver::new(rx), cancel))
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
    fn find_zoom_button_propagates_read_errors_for_null_element() {
        // Same discipline as the close button: a null element answers with an
        // error, so `maximize` advertisement and `maximize()` must not read
        // "no zoom button" from a wedged element.
        let result = find_zoom_button(std::ptr::null(), Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
    }

    #[test]
    fn window_zoom_propagates_read_errors_for_null_element() {
        // `window_zoom`'s first probe (`AXIsAttributeSettable`) on a null
        // element errors rather than answering false: the capability must be
        // unknown, not "no maximize" — otherwise a transient failure would
        // silently drop the verb from `actions`.
        let result = window_zoom(std::ptr::null(), Role::Window);
        assert!(matches!(result, Err(Error::Platform { .. })));
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

    #[test]
    fn map_ax_role_covers_all_known_roles() {
        assert_eq!(map_ax_role("AXSortButton", None), Role::Button);
        assert_eq!(map_ax_role("AXHelpTag", None), Role::Tooltip);
        assert_eq!(map_ax_role("AXPopover", None), Role::Group);
        assert_eq!(
            map_ax_role("AXGroup", Some("AXLandmarkNavigation")),
            Role::Navigation
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
    fn ax_action_to_name_covers_known() {
        assert_eq!(ax_action_to_name("AXPress"), Some("press"));
        assert_eq!(ax_action_to_name("AXConfirm"), Some("press"));
        assert_eq!(ax_action_to_name("AXShowMenu"), Some("show_menu"));
        assert_eq!(ax_action_to_name("AXIncrement"), Some("increment"));
        assert_eq!(ax_action_to_name("AXDecrement"), Some("decrement"));
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
    // count. Never raise it.
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

    #[test]
    #[ignore]
    fn ax_calls_find_button_by_name() {
        // Searching for Button[name="Submit"] from the app root.
        // Lightweight matching checks only role+name per node; batch
        // fetch builds full ElementData only for the single match.
        let _lock = ax_counters::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let provider = MacOSProvider::new().unwrap();
        let app = find_test_app(&provider);

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
        const MAX_CALLS: u64 = 298;
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
        const MAX_CALLS: u64 = 292;
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
        const MAX_CALLS: u64 = 284;
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
