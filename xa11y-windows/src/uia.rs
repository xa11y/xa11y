//! Windows UI Automation accessibility provider.

use std::collections::HashSet;
use std::sync::Mutex;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, HORZRES, VERTRES};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, Error, Node, NodeId, NormalizedRect, PermissionStatus,
    Provider, QueryOptions, RawPlatformData, Rect, Result, Role, StateSet, Toggled, Tree,
};

/// RAII wrapper for COM initialization.
struct ComInit;

impl ComInit {
    fn new() -> windows::core::Result<Self> {
        // Try STA first — UIA needs STA for proper IRawElementProviderFragmentRoot
        // callbacks (e.g., AccessKit virtual elements). If already initialized as
        // MTA (common in multi-threaded apps), fall back gracefully.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        match result {
            Ok(()) => {}
            Err(ref e) if e.code().0 as u32 == 0x80010106 => {
                // RPC_E_CHANGED_MODE: already initialized with MTA, that's OK
            }
            Err(ref e) if e.code().0 == 1 => {
                // S_FALSE: COM already initialized on this thread, that's OK
            }
            Err(e) => return Err(e),
        }
        Ok(Self)
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Windows accessibility provider using UI Automation.
pub struct WindowsProvider {
    automation: IUIAutomation,
    next_tree_id: Mutex<u64>,
    /// Cached UIA elements for action dispatch (keyed by NodeId).
    cached_elements: Mutex<Vec<IUIAutomationElement>>,
    _com: ComInit,
}

// IUIAutomation is COM and thread-safe via proxy
unsafe impl Send for WindowsProvider {}
unsafe impl Sync for WindowsProvider {}

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        let com = ComInit::new().map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("COM initialization failed: {}", e),
        })?;
        let automation: IUIAutomation = unsafe {
            windows::Win32::System::Com::CoCreateInstance(
                &CUIAutomation8,
                None,
                windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("Failed to create IUIAutomation: {}", e),
        })?;
        Ok(Self {
            automation,
            next_tree_id: Mutex::new(1),
            cached_elements: Mutex::new(Vec::new()),
            _com: com,
        })
    }

    fn next_tree_id(&self) -> u64 {
        let mut id = self.next_tree_id.lock().unwrap();
        let current = *id;
        *id += 1;
        current
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
                    return Ok((el, pid, el_name));
                }

                // Also check the process name if window title doesn't match
                if pid > 0 && seen_pids.insert(pid) {
                    if let Some(proc_name) = get_process_name(pid) {
                        if proc_name.to_lowercase().contains(&name_lower) {
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
        app_name: &str,
        nodes: &mut Vec<Node>,
        elements: &mut Vec<IUIAutomationElement>,
        parent_id: Option<NodeId>,
        depth: u32,
        screen_size: (u32, u32),
        visited: &mut HashSet<usize>,
    ) {
        const MAX_DEPTH: u32 = 50;
        if depth > MAX_DEPTH {
            return;
        }

        // Cycle detection via COM pointer identity
        let ptr_key = element as *const IUIAutomationElement as usize;
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

        let control_type = unsafe { element.CurrentControlType() }
            .unwrap_or(UIA_CONTROLTYPE_ID(0));
        let role = map_uia_control_type(control_type);

        // Role filter: skip node but still traverse children
        let role_filtered = if depth > 0 {
            if let Some(ref filter_roles) = opts.roles {
                !filter_roles.contains(&role)
            } else {
                false
            }
        } else {
            false
        };

        if role_filtered {
            self.traverse_children(
                element, opts, app_name, nodes, elements, parent_id, depth, screen_size, visited,
            );
            return;
        }

        let name = unsafe { element.CurrentName() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        // Value: use ValuePattern or RangeValuePattern
        let value = get_value(element, role);

        let description = unsafe {
            element.GetCurrentPropertyValue(UIA_HelpTextPropertyId)
        }
        .ok()
        .and_then(|v| {
            let bstr: windows::core::BSTR = windows::core::BSTR::try_from(&v).ok()?;
            let s = bstr.to_string();
            if s.is_empty() { None } else { Some(s) }
        });

        let states = parse_states(element, role);

        if depth > 0 && opts.visible_only && !states.visible {
            self.traverse_children(
                element, opts, app_name, nodes, elements, parent_id, depth, screen_size, visited,
            );
            return;
        }

        // Bounds
        let bounds = unsafe { element.CurrentBoundingRectangle() }.ok().and_then(|r| {
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

        let bounds_normalized = bounds.map(|b| {
            let (sw, sh) = screen_size;
            if sw == 0 || sh == 0 {
                return NormalizedRect {
                    left: 0.0,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
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
        let actions = get_actions(element, role);

        // Raw platform data
        let raw = if opts.include_raw {
            let automation_id = unsafe { element.CurrentAutomationId() }
                .ok()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            let class_name = unsafe { element.CurrentClassName() }
                .ok()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            Some(RawPlatformData::Windows {
                control_type_id: control_type.0,
                automation_id,
                class_name,
            })
        } else {
            None
        };

        let node_id = nodes.len() as NodeId;
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
            children: vec![],
            parent: parent_id,
            depth,
            app_name: Some(app_name.to_string()),
            raw,
        });
        elements.push(element.clone());

        // Recurse children. Try FindAll first (works with AccessKit fragment roots),
        // fall back to RawViewWalker for native elements.
        let mut child_ids = Vec::new();

        let children_found = if let Ok(true_cond) = unsafe { self.automation.CreateTrueCondition() } {
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
                            let child_node_id = nodes.len() as NodeId;
                            child_ids.push(child_node_id);
                            self.traverse(
                                &child_el,
                                opts,
                                app_name,
                                nodes,
                                elements,
                                Some(node_id),
                                depth + 1,
                                screen_size,
                                visited,
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
                    let child_node_id = nodes.len() as NodeId;
                    child_ids.push(child_node_id);
                    self.traverse(
                        child_el,
                        opts,
                        app_name,
                        nodes,
                        elements,
                        Some(node_id),
                        depth + 1,
                        screen_size,
                        visited,
                    );
                    child = unsafe { walker.GetNextSiblingElement(child_el) }.ok();
                }
            }
        }

        nodes[node_id as usize].children = child_ids;
    }

    /// Traverse children only (used when current node is filtered out).
    #[allow(clippy::too_many_arguments)]
    fn traverse_children(
        &self,
        element: &IUIAutomationElement,
        opts: &QueryOptions,
        app_name: &str,
        nodes: &mut Vec<Node>,
        elements: &mut Vec<IUIAutomationElement>,
        parent_id: Option<NodeId>,
        depth: u32,
        screen_size: (u32, u32),
        visited: &mut HashSet<usize>,
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
                app_name,
                nodes,
                elements,
                parent_id,
                depth + 1,
                screen_size,
                visited,
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

        *self.cached_elements.lock().unwrap() = elements;

        Ok(Tree::new(
            self.next_tree_id(),
            app_name,
            Some(pid),
            screen_size,
            nodes,
            opts.clone(),
        ))
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut elements = Vec::new();

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
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
            actions: vec![],
            states: StateSet::default(),
            children: vec![],
            parent: None,
            depth: 0,
            app_name: Some("Desktop".to_string()),
            raw: None,
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
        let mut visited = HashSet::new();
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
                let child_node_id = nodes.len() as NodeId;
                root_children.push(child_node_id);
                self.traverse(
                    &el,
                    opts,
                    &name,
                    &mut nodes,
                    &mut elements,
                    Some(0),
                    1,
                    screen_size,
                    &mut visited,
                );
            }
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

        if node.raw.is_none() {
            return Err(Error::Platform {
                code: -1,
                message: "Action dispatch requires include_raw: true in QueryOptions".to_string(),
            });
        }

        let cache = self.cached_elements.lock().unwrap();
        let element = cache
            .get(node_id as usize)
            .ok_or(Error::ElementStale { node_id })?;

        match action {
            Action::Press | Action::Toggle | Action::Select => {
                // Try InvokePattern first, then TogglePattern, then SelectionItemPattern
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) } {
                    unsafe { pattern.Invoke() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Invoke failed".to_string(),
                    })?;
                    return Ok(());
                }
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) } {
                    unsafe { pattern.Toggle() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Toggle failed".to_string(),
                    })?;
                    return Ok(());
                }
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) } {
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
                    if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId) } {
                        unsafe { pattern.SetValue(v) }.map_err(|e| Error::Platform {
                            code: e.code().0 as i64,
                            message: "RangeValue.SetValue failed".to_string(),
                        })?;
                        return Ok(());
                    }
                    // Fall back to ValuePattern with string
                    if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) } {
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
                    if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) } {
                        let s: windows::core::BSTR = text.into();
                        unsafe { pattern.SetValue(&s) }.map_err(|_| {
                            Error::TextValueNotSupported
                        })?;
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
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) } {
                    unsafe { pattern.Expand() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Expand failed".to_string(),
                    })?;
                    return Ok(());
                }
                // Fall back to Invoke
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) } {
                    let _ = unsafe { pattern.Invoke() };
                }
                Ok(())
            }

            Action::Collapse => {
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) } {
                    unsafe { pattern.Collapse() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: "Collapse failed".to_string(),
                    })?;
                    return Ok(());
                }
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) } {
                    let _ = unsafe { pattern.Invoke() };
                }
                Ok(())
            }

            Action::Increment => {
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId) } {
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
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId) } {
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
                if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId) } {
                    let _ = unsafe { pattern.ScrollIntoView() };
                }
                Ok(())
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
    let mut actions = Vec::new();

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
    }
    .is_ok()
    {
        actions.push(Action::Press);
    }

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
    }
    .is_ok()
    {
        if !actions.contains(&Action::Press) {
            actions.push(Action::Press);
        }
    }

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
            UIA_ExpandCollapsePatternId,
        )
    }
    .is_ok()
    {
        actions.push(Action::Expand);
        actions.push(Action::Collapse);
    }

    if unsafe {
        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    }
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
        element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
            UIA_SelectionItemPatternId,
        )
    }
    .is_ok()
    {
        actions.push(Action::Select);
    }

    // Focus: most elements can be focused
    if unsafe { element.CurrentIsKeyboardFocusable() }.unwrap_or(BOOL(0)).as_bool() {
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
fn parse_states(element: &IUIAutomationElement, role: Role) -> StateSet {
    let enabled = unsafe { element.CurrentIsEnabled() }.unwrap_or(BOOL(1)).as_bool();
    let offscreen = unsafe { element.CurrentIsOffscreen() }.unwrap_or(BOOL(0)).as_bool();
    let visible = !offscreen;
    let focused = unsafe { element.CurrentHasKeyboardFocus() }.unwrap_or(BOOL(0)).as_bool();

    // Checked: from TogglePattern
    let checked = match role {
        Role::CheckBox | Role::RadioButton => {
            if let Ok(pattern) = unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
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
                    if unsafe { pattern.CurrentIsSelected() }.unwrap_or(BOOL(0)).as_bool() {
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
        element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
            UIA_ExpandCollapsePatternId,
        )
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
        element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
            UIA_SelectionItemPatternId,
        )
    } {
        unsafe { pattern.CurrentIsSelected() }.unwrap_or(BOOL(0)).as_bool()
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

/// Map UIA ControlTypeId to xa11y Role.
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
        UIA_SpinnerControlTypeId => Role::TextField,
        UIA_SplitButtonControlTypeId => Role::Button,
        UIA_StatusBarControlTypeId => Role::Group,
        UIA_ThumbControlTypeId => Role::Unknown,
        UIA_TitleBarControlTypeId => Role::Group,
        UIA_ToolTipControlTypeId => Role::Group,
        UIA_CalendarControlTypeId => Role::Group,
        UIA_CustomControlTypeId => Role::Unknown,
        _ => Role::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_mapping_covers_common_types() {
        assert_eq!(map_uia_control_type(UIA_ButtonControlTypeId), Role::Button);
        assert_eq!(map_uia_control_type(UIA_CheckBoxControlTypeId), Role::CheckBox);
        assert_eq!(map_uia_control_type(UIA_EditControlTypeId), Role::TextField);
        assert_eq!(map_uia_control_type(UIA_TextControlTypeId), Role::StaticText);
        assert_eq!(map_uia_control_type(UIA_ComboBoxControlTypeId), Role::ComboBox);
        assert_eq!(map_uia_control_type(UIA_ListControlTypeId), Role::List);
        assert_eq!(map_uia_control_type(UIA_ListItemControlTypeId), Role::ListItem);
        assert_eq!(map_uia_control_type(UIA_MenuControlTypeId), Role::Menu);
        assert_eq!(map_uia_control_type(UIA_MenuItemControlTypeId), Role::MenuItem);
        assert_eq!(map_uia_control_type(UIA_MenuBarControlTypeId), Role::MenuBar);
        assert_eq!(map_uia_control_type(UIA_TabControlTypeId), Role::TabGroup);
        assert_eq!(map_uia_control_type(UIA_TabItemControlTypeId), Role::Tab);
        assert_eq!(map_uia_control_type(UIA_SliderControlTypeId), Role::Slider);
        assert_eq!(map_uia_control_type(UIA_WindowControlTypeId), Role::Window);
        assert_eq!(map_uia_control_type(UIA_ProgressBarControlTypeId), Role::ProgressBar);
        assert_eq!(map_uia_control_type(UIA_TreeItemControlTypeId), Role::TreeItem);
        assert_eq!(map_uia_control_type(UIA_SeparatorControlTypeId), Role::Separator);
        assert_eq!(map_uia_control_type(UIA_ImageControlTypeId), Role::Image);
        assert_eq!(map_uia_control_type(UIA_HyperlinkControlTypeId), Role::Link);
        assert_eq!(map_uia_control_type(UIA_GroupControlTypeId), Role::Group);
        assert_eq!(map_uia_control_type(UIA_CONTROLTYPE_ID(99999)), Role::Unknown);
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
