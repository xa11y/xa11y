//! macOS AXUIElement-based accessibility provider.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, Error, Node, NodeId, NormalizedRect, PermissionStatus,
    Provider, QueryOptions, RawPlatformData, Rect, Result, Role, StateSet, Toggled, Tree,
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
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementCopyActionNames(element: AXUIElementRef, names: *mut CFArrayRef) -> i32;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    fn AXValueGetValue(value: CFTypeRef, the_type: i32, value_ptr: *mut c_void) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
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
        AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef() as CFTypeRef, &mut value)
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
            let ok = CFNumberGetValue(value, CF_NUMBER_FLOAT64, &mut result as *mut _ as *mut c_void);
            CFRelease(value);
            if ok { Some(result) } else { None }
        } else {
            CFRelease(value);
            None
        }
    }
}

fn ax_number_i32(element: AXUIElementRef, attribute: &str) -> Option<i32> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut result: i32 = 0;
            let ok = CFNumberGetValue(value, CF_NUMBER_SINT32, &mut result as *mut _ as *mut c_void);
            CFRelease(value);
            if ok { Some(result) } else { None }
        } else {
            CFRelease(value);
            None
        }
    }
}

fn ax_number_i64(element: AXUIElementRef, attribute: &str) -> Option<i64> {
    let value = ax_attr(element, attribute)?;
    unsafe {
        if CFGetTypeID(value) == CFNumberGetTypeID() {
            let mut result: i64 = 0;
            let ok = CFNumberGetValue(value, CF_NUMBER_SINT64, &mut result as *mut _ as *mut c_void);
            CFRelease(value);
            if ok { Some(result) } else { None }
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
    let err = unsafe { AXUIElementCopyActionNames(element, &mut names) };
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
        AXValueGetValue(value, AX_VALUE_CGPOINT, &mut point as *mut _ as *mut c_void)
    };
    unsafe { CFRelease(value) };
    if ok { Some((point.x, point.y)) } else { None }
}

fn ax_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    let value = ax_attr(element, "AXSize")?;
    let mut size = CGSize::default();
    let ok = unsafe {
        AXValueGetValue(value, AX_VALUE_CGSIZE, &mut size as *mut _ as *mut c_void)
    };
    unsafe { CFRelease(value) };
    if ok { Some((size.width, size.height)) } else { None }
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
            if ok { return Some(f); }
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
            if ok { return Some(i); }
        }
        CFRelease(value);
        None
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
        _ => {}
    }

    match role {
        "AXApplication" => Role::Application,
        "AXWindow" | "AXSheet" | "AXDrawer" => {
            if role == "AXSheet" { Role::Dialog } else { Role::Window }
        }
        "AXButton" => {
            match subrole {
                Some("AXDisclosureTriangle") => Role::TreeItem,
                _ => Role::Button,
            }
        }
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
        "AXIncrementor" => Role::TextField, // spin button
        "AXColorWell" | "AXValueIndicator" | "AXGrid" | "AXRuler"
        | "AXGrowArea" | "AXMatte" | "AXDockItem" | "AXBrowser" => Role::Unknown,
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

fn xa11y_action_to_ax(action: Action) -> Option<&'static str> {
    match action {
        Action::Press | Action::Toggle | Action::Select => Some("AXPress"),
        Action::ShowMenu => Some("AXShowMenu"),
        Action::Increment => Some("AXIncrement"),
        Action::Decrement => Some("AXDecrement"),
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

    StateSet {
        enabled,
        visible,
        focused,
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
    next_tree_id: Mutex<u64>,
    /// Cached AXElement refs from the most recent tree build.
    /// Index corresponds to NodeId.
    cached_elements: Mutex<Vec<AXElement>>,
}

impl MacOSProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            next_tree_id: Mutex::new(1),
            cached_elements: Mutex::new(Vec::new()),
        })
    }

    fn next_tree_id(&self) -> u64 {
        let mut id = self.next_tree_id.lock().unwrap();
        let current = *id;
        *id += 1;
        current
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
        let info = unsafe { CGWindowListCopyWindowInfo(0, 0) }; // kCGWindowListOptionAll
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

                let pid_val = CFDictionaryGetValue(dict, pid_key.as_concrete_TypeRef() as CFTypeRef);
                let name_val = CFDictionaryGetValue(dict, name_key.as_concrete_TypeRef() as CFTypeRef);

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
                let element = AXElement::from_owned(unsafe { AXUIElementCreateApplication(*pid) });
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
        let element = AXElement::from_owned(unsafe { AXUIElementCreateApplication(pid as i32) });
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
        opts: &QueryOptions,
        app_name: &str,
        nodes: &mut Vec<Node>,
        elements: &mut Vec<AXElement>,
        parent_id: Option<NodeId>,
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

        if let Some(max_depth) = opts.max_depth {
            if depth > max_depth {
                return;
            }
        }
        if let Some(max_elements) = opts.max_elements {
            if nodes.len() >= max_elements as usize {
                return;
            }
        }

        let role_str = ax_string(element.as_ptr(), "AXRole").unwrap_or_default();
        let subrole_str = ax_string(element.as_ptr(), "AXSubrole");
        let role = map_ax_role(&role_str, subrole_str.as_deref());

        let role_filtered = if let Some(ref filter_roles) = opts.roles {
            !filter_roles.contains(&role)
        } else {
            false
        };

        // If role is filtered out, skip this node but still recurse into children
        if role_filtered {
            let children = ax_children(element.as_ptr());
            for child in &children {
                if let Some(max_elements) = opts.max_elements {
                    if nodes.len() >= max_elements as usize {
                        break;
                    }
                }
                self.traverse(child, opts, app_name, nodes, elements, parent_id, depth, screen_size, visited);
            }
            return;
        }

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

        if opts.visible_only && !states.visible {
            // Skip invisible node but still recurse children (they may be visible)
            let children = ax_children(element.as_ptr());
            for child in &children {
                self.traverse(child, opts, app_name, nodes, elements, parent_id, depth, screen_size, visited);
            }
            return;
        }

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

        let bounds_normalized = bounds.map(|b| {
            let (sw, sh) = screen_size;
            if sw == 0 || sh == 0 {
                return NormalizedRect {
                    left: 0.0, top: 0.0, right: 0.0, bottom: 0.0,
                };
            }
            NormalizedRect {
                left: b.x as f64 / sw as f64,
                top: b.y as f64 / sh as f64,
                right: (b.x as f64 + b.width as f64) / sw as f64,
                bottom: (b.y as f64 + b.height as f64) / sh as f64,
            }
        });

        // Actions
        let ax_actions = ax_action_names(element.as_ptr());
        let mut actions: Vec<Action> = ax_actions
            .iter()
            .filter_map(|a| map_ax_action(a))
            .collect();

        // Add Focus if the element can be focused
        if ax_bool(element.as_ptr(), "AXFocused").is_some() && !actions.contains(&Action::Focus) {
            actions.push(Action::Focus);
        }

        // Add SetValue for text fields and sliders
        if matches!(role, Role::TextField | Role::TextArea | Role::Slider) {
            if !actions.contains(&Action::SetValue) {
                actions.push(Action::SetValue);
            }
        }

        // Raw platform data
        let raw = if opts.include_raw {
            Some(RawPlatformData::MacOS {
                ax_role: role_str,
                ax_subrole: subrole_str,
                ax_identifier: ax_string(element.as_ptr(), "AXIdentifier"),
            })
        } else {
            None
        };

        let node_id = nodes.len() as NodeId;
        let name_ref = name.clone(); // keep for window chrome filter below
        nodes.push(Node {
            id: node_id,
            role,
            name,
            value,
            description,
            bounds,
            bounds_normalized,
            actions,
            states,
            children: vec![], // filled below
            parent: parent_id,
            depth,
            app_name: Some(app_name.to_string()),
            raw,
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
                if matches!(child_subrole.as_str(),
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
            let child_node_id = nodes.len() as NodeId;
            child_ids.push(child_node_id);
            self.traverse(child, opts, app_name, nodes, elements, Some(node_id), depth + 1, screen_size, visited);
        }

        nodes[node_id as usize].children = child_ids;
    }
}

impl Provider for MacOSProvider {
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
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
            opts,
            &app_name,
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

        Ok(Tree::new(
            self.next_tree_id(),
            app_name,
            Some(pid as u32),
            screen_size,
            nodes,
            opts.clone(),
        ))
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut elements = Vec::new();

        // Desktop root
        nodes.push(Node {
            id: 0,
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
            bounds_normalized: Some(NormalizedRect {
                left: 0.0, top: 0.0, right: 1.0, bottom: 1.0,
            }),
            actions: vec![],
            states: StateSet::default(),
            children: vec![],
            parent: None,
            depth: 0,
            app_name: Some("Desktop".to_string()),
            raw: None,
        });
        elements.push(AXElement(std::ptr::null())); // placeholder

        let apps = Self::list_gui_apps();
        let mut root_children = Vec::new();

        let mut visited = HashSet::new();
        for (pid, app_name) in &apps {
            let app_element = AXElement::from_owned(unsafe { AXUIElementCreateApplication(*pid) });
            if app_element.is_null() {
                continue;
            }
            let child_node_id = nodes.len() as NodeId;
            root_children.push(child_node_id);
            self.traverse(
                &app_element,
                opts,
                app_name,
                &mut nodes,
                &mut elements,
                Some(0),
                1,
                screen_size,
                &mut visited,
            );
        }

        nodes[0].children = root_children;

        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new(
            self.next_tree_id(),
            "Desktop".to_string(),
            None,
            screen_size,
            nodes,
            opts.clone(),
        ))
    }

    fn perform_action(
        &self,
        tree: &Tree,
        node_id: NodeId,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let node = tree.get(node_id).ok_or(Error::NodeNotFound { node_id })?;

        // Require include_raw (consistent with Linux provider behavior)
        if node.raw.is_none() {
            return Err(Error::Platform {
                code: -1,
                message: "Action dispatch requires include_raw: true in QueryOptions".to_string(),
            });
        }

        // Look up cached element
        let cache = self.cached_elements.lock().unwrap();
        let element = cache.get(node_id as usize).ok_or(Error::ElementStale { node_id })?;
        if element.is_null() {
            return Err(Error::ElementStale { node_id });
        }
        let el_ptr = element.as_ptr();

        match action {
            Action::Press | Action::Toggle | Action::Select => {
                let ax_action = CFString::new("AXPress");
                let err = unsafe {
                    AXUIElementPerformAction(el_ptr, ax_action.as_concrete_TypeRef() as CFTypeRef)
                };
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
                let err = unsafe {
                    AXUIElementSetAttributeValue(
                        el_ptr,
                        attr.as_concrete_TypeRef() as CFTypeRef,
                        val.as_CFTypeRef(),
                    )
                };
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
                    let err = unsafe {
                        AXUIElementSetAttributeValue(
                            el_ptr,
                            attr.as_concrete_TypeRef() as CFTypeRef,
                            num.as_CFTypeRef(),
                        )
                    };
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
                    let err = unsafe {
                        AXUIElementSetAttributeValue(
                            el_ptr,
                            attr.as_concrete_TypeRef() as CFTypeRef,
                            val.as_concrete_TypeRef() as CFTypeRef,
                        )
                    };
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
                let err = unsafe {
                    AXUIElementSetAttributeValue(
                        el_ptr,
                        attr.as_concrete_TypeRef() as CFTypeRef,
                        val.as_CFTypeRef(),
                    )
                };
                if err != AX_ERROR_SUCCESS {
                    // Try AXPress as fallback (disclosure triangles)
                    let press = CFString::new("AXPress");
                    let _ = unsafe {
                        AXUIElementPerformAction(el_ptr, press.as_concrete_TypeRef() as CFTypeRef)
                    };
                }
                Ok(())
            }

            Action::Collapse => {
                let attr = CFString::new("AXExpanded");
                let val = core_foundation::boolean::CFBoolean::false_value();
                let err = unsafe {
                    AXUIElementSetAttributeValue(
                        el_ptr,
                        attr.as_concrete_TypeRef() as CFTypeRef,
                        val.as_CFTypeRef(),
                    )
                };
                if err != AX_ERROR_SUCCESS {
                    let press = CFString::new("AXPress");
                    let _ = unsafe {
                        AXUIElementPerformAction(el_ptr, press.as_concrete_TypeRef() as CFTypeRef)
                    };
                }
                Ok(())
            }

            Action::Increment => {
                let ax_action = CFString::new("AXIncrement");
                let err = unsafe {
                    AXUIElementPerformAction(el_ptr, ax_action.as_concrete_TypeRef() as CFTypeRef)
                };
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
                let err = unsafe {
                    AXUIElementPerformAction(el_ptr, ax_action.as_concrete_TypeRef() as CFTypeRef)
                };
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
                let err = unsafe {
                    AXUIElementPerformAction(el_ptr, ax_action.as_concrete_TypeRef() as CFTypeRef)
                };
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

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        let apps = Self::list_gui_apps();
        Ok(apps
            .into_iter()
            .map(|(pid, name)| AppInfo {
                name,
                pid: pid as u32,
                bundle_id: None,
            })
            .collect())
    }
}
