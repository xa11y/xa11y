//! Windows UI Automation accessibility provider.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT};

use windows::Win32::UI::Accessibility::*;

use xa11y_core::{
    selector::{matches_simple, Combinator, Selector, SelectorSegment},
    CancelHandle, ElementData, Error, Event, EventReceiver, EventType, Provider, Rect, Result,
    Role, StateSet, Subscription, Toggled,
};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

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
    /// Describes which properties and patterns to pre-fetch in bulk queries.
    /// Not a cache — each FindAllBuildCache call takes a fresh snapshot.
    batch_request: IUIAutomationCacheRequest,
    /// UIA elements retained for action dispatch (keyed by handle ID).
    handle_cache: Mutex<HashMap<u64, IUIAutomationElement>>,
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
        let batch_request = create_batch_request(&automation)?;

        Ok(Self {
            automation,
            batch_request,
            handle_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Re-acquire a UIA element via its native window handle.
    /// This triggers WM_GETOBJECT which activates AccessKit's UIA provider,
    /// ensuring the element's children include virtual accessibility elements.
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

    fn find_app_by_pid(&self, pid: u32) -> Result<(IUIAutomationElement, String)> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let condition = uia_call(|| unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ProcessIdPropertyId,
                &windows::core::VARIANT::from(pid as i32),
            )
        })?;
        let el = unsafe { root.FindFirst(TreeScope_Children, &condition) }.map_err(|_| {
            Error::Platform {
                code: -1,
                message: format!("No window found for PID {}", pid),
            }
        })?;

        // Re-acquire via HWND to activate AccessKit provider
        let el = self.reacquire_via_hwnd(&el).unwrap_or(el);

        let name = unsafe { el.CurrentName() }
            .map(|s| s.to_string())
            .unwrap_or_default();

        Ok((el, name))
    }

    /// Cache a UIA element and return its handle ID.
    fn cache_element(&self, uia: IUIAutomationElement) -> u64 {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handle_cache.lock().unwrap().insert(handle, uia);
        handle
    }

    /// Look up a cached UIA element by handle.
    fn get_cached(&self, handle: u64) -> Result<IUIAutomationElement> {
        self.handle_cache
            .lock()
            .unwrap()
            .get(&handle)
            .cloned()
            .ok_or(Error::ElementStale {
                selector: format!("handle:{}", handle),
            })
    }

    /// Query UIA patterns from the element once, sharing across
    /// `get_value`, `get_actions`, and `parse_states` to avoid duplicate COM calls.
    fn query_patterns(element: &IUIAutomationElement) -> ElementPatterns {
        ElementPatterns {
            invoke: unsafe {
                element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            }
            .ok(),
            toggle: unsafe {
                element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            }
            .ok(),
            expand_collapse: unsafe {
                element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                )
            }
            .ok(),
            value: unsafe {
                element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            }
            .ok(),
            range_value: unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            }
            .ok(),
            selection_item: unsafe {
                element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                    UIA_SelectionItemPatternId,
                )
            }
            .ok(),
        }
    }

    /// Build an ElementData from a pre-fetched UIA element snapshot.
    ///
    /// The element MUST have been obtained via `FindAllBuildCache` or
    /// `BuildUpdatedCache` so that Cached* accessors are populated.
    /// Every query takes a fresh snapshot — callers never see stale data.
    fn build_element_data(&self, element: &IUIAutomationElement, pid: Option<u32>) -> ElementData {
        let control_type = unsafe { element.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
        let mut role = map_uia_control_type(control_type);

        // Refine role using AriaRole property for elements that UIA maps ambiguously
        // (e.g., Alert/Heading both become ControlType.Text, Dialog becomes Window)
        if matches!(
            role,
            Role::StaticText | Role::Window | Role::Group | Role::Unknown
        ) {
            if let Some(aria_str) = uia_cached_bstr(element, UIA_AriaRolePropertyId) {
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

        let name = unsafe { element.CachedName() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let patterns = Self::query_patterns(element);
        let value = get_value(role, &patterns);

        // Try FullDescription first (AccessKit's description), then HelpText
        let description = uia_cached_bstr(element, UIA_FullDescriptionPropertyId)
            .or_else(|| uia_cached_bstr(element, UIA_HelpTextPropertyId));

        let states = parse_states(element, role, &patterns);

        let bounds = unsafe { element.CachedBoundingRectangle() }
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

        let actions = get_actions(element, role, &patterns);

        let automation_id = unsafe { element.CachedAutomationId() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let class_name = unsafe { element.CachedClassName() }
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let raw = {
            let mut raw = std::collections::HashMap::new();
            raw.insert(
                "control_type_id".into(),
                serde_json::Value::Number(serde_json::Number::from(control_type.0)),
            );
            if let Some(ref aid) = automation_id {
                raw.insert(
                    "automation_id".into(),
                    serde_json::Value::String(aid.clone()),
                );
            }
            if let Some(ref cn) = class_name {
                raw.insert("class_name".into(), serde_json::Value::String(cn.clone()));
            }
            raw
        };

        let (numeric_value, min_value, max_value) = if matches!(
            role,
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::SpinButton
        ) {
            if let Some(ref pattern) = patterns.range_value {
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

        let handle = self.cache_element(element.clone());

        let mut data = ElementData {
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            stable_id: automation_id,
            numeric_value,
            min_value,
            max_value,
            pid,
            attributes: std::collections::HashMap::new(),
            raw,
            handle,
        };
        data.populate_attributes();
        data
    }

    /// Populate a UIA element's snapshot so Cached* accessors work.
    /// Used for single-element reads (e.g., get_parent) that don't go
    /// through FindAllBuildCache.
    fn populate_cache(
        &self,
        element: &IUIAutomationElement,
    ) -> windows::core::Result<IUIAutomationElement> {
        unsafe { element.BuildUpdatedCache(&self.batch_request) }
    }

    /// Get direct UIA children of an element with properties pre-fetched.
    /// Tries FindAllBuildCache first (works with AccessKit fragment roots),
    /// falls back to RawViewWalker + BuildUpdatedCache for native elements.
    fn uia_children(&self, element: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
        // Try FindAllBuildCache(Children) first — works with AccessKit virtual elements
        if let Ok(true_cond) = unsafe { self.automation.CreateTrueCondition() } {
            if let Ok(arr) = unsafe {
                element.FindAllBuildCache(TreeScope_Children, &true_cond, &self.batch_request)
            } {
                let count = uia_len(&arr);
                if count > 0 {
                    return (0..count).filter_map(|i| uia_get(&arr, i)).collect();
                }
            }
        }

        // Fall back to RawViewWalker if FindAll found nothing
        let mut children = Vec::new();
        if let Ok(walker) = unsafe { self.automation.RawViewWalker() } {
            let mut child = unsafe { walker.GetFirstChildElement(element) }.ok();
            while let Some(ref child_el) = child {
                // Populate snapshot so build_element_data can use Cached* accessors
                let cached = self.populate_cache(child_el);
                children.push(cached.as_ref().unwrap_or(child_el).clone());
                child = unsafe { walker.GetNextSiblingElement(child_el) }.ok();
            }
        }
        children
    }

    /// Fetch the entire subtree with all properties pre-fetched in one COM call.
    fn find_all_subtree(&self, root: &IUIAutomationElement) -> Result<IUIAutomationElementArray> {
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        uia_call(|| unsafe {
            root.FindAllBuildCache(TreeScope_Subtree, &true_cond, &self.batch_request)
        })
    }

    /// Narrow phase-1 candidates through subsequent selector segments.
    fn narrow_multi_segment(
        &self,
        mut candidates: Vec<ElementData>,
        segments: &[SelectorSegment],
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
                            if matches_simple(&child, &segment.simple) {
                                next_candidates.push(child);
                            }
                        }
                    }
                    Combinator::Descendant => {
                        let sub_selector = Selector {
                            segments: vec![SelectorSegment {
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

// ── Safe UIA helpers ────────────────────────────────────────────────────────

/// Wrap a UIA COM call, mapping the error to xa11y Error::Platform.
fn uia_call<T>(f: impl FnOnce() -> windows::core::Result<T>) -> Result<T> {
    f().map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: e.to_string(),
    })
}

/// Read a BSTR VARIANT property from the element's pre-fetched snapshot.
fn uia_cached_bstr(element: &IUIAutomationElement, prop: UIA_PROPERTY_ID) -> Option<String> {
    unsafe { element.GetCachedPropertyValue(prop) }
        .ok()
        .and_then(|v| windows::core::BSTR::try_from(&v).ok())
        .map(|b| b.to_string())
        .filter(|s| !s.is_empty())
}

/// Build the batch request that describes which properties and patterns
/// to pre-fetch. Created once per provider, used on every query.
fn create_batch_request(automation: &IUIAutomation) -> Result<IUIAutomationCacheRequest> {
    let request = uia_call(|| unsafe { automation.CreateCacheRequest() })?;

    for prop in BATCH_PROPERTIES {
        let _ = unsafe { request.AddProperty(*prop) };
    }

    Ok(request)
}

/// Properties pre-fetched in every bulk query.
const BATCH_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_AriaRolePropertyId,
    UIA_NamePropertyId,
    UIA_FullDescriptionPropertyId,
    UIA_HelpTextPropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
    UIA_ProcessIdPropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
];

/// Safe wrapper for IUIAutomationElementArray::Length.
fn uia_len(arr: &IUIAutomationElementArray) -> i32 {
    unsafe { arr.Length() }.unwrap_or(0)
}

/// Safe wrapper for IUIAutomationElementArray::GetElement.
fn uia_get(arr: &IUIAutomationElementArray, index: i32) -> Option<IUIAutomationElement> {
    unsafe { arr.GetElement(index) }.ok()
}

/// Pre-queried UIA patterns for an element, avoiding redundant COM calls
/// across `get_value`, `get_actions`, and `parse_states`.
struct ElementPatterns {
    invoke: Option<IUIAutomationInvokePattern>,
    toggle: Option<IUIAutomationTogglePattern>,
    expand_collapse: Option<IUIAutomationExpandCollapsePattern>,
    value: Option<IUIAutomationValuePattern>,
    range_value: Option<IUIAutomationRangeValuePattern>,
    selection_item: Option<IUIAutomationSelectionItemPattern>,
}

impl Provider for WindowsProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: list all GUI application windows
                let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
                let condition = uia_call(|| unsafe {
                    self.automation.CreatePropertyCondition(
                        UIA_ControlTypePropertyId,
                        &windows::core::VARIANT::from(UIA_WindowControlTypeId.0),
                    )
                })?;
                let found = uia_call(|| unsafe {
                    root.FindAllBuildCache(TreeScope_Children, &condition, &self.batch_request)
                })?;

                let mut results = Vec::new();
                let mut seen_pids = HashSet::new();

                for i in 0..uia_len(&found) {
                    let Some(el) = uia_get(&found, i) else {
                        continue;
                    };
                    let pid = unsafe { el.CachedProcessId() }.unwrap_or(0) as u32;
                    if pid == 0 || !seen_pids.insert(pid) {
                        continue;
                    }
                    let name = unsafe { el.CachedName() }
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    // Re-acquire via HWND to activate AccessKit provider,
                    // then populate snapshot for build_element_data.
                    let el = self
                        .reacquire_via_hwnd(&el)
                        .and_then(|e| self.populate_cache(&e).map_err(|_| ()))
                        .unwrap_or(el);
                    let mut data = self.build_element_data(&el, Some(pid));
                    if data.name.is_none() {
                        data.name = Some(name);
                    }
                    results.push(data);
                }

                Ok(results)
            }
            Some(element_data) => {
                let uia = self.get_cached(element_data.handle)?;
                let children = self.uia_children(&uia);
                let pid = element_data.pid;
                Ok(children
                    .iter()
                    .map(|child| self.build_element_data(child, pid))
                    .collect())
            }
        }
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let uia = self.get_cached(element.handle)?;
        if let Ok(walker) = unsafe { self.automation.RawViewWalker() } {
            if let Ok(parent) = unsafe { walker.GetParentElement(&uia) } {
                // Check if the parent is the desktop root (no further parent)
                let parent_parent = unsafe { walker.GetParentElement(&parent) };
                if parent_parent.is_err() {
                    return Ok(None);
                }
                // Populate snapshot so build_element_data can read Cached* props
                let parent = self.populate_cache(&parent).map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("BuildUpdatedCache failed: {}", e),
                })?;
                let data = self.build_element_data(&parent, element.pid);
                return Ok(Some(data));
            }
        }
        Ok(None)
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

        // Applications are always direct children of the desktop root
        if root.is_none()
            && matches!(
                first.role,
                Some(xa11y_core::selector::RoleMatch::Normalized(
                    Role::Application
                ))
            )
        {
            let mut matching = self.get_children(None)?;

            // Filter by selector attributes (name etc.)
            matching.retain(|el| matches_simple(el, first));

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

        // For non-root searches, use FindAll(TreeScope_Subtree) with
        // TrueCondition to fetch the entire subtree in one COM call, then filter
        // client-side with matches_simple.
        let root_data = match root {
            Some(el) => el,
            None => {
                // Searching from system root with a non-Application selector.
                // Fall back to the default tree-walking implementation.
                return xa11y_core::selector::find_elements_in_tree(
                    |el| self.get_children(el),
                    root,
                    selector,
                    limit,
                    max_depth,
                );
            }
        };

        let uia_root = self.get_cached(root_data.handle)?;
        let pid = root_data.pid;

        let subtree = self.find_all_subtree(&uia_root)?;
        let count = uia_len(&subtree);
        let mut matching = Vec::new();

        for i in 0..count {
            let el = match uia_get(&subtree, i) {
                Some(el) => el,
                None => continue,
            };

            let data = self.build_element_data(&el, pid);

            if !matches_simple(&data, first) {
                continue;
            }

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

        self.narrow_multi_segment(matching, &selector.segments[1..], max_depth_val, limit)
    }

    fn press(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        } {
            unsafe { pattern.Invoke() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "Invoke failed".to_string(),
            })?;
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "press".to_string(),
            role: element.role,
        })
    }

    fn focus(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        unsafe { uia_element.SetFocus() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: "SetFocus failed".to_string(),
        })?;
        Ok(())
    }

    fn blur(&self, _element: &ElementData) -> Result<()> {
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

    fn toggle(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
        } {
            unsafe { pattern.Toggle() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "Toggle failed".to_string(),
            })?;
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "toggle".to_string(),
            role: element.role,
        })
    }

    fn select(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
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
            action: "select".to_string(),
            role: element.role,
        })
    }

    fn expand(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                UIA_ExpandCollapsePatternId,
            )
        } {
            // Ignore errors (may already be expanded)
            let _ = unsafe { pattern.Expand() };
            return Ok(());
        }
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        } {
            let _ = unsafe { pattern.Invoke() };
        }
        Ok(())
    }

    fn collapse(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                UIA_ExpandCollapsePatternId,
            )
        } {
            // Ignore errors (may already be collapsed)
            let _ = unsafe { pattern.Collapse() };
            return Ok(());
        }
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        } {
            let _ = unsafe { pattern.Invoke() };
        }
        Ok(())
    }

    fn show_menu(&self, element: &ElementData) -> Result<()> {
        // No direct UIA equivalent; try context menu via legacy
        Err(Error::ActionNotSupported {
            action: "show_menu".to_string(),
            role: element.role,
        })
    }

    fn increment(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
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
            action: "increment".to_string(),
            role: element.role,
        })
    }

    fn decrement(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
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
            action: "decrement".to_string(),
            role: element.role,
        })
    }

    fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(
                UIA_ScrollItemPatternId,
            )
        } {
            let _ = unsafe { pattern.ScrollIntoView() };
        }
        Ok(())
    }

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        } {
            let s: windows::core::BSTR = value.into();
            unsafe { pattern.SetValue(&s) }
                .map_err(|_| Error::TextValueNotSupported)?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
                UIA_RangeValuePatternId,
            )
        } {
            unsafe { pattern.SetValue(value) }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "RangeValue.SetValue failed".to_string(),
            })?;
            return Ok(());
        }
        // Fall back to ValuePattern with string
        if let Ok(pattern) = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        } {
            let s: windows::core::BSTR = value.to_string().into();
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

    fn type_text(&self, element: &ElementData, text: &str) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        // Insert text via ValuePattern (accessibility API, not input simulation).
        // Get current value, get insertion point from TextPattern, splice, set new value.
        if let Ok(value_pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        } {
            let current = unsafe { value_pattern.CurrentValue() }
                .map(|s| s.to_string())
                .unwrap_or_default();

            // Try to get cursor position from TextPattern
            let insert_pos = if let Ok(text_pattern) = unsafe {
                uia_element
                    .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
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
            new_value.insert_str(insert_pos.min(new_value.len()), text);
            let bstr: windows::core::BSTR = new_value.into();
            unsafe { value_pattern.SetValue(&bstr) }
                .map_err(|_| Error::TextValueNotSupported)?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_text_selection(
        &self,
        element: &ElementData,
        start: u32,
        end: u32,
    ) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
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
            action: "set_text_selection".to_string(),
            role: element.role,
        })
    }

    fn scroll_down(&self, element: &ElementData, amount: f64) -> Result<()> {
        self.scroll_impl(element, amount, true)
    }

    fn scroll_up(&self, element: &ElementData, amount: f64) -> Result<()> {
        self.scroll_impl(element, -amount, true)
    }

    fn scroll_right(&self, element: &ElementData, amount: f64) -> Result<()> {
        self.scroll_impl(element, amount, false)
    }

    fn scroll_left(&self, element: &ElementData, amount: f64) -> Result<()> {
        self.scroll_impl(element, -amount, false)
    }

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
            _ => Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            }),
        }
    }

    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        let pid = element.pid.ok_or(Error::Platform {
            code: -1,
            message: "Element has no PID for subscribe".to_string(),
        })?;
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(app_name, pid, move |p| {
            let (el, _name) = p.find_app_by_pid(pid)?;
            Ok(el)
        })
    }
}

// ── Helper Functions ─────────────────────────────────────────────────────────

/// Get the value of an element from its pre-fetched pattern snapshot.
fn get_value(role: Role, patterns: &ElementPatterns) -> Option<String> {
    // For checkboxes/radios, value is handled by state — skip
    if matches!(role, Role::CheckBox | Role::RadioButton) {
        return None;
    }

    // Try RangeValuePattern first (sliders, progress bars, spinners)
    if let Some(ref pattern) = patterns.range_value {
        if let Ok(v) = unsafe { pattern.CurrentValue() } {
            return Some(v.to_string());
        }
    }

    // Try ValuePattern (text fields, combo boxes)
    if let Some(ref pattern) = patterns.value {
        if let Ok(v) = unsafe { pattern.CurrentValue() } {
            let s = v.to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }

    None
}

/// Determine available actions from pre-queried UIA patterns.
fn get_actions(
    element: &IUIAutomationElement,
    role: Role,
    patterns: &ElementPatterns,
) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();

    if patterns.invoke.is_some() {
        actions.push(Action::Press);
    }

    if patterns.toggle.is_some() && !actions.contains(&Action::Toggle) {
        actions.push(Action::Toggle);
    }

    if patterns.expand_collapse.is_some() {
        actions.push(Action::Expand);
        actions.push(Action::Collapse);
    }

    if patterns.value.is_some() && !actions.contains(&Action::SetValue) {
        actions.push(Action::SetValue);
    }

    if patterns.range_value.is_some() {
        if !actions.contains(&Action::SetValue) {
            actions.push(Action::SetValue);
        }
        actions.push(Action::Increment);
        actions.push(Action::Decrement);
    }

    if patterns.selection_item.is_some() {
        actions.push(Action::Select);
    }

    // Focus: most elements can be focused
    if unsafe { element.CachedIsKeyboardFocusable() }
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

/// Parse UIA element properties into xa11y StateSet using pre-queried patterns.
#[allow(non_upper_case_globals)]
fn parse_states(
    element: &IUIAutomationElement,
    role: Role,
    patterns: &ElementPatterns,
) -> StateSet {
    let enabled = unsafe { element.CachedIsEnabled() }
        .unwrap_or(BOOL(1))
        .as_bool();
    let offscreen = unsafe { element.CachedIsOffscreen() }
        .unwrap_or(BOOL(0))
        .as_bool();
    let visible = !offscreen;
    let focused = unsafe { element.CachedHasKeyboardFocus() }
        .unwrap_or(BOOL(0))
        .as_bool();

    // Checked: from TogglePattern
    let checked = match role {
        Role::CheckBox | Role::RadioButton => {
            if let Some(ref pattern) = patterns.toggle {
                match unsafe { pattern.CurrentToggleState() } {
                    Ok(ToggleState_On) => Some(Toggled::On),
                    Ok(ToggleState_Off) => Some(Toggled::Off),
                    Ok(ToggleState_Indeterminate) => Some(Toggled::Mixed),
                    _ => Some(Toggled::Off),
                }
            } else if let Some(ref pattern) = patterns.selection_item {
                // For radio buttons, check SelectionItemPattern
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
        _ => None,
    };

    // Expanded: from ExpandCollapsePattern
    let expanded = if let Some(ref pattern) = patterns.expand_collapse {
        match unsafe { pattern.CurrentExpandCollapseState() } {
            Ok(ExpandCollapseState_Expanded) => Some(true),
            Ok(ExpandCollapseState_Collapsed) => Some(false),
            _ => None,
        }
    } else {
        None
    };

    // Selected: from SelectionItemPattern
    let selected = if let Some(ref pattern) = patterns.selection_item {
        unsafe { pattern.CurrentIsSelected() }
            .unwrap_or(BOOL(0))
            .as_bool()
    } else {
        false
    };

    let editable = match role {
        Role::TextField | Role::TextArea => {
            if let Some(ref pattern) = patterns.value {
                unsafe { pattern.CurrentIsReadOnly() }.unwrap_or(BOOL(1)) == BOOL(0)
            } else {
                true
            }
        }
        _ => false,
    };

    let focusable = unsafe { element.CachedIsKeyboardFocusable() }.unwrap_or(FALSE) == TRUE;

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
        UIA_ThumbControlTypeId => Role::ScrollThumb,
        UIA_TitleBarControlTypeId => Role::Group,
        UIA_ToolTipControlTypeId => Role::Tooltip,
        UIA_CalendarControlTypeId => Role::Group,
        UIA_CustomControlTypeId => Role::Unknown,
        _ => xa11y_core::unknown_role(&format!("UIA control type {}", control_type.0)),
    }
}

// ── Event subscription (polling-based) ───────────────────────────────────────

impl WindowsProvider {
    /// Spawn a polling thread that detects focus and structure changes.
    fn subscribe_impl<F>(&self, app_name: String, app_pid: u32, root_fn: F) -> Result<Subscription>
    where
        F: Fn(&WindowsProvider) -> Result<IUIAutomationElement> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let poll_provider = WindowsProvider::new()?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut prev_focused: Option<String> = None;
            let mut prev_element_count: usize = 0;

            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));

                let root_uia = match root_fn(&poll_provider) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Fetch entire subtree with all properties in one COM call
                let all_elements: Vec<ElementData> =
                    if let Ok(arr) = poll_provider.find_all_subtree(&root_uia) {
                        let count = uia_len(&arr);
                        (0..count)
                            .filter_map(|i| uia_get(&arr, i))
                            .map(|el| poll_provider.build_element_data(&el, Some(app_pid)))
                            .collect()
                    } else {
                        continue;
                    };

                // Detect focus changes
                let focused_name = all_elements
                    .iter()
                    .find(|n| n.states.focused)
                    .and_then(|n| n.name.clone());
                if focused_name != prev_focused {
                    if prev_focused.is_some() {
                        let _ = tx.send(Event {
                            event_type: EventType::FocusChanged,
                            app_name: app_name.clone(),
                            app_pid,
                            target: all_elements.iter().find(|n| n.states.focused).cloned(),
                            state_flag: None,
                            state_value: None,
                            text_change: None,
                            timestamp: std::time::Instant::now(),
                        });
                    }
                    prev_focused = focused_name;
                }

                // Detect structure changes
                let element_count = all_elements.len();
                if element_count != prev_element_count && prev_element_count > 0 {
                    let _ = tx.send(Event {
                        event_type: EventType::StructureChanged,
                        app_name: app_name.clone(),
                        app_pid,
                        target: None,
                        state_flag: None,
                        state_value: None,
                        text_change: None,
                        timestamp: std::time::Instant::now(),
                    });
                }
                prev_element_count = element_count;
            }
        });

        let cancel = CancelHandle::new(move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.join();
        });

        Ok(Subscription::new(EventReceiver::new(rx), cancel))
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
            map_uia_control_type(UIA_ThumbControlTypeId),
            Role::ScrollThumb
        );
        assert_eq!(
            map_uia_control_type(UIA_CONTROLTYPE_ID(99999)),
            Role::Unknown
        );
    }
}
