//! macOS AXUIElement-based accessibility provider.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use xa11y_core::{
    Action, ActionData, CancelHandle, ElementData, Error, Event, EventReceiver, EventType,
    Provider, RawPlatformData, Rect, Result, Role, StateSet, Subscription, Toggled,
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

extern "C" {
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

// ── Safe FFI Wrappers ────────────────────────────────────────────────────────

fn do_perform_action(element: AXUIElementRef, action: &CFString) -> i32 {
    unsafe { safe_ax_perform_action(element, action.as_concrete_TypeRef() as CFTypeRef) }
}

fn do_set_attribute(element: AXUIElementRef, attribute: &CFString, value: CFTypeRef) -> i32 {
    unsafe {
        safe_ax_set_attribute_value(element, attribute.as_concrete_TypeRef() as CFTypeRef, value)
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
        Action::ScrollDown
        | Action::ScrollRight
        | Action::Blur
        | Action::SetTextSelection
        | Action::TypeText => None,
        _ => None,
    }
}

// ── State Parsing ─────────────────────────────────────────────────────────────

fn parse_states(element: AXUIElementRef, role: Role) -> StateSet {
    let enabled = ax_bool(element, "AXEnabled").unwrap_or(true);
    let focused = ax_bool(element, "AXFocused").unwrap_or(false);
    let selected = ax_bool(element, "AXSelected").unwrap_or(false);

    let hidden = ax_bool(element, "AXHidden").unwrap_or(false);
    let visible = !hidden;

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

    let expanded = ax_bool(element, "AXExpanded");

    let editable = matches!(role, Role::TextField | Role::TextArea);

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
    fn build_element_data(&self, ax: &AXElement, pid: Option<u32>) -> ElementData {
        let role_str = ax_string(ax.as_ptr(), "AXRole").unwrap_or_default();
        let subrole_str = ax_string(ax.as_ptr(), "AXSubrole");
        let role = map_ax_role(&role_str, subrole_str.as_deref());

        let ax_title = ax_string(ax.as_ptr(), "AXTitle");
        let ax_description = ax_string(ax.as_ptr(), "AXDescription");

        // Name: prefer AXTitle, fall back to AXDescription only if no title
        let name = ax_title.or_else(|| {
            if role == Role::StaticText {
                ax_string(ax.as_ptr(), "AXValue")
            } else {
                ax_description.clone()
            }
        });

        // Description: AXHelp first, then AXDescription (if not already used as name)
        let description = ax_string(ax.as_ptr(), "AXHelp").or_else(|| {
            // Only use AXDescription for description if name didn't consume it
            if name.as_ref() != ax_description.as_ref() {
                ax_description
            } else {
                None
            }
        });

        let value = match role {
            Role::CheckBox | Role::RadioButton => None,
            _ => ax_value_string(ax.as_ptr()),
        };

        let states = parse_states(ax.as_ptr(), role);

        let bounds = match (ax_position(ax.as_ptr()), ax_size(ax.as_ptr())) {
            (Some((x, y)), Some((w, h))) if w > 0.0 || h > 0.0 => Some(Rect {
                x: x as i32,
                y: y as i32,
                width: w.max(0.0) as u32,
                height: h.max(0.0) as u32,
            }),
            _ => None,
        };

        let ax_actions = ax_action_names(ax.as_ptr());
        let mut actions: Vec<Action> = ax_actions.iter().filter_map(|a| map_ax_action(a)).collect();

        if ax_bool(ax.as_ptr(), "AXFocused").is_some() && !actions.contains(&Action::Focus) {
            actions.push(Action::Focus);
        }

        if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
            && !actions.contains(&Action::SetValue)
        {
            actions.push(Action::SetValue);
        }

        let ax_identifier = ax_string(ax.as_ptr(), "AXIdentifier");

        let raw = RawPlatformData::MacOS {
            ax_role: role_str,
            ax_subrole: subrole_str,
            ax_identifier: ax_identifier.clone(),
        };

        let numeric_value = match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton => ax_value_number(ax.as_ptr()),
            _ => None,
        };

        let (min_value, max_value) = match role {
            Role::Slider => (
                ax_number_f64(ax.as_ptr(), "AXMinValue"),
                ax_number_f64(ax.as_ptr(), "AXMaxValue"),
            ),
            _ => (None, None),
        };

        let handle = self.cache_element(ax.clone());

        ElementData {
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
            pid,
            handle,
        }
    }

    /// Should this child be filtered out (macOS system chrome)?
    fn should_filter_child(
        parent_role: Role,
        parent_name: Option<&str>,
        child: &AXElement,
    ) -> bool {
        let child_role = ax_string(child.as_ptr(), "AXRole").unwrap_or_default();

        if parent_role == Role::Application && child_role == "AXMenuBar" {
            return true;
        }

        if parent_role == Role::Window {
            let child_subrole = ax_string(child.as_ptr(), "AXSubrole").unwrap_or_default();
            if matches!(
                child_subrole.as_str(),
                "AXCloseButton" | "AXMinimizeButton" | "AXFullScreenButton" | "AXZoomButton"
            ) {
                return true;
            }
            if child_role == "AXStaticText" {
                let child_sr = ax_string(child.as_ptr(), "AXSubrole").unwrap_or_default();
                if child_sr.is_empty() || child_sr == "AXUnknown" {
                    if let Some(v) = ax_string(child.as_ptr(), "AXValue") {
                        if let Some(win_name) = parent_name {
                            if v == win_name {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        false
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

                let mut results = Vec::new();
                for child in &ax_children_list {
                    if Self::should_filter_child(role, name, child) {
                        continue;
                    }
                    let data = self.build_element_data(child, element_data.pid);
                    results.push(data);
                }
                Ok(results)
            }
        }
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

            Action::ScrollIntoView => Ok(()),

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

            Action::ScrollDown => {
                let amount = match data {
                    Some(ActionData::ScrollAmount(amount)) => amount,
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "ScrollDown requires ActionData::ScrollAmount".to_string(),
                        })
                    }
                };
                let dy = -(amount * 10.0) as i32;
                unsafe { safe_cg_post_scroll_event(dy, 0) };
                Ok(())
            }

            Action::ScrollRight => {
                let amount = match data {
                    Some(ActionData::ScrollAmount(amount)) => amount,
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "ScrollRight requires ActionData::ScrollAmount".to_string(),
                        })
                    }
                };
                let dx = -(amount * 10.0) as i32;
                unsafe { safe_cg_post_scroll_event(0, dx) };
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
        Some(ElementData {
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
            pid: None,
            handle: 0,
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
}
