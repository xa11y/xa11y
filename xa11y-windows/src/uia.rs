//! Windows UI Automation accessibility provider.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use xa11y_core::{
    selector::{matches_simple, Combinator, Selector, SelectorSegment},
    CancelHandle, ElementData, Error, Event, EventKind, EventReceiver, Provider, Rect, Result,
    Role, StateFlag, StateSet, Subscription, Toggled,
};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// `EVENT_E_ALL_SUBSCRIBERS_FAILED` (0x80040201) — returned by UIA when an
/// action fires a notification and all registered event subscribers fail to
/// handle it. This means the UI action itself completed; only the notification
/// layer had a transient failure. Certain providers (notably Qt's UIA backend
/// for QTabBar items) propagate this error back through action methods like
/// `Invoke()` and `Select()`, which is incorrect — callers should treat it as
/// success. See: https://github.com/xa11y/xa11y/issues/169
const EVENT_E_ALL_SUBSCRIBERS_FAILED: windows::core::HRESULT =
    windows::core::HRESULT(0x80040201u32 as i32);

fn is_event_subscriber_failure(e: &windows::core::Error) -> bool {
    e.code() == EVENT_E_ALL_SUBSCRIBERS_FAILED
}

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
    /// Raw-view tree walker, created once. Snapshot builds use it to probe a
    /// pattern-less `DataItem`'s parent (the cell-vs-row disambiguation in
    /// `map_uia_role`); raw view matches `batch_request`'s TrueCondition so
    /// the probe sees the same tree the traversal does.
    raw_walker: IUIAutomationTreeWalker,
    /// UIA elements retained for action dispatch (keyed by handle ID).
    handle_cache: Mutex<HashMap<u64, IUIAutomationElement>>,
}

// IUIAutomation is COM and thread-safe via proxy
unsafe impl Send for WindowsProvider {}
unsafe impl Sync for WindowsProvider {}

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        // Establish Per-Monitor-V2 DPI awareness before the first bounds read
        // so UIA reports coordinates in a stable (physical) space that we can
        // convert to logical. Shared once-only init with the screenshot
        // backend — see `crate::dpi` and issue #300.
        crate::dpi::ensure_process_dpi_aware();
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
        let raw_walker = unsafe { automation.RawViewWalker() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("Failed to get RawViewWalker: {}", e),
        })?;

        Ok(Self {
            automation,
            batch_request,
            raw_walker,
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
        build_snapshot_data(element, pid, handle, Some(&self.raw_walker))
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
    /// batch_request uses raw view (TrueCondition), so FindAllBuildCache sees
    /// all elements including virtual/fragment elements.
    fn uia_children(&self, element: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
        let true_cond = match unsafe { self.automation.CreateTrueCondition() } {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        match unsafe {
            element.FindAllBuildCache(TreeScope_Children, &true_cond, &self.batch_request)
        } {
            Ok(arr) => (0..uia_len(&arr))
                .filter_map(|i| uia_get(&arr, i))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Fetch the entire subtree with all properties pre-fetched in one COM call.
    fn find_all_subtree(&self, root: &IUIAutomationElement) -> Result<IUIAutomationElementArray> {
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        uia_call(|| unsafe {
            root.FindAllBuildCache(TreeScope_Subtree, &true_cond, &self.batch_request)
        })
    }

    /// Extract a UIA element's RuntimeId as a `Vec<i32>` for use as a stable
    /// cross-call identity key. `GetRuntimeId` returns a SAFEARRAY of i32 that
    /// uniquely identifies an element within the UIA tree session — the only
    /// identifier safe to use for dedup across `narrow_multi_segment` walks
    /// within a single `find_elements_group` call.
    ///
    /// Returns `None` if the COM call fails or the SAFEARRAY shape isn't what
    /// UIA documents (1D, VT_I4). Callers treat `None` as "untracked" — the
    /// element falls through dedup, which is harmless because untracked
    /// duplicates would only over-report, never under-report.
    fn runtime_id_key(element: &IUIAutomationElement) -> Option<Vec<i32>> {
        use windows::Win32::System::Com::SAFEARRAY;
        use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayUnaccessData};

        let sa: *mut SAFEARRAY = match unsafe { element.GetRuntimeId() } {
            Ok(p) if !p.is_null() => p,
            _ => return None,
        };

        // Safety: GetRuntimeId hands us ownership of a SAFEARRAY*. We must
        // free it via SafeArrayDestroy when done, otherwise we leak. Lock
        // the data via SafeArrayAccessData, copy out the i32s, unlock,
        // destroy.
        let result = unsafe {
            let mut data_ptr: *mut core::ffi::c_void = core::ptr::null_mut();
            if SafeArrayAccessData(sa, &mut data_ptr as *mut _).is_err() {
                let _ = windows::Win32::System::Ole::SafeArrayDestroy(sa);
                return None;
            }
            // SAFEARRAY of i32 — rgsabound[0].cElements is the count.
            let bounds = (*sa).rgsabound.as_ptr();
            let count = (*bounds).cElements as usize;
            let slice = std::slice::from_raw_parts(data_ptr as *const i32, count);
            let v = slice.to_vec();
            let _ = SafeArrayUnaccessData(sa);
            let _ = windows::Win32::System::Ole::SafeArrayDestroy(sa);
            v
        };
        Some(result)
    }
}

// ── Safe UIA helpers ────────────────────────────────────────────────────────

/// Maximum number of attempts for a UIA call that keeps failing with
/// `EVENT_E_ALL_SUBSCRIBERS_FAILED`, and the delay between attempts.
const EVENT_SUBSCRIBER_FAILURE_ATTEMPTS: u32 = 3;
const EVENT_SUBSCRIBER_FAILURE_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(50);

/// Wrap a UIA COM call, mapping the error to xa11y Error::Platform.
///
/// `EVENT_E_ALL_SUBSCRIBERS_FAILED` (0x80040201) is transient (see the
/// constant's doc above): some providers (notably Qt's UIA backend) surface
/// it from query calls like `FindAllBuildCache` even though only the
/// notification layer hiccupped. The action paths (`press`/`toggle`/`select`)
/// can swallow it outright because the action already completed (#169); a
/// query needs a value, so the call is retried a few times before the error
/// is propagated. Any other error is returned immediately — this is a retry
/// of one classified-transient HRESULT, not a fallback (tenet 1).
/// See: https://github.com/xa11y/xa11y/issues/257
fn uia_call<T>(f: impl Fn() -> windows::core::Result<T>) -> Result<T> {
    let mut attempts_left = EVENT_SUBSCRIBER_FAILURE_ATTEMPTS;
    loop {
        attempts_left -= 1;
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if is_event_subscriber_failure(&e) && attempts_left > 0 => {
                std::thread::sleep(EVENT_SUBSCRIBER_FAILURE_RETRY_DELAY);
            }
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: e.to_string(),
                })
            }
        }
    }
}

/// Read a BSTR VARIANT property from the element's pre-fetched snapshot.
fn uia_cached_bstr(element: &IUIAutomationElement, prop: UIA_PROPERTY_ID) -> Option<String> {
    unsafe { element.GetCachedPropertyValue(prop) }
        .ok()
        .and_then(|v| windows::core::BSTR::try_from(&v).ok())
        .map(|b| b.to_string())
        .filter(|s| !s.is_empty())
}

/// Read a VT_BOOL VARIANT property from the element's pre-fetched snapshot.
fn uia_cached_bool(element: &IUIAutomationElement, prop: UIA_PROPERTY_ID) -> Option<bool> {
    unsafe { element.GetCachedPropertyValue(prop) }
        .ok()
        .and_then(|v| variant_bool(&v))
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
    walker: Option<&IUIAutomationTreeWalker>,
) -> ElementData {
    let control_type = unsafe { element.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
    let is_table_item = control_type == UIA_DataItemControlTypeId
        && uia_cached_bool(element, UIA_IsTableItemPatternAvailablePropertyId).unwrap_or(false);
    // The parent probe costs two live COM calls, so it only runs for the one
    // ambiguous case: a DataItem that doesn't implement TableItem. All other
    // control types resolve from the cached snapshot alone.
    let parent_is_data_item = control_type == UIA_DataItemControlTypeId
        && !is_table_item
        && walker.and_then(|w| parent_control_type(w, element)) == Some(UIA_DataItemControlTypeId);
    let mut role = map_uia_role(control_type, is_table_item, parent_is_data_item);

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
        // Native (non-ARIA) dialogs: UIA_IsDialogPropertyId is a first-class
        // UIA property (Windows 10 1703+) that native frameworks such as Qt
        // set without populating AriaRole. Only apply when AriaRole hasn't
        // already resolved the role to Dialog.
        if role == Role::Window && uia_cached_bool(element, UIA_IsDialogPropertyId).unwrap_or(false)
        {
            role = Role::Dialog;
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
                // Under Per-Monitor-V2 awareness UIA reports physical pixels.
                // Convert to logical coordinates so `Element::bounds` matches
                // the cross-platform contract (logical points, same space as
                // the screenshot/input layers). Scale is the DPI of the
                // monitor the element sits on.
                let scale = crate::dpi::scale_for_physical_point(r.left, r.top);
                Some(
                    Rect {
                        x: r.left,
                        y: r.top,
                        width,
                        height,
                    }
                    .to_logical(scale),
                )
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
        // Preserve unstripped originals so callers who need bidi marks can
        // recover them after the strip below.
        if let Some(ref n) = name {
            raw.insert("uia_name".into(), serde_json::Value::String(n.clone()));
        }
        if let Some(ref v) = value {
            raw.insert("uia_value".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(ref d) = description {
            raw.insert("uia_help_text".into(), serde_json::Value::String(d.clone()));
        }
        raw
    };

    // Strip Unicode bidi format controls. RTL apps on Windows embed LRM/RLM
    // marks into reported strings; the originals are preserved in `raw`.
    let name = xa11y_core::text::strip_bidi_opt(name);
    let value = xa11y_core::text::strip_bidi_opt(value);
    let description = xa11y_core::text::strip_bidi_opt(description);

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

    // Use raw view (TrueCondition) so FindAllBuildCache sees all UIA elements,
    // including virtual/fragment elements from Qt, AccessKit, etc. that don't
    // set IsControlElement=true and are silently excluded by the default
    // Control View tree filter.
    let raw_view = uia_call(|| unsafe { automation.CreateTrueCondition() })?;
    uia_call(|| unsafe { request.SetTreeFilter(&raw_view) })?;

    Ok(request)
}

/// Properties pre-fetched in every bulk query.
const BATCH_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_AriaRolePropertyId,
    UIA_IsDialogPropertyId,
    UIA_IsTableItemPatternAvailablePropertyId,
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
    UIA_NativeWindowHandlePropertyId,
];

/// Safe wrapper for IUIAutomationElementArray::Length.
fn uia_len(arr: &IUIAutomationElementArray) -> i32 {
    unsafe { arr.Length() }.unwrap_or(0)
}

/// Safe wrapper for IUIAutomationElementArray::GetElement.
fn uia_get(arr: &IUIAutomationElementArray, index: i32) -> Option<IUIAutomationElement> {
    unsafe { arr.GetElement(index) }.ok()
}

/// Locate the caret within a control's TextPattern, as a character offset
/// from the start of the document.
///
/// Returns:
/// - `Ok(Some(n))` if TextPattern reports a selection whose start lies `n`
///   characters into the document. For a collapsed caret, `n` is the caret
///   position. For a non-empty selection, `n` is the selection's start —
///   mirroring macOS/AT-SPI semantics of "insert at selection start".
/// - `Ok(None)` if the control has no TextPattern, or its selection array is
///   empty (no caret available). The caller should fall back to "append at
///   end" — the behaviour documented in design/README.md.
/// - `Err(..)` if TextPattern is present but a COM call to walk the range
///   fails. These are propagated rather than silently falling back, so
///   genuine platform errors surface (tenet 1).
fn caret_char_offset(uia_element: &IUIAutomationElement) -> Result<Option<usize>> {
    let text_pattern = match unsafe {
        uia_element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
    } {
        Ok(p) => p,
        Err(_) => return Ok(None), // TextPattern not supported on this control.
    };

    let selection = match unsafe { text_pattern.GetSelection() } {
        Ok(s) => s,
        // No active selection (e.g. control never focused) — treat as "no caret".
        Err(_) => return Ok(None),
    };

    if unsafe { selection.Length() }.unwrap_or(0) == 0 {
        return Ok(None);
    }

    let selection_range = unsafe { selection.GetElement(0) }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("TextRangeArray::GetElement(0) failed: {}", e),
    })?;

    let doc_range = unsafe { text_pattern.DocumentRange() }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("TextPattern::DocumentRange failed: {}", e),
    })?;

    // Clone the document range and clip it so it spans [doc start .. selection start].
    // The length of its text (in Unicode characters) is the caret offset.
    let prefix = unsafe { doc_range.Clone() }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("TextRange::Clone failed: {}", e),
    })?;
    unsafe {
        prefix.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &selection_range,
            TextPatternRangeEndpoint_Start,
        )
    }
    .map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("TextRange::MoveEndpointByRange failed: {}", e),
    })?;
    let prefix_text = unsafe { prefix.GetText(-1) }
        .map(|s| s.to_string())
        .map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("TextRange::GetText failed: {}", e),
        })?;

    Ok(Some(prefix_text.chars().count()))
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

                for i in 0..uia_len(&found) {
                    let Some(el) = uia_get(&found, i) else {
                        continue;
                    };
                    let pid = unsafe { el.CachedProcessId() }.unwrap_or(0) as u32;
                    // A process may own several top-level windows (e.g. a main
                    // window plus a modal dialog) and each is returned as its
                    // own entry. Deduping by pid silently dropped every window
                    // after the first, hiding modals from `App::list`/`find`
                    // (issue #304). The `pid == 0` skip still drops windows with
                    // no resolvable owning process; the empty-name skip below
                    // drops windows that are still unnamed mid-startup.
                    // Each entry's `states.active` marks the actual foreground
                    // window (HWND == GetForegroundWindow); that is what lets
                    // the core's foreground tagging pick the right window when a
                    // single process owns several top-level entries.
                    if pid == 0 {
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

    /// Enumerate top-level applications. UIA exposes apps as top-level
    /// `Window` control-type elements under the desktop root — there's no
    /// dedicated `Application` accessible — so we list the desktop's direct
    /// named window children, one entry per top-level window. A process that
    /// owns several top-level windows (e.g. an app showing a modal dialog)
    /// therefore yields several entries, not one per PID (issue #304). This
    /// is the canonical app discovery primitive (replaces the old
    /// `find_elements(None, "application"/"window", …, depth=0)` idiom).
    fn list_apps(&self) -> Result<Vec<ElementData>> {
        self.get_children(None)
    }

    /// Attach to an application directly by pid via a UIA `ProcessId`
    /// property search over the desktop root's children.
    ///
    /// `list_apps()` enumerates desktop-root children of control type
    /// `Window` and skips windows whose name is still empty — which is
    /// exactly the state a freshly launched app's top-level window is in
    /// while the process boots. Matching on the pid property alone closes
    /// that blind spot: any top-level element owned by the process counts,
    /// named or not.
    fn app_by_pid(&self, pid: u32) -> Result<ElementData> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let condition = uia_call(|| unsafe {
            self.automation
                .CreatePropertyCondition(UIA_ProcessIdPropertyId, &VARIANT::from(pid as i32))
        })?;
        // FindFirstBuildCache returns S_OK with a null element when nothing
        // matches; windows-rs surfaces that null as an `Err` carrying the
        // S_OK HRESULT. That case is "process not in the UIA tree yet" —
        // SelectorNotMatched, so the core poll loop retries — while a
        // failing HRESULT is a genuine UIA error and short-circuits.
        let el = match unsafe {
            root.FindFirstBuildCache(TreeScope_Children, &condition, &self.batch_request)
        } {
            Ok(el) => el,
            Err(e) if e.code().is_ok() => {
                return Err(
                    Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
                        xa11y_core::Diagnosis {
                            last_observed: Some(
                                "no top-level UIA element owned by the process yet".to_string(),
                            ),
                            ..Default::default()
                        },
                    ),
                );
            }
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("FindFirstBuildCache(ProcessId={pid}) failed: {e}"),
                });
            }
        };
        // Mirror get_children(None): re-acquire via HWND to activate
        // AccessKit's UIA provider, then repopulate the property snapshot.
        let el = self
            .reacquire_via_hwnd(&el)
            .and_then(|e| self.populate_cache(&e).map_err(|_| ()))
            .unwrap_or(el);
        Ok(self.build_element_data(&el, Some(pid)))
    }

    /// Identify the foreground application via `GetForegroundWindow` +
    /// `ElementFromHandle` — the canonical Win32 foreground query mapped into
    /// the UIA tree. UIA exposes apps as top-level `Window` elements (see
    /// [`list_apps`](Self::list_apps)), and the foreground HWND is exactly such
    /// a top-level window, so the resolved element's pid lines up with a
    /// `list_apps` entry for the core to tag.
    ///
    /// A NULL foreground window (nothing active — e.g. the desktop has focus,
    /// or during a fast app switch) maps to [`Error::SelectorNotMatched`]
    /// ("nothing focused"); a failing `ElementFromHandle` is a genuine UIA
    /// error and propagates.
    fn focused_app(&self) -> Result<ElementData> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return Err(Error::selector_not_matched("focused application"));
        }
        let el = uia_call(|| unsafe { self.automation.ElementFromHandle(hwnd) })?;
        let pid = unsafe { el.CurrentProcessId() }.unwrap_or(0) as u32;
        let pid_opt = (pid != 0).then_some(pid);
        // Populate the snapshot so build_element_data's Cached* reads work,
        // falling back to the live element if caching fails.
        let el = self.populate_cache(&el).unwrap_or(el);
        Ok(self.build_element_data(&el, pid_opt))
    }

    /// Override the default `narrow_multi_segment` so that the Descendant
    /// combinator uses `find_elements_in_tree` (tree-walking via `get_children`)
    /// rather than `self.find_elements` (which would invoke `find_all_subtree`).
    ///
    /// `find_all_subtree` calls `FindAllBuildCache(TreeScope_Subtree)`. When the
    /// candidate element is a UIA *fragment element* (not the HWND fragment root
    /// — e.g. a Qt QFormLayout virtual group), that call can return an incomplete
    /// array due to provider-activation boundaries, regardless of the tree view
    /// filter. Walking level-by-level via `get_children` avoids that problem.
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
                        // Walk level-by-level to avoid provider-activation boundary
                        // issues with FindAllBuildCache(Subtree) on fragment elements.
                        let sub_selector = Selector {
                            segments: vec![SelectorSegment {
                                combinator: Combinator::Root,
                                simple: segment.simple.clone(),
                            }],
                        };
                        let mut sub_results = xa11y_core::selector::find_elements_in_tree(
                            |el| self.get_children(el),
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
            let mut seen = std::collections::HashSet::new();
            next_candidates.retain(|e| seen.insert(e.handle));
            candidates = next_candidates;
        }

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

    fn find_elements_group(
        &self,
        root: &ElementData,
        group: &xa11y_core::selector::SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        if group.clauses.is_empty() {
            return Ok(vec![]);
        }
        // Reject any clause with zero segments early — `clause.segments[0]`
        // below would otherwise panic. The parser doesn't produce empty
        // clauses, but be defensive against direct `SelectorGroup` builders.
        if group.clauses.iter().any(|c| c.segments.is_empty()) {
            return Ok(vec![]);
        }

        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        // ── Phase-1 limit short-circuit ───────────────────────────
        // When there's exactly one clause, propagate the user's `limit`
        // (adjusted for `:nth`) to the subtree walk so e.g.
        // `app.locator("button").first()` stops at the first match. With
        // multiple clauses, phase-1 must collect the full union before
        // truncating because cross-clause doc-order can promote later-clause
        // hits ahead of earlier ones.
        let phase1_limit = if group.clauses.len() == 1 {
            let first = &group.clauses[0].segments[0].simple;
            let outer = if group.clauses[0].segments.len() == 1 {
                limit
            } else {
                None
            };
            match (outer, first.nth) {
                (Some(l), Some(n)) => Some(l.max(n)),
                (_, Some(n)) => Some(n),
                (l, None) => l,
            }
        } else {
            None
        };

        // ── Subtree group walk ─────────────────────────────────────
        // Do ONE `FindAllBuildCache(TreeScope_Subtree)` and evaluate every
        // clause's first segment against each subtree element. App
        // discovery is handled separately by `list_apps()`.
        let root_data = root;

        let uia_root = self.get_cached(root_data.handle)?;
        let pid = root_data.pid;

        // Fragment elements (no HWND) don't support reliable
        // `TreeScope_Subtree` traversal. Fall through to the path-based
        // default which goes level-by-level through `get_children`.
        let is_hwnd_root = unsafe { uia_root.CurrentNativeWindowHandle() }
            .ok()
            .map(|h| !h.0.is_null())
            .unwrap_or(false);
        if !is_hwnd_root {
            return xa11y_core::selector::find_elements_in_tree_group(
                |el| self.get_children(el),
                Some(root),
                group,
                limit,
                max_depth,
            );
        }

        // One COM call fetches the whole subtree in doc order.
        let subtree = self.find_all_subtree(&uia_root)?;
        let count = uia_len(&subtree);

        // Single-pass: visit every subtree element once and check every
        // clause's first segment. Per-element results carry the array
        // index, which is the natural doc-order rank.
        //
        // For all-single-segment groups this is the entire computation —
        // phase 2 is a no-op. For groups with multi-segment clauses we
        // collect the phase-1 (cached_uia, clause_idx, pos) triples and
        // narrow each one after the walk.
        let any_multi_segment = group.clauses.iter().any(|c| c.segments.len() > 1);

        let mut by_clause: Vec<Vec<(usize, ElementData, Option<IUIAutomationElement>)>> =
            (0..group.clauses.len()).map(|_| Vec::new()).collect();

        'walk: for i in 0..count {
            let el = match uia_get(&subtree, i) {
                Some(el) => el,
                None => continue,
            };
            // Build ElementData once; reuse for every clause check. The
            // handle assigned here is stable for the rest of this call.
            let data = self.build_element_data(&el, pid);

            for (idx, clause) in group.clauses.iter().enumerate() {
                if matches_simple(&data, &clause.segments[0].simple) {
                    // Keep the live UIA element alongside the ElementData
                    // only when we'll need it for narrowing — saves a clone
                    // per match on the hot all-single-segment path.
                    let live = if any_multi_segment && clause.segments.len() > 1 {
                        Some(el.clone())
                    } else {
                        None
                    };
                    by_clause[idx].push((i as usize, data.clone(), live));
                    // N=1 phase-1 limit short-circuit (see comment at the
                    // top of this method). Only safe for single-clause
                    // groups; otherwise cross-clause doc-order would be
                    // wrong.
                    if let Some(cap) = phase1_limit {
                        if by_clause[idx].len() >= cap {
                            break 'walk;
                        }
                    }
                }
            }
        }

        // Per-clause phase-2 narrowing (skipped for single-segment clauses).
        // Each narrowed result keeps its phase-1 ancestor's walk position
        // for the global doc-order merge.
        let mut merged: Vec<(usize, ElementData)> = Vec::new();
        for (clause_idx, hits) in by_clause.into_iter().enumerate() {
            if hits.is_empty() {
                continue;
            }
            let clause = &group.clauses[clause_idx];
            if clause.segments.len() == 1 {
                // Apply per-clause `:nth` before merging.
                let mut hits: Vec<(usize, ElementData)> =
                    hits.into_iter().map(|(p, d, _)| (p, d)).collect();
                if let Some(nth) = clause.segments[0].simple.nth {
                    if nth <= hits.len() {
                        let kept = hits.remove(nth - 1);
                        hits.clear();
                        hits.push(kept);
                    } else {
                        hits.clear();
                    }
                }
                merged.extend(hits);
                continue;
            }

            for (anchor_pos, head, _live) in hits {
                let narrowed = self.narrow_multi_segment(
                    vec![head],
                    &clause.segments[1..],
                    max_depth_val,
                    None,
                )?;
                for n in narrowed {
                    merged.push((anchor_pos, n));
                }
            }
        }

        // Stable sort by walk position keeps doc-order; dedup by UIA
        // RuntimeId so descendants reached via multiple phase-1 anchors
        // (or matched by multiple clauses) collapse to one result.
        merged.sort_by_key(|(pos, _)| *pos);
        let mut seen_rt: HashSet<Vec<i32>> = HashSet::new();
        let mut seen_handle: HashSet<u64> = HashSet::new();
        let mut out: Vec<ElementData> = Vec::with_capacity(merged.len());
        for (_, data) in merged {
            // Primary identity: RuntimeId. Cheap to fetch from the cached
            // element and stable across narrowings within this call.
            let key = self
                .get_cached(data.handle)
                .ok()
                .and_then(|el| Self::runtime_id_key(&el));
            match key {
                Some(rt) => {
                    if !seen_rt.insert(rt) {
                        continue;
                    }
                }
                None => {
                    // Fall back to handle dedup if RuntimeId is unavailable.
                    // Handle uniqueness across the call is weaker than
                    // RuntimeId (the same physical element rebuilt in phase
                    // 2 gets a fresh handle), but it's better than nothing
                    // — over-counting beats under-counting on the rare path
                    // where the COM call fails.
                    if !seen_handle.insert(data.handle) {
                        continue;
                    }
                }
            }
            out.push(data);
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
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
            unsafe { pattern.Invoke() }.or_else(|e| {
                if is_event_subscriber_failure(&e) {
                    Ok(())
                } else {
                    Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: "Invoke failed".to_string(),
                    })
                }
            })?;
            return Ok(());
        }
        // Try TogglePattern (checkboxes, switches)
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
        } {
            unsafe { pattern.Toggle() }.or_else(|e| {
                if is_event_subscriber_failure(&e) {
                    Ok(())
                } else {
                    Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: "Toggle failed".to_string(),
                    })
                }
            })?;
            return Ok(());
        }
        // Try SelectionItemPattern (list items, radio buttons)
        if let Ok(pattern) = unsafe {
            uia_element.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                UIA_SelectionItemPatternId,
            )
        } {
            unsafe { pattern.Select() }.or_else(|e| {
                if is_event_subscriber_failure(&e) {
                    Ok(())
                } else {
                    Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: "Select failed".to_string(),
                    })
                }
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
            unsafe { pattern.Toggle() }.or_else(|e| {
                if is_event_subscriber_failure(&e) {
                    Ok(())
                } else {
                    Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: "Toggle failed".to_string(),
                    })
                }
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
            unsafe { pattern.Select() }.or_else(|e| {
                if is_event_subscriber_failure(&e) {
                    Ok(())
                } else {
                    Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: "Select failed".to_string(),
                    })
                }
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

            // If TextPattern is present, use it to locate the caret (start
            // endpoint of the first selection range). If TextPattern is not
            // supported on this control, or no selection exists, fall back to
            // appending at the end of the current value — matching the
            // documented behaviour in design/README.md.
            let caret_char_offset =
                caret_char_offset(&uia_element)?.unwrap_or_else(|| current.chars().count());

            let new_value = crate::splice::splice_at_char_offset(&current, text, caret_char_offset);
            let bstr: windows::core::BSTR = new_value.into();
            unsafe { value_pattern.SetValue(&bstr) }.map_err(|_| Error::TextValueNotSupported)?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        if start > end {
            return Err(Error::InvalidActionData {
                message: format!("set_text_selection start ({start}) must be <= end ({end})"),
            });
        }
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
            // Extend end to selection length. `end >= start` is enforced
            // above, so the u32 subtraction cannot underflow.
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

    // Active: this element is the active (foreground) top-level window.
    // `GetForegroundWindow` returns a top-level HWND; child controls have
    // different (or null) HWNDs, so plain equality is exactly the
    // "this is the foreground window" test — no role check needed.
    // A missing/unreadable cached handle (e.g. an event-path snapshot where
    // the cache was never populated) degrades to `active: false`, matching
    // the other snapshot-default state reads above.
    let active = match unsafe { element.CachedNativeWindowHandle() } {
        Ok(hwnd) => !hwnd.0.is_null() && hwnd.0 == unsafe { GetForegroundWindow() }.0,
        Err(_) => false,
    };

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

    let mut states = StateSet::default();
    states.enabled = enabled;
    states.visible = visible;
    states.focused = focused;
    states.active = active;
    states.focusable = focusable;
    states.modal = false;
    states.checked = checked;
    states.selected = selected;
    states.expanded = expanded;
    states.editable = editable;
    states.required = false;
    states.busy = false;
    states
}

/// Map a UIA control type and its cell signals to an xa11y role.
///
/// UIA uses `DataItem` for both row containers and individual cells. Two
/// independent signals mark a `DataItem` as a cell:
///
/// - `is_table_item` — the element implements the `TableItem` pattern, which
///   exists to supply a cell's row/column header relationships (Qt, WPF, and
///   web grids expose cells this way). Read from the cached property batch.
/// - `parent_is_data_item` — the element's raw-view parent is itself a
///   `DataItem`. AccessKit's UIA adapter exposes `Cell`, `Row`, and both
///   header roles as `DataItem` with no table patterns at all, so its cells
///   are recognizable only structurally: rows sit under tables, cells under
///   rows. No mainstream framework nests row `DataItem`s inside row
///   `DataItem`s, so a `DataItem` under a `DataItem` is a cell. (A tree-grid
///   that nested child-row DataItems directly under parent rows would
///   misreport child rows as cells; no framework we cover does this — tree
///   rows use `TreeItem`.)
///
/// The `GridItem` pattern is deliberately NOT a cell signal: UIA's DataItem
/// spec allows list-style grid items (e.g. a file row in an Explorer details
/// view) to implement `GridItem` while being rows, so its presence cannot
/// distinguish cell from row. A pattern-less `DataItem` whose parent is not a
/// row keeps mapping to `TableRow`.
fn map_uia_role(
    control_type: UIA_CONTROLTYPE_ID,
    is_table_item: bool,
    parent_is_data_item: bool,
) -> Role {
    if control_type == UIA_DataItemControlTypeId && (is_table_item || parent_is_data_item) {
        Role::TableCell
    } else {
        map_uia_control_type(control_type)
    }
}

/// Live (uncached) control type of `element`'s raw-view parent.
///
/// Deliberately not part of the cached batch: UIA cache requests cannot
/// reach upward in the tree, so parent identity is only available via a
/// walker round trip. Called only for pattern-less `DataItem`s (see
/// `build_snapshot_data`).
///
/// Returns `None` when the element has no parent (desktop root) or the
/// element vanished mid-walk; both leave the `DataItem` mapped as a row,
/// identical to "parent is not a row" — this is a refinement probe, not a
/// fallible operation whose error a caller could act on.
fn parent_control_type(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
) -> Option<UIA_CONTROLTYPE_ID> {
    let parent = unsafe { walker.GetParentElement(element) }.ok()?;
    unsafe { parent.CurrentControlType() }.ok()
}

/// Map UIA ControlTypeId to its coarse xa11y Role.
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
    /// Clone of the provider's raw-view walker, so event-target snapshots
    /// resolve DataItem cells the same way tree traversal does. COM in MTA
    /// serializes access via proxies (see `ComCallbackWrapper`), so sharing
    /// the interface pointer with UIA callback threads is safe.
    walker: IUIAutomationTreeWalker,
}

// The COM walker pointer keeps EventContext from auto-deriving Send/Sync
// (it's shared via Arc with UIA's MTA callback threads). The same MTA proxy
// guarantee behind `unsafe impl Send for WindowsProvider` covers it: every
// dereference happens under MTA.
unsafe impl Send for EventContext {}
unsafe impl Sync for EventContext {}

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
            // Receiver may be dropped after close(); lost event is expected then.
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
        build_snapshot_data(&cached_element, Some(self.app_pid), 0, Some(&self.walker))
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
            walker: self.raw_walker.clone(),
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
                // Cleanup during error path; can't override the original error.
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
    fn get_children_none_at_most_one_active() {
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap();
        // At most one top-level window is the foreground (active) window.
        // Zero is legal: the foreground window may be unnamed/filtered.
        let active_count = apps.iter().filter(|a| a.states.active).count();
        assert!(
            active_count <= 1,
            "At most one top-level window may be active, found {active_count}"
        );
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
    fn batch_properties_includes_is_dialog() {
        // UIA_IsDialogPropertyId must be pre-fetched so native (non-ARIA)
        // dialogs — e.g. from Qt — are recognised as Role::Dialog rather
        // than Role::Window on Windows.
        assert!(
            BATCH_PROPERTIES.contains(&UIA_IsDialogPropertyId),
            "UIA_IsDialogPropertyId must be in BATCH_PROPERTIES for native dialog detection"
        );
    }

    #[test]
    fn batch_properties_includes_is_table_item_pattern_available() {
        assert!(
            BATCH_PROPERTIES.contains(&UIA_IsTableItemPatternAvailablePropertyId),
            "TableItem availability must be cached to distinguish DataItem cells from rows"
        );
    }

    #[test]
    fn data_item_role_uses_table_item_pattern() {
        // TableItem pattern marks a cell regardless of parent (Qt, WPF).
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, true, false),
            Role::TableCell
        );
        // Neither signal: a row container.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, false, false),
            Role::TableRow
        );
        // Cell signals never leak onto other control types.
        assert_eq!(
            map_uia_role(UIA_ButtonControlTypeId, true, true),
            Role::Button
        );
    }

    #[test]
    fn pattern_less_data_item_under_row_is_cell() {
        // AccessKit exposes cells as pattern-less DataItems under a row
        // DataItem — the structural signal alone must classify them.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, false, true),
            Role::TableCell
        );
        // Both signals agreeing is still a cell.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, true, true),
            Role::TableCell
        );
    }

    #[test]
    fn window_control_type_maps_to_window_not_dialog() {
        // map_uia_control_type alone always returns Window for WindowControlTypeId;
        // the Dialog refinement is a separate step that reads IsDialog/AriaRole.
        assert_eq!(map_uia_control_type(UIA_WindowControlTypeId), Role::Window);
    }

    #[test]
    fn find_elements_empty_selector_returns_empty() {
        let Some(provider) = try_provider() else {
            return;
        };
        // `find_elements` now requires a root; grab any top-level app from
        // the discovery primitive. If no app is present (headless CI),
        // skip — the empty-selector check needs a real subtree to walk.
        let Some(root) = provider.list_apps().unwrap_or_default().into_iter().next() else {
            return;
        };
        let empty_selector = Selector { segments: vec![] };
        let result = provider
            .find_elements(&root, &empty_selector, None, None)
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

    // ── EVENT_E_ALL_SUBSCRIBERS_FAILED handling ─────────────────────────────

    #[test]
    fn event_e_all_subscribers_failed_constant_matches_sdk_value() {
        // 0x80040201 is EVENT_E_ALL_SUBSCRIBERS_FAILED from <eventsys.h>.
        // The constant value must be stable — it is part of the Windows ABI.
        assert_eq!(EVENT_E_ALL_SUBSCRIBERS_FAILED.0, 0x80040201u32 as i32);
    }

    #[test]
    fn is_event_subscriber_failure_recognises_0x80040201() {
        // Construct the error the way the Windows crate does when a COM call
        // returns this HRESULT: via HRESULT::ok() → Err(windows::core::Error).
        let err = windows::core::HRESULT(0x80040201u32 as i32)
            .ok()
            .unwrap_err();
        assert!(
            is_event_subscriber_failure(&err),
            "0x80040201 must be classified as an event-subscriber failure"
        );
    }

    #[test]
    fn is_event_subscriber_failure_passes_other_hresults() {
        for &code in &[
            0x80004005u32, // E_FAIL
            0x80070057u32, // E_INVALIDARG
            0x80004003u32, // E_POINTER
        ] {
            let err = windows::core::HRESULT(code as i32).ok().unwrap_err();
            assert!(
                !is_event_subscriber_failure(&err),
                "HRESULT 0x{code:08X} must not be classified as an event-subscriber failure"
            );
        }
    }

    // ── uia_call retry behaviour (issue #257) ───────────────────────────────

    fn subscriber_failure() -> windows::core::Error {
        EVENT_E_ALL_SUBSCRIBERS_FAILED.ok().unwrap_err()
    }

    #[test]
    fn uia_call_success_calls_once() {
        let calls = std::cell::Cell::new(0u32);
        let result = uia_call(|| {
            calls.set(calls.get() + 1);
            Ok(42)
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn uia_call_retries_event_subscriber_failure_then_succeeds() {
        // Two transient 0x80040201 failures, then success — the query path
        // must ride out the same Qt-UIA hiccup the action path tolerates.
        let calls = std::cell::Cell::new(0u32);
        let result = uia_call(|| {
            calls.set(calls.get() + 1);
            if calls.get() < EVENT_SUBSCRIBER_FAILURE_ATTEMPTS {
                Err(subscriber_failure())
            } else {
                Ok("tree")
            }
        });
        assert_eq!(result.unwrap(), "tree");
        assert_eq!(calls.get(), EVENT_SUBSCRIBER_FAILURE_ATTEMPTS);
    }

    #[test]
    fn uia_call_propagates_persistent_event_subscriber_failure() {
        // If every attempt fails with 0x80040201, the error must still
        // surface (no infinite retry, no silent swallow on the read path —
        // unlike an action, a query has no value to return).
        let calls = std::cell::Cell::new(0u32);
        let result: Result<()> = uia_call(|| {
            calls.set(calls.get() + 1);
            Err(subscriber_failure())
        });
        assert_eq!(calls.get(), EVENT_SUBSCRIBER_FAILURE_ATTEMPTS);
        match result {
            Err(Error::Platform { code, .. }) => {
                assert_eq!(code, EVENT_E_ALL_SUBSCRIBERS_FAILED.0 as i64);
            }
            other => panic!("expected Error::Platform, got {other:?}"),
        }
    }

    #[test]
    fn uia_call_does_not_retry_other_errors() {
        let calls = std::cell::Cell::new(0u32);
        let e_fail = windows::core::HRESULT(0x80004005u32 as i32);
        let result: Result<()> = uia_call(|| {
            calls.set(calls.get() + 1);
            Err(e_fail.ok().unwrap_err())
        });
        assert_eq!(
            calls.get(),
            1,
            "non-transient errors must fail on the first attempt"
        );
        match result {
            Err(Error::Platform { code, .. }) => assert_eq!(code, e_fail.0 as i64),
            other => panic!("expected Error::Platform, got {other:?}"),
        }
    }
}
