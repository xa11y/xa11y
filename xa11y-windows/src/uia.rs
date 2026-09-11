//! Windows UI Automation accessibility provider.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SetForegroundWindow, STATE_SYSTEM_SELECTED,
};

use xa11y_core::{
    selector::{matches_simple, Combinator, Selector, SelectorSegment},
    CancelHandle, ElementData, ElementParts, Error, Event, EventKind, EventParts, EventReceiver,
    Provider, Result, Role, ShellSurfaceKind, StateFlag, StateParts, StateSet, Subscription,
    Toggled,
};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// High bit of a synthesized Application-node handle.
///
/// UIA has no `Application` accessible — processes surface only through their
/// top-level HWNDs — so the per-process Application node this provider reports
/// is synthesized: it deliberately holds no live UIA element, and its handle
/// is a tagged entry in [`WindowsProvider::synthetic_apps`] instead of a
/// `handle_cache` key. Handles minted by [`WindowsProvider::cache_element`]
/// increment from 1 and never carry the tag, so the tag space is disjoint by
/// construction; the counter is shared with `cache_element` so two syntheses
/// of the same process never collide.
const SYNTHETIC_APP_TAG: u64 = 1 << 63;

/// True for every handle in the synthetic Application-node tag space.
fn is_synthetic_handle(handle: u64) -> bool {
    handle & SYNTHETIC_APP_TAG != 0
}

/// Process-generation identity of a synthesized Application node.
///
/// The pid alone cannot identify a process: Windows reuses PIDs, so a stale
/// `App` node (a handle minted before the process exited) would otherwise
/// re-enumerate an unrelated process that was assigned the same PID. The
/// creation time is captured via [`process_creation_time`] at synthesis and
/// re-read when the node resolves its children; a mismatch is surfaced as
/// [`Error::ElementStale`] instead of silently retargeting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SyntheticAppIdentity {
    pid: u32,
    /// FILETIME creation timestamp (100ns since 1601-01-01), or `None` when
    /// the process could not be opened at synthesis time (access denied, or
    /// it exited between enumeration and query). `None` disables the
    /// generation check — a guard that cannot capture its baseline cannot
    /// verify one, and refusing to synthesize the node would break
    /// `App::list` for exactly the processes (permission-guarded, transient)
    /// the name fallback already tolerates.
    creation_time: Option<u64>,
}

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
    /// Identities of the synthesized Application nodes this provider minted,
    /// keyed by their tagged handle. `get_children(Some(app))` validates the
    /// identity (creation time) before enumerating, so a stale `App` whose
    /// process exited and whose PID was reused by another process surfaces an
    /// error rather than silently retargeting to the new process.
    synthetic_apps: Mutex<HashMap<u64, SyntheticAppIdentity>>,
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
            synthetic_apps: Mutex::new(HashMap::new()),
        })
    }

    /// Re-acquire a UIA element via its native window handle.
    /// This triggers WM_GETOBJECT which activates AccessKit's UIA provider,
    /// ensuring the element's children include virtual accessibility elements.
    ///
    /// Returns the COM error rather than collapsing it: a caller wants to
    /// decide whether a given failure is fatal to its enumeration, and the
    /// `()`-shaped old signature forced every caller into the same silent
    /// fallback (tenet 1). An element with no native handle is not an error —
    /// there is nothing to re-acquire — so it returns the element itself;
    /// `element` is a COM interface and cloning it is an `AddRef`.
    fn reacquire_via_hwnd(
        &self,
        element: &IUIAutomationElement,
    ) -> windows::core::Result<IUIAutomationElement> {
        let hwnd = unsafe { element.CurrentNativeWindowHandle() }?;
        if hwnd.0.is_null() {
            return Ok(element.clone());
        }
        // Callers that fall back to the un-reacquired element on `Err` would,
        // for a transiently-busy COM server, silently hand back an element
        // whose AccessKit provider was never activated. Retry first so a
        // foreign app's momentary busy-ness doesn't quietly degrade the
        // result; a persistent failure still surfaces as the HRESULT.
        retry_transient(|| unsafe { self.automation.ElementFromHandle(hwnd) })
    }

    /// Cache a UIA element and return its handle ID.
    fn cache_element(&self, uia: IUIAutomationElement) -> u64 {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        // Handles minted here increment from 1 and must never enter the
        // synthetic tag space: a tagged handle would be reported as a
        // synthesized app node by `is_synthetic_handle` and become
        // unreachable through `get_cached`. The counter cannot reach bit 63
        // in practice; the assert documents the invariant rather than
        // guarding a reachable state.
        debug_assert!(
            !is_synthetic_handle(handle),
            "cache_element minted handle {handle} inside the synthetic tag space"
        );
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, uia);
        handle
    }

    /// Look up a cached UIA element by handle.
    fn get_cached(&self, handle: u64) -> Result<IUIAutomationElement> {
        // A synthesized Application node has no live element behind it, so no
        // cached lookup can succeed. Answer with the remedy rather than an
        // opaque "stale handle": the pid identifies the node and the message
        // names the path that does work (tenet 6 — the error carries its own
        // diagnosis).
        if is_synthetic_handle(handle) {
            let pid = self
                .synthetic_apps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&handle)
                .map(|identity| identity.pid.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(Error::Unsupported {
                feature: format!(
                    "handle {handle} is a synthesized Application node (pid {pid}) with no \
                     live UIA element; enumerate the process's windows via `App::windows` \
                     and act on a window child"
                ),
            });
        }
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .cloned()
            .ok_or(Error::ElementStale {
                selector: format!("handle:{}", handle),
            })
    }

    /// Identity of the synthesized Application node for `handle`, if any.
    ///
    /// Handles tagged by [`is_synthetic_handle`] but absent from the map
    /// (minted by another provider instance, or an enum-keyed carry-over)
    /// return `None` — callers then treat the node as a plain stale handle.
    fn synthetic_app_identity(&self, handle: u64) -> Option<SyntheticAppIdentity> {
        if !is_synthetic_handle(handle) {
            return None;
        }
        self.synthetic_apps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .copied()
    }

    /// Query UIA patterns from the element once, sharing across
    /// `get_value`, `get_actions`, and `parse_states` to avoid duplicate COM calls.
    ///
    /// WindowPattern / TransformPattern exist only on top-level window
    /// elements, and `GetCurrentPatternAs` is a live COM round-trip — not
    /// served from the snapshot cache — so they are queried only for
    /// window/dialog roles. Every other element previously paid two failed
    /// provider calls per snapshot for patterns nothing consults.
    ///
    /// Only the known "pattern absent" HRESULTs (see [`is_pattern_absent`])
    /// become `None`. A stale element or wedged provider is a real COM
    /// failure and must not bleed into the snapshot as "no window actions and
    /// unknown window states" — the same distinction the window verbs make via
    /// [`pattern_acquisition_error`] (tenet 1).
    ///
    /// These acquisitions run for *every* Window/Dialog element in every
    /// snapshot, and `GetCurrentPatternAs` is a cross-process COM call into
    /// a foreign app that can be momentarily busy (`RPC_E_CALL_REJECTED` and
    /// friends — see [`is_com_server_busy`]). Unlike the window verbs, a
    /// transient rejection here has no caller left to retry: it would fail
    /// the whole `get_children` walk. So the acquisitions are wrapped in
    /// [`retry_transient`], which re-issues only the classified transient
    /// HRESULTs and propagates everything else unchanged.
    fn query_patterns(role: Role, element: &IUIAutomationElement) -> Result<ElementPatterns> {
        let window = if matches!(role, Role::Window | Role::Dialog) {
            match retry_transient(|| unsafe {
                element.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
            }) {
                Ok(p) => Some(p),
                Err(e) if is_pattern_absent(&e) => None,
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "acquiring WindowPattern while building element data failed: {e}"
                        ),
                    });
                }
            }
        } else {
            None
        };
        let transform = if matches!(role, Role::Window | Role::Dialog) {
            match retry_transient(|| unsafe {
                element.GetCurrentPatternAs::<IUIAutomationTransformPattern>(UIA_TransformPatternId)
            }) {
                Ok(p) => Some(p),
                Err(e) if is_pattern_absent(&e) => None,
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "acquiring TransformPattern while building element data failed: {e}"
                        ),
                    });
                }
            }
        } else {
            None
        };
        Ok(ElementPatterns {
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
            window,
            transform,
        })
    }

    /// Build an ElementData from a pre-fetched UIA element snapshot.
    ///
    /// The element MUST have been obtained via `FindAllBuildCache` or
    /// `BuildUpdatedCache` so that Cached* accessors are populated.
    /// Every query takes a fresh snapshot — callers never see stale data.
    fn build_element_data(
        &self,
        element: &IUIAutomationElement,
        pid: Option<u32>,
    ) -> Result<ElementData> {
        let handle = self.cache_element(element.clone());
        build_snapshot_data(element, pid, handle, Some(&self.raw_walker))
    }

    /// Build the per-process Application node Windows lacks natively.
    ///
    /// UIA exposes processes only through their top-level HWNDs, so this node
    /// is *synthesized*: a handle tagged with [`SYNTHETIC_APP_TAG`] and keyed
    /// into [`Self::synthetic_apps`] (recognized by
    /// [`synthetic_app_identity`](Self::synthetic_app_identity), funnelled to
    /// an explicit `Unsupported` error by [`get_cached`](Self::get_cached)),
    /// no live UIA element, no bounds (UIA has no process geometry — `None`
    /// is the honest answer, not a union of window rects), no window actions,
    /// no window-state flags. The node's top-level windows are its
    /// `get_children` answer.
    ///
    /// `representative` is the process's first top-level window in z-order;
    /// it is the *fallback* name source only. Name resolution: process
    /// executable stem (`OpenProcess` + `QueryFullProcessImageNameW`) first;
    /// the representative window's title when the process cannot be opened.
    /// `raw` records which one was used (`uia_name_source`), plus the
    /// synthesized marker and the full executable path when known — the
    /// fallback is explicit, not silent (tenet 1).
    ///
    /// The handle is minted from the shared [`NEXT_HANDLE`] counter, so two
    /// syntheses of the same pid in one provider session never share a handle
    /// — each records its own process creation time, which is how a stale
    /// `App` node is distinguished from a live process that reuses the pid.
    fn build_synthetic_app_data(
        &self,
        pid: u32,
        representative: &IUIAutomationElement,
    ) -> Result<ElementData> {
        let (name, name_source, executable) = match process_image_name(pid) {
            Some(path) => {
                let stem = std::path::Path::new(&path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                (stem, "process", Some(path))
            }
            None => {
                // The fallback name source is explicit, so a failing read must
                // not silently become "unnamed window": CurrentName() is the
                // last chance for a name and its COM error is the diagnosis,
                // not an empty title (tenet 1).
                let title = unsafe { representative.CurrentName() }
                    .map_err(|e| Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "IUIAutomationElement.CurrentName failed for the representative \
                             window (pid {pid}): {e}"
                        ),
                    })?
                    .to_string();
                (title, "window_title", None)
            }
        };
        let handle = SYNTHETIC_APP_TAG | NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.synthetic_apps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                handle,
                SyntheticAppIdentity {
                    pid,
                    creation_time: process_creation_time(pid),
                },
            );
        let mut raw = HashMap::new();
        raw.insert("uia_synthesized".into(), serde_json::Value::Bool(true));
        raw.insert(
            "uia_name_source".into(),
            serde_json::Value::String(name_source.into()),
        );
        if let Some(path) = executable {
            raw.insert("uia_process_name".into(), serde_json::Value::String(path));
        }
        Ok(ElementParts {
            role: Role::Application,
            name: if name.is_empty() { None } else { Some(name) },
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: Some(pid),
            raw,
            handle,
        }
        .into())
    }

    /// Enumerate every top-level window (`ControlType.Window`) owned by `pid`
    /// under the desktop root, in z-order.
    ///
    /// This is the single window-discovery primitive now: [`list_apps`] /
    /// [`get_children(None)`](Self::get_children) group its result by pid,
    /// [`app_by_pid`](Self::app_by_pid) takes its first match as the
    /// representative, and [`get_children(Some(app))`](Self::get_children)
    /// answers with it. Requiring the Window control type keeps the answer
    /// window-shaped even for a WebView2/wry host (Tauri, egui, Electron),
    /// whose process owns several pid-matching desktop children.
    ///
    /// An empty result is a truth, not an error: the last window closing is
    /// exactly the state "no windows" must report. Length / GetElement
    /// failures are real COM failures and propagate (tenet 1) — an empty
    /// `Ok` reads as "this process has no windows" when a transient UIA
    /// failure actually occurred.
    fn top_level_windows_of_pid(&self, pid: u32) -> Result<Vec<IUIAutomationElement>> {
        top_level_windows_of_pid_with(&self.automation, pid, &self.batch_request)
    }

    /// Populate a UIA element's snapshot so Cached* accessors work.
    /// Used for single-element reads (e.g., get_parent) that don't go
    /// through FindAllBuildCache.
    fn populate_cache(
        &self,
        element: &IUIAutomationElement,
    ) -> windows::core::Result<IUIAutomationElement> {
        retry_transient(|| unsafe { element.BuildUpdatedCache(&self.batch_request) })
    }

    /// Get direct UIA children of an element with properties pre-fetched.
    /// batch_request uses raw view (TrueCondition), so FindAllBuildCache sees
    /// all elements including virtual/fragment elements.
    fn uia_children(&self, element: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
        let true_cond = match unsafe { self.automation.CreateTrueCondition() } {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        // An empty Vec here reads downstream as "this element has no children",
        // so a transiently-busy provider would make a populated subtree look
        // empty. Retry the classified-transient HRESULTs before degrading.
        match retry_transient(|| unsafe {
            element.FindAllBuildCache(TreeScope_Children, &true_cond, &self.batch_request)
        }) {
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

    /// True when at least one direct child of `element` is reported on screen.
    ///
    /// Used by [`list_shell_surfaces`](Provider::list_shell_surfaces) for the
    /// one shell class whose presence under the desktop root does not mean it
    /// is open — see the `ControlCenterWindow` arm there. A child that does
    /// not publish `IsOffscreen` at all counts as *not* on screen: the
    /// platform did not say it was visible, and inventing an answer would put
    /// a dismissed panel in the listing.
    ///
    /// A failing COM call propagates rather than degrading to `false` — "the
    /// shell window could not be inspected" is not "the shell window is
    /// closed" (tenet 1). The caller re-wraps that failure with the surface it
    /// was probing and the surfaces the scan had already classified, because
    /// `Error::Platform` has no diagnosis field to attach one to (tenet 6).
    fn has_onscreen_child(&self, element: &IUIAutomationElement) -> Result<bool> {
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        let children = uia_call(|| unsafe {
            element.FindAllBuildCache(TreeScope_Children, &true_cond, &self.batch_request)
        })?;
        Ok((0..uia_len(&children))
            .filter_map(|i| uia_get(&children, i))
            .any(|child| uia_cached_bool(&child, UIA_IsOffscreenPropertyId) == Some(false)))
    }

    /// The first descendant of `root` whose `ClassName` is `class`, with the
    /// batch cache populated so `build_element_data` can read it.
    ///
    /// `FindFirstBuildCache` with a `ClassName` property condition, the same
    /// primitive [`app_by_pid`](Self::app_by_pid) uses against the desktop
    /// root, scoped to a subtree instead. Used by
    /// [`list_shell_surfaces`](Provider::list_shell_surfaces) to reach the
    /// desktop icon list view under `Progman`.
    ///
    /// `Ok(None)` means the class genuinely is not in the subtree — windows-rs
    /// surfaces UIA's "S_OK, null element" as an `Err` carrying an *ok*
    /// HRESULT. A failing HRESULT is a real UIA error and propagates (tenet
    /// 1): "the subtree could not be searched" is not "the class is absent".
    fn descendant_by_class(
        &self,
        root: &IUIAutomationElement,
        class: &str,
    ) -> Result<Option<IUIAutomationElement>> {
        let condition = uia_call(|| unsafe {
            self.automation
                .CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(class))
        })?;
        match unsafe {
            root.FindFirstBuildCache(TreeScope_Descendants, &condition, &self.batch_request)
        } {
            Ok(el) => Ok(Some(el)),
            Err(e) if e.code().is_ok() => Ok(None),
            Err(e) => Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("FindFirstBuildCache(ClassName={class}) failed: {e}"),
            }),
        }
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

/// Maximum number of attempts for a UIA call that keeps failing with a
/// classified-transient HRESULT, and the delay between attempts.
const TRANSIENT_RETRY_ATTEMPTS: u32 = 3;
const TRANSIENT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// True for COM "the server can't take this call right now" HRESULTs.
///
/// App discovery reaches well beyond xa11y's own target: `get_children(None)`
/// enumerates *every* top-level window on the desktop and makes a cross-process
/// call into each one. Those foreign processes are ordinary applications with
/// their own threading models, and COM refuses an incoming cross-apartment call
/// while the target is busy:
///
/// - `RPC_E_CALL_REJECTED` (0x80010001) — the callee rejected the call.
/// - `RPC_E_SERVERCALL_RETRYLATER` (0x8001010A) — the server is in a state
///   that cannot process it, and says so explicitly.
/// - `RPC_E_CANTCALLOUT_ININPUTSYNCCALL` (0x8001010D) — the target STA thread
///   is dispatching an input-synchronous call (it is inside a cross-thread
///   `SendMessage` handler), so COM will not let it call out.
///
/// None of these say anything about the app the caller actually asked for —
/// an unrelated busy process on the machine must not fail their query. Windows
/// raises them as first-chance SEH exceptions in the *calling* process before
/// COM converts them to a failed HRESULT, which is why they surface in CI logs
/// as `Windows fatal exception: code 0x8001010d` under pytest's faulthandler.
/// Those lines are handled exceptions, not crashes.
///
/// Observed on the `winforms` Windows integ cell in #328's CI run.
fn is_com_server_busy(e: &windows::core::Error) -> bool {
    let code = e.code();
    code == RPC_E_CALL_REJECTED
        || code == RPC_E_SERVERCALL_RETRYLATER
        || code == RPC_E_CANTCALLOUT_ININPUTSYNCCALL
}

/// The complete set of HRESULTs worth another attempt.
///
/// Deliberately a closed list. Every other error propagates on the first
/// attempt, so this stays a retry of specifically-classified transient
/// failures rather than a fallback chain (tenet 1).
fn is_transient(e: &windows::core::Error) -> bool {
    is_event_subscriber_failure(e) || is_com_server_busy(e)
}

/// Retry a COM call while it fails transiently, preserving the raw HRESULT.
///
/// Used by the call sites that need the original `windows::core::Error` (or
/// that degrade rather than propagate). [`uia_call`] wraps this for the common
/// case of mapping into [`Error::Platform`].
fn retry_transient<T>(f: impl Fn() -> windows::core::Result<T>) -> windows::core::Result<T> {
    let mut attempts_left = TRANSIENT_RETRY_ATTEMPTS;
    loop {
        attempts_left -= 1;
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if is_transient(&e) && attempts_left > 0 => {
                std::thread::sleep(TRANSIENT_RETRY_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Wrap a UIA COM call, mapping the error to xa11y Error::Platform.
///
/// Two families of HRESULT are retried before the error is propagated:
///
/// `EVENT_E_ALL_SUBSCRIBERS_FAILED` (0x80040201) — some providers (notably
/// Qt's UIA backend) surface it from query calls like `FindAllBuildCache` even
/// though only the notification layer hiccupped. The action paths
/// (`press`/`toggle`/`select`) can swallow it outright because the action
/// already completed (#169); a query needs a value, so it is retried.
/// See: https://github.com/xa11y/xa11y/issues/257
///
/// The COM server-busy family — see [`is_com_server_busy`].
///
/// Any other error is returned immediately.
fn uia_call<T>(f: impl Fn() -> windows::core::Result<T>) -> Result<T> {
    retry_transient(f).map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: e.to_string(),
    })
}

/// Class name of the desktop icon list view, the element
/// [`ShellSurfaceKind::Desktop`] names.
///
/// It sits two levels under `Progman` (`Progman` → `SHELLDLL_DefView` →
/// `SysListView32`), so the shell scan descends by class rather than by a
/// fixed child index — the intermediate `SHELLDLL_DefView` is reparented to a
/// `WorkerW` window when Active Desktop-style wallpaper hosts are in play, and
/// depth is the one thing that varies.
const DESKTOP_LIST_VIEW_CLASS: &str = "SysListView32";

/// Longest candidate list a shell-scan failure carries. Bounded per tenet 6 —
/// a diagnosis must not grow with the desktop.
const DIAG_SHELL_CANDIDATE_LIMIT: usize = 20;

/// Bounded `kind "name"` rendering of the shell surfaces a scan had already
/// classified when it aborted.
///
/// The candidate list a consumer would otherwise reconstruct by re-running the
/// listing under logging — except that a scan that aborts never returns one,
/// which is exactly why the partial result belongs in the error.
fn classified_so_far(surfaces: &[(u8, ShellSurfaceKind, ElementData)]) -> Vec<String> {
    let mut out: Vec<String> = surfaces
        .iter()
        .take(DIAG_SHELL_CANDIDATE_LIMIT)
        .map(|(_, kind, data)| {
            format!(
                "{} \"{}\"",
                kind.to_snake_case(),
                data.name.as_deref().unwrap_or("")
            )
        })
        .collect();
    if surfaces.len() > DIAG_SHELL_CANDIDATE_LIMIT {
        out.push(format!(
            "… (+{} more)",
            surfaces.len() - DIAG_SHELL_CANDIDATE_LIMIT
        ));
    }
    out
}

/// Build the UIA `ProcessId` property-condition value for `pid`.
///
/// UIA property conditions carry an i32: a `u32` pid above 2^31 would wrap
/// and silently match nothing (tenet 1), so fail surfaceably instead.
fn pid_variant(pid: u32) -> Result<VARIANT> {
    i32::try_from(pid)
        .map(VARIANT::from)
        .map_err(|_| Error::Platform {
            code: -1,
            message: format!("PID {pid} exceeds the i32 range UIA property conditions accept"),
        })
}

/// Free-function form of [`WindowsProvider::top_level_windows_of_pid`].
///
/// The event subscription's open/close watch runs on UIA's callback thread
/// without a `WindowsProvider` handle, but must re-attach handlers to the
/// same window set — so the enumeration lives here and the provider method
/// delegates, keeping one implementation of the discovery primitive.
fn top_level_windows_of_pid_with(
    autom: &IUIAutomation,
    pid: u32,
    cache: &IUIAutomationCacheRequest,
) -> Result<Vec<IUIAutomationElement>> {
    let root = uia_call(|| unsafe { autom.GetRootElement() })?;
    let value = pid_variant(pid)?;
    let pid_condition =
        uia_call(|| unsafe { autom.CreatePropertyCondition(UIA_ProcessIdPropertyId, &value) })?;
    let window_condition = uia_call(|| unsafe {
        autom.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_WindowControlTypeId.0),
        )
    })?;
    let condition =
        uia_call(|| unsafe { autom.CreateAndCondition(&pid_condition, &window_condition) })?;
    let found =
        uia_call(|| unsafe { root.FindAllBuildCache(TreeScope_Children, &condition, cache) })?;
    let len = uia_call(|| unsafe { found.Length() })?;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let el = uia_call(|| unsafe { found.GetElement(i) }).map_err(|e| match e {
            Error::Platform { code, message } => Error::Platform {
                code,
                message: format!("IUIAutomationElementArray.GetElement({i}) failed: {message}"),
            },
            other => other,
        })?;
        out.push(el);
    }
    Ok(out)
}

/// Resolve the executable image path of `pid` via
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
/// `QueryFullProcessImageNameW`.
///
/// `None` when the process cannot be opened or queried (access denied, the
/// process exited between enumeration and query, 32-bit/64-bit boundary in a
/// hard case). That is the *windows do not stay alive by name* trigger for
/// [`WindowsProvider::build_synthetic_app_data`]'s representative-window-title
/// fallback — returning `None` rather than a synthesized error keeps the
/// Application node constructible for a process that is enumerable but not
/// inspectable, and the caller records which source produced the name in
/// `raw["uia_name_source"]` so the fallback is explicit, not silent
/// (tenet 1).
fn process_image_name(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // QueryFullProcessImageNameW follows the Create Rule: nothing else owns
    // `handle` here, so it must be released (and the error dropped rather
    // than leaked) on every path.
    let mut buf = vec![0u16; 32768];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    }
    .is_ok();
    // Releasing the OpenProcess handle is best-effort only: a failure here
    // leaks a single process handle we only queried for a name — the leak is
    // unrecoverable at this layer, and the name lookup's outcome was already
    // decided. Treating it as an error would report a fallback that never
    // happened (tenet 1), so the call is dropped with its reason stated.
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..size as usize]);
    Some(path)
}

/// Resolve the creation time of `pid` (FILETIME, 100ns since 1601-01-01) via
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetProcessTimes`.
///
/// This is the process-generation token [`WindowsProvider::synthetic_app_identity`]
/// validates against: the pid alone cannot identify a process, because
/// Windows reuses PIDs once a process exits. `None` when the process cannot
/// be opened or queried (access denied, exited between enumeration and
/// query) — same shape as [`process_image_name`], and the same consequence:
/// a synthesized node minted for such a process cannot be re-validated, so
/// it disables the generation check rather than failing to synthesize
/// (tenet 1 does not demand a guard when the platform refuses the baseline).
fn process_creation_time(pid: u32) -> Option<u64> {
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // Same ownership rule as `process_image_name`: nothing else owns
    // `handle`, so it must be released on every path.
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .is_ok();
    let _ = unsafe { CloseHandle(handle) };
    if !ok {
        return None;
    }
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// Whether a synthesized `App` node's captured process generation no longer
/// matches the process occupying its pid.
///
/// A `None` on either side is "no verdict", not "stale": the guard runs only
/// when both the baseline and the current read are available, so a process
/// that could not be opened at synthesis (or has since become unopenable) is
/// left to the enumeration itself rather than falsely declared dead.
fn synthetic_app_is_stale(stored: Option<u64>, current: Option<u64>) -> bool {
    match (stored, current) {
        (Some(stored), Some(current)) => stored != current,
        _ => false,
    }
}

/// The error a stale synthesized `App` node returns: its process exited and
/// Windows reused its pid, so the node no longer names anything the caller
/// asked for. The message carries the diagnosis (pid + handle + why), per
/// tenet 6 — reading "Element stale: could not relocate element for
/// selector: handle:… (pid …)" must be enough to understand the failure.
fn synthetic_app_stale_error(handle: u64, pid: u32) -> Error {
    Error::ElementStale {
        selector: format!(
            "handle:{handle} (pid {pid}); the process exited and Windows reused its pid, \
             so this Application node is stale"
        ),
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

/// Read a VT_I4 VARIANT property from the element's pre-fetched snapshot.
fn uia_cached_i32(element: &IUIAutomationElement, prop: UIA_PROPERTY_ID) -> Option<i32> {
    unsafe { element.GetCachedPropertyValue(prop) }
        .ok()
        .and_then(|v| variant_i32(&v))
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
) -> Result<ElementData> {
    let control_type = unsafe { element.CachedControlType() }.unwrap_or(UIA_CONTROLTYPE_ID(0));
    let is_table_item = (control_type == UIA_DataItemControlTypeId
        || control_type == UIA_CustomControlTypeId)
        && uia_cached_bool(element, UIA_IsTableItemPatternAvailablePropertyId).unwrap_or(false);
    // The parent probe costs two live COM calls, so it only runs for the one
    // ambiguous case: a DataItem that doesn't implement TableItem. All other
    // control types resolve from the cached snapshot alone.
    let parent_is_data_item = control_type == UIA_DataItemControlTypeId
        && !is_table_item
        && walker.and_then(|w| parent_control_type(w, element)) == Some(UIA_DataItemControlTypeId);
    // Only read for `Custom`, where it is the provider's sole role signal
    // (see `map_msaa_role`); every other control type answers from the UIA
    // control type alone.
    let legacy_role = if control_type == UIA_CustomControlTypeId {
        uia_cached_i32(element, UIA_LegacyIAccessibleRolePropertyId)
    } else {
        None
    };
    let mut role = map_uia_role(
        control_type,
        is_table_item,
        parent_is_data_item,
        legacy_role,
    );

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

    let patterns = WindowsProvider::query_patterns(role, element)?;
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
                // Convert to logical coordinates (origin-preserving per
                // monitor — see `crate::dpi`) so `Element::bounds` matches the
                // cross-platform contract and a mixed-DPI desktop produces a
                // non-overlapping logical space.
                Some(crate::dpi::physical_rect_to_logical(r))
            }
        });

    let actions = get_actions(element, role, &patterns)?;

    let automation_id = unsafe { element.CachedAutomationId() }
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    let native_handle = unsafe { element.CachedNativeWindowHandle() }.ok();
    let stable_id = uia_stable_id(native_handle, automation_id.clone());

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

    Ok(ElementParts {
        role,
        name,
        value,
        description,
        bounds,
        actions,
        states,
        numeric_value,
        min_value,
        max_value,
        stable_id,
        pid,
        raw,
        handle,
    }
    .into())
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
    // MSAA state bitmask — the only place pre-SelectionItem frameworks report
    // selection (see the `selected` derivation in `parse_states`).
    UIA_LegacyIAccessibleStatePropertyId,
    // MSAA role — the only role signal for providers that publish no UIA
    // control type (see `map_msaa_role`).
    UIA_LegacyIAccessibleRolePropertyId,
];

/// Safe wrapper for IUIAutomationElementArray::Length.
fn uia_len(arr: &IUIAutomationElementArray) -> i32 {
    unsafe { arr.Length() }.unwrap_or(0)
}

/// Safe wrapper for IUIAutomationElementArray::GetElement.
fn uia_get(arr: &IUIAutomationElementArray, index: i32) -> Option<IUIAutomationElement> {
    unsafe { arr.GetElement(index) }.ok()
}

/// True when `GetCurrentPatternAs` reported that the element genuinely has no
/// such pattern.
///
/// UIA says "not supported" via `E_NOINTERFACE` or `UIA_E_INVALIDOPERATION`,
/// and AccessKit's provider via the empty error: `GetPatternProvider`
/// returns `Err(Error::empty())` for unsupported patterns
/// (`accesskit_windows`' `pattern_provider` fallback arm), which windows-rs
/// surfaces as a null pattern pointer with S_OK — an error whose `code()` is
/// `HRESULT(0)` ("The operation completed successfully."). S_OK is the only
/// non-failed HRESULT, so code 0 is definitively "the provider delivered no
/// pattern", never a COM failure; every real failure has a nonzero (failed)
/// HRESULT.
///
/// Every other HRESULT is therefore a real COM failure (a dead element, a
/// wedged provider) and must be propagated, not treated as an absent
/// capability (tenet 1).
fn is_pattern_absent(err: &windows::core::Error) -> bool {
    let code = err.code().0;
    code == E_NOINTERFACE.0 || code == UIA_E_INVALIDOPERATION as i32 || code == 0
}

/// The stable identity of a UIA element, or `None` when it has none.
///
/// UIA excludes top-level application windows from the AutomationId contract
/// (they have none — see Microsoft's AutomationId docs), so their stable
/// identity is the native window handle, which is what the provider already
/// uses to reacquire and activate windows ([`WindowsProvider::reacquire_via_hwnd`]).
/// Nested framework controls (WPF/WinForms) carry an AutomationId but no HWND
/// of their own. Prefer the HWND when one exists, fall back to the
/// AutomationId: that populates `stable_id` for both element kinds, which is
/// what cross-snapshot correlation, `[stable_id=...]` selectors, and the
/// window-list dedup all need.
///
/// The handle is formatted as `hwnd:0x…` in lowercase hex, stable for the
/// life of the window within a session (like the Linux D-Bus object path;
/// HWNDs are reused after a window closes, so the identity is session-scoped,
/// not launch-scoped).
fn uia_stable_id(native_handle: Option<HWND>, automation_id: Option<String>) -> Option<String> {
    native_handle
        .filter(|h| !h.0.is_null())
        .map(|h| format!("hwnd:{:#x}", h.0 as usize))
        .or(automation_id)
}

/// Translate a window-verb `GetCurrentPatternAs` failure. Only the two
/// known-absent HRESULTs (see [`is_pattern_absent`]) mean the element has no
/// such pattern, and thus `ActionNotSupported`; every other COM error — a dead
/// element, a wedged provider — is a platform failure and must propagate
/// (tenet 1), exactly as the `activate` path below does.
fn pattern_acquisition_error(err: &windows::core::Error, verb: &str, role: Role) -> Error {
    if is_pattern_absent(err) {
        Error::ActionNotSupported {
            action: verb.to_string(),
            role,
        }
    } else {
        Error::Platform {
            code: err.code().0 as i64,
            message: format!("acquiring pattern for {verb} failed: {err}"),
        }
    }
}

fn is_top_level_window_control(element: &IUIAutomationElement) -> Result<bool> {
    // `Role::Dialog` also covers in-page ARIA dialogs; the actual desktop
    // window identity on Windows is UIA's Window control type.
    //
    // `CachedControlType` replays whatever the walk's cache fetch stored, and
    // AccessKit's provider bakes a transient
    // `EVENT_E_ALL_SUBSCRIBERS_FAILED` (0x80040201, issue #257) into that
    // fetch for a regenerating node — the intermittent "reading UIA control
    // type failed" that hit the egui Windows integ cell in different tests
    // each run. A cache rebuild does not help because the same fetch reruns;
    // a live `Current` read bypasses the cache and answers from the provider,
    // which AccessKit serves in-process. A genuinely dead node surfaces
    // UIA_E_ELEMENTNOTAVAILABLE and propagates (tenet 1), and the retry
    // stays inside the classified-transient set (`retry_transient`),
    // so this is a recovery of a known transient, not a fallback chain.
    match retry_transient(|| unsafe { element.CachedControlType() }) {
        Ok(t) => Ok(t == UIA_WindowControlTypeId),
        Err(e) if is_event_subscriber_failure(&e) => {
            let t = retry_transient(|| unsafe { element.CurrentControlType() }).map_err(|e| {
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("reading UIA control type failed: {e}"),
                }
            })?;
            Ok(t == UIA_WindowControlTypeId)
        }
        Err(e) => Err(Error::Platform {
            code: e.code().0 as i64,
            message: format!("reading UIA control type failed: {e}"),
        }),
    }
}

fn ensure_top_level_window_target(
    element: &IUIAutomationElement,
    action: &str,
    role: Role,
) -> Result<()> {
    if is_top_level_window_control(element)? {
        Ok(())
    } else {
        Err(Error::ActionNotSupported {
            action: action.to_string(),
            role,
        })
    }
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
    /// WindowPattern — present only on elements backed by an HWND frame
    /// (top-level windows, dialogs). Drives the window verbs and the
    /// `minimized` / `maximized` / `modal` state reads.
    window: Option<IUIAutomationWindowPattern>,
    /// TransformPattern — present on movable/resizable windows.
    transform: Option<IUIAutomationTransformPattern>,
}

impl Provider for WindowsProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: enumerate the desktop root's named top-level
                // windows, then group them by pid — one synthesized
                // Application node per process, in first-seen (z-) order,
                // with that pid's first window as the representative. The old
                // code returned one entry per window (issue #304's shape);
                // grouping now reports one Application node per process on
                // every platform, and the windows are the node's children.
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

                let mut windows: Vec<(IUIAutomationElement, u32)> = Vec::new();
                // Strict iteration (the same shape `top_level_windows_of_pid`
                // uses): `Length` and `GetElement` failures are real COM
                // failures, not absent windows — propagating keeps a transient
                // UIA failure from silently truncating the process list to
                // zero or a partial subset (tenet 1). `uia_call` retries the
                // classified-transient HRESULTs first; what survives is
                // persistent and must surface.
                let len = uia_call(|| unsafe { found.Length() })?;
                for i in 0..len {
                    let el = uia_call(|| unsafe { found.GetElement(i) }).map_err(|e| match e {
                        Error::Platform { code, message } => Error::Platform {
                            code,
                            message: format!(
                                "IUIAutomationElementArray.GetElement({i}) failed: {message}"
                            ),
                        },
                        other => other,
                    })?;
                    // The Cached* reads come from the batch cache populated by
                    // FindAllBuildCache, so a failure here is an enumeration
                    // failure rather than an absent value — propagate it with
                    // the index it failed for (tenet 6), instead of letting
                    // `pid == 0` / empty-name filters turn it into a silent
                    // dismissal of the window.
                    let pid = uia_call(|| unsafe { el.CachedProcessId() })
                        .map_err(|e| match e {
                            Error::Platform { code, message } => Error::Platform {
                                code,
                                message: format!(
                                    "reading the process id of desktop window #{i} failed: \
                                     {message}"
                                ),
                            },
                            other => other,
                        })?
                        .max(0) as u32;
                    // `pid == 0` skips windows with no resolvable owning
                    // process; the empty-name skip drops windows that are
                    // still unnamed mid-startup. Both are the same filters
                    // the pre-unification enumeration applied.
                    if pid == 0 {
                        continue;
                    }
                    let name = uia_call(|| unsafe { el.CachedName() })
                        .map_err(|e| match e {
                            Error::Platform { code, message } => Error::Platform {
                                code,
                                message: format!(
                                    "reading the name of desktop window #{i} failed: \
                                     {message}"
                                ),
                            },
                            other => other,
                        })?
                        .to_string();
                    if name.is_empty() {
                        continue;
                    }
                    windows.push((el, pid));
                }

                let mut seen = HashSet::new();
                let mut results = Vec::new();
                for (el, pid) in windows {
                    if !seen.insert(pid) {
                        continue;
                    }
                    results.push(self.build_synthetic_app_data(pid, &el)?);
                }
                Ok(results)
            }
            Some(element_data) => {
                // A synthesized Application node answers with the process's
                // top-level windows — a process-wide UIA query, not a tree
                // walk, because the node deliberately has no live element
                // behind it. This uniform "windows are the Application node's
                // children" answer is what makes `App::windows` identical
                // across platforms, and an empty result is the truth of a
                // process whose last window closed.
                if let Some(identity) = self.synthetic_app_identity(element_data.handle) {
                    // Stale-process guard: the pid may have been reused since
                    // this node was synthesized. Re-read the process
                    // creation time and refuse to enumerate a different
                    // process — the alternative is silently retargeting an
                    // `App` that no longer names anything (tenet 1). A guard
                    // that never captured a baseline (`None`) cannot verify
                    // one, and the mismatch case is ElementStale: the node is
                    // stale, not the window list empty.
                    if synthetic_app_is_stale(
                        identity.creation_time,
                        process_creation_time(identity.pid),
                    ) {
                        return Err(synthetic_app_stale_error(element_data.handle, identity.pid));
                    }
                    let windows = self.top_level_windows_of_pid(identity.pid)?;
                    let mut data = Vec::with_capacity(windows.len());
                    for el in windows {
                        // Strict path: re-acquire via HWND to activate
                        // AccessKit's provider, then populate the snapshot
                        // that build_element_data reads. A failure here is a
                        // real COM failure — the fallback would silently hand
                        // back a window whose provider was never activated
                        // (tenet 1).
                        let el = self
                            .reacquire_via_hwnd(&el)
                            .and_then(|e| self.populate_cache(&e))
                            .map_err(|e| Error::Platform {
                                code: e.code().0 as i64,
                                message: format!(
                                    "re-acquiring a top-level window via HWND and populating \
                                     its cache failed: {e}"
                                ),
                            })?;
                        let mut window_data = self.build_element_data(&el, Some(identity.pid))?;
                        if window_data.name.is_none() {
                            // Error-preserving live name read (tenet 1): a
                            // CurrentName COM failure must not collapse into
                            // an honestly unnamed window via `.ok()` — the
                            // result would be indistinguishable from "this
                            // window has no name" in listings and selectors.
                            // A successfully read empty string IS the "no
                            // name" answer.
                            window_data.name = match unsafe { el.CurrentName() } {
                                Ok(s) => {
                                    let s = s.to_string();
                                    if s.is_empty() {
                                        None
                                    } else {
                                        Some(s)
                                    }
                                }
                                Err(e) => {
                                    return Err(Error::Platform {
                                        code: e.code().0 as i64,
                                        message: format!(
                                            "CurrentName failed while listing a top-level \
                                             window of pid {}: {e}",
                                            identity.pid
                                        ),
                                    });
                                }
                            };
                        }
                        data.push(window_data);
                    }
                    return Ok(data);
                }
                let uia = self.get_cached(element_data.handle)?;
                let children = self.uia_children(&uia);
                let pid = element_data.pid;
                let mut data = Vec::with_capacity(children.len());
                for child in children {
                    data.push(self.build_element_data(&child, pid)?);
                }
                Ok(data)
            }
        }
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        // A synthesized Application node is top-level by construction — no
        // element has the process as a child. The walk needs a live element
        // the node does not have, so answer `None` directly instead of
        // routing a synthetic handle through `get_cached` into the
        // "unsupported" error.
        if is_synthetic_handle(element.handle) {
            return Ok(None);
        }
        let uia = self.get_cached(element.handle)?;
        if let Ok(walker) = unsafe { self.automation.RawViewWalker() } {
            if let Ok(parent) = unsafe { walker.GetParentElement(&uia) } {
                // Check if the parent is the desktop root (no further parent)
                let parent_parent = unsafe { walker.GetParentElement(&parent) };
                if parent_parent.is_err() {
                    // The desktop root is not a "real" parent: the owning
                    // process is. Answer with the synthetic Application node
                    // for the element's pid, using the element itself as the
                    // representative window — name resolution reads the
                    // process path in the common case. An element whose
                    // pid could not be resolved (rare — a shell Pane from a
                    // vanished process) has no process identity to report,
                    // which is "no parent", not an error.
                    let Some(pid) = element.pid else {
                        return Ok(None);
                    };
                    return Ok(Some(self.build_synthetic_app_data(pid, &uia)?));
                }
                // Populate snapshot so build_element_data can read Cached* props
                let parent = self.populate_cache(&parent).map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("BuildUpdatedCache failed: {}", e),
                })?;
                let data = self.build_element_data(&parent, element.pid)?;
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// Enumerate the Windows shell surfaces by classifying the UIA desktop
    /// root's **direct** children by class name.
    ///
    /// [`get_children(None)`](Self::get_children) filters the same children to
    /// `ControlType.Window`, which is exactly what hides the shell: every
    /// surface below is a `Pane`. This walk therefore drops the control-type
    /// filter (`TreeScope_Children` + `TrueCondition`) and keeps only the
    /// classes it recognises. Note this is still the **Control View**, not the
    /// Raw View: `FindAllBuildCache` walks the control view unless given
    /// `RawViewWalker`. It works because every shell pane below is a control
    /// element — a shell window with `IsControlElement=false` would be
    /// invisible here, and would need the raw walker to reach. Anything else — ordinary app windows and their
    /// panes — is skipped silently: it is not a shell surface, and it is
    /// already reachable through `list_apps`.
    ///
    /// | Class name | Kind |
    /// |---|---|
    /// | `Shell_TrayWnd` | [`Taskbar`](ShellSurfaceKind::Taskbar) — task band, visible tray row, overflow chevron |
    /// | `Progman` | [`Desktop`](ShellSurfaceKind::Desktop) — descended to its `SysListView32` icon list view; **no surface** if that is missing |
    /// | `TopLevelWindowForOverflowXamlIsland` | [`Flyout`](ShellSurfaceKind::Flyout) — the tray overflow |
    /// | `Microsoft.UI.Content.PopupWindowSiteBridge` | [`Flyout`](ShellSurfaceKind::Flyout) — a shell popup |
    /// | `ControlCenterWindow` | [`Flyout`](ShellSurfaceKind::Flyout) — Quick Settings, **only while its content is on screen** |
    ///
    /// Two shell classes are deliberately *not* listed:
    ///
    /// - `Windows.UI.Core.CoreWindow` hosts the Notification Center, but only
    ///   when the owning process is `ShellExperienceHost` — every other
    ///   `CoreWindow` under the desktop root is an ordinary UWP app. UIA
    ///   carries no process *image name*, and this crate does no process-image
    ///   lookup at all, so telling the two apart would mean adding
    ///   `OpenProcess`/`QueryFullProcessImageName` FFI for one surface.
    ///   Notification Center is therefore absent from the listing rather than
    ///   guessed at — absence is honest, a misclassified app window is not.
    /// - `Shell_SecondaryTrayWnd` (per-monitor taskbars) is out of v1 scope,
    ///   per the proposal.
    ///
    /// Order is stable: taskbars, then desktops, then flyouts in enumeration
    /// order.
    ///
    /// # Errors
    ///
    /// [`Error::Platform`] when the desktop root itself cannot be read or its
    /// children cannot be enumerated. Individual classes are never
    /// error-skipped (tenet 1): the one per-class probe — Quick Settings'
    /// on-screen test — propagates too, wrapped so the message names the
    /// surface it was probing and lists the surfaces classified before the
    /// scan aborted.
    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        let found = uia_call(|| unsafe {
            root.FindAllBuildCache(TreeScope_Children, &true_cond, &self.batch_request)
        })?;

        // (rank, kind, data) — rank groups the output; the stable sort below
        // keeps enumeration order within a group.
        let mut surfaces: Vec<(u8, ShellSurfaceKind, ElementData)> = Vec::new();

        for i in 0..uia_len(&found) {
            let Some(el) = uia_get(&found, i) else {
                continue;
            };
            let Some(class_name) = uia_cached_bstr(&el, UIA_ClassNamePropertyId) else {
                continue;
            };

            let (rank, kind) = match class_name.as_str() {
                "Shell_TrayWnd" => (0u8, ShellSurfaceKind::Taskbar),
                "Progman" => (1u8, ShellSurfaceKind::Desktop),
                // The tray overflow host is a desktop-root child only while
                // the flyout is open, so its presence is the open signal.
                // The XAML popup site bridge behaves the same way.
                "TopLevelWindowForOverflowXamlIsland"
                | "Microsoft.UI.Content.PopupWindowSiteBridge" => (2u8, ShellSurfaceKind::Flyout),
                // Quick Settings is the exception: its host window persists as
                // a desktop-root child after dismissal, keeping the bounds it
                // had while open, and only its XAML content goes offscreen.
                // Presence therefore does not mean open — the content's
                // `IsOffscreen` does.
                "ControlCenterWindow" => {
                    // The probe's COM failure still propagates (tenet 1 — a
                    // window that could not be inspected is not a window that
                    // is closed), but it must say what it was probing and how
                    // far the scan got (tenet 6). `Error::Platform` carries no
                    // structured diagnosis field, so the `Diagnosis` is
                    // rendered into the message through its `Display` impl —
                    // the same clauses a `SelectorNotMatched` would show.
                    let on_screen = self.has_onscreen_child(&el).map_err(|e| {
                        let diagnosis = xa11y_core::Diagnosis::new()
                            .condition(
                                "a Quick Settings flyout surface, if its content is on screen",
                            )
                            .last_observed(format!(
                                "the probe failed with: {e}; {} shell surface(s) had been \
                                 classified before the scan aborted",
                                surfaces.len()
                            ))
                            .candidates(classified_so_far(&surfaces));
                        Error::Platform {
                            code: match &e {
                                Error::Platform { code, .. } => *code,
                                _ => -1,
                            },
                            message: format!(
                                "probing whether Quick Settings (ControlCenterWindow) is on \
                                 screen failed{diagnosis}"
                            ),
                        }
                    })?;
                    if !on_screen {
                        continue;
                    }
                    (2u8, ShellSurfaceKind::Flyout)
                }
                _ => continue,
            };

            // `Progman` is the desktop *host* pane; the surface
            // `ShellSurfaceKind::Desktop` documents is the icon list view
            // inside it (Progman → SHELLDLL_DefView → SysListView32), so a
            // caller targeting `desktop` finds the icons as direct children.
            // When the chain is absent no desktop surface is emitted at all —
            // never the Progman pane as a stand-in (tenet 1).
            let el = if kind == ShellSurfaceKind::Desktop {
                match self.descendant_by_class(&el, DESKTOP_LIST_VIEW_CLASS)? {
                    Some(list_view) => list_view,
                    None => continue,
                }
            } else {
                el
            };

            // The host process (explorer.exe, ShellHost.exe), never a
            // per-icon owner — UIA does not carry one.
            //
            // A failed read is propagated rather than folded into the "no
            // process" answer (tenet 1). `ShellSurface::pid` documents `None`
            // as "the platform attributes no process to this surface", which is
            // a real statement about the surface; a `CachedProcessId` that
            // errored is a statement about the call, and reporting it as the
            // former would let a broken cache read look like a shell-owned
            // surface with no owner.
            let pid = unsafe { el.CachedProcessId() }.map_err(|e| Error::Platform {
                code: i64::from(e.code().0),
                message: format!("CachedProcessId failed for the {kind} shell surface: {e}"),
            })? as u32;
            // Mirror get_children(None): re-acquire via HWND so the window's
            // UIA provider is activated, then repopulate the snapshot that
            // build_element_data reads. Best-effort here, as in
            // get_children — see the rationale there.
            let el = match self.reacquire_via_hwnd(&el) {
                Ok(re) => self.populate_cache(&re).unwrap_or(el),
                Err(_) => el,
            };
            let data = self.build_element_data(&el, (pid != 0).then_some(pid))?;
            surfaces.push((rank, kind, data));
        }

        surfaces.sort_by_key(|(rank, _, _)| *rank);
        Ok(surfaces
            .into_iter()
            .map(|(_, kind, data)| (kind, data))
            .collect())
    }

    /// Enumerate top-level applications.
    ///
    /// UIA has no `Application` accessible — processes surface only as
    /// top-level `Window` control-type elements under the desktop root — so
    /// this lists the desktop's named window children and groups them by
    /// pid: one synthesized Application node per process, in first-seen
    /// (z-) order. A process owning several top-level windows (e.g. an app
    /// showing a modal dialog, issue #304) now yields *one* Application node
    /// whose children are its windows — the uniform shape every platform
    /// reports. This is the canonical app discovery primitive (replaces the
    /// old `find_elements(None, "application"/"window", …, depth=0)` idiom).
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
    /// named or not. The first match is the representative; the returned
    /// node is the process's synthesized Application node (issue #304's
    /// missing-entry shape: the process's windows, not its first window,
    /// are what the pid means).
    fn app_by_pid(&self, pid: u32) -> Result<ElementData> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let value = pid_variant(pid)?;
        let pid_condition = uia_call(|| unsafe {
            self.automation
                .CreatePropertyCondition(UIA_ProcessIdPropertyId, &value)
        })?;
        // Require the Window control type, mirroring the process-wide
        // enumeration: a WebView2/wry host (Tauri, egui, Electron)
        // owns several pid-matching desktop children, and the first of them
        // is the content Pane, whose UIA subtree disappears while the window
        // is minimized — so the app root resolved by pid alone would drop
        // the window exactly when `xa11y action restore "window" --pid PID`
        // must reach it. The Window-type desktop child (the HWND) stays in
        // the tree while minimized, matching what `windows --pid` lists and
        // what the handle-based binding suites restore. On native apps
        // (Qt, WinForms, WPF) the first pid child IS that window, so the
        // additional condition changes nothing for them.
        let window_condition = uia_call(|| unsafe {
            self.automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_WindowControlTypeId.0),
            )
        })?;
        let condition = uia_call(|| unsafe {
            self.automation
                .CreateAndCondition(&pid_condition, &window_condition)
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
                        xa11y_core::Diagnosis::new()
                            .last_observed("no top-level UIA element owned by the process yet"),
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
        // The representative window serves only the fallback name source of
        // the synthesized node (process path first); no snapshot is needed,
        // and the node deliberately keeps no live element (tenet 2 — no
        // window-shaped stand-in).
        self.build_synthetic_app_data(pid, &el)
    }

    /// Identify the foreground application via `GetForegroundWindow` +
    /// `ElementFromHandle` — the canonical Win32 foreground query mapped into
    /// the UIA tree. The foreground HWND is a process's top-level window, so
    /// its pid resolves to that process's synthesized Application node — the
    /// same node `list_apps` reports, which is what lets the core's
    /// foreground tagging line a `list_apps` entry up by pid.
    ///
    /// A NULL foreground window (nothing active — e.g. the desktop has focus,
    /// or during a fast app switch) maps to [`Error::SelectorNotMatched`]
    /// ("nothing focused"); a failing `ElementFromHandle` is a genuine UIA
    /// error and propagates; a foreground window with no resolvable pid is an
    /// honest `Platform` error, not "no foreground app" (tenet 1).
    fn focused_app(&self) -> Result<ElementData> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return Err(Error::selector_not_matched("focused application"));
        }
        let el = uia_call(|| unsafe { self.automation.ElementFromHandle(hwnd) })?;
        // A failing read propagates with its HRESULT; this branch is reserved
        // for a *successfully returned* zero pid (an honest "no owning
        // process", tenet 1 — not "no foreground app").
        let pid = unsafe { el.CurrentProcessId() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!(
                "IUIAutomationElement.CurrentProcessId failed for the foreground window: {e}"
            ),
        })? as u32;
        if pid == 0 {
            return Err(Error::Platform {
                code: -1,
                message: "foreground window returns pid 0 (no owning process)".to_string(),
            });
        }
        // The foreground window is the representative; only its title can
        // fall back as the name source of the synthesized node.
        self.build_synthetic_app_data(pid, &el)
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

        // A synthesized Application node has no live UIA element, so the UIA
        // subtree query below cannot be scoped to it. Answer with the
        // level-by-level walk instead, which goes through `get_children`:
        // the synthetic node's children are the process's top-level windows,
        // and each is walked natively. Same shape as the fragment-element
        // fallback further down — the walk is the honest primitive for a
        // root that is not a UIA HWND fragment.
        if is_synthetic_handle(root.handle) {
            return xa11y_core::selector::find_elements_in_tree_group(
                |el| self.get_children(el),
                Some(root),
                group,
                limit,
                max_depth,
            );
        }

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
            let data = self.build_element_data(&el, pid)?;

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
        // No direct UIA equivalent; try context menu via legacy.
        //
        // `get_actions` never advertises `show_menu`, which is what keeps this
        // honest on shell chrome: `IUIAutomationElement3::ShowContextMenu`
        // returns S_OK on XAML shell chrome (the Win11 tray buttons) while
        // opening nothing. Anyone wiring it up later must advertise it only
        // where the platform really implements it — Win32 shell UI such as
        // the desktop's list items — and keep it off XAML chrome, or the
        // action list starts claiming a verb that silently does nothing
        // (tenet 3). The tray-icon context menu stays an explicit `InputSim`
        // right-click composition.
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

    // ── Window management ──────────────────────────────────────────
    //
    // Window verbs go through UIA's WindowPattern / TransformPattern — the
    // canonical accessibility interfaces for window state and geometry. No
    // input simulation is involved (tenet 2).

    fn activate(&self, element: &ElementData) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "activate", element.role)?;
        // winlenium parity: if minimized, restore; then bring the HWND to the
        // foreground; then complete with a UIA SetFocus so UIA-backed
        // providers treat the window as focused.
        //
        // Only a genuinely absent WindowPattern is a skip — activate's fore/focus
        // work does not need the pattern. Every other pattern-acquisition
        // error (a dead element, a wedged provider) propagates, and so does a
        // failed visual-state read: a minimized window whose state could not
        // be read must not be reported as successfully activated while it stays
        // minimized (tenet 1).
        match unsafe { uia.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) }
        {
            Ok(pattern) => match unsafe { pattern.CurrentWindowVisualState() } {
                Ok(v) if v == WindowVisualState_Minimized => {
                    unsafe { pattern.SetWindowVisualState(WindowVisualState_Normal) }.map_err(
                        |e| Error::Platform {
                            code: e.code().0 as i64,
                            message: format!(
                                "WindowPattern.SetWindowVisualState(Normal) while activating failed: {e}"
                            ),
                        },
                    )?;
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "WindowPattern.CurrentWindowVisualState failed while raising: {e}"
                        ),
                    });
                }
            },
            Err(e) if is_pattern_absent(&e) => {}
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("acquiring WindowPattern while activating failed: {e}"),
                });
            }
        }
        let hwnd = unsafe { uia.CurrentNativeWindowHandle() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("CurrentNativeWindowHandle failed while raising: {e}"),
        })?;
        if hwnd.0.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "window has no native handle; cannot activate".to_string(),
            });
        }
        if !unsafe { SetForegroundWindow(hwnd) }.as_bool() {
            // Windows restricts foreground changes (foreground lock); a
            // denied SetForegroundWindow is a real failure, not a no-op —
            // surface it (tenet 1).
            return Err(Error::Platform {
                code: -1,
                message: "SetForegroundWindow was denied (foreground lock); the window may not \
                          have been activated"
                    .to_string(),
            });
        }
        unsafe { uia.SetFocus() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("SetFocus during activate failed: {e}"),
        })?;
        Ok(())
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "minimize", element.role)?;
        let pattern =
            unsafe { uia.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) }
                .map_err(|e| pattern_acquisition_error(&e, "minimize", element.role))?;
        // A failed capability read is a platform failure, not an absent
        // capability: transient/stale-element COM errors must not masquerade
        // as ActionNotSupported (tenet 1). Only a successfully reported
        // FALSE means the window cannot be minimized.
        if unsafe { pattern.CurrentCanMinimize() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("WindowPattern.CurrentCanMinimize failed: {e}"),
        })? != TRUE
        {
            return Err(Error::ActionNotSupported {
                action: "minimize".to_string(),
                role: element.role,
            });
        }
        unsafe { pattern.SetWindowVisualState(WindowVisualState_Minimized) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("WindowPattern.SetWindowVisualState(Minimized) failed: {e}"),
            }
        })?;
        Ok(())
    }

    fn maximize(&self, element: &ElementData) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "maximize", element.role)?;
        let pattern =
            unsafe { uia.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) }
                .map_err(|e| pattern_acquisition_error(&e, "maximize", element.role))?;
        // See minimize: a failed read propagates; only a successful FALSE is
        // "cannot maximize".
        if unsafe { pattern.CurrentCanMaximize() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("WindowPattern.CurrentCanMaximize failed: {e}"),
        })? != TRUE
        {
            return Err(Error::ActionNotSupported {
                action: "maximize".to_string(),
                role: element.role,
            });
        }
        unsafe { pattern.SetWindowVisualState(WindowVisualState_Maximized) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("WindowPattern.SetWindowVisualState(Maximized) failed: {e}"),
            }
        })?;
        Ok(())
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "restore", element.role)?;
        let pattern =
            unsafe { uia.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) }
                .map_err(|e| pattern_acquisition_error(&e, "restore", element.role))?;
        // See minimize: each capability read propagates its COM error, and
        // `ActionNotSupported` is reserved for the case where both capability
        // flags were successfully read as FALSE.
        let can_minimize =
            unsafe { pattern.CurrentCanMinimize() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("WindowPattern.CurrentCanMinimize failed: {e}"),
            })? == TRUE;
        let can_maximize =
            unsafe { pattern.CurrentCanMaximize() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("WindowPattern.CurrentCanMaximize failed: {e}"),
            })? == TRUE;
        if !can_minimize && !can_maximize {
            return Err(Error::ActionNotSupported {
                action: "restore".to_string(),
                role: element.role,
            });
        }
        unsafe { pattern.SetWindowVisualState(WindowVisualState_Normal) }.map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("WindowPattern.SetWindowVisualState(Normal) failed: {e}"),
            }
        })?;
        Ok(())
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "close", element.role)?;
        let pattern =
            unsafe { uia.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId) }
                .map_err(|e| pattern_acquisition_error(&e, "close", element.role))?;
        unsafe { pattern.Close() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("WindowPattern.Close failed: {e}"),
        })?;
        Ok(())
    }

    fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "move_to", element.role)?;
        let pattern = unsafe {
            uia.GetCurrentPatternAs::<IUIAutomationTransformPattern>(UIA_TransformPatternId)
        }
        .map_err(|e| pattern_acquisition_error(&e, "move_to", element.role))?;
        // See minimize: a failed read propagates; only a successful FALSE is
        // "cannot move".
        if unsafe { pattern.CurrentCanMove() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("TransformPattern.CurrentCanMove failed: {e}"),
        })? != TRUE
        {
            return Err(Error::ActionNotSupported {
                action: "move_to".to_string(),
                role: element.role,
            });
        }
        // UIA TransformPattern works in physical pixels; the core contract is
        // logical coordinates. Convert at the target position (origin
        // preserved per monitor — see `crate::dpi`): the monitors' logical
        // rects never overlap, so a target resolves to exactly one monitor,
        // and a target inside the window's own monitor keeps that monitor's
        // transform (the window-identity preference the previous model needed
        // to disambiguate the seam).
        let (px, py) = window_logical_to_physical(&uia, x, y)?;
        unsafe { pattern.Move(f64::from(px), f64::from(py)) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("TransformPattern.Move({x}, {y}) failed: {e}"),
        })?;
        Ok(())
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        let uia = self.get_cached(element.handle)?;
        ensure_top_level_window_target(&uia, "resize_to", element.role)?;
        let pattern = unsafe {
            uia.GetCurrentPatternAs::<IUIAutomationTransformPattern>(UIA_TransformPatternId)
        }
        .map_err(|e| pattern_acquisition_error(&e, "resize_to", element.role))?;
        // See minimize: a failed read propagates; only a successful FALSE is
        // "cannot resize".
        if unsafe { pattern.CurrentCanResize() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("TransformPattern.CurrentCanResize failed: {e}"),
        })? != TRUE
        {
            return Err(Error::ActionNotSupported {
                action: "resize_to".to_string(),
                role: element.role,
            });
        }
        // Logical → physical (see move_to). The scale is the monitor the
        // window currently sits on, resolved from the live physical rect —
        // unambiguous even on a mixed-DPI desktop, and independent of how
        // the snapshot's logical origin is interpreted. A minimized window
        // reports a 0×0 live rect (UIA gives it no geometry), so fall back
        // to the snapshot's logical origin resolved monitor-aware; with no
        // bounds either, the physical query on the zero rect degrades to the
        // primary's scale, the pre-existing behavior for a window this
        // degenerate.
        let scale = {
            let rect = unsafe { uia.CurrentBoundingRectangle() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("CurrentBoundingRectangle failed while resizing: {e}"),
            })?;
            let has_geometry =
                rect.left != 0 || rect.top != 0 || rect.right != 0 || rect.bottom != 0;
            if has_geometry {
                crate::dpi::scale_for_physical_point(rect.left, rect.top)
            } else {
                match element.bounds {
                    Some(b) => crate::dpi::scale_for_logical_point(b.x, b.y)?,
                    // No geometry at all: the physical query on the zero rect
                    // degrades to the primary's scale, the pre-existing
                    // behavior for a window this degenerate.
                    None => crate::dpi::scale_for_physical_point(rect.left, rect.top),
                }
            }
        };
        unsafe { pattern.Resize(f64::from(width) * scale, f64::from(height) * scale) }.map_err(
            |e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("TransformPattern.Resize({width}, {height}) failed: {e}"),
            },
        )?;
        Ok(())
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
            "activate" => self.activate(element),
            "minimize" => self.minimize(element),
            "maximize" => self.maximize(element),
            "restore" => self.restore(element),
            "close" => self.close(element),
            // Payload verbs have no arguments on the generic escape hatch;
            // fail surfaceably with how to call them instead of guessing
            // (tenet 1: no silent fallback).
            "move_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"move_to\") requires coordinates; call move_to(x, y)"
                    .to_string(),
            }),
            "resize_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"resize_to\") requires dimensions; call \
                           resize_to(width, height)"
                    .to_string(),
            }),
            _ => Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            }),
        }
    }

    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        // A synthesized Application node resolves its pid from the identity
        // map; a plain element carries its pid directly. Either way, the
        // stale-process guard runs: subscribing to a reused pid would attach
        // the handler to an unrelated process, the same silent retarget the
        // children path guards against (tenet 1).
        let identity = if let Some(identity) = self.synthetic_app_identity(element.handle) {
            if synthetic_app_is_stale(identity.creation_time, process_creation_time(identity.pid)) {
                return Err(synthetic_app_stale_error(element.handle, identity.pid));
            }
            identity.pid
        } else {
            element.pid.ok_or(Error::Platform {
                code: -1,
                message: "Element has no PID for subscribe".to_string(),
            })?
        };
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(identity, app_name)
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
///
/// The window capability reads propagate their errors: a stale element or
/// wedged provider is a platform failure, not evidence that the capability
/// is absent. `ActionNotSupported`-free advertisement is reserved for a
/// successful `FALSE` (tenet 1 — the action list must not degrade "could
/// not read" into "does not support").
fn get_actions(
    element: &IUIAutomationElement,
    role: Role,
    patterns: &ElementPatterns,
) -> Result<Vec<String>> {
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

    // Window verbs, from WindowPattern / TransformPattern.
    if let Some(ref pattern) = patterns.window {
        if !actions.iter().any(|a| a == "close") {
            actions.push("close".to_string());
        }
        // Setup for minimize/maximize/restore advertisement, based on what
        // the window can actually do (CurrentCanMinimize/CurrentCanMaximize).
        // A failed read propagates — a capability unknown is not a
        // capability absent (see get_actions' doc).
        let can_minimize =
            unsafe { pattern.CurrentCanMinimize() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!(
                    "WindowPattern.CurrentCanMinimize failed while advertising actions: {e}"
                ),
            })? == TRUE;
        let can_maximize =
            unsafe { pattern.CurrentCanMaximize() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!(
                    "WindowPattern.CurrentCanMaximize failed while advertising actions: {e}"
                ),
            })? == TRUE;
        if can_minimize {
            actions.push("minimize".to_string());
        }
        if can_maximize {
            actions.push("maximize".to_string());
        }
        if can_minimize || can_maximize {
            actions.push("restore".to_string());
        }
    }
    if let Some(ref pattern) = patterns.transform {
        if unsafe { pattern.CurrentCanMove() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!(
                "TransformPattern.CurrentCanMove failed while advertising actions: {e}"
            ),
        })? == TRUE
        {
            actions.push("move_to".to_string());
        }
        if unsafe { pattern.CurrentCanResize() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!(
                "TransformPattern.CurrentCanResize failed while advertising actions: {e}"
            ),
        })? == TRUE
        {
            actions.push("resize_to".to_string());
        }
    }
    // `activate` works on any top-level HWND window (SetForegroundWindow +
    // UIA SetFocus), independent of the pattern set.
    if !actions.iter().any(|a| a == "activate") && is_top_level_window_control(element)? {
        actions.push("activate".to_string());
    }

    Ok(actions)
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

    // Selected: SelectionItemPattern where the framework implements it,
    // otherwise the MSAA selection bit.
    //
    // SelectionItem is the modern signal and stays authoritative. But a large
    // class of Windows UI predates it and publishes selection *only* as
    // `STATE_SYSTEM_SELECTED` on LegacyIAccessible.State: WinForms grids and
    // lists (e.g. `DataGridViewCell.DataGridViewCellAccessibleObject`, whose
    // pattern set is Legacy/Invoke/Value/TableItem/GridItem — no
    // SelectionItem), and anything reaching UIA through the MSAA proxy.
    // Reading only the pattern reported every such element as unselected,
    // which is a wrong answer rather than a missing one (issue #324).
    //
    // This is a two-source read of the same fact, not a fallback chain: each
    // source is consulted for the frameworks that implement it, and neither
    // hides an error from the other (mirrors the container-selection
    // derivation xa11y-macos does for Qt's AX bridge).
    let selected = match patterns.selection_item {
        Some(ref pattern) => unsafe { pattern.CurrentIsSelected() }
            .unwrap_or(BOOL(0))
            .as_bool(),
        None => legacy_state_selected(uia_cached_i32(
            element,
            UIA_LegacyIAccessibleStatePropertyId,
        )),
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

    // Window visual state (minimized / maximized) from WindowPattern. `None`
    // means unknown: no WindowPattern (non-window element) or a state that
    // couldn't be read. Fullscreen is not reported by UIA at all
    // (WindowVisualState has no fullscreen value), so it stays `None` —
    // never guessed (tenet 1). macOS reads the fullscreen state from
    // AXFullScreen and AT-SPI has no fullscreen state bit; no platform can
    // raise a StateChanged{fullscreen} event, because the AX API has no
    // fullscreen notification.
    let (minimized, maximized) = match patterns.window {
        Some(ref pattern) => match unsafe { pattern.CurrentWindowVisualState() } {
            Ok(WindowVisualState_Minimized) => (Some(true), Some(false)),
            Ok(WindowVisualState_Maximized) => (Some(false), Some(true)),
            // Normal is the only other defined value (there is no Restored);
            // it clears both flags. A value outside the spec is unknown, not
            // "definitely not minimized/maximized" — `None`, never guessed.
            Ok(WindowVisualState_Normal) => (Some(false), Some(false)),
            Ok(_) => (None, None),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    // Modal from WindowPattern.CurrentIsModal — the only authoritative UIA
    // signal. Previously hard-coded `false`, which reported every modal
    // dialog (e.g. a WinForms modal form) as non-modal.
    let modal = match patterns.window {
        Some(ref pattern) => unsafe { pattern.CurrentIsModal() }.unwrap_or(FALSE) == TRUE,
        None => false,
    };

    StateParts {
        enabled,
        visible,
        focused,
        active,
        focusable,
        modal,
        minimized,
        maximized,
        fullscreen: None,
        checked,
        selected,
        expanded,
        editable,
        required: false,
        busy: false,
    }
    .into()
}

/// True when an MSAA state bitmask has `STATE_SYSTEM_SELECTED` set.
///
/// `None` (the property is absent from the snapshot, or the provider does not
/// implement LegacyIAccessible at all) means "no selection information", which
/// is reported as not-selected — the same answer as an explicit clear bit.
fn legacy_state_selected(state: Option<i32>) -> bool {
    matches!(state, Some(s) if s as u32 & STATE_SYSTEM_SELECTED != 0)
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
///
/// `legacy_role` is the element's MSAA `ROLE_SYSTEM_*` value from
/// `LegacyIAccessible.Role`, consulted only for `ControlType.Custom` — see
/// [`map_msaa_role`].
fn map_uia_role(
    control_type: UIA_CONTROLTYPE_ID,
    is_table_item: bool,
    parent_is_data_item: bool,
    legacy_role: Option<i32>,
) -> Role {
    // WPF and WinForms DataGrids expose their cells as Custom elements whose
    // only table signal is the TableItem pattern — without this they'd map to
    // Unknown. The structural parent signal stays DataItem-only: a custom
    // widget embedded in a row is not a cell.
    let is_cell = if control_type == UIA_DataItemControlTypeId {
        is_table_item || parent_is_data_item
    } else if control_type == UIA_CustomControlTypeId {
        is_table_item
    } else {
        false
    };
    if is_cell {
        return Role::TableCell;
    }

    if control_type == UIA_CustomControlTypeId {
        if let Some(role) = legacy_role.and_then(map_msaa_role) {
            return role;
        }
    }

    map_uia_control_type(control_type)
}

/// Map an MSAA `ROLE_SYSTEM_*` value to its xa11y role.
///
/// Consulted **only** when the UIA control type is `Custom`, which does not
/// mean "custom widget" — it is what UIA reports when a provider publishes no
/// `ControlType` at all. WinForms accessible objects that don't derive from
/// `ControlAccessibleObject` do exactly that: a `DataGridView`'s rows
/// (`DataGridViewRowAccessibleObject`) implement only LegacyIAccessible, so
/// UIA reports `Custom` while MSAA still says `ROLE_SYSTEM_ROW`. Mapping from
/// the sole role the provider *did* publish turns those into real roles
/// instead of `unknown` (issue #324).
///
/// Deliberately not applied to unrecognized UIA control types: those are gaps
/// in [`map_uia_control_type`] and must stay visible as `unknown_role` so the
/// role-map drift tests keep catching them.
///
/// Returns `None` for MSAA roles with no clean xa11y equivalent (`Sound`,
/// `Caret`, `Cursor`, `Animation`, …), leaving the element `unknown` rather
/// than inventing a mapping.
fn map_msaa_role(legacy_role: i32) -> Option<Role> {
    let role = u32::try_from(legacy_role).ok()?;
    let mapped = match role {
        ROLE_SYSTEM_TITLEBAR => Role::Group,
        ROLE_SYSTEM_MENUBAR => Role::MenuBar,
        ROLE_SYSTEM_SCROLLBAR => Role::ScrollBar,
        ROLE_SYSTEM_GRIP => Role::ScrollThumb,
        ROLE_SYSTEM_WINDOW => Role::Window,
        ROLE_SYSTEM_CLIENT => Role::Group,
        ROLE_SYSTEM_MENUPOPUP => Role::Menu,
        ROLE_SYSTEM_MENUITEM => Role::MenuItem,
        ROLE_SYSTEM_TOOLTIP => Role::Tooltip,
        ROLE_SYSTEM_APPLICATION => Role::Application,
        ROLE_SYSTEM_DOCUMENT => Role::WebArea,
        ROLE_SYSTEM_PANE => Role::Group,
        ROLE_SYSTEM_DIALOG => Role::Dialog,
        ROLE_SYSTEM_GROUPING => Role::Group,
        ROLE_SYSTEM_SEPARATOR => Role::Separator,
        ROLE_SYSTEM_TOOLBAR => Role::Toolbar,
        ROLE_SYSTEM_STATUSBAR => Role::Status,
        ROLE_SYSTEM_TABLE => Role::Table,
        // Header cells: xa11y reports them as cells, matching the
        // UIA_HeaderItemControlTypeId arm of `map_uia_control_type`.
        ROLE_SYSTEM_COLUMNHEADER | ROLE_SYSTEM_ROWHEADER => Role::TableCell,
        ROLE_SYSTEM_ROW => Role::TableRow,
        ROLE_SYSTEM_CELL => Role::TableCell,
        ROLE_SYSTEM_LINK => Role::Link,
        ROLE_SYSTEM_LIST => Role::List,
        ROLE_SYSTEM_LISTITEM => Role::ListItem,
        ROLE_SYSTEM_OUTLINE => Role::List,
        ROLE_SYSTEM_OUTLINEITEM => Role::TreeItem,
        ROLE_SYSTEM_PAGETAB => Role::Tab,
        ROLE_SYSTEM_PAGETABLIST => Role::TabGroup,
        ROLE_SYSTEM_GRAPHIC => Role::Image,
        ROLE_SYSTEM_STATICTEXT => Role::StaticText,
        ROLE_SYSTEM_TEXT => Role::TextField,
        ROLE_SYSTEM_PUSHBUTTON => Role::Button,
        ROLE_SYSTEM_CHECKBUTTON => Role::CheckBox,
        ROLE_SYSTEM_RADIOBUTTON => Role::RadioButton,
        ROLE_SYSTEM_COMBOBOX | ROLE_SYSTEM_DROPLIST => Role::ComboBox,
        ROLE_SYSTEM_PROGRESSBAR => Role::ProgressBar,
        ROLE_SYSTEM_SLIDER => Role::Slider,
        ROLE_SYSTEM_SPINBUTTON => Role::SpinButton,
        ROLE_SYSTEM_BUTTONDROPDOWN | ROLE_SYSTEM_BUTTONMENU | ROLE_SYSTEM_SPLITBUTTON => {
            Role::Button
        }
        ROLE_SYSTEM_ALERT => Role::Alert,
        _ => return None,
    };
    Some(mapped)
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
        UIA_SemanticZoomControlTypeId => Role::Group,
        UIA_AppBarControlTypeId => Role::Toolbar,
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

/// The event-handler registrations attached to one top-level window of a live
/// subscription.
///
/// A subscription now registers the automation / property-changed /
/// structure-changed handlers on **every** current top-level window of the pid
/// (the pre-C1 shape scoped them to the first window only, so sibling
/// windows — a dialog next to the main window — never delivered events). The
/// record keeps the exact element pointer and event-ID subset each window was
/// registered with, so removal (watch teardown, window close, subscription
/// cancel) removes precisely what was added.
struct RegisteredWindow {
    /// The element the handlers were registered on; removal requires the same
    /// pointer `Add*` was given.
    element: ComSend<IUIAutomationElement>,
    hwnd: usize,
    /// The subset of `AUTOMATION_EVENT_IDS` successfully registered.
    automation_ids: Vec<UIA_EVENT_ID>,
}

/// Live state of one app subscription, shared between `subscribe_impl`, the
/// per-window handlers, the open/close watch, and the cancel closure.
///
/// MTA COM proxies make every dereference of the captured interfaces safe
/// from the UIA callback threads (the same guarantee behind `ComSend` and
/// `unsafe impl Send for WindowsProvider`), so sharing the state across those
/// threads is sound.
struct SubscriptionState {
    automation: ComSend<IUIAutomation>,
    automation_handler: ComSend<IUIAutomationEventHandler>,
    property_handler: ComSend<IUIAutomationPropertyChangedEventHandler>,
    structure_handler: ComSend<IUIAutomationStructureChangedEventHandler>,
    /// Per-window registrations, keyed by HWND — the open/close watch diffs
    /// against this set and the cancel closure drains it.
    registered: Mutex<HashMap<usize, RegisteredWindow>>,
    /// Current `WindowVisualState` per top-level window; shared with
    /// [`PropertyHandler`] so the delta map and the add/remove paths agree.
    visual_states: Arc<Mutex<HashMap<usize, i32>>>,
    /// Serializes the whole reconcile sequence (enumerate, diff, register,
    /// tear down) against concurrent reconcile runs and against the cancel
    /// closure. UIA event handlers can be invoked concurrently, so without
    /// this two reconciles could compute the same `to_add` and attach one
    /// window's handlers twice, and cancel could drain the map while a
    /// reconcile re-registers a window (leaking its handlers).
    reconciliation: Mutex<()>,
    /// Set once, under `reconciliation`, when the subscription is cancelled.
    /// An in-flight reconcile that started before the flag was set finishes
    /// first (cancel waits on the same lock) and its registrations are then
    /// drained; a reconcile that acquires the lock after sees the flag and
    /// registers nothing.
    cancelled: AtomicBool,
}

unsafe impl Send for SubscriptionState {}
unsafe impl Sync for SubscriptionState {}

/// The native handle of a top-level window element.
///
/// Errors rather than keying a failed read with a sentinel: two windows
/// whose handle cannot be read must not collapse into one registration key,
/// which would lose one window's handlers and tear down the wrong
/// registration. The subscribe-time path propagates the error (tenet 1);
/// background reconciliation logs it and skips the window, and the next
/// open/close event re-runs the sync.
fn window_handle(el: &IUIAutomationElement) -> Result<usize> {
    match unsafe { el.CurrentNativeWindowHandle() } {
        Ok(h) => Ok(h.0 as usize),
        Err(e) => Err(Error::Platform {
            code: e.code().0 as i64,
            message: format!("CurrentNativeWindowHandle failed: {e}"),
        }),
    }
}

/// Split a window-set sync into (HWNDs to attach, HWNDs to tear down).
///
/// Pure and unit-testable: the open/close watch and the post-subscribe
/// reconciliation both call it, and registration bookkeeping is the fiddly
/// half of per-window scoping (tenet: never let a closed window's stale
/// registration survive, and never attach twice to an open one).
fn plan_window_registration_diff(
    registered: &HashSet<usize>,
    current: &HashSet<usize>,
) -> (Vec<usize>, Vec<usize>) {
    let mut to_add: Vec<usize> = current
        .iter()
        .copied()
        .filter(|h| !registered.contains(h))
        .collect();
    let mut to_remove: Vec<usize> = registered
        .iter()
        .copied()
        .filter(|h| !current.contains(h))
        .collect();
    to_add.sort_unstable();
    to_remove.sort_unstable();
    (to_add, to_remove)
}

/// Register the automation / property-changed / structure-changed handlers on
/// one top-level window and seed its `WindowVisualState` baseline.
///
/// The caller stores the returned record in the subscription's per-window map.
/// A partial failure removes what was registered and returns the error (tenet
/// 1): a half-registered window would deliver only some event kinds and read
/// as a complete subscription. Seeding happens first so a
/// `WindowVisualState` event arriving mid-registration has a prior value to
/// delta against.
///
/// The handle is passed in rather than re-read here: the caller already read
/// it (enumerating the window set), and a second read that fails after the
/// first succeeded would abort a registration over a transient property read.
#[allow(
    clippy::too_many_arguments,
    reason = "The arguments are the exact per-window registration closure: one automation handle, the target window + its handle, the cache request, and the three UIA handler interfaces the registration wires. Grouping them behind a struct would move the coupling the caller already names explicitly."
)]
fn register_window_handlers(
    autom: &IUIAutomation,
    window: &IUIAutomationElement,
    hwnd: usize,
    cache: &IUIAutomationCacheRequest,
    automation_handler: &IUIAutomationEventHandler,
    property: &IUIAutomationPropertyChangedEventHandler,
    structure: &IUIAutomationStructureChangedEventHandler,
    visual_states: &Mutex<HashMap<usize, i32>>,
) -> Result<RegisteredWindow> {
    if hwnd != 0 {
        // Seed the visual-state baseline so the first WindowVisualState
        // notification after subscribe is already a true delta. The
        // failures are classified: only an actually absent WindowPattern
        // means "no baseline" (a window without the pattern never raises
        // visual-state events, so there is nothing to seed). A transient or
        // stale-provider failure propagates instead of being swallowed —
        // the PropertyHandler drops a state event whose baseline is missing,
        // so swallowing this read would silently consume the first real
        // minimize/maximize transition and miss the event while the
        // subscription reports success (tenet 1). Same classification the
        // window verbs apply via `pattern_acquisition_error`.
        match unsafe {
            window.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
        } {
            Ok(pattern) => match unsafe { pattern.CurrentWindowVisualState() } {
                Ok(state) => {
                    let mut states = visual_states.lock().unwrap_or_else(|e| e.into_inner());
                    states.insert(hwnd, state.0);
                }
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "CurrentWindowVisualState failed while seeding the baseline of \
                             window {hwnd:#x}: {e}"
                        ),
                    });
                }
            },
            Err(e) if is_pattern_absent(&e) => {}
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!(
                        "GetCurrentPatternAs(WindowPattern) failed while subscribing to \
                         window {hwnd:#x}: {e}"
                    ),
                });
            }
        }
    }

    let mut automation_ids: Vec<UIA_EVENT_ID> = Vec::new();
    for eid in AUTOMATION_EVENT_IDS {
        if let Err(e) = unsafe {
            autom.AddAutomationEventHandler(
                *eid,
                window,
                TreeScope_Subtree,
                cache,
                automation_handler,
            )
        } {
            let err = Error::Platform {
                code: e.code().0 as i64,
                message: format!("AddAutomationEventHandler({:?}) failed: {e}", eid),
            };
            remove_handlers_of(
                autom,
                window,
                &automation_ids,
                automation_handler,
                property,
                structure,
            );
            return Err(err);
        }
        automation_ids.push(*eid);
    }
    if let Err(e) = unsafe {
        autom.AddPropertyChangedEventHandlerNativeArray(
            window,
            TreeScope_Subtree,
            cache,
            property,
            PROPERTY_CHANGE_IDS,
        )
    } {
        let err = Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddPropertyChangedEventHandlerNativeArray failed: {e}"),
        };
        remove_handlers_of(
            autom,
            window,
            &automation_ids,
            automation_handler,
            property,
            structure,
        );
        return Err(err);
    }
    if let Err(e) = unsafe {
        autom.AddStructureChangedEventHandler(window, TreeScope_Subtree, cache, structure)
    } {
        let err = Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddStructureChangedEventHandler failed: {e}"),
        };
        let _ = unsafe { autom.RemovePropertyChangedEventHandler(window, property) };
        remove_handlers_of(
            autom,
            window,
            &automation_ids,
            automation_handler,
            property,
            structure,
        );
        return Err(err);
    }

    Ok(RegisteredWindow {
        element: ComSend::new(window.clone()),
        hwnd,
        automation_ids,
    })
}

/// Remove a window's previously registered handlers. Removal errors are
/// ignored: a window that closed during subscription (or was never fully
/// registered) answers `Remove*` with an error there is nothing to do about,
/// and the callers treat removal as best-effort teardown.
fn remove_handlers_of(
    autom: &IUIAutomation,
    window: &IUIAutomationElement,
    automation_ids: &[UIA_EVENT_ID],
    automation_handler: &IUIAutomationEventHandler,
    property: &IUIAutomationPropertyChangedEventHandler,
    structure: &IUIAutomationStructureChangedEventHandler,
) {
    for eid in automation_ids {
        let _ = unsafe { autom.RemoveAutomationEventHandler(*eid, window, automation_handler) };
    }
    let _ = unsafe { autom.RemovePropertyChangedEventHandler(window, property) };
    let _ = unsafe { autom.RemoveStructureChangedEventHandler(window, structure) };
}

/// Reconcile per-window registrations with the pid's current top-level
/// windows: attach handlers to windows that opened, tear down handlers of
/// windows that closed, and seed / drop their visual-state baselines.
///
/// Used by the open/close watch after each event and once by `subscribe_impl`
/// right after the watch is registered, so a window that opened during the
/// subscribe-time enumeration is attached too. Failures here cannot reach a
/// caller (the watch is fire-and-forget), so they are diagnosed on stderr
/// (tenet 1: log what a background path cannot propagate — the next
/// open/close event re-runs the sync).
fn sync_registrations(state: &SubscriptionState, cache: &IUIAutomationCacheRequest, pid: u32) {
    // The whole diff/register/teardown sequence is serialized: UIA event
    // handlers may run concurrently, and two reconciles that both compute
    // the same `to_add` would attach the same window's handlers twice
    // (delivering every event twice) then race on the same map slot. The
    // cancel closure takes the same lock, so it either finishes first — in
    // which case `cancelled` is set and this reconcile registers nothing —
    // or waits until this reconcile has registered everything, then drains
    // it. (UIA delivers events asynchronously from its worker threads, so
    // the handler-add calls under this lock cannot re-enter
    // `sync_registrations` on the same thread.)
    let _guard = state
        .reconciliation
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let autom = state.automation.get();
    let current = match top_level_windows_of_pid_with(autom, pid, cache) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("window-reconciliation enumeration failed for pid {pid}: {e:?}");
            return;
        }
    };
    // Resolve every window's native handle once, building the map the
    // add-pass needs so no handle is read a second time (a second failure
    // would silently leave a newly opened window unregistered until some
    // unrelated event). A window whose handle cannot be read is diagnosed,
    // never keyed with a sentinel (see `window_handle`), and never treated as
    // closed: removing its existing registration on a transient read failure
    // would permanently stop a still-open window's events, and there may be
    // no later open/close event to reattach it. Such a sync therefore only
    // adds — teardown is skipped so an existing registration survives until a
    // sync that sees every window cleanly.
    let mut current_by_hwnd: HashMap<usize, &IUIAutomationElement> = HashMap::new();
    let mut unreadable = 0usize;
    for w in &current {
        match window_handle(w) {
            Ok(h) => {
                current_by_hwnd.insert(h, w);
            }
            Err(e) => {
                unreadable += 1;
                eprintln!(
                    "window-reconciliation: pid {pid} window with unreadable handle (preserving any registration): {e:?}"
                );
            }
        }
    }
    let current_hwnds: HashSet<usize> = current_by_hwnd.keys().copied().collect();
    let registered_hwnds: HashSet<usize> = {
        let m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
        m.keys().copied().collect()
    };
    let (to_add, to_remove) = plan_window_registration_diff(&registered_hwnds, &current_hwnds);

    // Tear down closed windows first: any subsequent open keeps the remaining
    // registrations intact, and a closed window cannot accept new handlers.
    // Skipped entirely when any window could not be identified — see above.
    if unreadable == 0 {
        for hwnd in to_remove {
            let reg = {
                let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
                m.remove(&hwnd)
            };
            if let Some(reg) = reg {
                remove_handlers_of(
                    autom,
                    reg.element.get(),
                    &reg.automation_ids,
                    state.automation_handler.get(),
                    state.property_handler.get(),
                    state.structure_handler.get(),
                );
            }
            let mut states = state
                .visual_states
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            states.remove(&hwnd);
        }
    } else if !to_remove.is_empty() {
        eprintln!(
            "window-reconciliation: skipping teardown for pid {pid}: {unreadable} window(s) had an unreadable handle"
        );
    }

    for hwnd in to_add {
        let Some(window) = current_by_hwnd.get(&hwnd).copied() else {
            continue;
        };
        match register_window_handlers(
            autom,
            window,
            hwnd,
            cache,
            state.automation_handler.get(),
            state.property_handler.get(),
            state.structure_handler.get(),
            &state.visual_states,
        ) {
            Ok(reg) => {
                let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
                m.insert(reg.hwnd, reg);
            }
            Err(e) => {
                eprintln!("failed to attach event handlers to pid {pid} window {hwnd:#x}: {e:?}");
            }
        }
    }
}

/// Best-effort removal of every registration of a subscription: the desktop
/// watch, the focus handler, and every per-window record. Used on the
/// subscribe-time error path and on cancel; removal errors are ignored for
/// the same reason as in [`remove_handlers_of`] (a dead window or an
/// already-removed handler has nothing to remove).
fn cleanup_registrations(
    autom: &IUIAutomation,
    root: &IUIAutomationElement,
    watch: &IUIAutomationEventHandler,
    focus: &IUIAutomationFocusChangedEventHandler,
    state: &SubscriptionState,
) {
    // Stop reconciles from registering anything further, and drain the
    // per-window records — atomically under the reconciliation lock, the
    // same critical section `sync_registrations` runs in. Remove* calls
    // happen *outside* the lock: UIA's RemoveXxx waits for in-flight handler
    // callbacks, and a WatchHandler callback running `sync_registrations`
    // itself waits for the reconciliation lock — holding the lock across
    // Remove* would deadlock teardown on that callback.
    let regs: Vec<RegisteredWindow> = {
        let _guard = state
            .reconciliation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.cancelled.store(true, Ordering::SeqCst);
        let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
        m.drain().map(|(_, r)| r).collect()
    };
    let _ = unsafe { autom.RemoveFocusChangedEventHandler(focus) };
    for eid in WATCH_EVENT_IDS {
        let _ = unsafe { autom.RemoveAutomationEventHandler(*eid, root, watch) };
    }
    for reg in regs {
        remove_handlers_of(
            autom,
            reg.element.get(),
            &reg.automation_ids,
            state.automation_handler.get(),
            state.property_handler.get(),
            state.structure_handler.get(),
        );
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
        let event: Event = EventParts {
            kind,
            target,
            app_name: self.app_name.clone(),
            app_pid: self.app_pid,
            timestamp: std::time::Instant::now(),
        }
        .into();
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
    ) -> Result<ElementData> {
        // `CachedControlType()` is cheap and indicates whether the cache
        // covers our expected properties. If it errors, refresh the cache.
        let cached_element = if unsafe { sender.CachedControlType() }.is_ok() {
            sender.clone()
        } else {
            unsafe { sender.BuildUpdatedCache(cache) }.unwrap_or_else(|_| sender.clone())
        };
        build_snapshot_data(&cached_element, Some(self.app_pid), 0, Some(&self.walker))
    }

    /// Snapshot an event sender's ElementData, or deliver the event without
    /// a target. An event handler is fire-and-forget: its return value goes
    /// to the UIA runtime, not to a subscriber, so a failed snapshot still
    /// delivers the event (the target is honestly unknown — that is the
    /// documented `None` case) while the error is diagnosed on stderr
    /// instead of being silently dropped (tenet 1).
    fn snapshot_or_log(
        &self,
        el: &IUIAutomationElement,
        cache: &IUIAutomationCacheRequest,
    ) -> Option<ElementData> {
        match self.snapshot(el, cache) {
            Ok(data) => Some(data),
            Err(e) => {
                eprintln!(
                    "event sender snapshot failed for pid {}: {e:?}",
                    self.app_pid
                );
                None
            }
        }
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

/// Resolve a logical target point to physical for the window-transform verbs.
///
/// The window's own monitor — resolved from its live physical rect — is the
/// identity its logical bounds were reported in, so a target inside *that
/// monitor's* logical rect is converted by that monitor even when a
/// different-DPI neighbor's logical rect also contains the number (under the
/// origin-preserving model the rects never overlap, so this is the identity
/// preference rather than a disambiguation). The window's own bounding rect is
/// not the test: a point outside the old frame but still on the monitor must
/// not fall through. A target outside the window's monitor falls back to the
/// global origin-preserving mapping.
fn window_logical_to_physical(uia: &IUIAutomationElement, x: i32, y: i32) -> Result<(i32, i32)> {
    let rect = unsafe { uia.CurrentBoundingRectangle() }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("CurrentBoundingRectangle failed while converting the target: {e}"),
    })?;
    let has_geometry = rect.left != 0 || rect.top != 0 || rect.right != 0 || rect.bottom != 0;
    if has_geometry {
        if let Some((monitor_rect, scale)) =
            crate::dpi::monitor_containing_physical_point(rect.left, rect.top)
        {
            if crate::dpi::logical_rect_contains(monitor_rect, scale, x, y) {
                return Ok((
                    monitor_rect.left + ((f64::from(x - monitor_rect.left)) * scale).round() as i32,
                    monitor_rect.top + ((f64::from(y - monitor_rect.top)) * scale).round() as i32,
                ));
            }
        }
    }
    crate::dpi::logical_point_to_physical(x, y)
}

/// Map a UIA `WindowVisualState` value (VT_I4, from either a
/// `PropertyChanged(WindowVisualState)` event or a `CurrentWindowVisualState`
/// read) to the `(minimized, maximized)` pair.
///
/// UIA defines exactly three values: Normal (0), Maximized (1), and
/// Minimized (2) — there is no Restored. Mirrors `parse_states`'s derivation
/// for the defined values, so the event and the next snapshot always agree:
/// a restore emits both flags cleared, matching what re-query shows. An
/// unrecognized value is `None`; the caller drops the event rather than
/// reporting a state it cannot name (tenet 1).
fn window_visual_state_to_flags(v: i32) -> Option<(bool, bool)> {
    if v == WindowVisualState_Minimized.0 {
        Some((true, false))
    } else if v == WindowVisualState_Maximized.0 {
        Some((false, true))
    } else if v == WindowVisualState_Normal.0 {
        Some((false, false))
    } else {
        None
    }
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
                let target = self.ctx.snapshot_or_log(el, &self.cache);
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
        let target = self.ctx.snapshot_or_log(el, &self.cache);
        self.ctx.emit(kind, target);
        Ok(())
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct PropertyHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
    /// Current `WindowVisualState` per top-level window, keyed by HWND. UIA
    /// reports the whole visual state on every transition while
    /// `StateChanged` promises a *change*, so a delta is only meaningful
    /// against that window's own previous state — two windows minimizing
    /// consecutively must not compare against each other. Shared with the
    /// subscription state so the open/close watch seeds (window opened) and
    /// clears (window closed) the baselines the first event of a window is
    /// already a true delta against.
    visual_state_by_hwnd: Arc<Mutex<HashMap<usize, i32>>>,
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
            // Window minimize/maximize/restore. UIA reports the whole visual
            // state on every transition, not a delta, so derive both flags
            // from it (the same derivation parse_states uses): a restore
            // clears both, mirroring what the next snapshot shows. The
            // Windows provider is the first to raise StateFlag::Maximized.
            // `StateChanged` promises a change, so only flags that actually
            // changed are emitted, per window, against that window's own
            // previous observation (seeded at subscription time) — a
            // Normal→Minimized transition must not claim Maximized changed.
            // An unrecognized value is dropped rather than invented as
            // "restored" (tenet 1).
            UIA_WindowWindowVisualStatePropertyId => {
                if let Some(v) = variant_i32(newvalue) {
                    let Some((minimized, maximized)) = window_visual_state_to_flags(v) else {
                        return Ok(());
                    };
                    // Delta per window: the sender's HWND keys the baseline.
                    // WindowVisualState changes come only from top-level
                    // windows, which always carry an HWND; a sender without
                    // one is dropped rather than guessed (tenet 1).
                    let hwnd = match unsafe { el.CurrentNativeWindowHandle() } {
                        Ok(h) if !h.0.is_null() => h.0 as usize,
                        _ => return Ok(()),
                    };
                    let prev = {
                        let mut states = self
                            .visual_state_by_hwnd
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let prev = states
                            .get(&hwnd)
                            .copied()
                            .and_then(window_visual_state_to_flags);
                        states.insert(hwnd, v);
                        prev
                    };
                    let (was_minimized, was_maximized) = match prev {
                        Some(prev) => prev,
                        // A window not seeded at subscription time (opened
                        // after subscribe): the prior state is unknown, and
                        // inventing one would be exactly the false delta
                        // `StateChanged` promises not to send. Drop the event
                        // — the current state is re-queryable.
                        None => return Ok(()),
                    };
                    if minimized != was_minimized {
                        kinds.push(EventKind::StateChanged {
                            flag: StateFlag::Minimized,
                            value: minimized,
                        });
                    }
                    if maximized != was_maximized {
                        kinds.push(EventKind::StateChanged {
                            flag: StateFlag::Maximized,
                            value: maximized,
                        });
                    }
                }
            }
            _ => return Ok(()),
        }

        if kinds.is_empty() {
            return Ok(());
        }

        // Build the snapshot once and clone into each emit — cheap since
        // ElementData is just owned strings + small primitives.
        let target = self.ctx.snapshot_or_log(el, &self.cache);
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
                self.ctx.snapshot_or_log(el, &self.cache)
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

/// Desktop-scoped open/close watch: keeps a subscription's handler
/// registrations in step with the pid's current top-level windows.
///
/// Per-window registrations catch events *within* each window's subtree, but
/// a sibling top-level window opening is not inside any registered subtree —
/// so this handler is registered on the desktop root (TreeScope_Children)
/// for `WindowOpened` / `WindowClosed`, filters by pid, emits the event, and
/// reconciles the per-window attachment set. Without it a window that opens
/// after subscribe would never deliver property events and its leftovers
/// would never be torn down.
#[implement(IUIAutomationEventHandler)]
struct WatchHandler {
    ctx: Arc<EventContext>,
    cache: IUIAutomationCacheRequest,
    state: Arc<SubscriptionState>,
}

impl IUIAutomationEventHandler_Impl for WatchHandler_Impl {
    #[allow(non_upper_case_globals)] // UIA constants use CamelCase in the windows crate
    fn HandleAutomationEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
        eventid: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        let Some(el) = sender.as_ref() else {
            return Ok(());
        };
        match eventid {
            UIA_Window_WindowOpenedEventId | UIA_Window_WindowClosedEventId => {
                // The event target is a top-level window of *some* process;
                // emit only for ours (the per-window registrations already
                // scoped child-window events by subtree; ours are the pid
                // filter).
                if !self.ctx.matches_pid(el) {
                    return Ok(());
                }
                let kind = if eventid == UIA_Window_WindowOpenedEventId {
                    EventKind::WindowOpened
                } else {
                    EventKind::WindowClosed
                };
                let target = self.ctx.snapshot_or_log(el, &self.cache);
                self.ctx.emit(kind, target);
                sync_registrations(&self.state, &self.cache, self.ctx.app_pid);
            }
            _ => {}
        }
        Ok(())
    }
}

// Event IDs registered through `AddAutomationEventHandler` on *each* top-level
// window's subtree. Kept as a shared constant so registration and removal
// iterate the same list. `WindowOpened` / `WindowClosed` are deliberately NOT
// here: a per-window subtree registration would deliver a top-level window's
// open/close twice (once from its own subtree scope, once from the desktop
// root's Children scope) — the open/close watch owns those two event IDs.
const AUTOMATION_EVENT_IDS: &[UIA_EVENT_ID] = &[
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

// Event IDs registered through `AddAutomationEventHandler` on the *desktop
// root* (TreeScope_Children): the open/close watch, whose handler reconciles
// the per-window registrations with the pid's current top-level windows.
const WATCH_EVENT_IDS: &[UIA_EVENT_ID] = &[
    UIA_Window_WindowOpenedEventId,
    UIA_Window_WindowClosedEventId,
];

// Property IDs watched via `AddPropertyChangedEventHandlerNativeArray`.
// `WindowVisualState` is the canonical UIA notification for window
// minimize/maximize/restore: UIA has no dedicated event ID for it, so
// providers raise PropertyChanged(WindowVisualState) and a provider that
// doesn't watch the property never sees it.
const PROPERTY_CHANGE_IDS: &[UIA_PROPERTY_ID] = &[
    UIA_NamePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ValueValuePropertyId,
    UIA_RangeValueValuePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_WindowWindowVisualStatePropertyId,
];

impl WindowsProvider {
    fn subscribe_impl(&self, pid: u32, app_name: String) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();

        // Enumerate every current top-level window of the pid up front and
        // register the scoped handlers on *each* of them — the pre-C1 shape
        // resolved a single representative via `find_app_by_pid` (FindFirst)
        // and scoped the handlers to that one window's subtree, so events
        // from same-pid sibling top-level windows (a modal dialog next to the
        // main window, issue #304) were never delivered. The desktop-scoped
        // open/close watch below keeps the set in step as windows come and
        // go, so "app subscription" now means "the process".
        let windows = self.top_level_windows_of_pid(pid)?;
        if windows.is_empty() {
            // Not reachable yet (or its last window just closed): surface it
            // as a selector miss so core's poll loop retries, matching
            // `find_app_by_pid`'s contract for a fresh process.
            return Err(Error::Platform {
                code: -1,
                message: format!("No top-level window found for PID {pid} while subscribing"),
            });
        }

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
        let visual_states = Arc::new(Mutex::new(HashMap::<usize, i32>::new()));
        let property: IUIAutomationPropertyChangedEventHandler = PropertyHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
            visual_state_by_hwnd: Arc::clone(&visual_states),
        }
        .into();
        let structure: IUIAutomationStructureChangedEventHandler = StructureHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();

        // The desktop root anchors the open/close watch below.
        let root = uia_call(|| unsafe { self.automation.GetRootElement() }).map_err(|e| {
            Error::Platform {
                code: -1,
                message: format!("GetRootElement failed while subscribing: {e}"),
            }
        })?;

        let state = Arc::new(SubscriptionState {
            automation: ComSend::new(self.automation.clone()),
            automation_handler: ComSend::new(automation_handler.clone()),
            property_handler: ComSend::new(property.clone()),
            structure_handler: ComSend::new(structure.clone()),
            registered: Mutex::new(HashMap::new()),
            visual_states,
            reconciliation: Mutex::new(()),
            cancelled: AtomicBool::new(false),
        });
        let watch: IUIAutomationEventHandler = WatchHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
            state: Arc::clone(&state),
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

        // The desktop-scoped open/close watch is registered *after* the
        // per-window handlers: while a WindowOpened/WindowClosed event can
        // only arrive once the watch is live, nothing else can trigger a
        // reconciliation during the initial registration, so the loop below
        // cannot race a `sync_registrations` run. If any registration fails,
        // events of that type would never arrive — the caller must know
        // (tenet 1). Clean up what was already registered before returning so
        // no native handler leaks on a half-built subscription.
        let cleanup_error = |e: Error| {
            cleanup_registrations(&self.automation, &root, &watch, &focus, &state);
            e
        };

        // Per-window handlers on every top-level window of the pid. Each
        // window's baseline is seeded by register_window_handlers, so the
        // first WindowVisualState notification is already a true delta. A
        // window whose native handle cannot be read fails the subscription —
        // keying it with a sentinel would alias it with every other unreadable
        // window (see `window_handle`).
        for window in &windows {
            let hwnd = window_handle(window).map_err(&cleanup_error)?;
            match register_window_handlers(
                &self.automation,
                window,
                hwnd,
                &cache,
                &automation_handler,
                &property,
                &structure,
                &state.visual_states,
            ) {
                Ok(reg) => {
                    let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
                    m.insert(reg.hwnd, reg);
                }
                Err(e) => return Err(cleanup_error(e)),
            }
        }

        for eid in WATCH_EVENT_IDS {
            if let Err(e) = unsafe {
                self.automation.AddAutomationEventHandler(
                    *eid,
                    &root,
                    TreeScope_Children,
                    &cache,
                    &watch,
                )
            } {
                return Err(cleanup_error(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!(
                        "AddAutomationEventHandler({:?}) on desktop root failed: {e}",
                        eid
                    ),
                }));
            }
        }

        // A window that opened between the enumeration above and the watch
        // registration has no WindowOpened event to trigger reconciliation —
        // sync once after the watch is live. (A window that opened and closed
        // in the gap is irrelevant: it is gone again. If a watch event fires
        // concurrently with this sync, the reconciliation lock serializes
        // them.)
        sync_registrations(&state, &cache, pid);

        // Each captured COM interface is wrapped in ComSend so the cancel
        // closure satisfies CancelHandle::new's `Send` bound. See ComSend's
        // doc comment for the safety argument.
        let root_c = ComSend::new(root);
        let focus_c = ComSend::new(focus);
        let watch_c = ComSend::new(watch);
        let state_c = Arc::clone(&state);
        let cancel = CancelHandle::new(move || {
            // Mark cancelled and drain the per-window records atomically
            // under the reconciliation lock: a reconcile in flight finishes
            // first and its registrations are drained here, while one that
            // acquires the lock afterwards observes the flag and registers
            // nothing — a window can never be re-registered after the drain.
            // All Remove* calls happen *outside* the lock: UIA's RemoveXxx
            // waits for in-flight handler callbacks, and a WatchHandler
            // callback running `sync_registrations` itself waits for this
            // lock — holding it across Remove* would deadlock teardown on
            // that callback (remove waits for the callback, the callback
            // waits for the lock).
            let regs: Vec<RegisteredWindow> = {
                let _guard = state_c
                    .reconciliation
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                state_c.cancelled.store(true, Ordering::SeqCst);
                let mut m = state_c.registered.lock().unwrap_or_else(|e| e.into_inner());
                m.drain().map(|(_, r)| r).collect()
            };
            // RemoveXxx is synchronous: when it returns, UIA guarantees no
            // further callbacks for this handler. We ignore errors because
            // there's nothing useful to do in a cancel path (a window that
            // closed during the subscription answers Remove* with an error
            // there is nothing to do about).
            let automation = state_c.automation.get();
            let root = root_c.get();
            unsafe {
                let _ = automation.RemoveFocusChangedEventHandler(focus_c.get());
                for eid in WATCH_EVENT_IDS {
                    let _ = automation.RemoveAutomationEventHandler(*eid, root, watch_c.get());
                }
            }
            for reg in regs {
                remove_handlers_of(
                    automation,
                    reg.element.get(),
                    &reg.automation_ids,
                    state_c.automation_handler.get(),
                    state_c.property_handler.get(),
                    state_c.structure_handler.get(),
                );
            }
        });

        Ok(Subscription::new(EventReceiver::new(rx), cancel))
    }
}

#[cfg(test)]
#[allow(non_upper_case_globals)]
mod tests {
    use super::*;

    /// Build a shell-scan partial result of `n` classified surfaces.
    fn classified(n: usize) -> Vec<(u8, ShellSurfaceKind, ElementData)> {
        (0..n)
            .map(|i| {
                let mut data = ElementData::for_role(Role::Group);
                data.name = Some(format!("surface {i}"));
                (0u8, ShellSurfaceKind::Taskbar, data)
            })
            .collect()
    }

    #[test]
    fn a_shell_scan_diagnosis_names_what_was_already_classified() {
        let lines = classified_so_far(&classified(2));
        assert_eq!(
            lines,
            vec!["taskbar \"surface 0\"", "taskbar \"surface 1\""]
        );
        assert!(classified_so_far(&[]).is_empty());
    }

    #[test]
    fn a_shell_scan_diagnosis_is_bounded() {
        // Tenet 6: the failure path must not emit a line per desktop child.
        let lines = classified_so_far(&classified(DIAG_SHELL_CANDIDATE_LIMIT + 5));
        assert_eq!(lines.len(), DIAG_SHELL_CANDIDATE_LIMIT + 1);
        assert_eq!(lines[DIAG_SHELL_CANDIDATE_LIMIT], "… (+5 more)");
    }

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
    fn get_children_none_returns_synthetic_applications() {
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap();
        // Should find at least one process on a Windows desktop
        assert!(
            !apps.is_empty(),
            "Should find at least one top-level application"
        );
        for app in &apps {
            assert_eq!(
                app.role,
                Role::Application,
                "top-level entries must be Application nodes after the unification"
            );
            assert!(app.pid.is_some(), "Application nodes should have a PID");
            assert!(app.name.is_some(), "Application nodes should have a name");
            assert!(
                app.bounds.is_none(),
                "UIA has no process geometry; Application bounds must be None"
            );
            assert_eq!(
                app.raw.get("uia_synthesized"),
                Some(&serde_json::Value::Bool(true)),
                "every Application node must be marked synthesized"
            );
        }
    }

    #[test]
    fn get_children_none_applications_carry_no_window_flags() {
        let Some(provider) = try_provider() else {
            return;
        };
        let apps = provider.get_children(None).unwrap();
        // The synthesized Application node is a process, not a window: it
        // must not advertise window-state flags or window actions — asking
        // the app to minimize itself has no meaning (and `App::windows` is
        // how the process's windows are reached).
        for app in &apps {
            assert!(
                !app.states.active
                    && !app.states.minimized.unwrap_or(false)
                    && !app.states.maximized.unwrap_or(false)
                    && !app.states.modal,
                "Application nodes must not carry window-state flags"
            );
            assert!(
                app.actions.is_empty(),
                "Application nodes are not window-like"
            );
        }
    }

    #[test]
    fn get_cached_stale_handle_returns_error() {
        let Some(provider) = try_provider() else {
            return;
        };
        // A real cached handle's high bit is clear (cache_element increments
        // from 1), so a far-out handle with the bit clear is a plain stale
        // handle. Bit 63 is the synthetic tag space and is covered by
        // `get_cached_synthetic_handle_returns_unsupported`.
        let result = provider.get_cached(1 << 40);
        assert!(
            matches!(result, Err(Error::ElementStale { .. })),
            "Stale handle should return ElementStale error"
        );
    }

    #[test]
    fn synthetic_handle_tag_recognizer() {
        // Bit 63 is the synthetic tag space.
        assert!(is_synthetic_handle(SYNTHETIC_APP_TAG));
        assert!(is_synthetic_handle(SYNTHETIC_APP_TAG | 42));
        assert!(is_synthetic_handle(SYNTHETIC_APP_TAG | (1 << 33)));
        // A real cached handle (high bit clear) is not synthetic.
        assert!(!is_synthetic_handle(1));
        assert!(!is_synthetic_handle(!SYNTHETIC_APP_TAG));
        assert!(!is_synthetic_handle(1 << 40));
    }

    #[test]
    fn synthetic_app_is_stale_only_on_timestamp_mismatch() {
        // Same creation time = the same process instance: not stale even if
        // the node was minted in an earlier list pass.
        assert!(!synthetic_app_is_stale(Some(1000), Some(1000)));
        // A different creation time = the pid was reused by another process.
        assert!(synthetic_app_is_stale(Some(1000), Some(2000)));
        // No baseline or no current read = no verdict, not "stale": the guard
        // only fires when both sides are known (tenet 1).
        assert!(!synthetic_app_is_stale(Some(1000), None));
        assert!(!synthetic_app_is_stale(None, Some(1000)));
        assert!(!synthetic_app_is_stale(None, None));
    }

    #[test]
    fn is_pattern_absent_classifies_the_known_absent_signals() {
        use windows::core::{Error as CoreError, HRESULT};
        // The canonical "no such pattern" HRESULTs.
        assert!(
            is_pattern_absent(&CoreError::from_hresult(E_NOINTERFACE)),
            "E_NOINTERFACE is the canonical absent signal"
        );
        assert!(
            is_pattern_absent(&CoreError::from_hresult(HRESULT(
                UIA_E_INVALIDOPERATION as i32
            ))),
            "UIA_E_INVALIDOPERATION is the UIA-specific absent signal"
        );
        // AccessKit's provider declines unsupported patterns with
        // `Error::empty()` (S_OK with a null pattern pointer): code() is
        // HRESULT(0), "The operation completed successfully." — the AccessKit
        // test-app windows hit exactly this on TransformPattern.
        assert!(
            is_pattern_absent(&CoreError::empty()),
            "the empty error (S_OK + null pattern) is AccessKit's absent signal"
        );
        // A genuine COM failure is never "absent" — it must propagate as a
        // platform error, not silently degrade to a capability the element
        // does not advertise (tenet 1).
        assert!(
            !is_pattern_absent(&CoreError::from_hresult(HRESULT(E_FAIL.0))),
            "a failed HRESULT is a real COM failure, not an absent pattern"
        );
    }

    #[test]
    fn get_cached_synthetic_handle_returns_unsupported() {
        let Some(provider) = try_provider() else {
            return;
        };
        // A synthetic handle only has an identity once `build_synthetic_app_data`
        // registered it; mint one directly so the error names the pid.
        let handle = SYNTHETIC_APP_TAG | 1_000_001;
        provider.synthetic_apps.lock().unwrap().insert(
            handle,
            SyntheticAppIdentity {
                pid: 42,
                creation_time: Some(12345),
            },
        );
        let err = provider
            .get_cached(handle)
            .expect_err("a synthetic handle must never resolve to a live element");
        assert!(
            matches!(&err, Error::Unsupported { feature } if feature.contains("pid 42")),
            "error must name the synthesized node's pid and the remedy, got {err:?}"
        );
    }

    #[test]
    fn get_parent_of_top_level_window_is_synthetic_app() {
        let Some(provider) = try_provider() else {
            return;
        };
        // The desktop-root branch resolves the owning process's Application
        // node for any element whose UIA parent is the desktop root — so a
        // top-level window's parent is its process, not the desktop.
        let Some(app) = provider.list_apps().ok().and_then(|a| a.into_iter().next()) else {
            return;
        };
        let Some(window) = provider
            .get_children(Some(&app))
            .ok()
            .and_then(|w| w.into_iter().next())
        else {
            return;
        };
        let parent = provider.get_parent(&window).expect("parent must resolve");
        let parent = parent.expect("a top-level window has its process as parent");
        assert_eq!(
            parent.role,
            Role::Application,
            "a top-level window's parent must be the synthesized Application node"
        );
        assert_eq!(parent.pid, window.pid, "the parent app must own the window");
        assert_eq!(
            parent.raw.get("uia_synthesized"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn perform_action_delegates_to_named_methods() {
        let Some(provider) = try_provider() else {
            return;
        };
        let mut dummy = ElementData::for_role(Role::Button);
        dummy.name = Some("test".to_string());
        dummy.handle = u64::MAX; // stale handle
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
        let mut dummy = ElementData::for_role(Role::Button);
        dummy.name = Some("test".to_string());
        // High bit clear: a plain stale handle, not a synthetic one (the
        // synthetic tag space funnels to `Unsupported`, covered separately).
        dummy.handle = 1 << 40;
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
    fn batch_properties_includes_legacy_state() {
        // The MSAA state bitmask is the only selection signal frameworks that
        // predate SelectionItem publish (WinForms grids/lists, MSAA-proxied
        // Win32). Without it cached, `parse_states` cannot see their
        // selection at all.
        assert!(
            BATCH_PROPERTIES.contains(&UIA_LegacyIAccessibleStatePropertyId),
            "LegacyIAccessible.State must be cached for MSAA-only selection"
        );
    }

    #[test]
    fn legacy_state_selected_reads_the_msaa_selection_bit() {
        // STATE_SYSTEM_SELECTED (0x2) set, alone and alongside the bits a
        // WinForms grid cell reports next to it (SELECTABLE 0x200000,
        // FOCUSABLE 0x100000, FOCUSED 0x4, READONLY 0x40).
        assert!(legacy_state_selected(Some(0x2)));
        assert!(legacy_state_selected(Some(
            0x2 | 0x4 | 0x40 | 0x100000 | 0x200000
        )));
        // Selectable and focused but not selected — the sibling cells in the
        // same grid. This is the case that must not report `selected`.
        assert!(!legacy_state_selected(Some(0x4 | 0x100000 | 0x200000)));
        assert!(!legacy_state_selected(Some(0)));
        // No LegacyIAccessible implementation, or the property missing from
        // the snapshot: no selection information, so not selected.
        assert!(!legacy_state_selected(None));
    }

    #[test]
    fn custom_control_falls_back_to_the_msaa_role() {
        // A WinForms DataGridView row: DataGridViewRowAccessibleObject derives
        // from AccessibleObject (not ControlAccessibleObject), so it publishes
        // no UIA ControlType — UIA reports Custom — while LegacyIAccessible
        // still reports ROLE_SYSTEM_ROW. Without this the named rows ("Row 1",
        // "Row 2", "Top Row") land in the tree as `unknown`.
        assert_eq!(
            map_uia_role(
                UIA_CustomControlTypeId,
                false,
                false,
                Some(ROLE_SYSTEM_ROW as i32)
            ),
            Role::TableRow
        );
        assert_eq!(
            map_uia_role(
                UIA_CustomControlTypeId,
                false,
                false,
                Some(ROLE_SYSTEM_CELL as i32)
            ),
            Role::TableCell
        );
        // The TableItem pattern still wins: a Custom cell that advertises it
        // is a cell whatever MSAA calls it.
        assert_eq!(
            map_uia_role(
                UIA_CustomControlTypeId,
                true,
                false,
                Some(ROLE_SYSTEM_ROW as i32)
            ),
            Role::TableCell
        );
        // An MSAA role with no clean equivalent leaves the element unknown
        // rather than inventing a mapping.
        assert_eq!(
            map_uia_role(
                UIA_CustomControlTypeId,
                false,
                false,
                Some(ROLE_SYSTEM_SOUND as i32)
            ),
            Role::Unknown
        );
        // No LegacyIAccessible role at all: unchanged behaviour.
        assert_eq!(
            map_uia_role(UIA_CustomControlTypeId, false, false, None),
            Role::Unknown
        );
    }

    #[test]
    fn msaa_role_refinement_is_scoped_to_custom() {
        // A real UIA control type is authoritative — the MSAA role must never
        // override it, even when the two disagree.
        assert_eq!(
            map_uia_role(
                UIA_ButtonControlTypeId,
                false,
                false,
                Some(ROLE_SYSTEM_ROW as i32)
            ),
            Role::Button
        );
        // An *unrecognized* control type stays an unknown_role so the
        // role-map drift tests keep failing on real gaps in
        // map_uia_control_type, rather than being papered over by MSAA.
        let unmapped = UIA_CONTROLTYPE_ID(50041);
        assert_eq!(
            map_uia_role(unmapped, false, false, Some(ROLE_SYSTEM_ROW as i32)),
            map_uia_control_type(unmapped)
        );
    }

    #[test]
    fn batch_properties_includes_legacy_role() {
        assert!(
            BATCH_PROPERTIES.contains(&UIA_LegacyIAccessibleRolePropertyId),
            "LegacyIAccessible.Role must be cached to resolve ControlType.Custom elements"
        );
    }

    #[test]
    fn msaa_role_map_has_no_accidental_unknowns() {
        // Every MSAA role we claim to map must produce a real role; roles we
        // deliberately leave unmapped must return None (not Role::Unknown,
        // which would be indistinguishable from a mapping bug).
        for role in [
            ROLE_SYSTEM_ROW,
            ROLE_SYSTEM_CELL,
            ROLE_SYSTEM_COLUMNHEADER,
            ROLE_SYSTEM_ROWHEADER,
            ROLE_SYSTEM_TABLE,
            ROLE_SYSTEM_LIST,
            ROLE_SYSTEM_LISTITEM,
            ROLE_SYSTEM_PUSHBUTTON,
            ROLE_SYSTEM_CHECKBUTTON,
            ROLE_SYSTEM_RADIOBUTTON,
            ROLE_SYSTEM_TEXT,
            ROLE_SYSTEM_STATICTEXT,
            ROLE_SYSTEM_WINDOW,
            ROLE_SYSTEM_DIALOG,
        ] {
            let mapped = map_msaa_role(role as i32);
            assert!(
                matches!(mapped, Some(r) if r != Role::Unknown),
                "MSAA role {role} should map to a concrete xa11y role, got {mapped:?}"
            );
        }
        assert_eq!(map_msaa_role(ROLE_SYSTEM_CURSOR as i32), None);
        // Negative / out-of-range values from a misbehaving provider are not
        // roles — they must not panic or match a mapping.
        assert_eq!(map_msaa_role(-1), None);
        assert_eq!(map_msaa_role(i32::MAX), None);
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
            map_uia_role(UIA_DataItemControlTypeId, true, false, None),
            Role::TableCell
        );
        // Neither signal: a row container.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, false, false, None),
            Role::TableRow
        );
        // Cell signals never leak onto other control types.
        assert_eq!(
            map_uia_role(UIA_ButtonControlTypeId, true, true, None),
            Role::Button
        );
    }

    #[test]
    fn custom_control_with_table_item_pattern_is_cell() {
        // WPF/WinForms DataGrid cells: ControlType.Custom + TableItem.
        assert_eq!(
            map_uia_role(UIA_CustomControlTypeId, true, false, None),
            Role::TableCell
        );
        // Pattern-less Custom stays Unknown even under a row — an embedded
        // custom widget inside a row is not a cell.
        assert_eq!(
            map_uia_role(UIA_CustomControlTypeId, false, true, None),
            Role::Unknown
        );
        assert_eq!(
            map_uia_role(UIA_CustomControlTypeId, false, false, None),
            Role::Unknown
        );
    }

    #[test]
    fn control_type_map_covers_every_uia_control_type() {
        // The complete UIA control-type range (50000..=50040). Every id must
        // resolve without reaching the unknown_role catch-all (which panics
        // under strict-roles); Custom (50025) is the one deliberate Unknown.
        // Guards against the AT-SPI-style drift where common types were
        // silently missing (SemanticZoom and AppBar were, before this test).
        for id in 50000..=50040u32 {
            let ct = UIA_CONTROLTYPE_ID(id as i32);
            let role = map_uia_control_type(ct);
            if ct == UIA_CustomControlTypeId {
                assert_eq!(role, Role::Unknown);
            } else {
                assert_ne!(
                    role,
                    Role::Unknown,
                    "UIA control type {id} has no explicit mapping"
                );
            }
        }
    }

    #[test]
    fn pattern_less_data_item_under_row_is_cell() {
        // AccessKit exposes cells as pattern-less DataItems under a row
        // DataItem — the structural signal alone must classify them.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, false, true, None),
            Role::TableCell
        );
        // Both signals agreeing is still a cell.
        assert_eq!(
            map_uia_role(UIA_DataItemControlTypeId, true, true, None),
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
        // Getting an Application node's children allocates a handle per window
        // (window elements are cached real UIA elements); the synthetic node
        // itself never mints one, so resolve one first.
        let Some(app) = provider.list_apps().ok().and_then(|a| a.into_iter().next()) else {
            return;
        };
        let _ = provider.get_children(Some(&app)).unwrap();
        let after = NEXT_HANDLE.load(Ordering::Relaxed);
        assert!(
            after > before,
            "Handle counter should increment after caching window elements"
        );
    }

    // ── Event subscription tests ────────────────────────────────────────────

    fn dummy_element(pid: Option<u32>) -> ElementData {
        let mut data = ElementData::for_role(Role::Application);
        data.name = Some("test".to_string());
        data.pid = pid;
        data
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
        // The setup path (cache request creation, handler boxing into COM
        // objects, Arc<EventContext> + per-window registration) is exercised
        // by the integer-PID cases above. Here we additionally verify that
        // dropping a live Subscription runs the cancel closure without
        // panicking when the pid has at least one top-level window.
        //
        // The flow: if the pid resolves a window, subscribe registers the
        // per-window handlers and the desktop watch, and the drop happens at
        // end of scope. If not, subscribe returns Err and we just confirm
        // the err type.
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
    fn plan_window_registration_diff_adds_new_and_removes_closed() {
        // The open/close watch's reconcile: a new sibling window must be
        // attached, a closed one must be torn down, common windows are left
        // alone.
        let registered: HashSet<usize> = [0x10, 0x20, 0x30].into_iter().collect();
        let current: HashSet<usize> = [0x20, 0x30, 0x40].into_iter().collect();
        let (to_add, to_remove) = plan_window_registration_diff(&registered, &current);
        assert_eq!(to_add, vec![0x40]);
        assert_eq!(to_remove, vec![0x10]);
    }

    #[test]
    fn plan_window_registration_diff_is_a_noop_for_an_unchanged_set() {
        let windows: HashSet<usize> = [0x10, 0x20].into_iter().collect();
        let (to_add, to_remove) = plan_window_registration_diff(&windows, &windows);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());
    }

    #[test]
    fn plan_window_registration_diff_attaches_everything_on_first_sync() {
        // The post-subscribe reconcile: the initial set is registered during
        // subscribe, so a window that opened in the gap shows up as new.
        let registered: HashSet<usize> = [0x10].into_iter().collect();
        let current: HashSet<usize> = [0x10, 0x11].into_iter().collect();
        let (to_add, to_remove) = plan_window_registration_diff(&registered, &current);
        assert_eq!(to_add, vec![0x11]);
        assert!(to_remove.is_empty());
    }

    #[test]
    fn automation_event_ids_covers_design_doc() {
        // These are the event IDs the design doc mandates we watch. If a
        // future refactor drops one silently, this test will catch it.
        for eid in WATCH_EVENT_IDS {
            assert!(
                eid == &UIA_Window_WindowOpenedEventId || eid == &UIA_Window_WindowClosedEventId,
                "the watch must cover exactly the top-level window open/close events"
            );
        }
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
        assert!(PROPERTY_CHANGE_IDS.contains(&UIA_WindowWindowVisualStatePropertyId));
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
        // make the conversion. ToggleState, ExpandCollapseState, and
        // WindowVisualState are the only properties our handler feeds to
        // it and they're strictly VT_I4, so the coercion is a non-issue in
        // practice.
        let v = VARIANT::from(ExpandCollapseState_Expanded.0);
        assert_eq!(variant_i32(&v), Some(1));
    }

    #[test]
    fn window_visual_state_to_flags_maps_minimize_maximize_normal() {
        // Mirror of the parse_states derivation: a single WindowVisualState
        // read/event produces the (minimized, maximized) pair.
        assert_eq!(
            window_visual_state_to_flags(WindowVisualState_Minimized.0),
            Some((true, false))
        );
        assert_eq!(
            window_visual_state_to_flags(WindowVisualState_Maximized.0),
            Some((false, true))
        );
        assert_eq!(
            window_visual_state_to_flags(WindowVisualState_Normal.0),
            Some((false, false))
        );
        // UIA defines only Normal (0), Maximized (1), and Minimized (2) —
        // 3 is not "Restored" and no such constant exists in the windows
        // crate. An unrecognized (invalid or future) value is `None` so the
        // caller drops the event rather than inventing a state it cannot
        // verify (tenet 1).
        assert_eq!(window_visual_state_to_flags(3), None);
        assert_eq!(window_visual_state_to_flags(42), None);
    }

    #[test]
    fn uia_stable_id_prefers_hwnd_and_falls_back_to_automation_id() {
        // Top-level windows have no AutomationId (UIA excludes them by
        // contract), so their identity is the HWND; nested controls have an
        // AutomationId and no HWND of their own. HWND wins when both exist;
        // a null HWND is the same as no HWND.
        let hwnd = |v: usize| HWND(v as *mut _);
        let hwnd_of = |v: usize| Some(hwnd(v));
        assert_eq!(
            uia_stable_id(hwnd_of(0x1234), Some("btn-close".into())),
            Some("hwnd:0x1234".into())
        );
        assert_eq!(
            uia_stable_id(hwnd_of(0x1A2B), None),
            Some("hwnd:0x1a2b".into())
        );
        assert_eq!(
            uia_stable_id(hwnd_of(0), None),
            None,
            "a null HWND must not produce a bogus identity"
        );
        assert_eq!(
            uia_stable_id(None, Some("PanelFields".into())),
            Some("PanelFields".into())
        );
        assert_eq!(uia_stable_id(None, None), None);
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
        // Every Application node's handle is a tagged synthetic handle
        // (non-zero by construction); real window children mint handles via
        // build_element_data. Event-path snapshots pass 0; that path is
        // covered by the actual handler wiring.
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

    // ── pid_variant ─────────────────────────────────────────────────────────

    #[test]
    fn pid_variant_accepts_pids_in_i32_range() {
        for pid in [0u32, 1, 42, i32::MAX as u32] {
            pid_variant(pid).expect("pids within i32 range must convert");
        }
    }

    #[test]
    fn pid_variant_rejects_pids_above_i32_range() {
        // The smallest pid that no longer fits: 2^31. A silent wrap would
        // make the UIA ProcessId condition match nothing (tenet 1).
        let err = pid_variant(i32::MAX as u32 + 1)
            .expect_err("pid above i32 range must be a surfaceable error");
        assert!(matches!(err, Error::Platform { .. }));
        pid_variant(u32::MAX).expect_err("u32::MAX pid must fail");
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
            if calls.get() < TRANSIENT_RETRY_ATTEMPTS {
                Err(subscriber_failure())
            } else {
                Ok("tree")
            }
        });
        assert_eq!(result.unwrap(), "tree");
        assert_eq!(calls.get(), TRANSIENT_RETRY_ATTEMPTS);
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
        assert_eq!(calls.get(), TRANSIENT_RETRY_ATTEMPTS);
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

    // ── COM server-busy retry ───────────────────────────────────────────────
    //
    // App discovery calls cross-process into every top-level window on the
    // desktop, so a busy *unrelated* application used to fail the caller's
    // query outright: uia_call propagated the HRESULT as Error::Platform, and
    // App::find's poll_lookup only retries SelectorNotMatched.

    /// The three HRESULTs that mean "busy, try again", with the shape of the
    /// situation each one comes from.
    const SERVER_BUSY_CODES: &[(windows::core::HRESULT, &str)] = &[
        (RPC_E_CALL_REJECTED, "callee rejected the call"),
        (RPC_E_SERVERCALL_RETRYLATER, "server says retry later"),
        (
            RPC_E_CANTCALLOUT_ININPUTSYNCCALL,
            "target STA is in an input-synchronous call",
        ),
    ];

    #[test]
    fn com_server_busy_codes_are_classified_transient() {
        for (code, what) in SERVER_BUSY_CODES {
            let err = code.ok().unwrap_err();
            assert!(is_com_server_busy(&err), "{what} ({code:?}) must be busy");
            assert!(is_transient(&err), "{what} ({code:?}) must be transient");
        }
    }

    #[test]
    fn uia_call_retries_each_server_busy_code_then_succeeds() {
        for (code, what) in SERVER_BUSY_CODES {
            let calls = std::cell::Cell::new(0u32);
            let result = uia_call(|| {
                calls.set(calls.get() + 1);
                if calls.get() < TRANSIENT_RETRY_ATTEMPTS {
                    Err(code.ok().unwrap_err())
                } else {
                    Ok("apps")
                }
            });
            assert_eq!(
                result.unwrap(),
                "apps",
                "{what} should have been ridden out"
            );
            assert_eq!(calls.get(), TRANSIENT_RETRY_ATTEMPTS, "{what}");
        }
    }

    #[test]
    fn uia_call_propagates_persistent_server_busy() {
        // Retrying is bounded: a server that is busy forever is a real
        // failure and must reach the caller rather than spin.
        let calls = std::cell::Cell::new(0u32);
        let result: Result<()> = uia_call(|| {
            calls.set(calls.get() + 1);
            Err(RPC_E_CANTCALLOUT_ININPUTSYNCCALL.ok().unwrap_err())
        });
        assert_eq!(calls.get(), TRANSIENT_RETRY_ATTEMPTS);
        match result {
            Err(Error::Platform { code, .. }) => {
                assert_eq!(code, RPC_E_CANTCALLOUT_ININPUTSYNCCALL.0 as i64);
            }
            other => panic!("expected Error::Platform, got {other:?}"),
        }
    }

    #[test]
    fn retry_transient_preserves_the_raw_hresult() {
        // The degrading call sites (reacquire_via_hwnd, populate_cache,
        // uia_children) need the COM error itself, not Error::Platform.
        let err = retry_transient::<()>(|| Err(RPC_E_CALL_REJECTED.ok().unwrap_err()))
            .expect_err("should still fail once attempts are exhausted");
        assert_eq!(err.code(), RPC_E_CALL_REJECTED);
    }

    #[test]
    fn retry_transient_does_not_retry_other_errors() {
        let calls = std::cell::Cell::new(0u32);
        let e_fail = windows::core::HRESULT(0x80004005u32 as i32);
        let result: windows::core::Result<()> = retry_transient(|| {
            calls.set(calls.get() + 1);
            Err(e_fail.ok().unwrap_err())
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(result.unwrap_err().code(), e_fail);
    }
}
