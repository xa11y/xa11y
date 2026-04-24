//! macOS AXUIElement-based accessibility provider.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use rayon::prelude::*;

use xa11y_core::{
    CancelHandle, ElementData, Error, Event, EventKind, EventReceiver, Provider, Rect, Result,
    Role, Selector, StateFlag, StateSet, Subscription, Toggled,
};

// ── FFI Declarations ──────────────────────────────────────────────────────────

type AXUIElementRef = *const c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFIndex = isize;

const AX_ERROR_SUCCESS: i32 = 0;
const AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;
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
    fn safe_ax_create_application(pid: i32) -> AXUIElementRef;
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
    fn safe_ax_observer_get_run_loop_source(observer: CFTypeRef) -> CFTypeRef;
    fn safe_cf_run_loop_add_source(source: CFTypeRef);
    fn safe_cf_run_loop_get_current() -> CFTypeRef;
    fn safe_cf_run_loop_run();
    fn safe_cf_run_loop_stop(run_loop: CFTypeRef);
    fn safe_ax_value_create_cf_range(location: isize, length: isize) -> CFTypeRef;

    // CoreFoundation helpers - all calls from ax.rs go through these.
    fn safe_cf_retain(cf: CFTypeRef) -> CFTypeRef;
    fn safe_cf_release(cf: CFTypeRef);
    fn safe_cf_get_type_id(cf: CFTypeRef) -> u64;
    fn safe_cf_array_get_count(arr: CFArrayRef) -> CFIndex;
    fn safe_cf_array_get_value(arr: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn safe_cf_boolean_get_value(b: CFTypeRef) -> bool;
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
    pub const COUNT: usize = 15;
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

// ── Role Mapping ──────────────────────────────────────────────────────────────

fn map_ax_role(role: &str, subrole: Option<&str>) -> Role {
    match subrole {
        Some("AXDialog") => return Role::Dialog,
        Some("AXApplicationAlert") | Some("AXSystemAlert") => return Role::Alert,
        Some("AXTabButton") => return Role::Tab,
        Some("AXOutlineRow") => return Role::TreeItem,
        Some("AXHeading") => return Role::Heading,
        Some("AXSwitch") => return Role::Switch,
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
        // the match set is assembled.
        let data = build_snapshot_data(ax, None, 0);
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

/// Global handle counter for mapping ElementData back to AXElements.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub struct MacOSProvider {
    /// Cached AXElement refs keyed by handle ID.
    handle_cache: Mutex<HashMap<u64, AXElement>>,
}

impl MacOSProvider {
    pub fn new() -> Result<Self> {
        if !unsafe { safe_ax_is_process_trusted() } {
            return Err(Error::PermissionDenied {
                instructions:
                    "Enable Accessibility in System Settings → Privacy & Security → Accessibility"
                        .to_string(),
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
    fn build_element_data(&self, ax: &AXElement, pid: Option<u32>) -> ElementData {
        let handle = self.cache_element(ax.clone());
        build_snapshot_data(ax.as_ptr(), pid, handle)
    }
}

/// Build a snapshot `ElementData` from a raw `AXUIElementRef` without touching
/// the provider's handle cache. `handle` is stored verbatim — callers that
/// want the snapshot to be navigable later must supply one from
/// `MacOSProvider::cache_element`. For read-only snapshots (e.g. event
/// targets), pass `0`.
fn build_snapshot_data(element: AXUIElementRef, pid: Option<u32>, handle: u64) -> ElementData {
    if element.is_null() {
        return ElementData {
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
            raw: std::collections::HashMap::new(),
            pid,
            handle,
        };
    }

    let body = move || -> ElementData {
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

        let states = StateSet {
            enabled: attrs.enabled.unwrap_or(true),
            visible: !attrs.hidden.unwrap_or(false),
            focused: attrs.focused.unwrap_or(false),
            focusable,
            modal: attrs.modal.unwrap_or(false),
            checked,
            selected: attrs.selected.unwrap_or(false),
            expanded: attrs.expanded,
            editable: matches!(role, Role::TextField | Role::TextArea),
            required: false,
            busy: false,
        };

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

        let numeric_value = match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton => attrs.value_number,
            _ => None,
        };

        // Min/max still require individual calls (not in the batch set).
        let (min_value, max_value) = match role {
            Role::Slider => (
                ax_number_f64(element, "AXMinValue"),
                ax_number_f64(element, "AXMaxValue"),
            ),
            _ => (None, None),
        };

        ElementData {
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            stable_id: attrs.identifier,
            numeric_value,
            min_value,
            max_value,
            raw,
            pid,
            handle,
        }
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

    /// Parallel DFS collecting AXElements matching a SimpleSelector without
    /// building full ElementData. At each level, children are processed in
    /// parallel using rayon — each child's role check and recursive subtree
    /// search happen concurrently across threads.
    #[allow(clippy::too_many_arguments)] // recursive DFS with parent context
    fn collect_matching_ax(
        &self,
        parent: &AXElement,
        parent_role: Role,
        parent_name: Option<&str>,
        simple: &SimpleSelector,
        depth: u32,
        max_depth: u32,
        limit: Option<usize>,
    ) -> Vec<AXElement> {
        if depth > max_depth {
            return vec![];
        }

        let children = ax_children(parent.as_ptr());

        // Process children in parallel: check match + recurse.
        let per_child_results: Vec<Vec<AXElement>> = children
            .par_iter()
            .map(|child| {
                let mut child_results = Vec::new();

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

                if matches_ax_with_role(child.as_ptr(), simple, Some(child_role)) {
                    child_results.push(child.clone());
                }

                // Recurse into subtree.
                let child_name = ax_string(child.as_ptr(), "AXTitle");
                let sub = self.collect_matching_ax(
                    child,
                    child_role,
                    child_name.as_deref(),
                    simple,
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

}

impl Provider for MacOSProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: list all GUI apps as application elements
                let apps = Self::list_gui_apps();
                let mut results = Vec::new();
                for (pid, app_name) in &apps {
                    let app_element =
                        AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
                    if app_element.is_null() {
                        continue;
                    }
                    let mut data = self.build_element_data(&app_element, Some(*pid as u32));
                    // Override name with the CGWindowList name (more reliable)
                    data.name = Some(app_name.clone());
                    results.push(data);
                }
                Ok(results)
            }
            Some(element_data) => {
                let ax = self.get_cached(element_data.handle)?;
                let role = element_data.role;
                let name = element_data.name.as_deref();

                let ax_children_list = ax_children(ax.as_ptr());

                // Filter first (cheap string checks), then build ElementData
                // in parallel (each build_element_data is an IPC round-trip).
                let filtered: Vec<&AXElement> = ax_children_list
                    .iter()
                    .filter(|child| !Self::should_filter_child(role, name, child))
                    .collect();

                let results: Vec<ElementData> = filtered
                    .par_iter()
                    .map(|child| self.build_element_data(child, element_data.pid))
                    .collect();

                Ok(results)
            }
        }
    }

    fn find_elements(
        &self,
        root: Option<&ElementData>,
        selector: &Selector,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        if selector.segments.is_empty() {
            return Ok(vec![]);
        }

        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        // Phase 1: lightweight AXElement-based search for first segment.
        // Only the attributes the selector needs are fetched per candidate.
        let first = &selector.segments[0].simple;

        let phase1_limit = if selector.segments.len() == 1 {
            limit
        } else {
            None
        };
        let phase1_limit = match (phase1_limit, first.nth) {
            (Some(l), Some(n)) => Some(l.max(n)),
            (_, Some(n)) => Some(n),
            (l, None) => l,
        };

        let root_data = match root {
            Some(el) => el,
            None => {
                // Searching from system root for applications. Use
                // CGWindowList (no AX calls) to filter apps by name, then
                // only build full ElementData for matches.
                if matches!(
                    first.role,
                    Some(xa11y_core::selector::RoleMatch::Normalized(
                        Role::Application
                    ))
                ) {
                    let apps = Self::list_gui_apps();
                    let mut matching: Vec<ElementData> = Vec::new();
                    for (pid, app_name) in &apps {
                        // Check name filters against CGWindowList name
                        let name_matches = first.filters.iter().all(|f| {
                            if f.attr != "name" {
                                return true; // non-name filters checked after build
                            }
                            match_op(&f.op, &f.value, Some(app_name.as_str()))
                        });
                        if !name_matches {
                            continue;
                        }

                        let app_element =
                            AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
                        if app_element.is_null() {
                            continue;
                        }
                        let mut data = self.build_element_data(&app_element, Some(*pid as u32));
                        data.name = Some(app_name.clone());
                        matching.push(data);

                        if let Some(limit) = phase1_limit {
                            if matching.len() >= limit {
                                break;
                            }
                        }
                    }

                    // Apply :nth
                    if let Some(nth) = first.nth {
                        if nth <= matching.len() {
                            matching = vec![matching.remove(nth - 1)];
                        } else {
                            matching.clear();
                        }
                    }

                    if selector.segments.len() == 1 {
                        if let Some(limit) = limit {
                            matching.truncate(limit);
                        }
                        return Ok(matching);
                    }

                    return self.narrow_multi_segment(
                        matching,
                        &selector.segments[1..],
                        max_depth_val,
                        limit,
                    );
                }

                // Non-application root search — fall back to default impl.
                return xa11y_core::selector::find_elements_in_tree(
                    |el| self.get_children(el),
                    root,
                    selector,
                    limit,
                    max_depth,
                );
            }
        };

        let root_ax = self.get_cached(root_data.handle)?;

        let mut matching_ax = self.collect_matching_ax(
            &root_ax,
            root_data.role,
            root_data.name.as_deref(),
            first,
            0,
            max_depth_val,
            phase1_limit,
        );

        // Single-segment: build ElementData only for matches, apply nth/limit
        if selector.segments.len() == 1 {
            if let Some(nth) = first.nth {
                if nth <= matching_ax.len() {
                    let ax = &matching_ax[nth - 1];
                    return Ok(vec![self.build_element_data(ax, root_data.pid)]);
                } else {
                    return Ok(vec![]);
                }
            }

            if let Some(limit) = limit {
                matching_ax.truncate(limit);
            }

            return Ok(matching_ax
                .iter()
                .map(|ax| self.build_element_data(ax, root_data.pid))
                .collect());
        }

        // Multi-segment: build ElementData for phase 1 matches, then narrow
        // using standard matching on the (small) candidate set.
        let candidates: Vec<ElementData> = matching_ax
            .iter()
            .map(|ax| self.build_element_data(ax, root_data.pid))
            .collect();

        self.narrow_multi_segment(candidates, &selector.segments[1..], max_depth_val, limit)
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let ax = self.get_cached(element.handle)?;
        match ax_parent(ax.as_ptr()) {
            Some(parent_ax) => {
                if parent_ax.is_null() {
                    return Ok(None);
                }
                // Check if parent is an application — if so, still return it
                let data = self.build_element_data(&parent_ax, element.pid);
                Ok(Some(data))
            }
            None => Ok(None),
        }
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
        set_bool_attr(ax.as_ptr(), "AXExpanded", true, "expand", element.role)
    }

    fn collapse(&self, element: &ElementData) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        set_bool_attr(ax.as_ptr(), "AXExpanded", false, "collapse", element.role)
    }

    fn scroll_into_view(&self, _element: &ElementData) -> Result<()> {
        // macOS has no accessibility API equivalent for scroll-into-view.
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
    // so tests can assert on name, value, states, numeric_value, etc.
    let target = if element.is_null() {
        None
    } else {
        Some(build_snapshot_data(element, Some(ctx.app_pid), 0))
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

        "AXWindowMiniaturized" => vec![EventKind::WindowDeactivated],
        "AXWindowDeminiaturized" => vec![EventKind::WindowActivated],

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
        let event = Event {
            kind,
            app_name: ctx.app_name.clone(),
            app_pid: ctx.app_pid,
            target: target.clone(),
            timestamp: std::time::Instant::now(),
        };
        let _ = ctx.sender.send(event);
    }
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
            return Err(Error::SelectorNotMatched {
                selector: format!("application with pid:{}", pid),
            });
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

        for notif in &notifications {
            let name = CFString::new(notif);
            let _ = unsafe {
                safe_ax_observer_add_notification(
                    observer,
                    app_element,
                    name.as_concrete_TypeRef() as CFTypeRef,
                    ctx_ptr,
                )
            };
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
    fn ax_string_returns_none_for_null_element() {
        let result = ax_string(std::ptr::null(), "AXTitle");
        assert!(result.is_none());
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
    }

    #[test]
    fn ax_action_to_name_returns_none_for_unknown() {
        // Unknown AX actions get converted via ax_pascal_to_snake instead
        assert_eq!(ax_action_to_name("AXRaise"), None);
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

    /// Find the test app's root ElementData via find_elements (same path
    /// as App::by_name, which is known to work).
    fn find_test_app(provider: &MacOSProvider) -> ElementData {
        let selector = Selector::parse("application[name=\"xa11y-test-app\"]").unwrap();
        let results = provider
            .find_elements(None, &selector, Some(1), Some(0))
            .unwrap();
        results
            .into_iter()
            .next()
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
            .find_elements(Some(&app), &selector, Some(1), None)
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
        const MAX_CALLS: u64 = 294;
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
            .find_elements(Some(&window), &selector, Some(1), None)
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
        const MAX_CALLS: u64 = 288;
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
            .find_elements(Some(&window), &selector, None, None)
            .unwrap();
        let (copy_attr, copy_multi, copy_actions) = ax_counters::snapshot();
        let total = ax_counters::total();

        assert!(!results.is_empty(), "Should find at least one checkbox.");

        // Upper bound — this counts AX IPC calls. Reducing this number is good;
        // increasing it is a regression. Update the bound if a deliberate feature
        // addition raises it.
        const MAX_CALLS: u64 = 280;
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
