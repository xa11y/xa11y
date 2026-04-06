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
    Action, ActionData, CancelHandle, ElementData, Error, Event, EventReceiver, EventType,
    Provider, Rect, Result, Role, StateSet, Subscription, Toggled,
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

extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> u64;
    fn CFStringGetTypeID() -> u64;
    fn CFNumberGetTypeID() -> u64;
    fn CFBooleanGetTypeID() -> u64;
    fn CFArrayGetTypeID() -> u64;
    fn CFArrayGetCount(arr: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(arr: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn CFBooleanGetValue(b: CFTypeRef) -> bool;
    fn CFNumberGetValue(num: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const CFTypeRef,
        num_values: CFIndex,
        callbacks: *const c_void,
    ) -> CFArrayRef;
    static kCFTypeArrayCallBacks: c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

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
    fn safe_cg_post_scroll_event(dy: i32, dx: i32);
    fn safe_ax_value_create_cf_range(location: isize, length: isize) -> CFTypeRef;

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
            unsafe { CFRetain(ptr) };
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
            unsafe { CFRelease(self.0) };
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
        if CFGetTypeID(value) == CFStringGetTypeID() {
            let s = CFString::wrap_under_create_rule(value as *const _);
            Some(s.to_string())
        } else {
            CFRelease(value);
            None
        }
    }
}

fn ax_bool(element: AXUIElementRef, attribute: &str) -> Option<bool> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            let b = CFBooleanGetValue(value);
            CFRelease(value);
            Some(b)
        } else {
            CFRelease(value);
            None
        }
    }
}

fn ax_number_f64(element: AXUIElementRef, attribute: &str) -> Option<f64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut result: f64 = 0.0;
            let ok = CFNumberGetValue(
                value,
                CF_NUMBER_FLOAT64,
                &mut result as *mut _ as *mut c_void,
            );
            CFRelease(value);
            if ok {
                return Some(result);
            }
        }
        if CFGetTypeID(value) == CFStringGetTypeID() {
            let s = CFString::wrap_under_create_rule(value as *const _);
            return s.to_string().trim().parse::<f64>().ok();
        }
        CFRelease(value);
        None
    }
}

#[allow(dead_code)]
fn ax_number_i32(element: AXUIElementRef, attribute: &str) -> Option<i32> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut result: i32 = 0;
            let ok = CFNumberGetValue(
                value,
                CF_NUMBER_SINT32,
                &mut result as *mut _ as *mut c_void,
            );
            CFRelease(value);
            if ok {
                Some(result)
            } else {
                None
            }
        } else {
            CFRelease(value);
            None
        }
    }
}

#[allow(dead_code)]
fn ax_number_i64(element: AXUIElementRef, attribute: &str) -> Option<i64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut result: i64 = 0;
            let ok = CFNumberGetValue(
                value,
                CF_NUMBER_SINT64,
                &mut result as *mut _ as *mut c_void,
            );
            CFRelease(value);
            if ok {
                Some(result)
            } else {
                None
            }
        } else {
            CFRelease(value);
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
        if CFGetTypeID(value) != CFArrayGetTypeID() {
            CFRelease(value);
            return vec![];
        }
        let count = CFArrayGetCount(value);
        let mut children = Vec::with_capacity(count as usize);
        for i in 0..count {
            let child = CFArrayGetValueAtIndex(value, i);
            if !child.is_null() {
                children.push(AXElement::from_borrowed(child));
            }
        }
        CFRelease(value);
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
        let count = CFArrayGetCount(names);
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            let name = CFArrayGetValueAtIndex(names, i);
            if !name.is_null() && CFGetTypeID(name) == CFStringGetTypeID() {
                let s = CFString::wrap_under_get_rule(name as *const _);
                result.push(s.to_string());
            }
        }
        CFRelease(names);
        result
    }
}

fn ax_position(element: AXUIElementRef) -> Option<(f64, f64)> {
    let value = ax_attr(element, "AXPosition")?;
    let mut point = CGPoint::default();
    let ok = unsafe {
        safe_ax_value_get_value(value, AX_VALUE_CGPOINT, &mut point as *mut _ as *mut c_void)
    };
    unsafe { CFRelease(value) };
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
    unsafe { CFRelease(value) };
    if ok {
        Some((size.width, size.height))
    } else {
        None
    }
}

fn ax_value_string(element: AXUIElementRef) -> Option<String> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        let tid = CFGetTypeID(value);
        if tid == CFStringGetTypeID() {
            let s = CFString::wrap_under_create_rule(value as *const _);
            return Some(s.to_string());
        }
        if tid == CFNumberGetTypeID() {
            let mut f: f64 = 0.0;
            if CFNumberGetValue(value, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
                CFRelease(value);
                return Some(f.to_string());
            }
        }
        CFRelease(value);
        None
    }
}

fn ax_value_number(element: AXUIElementRef) -> Option<f64> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut f: f64 = 0.0;
            let ok = CFNumberGetValue(value, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void);
            CFRelease(value);
            if ok {
                return Some(f);
            }
        }
        if CFGetTypeID(value) == CFStringGetTypeID() {
            let s = CFString::wrap_under_create_rule(value as *const _);
            return s.to_string().trim().parse::<f64>().ok();
        }
        CFRelease(value);
        None
    }
}

fn ax_value_int(element: AXUIElementRef) -> Option<i32> {
    let value = ax_attr(element, "AXValue")?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut i: i32 = 0;
            let ok = CFNumberGetValue(value, CF_NUMBER_SINT32, &mut i as *mut _ as *mut c_void);
            CFRelease(value);
            if ok {
                return Some(i);
            }
        }
        CFRelease(value);
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

        let cf_attrs = unsafe {
            CFArrayCreate(
                std::ptr::null(),
                ptrs.as_ptr(),
                ptrs.len() as CFIndex,
                &kCFTypeArrayCallBacks,
            )
        };
        if cf_attrs.is_null() {
            return None;
        }

        let mut values: CFArrayRef = std::ptr::null();
        let err = ffi_copy_multiple_attribute_values(element, cf_attrs, &mut values);
        unsafe { CFRelease(cf_attrs) };

        if err != AX_ERROR_SUCCESS || values.is_null() {
            return None;
        }

        let count = unsafe { CFArrayGetCount(values) } as usize;
        let mut vals = [std::ptr::null(); attr_idx::COUNT];
        for (i, slot) in vals.iter_mut().enumerate().take(count.min(attr_idx::COUNT)) {
            let v = unsafe { CFArrayGetValueAtIndex(values, i as CFIndex) };
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
            if CFGetTypeID(v) == CFStringGetTypeID() {
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
            if CFGetTypeID(v) == CFBooleanGetTypeID() {
                Some(CFBooleanGetValue(v))
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
            let tid = CFGetTypeID(v);
            if tid == CFStringGetTypeID() {
                let s = CFString::wrap_under_get_rule(v as *const _);
                return Some(s.to_string());
            }
            if tid == CFNumberGetTypeID() {
                let mut f: f64 = 0.0;
                if CFNumberGetValue(v, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
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
            if CFGetTypeID(v) == CFNumberGetTypeID() {
                let mut f: f64 = 0.0;
                if CFNumberGetValue(v, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void) {
                    return Some(f);
                }
            }
            if CFGetTypeID(v) == CFStringGetTypeID() {
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
            if CFGetTypeID(v) == CFNumberGetTypeID() {
                let mut i: i32 = 0;
                if CFNumberGetValue(v, CF_NUMBER_SINT32, &mut i as *mut _ as *mut c_void) {
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
            unsafe { CFRelease(self._values_array) };
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
                    if CFGetTypeID(v) == CFBooleanGetTypeID() {
                        Some(CFBooleanGetValue(v))
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
fn action_error(err: i32, action: &Action, role: Role, fallback_msg: &str) -> Error {
    if err == AX_ERROR_ACTION_UNSUPPORTED {
        Error::ActionNotSupported {
            action: action.clone(),
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
    action: &Action,
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
    action: &Action,
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

/// SetValue: set AXValue to either a string or numeric value.
fn perform_set_value(el_ptr: AXUIElementRef, data: Option<ActionData>) -> Result<()> {
    match data {
        Some(ActionData::NumericValue(v)) => {
            let attr = CFString::new("AXValue");
            let num = CFNumber::from(v);
            let err = do_set_attribute(el_ptr, &attr, num.as_CFTypeRef());
            if err != AX_ERROR_SUCCESS {
                return Err(Error::Platform {
                    code: err as i64,
                    message: "SetValue numeric failed".to_string(),
                });
            }
            Ok(())
        }
        Some(ActionData::Value(text)) => {
            let attr = CFString::new("AXValue");
            let val = CFString::new(&text);
            let err = do_set_attribute(el_ptr, &attr, val.as_concrete_TypeRef() as CFTypeRef);
            if err != AX_ERROR_SUCCESS {
                return Err(Error::TextValueNotSupported);
            }
            Ok(())
        }
        _ => Err(Error::InvalidActionData {
            message: "SetValue requires ActionData::Value or ActionData::NumericValue".to_string(),
        }),
    }
}

/// Extract scroll amount from ActionData.
fn require_scroll_amount(data: Option<ActionData>) -> Result<f64> {
    match data {
        Some(ActionData::ScrollAmount(amount)) => Ok(amount),
        _ => Err(Error::InvalidActionData {
            message: "Scroll requires ActionData::ScrollAmount".to_string(),
        }),
    }
}

/// SetTextSelection: set AXSelectedTextRange to a CFRange.
fn perform_set_text_selection(el_ptr: AXUIElementRef, data: Option<ActionData>) -> Result<()> {
    let (start, end) = match data {
        Some(ActionData::TextSelection { start, end }) => (start, end),
        _ => {
            return Err(Error::InvalidActionData {
                message: "SetTextSelection requires ActionData::TextSelection".to_string(),
            })
        }
    };
    let location = start as isize;
    let length = (end - start) as isize;
    let range_value = unsafe { safe_ax_value_create_cf_range(location, length) };
    if range_value.is_null() {
        return Err(Error::Platform {
            code: -1,
            message: "Failed to create CFRange value".to_string(),
        });
    }
    let attr = CFString::new("AXSelectedTextRange");
    let err = do_set_attribute(el_ptr, &attr, range_value);
    unsafe { CFRelease(range_value) };
    if err != AX_ERROR_SUCCESS {
        return Err(Error::Platform {
            code: err as i64,
            message: "Set AXSelectedTextRange failed".to_string(),
        });
    }
    Ok(())
}

/// TypeText: set AXSelectedText to insert text at the cursor.
fn perform_type_text(el_ptr: AXUIElementRef, data: Option<ActionData>) -> Result<()> {
    let text = match data {
        Some(ActionData::Value(text)) => text,
        _ => {
            return Err(Error::InvalidActionData {
                message: "TypeText requires ActionData::Value".to_string(),
            })
        }
    };
    let attr = CFString::new("AXSelectedText");
    let val = CFString::new(&text);
    let err = do_set_attribute(el_ptr, &attr, val.as_concrete_TypeRef() as CFTypeRef);
    if err != AX_ERROR_SUCCESS {
        return Err(Error::Platform {
            code: err as i64,
            message: "Set AXSelectedText failed".to_string(),
        });
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
//   - Known names (e.g. "AXPress") map to `Action` enum variants
//   - Unknown names following `AXFooBar` convention become `snake_case` custom
//     actions (e.g. "AXCustomThing" → "custom_thing")
//   - Implicit actions are added from settable attributes (Focus, SetValue)
//
// For **performing** a custom action by name:
//   1. Convert `snake_case` → `AXPascalCase` (e.g. "custom_thing" → "AXCustomThing")
//   2. Check if the element's action list contains that name
//   3. If not, try the literal `snake_case` name
//   4. If neither matches, return error

/// A single entry in the macOS AX ↔ xa11y action mapping table.
struct AxActionMapping {
    action: Action,
    /// The canonical AX action name (round-trips through [`map_ax_action`]).
    canonical: &'static str,
    /// Additional AX action names that map to the same xa11y Action.
    aliases: &'static [&'static str],
}

/// Single source of truth for macOS AX ↔ xa11y action mappings.
///
/// Only actions that use the AX Action interface are listed. Actions handled
/// via AX attributes (Toggle, Select, Focus, SetValue, Expand, Collapse) or
/// that have no macOS equivalent are omitted.
const AX_ACTION_MAPPINGS: &[AxActionMapping] = &[
    AxActionMapping {
        action: Action::Press,
        canonical: "AXPress",
        aliases: &["AXConfirm"],
    },
    AxActionMapping {
        action: Action::ShowMenu,
        canonical: "AXShowMenu",
        aliases: &[],
    },
    AxActionMapping {
        action: Action::Increment,
        canonical: "AXIncrement",
        aliases: &[],
    },
    AxActionMapping {
        action: Action::Decrement,
        canonical: "AXDecrement",
        aliases: &[],
    },
];

/// AX action names that are valid but intentionally unmapped (no xa11y equivalent).
/// These are recognized platform actions that we deliberately skip during
/// action discovery — they don't correspond to any user-facing automation action.
const AX_IGNORED_ACTIONS: &[&str] = &["AXRaise", "AXCancel"];

/// Map an AX action name to a well-known xa11y Action.
///
/// Returns `None` for unrecognized names (which may be custom actions).
fn map_ax_action(name: &str) -> Option<Action> {
    AX_ACTION_MAPPINGS.iter().find_map(|m| {
        if m.canonical == name || m.aliases.contains(&name) {
            Some(m.action)
        } else {
            None
        }
    })
}

/// Map a well-known xa11y Action to its canonical macOS AX action name.
///
/// Returns `None` for actions handled via AX attributes rather than the
/// AX Action interface.
fn xa11y_action_to_ax(action: &Action) -> Option<&'static str> {
    AX_ACTION_MAPPINGS
        .iter()
        .find(|m| m.action == *action)
        .map(|m| m.canonical)
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

/// Classify an AX action name from `AXUIElementCopyActionNames`.
///
/// Returns:
/// - `Some(action)` for well-known actions
/// - `None` for ignored actions (AXRaise, AXCancel)
///
/// Unknown actions are not returned here — the caller handles them separately
/// by converting to `snake_case` via [`ax_pascal_to_snake`].
fn classify_ax_action(name: &str) -> ActionClassification {
    if let Some(action) = map_ax_action(name) {
        ActionClassification::Known(action)
    } else if AX_IGNORED_ACTIONS.contains(&name) {
        ActionClassification::Ignored
    } else {
        ActionClassification::Custom(ax_pascal_to_snake(name))
    }
}

enum ActionClassification {
    Known(Action),
    Ignored,
    Custom(String),
}

// ── Lightweight selector matching (no ElementData) ───────────────────────────

use xa11y_core::selector::{match_op, Combinator, SimpleSelector};
use xa11y_core::Selector;

/// Test whether a raw AXElement matches a SimpleSelector, fetching only the
/// attributes the selector actually inspects. This avoids building a full
/// ElementData (15-20 AX API calls) for elements that will be discarded.
#[cfg(test)]
fn matches_ax(ax: AXUIElementRef, simple: &SimpleSelector) -> bool {
    matches_ax_with_role(ax, simple, None)
}

/// Like `matches_ax` but accepts a pre-resolved role to avoid redundant
/// AX API calls when the caller already fetched the role.
fn matches_ax_with_role(
    ax: AXUIElementRef,
    simple: &SimpleSelector,
    precomputed_role: Option<Role>,
) -> bool {
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
            // Unknown attributes: not resolvable in lightweight matching
            _ => None,
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
        if !unsafe { AXIsProcessTrusted() } {
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
            let count = CFArrayGetCount(info);
            for i in 0..count {
                let dict = CFArrayGetValueAtIndex(info, i);
                if dict.is_null() {
                    continue;
                }

                let pid_val =
                    CFDictionaryGetValue(dict, pid_key.as_concrete_TypeRef() as CFTypeRef);
                let name_val =
                    CFDictionaryGetValue(dict, name_key.as_concrete_TypeRef() as CFTypeRef);

                if pid_val.is_null() {
                    continue;
                }

                let mut pid: i32 = 0;
                if CFGetTypeID(pid_val) == CFNumberGetTypeID() {
                    CFNumberGetValue(pid_val, CF_NUMBER_SINT32, &mut pid as *mut _ as *mut c_void);
                }

                if pid <= 0 || !seen.insert(pid) {
                    continue;
                }

                let name = if !name_val.is_null() && CFGetTypeID(name_val) == CFStringGetTypeID() {
                    CFString::wrap_under_get_rule(name_val as *const _).to_string()
                } else {
                    String::new()
                };

                if !name.is_empty() {
                    apps.push((pid, name));
                }
            }
            CFRelease(info);
        }

        apps
    }

    /// Check if Screen Recording permission is granted by inspecting
    /// CGWindowListCopyWindowInfo. Without this permission, the list
    /// contains only system chrome (layer != 0). With it, app windows
    /// (layer 0) are included.
    fn has_screen_recording_permission() -> bool {
        let info = unsafe { safe_cg_window_list_copy(0, 0) };
        if info.is_null() {
            return false;
        }
        let layer_key = CFString::new("kCGWindowLayer");
        let mut has_app_window = false;
        unsafe {
            let count = CFArrayGetCount(info);
            for i in 0..count {
                let dict = CFArrayGetValueAtIndex(info, i);
                if dict.is_null() {
                    continue;
                }
                let layer_val =
                    CFDictionaryGetValue(dict, layer_key.as_concrete_TypeRef() as CFTypeRef);
                if !layer_val.is_null() && CFGetTypeID(layer_val) == CFNumberGetTypeID() {
                    let mut layer: i32 = -1;
                    CFNumberGetValue(
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
            CFRelease(info);
        }
        has_app_window
    }

    /// Cache an AXElement and return a new handle ID.
    fn cache_element(&self, ax: AXElement) -> u64 {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handle_cache.lock().unwrap().insert(handle, ax);
        handle
    }

    /// Look up a cached AXElement by handle.
    fn get_cached(&self, handle: u64) -> Result<AXElement> {
        self.handle_cache
            .lock()
            .unwrap()
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
        let attrs = if let Some(batch) = BatchAttrs::fetch(ax.as_ptr()) {
            ResolvedAttrs::from_batch(&batch)
        } else {
            ResolvedAttrs::from_individual(ax.as_ptr())
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
        let ax_actions = ax_action_names(ax.as_ptr());
        let mut actions: Vec<Action> = Vec::new();

        for ax_name in &ax_actions {
            match classify_ax_action(ax_name) {
                ActionClassification::Known(action) => {
                    if !actions.contains(&action) {
                        actions.push(action);
                    }
                }
                ActionClassification::Custom(snake) => {
                    let custom = Action::Custom(snake);
                    if !actions.contains(&custom) {
                        actions.push(custom);
                    }
                }
                ActionClassification::Ignored => {}
            }
        }

        // Implicit actions from settable attributes.
        if attrs.focused.is_some() && !actions.contains(&Action::Focus) {
            actions.push(Action::Focus);
        }
        if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
            && !actions.contains(&Action::SetValue)
        {
            actions.push(Action::SetValue);
        }

        let numeric_value = match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton => attrs.value_number,
            _ => None,
        };

        // Min/max still require individual calls (not in the batch set).
        let (min_value, max_value) = match role {
            Role::Slider => (
                ax_number_f64(ax.as_ptr(), "AXMinValue"),
                ax_number_f64(ax.as_ptr(), "AXMaxValue"),
            ),
            _ => (None, None),
        };

        let handle = self.cache_element(ax.clone());

        let mut data = ElementData {
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
            attributes: std::collections::HashMap::new(),
            raw,
            pid,
            handle,
        };
        data.populate_attributes();
        data
    }

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

    /// Narrow candidates through remaining selector segments (Child/Descendant
    /// combinators), deduplicate, apply final :nth and limit.
    fn narrow_multi_segment(
        &self,
        mut candidates: Vec<ElementData>,
        segments: &[xa11y_core::selector::SelectorSegment],
        max_depth: u32,
        limit: Option<usize>,
    ) -> Result<Vec<ElementData>> {
        for segment in segments {
            let mut next_candidates = Vec::new();
            for candidate in &candidates {
                match segment.combinator {
                    Combinator::Child => {
                        let children = self.get_children(Some(candidate))?;
                        for child in children {
                            if xa11y_core::selector::matches_simple(&child, &segment.simple) {
                                next_candidates.push(child);
                            }
                        }
                    }
                    Combinator::Descendant => {
                        let sub_selector = Selector {
                            segments: vec![xa11y_core::selector::SelectorSegment {
                                combinator: Combinator::Root,
                                simple: segment.simple.clone(),
                            }],
                        };
                        let mut sub_results = self.find_elements(
                            Some(candidate),
                            &sub_selector,
                            None,
                            Some(max_depth),
                        )?;
                        next_candidates.append(&mut sub_results);
                    }
                    Combinator::Root => unreachable!(),
                }
            }
            let mut seen = HashSet::new();
            next_candidates.retain(|e| seen.insert(e.handle));
            candidates = next_candidates;
        }

        // Apply :nth on last segment
        if let Some(nth) = segments.last().and_then(|s| s.simple.nth) {
            if nth <= candidates.len() {
                candidates = vec![candidates.remove(nth - 1)];
            } else {
                candidates.clear();
            }
        }

        if let Some(limit) = limit {
            candidates.truncate(limit);
        }

        Ok(candidates)
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

    fn perform_action(
        &self,
        element: &ElementData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let ax = self.get_cached(element.handle)?;
        let el_ptr = ax.as_ptr();

        match action {
            // ── AX Action interface ─────────────────────────────────────
            //
            // These actions map 1:1 to AX action names via the mapping table.
            // Press and Toggle both invoke AXPress — macOS uses the same
            // action for buttons, checkboxes, and switches.
            Action::Press | Action::Toggle => {
                perform_ax_action(el_ptr, "AXPress", &action, element.role)
            }
            Action::ShowMenu => perform_ax_action(el_ptr, "AXShowMenu", &action, element.role),
            Action::Increment => perform_ax_action(el_ptr, "AXIncrement", &action, element.role),
            Action::Decrement => perform_ax_action(el_ptr, "AXDecrement", &action, element.role),

            // ── Attribute-based actions ─────────────────────────────────
            //
            // These actions set AX attributes rather than invoking AX actions.
            Action::Focus => set_bool_attr(el_ptr, "AXFocused", true, &action, element.role),
            Action::Blur => set_bool_attr(el_ptr, "AXFocused", false, &action, element.role),
            Action::Select => set_bool_attr(el_ptr, "AXSelected", true, &action, element.role),
            Action::Expand => set_bool_attr(el_ptr, "AXExpanded", true, &action, element.role),
            Action::Collapse => set_bool_attr(el_ptr, "AXExpanded", false, &action, element.role),

            Action::SetValue => perform_set_value(el_ptr, data),

            // ── Scroll ──────────────────────────────────────────────────
            //
            // macOS has no accessibility API for programmatic scrolling, so
            // these use CGEvent scroll wheel events (input simulation).
            Action::ScrollIntoView => Ok(()),

            Action::ScrollDown => {
                let amount = require_scroll_amount(data)?;
                let dy = -(amount * 10.0) as i32;
                unsafe { safe_cg_post_scroll_event(dy, 0) };
                Ok(())
            }
            Action::ScrollRight => {
                let amount = require_scroll_amount(data)?;
                let dx = -(amount * 10.0) as i32;
                unsafe { safe_cg_post_scroll_event(0, dx) };
                Ok(())
            }

            // ── Text operations ─────────────────────────────────────────
            Action::SetTextSelection => perform_set_text_selection(el_ptr, data),
            Action::TypeText => perform_type_text(el_ptr, data),

            // ── Custom platform-specific actions ────────────────────────
            //
            // Resolution strategy per design doc:
            // 1. Convert snake_case → AXPascalCase, check element's action list
            // 2. Try the literal snake_case name
            // 3. Return error if neither matches
            Action::Custom(ref name) => {
                let available = ax_action_names(el_ptr);

                // Strategy 1: snake_case → AXPascalCase
                let ax_name = snake_to_ax_pascal(name);
                if available.iter().any(|a| a == &ax_name) {
                    let cf_action = CFString::new(&ax_name);
                    let err = do_perform_action(el_ptr, &cf_action);
                    if err != AX_ERROR_SUCCESS {
                        return Err(action_error(err, &action, element.role, &ax_name));
                    }
                    return Ok(());
                }

                // Strategy 2: literal name
                if available.iter().any(|a| a == name.as_str()) {
                    let cf_action = CFString::new(name);
                    let err = do_perform_action(el_ptr, &cf_action);
                    if err != AX_ERROR_SUCCESS {
                        return Err(action_error(err, &action, element.role, name));
                    }
                    return Ok(());
                }

                Err(Error::ActionNotSupported {
                    action,
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

    let event_type = match notif_str.as_str() {
        "AXValueChanged" => EventType::ValueChanged,
        "AXFocusedUIElementChanged" => EventType::FocusChanged,
        "AXWindowCreated" => EventType::WindowOpened,
        "AXWindowMiniaturized" => EventType::WindowDeactivated,
        "AXWindowDeminiaturized" => EventType::WindowActivated,
        "AXUIElementDestroyed" => EventType::StructureChanged,
        "AXSelectedTextChanged" => EventType::SelectionChanged,
        "AXMenuOpened" => EventType::MenuOpened,
        "AXMenuClosed" => EventType::MenuClosed,
        "AXTitleChanged" => EventType::NameChanged,
        _ => return,
    };

    let target = if !element.is_null() {
        let role_str = ax_string(element, "AXRole").unwrap_or_default();
        let subrole = ax_string(element, "AXSubrole");
        let role = map_ax_role(&role_str, subrole.as_deref());
        Some({
            let mut data = ElementData {
                role,
                name: ax_string(element, "AXTitle"),
                value: ax_value_string(element),
                description: ax_string(element, "AXDescription"),
                bounds: None,
                actions: vec![],
                states: StateSet::default(),
                numeric_value: None,
                min_value: None,
                max_value: None,
                stable_id: None,
                attributes: std::collections::HashMap::new(),
                raw: std::collections::HashMap::new(),
                pid: None,
                handle: 0,
            };
            data.populate_attributes();
            data
        })
    } else {
        None
    };

    let event = Event {
        event_type,
        app_name: ctx.app_name.clone(),
        app_pid: ctx.app_pid,
        target,
        state_flag: None,
        state_value: None,
        text_change: None,
        timestamp: std::time::Instant::now(),
    };
    let _ = ctx.sender.send(event);
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
                CFRelease(observer);
                drop(Box::from_raw(ctx_ptr as *mut ObserverContext));
            }
            return Err(Error::SelectorNotMatched {
                selector: format!("application with pid:{}", pid),
            });
        }

        let notifications = [
            "AXValueChanged",
            "AXFocusedUIElementChanged",
            "AXWindowCreated",
            "AXWindowMiniaturized",
            "AXWindowDeminiaturized",
            "AXUIElementDestroyed",
            "AXSelectedTextChanged",
            "AXMenuOpened",
            "AXMenuClosed",
            "AXTitleChanged",
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

        unsafe { CFRelease(app_element) };

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

        let run_loop_usize =
            rl_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| Error::Platform {
                    code: -1,
                    message: "Failed to start observer RunLoop".to_string(),
                })?;

        let ctx_usize = ctx_ptr as usize;

        let cancel = CancelHandle::new(move || {
            unsafe {
                safe_cf_run_loop_stop(run_loop_usize as CFTypeRef);
            }
            let _ = handle.join();
            unsafe {
                drop(Box::from_raw(ctx_usize as *mut ObserverContext));
                CFRelease(observer_usize as CFTypeRef);
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
    fn map_ax_action_covers_all_mappings() {
        assert_eq!(map_ax_action("AXPress"), Some(Action::Press));
        assert_eq!(map_ax_action("AXConfirm"), Some(Action::Press));
        assert_eq!(map_ax_action("AXShowMenu"), Some(Action::ShowMenu));
        assert_eq!(map_ax_action("AXIncrement"), Some(Action::Increment));
        assert_eq!(map_ax_action("AXDecrement"), Some(Action::Decrement));
        assert_eq!(map_ax_action("AXRaise"), None);
        assert_eq!(map_ax_action("AXCancel"), None);
        assert_eq!(map_ax_action("UnknownAction"), None);
    }

    #[test]
    fn xa11y_action_to_ax_covers_all_mappings() {
        assert_eq!(xa11y_action_to_ax(&Action::Press), Some("AXPress"));
        // Toggle is attribute-based on macOS (no distinct AX action)
        assert_eq!(xa11y_action_to_ax(&Action::Toggle), None);
        // Select is handled via AXSelected attribute, not AXPress action
        assert_eq!(xa11y_action_to_ax(&Action::Select), None);
        assert_eq!(xa11y_action_to_ax(&Action::ShowMenu), Some("AXShowMenu"));
        assert_eq!(xa11y_action_to_ax(&Action::Increment), Some("AXIncrement"));
        assert_eq!(xa11y_action_to_ax(&Action::Decrement), Some("AXDecrement"));
        assert_eq!(xa11y_action_to_ax(&Action::Focus), None);
        assert_eq!(xa11y_action_to_ax(&Action::SetValue), None);
        assert_eq!(xa11y_action_to_ax(&Action::Expand), None);
        assert_eq!(xa11y_action_to_ax(&Action::Collapse), None);
        assert_eq!(xa11y_action_to_ax(&Action::ScrollIntoView), None);
    }

    /// Every xa11y Action with a canonical AX name must round-trip:
    /// xa11y → AX → xa11y produces the same Action.
    #[test]
    fn test_action_roundtrip_xa11y_to_ax() {
        let actions_with_mapping = [
            Action::Press,
            Action::ShowMenu,
            Action::Increment,
            Action::Decrement,
        ];
        for action in &actions_with_mapping {
            let ax_name = xa11y_action_to_ax(action)
                .unwrap_or_else(|| panic!("{:?} should have a canonical AX name", action));
            let round_tripped = map_ax_action(ax_name).unwrap_or_else(|| {
                panic!("canonical name {:?} should map back to an Action", ax_name)
            });
            assert_eq!(
                *action, round_tripped,
                "round-trip failed: {:?} → {:?} → {:?}",
                action, ax_name, round_tripped
            );
        }
    }

    /// Every AX action name that maps to an xa11y Action must produce an Action
    /// whose canonical name maps back to the same Action.
    #[test]
    fn test_action_roundtrip_ax_to_xa11y() {
        let ax_names = [
            "AXPress",
            "AXConfirm",
            "AXShowMenu",
            "AXIncrement",
            "AXDecrement",
        ];
        for name in ax_names {
            let action = map_ax_action(name)
                .unwrap_or_else(|| panic!("AX name {:?} should map to an Action", name));
            let canonical = xa11y_action_to_ax(&action).unwrap_or_else(|| {
                panic!(
                    "{:?} (from {:?}) should have a canonical name",
                    action, name
                )
            });
            let back = map_ax_action(canonical)
                .unwrap_or_else(|| panic!("canonical {:?} should map back", canonical));
            assert_eq!(
                action, back,
                "AX {:?} → {:?} → canonical {:?} → {:?} (expected {:?})",
                name, action, canonical, back, action
            );
        }
    }

    /// No duplicate Action entries in the mapping table.
    #[test]
    fn ax_mapping_no_duplicate_actions() {
        for (i, a) in AX_ACTION_MAPPINGS.iter().enumerate() {
            for b in &AX_ACTION_MAPPINGS[i + 1..] {
                assert_ne!(
                    a.action, b.action,
                    "duplicate Action::{:?} in AX_ACTION_MAPPINGS",
                    a.action
                );
            }
        }
    }

    /// No duplicate canonical names in the mapping table.
    #[test]
    fn ax_mapping_no_duplicate_canonicals() {
        for (i, a) in AX_ACTION_MAPPINGS.iter().enumerate() {
            for b in &AX_ACTION_MAPPINGS[i + 1..] {
                assert_ne!(
                    a.canonical, b.canonical,
                    "duplicate canonical {:?} in AX_ACTION_MAPPINGS",
                    a.canonical
                );
            }
        }
    }

    /// Every canonical name round-trips through the table.
    #[test]
    fn ax_mapping_canonical_roundtrips() {
        for m in AX_ACTION_MAPPINGS {
            let mapped = map_ax_action(m.canonical);
            assert_eq!(
                mapped,
                Some(m.action),
                "canonical {:?} should map to {:?}",
                m.canonical,
                m.action
            );
        }
    }

    /// Every alias maps to the same action as its canonical.
    #[test]
    fn ax_mapping_aliases_consistent() {
        for m in AX_ACTION_MAPPINGS {
            for alias in m.aliases {
                let mapped = map_ax_action(alias);
                assert_eq!(
                    mapped,
                    Some(m.action),
                    "alias {:?} should map to {:?}",
                    alias,
                    m.action
                );
            }
        }
    }

    /// Ignored AX actions must not map to any xa11y Action.
    #[test]
    fn ax_ignored_actions_return_none() {
        for name in AX_IGNORED_ACTIONS {
            assert_eq!(
                map_ax_action(name),
                None,
                "ignored action {:?} should map to None",
                name
            );
        }
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
    fn classify_ax_action_known() {
        match classify_ax_action("AXPress") {
            ActionClassification::Known(Action::Press) => {}
            other => panic!(
                "expected Known(Press), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn classify_ax_action_ignored() {
        assert!(matches!(
            classify_ax_action("AXRaise"),
            ActionClassification::Ignored
        ));
        assert!(matches!(
            classify_ax_action("AXCancel"),
            ActionClassification::Ignored
        ));
    }

    #[test]
    fn classify_ax_action_custom() {
        match classify_ax_action("AXCustomThing") {
            ActionClassification::Custom(name) => assert_eq!(name, "custom_thing"),
            other => panic!("expected Custom, got {:?}", std::mem::discriminant(&other)),
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

        // Expected counts — ONLY LOWER THESE, never raise them.
        // When you optimize, update the count to match the new (lower) value.
        //
        // Breakdown: lightweight DFS fetches AXRole+AXSubrole per node (~2 calls
        // each) plus AXTitle/AXValue for name matching. 1 batch + 1 action-names
        // call for the single match's full ElementData.
        assert_eq!(
            total, 285,
            "AX call count regression: button[name=\"Submit\"] from app root.\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}\n\
             Only lower this count, never raise it.",
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

        assert_eq!(
            total, 279,
            "AX call count regression: button[name=\"Submit\"] from window.\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}\n\
             Only lower this count, never raise it.",
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

        assert_eq!(
            total, 272,
            "AX call count regression: check_box from window.\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}\n\
             Only lower this count, never raise it.",
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

        // With batch fetch, copy_multi should equal the number of children
        // (1 batch call per child for attributes, 1 action-names call per child).
        // copy_attr covers the filter checks (role/subrole for should_filter_child).
        assert_eq!(
            copy_multi, copy_actions,
            "Batch fetches should equal action-name fetches (1 per child).\n\
             copy_attr={copy_attr}, copy_multi={copy_multi}, copy_actions={copy_actions}\n\
             Only lower this count, never raise it.",
        );
    }
}
