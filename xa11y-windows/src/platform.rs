use std::sync::Mutex;

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern, IUIAutomationRangeValuePattern,
    IUIAutomationScrollItemPattern, IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, TreeScope_Children, TreeScope_Element,
    UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_RangeValuePatternId,
    UIA_ScrollItemPatternId, UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};

use xa11y_core::*;

use crate::mapping;

/// Windows accessibility provider using UI Automation.
pub struct WindowsProvider {
    uia: Mutex<Option<IUIAutomation>>,
    /// Cached element references from the last snapshot, indexed by NodeId.
    elements: Mutex<Vec<IUIAutomationElement>>,
}

impl WindowsProvider {
    pub fn new() -> Self {
        Self {
            uia: Mutex::new(None),
            elements: Mutex::new(Vec::new()),
        }
    }

    fn automation(&self) -> Result<IUIAutomation> {
        let mut guard = self.uia.lock().unwrap();
        if let Some(ref uia) = *guard {
            return Ok(uia.clone());
        }
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok();
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
                .map_err(|e| Error::Platform(format!("Failed to create UIA: {e}")))?;
            *guard = Some(uia.clone());
            Ok(uia)
        }
    }
}

impl Default for WindowsProvider {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Send for WindowsProvider {}
unsafe impl Sync for WindowsProvider {}

impl Provider for WindowsProvider {
    fn check_permissions(&self) -> Result<PermissionStatus> {
        // Windows doesn't require special permissions for local UIA queries
        Ok(PermissionStatus::Granted)
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        let uia = self.automation()?;
        unsafe {
            let root = uia
                .GetRootElement()
                .map_err(|e| Error::Platform(format!("Failed to get root element: {e}")))?;

            let condition: IUIAutomationCondition = uia
                .CreateTrueCondition()
                .map_err(|e| Error::Platform(format!("Failed to create condition: {e}")))?;

            let children = root
                .FindAll(TreeScope_Children, &condition)
                .map_err(|e| Error::Platform(format!("Failed to enumerate children: {e}")))?;

            let count = children
                .Length()
                .map_err(|e| Error::Platform(format!("Failed to get count: {e}")))?;

            let mut apps = Vec::new();
            for i in 0..count {
                let elem = match children.GetElement(i) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                let name = elem
                    .CurrentName()
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let pid = elem.CurrentProcessId().unwrap_or(0) as u32;

                if name.is_empty() && pid == 0 {
                    continue;
                }

                apps.push(AppInfo {
                    name: if name.is_empty() {
                        format!("pid:{pid}")
                    } else {
                        name
                    },
                    pid,
                    bundle_id: None,
                });
            }

            Ok(apps)
        }
    }

    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        let uia = self.automation()?;

        unsafe {
            let root = uia
                .GetRootElement()
                .map_err(|e| Error::Platform(format!("Failed to get root element: {e}")))?;

            // Find the target app window
            let condition: IUIAutomationCondition = uia
                .CreateTrueCondition()
                .map_err(|e| Error::Platform(format!("Failed to create condition: {e}")))?;

            let children = root
                .FindAll(TreeScope_Children, &condition)
                .map_err(|e| Error::Platform(format!("Failed to enumerate children: {e}")))?;

            let count = children.Length().unwrap_or(0);

            let mut app_elem: Option<IUIAutomationElement> = None;
            let mut app_name = String::new();
            let mut app_pid: u32 = 0;

            for i in 0..count {
                let elem = match children.GetElement(i) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                let name = elem
                    .CurrentName()
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let pid = elem.CurrentProcessId().unwrap_or(0) as u32;

                let matches = match target {
                    AppTarget::ByName(ref target_name) => {
                        name.to_lowercase().contains(&target_name.to_lowercase())
                    }
                    AppTarget::ByPid(target_pid) => pid == *target_pid,
                    AppTarget::ByWindow(handle) => {
                        // Compare native window handle
                        elem.CurrentNativeWindowHandle()
                            .map(|h| h.0 as u64 == handle.id)
                            .unwrap_or(false)
                    }
                };

                if matches {
                    app_elem = Some(elem);
                    app_name = name;
                    app_pid = pid;
                    break;
                }
            }

            let app_elem = app_elem.ok_or_else(|| {
                Error::AppNotFound(match target {
                    AppTarget::ByName(n) => n.clone(),
                    AppTarget::ByPid(p) => format!("pid:{p}"),
                    AppTarget::ByWindow(h) => format!("window:{}", h.id),
                })
            })?;

            let screen_size = get_screen_size();
            let walker = uia
                .ContentViewWalker()
                .map_err(|e| Error::Platform(format!("Failed to get content walker: {e}")))?;

            let mut nodes: Vec<Node> = Vec::new();
            let mut elements: Vec<IUIAutomationElement> = Vec::new();
            let mut next_id: NodeId = 0;

            traverse_element(
                &walker,
                &app_elem,
                None,
                0,
                &app_name,
                opts,
                screen_size,
                &mut nodes,
                &mut elements,
                &mut next_id,
            );

            *self.elements.lock().unwrap() = elements;

            Ok(Tree::new(
                app_name,
                app_pid,
                screen_size,
                nodes,
                opts.clone(),
            ))
        }
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let apps = self.list_apps()?;
        let screen_size = get_screen_size();

        let all_nodes: Vec<Node> = apps
            .iter()
            .enumerate()
            .map(|(i, app)| Node {
                id: i as NodeId,
                role: Role::Application,
                name: Some(app.name.clone()),
                value: None,
                description: None,
                bounds: None,
                bounds_normalized: None,
                actions: vec![],
                states: StateSet::default(),
                children: vec![],
                parent: None,
                depth: 0,
                app_name: Some(app.name.clone()),
                raw: None,
            })
            .collect();

        Ok(Tree::new(
            String::new(),
            0,
            screen_size,
            all_nodes,
            opts.clone(),
        ))
    }

    fn perform_action(
        &self,
        node_id: NodeId,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let elements = self.elements.lock().unwrap();
        let elem = elements
            .get(node_id as usize)
            .ok_or(Error::NodeNotFound(node_id))?;

        unsafe {
            match action {
                Action::Press => {
                    let pattern: IUIAutomationInvokePattern =
                        elem.GetCurrentPatternAs(UIA_InvokePatternId).map_err(|e| {
                            Error::ActionNotSupported(format!("no Invoke pattern: {e}"))
                        })?;
                    pattern
                        .Invoke()
                        .map_err(|e| Error::Platform(format!("Invoke failed: {e}")))?;
                    Ok(())
                }
                Action::Toggle => {
                    let pattern: IUIAutomationTogglePattern =
                        elem.GetCurrentPatternAs(UIA_TogglePatternId).map_err(|e| {
                            Error::ActionNotSupported(format!("no Toggle pattern: {e}"))
                        })?;
                    pattern
                        .Toggle()
                        .map_err(|e| Error::Platform(format!("Toggle failed: {e}")))?;
                    Ok(())
                }
                Action::Expand => {
                    let pattern: IUIAutomationExpandCollapsePattern = elem
                        .GetCurrentPatternAs(UIA_ExpandCollapsePatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no ExpandCollapse pattern: {e}"))
                        })?;
                    pattern
                        .Expand()
                        .map_err(|e| Error::Platform(format!("Expand failed: {e}")))?;
                    Ok(())
                }
                Action::Collapse => {
                    let pattern: IUIAutomationExpandCollapsePattern = elem
                        .GetCurrentPatternAs(UIA_ExpandCollapsePatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no ExpandCollapse pattern: {e}"))
                        })?;
                    pattern
                        .Collapse()
                        .map_err(|e| Error::Platform(format!("Collapse failed: {e}")))?;
                    Ok(())
                }
                Action::Select => {
                    let pattern: IUIAutomationSelectionItemPattern = elem
                        .GetCurrentPatternAs(UIA_SelectionItemPatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no SelectionItem pattern: {e}"))
                        })?;
                    pattern
                        .Select()
                        .map_err(|e| Error::Platform(format!("Select failed: {e}")))?;
                    Ok(())
                }
                Action::Focus => {
                    elem.SetFocus()
                        .map_err(|e| Error::Platform(format!("SetFocus failed: {e}")))?;
                    Ok(())
                }
                Action::SetValue => {
                    let data = data.ok_or_else(|| {
                        Error::ActionNotSupported("SetValue requires data".into())
                    })?;
                    match data {
                        ActionData::Value(text) => {
                            let pattern: IUIAutomationValuePattern =
                                elem.GetCurrentPatternAs(UIA_ValuePatternId).map_err(|e| {
                                    Error::ActionNotSupported(format!("no Value pattern: {e}"))
                                })?;
                            let bstr = windows::core::BSTR::from(text.as_str());
                            pattern
                                .SetValue(&bstr)
                                .map_err(|e| Error::Platform(format!("SetValue failed: {e}")))?;
                            Ok(())
                        }
                        ActionData::NumericValue(v) => {
                            let pattern: IUIAutomationRangeValuePattern = elem
                                .GetCurrentPatternAs(UIA_RangeValuePatternId)
                                .map_err(|e| {
                                    Error::ActionNotSupported(format!("no RangeValue pattern: {e}"))
                                })?;
                            pattern.SetValue(v).map_err(|e| {
                                Error::Platform(format!("SetRangeValue failed: {e}"))
                            })?;
                            Ok(())
                        }
                        _ => Err(Error::ActionNotSupported(
                            "SetValue requires Value or NumericValue data".into(),
                        )),
                    }
                }
                Action::ScrollIntoView => {
                    let pattern: IUIAutomationScrollItemPattern = elem
                        .GetCurrentPatternAs(UIA_ScrollItemPatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no ScrollItem pattern: {e}"))
                        })?;
                    pattern
                        .ScrollIntoView()
                        .map_err(|e| Error::Platform(format!("ScrollIntoView failed: {e}")))?;
                    Ok(())
                }
                Action::Increment => {
                    let pattern: IUIAutomationRangeValuePattern = elem
                        .GetCurrentPatternAs(UIA_RangeValuePatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no RangeValue pattern: {e}"))
                        })?;
                    let current = pattern.CurrentValue().unwrap_or(0.0);
                    let small_change = pattern.CurrentSmallChange().unwrap_or(1.0);
                    pattern
                        .SetValue(current + small_change)
                        .map_err(|e| Error::Platform(format!("Increment failed: {e}")))?;
                    Ok(())
                }
                Action::Decrement => {
                    let pattern: IUIAutomationRangeValuePattern = elem
                        .GetCurrentPatternAs(UIA_RangeValuePatternId)
                        .map_err(|e| {
                            Error::ActionNotSupported(format!("no RangeValue pattern: {e}"))
                        })?;
                    let current = pattern.CurrentValue().unwrap_or(0.0);
                    let small_change = pattern.CurrentSmallChange().unwrap_or(1.0);
                    pattern
                        .SetValue(current - small_change)
                        .map_err(|e| Error::Platform(format!("Decrement failed: {e}")))?;
                    Ok(())
                }
                Action::ShowMenu => {
                    // Try ExpandCollapse for dropdown menus
                    if let Ok(pattern) = elem
                        .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                            UIA_ExpandCollapsePatternId,
                        )
                    {
                        pattern
                            .Expand()
                            .map_err(|e| Error::Platform(format!("ShowMenu/Expand failed: {e}")))?;
                        Ok(())
                    } else {
                        Err(Error::ActionNotSupported(
                            "ShowMenu not supported on this element".into(),
                        ))
                    }
                }
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
unsafe fn traverse_element(
    walker: &IUIAutomationTreeWalker,
    elem: &IUIAutomationElement,
    parent_id: Option<NodeId>,
    depth: u32,
    app_name: &str,
    opts: &QueryOptions,
    screen_size: (u32, u32),
    nodes: &mut Vec<Node>,
    elements: &mut Vec<IUIAutomationElement>,
    next_id: &mut NodeId,
) {
    if depth > opts.max_depth || *next_id >= opts.max_elements {
        return;
    }

    let control_type = elem.CurrentControlType().unwrap_or(0);
    let role = mapping::map_role(control_type);

    if let Some(ref roles) = opts.roles {
        if !roles.contains(&role) {
            return;
        }
    }

    let name: Option<String> = elem
        .CurrentName()
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    let help_text: Option<String> = elem
        .CurrentHelpText()
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    // Bounds
    let bounds: Option<Rect> = elem.CurrentBoundingRectangle().ok().map(|r| Rect {
        x: r.left as i32,
        y: r.top as i32,
        width: (r.right - r.left) as i32,
        height: (r.bottom - r.top) as i32,
    });

    let bounds_normalized: Option<NormalizedRect> = bounds.and_then(|b| {
        if screen_size.0 > 0 && screen_size.1 > 0 {
            Some(NormalizedRect {
                x1: b.x as f64 / screen_size.0 as f64,
                y1: b.y as f64 / screen_size.1 as f64,
                x2: (b.x + b.width) as f64 / screen_size.0 as f64,
                y2: (b.y + b.height) as f64 / screen_size.1 as f64,
            })
        } else {
            None
        }
    });

    // States
    let is_enabled = elem.CurrentIsEnabled().map(|b| b.as_bool()).unwrap_or(true);
    let is_offscreen = elem
        .CurrentIsOffscreen()
        .map(|b| b.as_bool())
        .unwrap_or(false);
    let has_focus = elem
        .CurrentHasKeyboardFocus()
        .map(|b| b.as_bool())
        .unwrap_or(false);

    let toggle_state: Option<i32> = elem
        .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
        .ok()
        .and_then(|p| p.CurrentToggleState().ok())
        .map(|s| s.0);

    let is_selected = elem
        .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        .ok()
        .and_then(|p| p.CurrentIsSelected().ok())
        .map(|b| b.as_bool())
        .unwrap_or(false);

    let expand_state: Option<i32> = elem
        .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
        .ok()
        .and_then(|p| p.CurrentExpandCollapseState().ok())
        .map(|s| s.0);

    let mut states = mapping::map_states(
        is_enabled,
        is_offscreen,
        has_focus,
        toggle_state,
        is_selected,
        expand_state,
    );

    if matches!(role, Role::TextField | Role::TextArea) {
        states.editable = true;
    }

    if opts.visible_only && !states.visible {
        return;
    }

    let my_id = *next_id;
    *next_id += 1;

    // Value
    let value: Option<String> = get_element_value(elem, role);

    // Actions from patterns
    let has_invoke = elem
        .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        .is_ok();
    let has_toggle = toggle_state.is_some();
    let has_expand = expand_state.is_some();
    let has_selection_item = elem
        .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        .is_ok();
    let has_value = elem
        .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        .is_ok();
    let has_range_value = elem
        .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
        .is_ok();
    let has_scroll_item = elem
        .GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
        .is_ok();

    let actions = mapping::actions_from_patterns(
        has_invoke,
        has_toggle,
        has_expand,
        has_selection_item,
        has_value,
        has_range_value,
        has_scroll_item,
    );

    // Raw data
    let raw: Option<RawPlatformData> = if opts.include_raw {
        let automation_id = elem
            .CurrentAutomationId()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        let class_name = elem
            .CurrentClassName()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        Some(RawPlatformData::Windows {
            control_type_id: control_type,
            automation_id,
            class_name,
        })
    } else {
        None
    };

    let node = Node {
        id: my_id,
        role,
        name,
        value,
        description: help_text,
        bounds,
        bounds_normalized,
        actions,
        states,
        children: vec![],
        parent: parent_id,
        depth,
        app_name: Some(app_name.to_string()),
        raw,
    };

    let node_idx = nodes.len();
    nodes.push(node);
    elements.push(elem.clone());

    // Traverse children using the content view walker
    let mut child_ids: Vec<NodeId> = Vec::new();

    if let Ok(first_child) = walker.GetFirstChildElement(elem) {
        let mut current = first_child;
        loop {
            if *next_id >= opts.max_elements {
                break;
            }

            let child_id = *next_id;
            let prev_count = nodes.len();

            traverse_element(
                walker,
                &current,
                Some(my_id),
                depth + 1,
                app_name,
                opts,
                screen_size,
                nodes,
                elements,
                next_id,
            );

            if nodes.len() > prev_count {
                child_ids.push(child_id);
            }

            match walker.GetNextSiblingElement(&current) {
                Ok(next) => current = next,
                Err(_) => break,
            }
        }
    }

    nodes[node_idx].children = child_ids;
}

unsafe fn get_element_value(elem: &IUIAutomationElement, role: Role) -> Option<String> {
    match role {
        Role::TextField | Role::TextArea | Role::StaticText | Role::Heading | Role::Link => elem
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.CurrentValue().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        Role::Slider | Role::ProgressBar | Role::ScrollBar => elem
            .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            .ok()
            .and_then(|p| p.CurrentValue().ok())
            .map(|v| v.to_string()),
        _ => None,
    }
}

fn get_screen_size() -> (u32, u32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        if w > 0 && h > 0 {
            (w as u32, h as u32)
        } else {
            (1920, 1080)
        }
    }
}
