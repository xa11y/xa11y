//! macOS AXUIElement-based accessibility provider.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use xa11y_core::{
    Action, ActionData, AppTarget, CancelHandle, ElementState, Error, Event, EventFilter,
    EventKind, EventProvider, EventReceiver, NodeData, PermissionStatus, Provider, RawPlatformData,
    Rect, Result, Role, ScrollDirection, StateSet, Subscription, Toggled, Tree,
};

// ── FFI Declarations ──────────────────────────────────────────────────────────

type AXUIElementRef = *const c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFIndex = isize;

const AX_ERROR_SUCCESS: i32 = 0;
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
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
}

// ── ObjC Exception-Safe Wrappers (from exception_safe.m) ─────────────────────
//
// ALL CF/AX operations that touch accessibility objects go through these
// C wrappers which use @try/@catch to prevent ObjC exceptions from unwinding
// through Rust frames.

extern "C" {
    // AX API wrappers
    fn safe_ax_copy_attribute_value(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
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
    // AXObserver wrappers for EventProvider
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

    // CGEvent wrappers for input simulation
    fn safe_cg_post_scroll_event(dy: i32, dx: i32);
    fn safe_ax_value_create_cf_range(location: isize, length: isize) -> CFTypeRef;

    // Test helpers
    #[cfg(test)]
    fn test_throw_and_catch_nsexception() -> i32;
}

// ── AXElement RAII Wrapper ────────────────────────────────────────────────────

struct AXElement(AXUIElementRef);

unsafe impl Send for AXElement {}
unsafe impl Sync for AXElement {}

impl AXElement {
    /// Takes ownership of an already-retained ref (e.g. from Create functions).
    fn from_owned(ptr: AXUIElementRef) -> Self {
        Self(ptr)
    }

    /// Retains the ref (e.g. from array element access).
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

// ── Attribute Helpers ─────────────────────────────────────────────────────────

fn ax_attr(element: AXUIElementRef, attribute: &str) -> Option<CFTypeRef> {
    let attr = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        safe_ax_copy_attribute_value(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value)
    };
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

fn ax_action_names(element: AXUIElementRef) -> Vec<String> {
    let mut names: CFArrayRef = std::ptr::null();
    let err = unsafe { safe_ax_copy_action_names(element, &mut names) };
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

/// Get value as string, handling both string and numeric AXValue.
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

/// Get numeric value from AXValue attribute.
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
        CFRelease(value);
        None
    }
}

/// Get integer value from AXValue attribute (for checkbox state).
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

// ── Safe FFI Wrappers ────────────────────────────────────────────────────────

/// Perform an AX action, catching ObjC exceptions via the C wrapper.
fn do_perform_action(element: AXUIElementRef, action: &CFString) -> i32 {
    unsafe { safe_ax_perform_action(element, action.as_concrete_TypeRef() as CFTypeRef) }
}

/// Set an AX attribute value, catching ObjC exceptions via the C wrapper.
fn do_set_attribute(element: AXUIElementRef, attribute: &CFString, value: CFTypeRef) -> i32 {
    unsafe {
        safe_ax_set_attribute_value(element, attribute.as_concrete_TypeRef() as CFTypeRef, value)
    }
}

// ── Role Mapping ──────────────────────────────────────────────────────────────

fn map_ax_role(role: &str, subrole: Option<&str>) -> Role {
    // Check subrole first for more specific mappings
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
        "AXGroup" | "AXScrollArea" | "AXLayoutArea" | "AXRadioGroup" => Role::Group,
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
        "AXColorWell" | "AXValueIndicator" | "AXGrid" | "AXRuler" | "AXGrowArea" | "AXMatte"
        | "AXDockItem" | "AXBrowser" => Role::Unknown,
        _ => Role::Unknown,
    }
}

fn map_ax_action(name: &str) -> Option<Action> {
    match name {
        "AXPress" | "AXConfirm" => Some(Action::Press),
        "AXRaise" => None,
        "AXShowMenu" => Some(Action::ShowMenu),
        "AXIncrement" => Some(Action::Increment),
        "AXDecrement" => Some(Action::Decrement),
        "AXCancel" => None,
        _ => None,
    }
}

#[allow(dead_code)]
fn xa11y_action_to_ax(action: Action) -> Option<&'static str> {
    match action {
        Action::Press | Action::Toggle | Action::Select => Some("AXPress"),
        Action::ShowMenu => Some("AXShowMenu"),
        Action::Increment => Some("AXIncrement"),
        Action::Decrement => Some("AXDecrement"),
        Action::Scroll | Action::Blur | Action::SetTextSelection | Action::TypeText => None,
        _ => None,
    }
}

// ── State Parsing ─────────────────────────────────────────────────────────────

fn parse_states(element: AXUIElementRef, role: Role) -> StateSet {
    let enabled = ax_bool(element, "AXEnabled").unwrap_or(true);
    let focused = ax_bool(element, "AXFocused").unwrap_or(false);
    let selected = ax_bool(element, "AXSelected").unwrap_or(false);

    // Visibility: element is visible unless explicitly hidden
    let hidden = ax_bool(element, "AXHidden").unwrap_or(false);
    let visible = !hidden;

    // Checked state: for checkboxes and radio buttons, AXValue is 0/1/2
    // AccessKit may expose as integer or boolean
    let checked = match role {
        Role::CheckBox | Role::RadioButton => {
            if let Some(i) = ax_value_int(element) {
                match i {
                    0 => Some(Toggled::Off),
                    1 => Some(Toggled::On),
                    2 => Some(Toggled::Mixed),
                    _ => Some(Toggled::Off),
                }
            } else if let Some(b) = ax_bool(element, "AXValue") {
                Some(if b { Toggled::On } else { Toggled::Off })
            } else {
                // Also try reading AXValue as float (accesskit sometimes uses CFNumber)
                let value = ax_attr(element, "AXValue");
                if let Some(v) = value {
                    let mut f: f64 = 0.0;
                    let ok = unsafe {
                        CFNumberGetValue(v, CF_NUMBER_FLOAT64, &mut f as *mut _ as *mut c_void)
                    };
                    unsafe { CFRelease(v) };
                    if ok {
                        Some(if f > 0.5 { Toggled::On } else { Toggled::Off })
                    } else {
                        Some(Toggled::Off)
                    }
                } else {
                    Some(Toggled::Off)
                }
            }
        }
        _ => None,
    };

    // Expanded: only present on expandable elements
    let expanded = ax_bool(element, "AXExpanded");

    // Editable: text fields without read-only are editable
    let editable = match role {
        Role::TextField | Role::TextArea => {
            // If the element has an AXValue that's settable, it's editable
            // Approximation: text fields are editable unless in a read-only context
            true
        }
        _ => false,
    };

    // Focusable: interactive roles that can receive keyboard focus
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
    ) || ax_bool(element, "AXFocused").is_some();

    let modal = ax_bool(element, "AXModal").unwrap_or(false);

    StateSet {
        enabled,
        visible,
        focused,
        focusable,
        modal,
        checked,
        selected,
        expanded,
        editable,
        required: false,
        busy: false,
    }
}

// ── MacOS Provider ────────────────────────────────────────────────────────────

pub struct MacOSProvider {
    /// Cached AXElement refs from the most recent tree build.
    /// Index corresponds to the node's DFS index.
    cached_elements: Mutex<Vec<AXElement>>,
}

impl MacOSProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cached_elements: Mutex::new(Vec::new()),
        })
    }

    fn detect_screen_size() -> (u32, u32) {
        let display = unsafe { CGMainDisplayID() };
        let w = unsafe { CGDisplayPixelsWide(display) } as u32;
        let h = unsafe { CGDisplayPixelsHigh(display) } as u32;
        if w == 0 || h == 0 {
            (1920, 1080)
        } else {
            (w, h)
        }
    }

    /// List running GUI apps using CGWindowListCopyWindowInfo.
    fn list_gui_apps() -> Vec<(i32, String)> {
        let info = unsafe { safe_cg_window_list_copy(0, 0) }; // kCGWindowListOptionAll
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

    fn find_app_by_name(&self, name: &str) -> Result<(AXElement, i32, String)> {
        let name_lower = name.to_lowercase();
        let apps = Self::list_gui_apps();

        for (pid, app_name) in &apps {
            if app_name.to_lowercase().contains(&name_lower) {
                let element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
                if element.is_null() {
                    continue;
                }
                return Ok((element, *pid, app_name.clone()));
            }
        }

        Err(Error::AppNotFound {
            target: name.to_string(),
        })
    }

    fn find_app_by_pid(&self, pid: u32) -> Result<(AXElement, String)> {
        let element = AXElement::from_owned(unsafe { safe_ax_create_application(pid as i32) });
        if element.is_null() {
            return Err(Error::AppNotFound {
                target: format!("PID {}", pid),
            });
        }

        // Try to get app name
        let name = ax_string(element.as_ptr(), "AXTitle")
            .or_else(|| {
                // Fall back to window list name
                let apps = Self::list_gui_apps();
                apps.into_iter()
                    .find(|(p, _)| *p == pid as i32)
                    .map(|(_, n)| n)
            })
            .unwrap_or_default();

        Ok((element, name))
    }

    /// Recursively traverse the AX tree, building xa11y nodes.
    /// Hard depth limit of 50 to prevent stack overflow from circular AX trees.
    #[allow(clippy::too_many_arguments)]
    fn traverse(
        &self,
        element: &AXElement,
        nodes: &mut Vec<NodeData>,
        elements: &mut Vec<AXElement>,
        parent_idx: Option<u32>,
        depth: u32,
        screen_size: (u32, u32),
        visited: &mut HashSet<usize>,
    ) {
        // Hard depth limit to prevent stack overflow
        const MAX_DEPTH: u32 = 50;
        if depth > MAX_DEPTH {
            return;
        }

        // Cycle detection using element pointer as identity
        let ptr_key = element.as_ptr() as usize;
        if !visited.insert(ptr_key) {
            return;
        }

        let role_str = ax_string(element.as_ptr(), "AXRole").unwrap_or_default();
        let subrole_str = ax_string(element.as_ptr(), "AXSubrole");
        let role = map_ax_role(&role_str, subrole_str.as_deref());

        let name = ax_string(element.as_ptr(), "AXTitle")
            .or_else(|| ax_string(element.as_ptr(), "AXDescription"))
            .or_else(|| {
                // For static text, the "value" IS the name
                if role == Role::StaticText {
                    ax_string(element.as_ptr(), "AXValue")
                } else {
                    None
                }
            });

        let description = ax_string(element.as_ptr(), "AXHelp");

        // Value depends on role
        let value = match role {
            Role::CheckBox | Role::RadioButton => None, // checked state handled separately
            _ => ax_value_string(element.as_ptr()),
        };

        let states = parse_states(element.as_ptr(), role);

        // Bounds
        let bounds = match (ax_position(element.as_ptr()), ax_size(element.as_ptr())) {
            (Some((x, y)), Some((w, h))) if w > 0.0 || h > 0.0 => Some(Rect {
                x: x as i32,
                y: y as i32,
                width: w.max(0.0) as u32,
                height: h.max(0.0) as u32,
            }),
            _ => None,
        };

        // Actions
        let ax_actions = ax_action_names(element.as_ptr());
        let mut actions: Vec<Action> = ax_actions.iter().filter_map(|a| map_ax_action(a)).collect();

        // Add Focus if the element can be focused
        if ax_bool(element.as_ptr(), "AXFocused").is_some() && !actions.contains(&Action::Focus) {
            actions.push(Action::Focus);
        }

        // Add SetValue for text fields and sliders
        if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
            && !actions.contains(&Action::SetValue)
        {
            actions.push(Action::SetValue);
        }

        // Stable ID: AXIdentifier (always captured for cross-snapshot correlation)
        let ax_identifier = ax_string(element.as_ptr(), "AXIdentifier");

        let raw = RawPlatformData::MacOS {
            ax_role: role_str,
            ax_subrole: subrole_str,
            ax_identifier: ax_identifier.clone(),
        };

        // Numeric value for range controls
        let numeric_value = match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton => {
                ax_value_number(element.as_ptr())
            }
            _ => None,
        };

        // Min/Max values for sliders
        let (min_value, max_value) = match role {
            Role::Slider => (
                ax_number_f64(element.as_ptr(), "AXMinValue"),
                ax_number_f64(element.as_ptr(), "AXMaxValue"),
            ),
            _ => (None, None),
        };

        let node_idx = nodes.len() as u32;
        let name_ref = name.clone(); // keep for window chrome filter below
        nodes.push(NodeData {
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            stable_id: ax_identifier,
            numeric_value,
            min_value,
            max_value,
            raw,
            index: node_idx,
            children_indices: vec![], // filled below
            parent_index: parent_idx,
            pid: None,
        });
        elements.push(element.clone());

        // Recurse children (skip macOS system menu bar at app level —
        // it adds 100+ nodes that aren't part of the app's accessibility tree)
        let children = ax_children(element.as_ptr());
        let mut child_ids = Vec::new();

        for child in &children {
            if let Some(max_elements) = opts.max_elements {
                if nodes.len() >= max_elements as usize {
                    break;
                }
            }
            // Skip macOS system chrome (menu bar, window buttons, title bar text).
            // These are added by macOS, not by the app's accesskit tree.
            if role == Role::Application {
                let child_role = ax_string(child.as_ptr(), "AXRole").unwrap_or_default();
                if child_role == "AXMenuBar" {
                    continue;
                }
            }
            if role == Role::Window {
                let child_subrole = ax_string(child.as_ptr(), "AXSubrole").unwrap_or_default();
                if matches!(
                    child_subrole.as_str(),
                    "AXCloseButton" | "AXMinimizeButton" | "AXFullScreenButton" | "AXZoomButton"
                ) {
                    continue;
                }
                // Skip the title bar static text added by macOS
                let child_role = ax_string(child.as_ptr(), "AXRole").unwrap_or_default();
                if child_role == "AXStaticText" {
                    let child_sr = ax_string(child.as_ptr(), "AXSubrole").unwrap_or_default();
                    if child_sr.is_empty() || child_sr == "AXUnknown" {
                        // Check if it has the window title as value — that's the title bar text
                        if let Some(v) = ax_string(child.as_ptr(), "AXValue") {
                            if let Some(ref win_name) = name_ref {
                                if v == *win_name {
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            let child_idx = nodes.len() as u32;
            child_ids.push(child_idx);
            self.traverse(
                child,
                nodes,
                elements,
                Some(node_idx),
                depth + 1,
                screen_size,
                visited,
            );
        }

        nodes[node_idx as usize].children_indices = child_ids;
    }
}

impl Provider for MacOSProvider {
    fn get_app_tree(&self, target: &AppTarget) -> Result<Tree> {
        let (app_element, pid, app_name) = match target {
            AppTarget::ByName(name) => self.find_app_by_name(name)?,
            AppTarget::ByPid(pid) => {
                let (el, name) = self.find_app_by_pid(*pid)?;
                (el, *pid as i32, name)
            }
            AppTarget::ByWindow(handle) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("ByWindow not yet supported: {:?}", handle),
                });
            }
        };

        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut elements = Vec::new();

        let mut visited = HashSet::new();
        self.traverse(
            &app_element,
            &mut nodes,
            &mut elements,
            None,
            0,
            screen_size,
            &mut visited,
        );

        if nodes.is_empty() {
            return Err(Error::AppNotFound {
                target: format!("{:?}", target),
            });
        }

        // Cache elements for action dispatch
        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new(app_name, Some(pid as u32), screen_size, nodes))
    }

    fn get_apps(&self) -> Result<Tree> {
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut elements = Vec::new();

        // Desktop root
        nodes.push(NodeData {
            index: 0,
            role: Role::Application,
            name: Some("Desktop".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: screen_size.0,
                height: screen_size.1,
            }),
            actions: vec![],
            states: StateSet::default(),
            children_indices: vec![],
            parent_index: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            pid: None,
        });
        elements.push(AXElement(std::ptr::null())); // placeholder

        let apps = Self::list_gui_apps();
        let mut root_children = Vec::new();

        let mut visited = HashSet::new();
        for (pid, _app_name) in &apps {
            let app_element = AXElement::from_owned(unsafe { safe_ax_create_application(*pid) });
            if app_element.is_null() {
                continue;
            }
            let child_idx = nodes.len() as u32;
            root_children.push(child_idx);
            self.traverse(
                &app_element,
                &mut nodes,
                &mut elements,
                Some(0),
                1,
                screen_size,
                &mut visited,
            );
        }

        nodes[0].children_indices = root_children;

        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new("Desktop".to_string(), None, screen_size, nodes))
    }

    fn perform_action(
        &self,
        tree: &Tree,
        node: &NodeData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let node_idx = tree.node_index(node);

        // Look up cached element
        let cache = self.cached_elements.lock().unwrap();
        let element = cache.get(node_idx as usize).ok_or(Error::ElementStale {
            selector: format!("index:{}", node_idx),
        })?;
        if element.is_null() {
            return Err(Error::ElementStale {
                selector: format!("index:{}", node_idx),
            });
        }
        let el_ptr = element.as_ptr();

        match action {
            Action::Press | Action::Toggle | Action::Select => {
                let ax_action = CFString::new("AXPress");
                let err = do_perform_action(el_ptr, &ax_action);
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "AXPress failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::Focus => {
                let attr = CFString::new("AXFocused");
                let val = core_foundation::boolean::CFBoolean::true_value();
                let err = do_set_attribute(el_ptr, &attr, val.as_CFTypeRef());
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "Set AXFocused failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::SetValue => match data {
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
                    let err =
                        do_set_attribute(el_ptr, &attr, val.as_concrete_TypeRef() as CFTypeRef);
                    if err != AX_ERROR_SUCCESS {
                        return Err(Error::TextValueNotSupported);
                    }
                    Ok(())
                }
                _ => Err(Error::Platform {
                    code: -1,
                    message: "SetValue requires ActionData".to_string(),
                }),
            },

            Action::Expand => {
                let attr = CFString::new("AXExpanded");
                let val = core_foundation::boolean::CFBoolean::true_value();
                let err = do_set_attribute(el_ptr, &attr, val.as_CFTypeRef());
                if err != AX_ERROR_SUCCESS {
                    let press = CFString::new("AXPress");
                    let _ = do_perform_action(el_ptr, &press);
                }
                Ok(())
            }

            Action::Collapse => {
                let attr = CFString::new("AXExpanded");
                let val = core_foundation::boolean::CFBoolean::false_value();
                let err = do_set_attribute(el_ptr, &attr, val.as_CFTypeRef());
                if err != AX_ERROR_SUCCESS {
                    let press = CFString::new("AXPress");
                    let _ = do_perform_action(el_ptr, &press);
                }
                Ok(())
            }

            Action::Increment => {
                let ax_action = CFString::new("AXIncrement");
                let err = do_perform_action(el_ptr, &ax_action);
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "AXIncrement failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::Decrement => {
                let ax_action = CFString::new("AXDecrement");
                let err = do_perform_action(el_ptr, &ax_action);
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "AXDecrement failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::ShowMenu => {
                let ax_action = CFString::new("AXShowMenu");
                let err = do_perform_action(el_ptr, &ax_action);
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "AXShowMenu failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::ScrollIntoView => {
                // No direct AX equivalent; no-op
                Ok(())
            }

            Action::Blur => {
                let attr = CFString::new("AXFocused");
                let val = core_foundation::boolean::CFBoolean::false_value();
                let err = do_set_attribute(el_ptr, &attr, val.as_CFTypeRef());
                if err != AX_ERROR_SUCCESS {
                    return Err(Error::Platform {
                        code: err as i64,
                        message: "Set AXFocused=false failed".to_string(),
                    });
                }
                Ok(())
            }

            Action::Scroll => {
                let (direction, amount) = match data {
                    Some(ActionData::ScrollAmount { direction, amount }) => (direction, amount),
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "Scroll requires ActionData::ScrollAmount".to_string(),
                        })
                    }
                };
                // 1 logical scroll unit ≈ 10 pixels
                let pixels = (amount * 10.0) as i32;
                let (dy, dx) = match direction {
                    ScrollDirection::Up => (pixels, 0),
                    ScrollDirection::Down => (-pixels, 0),
                    ScrollDirection::Left => (0, pixels),
                    ScrollDirection::Right => (0, -pixels),
                };
                unsafe { safe_cg_post_scroll_event(dy, dx) };
                Ok(())
            }

            Action::SetTextSelection => {
                let (start, end) = match data {
                    Some(ActionData::TextSelection { start, end }) => (start, end),
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "SetTextSelection requires ActionData::TextSelection"
                                .to_string(),
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

            Action::TypeText => {
                let text = match data {
                    Some(ActionData::Value(text)) => text,
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "TypeText requires ActionData::Value".to_string(),
                        })
                    }
                };
                // Insert text via AXSelectedText — replaces current selection
                // (or inserts at cursor if selection is zero-length).
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
        }
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        if unsafe { AXIsProcessTrusted() } {
            Ok(PermissionStatus::Granted)
        } else {
            Ok(PermissionStatus::Denied {
                instructions:
                    "Enable Accessibility in System Settings → Privacy & Security → Accessibility"
                        .to_string(),
            })
        }
    }
}

// ── EventProvider ────────────────────────────────────────────────────────────

/// Context passed to AXObserver callback via refcon pointer.
struct ObserverContext {
    sender: std::sync::mpsc::Sender<Event>,
    filter: EventFilter,
    app_name: String,
    app_pid: u32,
}

/// AXObserver callback: maps AX notifications to xa11y events and sends them.
unsafe extern "C" fn ax_observer_callback(
    _observer: CFTypeRef,
    element: AXUIElementRef,
    notification: CFTypeRef, // CFStringRef
    refcon: *mut c_void,
) {
    let ctx = &*(refcon as *const ObserverContext);

    let notif_str = {
        let cf = CFString::wrap_under_get_rule(notification as *const _);
        cf.to_string()
    };

    let kind = match notif_str.as_str() {
        "AXValueChanged" => EventKind::ValueChanged,
        "AXFocusedUIElementChanged" => EventKind::FocusChanged,
        "AXWindowCreated" => EventKind::WindowOpened,
        "AXWindowMiniaturized" => EventKind::WindowDeactivated,
        "AXWindowDeminiaturized" => EventKind::WindowActivated,
        "AXUIElementDestroyed" => EventKind::StructureChanged,
        "AXSelectedTextChanged" => EventKind::SelectionChanged,
        "AXMenuOpened" => EventKind::MenuOpened,
        "AXMenuClosed" => EventKind::MenuClosed,
        "AXTitleChanged" => EventKind::NameChanged,
        _ => return,
    };

    if !ctx.filter.kinds.is_empty() && !ctx.filter.kinds.contains(&kind) {
        return;
    }

    // Build minimal target node from the AX element
    let target = if !element.is_null() {
        let role_str = ax_string(element, "AXRole").unwrap_or_default();
        let subrole = ax_string(element, "AXSubrole");
        let role = map_ax_role(&role_str, subrole.as_deref());
        Some(NodeData {
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
            raw: RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![],
            parent_index: None,
            pid: None,
        })
    } else {
        None
    };

    let event = Event {
        kind,
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

impl EventProvider for MacOSProvider {
    fn subscribe(&self, target: &AppTarget, filter: EventFilter) -> Result<Subscription> {
        let (pid, app_name) = match target {
            AppTarget::ByName(name) => {
                let apps = Self::list_gui_apps();
                let found = apps
                    .iter()
                    .find(|(_, n)| n.to_lowercase().contains(&name.to_lowercase()));
                match found {
                    Some((pid, name)) => (*pid, name.clone()),
                    None => {
                        return Err(Error::AppNotFound {
                            target: name.clone(),
                        })
                    }
                }
            }
            AppTarget::ByPid(pid) => (*pid as i32, String::new()),
            AppTarget::ByWindow(_) => {
                return Err(Error::Platform {
                    code: -1,
                    message: "ByWindow not supported for event subscription".to_string(),
                })
            }
        };

        let (tx, rx) = std::sync::mpsc::channel();

        let ctx = Box::new(ObserverContext {
            sender: tx,
            filter,
            app_name,
            app_pid: pid as u32,
        });
        let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

        // Create AXObserver
        let mut observer: CFTypeRef = std::ptr::null();
        let err = unsafe { safe_ax_observer_create(pid, ax_observer_callback, &mut observer) };
        if err != AX_ERROR_SUCCESS || observer.is_null() {
            unsafe { drop(Box::from_raw(ctx_ptr as *mut ObserverContext)) };
            return Err(Error::Platform {
                code: err as i64,
                message: "AXObserverCreate failed".to_string(),
            });
        }

        // Create app element and add notifications
        let app_element = unsafe { safe_ax_create_application(pid) };
        if app_element.is_null() {
            unsafe {
                CFRelease(observer);
                drop(Box::from_raw(ctx_ptr as *mut ObserverContext));
            }
            return Err(Error::AppNotFound {
                target: format!("pid:{}", pid),
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

        // Spawn background RunLoop thread — communicates RunLoop ref via channel.
        // Use usize casts to make raw pointers Send-safe across threads.
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
                safe_cf_run_loop_run(); // blocks until CFRunLoopStop
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

    fn wait_for_event(
        &self,
        target: &AppTarget,
        filter: EventFilter,
        timeout: Duration,
    ) -> Result<Event> {
        let sub = self.subscribe(target, filter)?;
        let start = std::time::Instant::now();
        loop {
            if let Some(event) = sub.try_recv() {
                return Ok(event);
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for(
        &self,
        target: &AppTarget,
        selector: &str,
        state: ElementState,
        timeout: Duration,
    ) -> Result<NodeData> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.get_app_tree(target)?;
            let matches = tree.query(selector).ok();
            let node = matches.as_ref().and_then(|m| m.first().copied());

            if state.is_met(node) {
                return Ok(node.cloned().unwrap_or_else(NodeData::synthetic_empty));
            }

            std::thread::sleep(poll_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objc_exception_is_caught_by_c_wrapper() {
        // The C test helper throws an NSException inside @try/@catch.
        // Verifies the ObjC exception safety mechanism works end-to-end.
        let result = unsafe { test_throw_and_catch_nsexception() };
        assert_eq!(result, 1, "C wrapper should have caught the NSException");
    }

    #[test]
    fn safe_ax_copy_attribute_returns_error_on_null_element() {
        // Calling the safe wrapper with null element should return error, not crash.
        let attr = CFString::new("AXRole");
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe {
            safe_ax_copy_attribute_value(
                std::ptr::null(),
                attr.as_concrete_TypeRef() as CFTypeRef,
                &mut value,
            )
        };
        // Should return an error code (not 0/success) and not crash
        assert_ne!(err, AX_ERROR_SUCCESS);
    }

    #[test]
    fn ax_attr_returns_none_for_null_element() {
        // A null element should not crash — should return None gracefully
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
        // Verify subrole precedence
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

        // Verify main role mappings
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
        assert_eq!(map_ax_role("AXValueIndicator", None), Role::Unknown);
        assert_eq!(map_ax_role("TotallyUnknownRole", None), Role::Unknown);
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
        assert_eq!(xa11y_action_to_ax(Action::Press), Some("AXPress"));
        assert_eq!(xa11y_action_to_ax(Action::Toggle), Some("AXPress"));
        assert_eq!(xa11y_action_to_ax(Action::Select), Some("AXPress"));
        assert_eq!(xa11y_action_to_ax(Action::ShowMenu), Some("AXShowMenu"));
        assert_eq!(xa11y_action_to_ax(Action::Increment), Some("AXIncrement"));
        assert_eq!(xa11y_action_to_ax(Action::Decrement), Some("AXDecrement"));
        assert_eq!(xa11y_action_to_ax(Action::Focus), None);
        assert_eq!(xa11y_action_to_ax(Action::SetValue), None);
        assert_eq!(xa11y_action_to_ax(Action::Expand), None);
        assert_eq!(xa11y_action_to_ax(Action::Collapse), None);
        assert_eq!(xa11y_action_to_ax(Action::ScrollIntoView), None);
    }

    #[test]
    fn provider_new_succeeds() {
        let provider = MacOSProvider::new();
        assert!(provider.is_ok());
    }

    #[test]
    fn detect_screen_size_returns_nonzero() {
        let (w, h) = MacOSProvider::detect_screen_size();
        assert!(w > 0);
        assert!(h > 0);
    }
}
