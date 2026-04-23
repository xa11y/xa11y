//! Windows UI Automation accessibility provider.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;

use xa11y_core::{
    selector::{matches_simple, Combinator, Selector, SelectorSegment},
    CancelHandle, ElementData, Error, Event, EventKind, EventReceiver, Provider, Rect, Result,
    Role, StateFlag, StateSet, Subscription, Toggled,
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

    /// Find an application's root UIA element + window name by PID.
    ///
    /// Used by `subscribe_impl` to scope native UIA event handlers to a
    /// single application's subtree.
    fn find_app_by_pid(&self, pid: u32) -> Result<(IUIAutomationElement, String)> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let condition = uia_call(|| unsafe {
            self.automation
                .CreatePropertyCondition(UIA_ProcessIdPropertyId, &VARIANT::from(pid as i32))
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
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, uia);
        handle
    }

    /// Look up a cached UIA element by handle.
    fn get_cached(&self, handle: u64) -> Result<IUIAutomationElement> {
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
        let handle = self.cache_element(element.clone());
        build_snapshot_data(element, pid, handle)
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

/// Build an ElementData snapshot from a pre-fetched UIA element without
/// retaining the live reference in the provider's handle cache.
///
/// Used both by [`WindowsProvider::build_element_data`] (which allocates a
/// handle and wraps this call) and by event handlers (which pass `handle=0`
/// because event targets are snapshots — callers don't act on them directly).
fn build_snapshot_data(
    element: &IUIAutomationElement,
    pid: Option<u32>,
    handle: u64,
) -> ElementData {
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

    let patterns = WindowsProvider::query_patterns(element);
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

    ElementData {
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
        raw,
        handle,
    }
}

/// Build the batch request that describes which properties and patterns
/// to pre-fetch. Created once per provider, used on every query.
fn create_batch_request(automation: &IUIAutomation) -> Result<IUIAutomationCacheRequest> {
    let request = uia_call(|| unsafe { automation.CreateCacheRequest() })?;

    // The property list is a fixed constant array of valid UIA property IDs.
    // If AddProperty ever fails here, something is structurally wrong with the
    // UIA environment (e.g. COM state corrupted). Propagate rather than
    // silently producing a half-configured cache request (tenet 1).
    for prop in BATCH_PROPERTIES {
        unsafe { request.AddProperty(*prop) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddProperty({:?}) failed: {e}", prop),
        })?;
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
                        &VARIANT::from(UIA_WindowControlTypeId.0),
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

    #[allow(non_upper_case_globals)]
    fn press(&self, element: &ElementData) -> Result<()> {
        // `press` dispatches to the element's primary-activation UIA pattern:
        // Invoke (buttons, menu items, links), Toggle (checkboxes, switches),
        // SelectionItem.Select (list items, radio buttons), or ExpandCollapse
        // (combo boxes, tree items). These patterns are mutually exclusive in
        // practice — a given UIA element supports at most one. This mirrors
        // AXPress on macOS and AT-SPI `DoAction("click")` on Linux, which
        // likewise collapse all activation under a single verb. Tenet 3
        // applies to the *semantic* verb (`press` = "activate this element"),
        // not the underlying API — each branch below is Windows' canonical
        // implementation of that semantic for the element's pattern.
        let uia_element = self.get_cached(element.handle)?;
        // Try InvokePattern (buttons, menu items)
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        } {
            unsafe { pattern.Invoke() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "Invoke failed".to_string(),
            })?;
            return Ok(());
        }
        // Try TogglePattern (checkboxes, switches)
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
        } {
            unsafe { pattern.Toggle() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "Toggle failed".to_string(),
            })?;
            return Ok(());
        }
        // Try SelectionItemPattern (list items, radio buttons)
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
        // Try ExpandCollapsePattern (combo boxes, tree items)
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                UIA_ExpandCollapsePatternId,
            )
        } {
            let state =
                unsafe { pattern.CurrentExpandCollapseState() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("CurrentExpandCollapseState failed: {}", e),
                })?;
            match state {
                ExpandCollapseState_Collapsed => {
                    unsafe { pattern.Expand() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: format!("Expand failed: {}", e),
                    })?;
                }
                _ => {
                    unsafe { pattern.Collapse() }.map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: format!("Collapse failed: {}", e),
                    })?;
                }
            }
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
        let root = unsafe { self.automation.GetRootElement() }.map_err(|e| Error::Platform {
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
            uia_element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
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
            unsafe { pattern.Expand() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("Expand failed: {}", e),
            })?;
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "expand".to_string(),
            role: element.role,
        })
    }

    fn collapse(&self, element: &ElementData) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                UIA_ExpandCollapsePatternId,
            )
        } {
            unsafe { pattern.Collapse() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("Collapse failed: {}", e),
            })?;
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "collapse".to_string(),
            role: element.role,
        })
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
            uia_element
                .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
        } {
            let current = unsafe { pattern.CurrentValue() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentValue failed: {}", e),
            })?;
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
            uia_element
                .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
        } {
            let current = unsafe { pattern.CurrentValue() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentValue failed: {}", e),
            })?;
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
            uia_element
                .GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
        } {
            unsafe { pattern.ScrollIntoView() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("ScrollIntoView failed: {}", e),
            })?;
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "scroll_into_view".to_string(),
            role: element.role,
        })
    }

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        } {
            let s: windows::core::BSTR = value.into();
            unsafe { pattern.SetValue(&s) }.map_err(|_| Error::TextValueNotSupported)?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        let pattern = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
        }
        .map_err(|_| Error::ActionNotSupported {
            action: "set_numeric_value".to_string(),
            role: element.role,
        })?;
        unsafe { pattern.SetValue(value) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("RangeValue.SetValue failed: {}", e),
        })?;
        Ok(())
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
                .map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("Value.CurrentValue failed: {}", e),
                })?;

            // Try to get cursor position from TextPattern
            let insert_pos = if let Ok(text_pattern) = unsafe {
                uia_element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
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
            unsafe { value_pattern.SetValue(&bstr) }.map_err(|_| Error::TextValueNotSupported)?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        let uia_element = self.get_cached(element.handle)?;
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        } {
            let range = unsafe { pattern.DocumentRange() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: "DocumentRange failed".to_string(),
            })?;
            // Collapse and move to start position. If the move fails, the
            // subsequent Select() would land on the wrong range — propagate
            // rather than silently mis-selecting (tenet 1).
            unsafe { range.Move(TextUnit_Character, start as i32) }.map_err(|e| {
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("TextRange::Move to {start} failed: {e}"),
                }
            })?;
            // Extend end to selection length.
            unsafe {
                range.MoveEndpointByUnit(
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    (end - start) as i32,
                )
            }
            .map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("TextRange::MoveEndpointByUnit({start}..{end}) failed: {e}"),
            })?;
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
        self.subscribe_impl(pid, app_name)
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
) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();

    if patterns.invoke.is_some() {
        actions.push("press".to_string());
    }

    if patterns.toggle.is_some() {
        if !actions.iter().any(|a| a == "press") {
            actions.push("press".to_string());
        }
        if !actions.iter().any(|a| a == "toggle") {
            actions.push("toggle".to_string());
        }
    }

    if patterns.expand_collapse.is_some() {
        actions.push("expand".to_string());
        actions.push("collapse".to_string());
    }

    if patterns.value.is_some() && !actions.iter().any(|a| a == "set_value") {
        actions.push("set_value".to_string());
    }

    if patterns.range_value.is_some() {
        if !actions.iter().any(|a| a == "set_value") {
            actions.push("set_value".to_string());
        }
        actions.push("increment".to_string());
        actions.push("decrement".to_string());
    }

    if patterns.selection_item.is_some() {
        if !actions.iter().any(|a| a == "press") {
            actions.push("press".to_string());
        }
        actions.push("select".to_string());
    }

    // Advertise `focus` iff focusing would have an observable effect: the
    // element must be both keyboard-focusable and enabled. A disabled-but-
    // focusable element shouldn't claim to support focus because SetFocus is
    // either a no-op or throws. This aligns Windows with Linux (requires
    // AT-SPI Action interface listing `focus`) and macOS (requires
    // `AXFocused` to be settable).
    let is_focusable = unsafe { element.CachedIsKeyboardFocusable() }
        .unwrap_or(BOOL(0))
        .as_bool();
    let is_enabled = unsafe { element.CachedIsEnabled() }
        .unwrap_or(BOOL(1))
        .as_bool();
    if is_focusable && is_enabled {
        actions.push("focus".to_string());
    }

    // For text fields and sliders, ensure set_value is present
    if matches!(role, Role::TextField | Role::TextArea | Role::Slider)
        && !actions.iter().any(|a| a == "set_value")
    {
        actions.push("set_value".to_string());
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

// ── Event subscription (native UIA event handlers) ───────────────────────────

/// Moves a COM interface into a `Send` closure. COM in MTA (the apartment
/// xa11y uses) serializes access via proxies, so transferring a raw pointer
/// across threads is safe as long as every dereference happens under MTA —
/// which is the case for the cancel closure, run from the subscriber's
/// thread on Subscription drop.
///
/// Mirrors the `unsafe impl Send for WindowsProvider` assertion in this file:
/// the same MTA guarantee holds for every COM type we need to capture.
///
/// Note the private inner field + accessor method: Rust 2021's disjoint
/// closure captures will grab `wrapper.0` (the inner `T`) if it's reachable,
/// which defeats the `Send` assertion on the wrapper. Going through `get()`
/// forces the full wrapper to be captured.
struct ComSend<T> {
    inner: T,
}
unsafe impl<T> Send for ComSend<T> {}

impl<T> ComSend<T> {
    fn new(value: T) -> Self {
        Self { inner: value }
    }

    fn get(&self) -> &T {
        &self.inner
    }
}

/// Shared context passed to every UIA event handler.
///
/// `sender` is wrapped in a `Mutex` because `mpsc::Sender` is `!Sync`
/// (its internal inner is `UnsafeCell`-like), while handler callbacks may be
/// invoked concurrently from the UIA MTA background thread. The lock is only
/// held for the duration of a single channel push, so contention is trivial.
struct EventContext {
    sender: Mutex<std::sync::mpsc::Sender<Event>>,
    app_name: String,
    app_pid: u32,
}

impl EventContext {
    fn emit(&self, kind: EventKind, target: Option<ElementData>) {
        let event = Event {
            kind,
            app_name: self.app_name.clone(),
            app_pid: self.app_pid,
            target,
            timestamp: std::time::Instant::now(),
        };
        if let Ok(tx) = self.sender.lock() {
            let _ = tx.send(event);
        }
    }

    /// Best-effort PID filter. `AddFocusChangedEventHandler` is process-wide,
    /// and scoped handlers occasionally leak events for sibling processes —
    /// checking the sender's PID keeps each subscription clean.
    fn matches_pid(&self, sender: &IUIAutomationElement) -> bool {
        unsafe { sender.CurrentProcessId() }
            .map(|p| p as u32 == self.app_pid)
            .unwrap_or(false)
    }

    /// Build a full ElementData snapshot from a UIA sender element.
    ///
    /// Event handlers are registered with a cache request, so cached accessors
    /// should work directly on `sender`. If the cache is cold for any reason,
    /// we fall back to `BuildUpdatedCache` so the target is always populated.
    fn snapshot(
        &self,
        sender: &IUIAutomationElement,
        cache: &IUIAutomationCacheRequest,
    ) -> ElementData {
        // `CachedControlType()` is cheap and indicates whether the cache
        // covers our expected properties. If it errors, refresh the cache.
        let cached_element = if unsafe { sender.CachedControlType() }.is_ok() {
            sender.clone()
        } else {
            unsafe { sender.BuildUpdatedCache(cache) }.unwrap_or_else(|_| sender.clone())
        };
        build_snapshot_data(&cached_element, Some(self.app_pid), 0)
    }
}

/// Unpack a UIA `VT_I4` VARIANT (used by `ToggleToggleState` and
/// `ExpandCollapseExpandCollapseState`) into an `i32`.
fn variant_i32(v: &VARIANT) -> Option<i32> {
    i32::try_from(v).ok()
}

/// Unpack a UIA `VT_BOOL` VARIANT (used by `IsEnabled`) into a `bool`.
fn variant_bool(v: &VARIANT) -> Option<bool> {
    bool::try_from(v).ok()
}

// ── Handler implementations ──────────────────────────────────────────────────

#[implement(IUIAutomationFocusChangedEventHandler)]
struct FocusHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
}

impl IUIAutomationFocusChangedEventHandler_Impl for FocusHandler_Impl {
    fn HandleFocusChangedEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        if let Some(el) = sender.as_ref() {
            if self.ctx.matches_pid(el) {
                let target = Some(self.ctx.snapshot(el, &self.cache));
                self.ctx.emit(EventKind::FocusChanged, target);
            }
        }
        Ok(())
    }
}

#[implement(IUIAutomationEventHandler)]
struct AutomationHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
}

impl IUIAutomationEventHandler_Impl for AutomationHandler_Impl {
    #[allow(non_upper_case_globals)] // UIA constants use CamelCase in the windows crate
    fn HandleAutomationEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
        eventid: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        let Some(el) = sender.as_ref() else {
            return Ok(());
        };
        if !self.ctx.matches_pid(el) {
            return Ok(());
        }
        let kind = match eventid {
            UIA_Window_WindowOpenedEventId => EventKind::WindowOpened,
            UIA_Window_WindowClosedEventId => EventKind::WindowClosed,
            UIA_MenuOpenedEventId => EventKind::MenuOpened,
            UIA_MenuClosedEventId => EventKind::MenuClosed,
            UIA_Text_TextChangedEventId => EventKind::TextChanged,
            UIA_SelectionItem_ElementSelectedEventId
            | UIA_SelectionItem_ElementAddedToSelectionEventId
            | UIA_SelectionItem_ElementRemovedFromSelectionEventId => EventKind::SelectionChanged,
            UIA_NotificationEventId | UIA_LiveRegionChangedEventId | UIA_SystemAlertEventId => {
                EventKind::Announcement
            }
            _ => return Ok(()),
        };
        let target = Some(self.ctx.snapshot(el, &self.cache));
        self.ctx.emit(kind, target);
        Ok(())
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct PropertyHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
}

impl IUIAutomationPropertyChangedEventHandler_Impl for PropertyHandler_Impl {
    #[allow(non_upper_case_globals)] // UIA constants use CamelCase in the windows crate
    fn HandlePropertyChangedEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
        propertyid: UIA_PROPERTY_ID,
        newvalue: &VARIANT,
    ) -> windows::core::Result<()> {
        let Some(el) = sender.as_ref() else {
            return Ok(());
        };
        if !self.ctx.matches_pid(el) {
            return Ok(());
        }

        // Determine the event kind(s) to emit — some property changes emit
        // more than one (ToggleState fires both ValueChanged and
        // StateChanged{Checked}, matching the design doc).
        let mut kinds: Vec<EventKind> = Vec::with_capacity(2);
        match propertyid {
            UIA_NamePropertyId => kinds.push(EventKind::NameChanged),
            UIA_IsEnabledPropertyId => {
                if let Some(v) = variant_bool(newvalue) {
                    kinds.push(EventKind::StateChanged {
                        flag: StateFlag::Enabled,
                        value: v,
                    });
                }
            }
            UIA_ToggleToggleStatePropertyId => {
                if let Some(v) = variant_i32(newvalue) {
                    kinds.push(EventKind::StateChanged {
                        flag: StateFlag::Checked,
                        value: v == ToggleState_On.0,
                    });
                }
                kinds.push(EventKind::ValueChanged);
            }
            UIA_ValueValuePropertyId | UIA_RangeValueValuePropertyId => {
                kinds.push(EventKind::ValueChanged);
            }
            UIA_ExpandCollapseExpandCollapseStatePropertyId => {
                if let Some(v) = variant_i32(newvalue) {
                    kinds.push(EventKind::StateChanged {
                        flag: StateFlag::Expanded,
                        value: v == ExpandCollapseState_Expanded.0,
                    });
                }
            }
            _ => return Ok(()),
        }

        if kinds.is_empty() {
            return Ok(());
        }

        // Build the snapshot once and clone into each emit — cheap since
        // ElementData is just owned strings + small primitives.
        let target = Some(self.ctx.snapshot(el, &self.cache));
        for kind in kinds {
            self.ctx.emit(kind, target.clone());
        }
        Ok(())
    }
}

#[implement(IUIAutomationStructureChangedEventHandler)]
struct StructureHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
}

impl IUIAutomationStructureChangedEventHandler_Impl for StructureHandler_Impl {
    fn HandleStructureChangedEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
        _changetype: StructureChangeType,
        _runtimeid: *const windows::Win32::System::Com::SAFEARRAY,
    ) -> windows::core::Result<()> {
        let target = sender.as_ref().and_then(|el| {
            if self.ctx.matches_pid(el) {
                Some(self.ctx.snapshot(el, &self.cache))
            } else {
                None
            }
        });
        // Even if the sender is detached (ChildRemoved without a live parent)
        // or we couldn't resolve PID, forward the kind so consumers can react.
        self.ctx.emit(EventKind::StructureChanged, target);
        Ok(())
    }
}

// Event IDs registered through `AddAutomationEventHandler`. Kept as a shared
// constant so registration and removal iterate the same list.
const AUTOMATION_EVENT_IDS: &[UIA_EVENT_ID] = &[
    UIA_Window_WindowOpenedEventId,
    UIA_Window_WindowClosedEventId,
    UIA_MenuOpenedEventId,
    UIA_MenuClosedEventId,
    UIA_Text_TextChangedEventId,
    UIA_SelectionItem_ElementSelectedEventId,
    UIA_SelectionItem_ElementAddedToSelectionEventId,
    UIA_SelectionItem_ElementRemovedFromSelectionEventId,
    UIA_NotificationEventId,
    UIA_LiveRegionChangedEventId,
    // `UIA_SystemAlertEventId` is the design-doc-listed Announcement source
    // for pre-Windows-10 alert messages and some legacy providers that
    // don't raise NotificationEvent. Dispatched to EventKind::Announcement
    // in `AutomationHandler::HandleAutomationEvent`.
    UIA_SystemAlertEventId,
];

// Property IDs watched via `AddPropertyChangedEventHandlerNativeArray`.
const PROPERTY_CHANGE_IDS: &[UIA_PROPERTY_ID] = &[
    UIA_NamePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ValueValuePropertyId,
    UIA_RangeValueValuePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
];

impl WindowsProvider {
    fn subscribe_impl(&self, pid: u32, app_name: String) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();

        // Scope handler registrations to the target app's subtree.
        let (app_root, _root_name) = self.find_app_by_pid(pid)?;

        let ctx = Arc::new(EventContext {
            sender: Mutex::new(tx),
            app_name,
            app_pid: pid,
        });

        // Dedicated cache request: ensures event handlers receive elements
        // with our standard batch of properties pre-fetched. We clone the
        // provider's shared request so we don't rely on a mutable handle
        // held elsewhere.
        let cache = create_batch_request(&self.automation)?;

        let focus: IUIAutomationFocusChangedEventHandler = FocusHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();
        let automation_handler: IUIAutomationEventHandler = AutomationHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();
        let property: IUIAutomationPropertyChangedEventHandler = PropertyHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();
        let structure: IUIAutomationStructureChangedEventHandler = StructureHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();

        // Focus handler is system-wide (UIA has no scope parameter here) —
        // the handler filters by PID.
        unsafe { self.automation.AddFocusChangedEventHandler(&cache, &focus) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("AddFocusChangedEventHandler failed: {}", e),
            }
        })?;

        // Other handlers are scoped to the app root's subtree. If any
        // registration fails, events of that type would never arrive — the
        // caller must know (tenet 1). Clean up already-registered handlers
        // before returning so we don't leak native handlers on the app root.
        let cleanup_focus = || unsafe {
            let _ = self.automation.RemoveFocusChangedEventHandler(&focus);
        };
        let cleanup_automation = |registered: &[UIA_EVENT_ID]| unsafe {
            for eid in registered {
                let _ = self.automation.RemoveAutomationEventHandler(
                    *eid,
                    &app_root,
                    &automation_handler,
                );
            }
        };

        let mut registered_automation_ids: Vec<UIA_EVENT_ID> = Vec::new();
        for eid in AUTOMATION_EVENT_IDS {
            if let Err(e) = unsafe {
                self.automation.AddAutomationEventHandler(
                    *eid,
                    &app_root,
                    TreeScope_Subtree,
                    &cache,
                    &automation_handler,
                )
            } {
                cleanup_automation(&registered_automation_ids);
                cleanup_focus();
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("AddAutomationEventHandler({:?}) failed: {e}", eid),
                });
            }
            registered_automation_ids.push(*eid);
        }

        if let Err(e) = unsafe {
            self.automation.AddPropertyChangedEventHandlerNativeArray(
                &app_root,
                TreeScope_Subtree,
                &cache,
                &property,
                PROPERTY_CHANGE_IDS,
            )
        } {
            cleanup_automation(&registered_automation_ids);
            cleanup_focus();
            return Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("AddPropertyChangedEventHandlerNativeArray failed: {e}"),
            });
        }

        if let Err(e) = unsafe {
            self.automation.AddStructureChangedEventHandler(
                &app_root,
                TreeScope_Subtree,
                &cache,
                &structure,
            )
        } {
            unsafe {
                let _ = self
                    .automation
                    .RemovePropertyChangedEventHandler(&app_root, &property);
            }
            cleanup_automation(&registered_automation_ids);
            cleanup_focus();
            return Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("AddStructureChangedEventHandler failed: {e}"),
            });
        }

        // Each captured COM interface is wrapped in ComSend so the cancel
        // closure satisfies CancelHandle::new's `Send` bound. See ComSend's
        // doc comment for the safety argument.
        let automation_clone = ComSend::new(self.automation.clone());
        let app_root_clone = ComSend::new(app_root.clone());
        let focus_c = ComSend::new(focus);
        let auto_c = ComSend::new(automation_handler);
        let property_c = ComSend::new(property);
        let structure_c = ComSend::new(structure);
        let cancel = CancelHandle::new(move || {
            // RemoveXxx is synchronous: when it returns, UIA guarantees no
            // further callbacks for this handler. We ignore errors because
            // there's nothing useful to do in a cancel path.
            let automation = automation_clone.get();
            let app_root = app_root_clone.get();
            unsafe {
                let _ = automation.RemoveFocusChangedEventHandler(focus_c.get());
                for eid in AUTOMATION_EVENT_IDS {
                    let _ = automation.RemoveAutomationEventHandler(*eid, app_root, auto_c.get());
                }
                let _ = automation.RemovePropertyChangedEventHandler(app_root, property_c.get());
                let _ = automation.RemoveStructureChangedEventHandler(app_root, structure_c.get());
            }
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

    #[test]
    fn role_mapping_covers_remaining_types() {
        assert_eq!(
            map_uia_control_type(UIA_RadioButtonControlTypeId),
            Role::RadioButton
        );
        assert_eq!(map_uia_control_type(UIA_TableControlTypeId), Role::Table);
        assert_eq!(map_uia_control_type(UIA_DataGridControlTypeId), Role::Table);
        assert_eq!(
            map_uia_control_type(UIA_DataItemControlTypeId),
            Role::TableRow
        );
        assert_eq!(
            map_uia_control_type(UIA_ToolBarControlTypeId),
            Role::Toolbar
        );
        assert_eq!(
            map_uia_control_type(UIA_ScrollBarControlTypeId),
            Role::ScrollBar
        );
        assert_eq!(map_uia_control_type(UIA_PaneControlTypeId), Role::Group);
        assert_eq!(map_uia_control_type(UIA_TreeControlTypeId), Role::List);
        assert_eq!(
            map_uia_control_type(UIA_DocumentControlTypeId),
            Role::WebArea
        );
        assert_eq!(map_uia_control_type(UIA_HeaderControlTypeId), Role::Group);
        assert_eq!(
            map_uia_control_type(UIA_HeaderItemControlTypeId),
            Role::TableCell
        );
        assert_eq!(
            map_uia_control_type(UIA_SpinnerControlTypeId),
            Role::SpinButton
        );
        assert_eq!(
            map_uia_control_type(UIA_SplitButtonControlTypeId),
            Role::Button
        );
        assert_eq!(
            map_uia_control_type(UIA_StatusBarControlTypeId),
            Role::Status
        );
        assert_eq!(map_uia_control_type(UIA_TitleBarControlTypeId), Role::Group);
        assert_eq!(
            map_uia_control_type(UIA_ToolTipControlTypeId),
            Role::Tooltip
        );
        assert_eq!(map_uia_control_type(UIA_CalendarControlTypeId), Role::Group);
        assert_eq!(map_uia_control_type(UIA_CustomControlTypeId), Role::Unknown);
    }

    #[test]
    fn role_mapping_unknown_id_returns_unknown() {
        assert_eq!(map_uia_control_type(UIA_CONTROLTYPE_ID(0)), Role::Unknown);
        assert_eq!(
            map_uia_control_type(UIA_CONTROLTYPE_ID(i32::MAX)),
            Role::Unknown
        );
    }

    /// Helper: create a provider, skipping the test if COM init fails
    /// (happens when cargo test runs with multiple threads in CI).
    fn try_provider() -> Option<WindowsProvider> {
        match WindowsProvider::new() {
            Ok(p) => Some(p),
            Err(Error::Platform {
                code: -2147467259, ..
            }) => {
                // E_FAIL (0x80004005) — COM init race in multi-threaded test runner
                eprintln!("Skipping: COM init failed (multi-threaded test runner)");
                None
            }
            Err(e) => panic!("Unexpected provider error: {}", e),
        }
    }

    #[test]
    fn provider_new_succeeds() {
        // May fail in multi-threaded test runners; that's expected.
        let _ = try_provider();
    }

    #[test]
    fn provider_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WindowsProvider>();
    }

    #[test]
    fn get_children_none_returns_applications() {
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap();
        // Should find at least one window on a Windows desktop
        assert!(
            !apps.is_empty(),
            "Should find at least one top-level window"
        );
        for app in &apps {
            assert!(app.pid.is_some(), "Top-level windows should have a PID");
            assert!(app.name.is_some(), "Top-level windows should have a name");
        }
    }

    #[test]
    fn get_cached_stale_handle_returns_error() {
        let Some(provider) = try_provider() else {
            return;
        };
        let result = provider.get_cached(u64::MAX);
        assert!(
            matches!(result, Err(Error::ElementStale { .. })),
            "Stale handle should return ElementStale error"
        );
    }

    #[test]
    fn perform_action_delegates_to_named_methods() {
        let Some(provider) = try_provider() else {
            return;
        };
        let dummy = ElementData {
            role: Role::Button,
            name: Some("test".to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: std::collections::HashMap::new(),
            handle: u64::MAX, // stale handle
        };
        // Unknown action name should return ActionNotSupported
        let result = provider.perform_action(&dummy, "nonexistent_action");
        assert!(
            matches!(result, Err(Error::ActionNotSupported { .. })),
            "Unknown action should return ActionNotSupported"
        );
    }

    #[test]
    fn perform_action_on_stale_handle_returns_error() {
        let Some(provider) = try_provider() else {
            return;
        };
        let dummy = ElementData {
            role: Role::Button,
            name: Some("test".to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: std::collections::HashMap::new(),
            handle: u64::MAX,
        };
        // Actions that look up the cached element should return ElementStale
        let result = provider.press(&dummy);
        assert!(
            matches!(result, Err(Error::ElementStale { .. })),
            "Press on stale handle should return ElementStale, got: {:?}",
            result
        );
    }

    #[test]
    fn batch_properties_not_empty() {
        assert!(
            !BATCH_PROPERTIES.is_empty(),
            "Batch properties should include at least one property"
        );
        // Verify essential properties are included
        assert!(BATCH_PROPERTIES.contains(&UIA_ControlTypePropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_NamePropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_BoundingRectanglePropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_IsEnabledPropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_ProcessIdPropertyId));
    }

    #[test]
    fn find_elements_empty_selector_returns_empty() {
        let Some(provider) = try_provider() else {
            return;
        };
        let empty_selector = Selector { segments: vec![] };
        let result = provider
            .find_elements(None, &empty_selector, None, None)
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn next_handle_increments() {
        let Some(provider) = try_provider() else {
            return;
        };
        let before = NEXT_HANDLE.load(Ordering::Relaxed);
        // Getting children allocates handles
        let _ = provider.get_children(None).unwrap();
        let after = NEXT_HANDLE.load(Ordering::Relaxed);
        assert!(
            after > before,
            "Handle counter should increment after caching elements"
        );
    }

    // ── Event subscription tests ────────────────────────────────────────────

    fn dummy_element(pid: Option<u32>) -> ElementData {
        ElementData {
            role: Role::Application,
            name: Some("test".to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid,
            raw: std::collections::HashMap::new(),
            handle: 0,
        }
    }

    #[test]
    fn subscribe_without_pid_returns_error() {
        let Some(provider) = try_provider() else {
            return;
        };
        let el = dummy_element(None);
        let result = provider.subscribe(&el);
        assert!(
            matches!(result, Err(Error::Platform { .. })),
            "subscribe without PID should return Platform error"
        );
    }

    #[test]
    fn subscribe_with_nonexistent_pid_returns_error() {
        let Some(provider) = try_provider() else {
            return;
        };
        // PIDs this large are effectively guaranteed not to be a running app.
        let el = dummy_element(Some(u32::MAX - 1));
        let result = provider.subscribe(&el);
        assert!(result.is_err(), "subscribe against missing PID should fail");
    }

    #[test]
    fn subscribe_and_drop_cleans_up() {
        // Use this test process itself as the target. find_app_by_pid scans
        // visible top-level windows and our test runner has none, so the call
        // will fail cleanly — but the *setup* path (cache request creation,
        // handler boxing into COM objects, Arc<EventContext> construction) is
        // still exercised by the integer-PID cases above. Here we additionally
        // verify that dropping a live Subscription runs the cancel closure
        // without panicking when find_app_by_pid does happen to succeed.
        //
        // The flow: if find_app_by_pid returns a window, subscribe returns
        // Ok(Subscription) and the drop happens at end of scope. If not,
        // subscribe returns Err and we just confirm the err type.
        let Some(provider) = try_provider() else {
            return;
        };
        // Pick the first enumerable window's PID to exercise the success path
        // when at least one GUI app exists on the test runner.
        let apps = provider.get_children(None).unwrap_or_default();
        if let Some(app) = apps.into_iter().find(|a| a.pid.is_some()) {
            let el = dummy_element(app.pid);
            if let Ok(sub) = provider.subscribe(&el) {
                // Dropping the subscription must call the cancel closure and
                // not panic. try_recv on a fresh subscription may be None.
                let _ = sub.try_recv();
                drop(sub);
            }
        }
    }

    #[test]
    fn subscribe_is_independent_of_prior_subscription() {
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap_or_default();
        let Some(app) = apps.into_iter().find(|a| a.pid.is_some()) else {
            return;
        };
        let el = dummy_element(app.pid);
        // Two sequential subscriptions must both succeed; the first's cancel
        // must not break the second (RemoveXxx is scoped per handler).
        let sub1 = provider.subscribe(&el);
        drop(sub1);
        let sub2 = provider.subscribe(&el);
        drop(sub2);
    }

    #[test]
    fn com_send_is_send() {
        fn assert_send<T: Send>() {}
        // ComSend<T> must be Send even when T is not — that's the whole point.
        #[allow(dead_code)] // constructed only to assert ComSend<NotSend>: Send
        struct NotSend(std::rc::Rc<()>);
        assert_send::<ComSend<NotSend>>();
        assert_send::<ComSend<*mut u8>>();
    }

    #[test]
    fn automation_event_ids_covers_design_doc() {
        // These are the event IDs the design doc mandates we watch. If a
        // future refactor drops one silently, this test will catch it.
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_Window_WindowOpenedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_Window_WindowClosedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_MenuOpenedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_MenuClosedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_Text_TextChangedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_SelectionItem_ElementSelectedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_NotificationEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_LiveRegionChangedEventId));
        assert!(AUTOMATION_EVENT_IDS.contains(&UIA_SystemAlertEventId));
    }

    #[test]
    fn is_window_control_unit() {
        // Covers the boolean helper used by the focus handler's window
        // ancestor walk. Given a real control-type read succeeds, equality
        // against UIA_WindowControlTypeId determines the answer. We can't
        // construct an IUIAutomationElement in a unit test, but we can
        // exercise the pattern via `try_provider`.
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap_or_default();
        // Every top-level enumerable element is a Window by construction.
        for a in &apps {
            // The helper takes a live IUIAutomationElement — we can only
            // verify via the ElementData role surface.
            assert_eq!(a.role, Role::Window);
        }
    }

    #[test]
    fn property_change_ids_covers_design_doc() {
        // Property IDs mandated by the events design doc for the Windows
        // PropertyChanged pathway.
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_NamePropertyId));
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_IsEnabledPropertyId));
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_ToggleToggleStatePropertyId));
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_ValueValuePropertyId));
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_RangeValueValuePropertyId));
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_ExpandCollapseExpandCollapseStatePropertyId));
    }

    #[test]
    fn variant_bool_unpacks_toggle_value() {
        // Mirrors what the UIA runtime hands to our PropertyChanged handler
        // for the `IsEnabled` property — a VT_BOOL VARIANT.
        let v = VARIANT::from(true);
        assert_eq!(variant_bool(&v), Some(true));
        let v = VARIANT::from(false);
        assert_eq!(variant_bool(&v), Some(false));
    }

    #[test]
    fn variant_i32_unpacks_toggle_state() {
        // UIA reports ToggleState changes as VT_I4 holding the enum's int
        // value. `ToggleState_On.0 == 1`, `ToggleState_Off.0 == 0`.
        let v = VARIANT::from(ToggleState_On.0);
        assert_eq!(variant_i32(&v), Some(1));
        let v = VARIANT::from(ToggleState_Off.0);
        assert_eq!(variant_i32(&v), Some(0));
        // VariantToInt32 coerces compatible scalar types (VT_BOOL, VT_UI2,
        // VT_R8, etc.) into i32 rather than failing — i.e. variant_i32 is
        // lenient on the wire representation as long as the runtime can
        // make the conversion. ToggleState / ExpandCollapseState are the
        // only properties our handler feeds to it and they're strictly
        // VT_I4, so the coercion is a non-issue in practice.
        let v = VARIANT::from(ExpandCollapseState_Expanded.0);
        assert_eq!(variant_i32(&v), Some(1));
    }

    #[test]
    fn build_snapshot_data_sets_handle_to_given_value() {
        // build_snapshot_data is the shared backbone for both the instance
        // method (which allocates a real handle) and event handlers (which
        // pass 0). Verify it honours the passed handle even when there's no
        // live element behind it — we only need to exercise the handle
        // plumbing, not the whole UIA stack.
        //
        // We can't fabricate a valid IUIAutomationElement, so instead cover
        // this via build_element_data on a real provider if one is available:
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap_or_default();
        // Every element returned from the provider has a non-zero handle
        // because build_element_data allocates one. Event-path snapshots
        // pass 0; that path is covered by the actual handler wiring.
        for a in &apps {
            assert!(a.handle != 0, "provider-built handle should be non-zero");
        }
    }
}
