//! Windows UI Automation accessibility provider.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, HORZRES, VERTRES};
use windows::Win32::System::Com::{CoInitializeEx, COINIT};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, CancelHandle, ElementState, Error, Event, EventFilter,
    EventKind, EventProvider, EventReceiver, Node, PermissionStatus, Provider, QueryOptions,
    RawPlatformData, Rect, Result, Role, ScrollDirection, StateSet, Subscription, Toggled, Tree,
};

/// Initialize COM for UIA. Called once per WindowsProvider creation.
/// Does not uninitialize on drop — COM lifetime is managed by the process.
fn ensure_com_initialized() -> windows::core::Result<()> {
    // Use MTA (0x0) — same mode as the Rust runtime default.
    // STA (0x2) would conflict with Rust's thread pool.
    let hr = unsafe { CoInitializeEx(None, COINIT(0x0)) };
    // S_OK, S_FALSE (already initialized), or RPC_E_CHANGED_MODE are all fine
    if hr.is_err() && hr.0 as u32 != 0x80010106 {
        hr.ok()?;
    }
    Ok(())
}

/// Windows accessibility provider using UI Automation.
pub struct WindowsProvider {
    automation: IUIAutomation,
    /// Cached UIA elements for action dispatch (keyed by node index).
    cached_elements: Mutex<Vec<IUIAutomationElement>>,
}

// IUIAutomation is COM and thread-safe via proxy
unsafe impl Send for WindowsProvider {}
unsafe impl Sync for WindowsProvider {}

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        ensure_com_initialized().map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("COM initialization failed: {}", e),
        })?;
        let automation: IUIAutomation = unsafe {
            windows::Win32::System::Com::CoCreateInstance(
                &CUIAutomation8,
                None,
                windows::Win32::System::Com::CLSCTX_ALL,
            )
        }
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("Failed to create IUIAutomation: {}", e),
        })?;
        Ok(Self {
            automation,
            cached_elements: Mutex::new(Vec::new()),
        })
    }

    /// Re-acquire a UIA element via its native window handle.
    /// This triggers WM_GETOBJECT which activates AccessKit's UIA provider,
    /// ensuring the element's children include virtual accessibility nodes.
    fn reacquire_via_hwnd(
        &self,
        element: &IUIAutomationElement,
    ) -> std::result::Result<IUIAutomationElement, ()> {
        let hwnd = unsafe { element.CurrentNativeWindowHandle() }.map_err(|_| ())?;
        if hwnd.0.is_null() {
            return Err(());
        }
        unsafe { self.automation.ElementFromHandle(hwnd) }.map_err(|_| ())
    }

    fn detect_screen_size() -> (u32, u32) {
        unsafe {
            let hdc = GetDC(HWND::default());
            if hdc.is_invalid() {
                return (1920, 1080);
            }
            let w = GetDeviceCaps(hdc, HORZRES) as u32;
            let h = GetDeviceCaps(hdc, VERTRES) as u32;
            let _ = ReleaseDC(HWND::default(), hdc);
            if w == 0 || h == 0 {
                (1920, 1080)
            } else {
                (w, h)
            }
        }
    }

    /// List running GUI applications by enumerating top-level windows.
    fn list_gui_apps(&self) -> Vec<(u32, String)> {
        let root = match unsafe { self.automation.GetRootElement() } {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        let condition = match unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &windows::core::VARIANT::from(UIA_WindowControlTypeId.0),
            )
        } {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let elements = match unsafe { root.FindAll(TreeScope_Children, &condition) } {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        let count = unsafe { elements.Length() }.unwrap_or(0);
        let mut seen = HashSet::new();
        let mut apps = Vec::new();

        for i in 0..count {
            if let Ok(el) = unsafe { elements.GetElement(i) } {
                let pid = unsafe { el.CurrentProcessId() }.unwrap_or(0) as u32;
                if pid == 0 || !seen.insert(pid) {
                    continue;
                }
                let name = unsafe { el.CurrentName() }
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                apps.push((pid, name));
            }
        }

        apps
    }

    fn find_app_by_name(&self, name: &str) -> Result<(IUIAutomationElement, u32, String)> {
        let name_lower = name.to_lowercase();
        let root = unsafe { self.automation.GetRootElement() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("GetRootElement failed: {}", e),
        })?;

        // Find all top-level windows
        let condition = unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &windows::core::VARIANT::from(UIA_WindowControlTypeId.0),
            )
        }
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("CreatePropertyCondition failed: {}", e),
        })?;

        let elements = unsafe { root.FindAll(TreeScope_Children, &condition) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("FindAll failed: {}", e),
            }
        })?;

        let count = unsafe { elements.Length() }.unwrap_or(0);
        let mut seen_pids = HashSet::new();

        for i in 0..count {
            if let Ok(el) = unsafe { elements.GetElement(i) } {
                let pid = unsafe { el.CurrentProcessId() }.unwrap_or(0) as u32;
                let el_name = unsafe { el.CurrentName() }
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                if el_name.to_lowercase().contains(&name_lower) {
                    let el = self.reacquire_via_hwnd(&el).unwrap_or(el);
                    return Ok((el, pid, el_name));
                }

                // Also check the process name if window title doesn't match
                if pid > 0 && seen_pids.insert(pid) {
                    if let Some(proc_name) = get_process_name(pid) {
                        if proc_name.to_lowercase().contains(&name_lower) {
                            let el = self.reacquire_via_hwnd(&el).unwrap_or(el);
                            return Ok((el, pid, proc_name));
                        }
                    }
                }
            }
        }

        Err(Error::AppNotFound {
            target: name.to_string(),
        })
    }

    fn find_app_by_pid(&self, pid: u32) -> Result<(IUIAutomationElement, String)> {
        let root = unsafe { self.automation.GetRootElement() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("GetRootElement failed: {}", e),
        })?;

        let condition = unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ProcessIdPropertyId,
                &windows::core::VARIANT::from(pid as i32),
            )
        }
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("CreatePropertyCondition failed: {}", e),
        })?;

        let el = unsafe { root.FindFirst(TreeScope_Children, &condition) }.map_err(|_| {
            Error::AppNotFound {
                target: format!("PID {}", pid),
            }
        })?;

        // Re-acquire via HWND to activate AccessKit provider
        let el = self.reacquire_via_hwnd(&el).unwrap_or(el);

        let name = unsafe { el.CurrentName() }
            .map(|s| s.to_string())
            .unwrap_or_default();

        Ok((el, name))
    }

    /// Recursively traverse the UIA tree, building xa11y nodes.
    #[allow(clippy::too_many_arguments)]
    fn traverse(
        &self,
        element: &IUIAutomationElement,
        opts: &QueryOptions,
        nodes: &mut Vec<Node>,
        elements: &mut Vec<IUIAutomationElement>,
        parent_idx: Option<u32>,
        depth: u32,
        screen_size: (u32, u32),
    ) {
        const MAX_DEPTH: u32 = 50;
        if depth > MAX_DEPTH {
            return;
        }

        // Depth limit is sufficient for cycle protection on UIA trees.
        // (COM pointer identity is unreliable for cycle detection because
        // UIA creates proxy objects with different addresses for the same element.)

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

        let control_type = unsafe { element.CurrentControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
        let mut role = map_uia_control_type(control_type);

        // Refine role using AriaRole property for elements that UIA maps ambiguously
        // (e.g., Alert/Heading both become ControlType.Text, Dialog becomes Window)
        if matches!(
            role,
            Role::StaticText | Role::Window | Role::Group | Role::Unknown
        ) {
            if let Ok(v) = unsafe { element.GetCurrentPropertyValue(UIA_AriaRolePropertyId) } {
                if let Ok(aria) = windows::core::BSTR::try_from(&v) {
                    let aria_str = aria.to_string();
                    match aria_str.as_str() {
                        "alert" => role = Role::Alert,
                        "dialog" | "alertdialog" => role = Role::Dialog,
                        "heading" => role = Role::Heading,
                        "separator" => role = Role::Separator,
                        "progressbar" => role = Role::ProgressBar,
                        "link" => role = Role::Link,
                        _ => {}
                    }
                }
            }
        }

        // Role filter: skip node but still traverse children
        let role_filtered = if depth > 0 {
            !opts.roles.is_empty() && !opts.roles.contains(&role)
        } else {
            false
        };

        if role_filtered {
            self.traverse_children(
                element,
                opts,
                nodes,
                elements,
                parent_idx,
                depth,
                screen_size,
            );
            return;
        }

        let name = unsafe { element.CurrentName() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        // Value: use ValuePattern or RangeValuePattern
        let value = get_value(element, role);

        // Try FullDescription first (AccessKit's description), then HelpText
        let description = unsafe { element.GetCurrentPropertyValue(UIA_FullDescriptionPropertyId) }
            .ok()
            .and_then(|v| {
                let bstr = windows::core::BSTR::try_from(&v).ok()?;
                let s = bstr.to_string();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            })
            .or_else(|| {
                unsafe { element.GetCurrentPropertyValue(UIA_HelpTextPropertyId) }
                    .ok()
                    .and_then(|v| {
                        let bstr = windows::core::BSTR::try_from(&v).ok()?;
                        let s = bstr.to_string();
                        if s.is_empty() {
                            None
                        } else {
                            Some(s)
                        }
                    })
            });

        let states = parse_states(element, role);

        if depth > 0 && opts.visible_only && !states.visible {
            self.traverse_children(
                element,
                opts,
                nodes,
                elements,
                parent_idx,
                depth,
                screen_size,
            );
            return;
        }

        // Bounds
        let bounds = unsafe { element.CurrentBoundingRectangle() }
            .ok()
            .and_then(|r| {
                let width = (r.right - r.left).max(0) as u32;
                let height = (r.bottom - r.top).max(0) as u32;
                if width == 0 && height == 0 {
                    None
                } else {
                    Some(Rect {
                        x: r.left,
                        y: r.top,
                        width,
                        height,
                    })
                }
            });

        // Actions
        let actions = get_actions(element, role);

        // Raw platform data
        let raw = {
            let automation_id = unsafe { element.CurrentAutomationId() }
                .ok()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            let class_name = unsafe { element.CurrentClassName() }
                .ok()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            RawPlatformData::Windows {
                control_type_id: control_type.0,
                automation_id,
                class_name,
            }
        };

        // Stable ID: AutomationId (always captured for cross-snapshot correlation)
        let stable_id = unsafe { element.CurrentAutomationId() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let (numeric_value, min_value, max_value) = if matches!(
            role,
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::SpinButton
        ) {
            if let Ok(pattern) = unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            } {
                (
                    unsafe { pattern.CurrentValue() }.ok(),
                    unsafe { pattern.CurrentMinimum() }.ok(),
                    unsafe { pattern.CurrentMaximum() }.ok(),
                )
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };

        let node_idx = nodes.len() as u32;
        nodes.push(Node {
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            stable_id,
            numeric_value,
            min_value,
            max_value,
            raw,
            index: node_idx,
            children_indices: vec![],
            parent_index: parent_idx,
        });
        elements.push(element.clone());

        // Recurse children. Try FindAll first (works with AccessKit fragment roots),
        // fall back to RawViewWalker for native elements.
        let mut child_ids = Vec::new();

        let children_found = if let Ok(true_cond) = unsafe { self.automation.CreateTrueCondition() }
        {
            if let Ok(children) = unsafe { element.FindAll(TreeScope_Children, &true_cond) } {
                let count = unsafe { children.Length() }.unwrap_or(0);
                if count > 0 {
                    for i in 0..count {
                        if let Some(max_elements) = opts.max_elements {
                            if nodes.len() >= max_elements as usize {
                                break;
                            }
                        }
                        if let Ok(child_el) = unsafe { children.GetElement(i) } {
                            let child_idx = nodes.len() as u32;
                            child_ids.push(child_idx);
                            self.traverse(
                                &child_el,
                                opts,
                                nodes,
                                elements,
                                Some(node_idx),
                                depth + 1,
                                screen_size,
                            );
                        }
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        // Fall back to RawViewWalker if FindAll found nothing
        if !children_found {
            if let Ok(walker) = unsafe { self.automation.RawViewWalker() } {
                let mut child = unsafe { walker.GetFirstChildElement(element) }.ok();
                while let Some(ref child_el) = child {
                    if let Some(max_elements) = opts.max_elements {
                        if nodes.len() >= max_elements as usize {
                            break;
                        }
                    }
                    let child_idx = nodes.len() as u32;
                    child_ids.push(child_idx);
                    self.traverse(
                        child_el,
                        opts,
                        nodes,
                        elements,
                        Some(node_idx),
                        depth + 1,
                        screen_size,
                    );
                    child = unsafe { walker.GetNextSiblingElement(child_el) }.ok();
                }
            }
        }

        nodes[node_idx as usize].children_indices = child_ids;
    }

    /// Traverse children only (used when current node is filtered out).
    #[allow(clippy::too_many_arguments)]
    fn traverse_children(
        &self,
        element: &IUIAutomationElement,
        opts: &QueryOptions,
        nodes: &mut Vec<Node>,
        elements: &mut Vec<IUIAutomationElement>,
        parent_idx: Option<u32>,
        depth: u32,
        screen_size: (u32, u32),
    ) {
        let walker = match unsafe { self.automation.RawViewWalker() } {
            Ok(w) => w,
            Err(_) => return,
        };

        let mut child = unsafe { walker.GetFirstChildElement(element) }.ok();
        while let Some(ref child_el) = child {
            if let Some(max_elements) = opts.max_elements {
                if nodes.len() >= max_elements as usize {
                    break;
                }
            }
            self.traverse(
                child_el,
                opts,
                nodes,
                elements,
                parent_idx,
                depth + 1,
                screen_size,
            );
            child = unsafe { walker.GetNextSiblingElement(child_el) }.ok();
        }
    }
}

impl Provider for WindowsProvider {
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        let (app_element, pid, app_name) = match target {
            AppTarget::ByName(name) => self.find_app_by_name(name)?,
            AppTarget::ByPid(pid) => {
                let (el, name) = self.find_app_by_pid(*pid)?;
                (el, *pid, name)
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

        self.traverse(
            &app_element,
            opts,
            &mut nodes,
            &mut elements,
            None,
            0,
            screen_size,
        );

        if nodes.is_empty() {
            return Err(Error::AppNotFound {
                target: format!("{:?}", target),
            });
        }

        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new(app_name, Some(pid), screen_size, nodes))
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut elements = Vec::new();

        nodes.push(Node {
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
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![],
            parent_index: None,
        });

        let root = unsafe { self.automation.GetRootElement() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("GetRootElement failed: {}", e),
        })?;
        // Use a dummy element for the Desktop root
        elements.push(root.clone());

        let condition = unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &windows::core::VARIANT::from(UIA_WindowControlTypeId.0),
            )
        }
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("CreatePropertyCondition failed: {}", e),
        })?;

        let windows = unsafe { root.FindAll(TreeScope_Children, &condition) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("FindAll failed: {}", e),
            }
        })?;

        let count = unsafe { windows.Length() }.unwrap_or(0);
        let mut root_children = Vec::new();
        let mut seen_pids = HashSet::new();

        for i in 0..count {
            if let Ok(el) = unsafe { windows.GetElement(i) } {
                let pid = unsafe { el.CurrentProcessId() }.unwrap_or(0) as u32;
                if pid == 0 || !seen_pids.insert(pid) {
                    continue;
                }
                let name = unsafe { el.CurrentName() }
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let child_idx = nodes.len() as u32;
                root_children.push(child_idx);
                self.traverse(
                    &el,
                    opts,
                    &mut nodes,
                    &mut elements,
                    Some(0),
                    1,
                    screen_size,
                );
            }
        }

        nodes[0].children_indices = root_children;
        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new("Desktop".to_string(), None, screen_size, nodes))
    }

    fn perform_action(
        &self,
        tree: &Tree,
        node: &Node,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let node_idx = tree.node_index(node);

        let cache = self.cached_elements.lock().unwrap();
        let element = cache.get(node_idx as usize).ok_or(Error::ElementStale {
            selector: format!("index:{}", node_idx),
        })?;

        match action {
            Action::Press | Action::Toggle | Action::Select => {
                // Try InvokePattern first, then TogglePattern, then SelectionItemPattern
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                } {
                    unsafe { pattern.Invoke() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Invoke failed".to_string(),
                    })?;
                    return Ok(());
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
                } {
                    unsafe { pattern.Toggle() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Toggle failed".to_string(),
                    })?;
                    return Ok(());
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                } {
                    unsafe { pattern.Select() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Select failed".to_string(),
                    })?;
                    return Ok(());
                }
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
            }

            Action::Focus => {
                unsafe { element.SetFocus() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: "SetFocus failed".to_string(),
                })?;
                Ok(())
            }

            Action::SetValue => match data {
                Some(ActionData::NumericValue(v)) => {
                    if let Ok(pattern) = unsafe {
                        element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
                            UIA_RangeValuePatternId,
                        )
                    } {
                        unsafe { pattern.SetValue(v) }.map_err(|e| Error::Platform {
                            code: e.code().0 as i64,
                            message: "RangeValue.SetValue failed".to_string(),
                        })?;
                        return Ok(());
                    }
                    // Fall back to ValuePattern with string
                    if let Ok(pattern) = unsafe {
                        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    } {
                        let s: windows::core::BSTR = v.to_string().into();
                        unsafe { pattern.SetValue(&s) }.map_err(|e| Error::Platform {
                            code: e.code().0 as i64,
                            message: "Value.SetValue failed".to_string(),
                        })?;
                        return Ok(());
                    }
                    Err(Error::Platform {
                        code: -1,
                        message: "No Value or RangeValue pattern".to_string(),
                    })
                }
                Some(ActionData::Value(text)) => {
                    if let Ok(pattern) = unsafe {
                        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    } {
                        let s: windows::core::BSTR = text.into();
                        unsafe { pattern.SetValue(&s) }
                            .map_err(|_| Error::TextValueNotSupported)?;
                        return Ok(());
                    }
                    Err(Error::TextValueNotSupported)
                }
                _ => Err(Error::Platform {
                    code: -1,
                    message: "SetValue requires ActionData".to_string(),
                }),
            },

            Action::Expand => {
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                } {
                    // Ignore errors (may already be expanded)
                    let _ = unsafe { pattern.Expand() };
                    return Ok(());
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                } {
                    let _ = unsafe { pattern.Invoke() };
                }
                Ok(())
            }

            Action::Collapse => {
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                } {
                    // Ignore errors (may already be collapsed)
                    let _ = unsafe { pattern.Collapse() };
                    return Ok(());
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                } {
                    let _ = unsafe { pattern.Invoke() };
                }
                Ok(())
            }

            Action::Increment => {
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
                        UIA_RangeValuePatternId,
                    )
                } {
                    let current = unsafe { pattern.CurrentValue() }.unwrap_or(0.0);
                    let small = unsafe { pattern.CurrentSmallChange() }.unwrap_or(1.0);
                    let step = if small <= 0.0 { 1.0 } else { small };
                    unsafe { pattern.SetValue(current + step) }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Increment failed".to_string(),
                    })?;
                    return Ok(());
                }
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
            }

            Action::Decrement => {
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
                        UIA_RangeValuePatternId,
                    )
                } {
                    let current = unsafe { pattern.CurrentValue() }.unwrap_or(0.0);
                    let small = unsafe { pattern.CurrentSmallChange() }.unwrap_or(1.0);
                    let step = if small <= 0.0 { 1.0 } else { small };
                    unsafe { pattern.SetValue(current - step) }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Decrement failed".to_string(),
                    })?;
                    return Ok(());
                }
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
            }

            Action::ShowMenu => {
                // No direct UIA equivalent; try context menu via legacy
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
            }

            Action::ScrollIntoView => {
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(
                        UIA_ScrollItemPatternId,
                    )
                } {
                    let _ = unsafe { pattern.ScrollIntoView() };
                }
                Ok(())
            }

            Action::Blur => {
                // Focus the desktop root to blur the current element
                let root =
                    unsafe { self.automation.GetRootElement() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "GetRootElement failed".to_string(),
                    })?;
                unsafe { root.SetFocus() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: "SetFocus on root failed".to_string(),
                })?;
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
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
                } {
                    // 1 logical scroll unit = 1 SmallIncrement
                    let count = (amount.abs() as u32).max(1);
                    for _ in 0..count {
                        let (h, v) = match direction {
                            ScrollDirection::Up => {
                                (ScrollAmount_NoAmount, ScrollAmount_SmallDecrement)
                            }
                            ScrollDirection::Down => {
                                (ScrollAmount_NoAmount, ScrollAmount_SmallIncrement)
                            }
                            ScrollDirection::Left => {
                                (ScrollAmount_SmallDecrement, ScrollAmount_NoAmount)
                            }
                            ScrollDirection::Right => {
                                (ScrollAmount_SmallIncrement, ScrollAmount_NoAmount)
                            }
                        };
                        let _ = unsafe { pattern.Scroll(h, v) };
                    }
                    return Ok(());
                }
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
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
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                } {
                    let range =
                        unsafe { pattern.DocumentRange() }.map_err(|e| Error::Platform {
                            code: e.code().0 as i64,
                            message: "DocumentRange failed".to_string(),
                        })?;
                    // Collapse and move to start position
                    let _ = unsafe { range.Move(TextUnit_Character, start as i32) };
                    // Extend end to selection length
                    let _ = unsafe {
                        range.MoveEndpointByUnit(
                            TextPatternRangeEndpoint_End,
                            TextUnit_Character,
                            (end - start) as i32,
                        )
                    };
                    unsafe { range.Select() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Select range failed".to_string(),
                    })?;
                    return Ok(());
                }
                Err(Error::ActionNotSupported {
                    action,
                    role: node.role,
                })
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
                // Insert text via ValuePattern (accessibility API, not input simulation).
                // Get current value, get insertion point from TextPattern, splice, set new value.
                if let Ok(value_pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                } {
                    let current = unsafe { value_pattern.CurrentValue() }
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    // Try to get cursor position from TextPattern
                    let insert_pos = if let Ok(text_pattern) = unsafe {
                        element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                    } {
                        // Get the selection/caret range — its start offset is the cursor
                        unsafe { text_pattern.GetSelection() }
                            .ok()
                            .and_then(|arr| unsafe { arr.GetElement(0) }.ok())
                            .map(|_| current.len()) // Fallback: append at end
                            .unwrap_or(current.len())
                    } else {
                        current.len() // No TextPattern — append at end
                    };

                    let mut new_value = current;
                    new_value.insert_str(insert_pos.min(new_value.len()), &text);
                    let bstr: windows::core::BSTR = new_value.into();
                    unsafe { value_pattern.SetValue(&bstr) }
                        .map_err(|_| Error::TextValueNotSupported)?;
                    return Ok(());
                }
                Err(Error::TextValueNotSupported)
            }
        }
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        Ok(PermissionStatus::Granted)
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        let apps = self.list_gui_apps();
        Ok(apps
            .into_iter()
            .map(|(pid, name)| AppInfo {
                name,
                pid,
                bundle_id: None,
            })
            .collect())
    }
}

// ── Helper Functions ─────────────────────────────────────────────────────────

fn get_process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if ok.is_ok() && size > 0 {
            let path = String::from_utf16_lossy(&buf[..size as usize]);
            path.rsplit('\\').next().map(|s| s.to_string())
        } else {
            None
        }
    }
}

/// Get the value of an element using UIA patterns.
fn get_value(element: &IUIAutomationElement, role: Role) -> Option<String> {
    // For checkboxes/radios, value is handled by state — skip
    if matches!(role, Role::CheckBox | Role::RadioButton) {
        return None;
    }

    // Try RangeValuePattern first (sliders, progress bars, spinners)
    if let Ok(pattern) = unsafe {
        element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
    } {
        if let Ok(v) = unsafe { pattern.CurrentValue() } {
            return Some(v.to_string());
        }
    }

    // Try ValuePattern (text fields, combo boxes)
    if let Ok(pattern) =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
    {
        if let Ok(v) = unsafe { pattern.CurrentValue() } {
            let s = v.to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }

    None
}

/// Determine available actions from UIA patterns.
fn get_actions(element: &IUIAutomationElement, role: Role) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();

    if unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
        .is_ok()
    {
        actions.push(Action::Press);
    }

    if unsafe { element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) }
        .is_ok()
    {
        if !actions.contains(&Action::Press) {
            actions.push(Action::Press);
        }
    }

    if unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
    }
    .is_ok()
    {
        actions.push(Action::Expand);
        actions.push(Action::Collapse);
    }

    if unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
        .is_ok()
    {
        if !actions.contains(&Action::SetValue) {
            actions.push(Action::SetValue);
        }
    }

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
    }
    .is_ok()
    {
        if !actions.contains(&Action::SetValue) {
            actions.push(Action::SetValue);
        }
        actions.push(Action::Increment);
        actions.push(Action::Decrement);
    }

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .is_ok()
    {
        actions.push(Action::Select);
    }

    // Focus: most elements can be focused
    if unsafe { element.CurrentIsKeyboardFocusable() }
        .unwrap_or(BOOL(0))
        .as_bool()
    {
        actions.push(Action::Focus);
    }

    // For text fields and sliders, ensure SetValue is present
    if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
        && !actions.contains(&Action::SetValue)
    {
        actions.push(Action::SetValue);
    }

    actions
}

/// Parse UIA element properties into xa11y StateSet.
#[allow(non_upper_case_globals)]
fn parse_states(element: &IUIAutomationElement, role: Role) -> StateSet {
    let enabled = unsafe { element.CurrentIsEnabled() }
        .unwrap_or(BOOL(1))
        .as_bool();
    let offscreen = unsafe { element.CurrentIsOffscreen() }
        .unwrap_or(BOOL(0))
        .as_bool();
    let visible = !offscreen;
    let focused = unsafe { element.CurrentHasKeyboardFocus() }
        .unwrap_or(BOOL(0))
        .as_bool();

    // Checked: from TogglePattern
    let checked = match role {
        Role::CheckBox | Role::RadioButton => {
            if let Ok(pattern) = unsafe {
                element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            } {
                match unsafe { pattern.CurrentToggleState() } {
                    Ok(ToggleState_On) => Some(Toggled::On),
                    Ok(ToggleState_Off) => Some(Toggled::Off),
                    Ok(ToggleState_Indeterminate) => Some(Toggled::Mixed),
                    _ => Some(Toggled::Off),
                }
            } else {
                // For radio buttons, check SelectionItemPattern
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                } {
                    if unsafe { pattern.CurrentIsSelected() }
                        .unwrap_or(BOOL(0))
                        .as_bool()
                    {
                        Some(Toggled::On)
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

    // Expanded: from ExpandCollapsePattern
    let expanded = if let Ok(pattern) = unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
    } {
        match unsafe { pattern.CurrentExpandCollapseState() } {
            Ok(ExpandCollapseState_Expanded) => Some(true),
            Ok(ExpandCollapseState_Collapsed) => Some(false),
            _ => None,
        }
    } else {
        None
    };

    // Selected: from SelectionItemPattern
    let selected = if let Ok(pattern) = unsafe {
        element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    } {
        unsafe { pattern.CurrentIsSelected() }
            .unwrap_or(BOOL(0))
            .as_bool()
    } else {
        false
    };

    let editable = match role {
        Role::TextField | Role::TextArea => {
            if let Ok(pattern) = unsafe {
                element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            } {
                unsafe { pattern.CurrentIsReadOnly() }.unwrap_or(BOOL(1)) == BOOL(0)
            } else {
                true
            }
        }
        _ => false,
    };

    let focusable = unsafe { element.CurrentIsKeyboardFocusable() }.unwrap_or(FALSE) == TRUE;

    StateSet {
        enabled,
        visible,
        focused,
        focusable,
        modal: false,
        checked,
        selected,
        expanded,
        editable,
        required: false,
        busy: false,
    }
}

/// Map UIA ControlTypeId to xa11y Role.
#[allow(non_upper_case_globals)]
fn map_uia_control_type(control_type: UIA_CONTROLTYPE_ID) -> Role {
    match control_type {
        UIA_ButtonControlTypeId => Role::Button,
        UIA_CheckBoxControlTypeId => Role::CheckBox,
        UIA_RadioButtonControlTypeId => Role::RadioButton,
        UIA_EditControlTypeId => Role::TextField,
        UIA_TextControlTypeId => Role::StaticText,
        UIA_ComboBoxControlTypeId => Role::ComboBox,
        UIA_ListControlTypeId => Role::List,
        UIA_ListItemControlTypeId => Role::ListItem,
        UIA_MenuControlTypeId => Role::Menu,
        UIA_MenuItemControlTypeId => Role::MenuItem,
        UIA_MenuBarControlTypeId => Role::MenuBar,
        UIA_TabControlTypeId => Role::TabGroup,
        UIA_TabItemControlTypeId => Role::Tab,
        UIA_TableControlTypeId => Role::Table,
        UIA_DataGridControlTypeId => Role::Table,
        UIA_DataItemControlTypeId => Role::TableRow,
        UIA_ToolBarControlTypeId => Role::Toolbar,
        UIA_ScrollBarControlTypeId => Role::ScrollBar,
        UIA_SliderControlTypeId => Role::Slider,
        UIA_ImageControlTypeId => Role::Image,
        UIA_HyperlinkControlTypeId => Role::Link,
        UIA_GroupControlTypeId => Role::Group,
        UIA_WindowControlTypeId => Role::Window,
        UIA_PaneControlTypeId => Role::Group,
        UIA_ProgressBarControlTypeId => Role::ProgressBar,
        UIA_TreeItemControlTypeId => Role::TreeItem,
        UIA_TreeControlTypeId => Role::List,
        UIA_DocumentControlTypeId => Role::WebArea,
        UIA_HeaderControlTypeId => Role::Group,
        UIA_HeaderItemControlTypeId => Role::TableCell,
        UIA_SeparatorControlTypeId => Role::Separator,
        UIA_SpinnerControlTypeId => Role::SpinButton,
        UIA_SplitButtonControlTypeId => Role::Button,
        UIA_StatusBarControlTypeId => Role::Status,
        UIA_ThumbControlTypeId => Role::Unknown,
        UIA_TitleBarControlTypeId => Role::Group,
        UIA_ToolTipControlTypeId => Role::Tooltip,
        UIA_CalendarControlTypeId => Role::Group,
        UIA_CustomControlTypeId => Role::Unknown,
        _ => Role::Unknown,
    }
}

// ── EventProvider (polling-based) ─────────────────────────────────────────────

impl EventProvider for WindowsProvider {
    fn subscribe(&self, target: &AppTarget, filter: EventFilter) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel();

        let app_info = match target {
            AppTarget::ByName(name) => {
                let apps = self.list_gui_apps();
                let found = apps
                    .iter()
                    .find(|(_, n)| n.to_lowercase().contains(&name.to_lowercase()));
                match found {
                    Some((pid, name)) => AppInfo {
                        name: name.clone(),
                        pid: *pid,
                        bundle_id: None,
                    },
                    None => {
                        return Err(Error::AppNotFound {
                            target: name.clone(),
                        })
                    }
                }
            }
            AppTarget::ByPid(pid) => AppInfo {
                name: String::new(),
                pid: *pid,
                bundle_id: None,
            },
            AppTarget::ByWindow(_) => {
                return Err(Error::Platform {
                    code: -1,
                    message: "ByWindow not supported for event subscription".to_string(),
                })
            }
        };

        // Create a separate provider for polling on the background thread
        let poll_provider = WindowsProvider::new()?;
        let target_clone = target.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut prev_focused: Option<String> = None;
            let mut prev_node_count: usize = 0;

            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));

                let tree = match poll_provider.get_app_tree(&target_clone, &QueryOptions::default())
                {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                // Detect focus changes
                let focused_name = tree
                    .iter()
                    .find(|n| n.states.focused)
                    .and_then(|n| n.name.clone());
                if focused_name != prev_focused {
                    if prev_focused.is_some() {
                        let kind = EventKind::FocusChanged;
                        if filter.kinds.is_empty() || filter.kinds.contains(&kind) {
                            let _ = tx.send(Event {
                                kind,
                                app: app_info.clone(),
                                target: tree.iter().find(|n| n.states.focused).cloned(),
                                state_flag: None,
                                state_value: None,
                                text_change: None,
                                timestamp: std::time::Instant::now(),
                            });
                        }
                    }
                    prev_focused = focused_name;
                }

                // Detect structure changes
                let node_count = tree.len();
                if node_count != prev_node_count && prev_node_count > 0 {
                    let kind = EventKind::StructureChanged;
                    if filter.kinds.is_empty() || filter.kinds.contains(&kind) {
                        let _ = tx.send(Event {
                            kind,
                            app: app_info.clone(),
                            target: None,
                            state_flag: None,
                            state_value: None,
                            text_change: None,
                            timestamp: std::time::Instant::now(),
                        });
                    }
                }
                prev_node_count = node_count;
            }
        });

        let cancel = CancelHandle::new(move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.join();
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
    ) -> Result<Node> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.get_app_tree(target, &QueryOptions::default())?;
            let matches = tree.query(selector).ok();
            let node = matches.as_ref().and_then(|m| m.first().copied());

            if state.is_met(node) {
                return Ok(node.cloned().unwrap_or_else(Node::synthetic_empty));
            }

            std::thread::sleep(poll_interval);
        }
    }
}

#[cfg(test)]
#[allow(non_upper_case_globals)]
mod tests {
    use super::*;

    #[test]
    fn role_mapping_covers_common_types() {
        assert_eq!(map_uia_control_type(UIA_ButtonControlTypeId), Role::Button);
        assert_eq!(
            map_uia_control_type(UIA_CheckBoxControlTypeId),
            Role::CheckBox
        );
        assert_eq!(map_uia_control_type(UIA_EditControlTypeId), Role::TextField);
        assert_eq!(
            map_uia_control_type(UIA_TextControlTypeId),
            Role::StaticText
        );
        assert_eq!(
            map_uia_control_type(UIA_ComboBoxControlTypeId),
            Role::ComboBox
        );
        assert_eq!(map_uia_control_type(UIA_ListControlTypeId), Role::List);
        assert_eq!(
            map_uia_control_type(UIA_ListItemControlTypeId),
            Role::ListItem
        );
        assert_eq!(map_uia_control_type(UIA_MenuControlTypeId), Role::Menu);
        assert_eq!(
            map_uia_control_type(UIA_MenuItemControlTypeId),
            Role::MenuItem
        );
        assert_eq!(
            map_uia_control_type(UIA_MenuBarControlTypeId),
            Role::MenuBar
        );
        assert_eq!(map_uia_control_type(UIA_TabControlTypeId), Role::TabGroup);
        assert_eq!(map_uia_control_type(UIA_TabItemControlTypeId), Role::Tab);
        assert_eq!(map_uia_control_type(UIA_SliderControlTypeId), Role::Slider);
        assert_eq!(map_uia_control_type(UIA_WindowControlTypeId), Role::Window);
        assert_eq!(
            map_uia_control_type(UIA_ProgressBarControlTypeId),
            Role::ProgressBar
        );
        assert_eq!(
            map_uia_control_type(UIA_TreeItemControlTypeId),
            Role::TreeItem
        );
        assert_eq!(
            map_uia_control_type(UIA_SeparatorControlTypeId),
            Role::Separator
        );
        assert_eq!(map_uia_control_type(UIA_ImageControlTypeId), Role::Image);
        assert_eq!(map_uia_control_type(UIA_HyperlinkControlTypeId), Role::Link);
        assert_eq!(map_uia_control_type(UIA_GroupControlTypeId), Role::Group);
        assert_eq!(
            map_uia_control_type(UIA_CONTROLTYPE_ID(99999)),
            Role::Unknown
        );
    }

    #[test]
    fn provider_new_succeeds() {
        let provider = WindowsProvider::new();
        assert!(provider.is_ok());
    }

    #[test]
    fn detect_screen_size_returns_nonzero() {
        let (w, h) = WindowsProvider::detect_screen_size();
        assert!(w > 0);
        assert!(h > 0);
    }
}
