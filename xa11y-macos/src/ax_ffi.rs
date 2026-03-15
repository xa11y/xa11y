//! Raw FFI bindings to macOS AXUIElement (ApplicationServices) functions.

use core_foundation::array::CFArrayRef;
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBooleanRef;
use core_foundation::number::CFNumberRef;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::ptr;

// Opaque AXUIElement type
pub type AXUIElementRef = *const c_void;
pub type AXError = i32;

// AXError codes
pub const K_AX_ERROR_SUCCESS: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn AXIsProcessTrusted() -> bool;

    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;

    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;

    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;

    pub fn AXUIElementGetAttributeValueCount(
        element: AXUIElementRef,
        attribute: CFStringRef,
        count: *mut i64,
    ) -> AXError;

    pub fn AXUIElementCopyAttributeValues(
        element: AXUIElementRef,
        attribute: CFStringRef,
        index: i64,
        max_values: i64,
        values: *mut CFArrayRef,
    ) -> AXError;

    pub fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;

    pub fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;

    pub fn AXUIElementCopyActionNames(element: AXUIElementRef, names: *mut CFArrayRef) -> AXError;

    pub fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
}

// Keep AXUIElementRef alive; release on drop via CFRelease.
use core_foundation::base::CFRelease;

/// Safe wrapper around AXUIElementRef with automatic memory management.
#[derive(Debug)]
pub struct AXElement {
    raw: AXUIElementRef,
}

unsafe impl Send for AXElement {}
unsafe impl Sync for AXElement {}

impl Clone for AXElement {
    fn clone(&self) -> Self {
        unsafe {
            core_foundation::base::CFRetain(self.raw as CFTypeRef);
        }
        Self { raw: self.raw }
    }
}

impl Drop for AXElement {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { CFRelease(self.raw as CFTypeRef) };
        }
    }
}

impl AXElement {
    /// Create from a raw AXUIElementRef. Takes ownership (does NOT retain).
    pub fn from_raw(raw: AXUIElementRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self { raw })
        }
    }

    /// Create an AXUIElement for an application by PID.
    pub fn application(pid: i32) -> Option<Self> {
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        Self::from_raw(raw)
    }

    /// Create a system-wide AXUIElement.
    pub fn system_wide() -> Option<Self> {
        let raw = unsafe { AXUIElementCreateSystemWide() };
        Self::from_raw(raw)
    }

    pub fn raw(&self) -> AXUIElementRef {
        self.raw
    }

    /// Get a string attribute value.
    pub fn string_attribute(&self, attr: &str) -> Option<String> {
        let cf_attr = CFString::new(attr);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        // Check if it's a CFString
        let cf_str: CFString = unsafe {
            if core_foundation::string::CFStringGetTypeID()
                == core_foundation::base::CFGetTypeID(value)
            {
                CFString::wrap_under_create_rule(value as CFStringRef)
            } else {
                CFRelease(value);
                return None;
            }
        };
        Some(cf_str.to_string())
    }

    /// Get a boolean attribute value.
    pub fn bool_attribute(&self, attr: &str) -> Option<bool> {
        let cf_attr = CFString::new(attr);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let type_id = unsafe { core_foundation::base::CFGetTypeID(value) };
        if type_id == unsafe { core_foundation::boolean::CFBooleanGetTypeID() } {
            let b = unsafe { core_foundation::boolean::CFBooleanGetValue(value as CFBooleanRef) };
            unsafe { CFRelease(value) };
            Some(b)
        } else {
            unsafe { CFRelease(value) };
            None
        }
    }

    /// Get a numeric (f64) attribute value.
    pub fn number_attribute(&self, attr: &str) -> Option<f64> {
        let cf_attr = CFString::new(attr);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let type_id = unsafe { core_foundation::base::CFGetTypeID(value) };
        if type_id == unsafe { core_foundation::number::CFNumberGetTypeID() } {
            let cf_num = unsafe {
                core_foundation::number::CFNumber::wrap_under_create_rule(value as CFNumberRef)
            };
            let result = cf_num.to_f64();
            result
        } else {
            unsafe { CFRelease(value) };
            None
        }
    }

    /// Get an AXUIElement attribute value (e.g. AXFocusedUIElement).
    pub fn element_attribute(&self, attr: &str) -> Option<AXElement> {
        let cf_attr = CFString::new(attr);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        // Treat the returned CFTypeRef as an AXUIElementRef (it is retained by CopyAttributeValue)
        Some(AXElement {
            raw: value as AXUIElementRef,
        })
    }

    /// Get children (array of AXUIElements).
    pub fn children(&self) -> Vec<AXElement> {
        let cf_attr = CFString::new("AXChildren");
        let mut count: i64 = 0;
        let err = unsafe {
            AXUIElementGetAttributeValueCount(self.raw, cf_attr.as_concrete_TypeRef(), &mut count)
        };
        if err != K_AX_ERROR_SUCCESS || count <= 0 {
            return vec![];
        }

        let mut array: CFArrayRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValues(
                self.raw,
                cf_attr.as_concrete_TypeRef(),
                0,
                count,
                &mut array,
            )
        };
        if err != K_AX_ERROR_SUCCESS || array.is_null() {
            return vec![];
        }

        let cf_array = unsafe {
            core_foundation::array::CFArray::<*const c_void>::wrap_under_create_rule(array)
        };
        let len = cf_array.len();
        let mut result = Vec::with_capacity(len as usize);
        for i in 0..len {
            let elem_ref = unsafe { *cf_array.get_unchecked(i) };
            if !elem_ref.is_null() {
                unsafe { core_foundation::base::CFRetain(elem_ref as CFTypeRef) };
                result.push(AXElement {
                    raw: elem_ref as AXUIElementRef,
                });
            }
        }
        result
    }

    /// Get the position (x, y) of the element.
    pub fn position(&self) -> Option<(f64, f64)> {
        let cf_attr = CFString::new("AXPosition");
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
        let ok = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                AX_VALUE_TYPE_CGPOINT,
                &mut point as *mut _ as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        if ok {
            Some((point.x, point.y))
        } else {
            None
        }
    }

    /// Get the size (width, height) of the element.
    pub fn size(&self) -> Option<(f64, f64)> {
        let cf_attr = CFString::new("AXSize");
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_attr.as_concrete_TypeRef(), &mut value)
        };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);
        let ok = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                AX_VALUE_TYPE_CGSIZE,
                &mut size as *mut _ as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        if ok {
            Some((size.width, size.height))
        } else {
            None
        }
    }

    /// Get action names supported by this element.
    pub fn action_names(&self) -> Vec<String> {
        let mut names: CFArrayRef = ptr::null();
        let err = unsafe { AXUIElementCopyActionNames(self.raw, &mut names) };
        if err != K_AX_ERROR_SUCCESS || names.is_null() {
            return vec![];
        }
        let cf_array = unsafe {
            core_foundation::array::CFArray::<*const c_void>::wrap_under_create_rule(names)
        };
        let len = cf_array.len();
        let mut result = Vec::with_capacity(len as usize);
        for i in 0..len {
            let val = unsafe { *cf_array.get_unchecked(i) };
            if !val.is_null() {
                let s = unsafe { CFString::wrap_under_get_rule(val as CFStringRef) };
                result.push(s.to_string());
            }
        }
        result
    }

    /// Perform a named action.
    pub fn perform_action(&self, action: &str) -> bool {
        let cf_action = CFString::new(action);
        let err = unsafe { AXUIElementPerformAction(self.raw, cf_action.as_concrete_TypeRef()) };
        err == K_AX_ERROR_SUCCESS
    }

    /// Set an attribute to a string value.
    pub fn set_string_attribute(&self, attr: &str, value: &str) -> bool {
        let cf_attr = CFString::new(attr);
        let cf_value = CFString::new(value);
        let err = unsafe {
            AXUIElementSetAttributeValue(
                self.raw,
                cf_attr.as_concrete_TypeRef(),
                cf_value.as_concrete_TypeRef() as CFTypeRef,
            )
        };
        err == K_AX_ERROR_SUCCESS
    }

    /// Set an attribute to a numeric value.
    pub fn set_number_attribute(&self, attr: &str, value: f64) -> bool {
        let cf_attr = CFString::new(attr);
        let cf_num = core_foundation::number::CFNumber::from(value);
        let err = unsafe {
            AXUIElementSetAttributeValue(
                self.raw,
                cf_attr.as_concrete_TypeRef(),
                cf_num.as_concrete_TypeRef() as CFTypeRef,
            )
        };
        err == K_AX_ERROR_SUCCESS
    }

    /// Get the PID of the application owning this element.
    pub fn pid(&self) -> Option<i32> {
        let mut pid: i32 = 0;
        let err = unsafe { AXUIElementGetPid(self.raw, &mut pid) };
        if err == K_AX_ERROR_SUCCESS {
            Some(pid)
        } else {
            None
        }
    }
}

// AXValue types for position/size
type AXValueRef = *const c_void;
const AX_VALUE_TYPE_CGPOINT: i32 = 1;
const AX_VALUE_TYPE_CGSIZE: i32 = 2;

extern "C" {
    fn AXValueGetValue(value: AXValueRef, value_type: i32, value_ptr: *mut c_void) -> bool;
}
