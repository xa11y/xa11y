//! Windows UI Automation accessibility provider.

mod rich;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use xa11y_core::handle_cache::{BoundedHandleCache, MAX_CACHED_ELEMENTS};

use windows::core::{implement, Interface, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetDesktopWindow, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible,
    SendMessageTimeoutW, SetForegroundWindow, GA_PARENT, GWL_EXSTYLE, GWL_STYLE, GW_OWNER,
    SMTO_ABORTIFHUNG, STATE_SYSTEM_SELECTED, WM_NULL, WS_EX_LAYERED, WS_EX_TRANSPARENT, WS_POPUP,
};

use xa11y_core::{
    selector::{matches_simple, Combinator, Selector, SelectorSegment},
    CancelHandle, ElementData, ElementParts, Error, Event, EventKind, EventParts, HiddenReason,
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

/// Upper bounds for provider-owned COM references and synthetic identities.
///
/// Handles are opaque, session-local capabilities. Keeping the newest 65K
/// real elements preserves several large tree snapshots for normal action
/// flows while preventing repeated enumeration from retaining every UIA COM
/// object for the lifetime of the process. Synthetic apps are much smaller,
/// but process churn is unbounded too, so they have their own cap.
const MAX_SYNTHETIC_APPS: usize = 4_096;

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
    /// the process could not be opened at synthesis time. Such a node may be
    /// listed, but any later operation that would resolve its pid fails stale:
    /// an unverifiable pid must never target a different process after reuse.
    creation_time: Option<u64>,
}

/// A saved synthetic node together with the handle used in stale diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SavedSyntheticAppIdentity {
    handle: u64,
    identity: SyntheticAppIdentity,
}

/// Return a stable handle for a process generation whose identity could be
/// verified, or mint a fresh fail-closed handle when it could not.
fn cache_synthetic_app_identity(
    synthetic_apps: &mut BoundedHandleCache<SyntheticAppIdentity>,
    identity: SyntheticAppIdentity,
) -> u64 {
    // Repeated App::list calls must not grow the table for a live process.
    // An unverifiable identity is deliberately never deduplicated by pid:
    // doing so could make a new process inherit an old process's handle.
    let existing = identity.creation_time.and_then(|_| {
        synthetic_apps
            .iter()
            .find_map(|(handle, candidate)| (*candidate == identity).then_some(*handle))
    });
    let handle =
        existing.unwrap_or_else(|| SYNTHETIC_APP_TAG | NEXT_HANDLE.fetch_add(1, Ordering::Relaxed));
    synthetic_apps.insert(handle, identity);
    handle
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
///
/// # Why `RPC_E_CHANGED_MODE` is accepted (issue #499)
///
/// That HRESULT means the calling thread is already a single-threaded
/// apartment: a Python host that imported comtypes / pywinauto first, or
/// Electron's main thread. The `CUIAutomation8` object is then created in
/// that STA. This is safe for two reasons:
///
/// - `CUIAutomation8` is registered `ThreadingModel=Both`, so it is created
///   directly in the caller's apartment with no proxy. UIA's client objects
///   (the automation object, elements, cache requests, walkers) aggregate
///   the free-threaded marshaler, so another thread calls them directly
///   instead of marshalling the call back into the STA. That STA never has
///   to pump messages for xa11y's calls to complete.
/// - Event handlers, the one part of UIA that does call back into an
///   apartment, are registered from [`registration_worker`], a thread of
///   xa11y's own that enters the MTA itself, never from the host's thread.
///
/// `xa11y-windows/tests/sta_host.rs` is the regression guard: it creates the
/// provider on an STA thread that never pumps, then reads the tree from four
/// other threads.
fn ensure_com_initialized() -> windows::core::Result<()> {
    // Use MTA (0x0) — same mode as the Rust runtime default.
    // STA (0x2) would conflict with Rust's thread pool.
    let hr = unsafe { CoInitializeEx(None, COINIT(0x0)) };
    // S_OK, S_FALSE (already initialized), or RPC_E_CHANGED_MODE (an STA
    // host thread; safe, see above) are all fine.
    if hr.is_err() && hr.0 as u32 != 0x80010106 {
        hr.ok()?;
    }
    Ok(())
}

/// Serializes UIA creation and its first call across threads.
///
/// Creating `CUIAutomation8` and making the first `GetRootElement` call on
/// several threads at once can fail with `E_FAIL` (issue #500): UIA's
/// first-touch initialization in the client process is not safe to race.
/// Holding this lock across COM init, creation, and one warm-up call makes
/// every later provider find UIA already initialized. It guards no data, so
/// a poisoned lock is recovered.
static UIA_FIRST_TOUCH: Mutex<()> = Mutex::new(());
/// Set once a provider has completed the first touch, after which creation
/// no longer needs [`UIA_FIRST_TOUCH`].
static UIA_WARMED: AtomicBool = AtomicBool::new(false);

/// The desktop's top-level windows with their app pids, and notes for the
/// UWP frames that belong to no app (see `WindowsProvider::desktop_app_windows`).
type DesktopAppWindows = (Vec<(IUIAutomationElement, u32)>, Vec<String>);

/// Keep the process MTA alive for xa11y's lifetime (`CoIncrementMTAUsage`,
/// once per process).
///
/// Provider methods run on whatever thread the caller uses, and most of
/// those threads never initialized COM. UIA's client objects are agile, so
/// they work there anyway, but the shell calls UWP attribution makes
/// (`SHGetPropertyStoreForWindow`, `SHCreateItemInKnownFolder`) need COM on
/// the calling thread, and fail with `CO_E_NOTINITIALIZED` without it. With
/// the MTA usage held, every thread that has not chosen an apartment is
/// implicitly in the MTA (issue #497; an STA host thread keeps its STA, which
/// those calls also accept). The cookie is never released: the usage lasts
/// as long as the process.
fn ensure_mta_usage() -> Result<()> {
    // Set only on success, so a failed attempt is retried by the next
    // provider (#533). Two threads racing here both increment; a second
    // usage count is harmless.
    static HELD: AtomicBool = AtomicBool::new(false);
    if HELD.load(Ordering::Acquire) {
        return Ok(());
    }
    unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("CoIncrementMTAUsage failed: {e}"),
    })?;
    HELD.store(true, Ordering::Release);
    Ok(())
}

/// Windows accessibility provider using UI Automation.
pub struct WindowsProvider {
    automation: IUIAutomation,
    /// Describes which properties and patterns to pre-fetch in bulk queries.
    /// Not a cache — each FindAllBuildCache call takes a fresh snapshot.
    batch_request: crate::coords::BoundsCacheRequest,
    /// Bounds and scroll overflow only: the cheap refresh behind the scroll
    /// clipping in [`Self::children_clip_of`] (issue #502).
    scroll_request: crate::coords::BoundsCacheRequest,
    /// Raw-view tree walker, created once. Snapshot builds use it to probe a
    /// pattern-less `DataItem`'s parent (the cell-vs-row disambiguation in
    /// `map_uia_role`); raw view matches `batch_request`'s TrueCondition so
    /// the probe sees the same tree the traversal does.
    raw_walker: IUIAutomationTreeWalker,
    /// UIA elements retained for action dispatch (keyed by handle ID).
    handle_cache: Mutex<BoundedHandleCache<IUIAutomationElement>>,
    /// The nearest native window (as a raw HWND value) of each cached
    /// element: its own HWND, or the one its parent resolved to. Lets
    /// `get_children` decide "is this in a minimized window?" for a level
    /// with one lookup instead of a raw-view walk (issue #501). A miss (the
    /// element was not built through a parent here) falls back to the walk.
    host_hwnds: Mutex<BoundedHandleCache<usize>>,
    /// The scroll clip each cached element was built under, with the bounds
    /// it had then (issue #502). Lets `get_children` clip a level with one
    /// refresh of the parent instead of a raw-view walk; the walk runs on a
    /// miss, or when the parent has moved since, which means an ancestor's
    /// viewport may have too.
    clips: Mutex<BoundedHandleCache<ClipRecord>>,
    /// Identities of the synthesized Application nodes this provider minted,
    /// keyed by their tagged handle. `get_children(Some(app))` validates the
    /// identity (creation time) before enumerating, so a stale `App` whose
    /// process exited and whose PID was reused by another process surfaces an
    /// error rather than silently retargeting to the new process.
    synthetic_apps: Mutex<BoundedHandleCache<SyntheticAppIdentity>>,
}

// UIA's client objects (IUIAutomation, elements, cache requests, walkers)
// are agile: they aggregate the free-threaded marshaler, so any thread may
// call them directly whatever apartment created them, with no proxy and no
// message pump needed (see `ensure_com_initialized`, issue #499;
// tests/sta_host.rs checks it).
unsafe impl Send for WindowsProvider {}
unsafe impl Sync for WindowsProvider {}

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        // Establish Per-Monitor-V2 DPI awareness before the first bounds read
        // so UIA reports coordinates in a stable (physical) space that we can
        // convert to logical. Shared once-only init with the screenshot
        // backend — see `crate::dpi` and issue #300.
        crate::dpi::ensure_process_dpi_aware();
        // COM initialization is per thread and not part of the race.
        ensure_com_initialized().map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("COM initialization failed: {}", e),
        })?;
        ensure_mta_usage()?;
        let automation: IUIAutomation = {
            let _first_touch = if UIA_WARMED.load(Ordering::Acquire) {
                None
            } else {
                Some(UIA_FIRST_TOUCH.lock().unwrap_or_else(|e| e.into_inner()))
            };
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
            // Before the warm-up, so the first cross-process call is
            // already bounded.
            set_uia_timeouts(&automation)?;
            // The warm-up call is part of the first touch: it is what races
            // when made concurrently, so it runs under the lock too. Its
            // failure is the provider's failure.
            unsafe { automation.GetRootElement() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("IUIAutomation.GetRootElement warm-up failed: {e}"),
            })?;
            UIA_WARMED.store(true, Ordering::Release);
            automation
        };
        let batch_request = create_batch_request(&automation)?;
        let scroll_request = create_scroll_request(&automation)?;
        let raw_walker = unsafe { automation.RawViewWalker() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("Failed to get RawViewWalker: {}", e),
        })?;

        Ok(Self {
            automation,
            batch_request,
            scroll_request,
            raw_walker,
            handle_cache: Mutex::new(BoundedHandleCache::new(MAX_CACHED_ELEMENTS)),
            host_hwnds: Mutex::new(BoundedHandleCache::new(MAX_CACHED_ELEMENTS)),
            clips: Mutex::new(BoundedHandleCache::new(MAX_CACHED_ELEMENTS)),
            synthetic_apps: Mutex::new(BoundedHandleCache::new(MAX_SYNTHETIC_APPS)),
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
            .ok_or(Error::stale(
                xa11y_core::StaleReason::Evicted,
                format!("handle:{}", handle),
            ))
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

    /// Resolve a tagged synthetic handle without ever falling back to the pid
    /// embedded in caller-owned `ElementData`.
    ///
    /// A missing entry means the bounded identity table evicted this saved
    /// capability (or it came from another provider instance). Either way it
    /// is stale: trusting `ElementData::pid` here would defeat both eviction
    /// safety and the process-generation guard.
    fn synthetic_app_identity_checked(&self, handle: u64) -> Result<Option<SyntheticAppIdentity>> {
        if !is_synthetic_handle(handle) {
            return Ok(None);
        }
        self.synthetic_app_identity(handle)
            .map(Some)
            .ok_or_else(|| {
                Error::stale(
                    xa11y_core::StaleReason::Evicted,
                    format!(
                        "handle:{handle}; the synthetic Application identity expired, so its pid \
                     can no longer be resolved safely"
                    ),
                )
            })
    }

    /// Read the patterns included in the element's bulk-query cache, sharing
    /// them across `get_value`, `get_actions`, and `parse_states`.
    ///
    /// `GetCachedPatternAs` avoids six live cross-process pattern probes per
    /// element after `FindAllBuildCache`. WindowPattern / TransformPattern are
    /// only needed on window/dialog roles, so their live acquisition stays
    /// role-gated instead of adding two patterns to every node's bulk request.
    /// The pattern's `Current*` properties remain live where callers need them.
    ///
    /// For WindowPattern / TransformPattern, only the known "pattern absent"
    /// HRESULTs (see [`is_pattern_absent`]) become `None`. A stale element or
    /// wedged provider is a real COM failure and must not bleed into the
    /// snapshot as "no window actions and unknown window states" — the same
    /// distinction the window verbs make via [`pattern_acquisition_error`]
    /// (tenet 1).
    ///
    /// Window/transform live calls can fail, so they keep the existing
    /// absence classification and transient retry behavior.
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
                element.GetCachedPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            }
            .ok(),
            toggle: unsafe {
                element.GetCachedPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            }
            .ok(),
            expand_collapse: unsafe {
                element.GetCachedPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                )
            }
            .ok(),
            value: unsafe {
                element.GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            }
            .ok(),
            range_value: unsafe {
                element
                    .GetCachedPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            }
            .ok(),
            selection_item: unsafe {
                element.GetCachedPatternAs::<IUIAutomationSelectionItemPattern>(
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
    ///
    /// For top-level windows only: it assumes no host window and no
    /// scrolling ancestor, which is true of nothing below the desktop root.
    fn build_element_data(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
        pid: Option<u32>,
    ) -> Result<ElementData> {
        self.build_element_data_in(dpi, element, pid, None, HostHint::Parent, &Clip::default())
    }

    /// [`Self::build_element_data`] for an element whose nearest native
    /// window the caller already knows (`host_hwnd`, from its parent). An
    /// element with an HWND of its own uses that instead. The result is
    /// remembered for the element's handle, so its children inherit it, and
    /// decides whether it sits in a minimized window, which makes it not
    /// visible whatever UIA's `IsOffscreen` says (issue #501).
    ///
    /// `hint` says how far `host_hwnd` can be trusted. Only a parent's answer
    /// ([`HostHint::Parent`]) is recorded for a windowless element; a search
    /// root's ([`HostHint::Ancestor`]) is used for this snapshot's visibility
    /// but not remembered, because a child HWND between the root and the
    /// element would make it the wrong window for later navigation.
    ///
    /// `clip` is the one the element sits under (see
    /// [`Self::children_clip_of`]), unless it is a popup or top-level window,
    /// which starts afresh ([`resets_clip`]). An element entirely outside it
    /// is scrolled out of view and not visible, whatever `IsOffscreen` says
    /// (issue #502). It is remembered with the element's bounds, so its
    /// children can be clipped without a walk.
    fn build_element_data_in(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
        pid: Option<u32>,
        host_hwnd: Option<HWND>,
        hint: HostHint,
        clip: &Clip,
    ) -> Result<ElementData> {
        let own = unsafe { element.CachedNativeWindowHandle() }
            .ok()
            .filter(|h| !h.0.is_null());
        let nearest = own.or(host_hwnd);
        let handle = self.cache_element(element.clone());
        let recorded = match hint {
            HostHint::Parent => Some(nearest),
            HostHint::Ancestor => own.map(Some),
        };
        if let Some(recorded) = recorded {
            // 0 records "no window above this element", so that answer is
            // remembered too instead of re-walking.
            self.host_hwnds
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(handle, recorded.map_or(0, |h| h.0 as usize));
        }
        // A failed bounds read is "no bounds", as the snapshot below already
        // reads it (`bounds: None`), rather than an error: propagating here
        // would fail a whole level the snapshot builds fine. `no_bounds`
        // keeps such an element from being judged, and its record from ever
        // counting as unchanged.
        let bounds = crate::coords::cached_bounding_rect(dpi, element).unwrap_or_default();
        let clip = if own.is_some_and(resets_clip) {
            Clip::default()
        } else {
            clip.clone()
        };
        // A minimized window outranks the clip: restoring the window is
        // what has to happen first (see `HiddenReason`).
        let hidden = if nearest.is_some_and(hwnd_in_minimized_window) {
            Some(HiddenReason::WindowMinimized)
        } else if clipped_out(bounds, clip.rect) {
            Some(HiddenReason::ScrolledOut)
        } else {
            None
        };
        self.clips.lock().unwrap_or_else(|e| e.into_inner()).insert(
            handle,
            ClipRecord {
                bounds,
                inherited: clip,
            },
        );
        let mut data =
            build_snapshot_data(dpi, element, pid, handle, Some(&self.raw_walker), hidden)?;
        rich::parent_header_kind(
            &mut data,
            element,
            &self.raw_walker,
            self.batch_request.get(dpi),
        )?;
        Ok(data)
    }

    /// The clip `element`'s children are built under: the one it sits under
    /// narrowed by its own viewport (issue #502). Costs one refresh of
    /// `element`, plus whatever [`Self::inherited_clip_of`] needs.
    fn children_clip_of(
        &self,
        dpi: &crate::dpi::DpiScope,
        handle: u64,
        element: &IUIAutomationElement,
    ) -> Result<Clip> {
        let fresh =
            uia_call(|| unsafe { element.BuildUpdatedCache(self.scroll_request.get(dpi)) })?;
        Ok(self
            .inherited_clip_of(dpi, handle, &fresh)?
            .below(dpi, &fresh))
    }

    /// The clip `fresh` (refreshed with [`Self::scroll_request`]) sits under.
    ///
    /// The remembered one from [`Self::clips`] serves while it still holds:
    /// the element has bounds, they have not changed, and every ancestor the
    /// clip came from still overflows with the same viewport (one refresh
    /// each, usually one or none). Anything else walks the raw view, and the
    /// answer is remembered. An ancestor that has only started overflowing
    /// since, without moving the element, is the one change this misses.
    fn inherited_clip_of(
        &self,
        dpi: &crate::dpi::DpiScope,
        handle: u64,
        fresh: &IUIAutomationElement,
    ) -> Result<Clip> {
        // Read as `build_element_data_in` reads it: a failure is no bounds,
        // and no bounds never reuses a record.
        let bounds = crate::coords::cached_bounding_rect(dpi, fresh).unwrap_or_default();
        let remembered = self
            .clips
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .cloned();
        if let Some(record) = remembered {
            if !no_bounds(&bounds)
                && record.bounds == bounds
                && self.clip_still_holds(dpi, &record.inherited)?
            {
                return Ok(record.inherited);
            }
        }
        let inherited = self.walk_inherited_clip(dpi, fresh)?;
        self.clips.lock().unwrap_or_else(|e| e.into_inner()).insert(
            handle,
            ClipRecord {
                bounds,
                inherited: inherited.clone(),
            },
        );
        Ok(inherited)
    }

    /// Whether every ancestor `clip` came from still overflows with the same
    /// viewport. One that has gone away means the clip no longer holds.
    fn clip_still_holds(&self, dpi: &crate::dpi::DpiScope, clip: &Clip) -> Result<bool> {
        for source in clip.sources.iter() {
            let Some(fresh) = live_snapshot(uia_call(|| unsafe {
                source
                    .element
                    .BuildUpdatedCache(self.scroll_request.get(dpi))
            }))?
            else {
                return Ok(false);
            };
            if cached_viewport(dpi, &fresh) != Some(source.viewport) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The clip each element of a `TreeScope_Subtree` fetch sits under, by
    /// index, or `None` when the flat array cannot be attributed and the
    /// caller must take the level-by-level path (issue #502).
    ///
    /// The array is in tree order, so an element's descendants are the
    /// entries right after it. Each element that changes the clip, by
    /// overflowing or by being a popup ([`resets_clip`]), gets one
    /// `TreeScope_Descendants` fetch, which says how many entries are its
    /// descendants; the first and last of them are checked against the array
    /// by RuntimeId. Those entries are then reset (popup) or narrowed by its
    /// viewport (overflow). That is k + 1 calls for k such elements (the +1
    /// refreshes the root), where the level-by-level path makes one per
    /// parent. Visiting in array order applies an outer element before an
    /// inner one, so nested viewports intersect, and a popup inside a
    /// scrolling list starts afresh while a list inside a popup still clips.
    ///
    /// `None` when a fetch disagrees with the array (the tree changed
    /// between the two, or a provider does not return tree order), when an
    /// entry there has no RuntimeId to check, or when one has gone. An error
    /// for the root itself, including its having gone, is returned.
    fn subtree_clips(
        &self,
        dpi: &crate::dpi::DpiScope,
        root_handle: u64,
        uia_root: &IUIAutomationElement,
        subtree: &IUIAutomationElementArray,
        count: i32,
    ) -> Result<Option<Vec<Clip>>> {
        let fresh_root =
            uia_call(|| unsafe { uia_root.BuildUpdatedCache(self.scroll_request.get(dpi)) })?;
        let base = self.inherited_clip_of(dpi, root_handle, &fresh_root)?;
        let len = count.max(0) as usize;
        let elements: Vec<Option<IUIAutomationElement>> =
            (0..count).map(|i| uia_get(subtree, i)).collect();
        let overflows: Vec<bool> = elements
            .iter()
            .map(|el| {
                el.as_ref()
                    .is_some_and(|el| cached_viewport(dpi, el).is_some())
            })
            .collect();
        if base.rect.is_none() && !overflows.iter().any(|&o| o) {
            // Nothing clips, so popups have nothing to reset.
            return Ok(Some(vec![base; len]));
        }
        let root_hwnd = cached_own_hwnd(&fresh_root);
        let is_popup: Vec<bool> = elements
            .iter()
            .map(|el| {
                el.as_ref().and_then(cached_own_hwnd).is_some_and(|h| {
                    // The root's own reset is already in `base`.
                    Some(h) != root_hwnd && resets_clip(h)
                })
            })
            .collect();
        let mut clips = vec![base; len];
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        for j in 0..len {
            if !overflows[j] && !is_popup[j] {
                continue;
            }
            let Some(element) = elements[j].as_ref() else {
                continue;
            };
            // The root heads the array, and everything after it is its
            // descendants: no fetch needed to say so.
            if j == 0 && cached_own_hwnd(element).is_some() && cached_own_hwnd(element) == root_hwnd
            {
                apply_range(
                    dpi,
                    &mut clips,
                    element,
                    0,
                    len - 1,
                    is_popup[0],
                    overflows[0],
                );
                continue;
            }
            // An element that has gone since the subtree fetch is one more
            // way the array no longer matches the tree.
            let Some(descendants) = live_snapshot(uia_call(|| unsafe {
                element.FindAllBuildCache(
                    TreeScope_Descendants,
                    &true_cond,
                    self.scroll_request.get(dpi),
                )
            }))?
            else {
                return Ok(None);
            };
            let m = uia_len(&descendants).max(0) as usize;
            if m > 0 {
                let (first, last) = (j + 1, j + m);
                if last >= len {
                    return Ok(None);
                }
                for (array_index, fetched_index) in [(first, 0), (last, m - 1)] {
                    let Some(fetched) = uia_get(&descendants, fetched_index as i32) else {
                        return Ok(None);
                    };
                    let Some(in_array) = elements[array_index].as_ref() else {
                        return Ok(None);
                    };
                    let (Some(fetched), Some(in_array)) = (
                        live_snapshot(Self::runtime_id(&fetched, 0))?,
                        live_snapshot(Self::runtime_id(in_array, 0))?,
                    ) else {
                        return Ok(None);
                    };
                    if fetched.is_empty() || in_array != fetched {
                        return Ok(None);
                    }
                }
            }
            apply_range(dpi, &mut clips, element, j, m, is_popup[j], overflows[j]);
        }
        Ok(Some(clips))
    }

    /// The clip `parent` sits under, for `get_parent`. The child's
    /// remembered clip is the parent's narrowed by the parent's own
    /// viewport, so while it still holds, dropping the parent's contribution
    /// gives the answer without a walk. A child that starts a clip of its own
    /// ([`resets_clip`]) says nothing about its parent's, and walks.
    fn parent_clip(
        &self,
        dpi: &crate::dpi::DpiScope,
        child_handle: u64,
        child: &IUIAutomationElement,
        parent: &IUIAutomationElement,
    ) -> Result<Clip> {
        let remembered = self
            .clips
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&child_handle)
            .cloned();
        if let Some(record) = remembered {
            if !cached_own_hwnd(child).is_some_and(resets_clip)
                && self.clip_still_holds(dpi, &record.inherited)?
            {
                let mut sources = record.inherited.sources;
                if let Some(nearest) = sources.last() {
                    let same = uia_call(|| unsafe {
                        self.automation.CompareElements(&nearest.element, parent)
                    })?;
                    if same.as_bool() {
                        sources.pop();
                    }
                }
                let rect = sources.iter().fold(None, |rect, source| {
                    intersect_clip(rect, Some(source.viewport))
                });
                return Ok(Clip { rect, sources });
            }
        }
        self.walk_inherited_clip(dpi, parent)
    }

    /// The clip `element` sits under, found by walking the raw view up to
    /// the nearest popup or top-level window ([`resets_clip`]), or the top.
    /// One call per level: each parent comes back with the scroll
    /// properties cached. `element` itself must carry them, or the batch
    /// set, for its own window handle.
    fn walk_inherited_clip(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
    ) -> Result<Clip> {
        if cached_own_hwnd(element).is_some_and(resets_clip) {
            return Ok(Clip::default());
        }
        let mut ancestors = Vec::new();
        let mut current = element.clone();
        let mut reached_top = false;
        for _ in 0..HOST_WINDOW_WALK_LIMIT {
            current = match unsafe {
                self.raw_walker
                    .GetParentElementBuildCache(&current, self.scroll_request.get(dpi))
            } {
                Ok(parent) => parent,
                // S_OK with a null parent: the top of the tree.
                Err(e) if e.code().0 == 0 => {
                    reached_top = true;
                    break;
                }
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "walking to the scrolling ancestors \
                             (GetParentElementBuildCache) failed: {e}"
                        ),
                    })
                }
            };
            ancestors.push(current.clone());
            // A popup's own viewport still clips its content; what is above
            // it does not.
            if cached_own_hwnd(&current).is_some_and(resets_clip) {
                reached_top = true;
                break;
            }
        }
        if !reached_top {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "no top of the raw view within {HOST_WINDOW_WALK_LIMIT} ancestors while \
                     finding scroll viewports; the tree is deeper than any real one or cyclic"
                ),
            });
        }
        Ok(ancestors
            .iter()
            .rev()
            .fold(Clip::default(), |clip, ancestor| clip.below(dpi, ancestor)))
    }

    /// The nearest native window of an element this provider built, from
    /// [`Self::host_hwnds`], or by walking the raw view on a miss.
    fn host_hwnd_of(&self, handle: u64, element: &IUIAutomationElement) -> Result<Option<HWND>> {
        let remembered = self
            .host_hwnds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .copied();
        match remembered {
            Some(0) => Ok(None),
            Some(raw) => Ok(Some(HWND(raw as *mut core::ffi::c_void))),
            None => self.walk_to_host_hwnd(element),
        }
    }

    /// The nearest native window of `element`, found by walking the raw view
    /// up to the first element with a native window handle (the element
    /// itself when it has one). Windowless elements (WPF, WebView2, Qt) have
    /// no HWND of their own, which is why this is a walk; it runs only when
    /// [`Self::host_hwnds`] has no answer. `None` when the walk reaches the top
    /// without one: there is no window to be minimized.
    fn walk_to_host_hwnd(&self, element: &IUIAutomationElement) -> Result<Option<HWND>> {
        let mut current = element.clone();
        for _ in 0..HOST_WINDOW_WALK_LIMIT {
            let hwnd =
                unsafe { current.CurrentNativeWindowHandle() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!(
                        "CurrentNativeWindowHandle failed while finding the host window: {e}"
                    ),
                })?;
            if !hwnd.0.is_null() {
                return Ok(Some(hwnd));
            }
            current = match unsafe { self.raw_walker.GetParentElement(&current) } {
                Ok(parent) => parent,
                // S_OK with a null parent: the top of the tree.
                Err(e) if e.code().0 == 0 => return Ok(None),
                Err(e) => {
                    return Err(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "walking to the host window (GetParentElement) failed: {e}"
                        ),
                    })
                }
            };
        }
        Err(Error::Platform {
            code: -1,
            message: format!(
                "no element with a native window handle within {HOST_WINDOW_WALK_LIMIT} \
                 ancestors; the raw view is deeper than any real tree or cyclic"
            ),
        })
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
    /// it is the *fallback* name source only. Name resolution: a packaged
    /// app's shell display name (issue #497), else the process executable
    /// stem (`OpenProcess` + `QueryFullProcessImageNameW`); the
    /// representative window's title when the process cannot be opened.
    /// `raw` records which one was used (`uia_name_source`), plus the
    /// synthesized marker and the full executable path when known — the
    /// fallback is explicit, not silent (tenet 1).
    ///
    /// Verified syntheses of the same process generation share a handle, so
    /// repeated enumeration remains bounded. A different creation time (or
    /// an unavailable one) gets a fresh handle from [`NEXT_HANDLE`], which is
    /// how a stale `App` node stays distinct from a process that reuses its
    /// pid.
    fn build_synthetic_app_data(
        &self,
        pid: u32,
        representative: &IUIAutomationElement,
    ) -> Result<ElementData> {
        let mut aumid = None;
        let (name, name_source, executable) = match process_image_name(pid) {
            Some((path, process_aumid)) => {
                // A packaged app is named as the shell names it ("Calculator",
                // not "CalculatorApp"); everything else by its executable
                // (issue #497). The AUMID was read through the handle the
                // image name came from, so a failure there is a real error,
                // not "unpackaged".
                aumid = process_aumid?;
                let display = match aumid.as_deref() {
                    Some(a) => crate::uwp::app_display_name(a)?,
                    None => None,
                };
                let (name, source) = process_app_name(&path, display);
                (name, source, Some(path))
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
        let identity = SyntheticAppIdentity {
            pid,
            creation_time: process_creation_time(pid),
        };
        let mut synthetic_apps = self
            .synthetic_apps
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let handle = cache_synthetic_app_identity(&mut synthetic_apps, identity);
        let mut raw = HashMap::new();
        raw.insert("uia_synthesized".into(), serde_json::Value::Bool(true));
        raw.insert(
            "uia_name_source".into(),
            serde_json::Value::String(name_source.into()),
        );
        if let Some(path) = executable {
            raw.insert("uia_process_name".into(), serde_json::Value::String(path));
        }
        if let Some(aumid) = aumid {
            raw.insert(
                "uia_app_user_model_id".into(),
                serde_json::Value::String(aumid),
            );
        }
        Ok(ElementParts {
            heading_level: None,
            landmark_type: None,
            web_role: None,
            link_url: None,
            live_region: None,
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
            // Table/collection metadata is not mapped on Windows yet (#479).
            table: None,
            cell: None,
            table_row: None,
            collection: None,
            stable_id: None,
            pid: Some(pid),
            raw,
            handle,
        }
        .into())
    }

    /// The desktop's named top-level windows with the app (pid) each belongs
    /// to, plus notes for the UWP frames that belong to no app. `list_apps`
    /// and `unlisted_apps` share it, so the frames one leaves out are exactly
    /// the ones the other reports (issue #497).
    fn desktop_app_windows(&self, dpi: &crate::dpi::DpiScope) -> Result<DesktopAppWindows> {
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        // A desktop-wide UIA query that reaches a hung app stalls
        // for the transaction timeout. Check first and fail the
        // listing with every hung window named, as the Linux backend
        // does, rather than stall or drop the app (issue #507).
        let interactive = interactive_top_level_windows("checking for hung windows")?;
        let hung = hung_among(&interactive);
        if !hung.is_empty() {
            return Err(hung_windows_error(
                "every top-level window on the desktop to respond".to_string(),
                &hung,
            ));
        }
        // Every desktop child, not only `Window`-type ones: the
        // listing keeps the `Window`s below, from the cached control
        // type, and the rest (the taskbar and desktop are `Pane`s)
        // are what tells a window UIA dropped from one it merely
        // does not list (issue #603).
        let condition = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        let (found, elapsed) = timed(|| {
            uia_call(|| unsafe {
                root.FindAllBuildCache(TreeScope_Children, &condition, self.batch_request.get(dpi))
            })
        });
        let found = found.map_err(|e| desktop_listing_timeout(e, elapsed))?;
        let listing = desktop_app_windows(&DesktopArray(&found))?;
        check_desktop_omissions(&interactive, &listing.returned, elapsed)?;

        let mut attribution = crate::uwp::Attribution::new();
        let mut windows = Vec::new();
        let mut unlisted = Vec::new();
        for (el, native_pid) in listing.windows {
            let hwnd = uia_call(|| unsafe { el.CachedNativeWindowHandle() })?;
            let pid = if hwnd.0.is_null() {
                native_pid
            } else {
                match attribution.owner(hwnd) {
                    Ok(crate::uwp::WindowOwner::Process(pid)) => pid,
                    Ok(crate::uwp::WindowOwner::Unresolved { note }) => {
                        unlisted.push(note);
                        continue;
                    }
                    // The window closed while it was being attributed.
                    Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => continue,
                    Err(error) => return Err(error),
                }
            };
            windows.push((el, pid));
        }
        Ok((windows, unlisted))
    }

    /// Enumerate the interactive top-level windows owned by `pid` on the
    /// calling thread's desktop, in z-order, whatever their control type.
    ///
    /// A window is skipped when nothing in it can be interacted with (hidden,
    /// cloaked, zero-sized, or click-through such as the `SysShadow` drop
    /// shadow behind a popup). Win32 ownership is deliberately not a filter:
    /// it says nothing about whether UIA already exposes this window's subtree.
    ///
    /// App-window listing uses this native set. App-tree traversal and
    /// subscriptions use its UIA-distinct roots, excluding roots whose
    /// accessible descendants are already reachable through another root.
    /// Windows are found with `EnumWindows` rather than the UIA desktop root:
    /// UIA can place owned dialogs below their owner, and popups are not
    /// necessarily `Window` control types.
    /// [`app_by_pid`](Self::app_by_pid) still requires a `Window` desktop
    /// child when choosing its representative.
    ///
    /// An empty result is a truth, not an error: the last window closing is
    /// exactly the state "no windows" must report.
    fn top_level_windows_of_pid(
        &self,
        dpi: &crate::dpi::DpiScope,
        pid: u32,
    ) -> Result<Vec<IUIAutomationElement>> {
        top_level_windows_of_pid_with(&self.automation, pid, self.batch_request.get(dpi))
    }

    /// Populate a UIA element's snapshot so Cached* accessors work.
    /// Used for single-element reads (e.g., get_parent) that don't go
    /// through FindAllBuildCache.
    fn populate_cache(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
    ) -> windows::core::Result<IUIAutomationElement> {
        retry_transient(|| unsafe { element.BuildUpdatedCache(self.batch_request.get(dpi)) })
    }

    /// Get direct UIA children of an element with properties pre-fetched.
    /// batch_request uses raw view (TrueCondition), so FindAllBuildCache sees
    /// all elements including virtual/fragment elements.
    fn uia_children(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
    ) -> Vec<IUIAutomationElement> {
        let true_cond = match unsafe { self.automation.CreateTrueCondition() } {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        // An empty Vec here reads downstream as "this element has no children",
        // so a transiently-busy provider would make a populated subtree look
        // empty. Retry the classified-transient HRESULTs before degrading.
        match retry_transient(|| unsafe {
            element.FindAllBuildCache(TreeScope_Children, &true_cond, self.batch_request.get(dpi))
        }) {
            Ok(arr) => (0..uia_len(&arr))
                .filter_map(|i| uia_get(&arr, i))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Fetch the entire subtree with all properties pre-fetched in one COM call.
    fn find_all_subtree(
        &self,
        dpi: &crate::dpi::DpiScope,
        root: &IUIAutomationElement,
    ) -> Result<IUIAutomationElementArray> {
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        uia_call(|| unsafe {
            root.FindAllBuildCache(TreeScope_Subtree, &true_cond, self.batch_request.get(dpi))
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
    fn has_onscreen_child(
        &self,
        dpi: &crate::dpi::DpiScope,
        element: &IUIAutomationElement,
    ) -> Result<bool> {
        let true_cond = uia_call(|| unsafe { self.automation.CreateTrueCondition() })?;
        let children = uia_call(|| unsafe {
            element.FindAllBuildCache(TreeScope_Children, &true_cond, self.batch_request.get(dpi))
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
        dpi: &crate::dpi::DpiScope,
        root: &IUIAutomationElement,
        class: &str,
    ) -> Result<Option<IUIAutomationElement>> {
        let condition = uia_call(|| unsafe {
            self.automation
                .CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(class))
        })?;
        match unsafe {
            root.FindFirstBuildCache(
                TreeScope_Descendants,
                &condition,
                self.batch_request.get(dpi),
            )
        } {
            Ok(el) => Ok(Some(el)),
            Err(e) if e.code().is_ok() => Ok(None),
            Err(e) => Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("FindFirstBuildCache(ClassName={class}) failed: {e}"),
            }),
        }
    }

    /// Direct Raw View children of the UIA desktop root, with the batch cache
    /// populated on every element.
    ///
    /// A `FindAllBuildCache(TreeScope_Children, ...)` query navigates Control
    /// View even when the cache request itself uses a Raw View tree filter.
    /// Native `#32768` popup-menu windows are not guaranteed to be control
    /// elements, so the shell scan must navigate with `RawViewWalker` too.
    fn raw_children_with_cache(
        &self,
        dpi: &crate::dpi::DpiScope,
        root: &IUIAutomationElement,
    ) -> Result<Vec<IUIAutomationElement>> {
        let mut current = match retry_transient(|| unsafe {
            self.raw_walker
                .GetFirstChildElementBuildCache(root, self.batch_request.get(dpi))
        }) {
            Ok(element) => element,
            Err(e) if e.code().is_ok() => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error::Platform {
                    code: i64::from(e.code().0),
                    message: format!("RawViewWalker.GetFirstChildElementBuildCache failed: {e}"),
                });
            }
        };
        let mut children = Vec::new();

        loop {
            children.push(current.clone());
            current = match retry_transient(|| unsafe {
                self.raw_walker
                    .GetNextSiblingElementBuildCache(&current, self.batch_request.get(dpi))
            }) {
                Ok(element) => element,
                Err(e) if e.code().is_ok() => break,
                Err(e) => {
                    return Err(Error::Platform {
                        code: i64::from(e.code().0),
                        message: format!(
                            "RawViewWalker.GetNextSiblingElementBuildCache failed after {} \
                             desktop child(ren): {e}",
                            children.len()
                        ),
                    });
                }
            };
        }

        Ok(children)
    }

    /// A UIA element's RuntimeId as a `Vec<i32>`: the identity UIA itself
    /// compares elements by (`CompareElements`), and the one key that
    /// survives this provider minting a fresh handle on every fetch.
    /// `GetRuntimeId` hands back a SAFEARRAY of i32 that this copies and
    /// destroys. A failure propagates (tenet 1): `UIA_E_ELEMENTNOTAVAILABLE`
    /// means the element is gone, which callers handle with `live_snapshot`.
    ///
    /// An element with no RuntimeId comes back as an empty `Vec`; it has no
    /// identity to compare, so callers must not dedup on it. `handle` is only
    /// for the error messages.
    fn runtime_id(element: &IUIAutomationElement, handle: u64) -> Result<Vec<i32>> {
        use windows::Win32::System::Com::SAFEARRAY;
        use windows::Win32::System::Ole::{
            SafeArrayAccessData, SafeArrayDestroy, SafeArrayUnaccessData,
        };

        let context =
            |message: String| format!("GetRuntimeId for element handle {handle}: {message}");
        let sa: *mut SAFEARRAY =
            uia_call(|| unsafe { element.GetRuntimeId() }).map_err(|e| match e {
                Error::Platform { code, message } => Error::Platform {
                    code,
                    message: context(message),
                },
                other => other,
            })?;
        if sa.is_null() {
            // UIA documents a null array for an element without a RuntimeId.
            return Ok(Vec::new());
        }
        // SAFETY: GetRuntimeId handed us ownership of `sa`. Its shape is
        // checked before the data is read; it is unlocked and destroyed on
        // every path.
        unsafe {
            let dims = (*sa).cDims;
            if dims != 1 {
                // Already failing; a failed destroy only leaks the array.
                let _ = SafeArrayDestroy(sa);
                return Err(Error::Platform {
                    code: -1,
                    message: context(format!("expected a 1-D array, got {dims} dimensions")),
                });
            }
            let count = (*sa).rgsabound[0].cElements as usize;
            let mut data_ptr: *mut core::ffi::c_void = core::ptr::null_mut();
            if let Err(e) = SafeArrayAccessData(sa, &mut data_ptr as *mut _) {
                let _ = SafeArrayDestroy(sa);
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: context(format!("SafeArrayAccessData failed: {e}")),
                });
            }
            let v = if count == 0 || data_ptr.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data_ptr as *const i32, count).to_vec()
            };
            // The values are already copied; a failed unlock or destroy
            // leaks the array and changes nothing about them.
            let _ = SafeArrayUnaccessData(sa);
            let _ = SafeArrayDestroy(sa);
            Ok(v)
        }
    }
}

/// Identity-stable `get_children` for one query's tree walk (issue #571).
///
/// `WindowsProvider` mints a fresh handle each time it builds an element, so
/// a walk that reaches one element through two anchors (`* button`: the
/// button sits under several `*` matches) gets two handles, and the core
/// walkers' handle dedup keeps both. This wrapper gives every element the
/// first handle this query minted for its RuntimeId, which turns that dedup
/// (and `:nth` counting after it) into a dedup by UIA identity. If the
/// canonical handle was evicted from the handle cache, it is re-inserted with
/// the newest element. An element without a RuntimeId keeps its own handle,
/// and an element that vanishes before its RuntimeId is read is left out, as
/// the walk does for any element that goes away mid-snapshot.
struct CanonicalWalk<'a> {
    provider: &'a WindowsProvider,
    handles: std::cell::RefCell<HashMap<Vec<i32>, u64>>,
}

impl<'a> CanonicalWalk<'a> {
    fn new(provider: &'a WindowsProvider) -> Self {
        Self {
            provider,
            handles: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn children(&self, parent: Option<&ElementData>) -> Result<Vec<ElementData>> {
        let children = self.provider.get_children(parent)?;
        let mut out = Vec::with_capacity(children.len());
        for mut child in children {
            if let Some(handle) = self.canonical_handle(child.handle)? {
                child.handle = handle;
                out.push(child);
            }
        }
        Ok(out)
    }

    /// The query's handle for the element behind `handle`; `None` when the
    /// element is gone. Synthesized app nodes have stable handles already.
    fn canonical_handle(&self, handle: u64) -> Result<Option<u64>> {
        if is_synthetic_handle(handle) {
            return Ok(Some(handle));
        }
        let element = self.provider.get_cached(handle)?;
        let Some(id) = live_snapshot(WindowsProvider::runtime_id(&element, handle))? else {
            return Ok(None);
        };
        if id.is_empty() {
            return Ok(Some(handle));
        }
        let canonical = *self.handles.borrow_mut().entry(id).or_insert(handle);
        if canonical != handle {
            let mut cache = self
                .provider
                .handle_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Only an evicted entry needs re-inserting; refreshing a live one
            // would cost the cache an O(n) reorder per duplicate.
            if cache.get(&canonical).is_none() {
                cache.insert(canonical, element);
            }
        }
        Ok(Some(canonical))
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

/// HRESULTs that mean the element, or the process serving it, is gone:
/// `UIA_E_ELEMENTNOTAVAILABLE`, `RPC_E_DISCONNECTED`, `CO_E_OBJNOTCONNECTED`
/// and `RPC_S_SERVER_UNAVAILABLE` as an HRESULT.
fn is_gone_code(code: i64) -> bool {
    const RPC_E_DISCONNECTED_HR: i32 = 0x8001_0108_u32 as i32;
    const CO_E_OBJNOTCONNECTED_HR: i32 = 0x8004_01FD_u32 as i32;
    const RPC_S_SERVER_UNAVAILABLE_HR: i32 = 0x8007_06BA_u32 as i32;
    [
        UIA_E_ELEMENTNOTAVAILABLE as i32,
        RPC_E_DISCONNECTED_HR,
        CO_E_OBJNOTCONNECTED_HR,
        RPC_S_SERVER_UNAVAILABLE_HR,
    ]
    .iter()
    .any(|hr| i64::from(*hr) == code)
}

/// A candidate invalidated during a live tree/query snapshot is no longer
/// part of that snapshot. Restrict pruning to UIA's exact unavailable HRESULT;
/// busy/disconnected providers and saved process-identity failures still fail.
///
/// Apply this only after the operation's existing transient retries. The same
/// numeric HRESULT also names EVENT_E_ALL_SUBSCRIBERS_FAILED, so a provider
/// that recovers during the retry budget still contributes its snapshot.
/// Direct reads and actions retain their existing error behavior.
fn live_snapshot<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Error::Platform { code, .. })
            if code == i64::from(UIA_E_ELEMENTNOTAVAILABLE as i32) =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
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
    let candidates = native_windows_of_pid_with(autom, pid, cache)?;
    distinct_uia_roots(autom, cache, candidates)
}

/// How long UIA waits to *connect* to a provider before giving up.
///
/// Set explicitly rather than inherited from `CUIAutomation8`'s defaults, so
/// the bound on a hung provider is a documented property of xa11y, not of the
/// OS build (issue #507).
const UIA_CONNECTION_TIMEOUT_MS: u32 = 2_000;
/// How long one UIA call (a property read, a FindAll) may take once
/// connected.
const UIA_TRANSACTION_TIMEOUT_MS: u32 = 20_000;
/// How long Windows lets a window go without pumping messages before
/// `IsHungAppWindow` reports it hung. Reported as the `Timeout` elapsed time,
/// since it is the wait the OS already observed.
const OS_HUNG_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(5);

/// Apply [`UIA_CONNECTION_TIMEOUT_MS`] / [`UIA_TRANSACTION_TIMEOUT_MS`].
///
/// `CUIAutomation8` always implements `IUIAutomation2`, so a failed cast or
/// setter is a real platform failure and propagates (tenet 1).
fn set_uia_timeouts(automation: &IUIAutomation) -> Result<()> {
    let automation2: IUIAutomation2 =
        windows::core::Interface::cast(automation).map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("IUIAutomation does not implement IUIAutomation2: {e}"),
        })?;
    unsafe { automation2.SetConnectionTimeout(UIA_CONNECTION_TIMEOUT_MS) }.map_err(|e| {
        Error::Platform {
            code: e.code().0 as i64,
            message: format!("IUIAutomation2.SetConnectionTimeout failed: {e}"),
        }
    })?;
    unsafe { automation2.SetTransactionTimeout(UIA_TRANSACTION_TIMEOUT_MS) }.map_err(|e| {
        Error::Platform {
            code: e.code().0 as i64,
            message: format!("IUIAutomation2.SetTransactionTimeout failed: {e}"),
        }
    })?;
    Ok(())
}

/// Whether Windows already considers a top-level window hung.
///
/// `IsHungAppWindow` is the OS's own verdict (no message pumped for
/// [`OS_HUNG_THRESHOLD`]), read without sending the window anything. A window
/// that is merely busy for a moment is not hung by this test, so its reads go
/// on into UIA, which the transaction timeout bounds; only a window the OS
/// has given up on fails fast (issue #507).
fn window_is_hung(hwnd: HWND) -> bool {
    unsafe { IsHungAppWindow(hwnd) }.as_bool()
}

/// A top-level window Windows considers hung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HungWindow {
    hwnd: HWND,
    pid: u32,
}

/// Every interactive top-level window on the desktop that Windows considers
/// hung, found before the desktop-wide UIA enumeration so the listing can
/// fail fast instead of stalling on one for the transaction timeout.
fn hung_top_level_windows() -> Result<Vec<HungWindow>> {
    Ok(hung_among(&interactive_top_level_windows(
        "checking for hung windows",
    )?))
}

/// The windows of `windows` that Windows considers hung.
fn hung_among(windows: &[HWND]) -> Vec<HungWindow> {
    windows
        .iter()
        .copied()
        .filter(|hwnd| window_is_hung(*hwnd))
        .map(|hwnd| HungWindow {
            hwnd,
            pid: hwnd_pid(hwnd),
        })
        .collect()
}

/// Every interactive top-level window on the desktop: the windows a desktop
/// listing reads. One that closes while it is being classified is skipped.
/// `purpose` completes "EnumWindows failed while …".
fn interactive_top_level_windows(purpose: &str) -> Result<Vec<HWND>> {
    interactive_top_level_windows_owned_by(purpose, None)
}

/// [`interactive_top_level_windows`], limited to the windows `owner` owns when
/// it is `Some`. Ownership is checked first, so only those windows pay for the
/// interactivity read.
fn interactive_top_level_windows_owned_by(purpose: &str, owner: Option<u32>) -> Result<Vec<HWND>> {
    unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> BOOL {
        let (owner, all) = unsafe { &mut *(param.0 as *mut (Option<u32>, Vec<HWND>)) };
        if owner.is_none_or(|pid| hwnd_pid(hwnd) == pid) {
            all.push(hwnd);
        }
        true.into()
    }
    let mut state: (Option<u32>, Vec<HWND>) = (owner, Vec::new());
    unsafe { EnumWindows(Some(collect), LPARAM((&raw mut state) as isize)) }.map_err(|e| {
        Error::Platform {
            code: e.code().0 as i64,
            message: format!("EnumWindows failed while {purpose}: {e}"),
        }
    })?;
    let mut interactive = Vec::new();
    for hwnd in state.1 {
        match is_interactive_top_level(hwnd) {
            Ok(true) => interactive.push(hwnd),
            Ok(false) => {}
            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(interactive)
}

/// The pid owning `hwnd`, from the window manager (0 when it cannot say).
fn hwnd_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Longest list of hung windows a diagnosis carries (tenet 6: bounded).
const DIAG_HUNG_WINDOW_LIMIT: usize = 10;

/// One hung window, described for a diagnosis: HWND, title, pid, executable.
fn describe_hung_window(window: HungWindow) -> String {
    let exe = process_image_name(window.pid)
        .and_then(|(path, _aumid)| {
            std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "<executable unreadable>".to_string());
    format!(
        "HWND {:#x} \"{}\" (pid {}, {exe})",
        window.hwnd.0 as usize,
        hwnd_title(window.hwnd),
        window.pid
    )
}

/// The [`Error::Timeout`] for a read that needs windows Windows considers
/// hung, the same shape the Linux backend reports for an unresponsive peer.
fn hung_windows_error(condition: String, hung: &[HungWindow]) -> Error {
    let mut listed: Vec<String> = hung
        .iter()
        .take(DIAG_HUNG_WINDOW_LIMIT)
        .map(|w| describe_hung_window(*w))
        .collect();
    if hung.len() > DIAG_HUNG_WINDOW_LIMIT {
        listed.push(format!("… (+{} more)", hung.len() - DIAG_HUNG_WINDOW_LIMIT));
    }
    Error::timeout(OS_HUNG_THRESHOLD).diagnose(
        xa11y_core::Diagnosis::new()
            .condition(condition)
            .last_observed(format!(
                "Windows reports {} top-level window(s) not responding (IsHungAppWindow: no \
                 message pumped for {:?}, the OS threshold, which is the elapsed time this \
                 error reports; xa11y did not wait): {}",
                hung.len(),
                OS_HUNG_THRESHOLD,
                listed.join("; ")
            ))
            .candidates(listed),
    )
}

/// Run `f`, returning its result and how long it took.
///
/// The discovery call sites time their UIA call so a `UIA_E_TIMEOUT` can be
/// reported with the wait that actually happened (see
/// [`uia_timeout_as_timeout`]).
fn timed<T>(f: impl FnOnce() -> T) -> (T, std::time::Duration) {
    let start = std::time::Instant::now();
    let out = f();
    (out, start.elapsed())
}

/// The message of a `UIA_E_TIMEOUT` platform error, `None` for any other
/// error.
fn uia_timeout_message(err: &Error) -> Option<&str> {
    match err {
        Error::Platform { code, message } if *code == i64::from(UIA_E_TIMEOUT as i32) => {
            Some(message)
        }
        _ => None,
    }
}

/// Which of the two UIA limits a `UIA_E_TIMEOUT` after `elapsed` fits.
///
/// The HRESULT is the same for both, so this is read off the wait itself. A
/// call that gave up before the transaction timeout could have elapsed can
/// only have hit the connection timeout. A longer wait is *consistent with*
/// the transaction timeout but does not prove it (a slow connection attempt
/// can follow a slow transaction), so it is stated as such.
fn uia_timeout_which(elapsed: std::time::Duration) -> String {
    if elapsed < std::time::Duration::from_millis(u64::from(UIA_TRANSACTION_TIMEOUT_MS)) {
        format!(
            "too soon for the {UIA_TRANSACTION_TIMEOUT_MS} ms transaction timeout, so it was \
             the {UIA_CONNECTION_TIMEOUT_MS} ms connection timeout: UIA could not reach a \
             window's provider"
        )
    } else {
        format!(
            "consistent with the {UIA_TRANSACTION_TIMEOUT_MS} ms transaction timeout: a \
             provider took the call but did not finish it"
        )
    }
}

/// The `last_observed` text for a `UIA_E_TIMEOUT` after `elapsed`.
fn uia_timeout_observed(elapsed: std::time::Duration, message: &str) -> String {
    format!(
        "UIA gave up after {elapsed:.1?} (UIA_E_TIMEOUT), {}: {message}",
        uia_timeout_which(elapsed)
    )
}

/// Map a UIA call that ran out one of its timeouts to the same
/// [`Error::Timeout`] form; every other error passes through unchanged.
///
/// `elapsed` is how long the call ran (see [`timed`]) and is the elapsed time
/// the error reports. `UIA_E_TIMEOUT` does not say whether the connection or
/// the transaction timeout fired, and the two differ tenfold, so the error
/// must not assert either one; [`uia_timeout_which`] reads it off `elapsed`.
///
/// Applied at the app-discovery call sites (desktop listing, a pid's
/// windows, `app_by_pid`, `focused_app`), which are where one hung app
/// stalls everything. Element-level calls keep reporting `UIA_E_TIMEOUT` as
/// the `Platform` error it arrives as.
fn uia_timeout_as_timeout(
    err: Error,
    elapsed: std::time::Duration,
    condition: impl FnOnce() -> String,
) -> Error {
    let Some(message) = uia_timeout_message(&err) else {
        return err;
    };
    let observed = uia_timeout_observed(elapsed, message);
    Error::timeout(elapsed).diagnose(
        xa11y_core::Diagnosis::new()
            .condition(condition())
            .last_observed(observed),
    )
}

/// How long one window gets to answer the `WM_NULL` probe.
const PROBE_WINDOW_TIMEOUT_MS: u32 = 250;
/// Most windows the probe sends to.
const PROBE_WINDOW_LIMIT: usize = 64;
/// Total time the probe may spend, whatever the window count.
const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// What [`probe_windows`] found: for the desktop's visible top-level
/// windows after a UIA timeout ([`probe_unresponsive_windows`]), or for the
/// windows the listing left out ([`check_desktop_omissions`]).
#[derive(Debug, Default)]
struct ProbeReport {
    /// What the probed windows are, completing "probed N of M … window(s)".
    scope: &'static str,
    /// How many windows were to be probed.
    total: usize,
    /// How many of them were sent `WM_NULL`.
    probed: usize,
    /// The probed windows that did not answer in time.
    unresponsive: Vec<HungWindow>,
    /// The probed windows whose `SendMessageTimeoutW` failed for another
    /// reason (for example UIPI refusing a message to an elevated window),
    /// with that error. Not suspects: they say nothing about responsiveness.
    failed: Vec<(HungWindow, String)>,
    /// The probed windows that answered.
    answered: Vec<HungWindow>,
    /// The probed windows that closed before or while they were probed.
    closed: Vec<HungWindow>,
    /// Why probing stopped before every window was sent to, if it did.
    stopped: Option<String>,
}

/// What one zero return from `SendMessageTimeoutW` means for the probe.
#[derive(Debug, PartialEq, Eq)]
enum ProbeMiss {
    /// The window did not answer: `ERROR_TIMEOUT`, or the OS considers it
    /// hung (`SMTO_ABORTIFHUNG` returns at once for those).
    Unresponsive,
    /// The window closed while it was probed; it is not what UIA waits on.
    Closed,
    /// The send failed for another reason, which says nothing about whether
    /// the window answers.
    Failed,
}

/// Classify a failed probe send. `code` is the thread's last error, read
/// straight after the send; `hung` is `IsHungAppWindow`; `exists` is
/// `IsWindow`.
fn classify_probe_miss(code: windows::core::HRESULT, hung: bool, exists: bool) -> ProbeMiss {
    if code == windows::core::HRESULT::from_win32(ERROR_TIMEOUT.0) || (exists && hung) {
        ProbeMiss::Unresponsive
    } else if !exists {
        ProbeMiss::Closed
    } else {
        ProbeMiss::Failed
    }
}

impl ProbeReport {
    /// One clause for `last_observed`: what was probed, how, and the result.
    fn summary(&self) -> String {
        let mut out = format!(
            "probed {} of {} {} window(s) with WM_NULL ({} ms each, {:?} budget)",
            self.probed, self.total, self.scope, PROBE_WINDOW_TIMEOUT_MS, PROBE_BUDGET
        );
        if let Some(stopped) = &self.stopped {
            out.push_str(&format!(", stopped early: {stopped}"));
        }
        if self.unresponsive.is_empty() && self.failed.is_empty() {
            out.push_str(
                "; every probed window answered, so the window UIA could not reach recovered \
                 before the probe, or its message loop is fine and its UIA provider is not",
            );
        } else if self.unresponsive.is_empty() {
            out.push_str("; no probed window timed out");
        } else {
            out.push_str(&format!(
                "; {} did not answer: {}",
                self.unresponsive.len(),
                self.candidates().join("; ")
            ));
        }
        if !self.failed.is_empty() {
            let mut failed: Vec<String> = self
                .failed
                .iter()
                .take(DIAG_HUNG_WINDOW_LIMIT)
                .map(|(w, err)| format!("probe of {} failed: {err}", describe_hung_window(*w)))
                .collect();
            if self.failed.len() > DIAG_HUNG_WINDOW_LIMIT {
                failed.push(format!(
                    "… (+{} more)",
                    self.failed.len() - DIAG_HUNG_WINDOW_LIMIT
                ));
            }
            out.push_str(&format!(
                "; {} probe(s) failed and say nothing about responsiveness: {}",
                self.failed.len(),
                failed.join("; ")
            ));
        }
        out
    }

    /// The unresponsive windows, described and bounded like
    /// [`hung_windows_error`]'s list.
    fn candidates(&self) -> Vec<String> {
        let mut listed: Vec<String> = self
            .unresponsive
            .iter()
            .take(DIAG_HUNG_WINDOW_LIMIT)
            .map(|w| describe_hung_window(*w))
            .collect();
        if self.unresponsive.len() > DIAG_HUNG_WINDOW_LIMIT {
            listed.push(format!(
                "… (+{} more)",
                self.unresponsive.len() - DIAG_HUNG_WINDOW_LIMIT
            ));
        }
        listed
    }
}

/// Find which top-level windows are not answering messages right now.
///
/// Failure path only: the desktop listing calls it after its `FindAll` came
/// back `UIA_E_TIMEOUT`, which names no window. Each interactive top-level
/// window gets `WM_NULL` through `SendMessageTimeoutW`; one that does not
/// answer within [`PROBE_WINDOW_TIMEOUT_MS`] is the window UIA could not
/// reach, or one like it. `SMTO_ABORTIFHUNG` returns at once for a window the
/// OS already considers hung. Bounded by [`PROBE_WINDOW_LIMIT`] windows and
/// [`PROBE_BUDGET`] in total, so it never costs more than the timeout that
/// led here (tenet 6: rich, but bounded).
fn probe_unresponsive_windows() -> Result<ProbeReport> {
    // The budget covers enumerating and classifying the windows too.
    let start = std::time::Instant::now();
    let windows = interactive_top_level_windows("probing for an unresponsive window")?;
    Ok(probe_windows(&windows, "visible top-level", start))
}

/// Send each of `windows` `WM_NULL` and sort them by how they answered,
/// within [`PROBE_WINDOW_LIMIT`] windows and [`PROBE_BUDGET`] counted from
/// `start`. `scope` says what the windows are, for the report's summary.
fn probe_windows(windows: &[HWND], scope: &'static str, start: std::time::Instant) -> ProbeReport {
    let mut report = ProbeReport {
        scope,
        total: windows.len(),
        ..ProbeReport::default()
    };
    for (i, hwnd) in windows.iter().copied().enumerate() {
        if i == PROBE_WINDOW_LIMIT {
            report.stopped = Some(format!("window limit of {PROBE_WINDOW_LIMIT} reached"));
            break;
        }
        if start.elapsed() >= PROBE_BUDGET {
            report.stopped = Some(format!("{PROBE_BUDGET:?} budget spent"));
            break;
        }
        report.probed += 1;
        let mut result = 0usize;
        let answered = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                PROBE_WINDOW_TIMEOUT_MS,
                Some(&mut result),
            )
        };
        if answered.0 != 0 {
            report.answered.push(HungWindow {
                hwnd,
                pid: hwnd_pid(hwnd),
            });
            continue;
        }
        // Zero means "failed or timed out". Read the reason before any other
        // call can overwrite the thread's last error.
        let err = windows::core::Error::from_thread();
        let window = HungWindow {
            hwnd,
            pid: hwnd_pid(hwnd),
        };
        match classify_probe_miss(
            err.code(),
            window_is_hung(hwnd),
            unsafe { IsWindow(Some(hwnd)) }.as_bool(),
        ) {
            ProbeMiss::Unresponsive => report.unresponsive.push(window),
            ProbeMiss::Closed => report.closed.push(window),
            ProbeMiss::Failed => report.failed.push((
                window,
                format!("HRESULT {:#010X}: {}", err.code().0 as u32, err.message()),
            )),
        }
    }
    report
}

/// [`uia_timeout_as_timeout`] for the desktop-wide listing, whose
/// `FindAll` does not say which window it could not reach: the diagnosis
/// adds a [`probe_unresponsive_windows`] report and names the windows that
/// did not answer as its candidates. A failed probe is reported in the
/// diagnosis and does not replace the timeout, which is the failure.
///
/// The listing's `Timeout` is terminal: no poll loop retries it (`App::find`
/// retries only `SelectorNotMatched`), so the probe runs once, for the error
/// the caller sees.
///
/// `elapsed` is the `FindAll` call alone, and it is what the `Timeout`
/// reports. The probe runs afterwards and its time (at most
/// [`PROBE_BUDGET`] plus one window's wait) is not included.
fn desktop_listing_timeout(err: Error, elapsed: std::time::Duration) -> Error {
    let Some(message) = uia_timeout_message(&err) else {
        return err;
    };
    let mut observed = uia_timeout_observed(elapsed, message);
    let mut diagnosis =
        xa11y_core::Diagnosis::new().condition("the desktop's top-level windows".to_string());
    match probe_unresponsive_windows() {
        Ok(report) => {
            observed.push_str("; ");
            observed.push_str(&report.summary());
            diagnosis = diagnosis.candidates(report.candidates());
        }
        Err(e) => observed.push_str(&format!(
            "; probing the top-level windows for the one that did not answer failed: {e}"
        )),
    }
    Error::timeout(elapsed).diagnose(diagnosis.last_observed(observed))
}

/// The desktop root's children as the listing reads them: each one's cached
/// HWND, control type, pid and name. A trait so [`desktop_app_windows`] can
/// be tested without a live UIA.
trait DesktopChildren {
    type Element;
    fn len(&self) -> Result<i32>;
    fn element(&self, i: i32) -> Result<Self::Element>;
    /// The element's native window, 0 when it has none.
    fn hwnd(&self, el: &Self::Element, i: i32) -> Result<usize>;
    fn control_type(&self, el: &Self::Element, i: i32) -> Result<i32>;
    fn pid(&self, el: &Self::Element, i: i32) -> Result<i32>;
    fn name(&self, el: &Self::Element, i: i32) -> Result<String>;
}

/// The listing's `FindAllBuildCache` result, read from its batch cache.
struct DesktopArray<'a>(&'a IUIAutomationElementArray);

/// Prefix a platform error's message with what was being read.
fn reading(what: impl FnOnce() -> String) -> impl FnOnce(Error) -> Error {
    move |e| match e {
        Error::Platform { code, message } => Error::Platform {
            code,
            message: format!("{} failed: {message}", what()),
        },
        other => other,
    }
}

// Strict reads: `Length` and `GetElement` failures are real COM failures,
// not absent windows, so propagating keeps a transient UIA failure from
// silently truncating the process list (tenet 1). `uia_call` retries the
// classified-transient HRESULTs first; what survives is persistent. The
// Cached* reads come from the batch cache FindAllBuildCache populated, so a
// failure there is an enumeration failure rather than an absent value, and
// it propagates with the index it failed for (tenet 6), instead of letting
// the `pid == 0` / empty-name filters turn it into a silent dismissal.
impl DesktopChildren for DesktopArray<'_> {
    type Element = IUIAutomationElement;
    fn len(&self) -> Result<i32> {
        uia_call(|| unsafe { self.0.Length() })
    }
    fn element(&self, i: i32) -> Result<IUIAutomationElement> {
        uia_call(|| unsafe { self.0.GetElement(i) }).map_err(reading(|| {
            format!("IUIAutomationElementArray.GetElement({i})")
        }))
    }
    fn hwnd(&self, el: &IUIAutomationElement, i: i32) -> Result<usize> {
        uia_call(|| unsafe { el.CachedNativeWindowHandle() })
            .map(|h| h.0 as usize)
            .map_err(reading(|| {
                format!("reading the native window of desktop window #{i}")
            }))
    }
    fn control_type(&self, el: &IUIAutomationElement, i: i32) -> Result<i32> {
        uia_call(|| unsafe { el.CachedControlType() })
            .map(|c| c.0)
            .map_err(reading(|| {
                format!("reading the control type of desktop window #{i}")
            }))
    }
    fn pid(&self, el: &IUIAutomationElement, i: i32) -> Result<i32> {
        uia_call(|| unsafe { el.CachedProcessId() }).map_err(reading(|| {
            format!("reading the process id of desktop window #{i}")
        }))
    }
    fn name(&self, el: &IUIAutomationElement, i: i32) -> Result<String> {
        uia_call(|| unsafe { el.CachedName() })
            .map(|n| n.to_string())
            .map_err(reading(|| {
                format!("reading the name of desktop window #{i}")
            }))
    }
}

/// What [`desktop_app_windows`] read from the desktop's children.
struct DesktopListing<E> {
    /// The listed windows and their pids, in the order UIA returned them.
    windows: Vec<(E, u32)>,
    /// The HWND of every child UIA returned that is listed or is not a
    /// `Window`. A `Window` the pid or name filter drops is left out.
    returned: HashSet<usize>,
}

/// Read the listing's desktop children: the windows it reports, and the
/// HWNDs [`check_desktop_omissions`] counts as returned.
///
/// A child is listed when it is a `Window` (the condition the listing's
/// `FindAll` used to apply; it now runs unconditioned so the other children
/// count as returned), has an owning process, and has a name. `pid == 0`
/// skips windows with no resolvable owning process, and the empty-name skip
/// drops windows still unnamed mid-startup. The pid and name of a child that
/// is not a `Window` are not read, as before.
fn desktop_app_windows<C: DesktopChildren>(children: &C) -> Result<DesktopListing<C::Element>> {
    let mut windows = Vec::new();
    let mut returned = HashSet::new();
    for i in 0..children.len()? {
        let el = children.element(i)?;
        let hwnd = children.hwnd(&el, i)?;
        let mut mark_returned = || {
            if hwnd != 0 {
                returned.insert(hwnd);
            }
        };
        if children.control_type(&el, i)? != UIA_WindowControlTypeId.0 {
            mark_returned();
            continue;
        }
        // A `Window` the filters drop does not count as returned, so the
        // omission check probes it: an unreadable owner or name is also what
        // a window UIA could not reach looks like.
        let pid = children.pid(&el, i)?.max(0) as u32;
        if pid == 0 {
            continue;
        }
        if children.name(&el, i)?.is_empty() {
            continue;
        }
        mark_returned();
        windows.push((el, pid));
    }
    Ok(DesktopListing { windows, returned })
}

/// The interactive top-level windows UIA's desktop listing did not return.
///
/// An owned window (a dialog, a progress or message box, a tool palette, a
/// drop-down) is top-level to `EnumWindows`, but UIA places it under its
/// owner rather than at the desktop root, so it counts as returned when its
/// root owner was. `root_owner` is [`root_owner`], injected so the
/// comparison is testable without windows.
fn omitted_windows(
    interactive: &[HWND],
    returned: &HashSet<usize>,
    root_owner: impl Fn(HWND) -> HWND,
) -> Vec<HWND> {
    interactive
        .iter()
        .copied()
        .filter(|hwnd| {
            !returned.contains(&(hwnd.0 as usize))
                && !returned.contains(&(root_owner(*hwnd).0 as usize))
        })
        .collect()
}

/// Longest owner chain [`root_owner`] follows; real chains are a few deep,
/// and the bound keeps a cycle from looping.
const OWNER_CHAIN_LIMIT: usize = 32;

/// A window's root owner, following `GetWindow(GW_OWNER)`: itself when it
/// has no owner.
///
/// Not `GetAncestor(GA_ROOTOWNER)`: that walks `GetParent`, which reports
/// the owner of a `WS_POPUP` window only. A WinForms or WPF dialog shown
/// with an owner is an overlapped window, so `GA_ROOTOWNER` returns the
/// dialog itself (measured on the Windows runner for #603).
fn root_owner(hwnd: HWND) -> HWND {
    let mut current = hwnd;
    for _ in 0..OWNER_CHAIN_LIMIT {
        // `GetWindow` fails with a null HWND when there is no owner, and
        // otherwise only for a window that closed meanwhile. Either way the
        // chain ends here: a closed window is classified by the probe.
        match unsafe { GetWindow(current, GW_OWNER) } {
            Ok(owner) if !owner.0.is_null() => current = owner,
            _ => break,
        }
    }
    current
}

/// Fail the listing when UIA silently left out a window that does not
/// answer (issue #603).
///
/// A window that stopped pumping messages less than the OS's 5 s ago passes
/// the hung pre-check; UIA's desktop `FindAll` then waits out its timeouts
/// on it and returns `S_OK` without it. The listing compares the interactive
/// windows `EnumWindows` reported before the `FindAll` with the HWNDs UIA
/// returned, and probes the missing ones only, so a listing with nothing
/// missing costs no more than the hung pre-check already did.
fn check_desktop_omissions(
    interactive: &[HWND],
    returned: &HashSet<usize>,
    elapsed: std::time::Duration,
) -> Result<()> {
    let missing = omitted_windows(interactive, returned, root_owner);
    if missing.is_empty() {
        return Ok(());
    }
    let report = probe_windows(&missing, "missing", std::time::Instant::now());
    // Missing windows that answered, closed, or could not be probed do not
    // fail the listing; the decision on #603 was to fail only on a window
    // that does not answer. One that answers is not one UIA timed out on:
    // it was shown, re-owned or renamed between EnumWindows and the FindAll,
    // or it is a `Window` the listing's pid or name filter dropped (an app
    // still starting up). One that closed has nothing left to list. A failed
    // probe (UIPI refusing a message to an elevated window, say) says nothing
    // about whether the window answers. Each is logged rather than dropped
    // silently, so the omission stays visible.
    for window in &report.answered {
        eprintln!(
            "xa11y: UIA's desktop listing did not return {}, which answers messages; not \
             treated as an error (shown or changed during the listing, or not yet named)",
            describe_hung_window(*window)
        );
    }
    for window in &report.closed {
        eprintln!(
            "xa11y: UIA's desktop listing did not return HWND {:#x} (pid {}), which has \
             since closed; not treated as an error",
            window.hwnd.0 as usize, window.pid
        );
    }
    for (window, err) in &report.failed {
        eprintln!(
            "xa11y: UIA's desktop listing did not return {}, and the WM_NULL probe of it \
             failed ({err}), which says nothing about whether it answers; not treated as an \
             error",
            describe_hung_window(*window)
        );
    }
    if let Some(stopped) = &report.stopped {
        eprintln!(
            "xa11y: stopped probing the windows UIA's desktop listing did not return \
             ({stopped}); {} of {} were not probed",
            report.total - report.probed,
            report.total
        );
    }
    match dropped_windows_error(&report, missing.len(), elapsed) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// The [`Error::Timeout`] for windows UIA's desktop listing left out and
/// that then did not answer a probe, `None` when there are none. The
/// elapsed time is the `FindAll`'s, the wait UIA spent on them.
fn dropped_windows_error(
    report: &ProbeReport,
    missing: usize,
    elapsed: std::time::Duration,
) -> Option<Error> {
    if report.unresponsive.is_empty() {
        return None;
    }
    Some(
        Error::timeout(elapsed).diagnose(
            xa11y_core::Diagnosis::new()
                .condition("every top-level window on the desktop to respond")
                .last_observed(format!(
                    "UIA's desktop listing returned after {elapsed:.1?} without {missing} \
                     interactive top-level window(s) that EnumWindows reports, which UIA does \
                     when it cannot reach a window within its timeouts; {}",
                    report.summary()
                ))
                .candidates(report.candidates()),
        ),
    )
}

/// The not-found error a pid lookup keeps when the probe's window scan
/// failed. The scan is best-effort enrichment and the not-found is real, so
/// the scan's failure is recorded in the diagnosis instead of replacing it
/// (tenet 1: nothing dropped, and callers matching `SelectorNotMatched` for
/// "app absent" still work).
fn not_found_after_failed_scan(pid: u32, scan: &Error) -> Error {
    Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
        xa11y_core::Diagnosis::new().last_observed(format!(
            "no top-level UIA element owned by the process; the check for an unresponsive \
             window of the process could not run: {scan}"
        )),
    )
}

/// The [`Error::Timeout`] for a pid lookup that found no element while some
/// of the pid's windows do not answer `WM_NULL`, `None` when `report` has no
/// unresponsive window (none owned, or all of them answer).
///
/// `owned` is how many interactive top-level windows the pid has. `waited` is
/// how long the lookup polled.
fn unanswered_pid_lookup_error(
    pid: u32,
    owned: usize,
    report: &ProbeReport,
    waited: std::time::Duration,
) -> Option<Error> {
    if report.unresponsive.is_empty() {
        return None;
    }
    Some(
        Error::timeout(waited).diagnose(
            xa11y_core::Diagnosis::new()
                .condition(format!("a top-level window of pid {pid}"))
                .last_observed(format!(
                    "UIA's search for the windows of pid {pid} found no element in {waited:.1?} \
                     although the process owns {owned} interactive top-level window(s), which \
                     UIA does when it cannot reach a window within its timeouts; {}",
                    report.summary()
                ))
                .candidates(report.candidates()),
        ),
    )
}

/// A window's title without sending it a message (`GetWindowTextW` reads
/// another process's caption from the window manager), so it is safe on a
/// hung window. Empty when untitled or unreadable; the HWND identifies it.
fn hwnd_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

/// Resolve every interactive top-level HWND of a process through UIA. The
/// returned set is native-window discovery, before UIA tree deduplication.
fn native_windows_of_pid_with(
    autom: &IUIAutomation,
    pid: u32,
    cache: &IUIAutomationCacheRequest,
) -> Result<Vec<IUIAutomationElement>> {
    unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> BOOL {
        let handles = unsafe { &mut *(param.0 as *mut Vec<HWND>) };
        handles.push(hwnd);
        true.into()
    }

    let mut all: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(collect), LPARAM((&raw mut all) as isize)) }.map_err(|e| {
        Error::Platform {
            code: e.code().0 as i64,
            message: format!("EnumWindows failed: {e}"),
        }
    })?;
    // App attribution, not raw Win32 ownership: a UWP frame belongs to the
    // app it shows, not to ApplicationFrameHost (issue #497). Non-frames are
    // decided by their native pid; frames are only examined in depth for a
    // packaged target.
    let mut filter = crate::uwp::PidFilter::for_pid(pid);
    let mut mine: Vec<HWND> = Vec::new();
    for hwnd in all {
        match filter.owns(hwnd) {
            Ok(true) => mine.push(hwnd),
            Ok(false) => {}
            // The window closed while it was being attributed.
            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => {}
            Err(e) => return Err(e),
        }
    }
    let mut out = Vec::with_capacity(mine.len());
    for hwnd in mine {
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            continue; // The window closed after EnumWindows visited it.
        }
        let interactive = match is_interactive_top_level(hwnd) {
            Ok(interactive) => interactive,
            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => continue,
            Err(e) => return Err(e),
        };
        if !interactive {
            continue;
        }
        // Reading a window Windows already considers hung blocks for the
        // transaction timeout and then fails anyway; fail now, naming it
        // (issue #507). A merely busy window goes on into UIA.
        if window_is_hung(hwnd) {
            return Err(hung_windows_error(
                format!("the top-level windows of pid {pid} to respond"),
                &[HungWindow { hwnd, pid }],
            ));
        }
        let (result, elapsed) =
            timed(|| uia_call(|| unsafe { autom.ElementFromHandleBuildCache(hwnd, cache) }));
        match result {
            Ok(el) => out.push(el),
            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => {}
            Err(e) => {
                return Err(uia_timeout_as_timeout(e, elapsed, || {
                    format!(
                        "HWND {:#x} \"{}\" of pid {pid} to answer UIA",
                        hwnd.0 as usize,
                        hwnd_title(hwnd)
                    )
                }))
            }
        }
    }
    Ok(out)
}

/// Keep only UIA roots that contribute a new subtree to an app-wide walk.
///
/// Qt demonstrates why this has to ask UIA rather than Win32: its File-menu
/// HWND has no owner, but UIA already parents it under the main window's File
/// item. Its combo popup is another desktop-level UIA window, but the popup's
/// direct child is parented beneath the main window's ComboBox. Both would
/// otherwise make app locators visit the same controls twice.
fn distinct_uia_roots(
    autom: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    candidates: Vec<IUIAutomationElement>,
) -> Result<Vec<IUIAutomationElement>> {
    let walker = uia_call(|| unsafe { autom.RawViewWalker() })?;
    let true_condition = uia_call(|| unsafe { autom.CreateTrueCondition() })?;
    let mut out = Vec::with_capacity(candidates.len());

    for (index, candidate) in candidates.iter().enumerate() {
        // Several HWNDs can resolve to one UIA element. Keep the first in
        // EnumWindows order rather than presenting the same root twice.
        let mut same_as_earlier = false;
        for earlier in candidates.iter().take(index) {
            if uia_call(|| unsafe { autom.CompareElements(candidate, earlier) })?.as_bool() {
                same_as_earlier = true;
                break;
            }
        }
        if same_as_earlier {
            continue;
        }

        if has_other_root_ancestor(autom, &walker, candidate, index, &candidates)? {
            continue;
        }

        // Some providers expose a popup HWND as a desktop child while its
        // contents' UIA parents lead back into the main window. If every
        // direct child is already under another candidate, the popup adds no
        // accessible content to an app walk. Keep empty roots: absence of
        // children is not evidence of duplication.
        let children = uia_call(|| unsafe {
            candidate.FindAllBuildCache(TreeScope_Children, &true_condition, cache)
        })?;
        let len = uia_call(|| unsafe { children.Length() })?;
        let mut all_covered = len > 0;
        for child_index in 0..len {
            let child = uia_call(|| unsafe { children.GetElement(child_index) })?;
            if !has_other_root_ancestor(autom, &walker, &child, index, &candidates)? {
                all_covered = false;
                break;
            }
        }
        if !all_covered {
            out.push(candidate.clone());
        }
    }
    Ok(out)
}

/// Whether UIA, rather than Win32 ownership, places `element` within one of
/// the other HWND roots. The traversal is bounded against a malformed provider
/// that cycles its parent chain.
fn has_other_root_ancestor(
    autom: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    own_index: usize,
    candidates: &[IUIAutomationElement],
) -> Result<bool> {
    let mut cursor = element.clone();
    for _ in 0..256 {
        let parent = match retry_transient(|| unsafe { walker.GetParentElement(&cursor) }) {
            Ok(parent) => parent,
            Err(e) if e.code().is_ok() => return Ok(false),
            Err(e) => {
                return Err(Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("RawViewWalker.GetParentElement failed: {e}"),
                });
            }
        };
        for (index, candidate) in candidates.iter().enumerate() {
            if index != own_index
                && uia_call(|| unsafe { autom.CompareElements(&parent, candidate) })?.as_bool()
            {
                return Ok(true);
            }
        }
        cursor = parent;
    }
    Err(Error::Platform {
        code: -1,
        message: "UIA parent chain exceeded 256 elements while resolving app roots".into(),
    })
}

/// Whether a top-level window can hold anything to interact with: visible,
/// not cloaked by DWM (e.g. a suspended UWP frame), with a non-empty frame,
/// and not click-through (layered and transparent together, as used by drop
/// shadows and overlays).
fn is_interactive_top_level(hwnd: HWND) -> Result<bool> {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return Ok(false);
    }
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let click_through = WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0;
    if ex_style & click_through == click_through {
        return Ok(false);
    }
    // Cloaking and the rect are both read inside one scope.
    let dpi = &crate::dpi::DpiScope::enter()?;
    let cloaked = crate::coords::is_cloaked(dpi, hwnd).map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("DwmGetWindowAttribute(DWMWA_CLOAKED) failed for {hwnd:?}: {e}"),
    })?;
    if cloaked {
        return Ok(false);
    }
    // Only emptiness is read here, but GetWindowRect is coordinate-bearing.
    let rect = crate::coords::window_rect(dpi, hwnd).map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("GetWindowRect failed for {hwnd:?}: {e}"),
    })?;
    Ok(rect.right > rect.left && rect.bottom > rect.top)
}

/// Enumerate a pid only while it still names the saved process generation.
///
/// Checking on both sides closes the useful PID-reuse race: a replacement
/// that appears before or during the PID-filtered UIA query is rejected, and
/// the returned COM elements are thereafter bound to the enumerated windows
/// rather than resolved again by PID.
fn top_level_windows_of_saved_app_with(
    autom: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    saved: SavedSyntheticAppIdentity,
) -> Result<Vec<IUIAutomationElement>> {
    windows_of_saved_app_with(autom, cache, saved, true)
}

fn windows_of_saved_app_with(
    autom: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    saved: SavedSyntheticAppIdentity,
    distinct_roots: bool,
) -> Result<Vec<IUIAutomationElement>> {
    validate_synthetic_app_identity(
        saved.handle,
        saved.identity,
        process_creation_time(saved.identity.pid),
    )?;
    let windows = if distinct_roots {
        top_level_windows_of_pid_with(autom, saved.identity.pid, cache)?
    } else {
        native_windows_of_pid_with(autom, saved.identity.pid, cache)?
    };
    validate_synthetic_app_identity(
        saved.handle,
        saved.identity,
        process_creation_time(saved.identity.pid),
    )?;
    Ok(windows)
}

/// The name and `uia_name_source` of a process's Application node: the
/// packaged app's shell display name when it has one, else the executable's
/// file stem (issue #497).
fn process_app_name(path: &str, display: Option<String>) -> (String, &'static str) {
    match display {
        Some(display) => (display, "package"),
        None => {
            let stem = std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned());
            (stem, "process")
        }
    }
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
///
/// The process's AppUserModelID is read through the same handle (see
/// [`crate::uwp::handle_aumid`]), so a process that exits between the two
/// reads cannot turn into an error for one and a name for the other.
fn process_image_name(pid: u32) -> Option<(String, Result<Option<String>>)> {
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
    let aumid = if ok {
        crate::uwp::handle_aumid(handle, pid)
    } else {
        Ok(None)
    };
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
    Some((path, aumid))
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

/// Verify that a saved synthetic app still names the same process generation.
///
/// Failure to read either side is a stale result, not permission to proceed:
/// a pid with no verifiable generation can be reused, and enumerating it would
/// silently retarget the saved `App` to an unrelated process.
fn validate_synthetic_app_identity(
    handle: u64,
    identity: SyntheticAppIdentity,
    current: Option<u64>,
) -> Result<()> {
    match (identity.creation_time, current) {
        (Some(stored), Some(current)) if stored == current => Ok(()),
        (Some(_), Some(_)) => Err(Error::stale(
            xa11y_core::StaleReason::ProcessRestarted,
            format!(
                "handle:{handle} (pid {}); the process exited and Windows reused its pid, \
                 so this Application node is stale",
                identity.pid
            ),
        )),
        (None, _) => Err(Error::stale(
            xa11y_core::StaleReason::ProcessRestarted,
            format!(
                "handle:{handle} (pid {}); the process identity could not be captured when \
                 this Application node was created, so resolving the pid would be unsafe",
                identity.pid
            ),
        )),
        (Some(_), None) => Err(Error::stale(
            xa11y_core::StaleReason::ProcessRestarted,
            format!(
                "handle:{handle} (pid {}); the current process identity could not be verified, \
                 so resolving the pid would be unsafe",
                identity.pid
            ),
        )),
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

/// Document semantics from an element's cached properties (#476).
struct UiaSemantics {
    heading_level: Option<u32>,
    landmark_type: Option<xa11y_core::Landmark>,
    web_role: Option<String>,
    link_url: Option<String>,
    live_region: Option<xa11y_core::LiveRegionInfo>,
}

/// The landmark a UIA `LandmarkType` id names. A custom landmark's kind is
/// in its ARIA role or, failing that, its localized name ("banner",
/// "content information").
fn landmark_from_type_id(
    element: &IUIAutomationElement,
    id: i32,
    web_role: Option<&str>,
) -> xa11y_core::Landmark {
    use xa11y_core::Landmark;
    match id {
        id if id == UIA_FormLandmarkTypeId.0 => Landmark::Form,
        id if id == UIA_MainLandmarkTypeId.0 => Landmark::Main,
        id if id == UIA_NavigationLandmarkTypeId.0 => Landmark::Navigation,
        id if id == UIA_SearchLandmarkTypeId.0 => Landmark::Search,
        // The ARIA role is not localized, so it is authoritative; the
        // localized name only matches English, and is the last resort.
        _ => web_role
            .and_then(Landmark::from_aria_role)
            .or_else(|| {
                uia_cached_bstr(element, UIA_LocalizedLandmarkTypePropertyId)
                    .as_deref()
                    .and_then(|name| {
                        if name.eq_ignore_ascii_case("content information") {
                            Some(Landmark::ContentInfo)
                        } else {
                            Landmark::from_aria_role(name)
                        }
                    })
            })
            .unwrap_or(Landmark::Unknown),
    }
}

fn uia_semantics(
    element: &IUIAutomationElement,
    role: Role,
    landmark_type_id: Option<i32>,
    value: Option<&str>,
) -> UiaSemantics {
    let web_role = uia_cached_bstr(element, UIA_AriaRolePropertyId);
    // `HeadingLevel1` .. `HeadingLevel9`; `HeadingLevel_None` and anything
    // else is no level.
    let heading_level = uia_cached_i32(element, UIA_HeadingLevelPropertyId)
        .map(|id| id - (HeadingLevel1.0 - 1))
        .filter(|level| (1..=9).contains(level))
        .map(|level| level as u32);
    let landmark_type = match landmark_type_id {
        Some(id) => Some(landmark_from_type_id(element, id, web_role.as_deref())),
        // No UIA LandmarkType: a landmark the ARIA role alone marked.
        None if role == Role::Landmark => Some(
            web_role
                .as_deref()
                .and_then(xa11y_core::Landmark::from_aria_role)
                .unwrap_or(xa11y_core::Landmark::Unknown),
        ),
        None => None,
    };
    let aria = uia_cached_bstr(element, UIA_AriaPropertiesPropertyId);
    let aria_prop = |key: &str| {
        aria.as_deref().and_then(|props| {
            props.split(';').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                (k.trim() == key).then(|| v.trim().to_string())
            })
        })
    };
    // UIA reports `Off` (0) for every element that is not a live region, so
    // only polite / assertive mark one.
    let politeness = match uia_cached_i32(element, UIA_LiveSettingPropertyId) {
        Some(1) => Some(xa11y_core::Politeness::Polite),
        Some(2) => Some(xa11y_core::Politeness::Assertive),
        _ => None,
    };
    let live = politeness.map(|politeness| {
        xa11y_core::LiveRegionParts {
            politeness: Some(politeness),
            atomic: aria_prop("atomic").and_then(|v| match v.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
            relevant: aria_prop("relevant").filter(|v| !v.is_empty()),
        }
        .into()
    });
    UiaSemantics {
        heading_level,
        landmark_type,
        web_role,
        // UIA has no URL property; browsers put a link's URL in its value.
        link_url: (role == Role::Link)
            .then(|| value.map(str::to_string))
            .flatten(),
        live_region: live,
    }
}

/// Build an ElementData snapshot from a pre-fetched UIA element without
/// retaining the live reference in the provider's handle cache.
///
/// Used both by [`WindowsProvider::build_element_data`] (which allocates a
/// handle and wraps this call) and by event handlers (which pass `handle=0`
/// because event targets are snapshots — callers don't act on them directly).
///
/// `hidden_by_ancestor`: something above the element hides it although UIA
/// may leave `IsOffscreen` false, and why: a minimized window (issue #501)
/// or a scrolling ancestor it is scrolled out of (issue #502).
fn build_snapshot_data(
    dpi: &crate::dpi::DpiScope,
    element: &IUIAutomationElement,
    pid: Option<u32>,
    handle: u64,
    walker: Option<&IUIAutomationTreeWalker>,
    hidden_by_ancestor: Option<HiddenReason>,
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
                // A landmark that sets only its ARIA role (AccessKit, and
                // frameworks without a UIA LandmarkType) is still a landmark
                // (#476); `uia_semantics` reads its kind from the same role.
                other
                    if matches!(role, Role::Group | Role::Unknown)
                        && xa11y_core::Landmark::from_aria_role(other).is_some() =>
                {
                    role = Role::Landmark;
                }
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
    // A landmark is a group with a UIA LandmarkType (#476); its kind is
    // read below.
    let landmark_type_id = uia_cached_i32(element, UIA_LandmarkTypePropertyId)
        .filter(|id| (UIA_CustomLandmarkTypeId.0..=UIA_SearchLandmarkTypeId.0).contains(id));
    if landmark_type_id.is_some() && matches!(role, Role::Group | Role::Unknown) {
        role = Role::Landmark;
    }

    let name = unsafe { element.CachedName() }
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    let patterns = WindowsProvider::query_patterns(role, element)?;
    // `IsPassword` is UIA's own secure-text flag (`ES_PASSWORD` edits,
    // WPF `PasswordBox`, AccessKit `PasswordInput`). The control type stays
    // Edit, so the role stays `TextField`. An unread property degrades to
    // `false` like the other cached boolean reads. Read before the value,
    // which a password control may decline to disclose (see
    // `value_pattern_text`).
    let password = unsafe { element.CachedIsPassword() }.unwrap_or(FALSE) == TRUE;
    let site = ReadSite {
        role,
        name: name.as_deref(),
    };
    let (value, observed_range_value) = get_value(site, &patterns, password)?;
    let semantics = uia_semantics(element, role, landmark_type_id, value.as_deref());

    // Try FullDescription first (AccessKit's description), then HelpText
    let description = uia_cached_bstr(element, UIA_FullDescriptionPropertyId)
        .or_else(|| uia_cached_bstr(element, UIA_HelpTextPropertyId));

    let states = parse_states(element, site, &patterns, password, hidden_by_ancestor)?;

    let bounds = crate::coords::cached_bounding_rect(dpi, element)
        .ok()
        .and_then(|r| {
            let width = (r.right - r.left).max(0) as u32;
            let height = (r.bottom - r.top).max(0) as u32;
            if width == 0 && height == 0 {
                None
            } else {
                // Per-Monitor-V2 UIA bounds are physical desktop pixels.
                Some(crate::dpi::physical_rect_to_desktop(r))
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
        // UIA's own answer, before the minimized-window and scroll-clip
        // corrections `visible` applies (issues #501, #502), so a consumer
        // can tell which one hid an element. Absent when the provider did
        // not supply it, like the other optional entries here.
        if let Ok(offscreen) = unsafe { element.CachedIsOffscreen() } {
            raw.insert(
                "uia_is_offscreen".into(),
                serde_json::Value::Bool(offscreen.as_bool()),
            );
        }
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
        if let Some(framework) = uia_cached_bstr(element, UIA_FrameworkIdPropertyId) {
            raw.insert(
                "uia_framework_id".into(),
                serde_json::Value::String(framework),
            );
        }
        // Which UIA activation patterns `press` could dispatch to, so a
        // caller (and the integ suite) can tell which mechanism a `press`
        // advertisement stands for.
        let activation: Vec<serde_json::Value> = [
            ("invoke", patterns.invoke.is_some()),
            ("toggle", patterns.toggle.is_some()),
            ("selection_item", patterns.selection_item.is_some()),
            ("expand_collapse", patterns.expand_collapse.is_some()),
        ]
        .into_iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| serde_json::Value::String(name.into()))
        .collect();
        raw.insert(
            "uia_activation_patterns".into(),
            serde_json::Value::Array(activation),
        );
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
            let site = ReadSite {
                role,
                name: name.as_deref(),
            };
            (
                // Reuse the value already read for `value` so the string and
                // numeric fields describe the same observation.
                observed_range_value,
                Some(pattern_read(
                    site,
                    "RangeValuePattern.CurrentMinimum",
                    || unsafe { pattern.CurrentMinimum() },
                )?),
                Some(pattern_read(
                    site,
                    "RangeValuePattern.CurrentMaximum",
                    || unsafe { pattern.CurrentMaximum() },
                )?),
            )
        } else {
            (None, None, None)
        }
    } else {
        (None, None, None)
    };

    Ok(ElementParts {
        heading_level: semantics.heading_level,
        landmark_type: semantics.landmark_type,
        web_role: semantics.web_role,
        link_url: semantics.link_url,
        live_region: semantics.live_region,
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
        table: rich::table_info(element)?,
        cell: rich::cell_info(element)?,
        // UIA has no row-index property independent of GridItem.
        table_row: rich::row_info(element, role)?,
        collection: rich::collection_info(element)?,
        stable_id,
        pid,
        raw,
        handle,
    }
    .into())
}

/// Build the small request behind the scroll-clip refresh and walk: bounds
/// and the two overflow flags, in the raw view like the batch request.
fn create_scroll_request(automation: &IUIAutomation) -> Result<crate::coords::BoundsCacheRequest> {
    let request = uia_call(|| unsafe { automation.CreateCacheRequest() })?;
    for prop in SCROLL_PROPERTIES {
        unsafe { request.AddProperty(*prop) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddProperty({:?}) failed: {e}", prop),
        })?;
    }
    let raw_view = uia_call(|| unsafe { automation.CreateTrueCondition() })?;
    uia_call(|| unsafe { request.SetTreeFilter(&raw_view) })?;
    crate::coords::BoundsCacheRequest::new(request)
}

/// [`BATCH_PROPERTIES`], for the coords test that bounds are not among them.
#[cfg(test)]
pub(crate) fn batch_properties() -> &'static [UIA_PROPERTY_ID] {
    BATCH_PROPERTIES
}

/// [`SCROLL_PROPERTIES`], for the same coords test.
#[cfg(test)]
pub(crate) fn scroll_properties() -> &'static [UIA_PROPERTY_ID] {
    SCROLL_PROPERTIES
}

/// Build the batch request that describes which properties and patterns
/// to pre-fetch. Created once per provider, used on every query.
fn create_batch_request(automation: &IUIAutomation) -> Result<crate::coords::BoundsCacheRequest> {
    create_cache_request(automation, BATCH_PROPERTIES)
}

/// The cache request event handlers fetch their targets with.
fn create_event_cache_request(
    automation: &IUIAutomation,
) -> Result<crate::coords::BoundsCacheRequest> {
    create_cache_request(automation, &event_cache_properties())
}

/// Event senders expose the same snapshot properties as bulk reads.
fn event_cache_properties() -> Vec<UIA_PROPERTY_ID> {
    BATCH_PROPERTIES.to_vec()
}

fn create_cache_request(
    automation: &IUIAutomation,
    properties: &[UIA_PROPERTY_ID],
) -> Result<crate::coords::BoundsCacheRequest> {
    let request = uia_call(|| unsafe { automation.CreateCacheRequest() })?;

    // The property list is a fixed constant array of valid UIA property IDs.
    // If AddProperty ever fails here, something is structurally wrong with the
    // UIA environment (e.g. COM state corrupted). Propagate rather than
    // silently producing a half-configured cache request (tenet 1).
    for prop in properties {
        unsafe { request.AddProperty(*prop) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddProperty({:?}) failed: {e}", prop),
        })?;
    }
    for pattern in BATCH_PATTERNS {
        unsafe { request.AddPattern(*pattern) }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("AddPattern({pattern:?}) failed: {e}"),
        })?;
    }

    // Use raw view (TrueCondition) so FindAllBuildCache sees all UIA elements,
    // including virtual/fragment elements from Qt, AccessKit, etc. that don't
    // set IsControlElement=true and are silently excluded by the default
    // Control View tree filter.
    let raw_view = uia_call(|| unsafe { automation.CreateTrueCondition() })?;
    uia_call(|| unsafe { request.SetTreeFilter(&raw_view) })?;

    crate::coords::BoundsCacheRequest::new(request)
}

/// Properties pre-fetched in every bulk query, besides the bounds that
/// [`crate::coords::BoundsCacheRequest`] adds.
const BATCH_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_GridRowCountPropertyId,
    UIA_GridColumnCountPropertyId,
    UIA_GridItemRowPropertyId,
    UIA_GridItemColumnPropertyId,
    UIA_GridItemRowSpanPropertyId,
    UIA_GridItemColumnSpanPropertyId,
    UIA_PositionInSetPropertyId,
    UIA_SizeOfSetPropertyId,
    UIA_LevelPropertyId,
    UIA_ControlTypePropertyId,
    UIA_AriaRolePropertyId,
    // Document semantics (#476): heading level, landmark, live region and
    // the ARIA properties carrying atomic / relevant.
    UIA_HeadingLevelPropertyId,
    UIA_LandmarkTypePropertyId,
    UIA_LocalizedLandmarkTypePropertyId,
    UIA_LiveSettingPropertyId,
    UIA_AriaPropertiesPropertyId,
    UIA_OrientationPropertyId,
    UIA_IsDialogPropertyId,
    UIA_IsTableItemPatternAvailablePropertyId,
    UIA_NamePropertyId,
    UIA_FullDescriptionPropertyId,
    UIA_HelpTextPropertyId,
    UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
    UIA_ProcessIdPropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    // Read by `parse_states` for the `password` state (#522).
    UIA_IsPasswordPropertyId,
    UIA_NativeWindowHandlePropertyId,
    // MSAA state bitmask — the only place pre-SelectionItem frameworks report
    // selection (see the `selected` derivation in `parse_states`).
    UIA_LegacyIAccessibleStatePropertyId,
    // MSAA role — the only role signal for providers that publish no UIA
    // control type (see `map_msaa_role`).
    UIA_LegacyIAccessibleRolePropertyId,
    // MSAA default action — advertises `press` on elements whose only
    // activation is LegacyIAccessible.DoDefaultAction (see `get_actions`).
    UIA_LegacyIAccessibleDefaultActionPropertyId,
    // Which provider framework serves the element. Gates the MSAA default
    // action as `press` to MSAA-proxied elements (see
    // `legacy_default_action_is_activation`).
    UIA_FrameworkIdPropertyId,
    // Scroll overflow: an element whose content overflows clips its
    // descendants to its bounds (see `cached_viewport`, issue #502).
    UIA_ScrollHorizontallyScrollablePropertyId,
    UIA_ScrollVerticallyScrollablePropertyId,
];

/// Properties [`create_scroll_request`] fetches: what
/// [`WindowsProvider::children_clip_of`] needs to re-check one element,
/// besides the bounds that [`crate::coords::BoundsCacheRequest`] adds.
const SCROLL_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    // For `resets_clip`: a popup's ancestors do not clip it.
    UIA_NativeWindowHandlePropertyId,
    UIA_ScrollHorizontallyScrollablePropertyId,
    UIA_ScrollVerticallyScrollablePropertyId,
];

/// Interfaces needed while turning a cached UIA element into ElementData.
/// Adding them to the bulk request avoids a live pattern acquisition for
/// every node in a subtree query.
const BATCH_PATTERNS: &[UIA_PATTERN_ID] = &[
    UIA_InvokePatternId,
    UIA_TogglePatternId,
    UIA_ExpandCollapsePatternId,
    UIA_ValuePatternId,
    UIA_RangeValuePatternId,
    UIA_SelectionItemPatternId,
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

/// Map a failed `ValuePattern.SetValue`. UIA reports a read-only control
/// with `UIA_E_INVALIDOPERATION`, which is the one failure that means "this
/// value cannot be set as text". Anything else (a disabled control, a dead
/// element, a wedged provider) is a platform error with its HRESULT
/// (tenet 1).
fn value_set_error(e: windows::core::Error, what: &str) -> Error {
    if e.code().0 == UIA_E_INVALIDOPERATION as i32 {
        Error::TextValueNotSupported
    } else {
        Error::Platform {
            code: e.code().0 as i64,
            message: format!("{what} failed: {e}"),
        }
    }
}

/// Read a property of a pattern the element already has, mapping a failure
/// to `Error::Platform` with the HRESULT and what was being read.
///
/// The element has the pattern, so a failed read is not "unsupported": it is
/// a dead element or a wedged provider, and substituting a default (`Off`,
/// `false`, read-only, `None`) would report a state nobody observed
/// (tenet 1, issue #597). Transient failures are retried first, like every
/// other snapshot read. `UIA_E_ELEMENTNOTAVAILABLE` keeps its code, so a
/// tree walk still prunes an element that vanished mid-snapshot
/// (see [`live_snapshot`]).
///
/// A read with a documented "no value" signal matches it at the call site
/// instead (see [`value_pattern_text`]).
fn pattern_read<T>(
    site: ReadSite<'_>,
    what: &str,
    f: impl Fn() -> windows::core::Result<T>,
) -> Result<T> {
    retry_transient(f).map_err(|e| pattern_read_error(site, what, &e))
}

/// The element a snapshot read is for, named in a failed read's error so it
/// says which element failed without a re-run (tenet 6). Borrowed, and only
/// formatted on the failure path.
#[derive(Clone, Copy)]
struct ReadSite<'a> {
    role: Role,
    name: Option<&'a str>,
}

impl std::fmt::Display for ReadSite<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.role.to_snake_case())?;
        match self.name {
            Some(name) => write!(f, " {name:?}"),
            None => Ok(()),
        }
    }
}

/// The error [`pattern_read`] reports for a failed read of `what`.
fn pattern_read_error(site: ReadSite<'_>, what: &str, e: &windows::core::Error) -> Error {
    Error::Platform {
        code: e.code().0 as i64,
        message: format!("reading {what} on {site} failed: {e}"),
    }
}

/// Upper bound on the raw-view walk in [`WindowsProvider::walk_to_host_hwnd`].
const HOST_WINDOW_WALK_LIMIT: usize = 256;

/// Where the `host_hwnd` passed to `build_element_data_in` came from.
#[derive(Clone, Copy)]
enum HostHint {
    /// The element's parent: its nearest native window is exactly this one
    /// unless the element has its own, so it is recorded for its children.
    Parent,
    /// A search root further up. Good enough for this snapshot's
    /// minimized-window check, but a child HWND may sit in between, so only
    /// the element's own HWND is recorded.
    Ancestor,
}

/// Whether `hwnd`, or any window it is a child of, is minimized.
///
/// UIA keeps `IsOffscreen = false` on the descendants of a minimized window,
/// so the windows' own iconic state is the only signal that the content
/// cannot be seen (issue #501). The walk follows the parent chain
/// (`GA_PARENT`) to the top-level window and checks each level, so a
/// minimized MDI child hides its contents too.
///
/// Ownership is deliberately not followed: an owned window (a dialog, a tool
/// palette) is a top-level window of its own, and Windows hides it separately
/// when its owner is minimized, which `IsOffscreen` already reports.
fn hwnd_in_minimized_window(hwnd: HWND) -> bool {
    let mut current = hwnd;
    // Bounded against a malformed chain; real nesting is a handful deep.
    for _ in 0..64 {
        if unsafe { IsIconic(current) }.as_bool() {
            return true;
        }
        let parent = unsafe { GetAncestor(current, GA_PARENT) };
        // A top-level window's parent is the desktop window; null means an
        // invalid HWND. Either way there is nothing above to check.
        if parent.0.is_null() || parent == unsafe { GetDesktopWindow() } {
            return false;
        }
        current = parent;
    }
    false
}

/// One overflowing ancestor that contributes to a [`Clip`], kept so the clip
/// can be re-checked against it later.
#[derive(Clone)]
struct ClipSource {
    element: IUIAutomationElement,
    /// Its viewport when the clip was computed.
    viewport: RECT,
}

/// The scroll clip an element sits under: the intersection of the viewports
/// of its overflowing ancestors, up to the nearest popup or top-level window
/// (see [`resets_clip`]).
#[derive(Clone, Default)]
struct Clip {
    /// The intersection, `None` when no ancestor overflows.
    rect: Option<RECT>,
    /// The ancestors it came from, outermost first. Usually zero or one, so
    /// a copy per element costs an AddRef or two.
    sources: Vec<ClipSource>,
}

impl Clip {
    /// The clip `element`'s children sit under, when `self` is the one
    /// `element` sits under: narrowed by `element`'s own viewport if its
    /// content overflows.
    fn below(&self, dpi: &crate::dpi::DpiScope, element: &IUIAutomationElement) -> Clip {
        let Some(viewport) = cached_viewport(dpi, element) else {
            return self.clone();
        };
        let mut sources = Vec::with_capacity(self.sources.len() + 1);
        sources.extend(self.sources.iter().cloned());
        sources.push(ClipSource {
            element: element.clone(),
            viewport,
        });
        Clip {
            rect: intersect_clip(self.rect, Some(viewport)),
            sources,
        }
    }
}

/// Apply element `j` of a subtree array to the clips of itself and its `m`
/// descendants (the entries after it): a popup starts them afresh, and an
/// overflowing element narrows its descendants' to its viewport.
fn apply_range(
    dpi: &crate::dpi::DpiScope,
    clips: &mut [Clip],
    element: &IUIAutomationElement,
    j: usize,
    m: usize,
    is_popup: bool,
    overflows: bool,
) {
    if is_popup {
        for clip in &mut clips[j..=j + m] {
            *clip = Clip::default();
        }
    }
    if overflows {
        for clip in &mut clips[j + 1..=j + m] {
            *clip = clip.below(dpi, element);
        }
    }
}

/// The clip an element was built under, as [`WindowsProvider::clips`]
/// remembers it.
#[derive(Clone)]
struct ClipRecord {
    /// The element's physical bounds when it was built. A different value
    /// later means it moved, so `inherited` may be stale. Empty when it had
    /// none, which never counts as unchanged.
    bounds: RECT,
    /// The clip it was built under.
    inherited: Clip,
}

/// Whether `rect` is the empty rectangle UIA reports for an element with no
/// bounds (or the default this provider reads a failed bounds read as).
fn no_bounds(rect: &RECT) -> bool {
    rect.right == rect.left && rect.bottom == rect.top
}

/// The element's own native window, from its cached properties.
fn cached_own_hwnd(element: &IUIAutomationElement) -> Option<HWND> {
    unsafe { element.CachedNativeWindowHandle() }
        .ok()
        .filter(|h| !h.0.is_null())
}

/// Whether an element with native window `hwnd` starts a clip of its own.
///
/// A popup (`WS_POPUP`: a drop-down list, a menu) or top-level window is
/// drawn above its owner's content, so it can extend past a scrolling
/// ancestor's viewport and still be seen. UIA still places an open ComboBox's
/// drop-down under the combo in the raw view, which is why the ancestors'
/// viewports stop applying here rather than at the next top-level element.
fn resets_clip(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    if style & WS_POPUP.0 != 0 {
        return true;
    }
    let parent = unsafe { GetAncestor(hwnd, GA_PARENT) };
    parent.0.is_null() || parent == unsafe { GetDesktopWindow() }
}

/// The viewport `element` clips its descendants to, from its cached
/// properties: its bounds, when the UIA Scroll pattern reports its content
/// as scrollable in either direction.
///
/// UIA has no viewport rectangle, so the bounds stand in. They include any
/// scroll bars and header, which can leave an item hidden behind those
/// counted as visible; an item scrolled fully out of view is what this
/// catches (issue #502). An element that does not overflow clips nothing:
/// its content fits, so no descendant can be scrolled out of it. Nor does an
/// overflowing element without bounds of its own, which UIA already reports
/// off screen along with its content. A custom scroller that implements no
/// Scroll pattern is invisible to this.
fn cached_viewport(dpi: &crate::dpi::DpiScope, element: &IUIAutomationElement) -> Option<RECT> {
    let overflows = uia_cached_bool(element, UIA_ScrollVerticallyScrollablePropertyId)
        == Some(true)
        || uia_cached_bool(element, UIA_ScrollHorizontallyScrollablePropertyId) == Some(true);
    if !overflows {
        return None;
    }
    crate::coords::cached_bounding_rect(dpi, element)
        .ok()
        .filter(|r| r.right > r.left && r.bottom > r.top)
}

/// The intersection of two clips, where `None` clips nothing. Disjoint
/// viewports give an inverted rectangle, which hides everything under it
/// (see [`clipped_out`]).
fn intersect_clip(a: Option<RECT>, b: Option<RECT>) -> Option<RECT> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => Some(RECT {
            left: a.left.max(b.left),
            top: a.top.max(b.top),
            right: a.right.min(b.right),
            bottom: a.bottom.min(b.bottom),
        }),
    }
}

/// Acquire a live pattern, mapping "the element has no such pattern" to
/// `Ok(None)` and every other failure to `Error::Platform` (tenet 1).
fn current_pattern<T: windows::core::Interface>(
    element: &IUIAutomationElement,
    id: UIA_PATTERN_ID,
    what: &str,
) -> Result<Option<T>> {
    match unsafe { element.GetCurrentPatternAs::<T>(id) } {
        Ok(pattern) => Ok(Some(pattern)),
        Err(e) if is_pattern_absent(&e) => Ok(None),
        Err(e) => Err(Error::Platform {
            code: e.code().0 as i64,
            message: format!("acquiring {what} failed: {e}"),
        }),
    }
}

/// Whether `clip` leaves no part of `bounds` showing.
///
/// An element with real size shows when it overlaps the viewport by some
/// area, so one only partly inside counts as showing and one merely touching
/// the edge does not. A zero-width or zero-height element (a separator line)
/// has no area to overlap, so for it the edges are closed: it shows when it
/// lies on or within the viewport. An element with no bounds at all is not
/// judged: there is nothing to place, and its `IsOffscreen` stands.
fn clipped_out(bounds: RECT, clip: Option<RECT>) -> bool {
    let Some(c) = clip else { return false };
    if c.right < c.left || c.bottom < c.top {
        // Disjoint ancestor viewports: nothing below them can show.
        return true;
    }
    if no_bounds(&bounds) {
        return false;
    }
    let sized = bounds.right > bounds.left && bounds.bottom > bounds.top;
    let overlaps = if sized {
        bounds.left < c.right
            && c.left < bounds.right
            && bounds.top < c.bottom
            && c.top < bounds.bottom
    } else {
        bounds.left <= c.right
            && c.left <= bounds.right
            && bounds.top <= c.bottom
            && c.top <= bounds.bottom
    };
    !overlaps
}

/// Whether `LegacyIAccessible.DoDefaultAction` is this element's `press`.
///
/// Only when it is the *sole* activation of an element exposed through MSAA:
/// the element has none of Invoke, Toggle, SelectionItem, or ExpandCollapse,
/// MSAA reports a non-empty default action, and the element is served by the
/// MSAA side of UIA (see [`is_msaa_framework`]). An element with a UIA
/// activation pattern keeps that pattern as its `press`; the default action is
/// never a second try after it (issue #505, tenet 1). A native UIA provider
/// that withheld every activation pattern did so on purpose (Chromium's
/// "click ancestor" default action is not this element's activation), so its
/// default action is not promoted either.
///
/// Advertisement (`get_actions`) and dispatch (`press`) both call this, so
/// they cannot disagree.
fn legacy_default_action_is_activation(
    has_activation_pattern: bool,
    default_action: Option<&str>,
    framework_id: Option<&str>,
) -> bool {
    !has_activation_pattern
        && is_msaa_framework(framework_id)
        && default_action.is_some_and(|a| !a.trim().is_empty())
}

/// Whether a UIA `FrameworkId` names an element exposed through MSAA rather
/// than a native UIA provider: the Win32 MSAA proxy (`"Win32"`), WinForms
/// accessible objects (`"WinForm"`), or a proxied element that reports none.
/// Native providers name themselves (`"Chrome"`, `"XAML"`, `"WPF"`, `"Qt"`,
/// `"DirectUI"`, …) and are excluded.
fn is_msaa_framework(framework_id: Option<&str>) -> bool {
    matches!(
        framework_id.map(str::trim),
        None | Some("" | "Win32" | "WinForm")
    )
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

/// How many times `activate` calls `SetForegroundWindow` before giving up.
const FOREGROUND_ATTEMPTS: u32 = 3;
/// How long each attempt waits for `GetForegroundWindow` to report the target.
const FOREGROUND_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
const FOREGROUND_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Bring `hwnd` to the foreground and verify that it got there.
///
/// `SetForegroundWindow` is subject to the foreground lock: Windows can refuse
/// it outright (returns FALSE), or accept the call and only flash the taskbar
/// button. The return value alone therefore proves nothing, so success is
/// `GetForegroundWindow() == hwnd`, polled for a short settle period because a
/// cross-thread foreground switch lands asynchronously.
///
/// A refused or unverified attempt is retried with the same call, a bounded
/// number of times. That is a retry, not a fallback to another mechanism
/// (tenet 1), and there is deliberately no simulated-input trick such as the
/// Alt-key tap that unlocks the foreground (tenet 2). When every attempt fails
/// the error says what was in front instead and whether the target is hung,
/// so the caller can tell a foreground-lock refusal from a frozen app.
fn bring_to_foreground(hwnd: HWND) -> Result<()> {
    let mut refused = 0u32;
    for _ in 0..FOREGROUND_ATTEMPTS {
        if !unsafe { SetForegroundWindow(hwnd) }.as_bool() {
            refused += 1;
        }
        let deadline = std::time::Instant::now() + FOREGROUND_SETTLE;
        loop {
            if unsafe { GetForegroundWindow() } == hwnd {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(FOREGROUND_POLL);
        }
    }

    let foreground = unsafe { GetForegroundWindow() };
    let in_front = if foreground.0.is_null() {
        "no window (the foreground is between windows or on the secure desktop)".to_string()
    } else {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
        format!(
            "HWND {:#x} \"{}\" (pid {pid})",
            foreground.0 as usize,
            window_text(foreground)
        )
    };
    let hung = if unsafe { IsHungAppWindow(hwnd) }.as_bool() {
        "; the target window is not responding (IsHungAppWindow)"
    } else {
        ""
    };
    Err(Error::Platform {
        code: -1,
        message: format!(
            "activate: HWND {:#x} \"{}\" did not become the foreground window after {} \
             SetForegroundWindow attempts ({refused} refused outright, the rest accepted but \
             not applied, most likely the foreground lock); in front instead: {in_front}{hung}",
            hwnd.0 as usize,
            window_text(hwnd),
            FOREGROUND_ATTEMPTS,
        ),
    })
}

/// A window's title for an error message; empty when it has none or the read fails.
fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    // A failed read (0) is indistinguishable from an untitled window here, and
    // either way the HWND printed alongside identifies it.
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

/// Locate the caret within a control's TextPattern, as a character offset
/// from the start of the document.
///
/// Returns:
/// - `Ok(Some(n))` if TextPattern reports a selection whose start lies `n`
///   characters into the document. For a collapsed caret, `n` is the caret
///   position. For a non-empty selection, `n` is the selection's start —
///   mirroring macOS/AT-SPI semantics of "insert at selection start".
/// - `Ok(None)` if the control has no TextPattern, its selection array is
///   empty (no caret available), or its provider does not support reading
///   the selection. The caller should fall back to "append at
///   end" — the behaviour documented in design/README.md.
/// - `Err(..)` if TextPattern is present but a COM call to walk the range
///   fails. These are propagated rather than silently falling back, so
///   genuine platform errors surface (tenet 1).
fn caret_char_offset(uia_element: &IUIAutomationElement) -> Result<Option<usize>> {
    let Some(text_pattern) =
        current_pattern::<IUIAutomationTextPattern>(uia_element, UIA_TextPatternId, "TextPattern")?
    else {
        return Ok(None); // TextPattern not supported on this control.
    };

    // A control with no selection answers with an empty array, handled
    // below. A provider that does not support text selection says so with
    // UIA_E_INVALIDOPERATION, or (AccessKit) with a null array and S_OK:
    // also "no caret". Every other failure propagates (tenet 1).
    let selection = match unsafe { text_pattern.GetSelection() } {
        Ok(s) => s,
        Err(e) if e.code().0 == UIA_E_INVALIDOPERATION as i32 || e.code().is_ok() => {
            return Ok(None)
        }
        Err(e) => {
            return Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("TextPattern::GetSelection failed: {e}"),
            })
        }
    };

    let selected = unsafe { selection.Length() }.map_err(|e| Error::Platform {
        code: e.code().0 as i64,
        message: format!("TextRangeArray::Length failed: {e}"),
    })?;
    if selected == 0 {
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

impl WindowsProvider {
    fn synthetic_app_windows(
        &self,
        dpi: &crate::dpi::DpiScope,
        app: &ElementData,
        identity: SyntheticAppIdentity,
        distinct_roots: bool,
    ) -> Result<Vec<ElementData>> {
        let windows = windows_of_saved_app_with(
            &self.automation,
            self.batch_request.get(dpi),
            SavedSyntheticAppIdentity {
                handle: app.handle,
                identity,
            },
            distinct_roots,
        )?;
        let mut data = Vec::with_capacity(windows.len());
        for el in windows {
            // The enumerated window may close during any live snapshot read.
            // A vanished candidate contributes no subtree; all other failures,
            // including process-generation mismatches, still abort enumeration.
            if let Some(window) = live_snapshot(self.build_app_window(dpi, &el, app, identity))? {
                data.push(window);
            }
        }
        Ok(data)
    }

    fn build_app_window(
        &self,
        dpi: &crate::dpi::DpiScope,
        el: &IUIAutomationElement,
        app: &ElementData,
        identity: SyntheticAppIdentity,
    ) -> Result<ElementData> {
        // Strict path: re-acquire via HWND to activate
        // AccessKit's provider, then populate the snapshot
        // that build_element_data reads. A failure here is a
        // real COM failure — the fallback would silently hand
        // back a window whose provider was never activated
        // (tenet 1).
        let el = self.reacquire_via_hwnd(el).map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("re-acquiring a top-level window via HWND failed: {e}"),
        })?;
        let reacquired_pid = unsafe { el.CurrentProcessId() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!(
                "reading the process id of a re-acquired top-level window failed: {e}"
            ),
        })? as u32;
        let belongs_to_app = if reacquired_pid == identity.pid {
            true
        } else {
            let hwnd = uia_call(|| unsafe { el.CurrentNativeWindowHandle() })?;
            // A valid UWP frame still reports ApplicationFrameHost's UIA
            // process id. Revalidate its current app attribution instead of
            // mistaking that expected host/app split for HWND reuse.
            !hwnd.0.is_null()
                && crate::uwp::is_frame(hwnd)
                && crate::uwp::PidFilter::for_pid(identity.pid).owns(hwnd)?
        };
        if !belongs_to_app {
            return Err(Error::stale(
                xa11y_core::StaleReason::IdentityChanged,
                format!(
                    "handle:{} (pid {}); top-level window HWND was reused by pid {reacquired_pid}",
                    app.handle, identity.pid
                ),
            ));
        }
        // Re-check the process generation after resolving the
        // numeric HWND. HWNDs are reusable too; together these
        // checks ensure ElementFromHandle cannot redirect this
        // saved App to another process or process generation.
        validate_synthetic_app_identity(app.handle, identity, process_creation_time(identity.pid))?;
        let el = self.populate_cache(dpi, &el).map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("populating the re-acquired top-level window cache failed: {e}"),
        })?;
        let mut window_data = self.build_element_data(dpi, &el, Some(identity.pid))?;
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
                            "CurrentName failed while listing a top-level window of pid {}: {e}",
                            identity.pid
                        ),
                    });
                }
            };
        }
        Ok(window_data)
    }
}

// Every method enters a `DpiScope`. The ones that read or produce
// coordinates (bounds in snapshots, window geometry, input points) need it,
// because Windows answers those in the calling thread's DPI space (issue
// #506); the compiler enforces that through the `&DpiScope` parameters in
// coords.rs. The methods bound to `_dpi` touch no coordinates, and their
// scope is a defensive habit, not a requirement: it keeps a coordinate call
// added to one of them later from needing a scope threaded in first.
/// Refuse to start a UIA operation once the caller's deadline has passed
/// (#531), naming it as the pending call.
///
/// UIA has no per-call timeout: its connection and transaction timeouts
/// belong to the shared `IUIAutomation` object, so lowering them for one
/// caller would cut every other thread's calls short too. What a deadline
/// can bound here is when an operation *starts*; one already in flight is
/// bounded by [`UIA_CONNECTION_TIMEOUT_MS`] and
/// [`UIA_TRANSACTION_TIMEOUT_MS`], as before.
fn uia_gate(operation: &str) -> Result<()> {
    xa11y_core::call_timeout(std::time::Duration::MAX, || format!("UIA {operation}")).map(|_| ())
}

impl Provider for WindowsProvider {
    fn identity(&self, element: &ElementData) -> Result<xa11y_core::NodeIdentity> {
        uia_gate("identity")?;
        if let Some(app) = self.synthetic_app_identity_checked(element.handle)? {
            return Ok(xa11y_core::NodeIdentity::from_key(
                "uia",
                format!("{}:{:?}", app.pid, app.creation_time),
                "application",
            ));
        }
        let uia = self.get_cached(element.handle)?;
        let rid = Self::runtime_id(&uia, element.handle).map_err(|e| match e {
            Error::Platform { code, .. } if is_gone_code(code) => Error::stale(
                xa11y_core::StaleReason::Removed,
                xa11y_core::error::stale_subject(element),
            ),
            other => other,
        })?;
        if rid.is_empty() {
            return Err(Error::Unsupported {
                feature: format!(
                    "identity of {}: its UIA provider reports no RuntimeId",
                    xa11y_core::error::stale_subject(element)
                ),
            });
        }
        // The pid alone can be reused; its creation time names the process
        // instance. A process that cannot be opened keeps `None`, which still
        // compares consistently within this session.
        let process = element.pid.map_or_else(
            || "-".to_string(),
            |pid| format!("{pid}:{:?}", process_creation_time(pid)),
        );
        let key = rid
            .iter()
            .map(|part| part.to_string())
            .collect::<Vec<_>>()
            .join(".");
        Ok(xa11y_core::NodeIdentity::from_key("uia", process, key))
    }

    fn revalidate(&self, element: &ElementData) -> Result<()> {
        uia_gate("revalidate")?;
        if let Some(app) = self.synthetic_app_identity_checked(element.handle)? {
            return validate_synthetic_app_identity(
                element.handle,
                app,
                process_creation_time(app.pid),
            );
        }
        let uia = self.get_cached(element.handle)?;
        let subject = xa11y_core::error::stale_subject(element);
        let gone = |code: i64| -> Error {
            let exited = element
                .pid
                .is_some_and(|pid| process_creation_time(pid).is_none());
            let reason = if exited {
                xa11y_core::StaleReason::ProcessExited
            } else {
                xa11y_core::StaleReason::Removed
            };
            Error::stale(reason, format!("{subject} (HRESULT {code:#x})"))
        };
        let live = |result: Result<i32>| -> Result<i32> {
            result.map_err(|e| match e {
                Error::Platform { code, .. } if is_gone_code(code) => gone(code),
                other => other,
            })
        };
        // `ElementData::pid` is the owning application's, which for a
        // hosted window (a UWP frame) is not the element's own process, so
        // it is not compared: the COM element is bound to its node, and the
        // control type below catches a reused id.
        // The role guard compares UIA's own control type, which the snapshot
        // stored as `control_type_id`, so a mapping refinement (ARIA role,
        // DataItem cells) cannot make a live node look replaced. Partial
        // data with no control type carries nothing to compare.
        let control_type = live(uia_call(|| unsafe { uia.CurrentControlType() }).map(|c| c.0))?;
        match element.raw.get("control_type_id").and_then(|v| v.as_i64()) {
            Some(captured) if captured != i64::from(control_type) => Err(Error::stale(
                xa11y_core::StaleReason::IdentityChanged,
                format!(
                    "{subject}; the element now reports UIA control type {control_type}, \
                     not {captured}"
                ),
            )),
            _ => Ok(()),
        }
    }

    fn app_windows(&self, app: &ElementData) -> Result<Vec<ElementData>> {
        uia_gate("app_windows")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
        if let Some(identity) = self.synthetic_app_identity_checked(app.handle)? {
            return self.synthetic_app_windows(dpi, app, identity, false);
        }
        self.get_children(Some(app))
    }

    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        uia_gate("get_children")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
        match element {
            None => {
                // Top-level: enumerate the desktop root's named top-level
                // windows, then group them by pid — one synthesized
                // Application node per process, in first-seen (z-) order,
                // with that pid's first window as the representative. The old
                // code returned one entry per window (issue #304's shape);
                // grouping now reports one Application node per process on
                // every platform, and the windows are the node's children.
                let (windows, _unlisted) = self.desktop_app_windows(dpi)?;

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
                if let Some(identity) = self.synthetic_app_identity_checked(element_data.handle)? {
                    return self.synthetic_app_windows(dpi, element_data, identity, true);
                }
                let uia = self.get_cached(element_data.handle)?;
                let children = self.uia_children(dpi, &uia);
                if children.is_empty() {
                    return Ok(Vec::new());
                }
                // The parent's nearest window, remembered when it was built,
                // so the whole level costs one lookup. A parent that
                // vanished during a fallback walk has no children to report.
                let Some(host_hwnd) = live_snapshot(self.host_hwnd_of(element_data.handle, &uia))?
                else {
                    return Ok(Vec::new());
                };
                // Likewise the scroll clip: one refresh of the parent.
                let Some(clip) =
                    live_snapshot(self.children_clip_of(dpi, element_data.handle, &uia))?
                else {
                    return Ok(Vec::new());
                };
                let pid = element_data.pid;
                let mut data = Vec::with_capacity(children.len());
                for child in children {
                    if let Some(child) = live_snapshot(self.build_element_data_in(
                        dpi,
                        &child,
                        pid,
                        host_hwnd,
                        HostHint::Parent,
                        &clip,
                    ))? {
                        data.push(child);
                    }
                }
                Ok(data)
            }
        }
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        uia_gate("get_parent")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
        // A synthesized Application node is top-level by construction — no
        // element has the process as a child. The walk needs a live element
        // the node does not have, so answer `None` directly instead of
        // routing a synthetic handle through `get_cached` into the
        // "unsupported" error.
        if is_synthetic_handle(element.handle) {
            return Ok(None);
        }
        let uia = self.get_cached(element.handle)?;
        // `GetParentElement` reports "no parent" as S_OK with a null
        // element, which windows-rs surfaces as an error whose code is
        // S_OK. Only that is "no parent"; a failed HRESULT propagates
        // (tenet 1).
        let parent_of = |el: &IUIAutomationElement, what: &str| match unsafe {
            self.raw_walker.GetParentElement(el)
        } {
            Ok(parent) => Ok(Some(parent)),
            Err(e) if e.code().is_ok() => Ok(None),
            Err(e) => Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("GetParentElement of {what} failed: {e}"),
            }),
        };
        let Some(parent) = parent_of(&uia, "the element")? else {
            return Ok(None);
        };
        // A parent with no parent of its own is the desktop root.
        if parent_of(&parent, "the element's parent")?.is_none() {
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
        let parent = self
            .populate_cache(dpi, &parent)
            .map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("BuildUpdatedCache failed: {}", e),
            })?;
        // A windowless child shares its parent's nearest window (an
        // HWND-bearing parent uses its own anyway), so the child's
        // remembered answer serves; a child with its own HWND, or one
        // not built here, needs the walk.
        let child_has_own_hwnd = element
            .stable_id
            .as_deref()
            .is_some_and(|id| id.starts_with("hwnd:"));
        let host_hwnd = if child_has_own_hwnd {
            self.walk_to_host_hwnd(&parent)?
        } else {
            self.host_hwnd_of(element.handle, &uia)?
        };
        let clip = self.parent_clip(dpi, element.handle, &uia, &parent)?;
        let data = self.build_element_data_in(
            dpi,
            &parent,
            element.pid,
            host_hwnd,
            HostHint::Parent,
            &clip,
        )?;
        Ok(Some(data))
    }

    // Chromium builds its UIA tree on its own once a UI Automation client
    // connects, so there is no switch to set.
    fn enable_web_accessibility(&self, _app: &ElementData) -> Result<()> {
        uia_gate("enable_web_accessibility")?;
        Err(Error::Unsupported {
            feature: "enable_web_accessibility on Windows: Chromium and Electron build \
                their web accessibility tree on their own when a UI Automation client \
                connects, so there is no switch to set. If web content is still missing, \
                relaunch the app with `--force-renderer-accessibility`."
                .to_string(),
        })
    }

    /// Enumerate the Windows shell surfaces by classifying the UIA desktop
    /// root's **direct** children by class name.
    ///
    /// [`get_children(None)`](Self::get_children) filters desktop children to
    /// `ControlType.Window`, which hides the shell panes and native popup-menu
    /// hosts. This scan walks the desktop root's direct **Raw View** children
    /// and keeps only the classes it recognises. Anything else — ordinary app
    /// windows and their panes — is skipped: it is not a shell surface, and it
    /// is already reachable through `list_apps`.
    ///
    /// | Class name | Kind |
    /// |---|---|
    /// | `Shell_TrayWnd` | [`Taskbar`](ShellSurfaceKind::Taskbar) — task band, visible tray row, overflow chevron |
    /// | `Progman` | [`Desktop`](ShellSurfaceKind::Desktop) — descended to its `SysListView32` icon list view; **no surface** if that is missing |
    /// | `TopLevelWindowForOverflowXamlIsland` | [`Flyout`](ShellSurfaceKind::Flyout) — the tray overflow |
    /// | `Microsoft.UI.Content.PopupWindowSiteBridge` | [`Flyout`](ShellSurfaceKind::Flyout) — a shell popup |
    /// | `ControlCenterWindow` | [`Flyout`](ShellSurfaceKind::Flyout) — Quick Settings, **only while its content is on screen** |
    /// | `#32768` | [`Flyout`](ShellSurfaceKind::Flyout) — a native Win32 popup menu, while open |
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
        uia_gate("list_shell_surfaces")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
        let root = uia_call(|| unsafe { self.automation.GetRootElement() })?;
        let found = self.raw_children_with_cache(dpi, &root)?;

        // (rank, kind, data) — rank groups the output; the stable sort below
        // keeps enumeration order within a group.
        let mut surfaces: Vec<(u8, ShellSurfaceKind, ElementData)> = Vec::new();

        for el in found {
            let class_name = uia_cached_bstr(&el, UIA_ClassNamePropertyId).unwrap_or_default();
            let control_type = uia_cached_i32(&el, UIA_ControlTypePropertyId);

            let (rank, kind) = match class_name.as_str() {
                "Shell_TrayWnd" => (0u8, ShellSurfaceKind::Taskbar),
                "Progman" => (1u8, ShellSurfaceKind::Desktop),
                // The tray overflow host is a desktop-root child only while
                // the flyout is open, so its presence is the open signal.
                // The XAML popup site bridge behaves the same way.
                "TopLevelWindowForOverflowXamlIsland"
                | "Microsoft.UI.Content.PopupWindowSiteBridge"
                // Win32 creates a top-level window of the system menu class
                // for TrackPopupMenu-style menus. It exists only while the
                // menu is open and exposes its commands as UIA menu items.
                // Native notification-area menus use this path rather than
                // either of the XAML popup hosts above.
                | "#32768" => (2u8, ShellSurfaceKind::Flyout),
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
                    let on_screen = self.has_onscreen_child(dpi, &el).map_err(|e| {
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
                // Windows 11 shell controls do not consistently use the
                // Win32 system-menu class. UIA's top-level Menu control type
                // is the stable accessibility signal for those popup hosts;
                // direct Raw View scope keeps ordinary in-app menus out.
                _ if control_type == Some(UIA_MenuControlTypeId.0) => {
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
                match self.descendant_by_class(dpi, &el, DESKTOP_LIST_VIEW_CLASS)? {
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
            // Mirror build_app_window: re-acquire via HWND so the window's
            // UIA provider is activated, then repopulate the snapshot that
            // build_element_data reads. A failure is a real COM failure;
            // falling back to the un-reacquired element would hand back a
            // surface whose provider was never activated (tenet 1).
            let el = self.reacquire_via_hwnd(&el).map_err(|e| Error::Platform {
                code: i64::from(e.code().0),
                message: format!("re-acquiring the {kind} shell surface via HWND failed: {e}"),
            })?;
            let el = self.populate_cache(dpi, &el).map_err(|e| Error::Platform {
                code: i64::from(e.code().0),
                message: format!(
                    "populating the re-acquired {kind} shell surface cache failed: {e}"
                ),
            })?;
            let data = self.build_element_data(dpi, &el, (pid != 0).then_some(pid))?;
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
        uia_gate("list_apps")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        self.get_children(None)
    }

    /// UWP frames whose app could not be found: `list_apps` leaves them out
    /// because they belong to no running process (issue #497). The same
    /// enumeration as `list_apps`, so the two sets match.
    fn unlisted_apps(&self) -> Result<Vec<String>> {
        uia_gate("unlisted_apps")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
        Ok(self.desktop_app_windows(dpi)?.1)
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
        uia_gate("app_by_pid")?;
        // A hung window of this pid would stall the UIA search below for the
        // transaction timeout; fail fast naming it instead (issue #507).
        let hung: Vec<HungWindow> = hung_top_level_windows()?
            .into_iter()
            .filter(|w| w.pid == pid)
            .collect();
        if !hung.is_empty() {
            return Err(hung_windows_error(
                format!("the top-level windows of pid {pid} to respond"),
                &hung,
            ));
        }
        let dpi = &crate::dpi::DpiScope::enter()?;
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
        let (found, elapsed) = timed(|| unsafe {
            root.FindFirstBuildCache(TreeScope_Children, &condition, self.batch_request.get(dpi))
        });
        let el = match found {
            Ok(el) => el,
            Err(e) if e.code().is_ok() => {
                // A UWP app owns no top-level window of its own: its frame
                // belongs to ApplicationFrameHost and only the hosted core
                // window is the app's (issue #497). That frame is the app's
                // window, found by attribution rather than by ProcessId.
                let mut frame = None;
                // An exited pid falls through to "not matched" with the
                // unpackaged ones; only a packaged one can have a frame.
                if matches!(
                    crate::uwp::PidFilter::for_pid(pid).packaging()?,
                    crate::uwp::Packaging::Packaged(_)
                ) {
                    for el in native_windows_of_pid_with(
                        &self.automation,
                        pid,
                        self.batch_request.get(dpi),
                    )? {
                        let hwnd = unsafe { el.CachedNativeWindowHandle() }.map_err(|e| {
                            Error::Platform {
                                code: e.code().0 as i64,
                                message: format!(
                                    "reading the window handle of a window attributed to \
                                         pid {pid} failed: {e}"
                                ),
                            }
                        })?;
                        if !hwnd.0.is_null() && crate::uwp::is_frame(hwnd) {
                            frame = Some(el);
                            break;
                        }
                    }
                }
                match frame {
                    Some(frame) => frame,
                    None => {
                        return Err(
                            Error::selector_not_matched(format!("application[pid={pid}]"))
                                .diagnose(xa11y_core::Diagnosis::new().last_observed(
                                "no top-level UIA element owned by the process yet, and no UWP \
                             frame hosting it",
                            )),
                        );
                    }
                }
            }
            Err(e) => {
                return Err(uia_timeout_as_timeout(
                    Error::Platform {
                        code: e.code().0 as i64,
                        message: format!("FindFirstBuildCache(ProcessId={pid}) failed: {e}"),
                    },
                    elapsed,
                    || format!("a top-level window of pid {pid}"),
                ));
            }
        };
        // The representative window serves only the fallback name source of
        // the synthesized node (process path first); no snapshot is needed,
        // and the node deliberately keeps no live element (tenet 2 — no
        // window-shaped stand-in).
        self.build_synthetic_app_data(pid, &el)
    }

    /// Name a window that stopped answering when the pid lookup gave up
    /// (issue #615).
    ///
    /// A window that stopped pumping less than [`OS_HUNG_THRESHOLD`] ago passes
    /// the `IsHungAppWindow` check in `app_by_pid`, and UIA answers a search
    /// that reaches it with S_OK and no element. That is also what an app still
    /// starting up looks like, so `app_by_pid` keeps reporting it as not found
    /// while the caller polls and the probe runs only here, once, after the
    /// loop gave up. No windows, or windows that all answer, keep the not-found
    /// error.
    fn app_by_pid_failure(&self, pid: u32, waited: std::time::Duration) -> Option<Error> {
        let windows =
            match interactive_top_level_windows_owned_by("probing the windows of a pid", Some(pid))
            {
                Ok(windows) => windows,
                Err(scan) => return Some(not_found_after_failed_scan(pid, &scan)),
            };
        if windows.is_empty() {
            return None;
        }
        let report = probe_windows(&windows, "top-level", std::time::Instant::now());
        unanswered_pid_lookup_error(pid, windows.len(), &report, waited)
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
        uia_gate("focused_app")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return Err(Error::selector_not_matched("focused application"));
        }
        // The pid comes from the window manager, which answers even when the
        // app cannot.
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // A window whose owner the window manager cannot name (an honest "no
        // owning process", tenet 1 — not "no foreground app").
        if pid == 0 {
            return Err(Error::Platform {
                code: -1,
                message: "foreground window returns pid 0 (no owning process)".to_string(),
            });
        }
        // `App::list` / `App::find` tag the foreground app through here, so a
        // hung foreground app must fail fast rather than stall them in UIA.
        if window_is_hung(hwnd) {
            return Err(hung_windows_error(
                "the foreground window to respond".to_string(),
                &[HungWindow { hwnd, pid }],
            ));
        }
        let (el, elapsed) =
            timed(|| uia_call(|| unsafe { self.automation.ElementFromHandle(hwnd) }));
        let el = el.map_err(|e| {
            uia_timeout_as_timeout(e, elapsed, || {
                format!(
                    "the foreground window HWND {:#x} \"{}\" (pid {pid}) to answer UIA",
                    hwnd.0 as usize,
                    hwnd_title(hwnd)
                )
            })
        })?;
        let pid = match crate::uwp::Attribution::new().owner(hwnd) {
            Ok(crate::uwp::WindowOwner::Process(pid)) => pid,
            Ok(crate::uwp::WindowOwner::Unresolved { note }) => {
                return Err(Error::selector_not_matched("focused application")
                    .diagnose(xa11y_core::Diagnosis::new().last_observed(note)));
            }
            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => {
                return Err(Error::selector_not_matched("focused application"));
            }
            Err(error) => return Err(error),
        };
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
        uia_gate("narrow_multi_segment")?;
        xa11y_core::selector::validate_tail_segments(segments)?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        // Dedup below is by handle; the canonical walk makes that a dedup by
        // UIA identity (issue #571).
        let walk = CanonicalWalk::new(self);
        for segment in segments {
            let mut next_candidates = Vec::new();
            for candidate in &candidates {
                match segment.combinator {
                    Combinator::Child => {
                        let children = walk.children(Some(candidate))?;
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
                            |el| walk.children(el),
                            Some(candidate),
                            &sub_selector,
                            None,
                            Some(max_depth),
                        )?;
                        next_candidates.append(&mut sub_results);
                    }
                    // `validate_tail_segments` above rejects a Root here.
                    Combinator::Root => unreachable!("validated: no Root in tail segments"),
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
        uia_gate("find_elements_group")?;
        // The AST's fields are public, so a hand-built group can break the
        // invariants the walk below indexes on (`clause.segments[0]`, Root
        // only first, 1-based `:nth`). Reject it before traversal.
        group.validate()?;
        let dpi = &crate::dpi::DpiScope::enter()?;

        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        // A synthesized Application node has no live UIA element, so the UIA
        // subtree query below cannot be scoped to it. Answer with the
        // level-by-level walk instead, which goes through `get_children`:
        // the synthetic node's children are the process's top-level windows,
        // and each is walked natively. Same shape as the fragment-element
        // fallback further down — the walk is the honest primitive for a
        // root that is not a UIA HWND fragment.
        if is_synthetic_handle(root.handle) {
            let walk = CanonicalWalk::new(self);
            return xa11y_core::selector::find_elements_in_tree_group(
                |el| walk.children(el),
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
        let root_hwnd = unsafe { uia_root.CurrentNativeWindowHandle() }
            .ok()
            .filter(|h| !h.0.is_null());
        let is_hwnd_root = root_hwnd.is_some();
        if !is_hwnd_root {
            let walk = CanonicalWalk::new(self);
            return xa11y_core::selector::find_elements_in_tree_group(
                |el| walk.children(el),
                Some(root),
                group,
                limit,
                max_depth,
            );
        }

        // One COM call fetches the whole subtree in doc order.
        let Some(subtree) = live_snapshot(self.find_all_subtree(dpi, &uia_root))? else {
            return Ok(Vec::new());
        };
        let count = uia_len(&subtree);

        // Windowless elements below inherit the root's window, which is only
        // right while no nested window under it is minimized (an MDI child):
        // the flat array does not say which nested window an element sits
        // in. When one is, take the level-by-level path, which tracks each
        // element's nearest window (issue #501).
        //
        // Scroll clipping (issue #502) needs the same thing, which elements
        // sit inside which overflowing one, and gets it without leaving the
        // single pass: see `subtree_clips`.
        let root_minimized = root_hwnd.is_some_and(hwnd_in_minimized_window);
        let nested_minimized = !root_minimized
            && (0..count).any(|i| {
                uia_get(&subtree, i)
                    .and_then(|el| unsafe { el.CachedNativeWindowHandle() }.ok())
                    .is_some_and(|h| !h.0.is_null() && hwnd_in_minimized_window(h))
            });
        let clips = if nested_minimized {
            None
        } else {
            let Some(clips) = live_snapshot(self.subtree_clips(
                dpi,
                root_data.handle,
                &uia_root,
                &subtree,
                count,
            ))?
            else {
                return Ok(Vec::new());
            };
            clips
        };
        let Some(clips) = clips else {
            return xa11y_core::selector::find_elements_in_tree_group(
                |el| self.get_children(el),
                Some(root),
                group,
                limit,
                max_depth,
            );
        };

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
            // Windowless elements inherit the root's window for this snapshot
            // only (`Ancestor`: a child HWND may sit between them and the
            // root, so it is not recorded); elements with an HWND of their
            // own use and record it.
            let Some(data) = live_snapshot(self.build_element_data_in(
                dpi,
                &el,
                pid,
                root_hwnd,
                HostHint::Ancestor,
                &clips[i as usize],
            ))?
            else {
                continue;
            };

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
        // (or matched by multiple clauses) collapse to the first one found
        // (issue #571). Every narrowing above minted its own handles, so the
        // handle cannot be the key.
        merged.sort_by_key(|(pos, _)| *pos);
        let mut seen: HashSet<Vec<i32>> = HashSet::new();
        let mut out: Vec<ElementData> = Vec::with_capacity(merged.len());
        for (_, data) in merged {
            let element = self.get_cached(data.handle)?;
            // An element that vanished since the walk is left out, as the
            // walk itself does.
            let Some(id) = live_snapshot(Self::runtime_id(&element, data.handle))? else {
                continue;
            };
            // No RuntimeId means no identity to dedup on: keep it.
            if id.is_empty() || seen.insert(id) {
                out.push(data);
            }
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
    }

    #[allow(non_upper_case_globals)]
    fn press(&self, element: &ElementData) -> Result<()> {
        uia_gate("press")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        //
        // Each lookup distinguishes "pattern absent" (try the next) from a
        // real COM failure (propagate): a wedged provider must not fall
        // through to a different activation (tenet 1).
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationInvokePattern>(
            &uia_element,
            UIA_InvokePatternId,
            "InvokePattern",
        )? {
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
        if let Some(pattern) = current_pattern::<IUIAutomationTogglePattern>(
            &uia_element,
            UIA_TogglePatternId,
            "TogglePattern",
        )? {
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
        if let Some(pattern) = current_pattern::<IUIAutomationSelectionItemPattern>(
            &uia_element,
            UIA_SelectionItemPatternId,
            "SelectionItemPattern",
        )? {
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
        if let Some(pattern) = current_pattern::<IUIAutomationExpandCollapsePattern>(
            &uia_element,
            UIA_ExpandCollapsePatternId,
            "ExpandCollapsePattern",
        )? {
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
        // None of the four activation patterns exists. For an element
        // exposed only through MSAA, the MSAA default action *is* its primary
        // activation, so `press` dispatches to it (issue #505). Reached only
        // when every lookup above reported the pattern absent — never after
        // one of them failed (tenet 1).
        if let Some(pattern) = current_pattern::<IUIAutomationLegacyIAccessiblePattern>(
            &uia_element,
            UIA_LegacyIAccessiblePatternId,
            "LegacyIAccessiblePattern",
        )? {
            let default_action =
                unsafe { pattern.CurrentDefaultAction() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("LegacyIAccessible.CurrentDefaultAction failed: {e}"),
                })?;
            let framework_id =
                unsafe { uia_element.CurrentFrameworkId() }.map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("CurrentFrameworkId failed: {e}"),
                })?;
            if legacy_default_action_is_activation(
                false,
                Some(&default_action.to_string()),
                Some(&framework_id.to_string()),
            ) {
                unsafe { pattern.DoDefaultAction() }.or_else(|e| {
                    if is_event_subscriber_failure(&e) {
                        Ok(())
                    } else {
                        Err(Error::Platform {
                            code: e.code().0 as i64,
                            message: format!(
                                "LegacyIAccessible.DoDefaultAction (\"{default_action}\") \
                                 failed: {e}"
                            ),
                        })
                    }
                })?;
                return Ok(());
            }
        }
        Err(Error::ActionNotSupported {
            action: "press".to_string(),
            role: element.role,
        })
    }

    fn focus(&self, element: &ElementData) -> Result<()> {
        uia_gate("focus")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        unsafe { uia_element.SetFocus() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: "SetFocus failed".to_string(),
        })?;
        Ok(())
    }

    fn blur(&self, _element: &ElementData) -> Result<()> {
        uia_gate("blur")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        uia_gate("toggle")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationTogglePattern>(
            &uia_element,
            UIA_TogglePatternId,
            "TogglePattern",
        )? {
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
        uia_gate("select")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationSelectionItemPattern>(
            &uia_element,
            UIA_SelectionItemPatternId,
            "SelectionItemPattern",
        )? {
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
        uia_gate("expand")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationExpandCollapsePattern>(
            &uia_element,
            UIA_ExpandCollapsePatternId,
            "ExpandCollapsePattern",
        )? {
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
        uia_gate("collapse")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationExpandCollapsePattern>(
            &uia_element,
            UIA_ExpandCollapsePatternId,
            "ExpandCollapsePattern",
        )? {
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
        uia_gate("show_menu")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        uia_gate("increment")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationRangeValuePattern>(
            &uia_element,
            UIA_RangeValuePatternId,
            "RangeValuePattern",
        )? {
            let current = unsafe { pattern.CurrentValue() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentValue failed: {}", e),
            })?;
            let small = unsafe { pattern.CurrentSmallChange() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentSmallChange failed: {e}"),
            })?;
            // UIA's step is SmallChange. A provider reporting zero or less
            // (a continuous range has no natural step) would make the call
            // a no-op, so one unit stands in. Core promises "one step", not
            // a size.
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
        uia_gate("decrement")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationRangeValuePattern>(
            &uia_element,
            UIA_RangeValuePatternId,
            "RangeValuePattern",
        )? {
            let current = unsafe { pattern.CurrentValue() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentValue failed: {}", e),
            })?;
            let small = unsafe { pattern.CurrentSmallChange() }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("RangeValue.CurrentSmallChange failed: {e}"),
            })?;
            // UIA's step is SmallChange. A provider reporting zero or less
            // (a continuous range has no natural step) would make the call
            // a no-op, so one unit stands in. Core promises "one step", not
            // a size.
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
        uia_gate("scroll_into_view")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        // Only a genuinely absent ScrollItemPattern is "not supported"; any
        // other lookup failure (a dead element, a wedged provider) is a real
        // error and propagates rather than reading as unsupported (tenet 1).
        let pattern = unsafe {
            uia_element
                .GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
        }
        .map_err(|e| pattern_acquisition_error(&e, "scroll_into_view", element.role))?;
        unsafe { pattern.ScrollIntoView() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("ScrollIntoView failed: {}", e),
        })
    }

    // ── Window management ──────────────────────────────────────────
    //
    // Window verbs go through UIA's WindowPattern / TransformPattern — the
    // canonical accessibility interfaces for window state and geometry. No
    // input simulation is involved (tenet 2).

    fn activate(&self, element: &ElementData) -> Result<()> {
        uia_gate("activate")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        bring_to_foreground(hwnd)?;
        unsafe { uia.SetFocus() }.map_err(|e| Error::Platform {
            code: e.code().0 as i64,
            message: format!("SetFocus during activate failed: {e}"),
        })?;
        Ok(())
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        uia_gate("minimize")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        uia_gate("maximize")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        uia_gate("enter_fullscreen")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia = self.get_cached(element.handle)?;
        // Same target contract as the sibling verbs: a non-window target is
        // rejected as an unsupported action on that element, before the
        // platform-level answer.
        ensure_top_level_window_target(&uia, "enter_fullscreen", element.role)?;
        // UIA has no fullscreen surface at all: `WindowVisualState` has no
        // fullscreen value and no other pattern exposes one. Substituting
        // `Maximized` would be a different operation (see `maximize`), and
        // sizing the HWND by hand is not an accessibility call (tenet 2), so
        // the verb is platform-unsupported on a real window — the same split
        // the Linux backend makes (tenet 1: no silent fallback).
        Err(Error::Unsupported {
            feature: format!(
                "enter_fullscreen on {}: UIA has no API to enter fullscreen \
                 (WindowVisualState has no fullscreen value)",
                element.role.to_snake_case()
            ),
        })
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        uia_gate("restore")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        uia_gate("close")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
        uia_gate("move_to")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
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
        // Windows desktop coordinates match TransformPattern's pixels.
        crate::coords::transform_move(dpi, &pattern, f64::from(x), f64::from(y)).map_err(|e| {
            Error::Platform {
                code: e.code().0 as i64,
                message: format!("TransformPattern.Move({x}, {y}) failed: {e}"),
            }
        })?;
        Ok(())
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        uia_gate("resize_to")?;
        let dpi = &crate::dpi::DpiScope::enter()?;
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
        // Width and height use the same physical desktop units as bounds.
        crate::coords::transform_resize(dpi, &pattern, f64::from(width), f64::from(height))
            .map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("TransformPattern.Resize({width}, {height}) failed: {e}"),
            })?;
        Ok(())
    }

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        uia_gate("set_value")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        if let Some(pattern) = current_pattern::<IUIAutomationValuePattern>(
            &uia_element,
            UIA_ValuePatternId,
            "ValuePattern",
        )? {
            let s: windows::core::BSTR = value.into();
            unsafe { pattern.SetValue(&s) }
                .map_err(|e| value_set_error(e, "ValuePattern.SetValue"))?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        uia_gate("set_numeric_value")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        let pattern = current_pattern::<IUIAutomationRangeValuePattern>(
            &uia_element,
            UIA_RangeValuePatternId,
            "RangeValuePattern",
        )?
        .ok_or_else(|| Error::ActionNotSupported {
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
        uia_gate("type_text")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
        let uia_element = self.get_cached(element.handle)?;
        // Insert text via ValuePattern (accessibility API, not input simulation).
        // Get current value, get insertion point from TextPattern, splice, set new value.
        if let Some(value_pattern) = current_pattern::<IUIAutomationValuePattern>(
            &uia_element,
            UIA_ValuePatternId,
            "ValuePattern",
        )? {
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
            unsafe { value_pattern.SetValue(&bstr) }
                .map_err(|e| value_set_error(e, "ValuePattern.SetValue"))?;
            return Ok(());
        }
        Err(Error::TextValueNotSupported)
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        uia_gate("set_text_selection")?;
        // Name the action the caller asked for, not the text read under it.
        let doc = self.document(element).map_err(|e| match e {
            Error::ActionNotSupported { role, .. } => Error::ActionNotSupported {
                action: "set_text_selection".to_string(),
                role,
            },
            e => e,
        })?;
        let range = rich::range(&doc, xa11y_core::text_api::TextRange { start, end })?;
        uia_call(|| unsafe { range.Select() })
    }

    fn table_cell_at(&self, e: &ElementData, row: u32, column: u32) -> Result<Option<ElementData>> {
        uia_gate("table_cell_at")?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let p: IUIAutomationGridPattern = self.rich_pattern(e, UIA_GridPatternId, "cell_at")?;
        let rows = uia_call(|| unsafe { p.CurrentRowCount() })?;
        let columns = uia_call(|| unsafe { p.CurrentColumnCount() })?;
        let (Ok(row), Ok(column)) = (i32::try_from(row), i32::try_from(column)) else {
            return Ok(None);
        };
        if (rows >= 0 && row >= rows) || (columns >= 0 && column >= columns) {
            return Ok(None);
        }
        match unsafe { p.GetItem(row, column) } {
            Ok(item) => self.rich_element(&dpi, &item).map(Some),
            Err(e)
                if e.code() == E_INVALIDARG
                    || e.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32
                    || e.code().0 == 0 =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Platform {
                code: e.code().0 as i64,
                message: format!("Grid.GetItem({row}, {column}) failed: {e}"),
            }),
        }
    }

    fn relations(
        &self,
        e: &ElementData,
        kinds: &[xa11y_core::RelationKind],
    ) -> Result<Vec<(xa11y_core::RelationKind, Vec<ElementData>)>> {
        uia_gate("relations")?;
        use xa11y_core::RelationKind as K;
        // Refuse an unsupported kind before any COM call, naming it.
        for kind in kinds {
            if !matches!(
                kind,
                K::LabelledBy | K::Details | K::Controls | K::FlowsTo | K::FlowsFrom
            ) {
                return Err(Error::Unsupported {
                    feature: format!(
                        "relation {kind} on UIA: UIA reports labelled_by (LabeledBy), details \
                         (DescribedBy), controls (ControllerFor), flows_to and flows_from only"
                    ),
                });
            }
        }
        let dpi = crate::dpi::DpiScope::enter()?;
        let uia = self.get_cached(e.handle)?;
        let mut out = Vec::with_capacity(kinds.len());
        for kind in kinds {
            let targets = match kind {
                K::LabelledBy => match uia_call(|| unsafe { uia.CurrentLabeledBy() }) {
                    Ok(label) => vec![self.rich_element(&dpi, &label)?],
                    // No label: UIA answers a null element, which the
                    // binding surfaces as an error with a success-class
                    // code. Anything else is a real failure.
                    Err(Error::Platform { code: 0, .. }) => Vec::new(),
                    Err(err) => return Err(err),
                },
                K::Details => {
                    let array = uia_call(|| unsafe { uia.CurrentDescribedBy() })?;
                    self.rich_elements(&dpi, &array)?
                }
                K::Controls => {
                    let array = uia_call(|| unsafe { uia.CurrentControllerFor() })?;
                    self.rich_elements(&dpi, &array)?
                }
                K::FlowsTo => {
                    let array = uia_call(|| unsafe { uia.CurrentFlowsTo() })?;
                    self.rich_elements(&dpi, &array)?
                }
                K::FlowsFrom => {
                    let element2: IUIAutomationElement2 = windows::core::Interface::cast(&uia)
                        .map_err(|err| Error::Unsupported {
                            feature: format!(
                                "relation flows_from: this UIA element has no \
                                 IUIAutomationElement2 ({err})"
                            ),
                        })?;
                    let array = uia_call(|| unsafe { element2.CurrentFlowsFrom() })?;
                    self.rich_elements(&dpi, &array)?
                }
                // Refused above.
                _ => Vec::new(),
            };
            out.push((*kind, targets));
        }
        Ok(out)
    }

    fn table_headers(
        &self,
        e: &ElementData,
        kind: xa11y_core::table::HeaderKind,
    ) -> Result<Vec<ElementData>> {
        uia_gate("table_headers")?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let p: IUIAutomationTablePattern =
            self.rich_pattern(e, UIA_TablePatternId, "table_headers")?;
        let array = uia_call(|| unsafe {
            match kind {
                xa11y_core::table::HeaderKind::Column => p.GetCurrentColumnHeaders(),
                xa11y_core::table::HeaderKind::Row => p.GetCurrentRowHeaders(),
            }
        })?;
        let mut headers = self.rich_elements(&dpi, &array)?;
        rich::label_headers(&mut headers, kind);
        Ok(headers)
    }

    fn cell_headers(
        &self,
        e: &ElementData,
        kind: xa11y_core::table::HeaderKind,
    ) -> Result<Vec<ElementData>> {
        uia_gate("cell_headers")?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let p: IUIAutomationTableItemPattern =
            self.rich_pattern(e, UIA_TableItemPatternId, "cell_headers")?;
        let array = uia_call(|| unsafe {
            match kind {
                xa11y_core::table::HeaderKind::Column => p.GetCurrentColumnHeaderItems(),
                xa11y_core::table::HeaderKind::Row => p.GetCurrentRowHeaderItems(),
            }
        })?;
        let mut headers = self.rich_elements(&dpi, &array)?;
        rich::label_headers(&mut headers, kind);
        Ok(headers)
    }

    fn selected_items(&self, e: &ElementData) -> Result<Vec<ElementData>> {
        uia_gate("selected_items")?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let p: IUIAutomationSelectionPattern =
            self.rich_pattern(e, UIA_SelectionPatternId, "selected_items")?;
        self.rich_elements(&dpi, &uia_call(|| unsafe { p.GetCurrentSelection() })?)
    }

    fn text_length(&self, e: &ElementData) -> Result<u32> {
        uia_gate("text_length")?;
        Ok(xa11y_core::text_api::utf16_len(&rich::contents(
            &self.document(e)?,
        )?))
    }

    fn text_caret(&self, e: &ElementData) -> Result<Option<u32>> {
        uia_gate("text_caret")?;
        let el = self.get_cached(e.handle)?;
        // TextPattern2 reports whether its caret is active, including the
        // active endpoint of a backwards selection.
        if let Some(p) =
            current_pattern::<IUIAutomationTextPattern2>(&el, UIA_TextPattern2Id, "TextPattern2")?
        {
            let (active, caret) = uia_call(|| {
                let mut active = BOOL(0);
                let caret = unsafe { p.GetCaretRange(&mut active) }?;
                Ok((active, caret))
            })?;
            if !active.as_bool() {
                return Ok(None);
            }
            return Ok(Some(rich::offsets(&self.document(e)?, &caret)?.start));
        }
        let p: IUIAutomationTextPattern = self.rich_pattern(e, UIA_TextPatternId, "text")?;
        // Older providers expose the caret as a degenerate TextPattern
        // selection. Only use this documented representation while focused;
        // an inactive selection is not a caret.
        if !uia_call(|| unsafe { el.CurrentHasKeyboardFocus() })?.as_bool() {
            return Ok(None);
        }
        let selected = uia_call(|| unsafe { p.GetSelection() })?;
        if uia_call(|| unsafe { selected.Length() })? == 0 {
            return Ok(None);
        }
        let selection = uia_call(|| unsafe { selected.GetElement(0) })?;
        let offsets = rich::offsets(&self.document(e)?, &selection)?;
        if offsets.is_empty() {
            return Ok(Some(offsets.start));
        }
        let active =
            uia_call(|| unsafe { selection.GetAttributeValue(UIA_SelectionActiveEndAttributeId) })?;
        match variant_i32(&active) {
            Some(n) if n == ActiveEnd_Start.0 => Ok(Some(offsets.start)),
            Some(n) if n == ActiveEnd_End.0 => Ok(Some(offsets.end)),
            _ => Err(Error::Unsupported {
                feature: "UIA provider reports no active end for its text selection".into(),
            }),
        }
    }

    fn text_selections(&self, e: &ElementData) -> Result<Vec<xa11y_core::text_api::TextRange>> {
        uia_gate("text_selections")?;
        let p: IUIAutomationTextPattern = self.rich_pattern(e, UIA_TextPatternId, "text")?;
        let doc = uia_call(|| unsafe { p.DocumentRange() })?;
        let array = uia_call(|| unsafe { p.GetSelection() })?;
        let mut ranges = Vec::new();
        for i in 0..uia_call(|| unsafe { array.Length() })? {
            let range = rich::offsets(&doc, &uia_call(|| unsafe { array.GetElement(i) })?)?;
            if !range.is_empty() {
                ranges.push(range);
            }
        }
        ranges.sort_by_key(|r| (r.start, r.end));
        Ok(ranges)
    }

    fn text_read(&self, e: &ElementData, r: xa11y_core::text_api::TextRange) -> Result<String> {
        uia_gate("text_read")?;
        // UIA has no numeric offsets. Read only the prefix needed to
        // validate and slice the request, including a surrogate guard unit.
        // Scalar boundaries inside CRLF/graphemes remain readable even when
        // selection/geometry cannot address them as native endpoints.
        let doc = self.document(e)?;
        let max = i32::try_from(r.end.saturating_add(1)).unwrap_or(-1);
        // -1 is UIA's whole-range sentinel when the requested limit cannot
        // fit its i32 ABI. The resulting text still validates the range.
        let prefix = uia_call(|| unsafe { doc.GetText(max) })?.to_string();
        xa11y_core::text_api::slice_utf16(&prefix, r).map_err(|error| {
            if let Error::InvalidTextRange {
                start,
                end,
                length,
                message,
            } = error
            {
                Error::InvalidTextRange {
                    start,
                    end,
                    // A capped prefix is not the document's length.
                    length: length.filter(|n| max < 0 || *n < max as u32),
                    message,
                }
            } else {
                error
            }
        })
    }

    fn text_find(
        &self,
        e: &ElementData,
        needle: &str,
        options: xa11y_core::text_api::FindOptions,
    ) -> Result<Option<xa11y_core::text_api::TextRange>> {
        uia_gate("text_find")?;
        // Numeric UIA reads need prefixes. Share one fresh observation for
        // the entire search rather than re-reading it once per chunk. Use
        // core's exact matcher, not UIA's provider-dependent FindText rules.
        let text = rich::contents(&self.document(e)?)?;
        let units: Vec<u16> = text.encode_utf16().collect();
        xa11y_core::text_api::find_with_reader(needle, options, units.len() as u32, |r| {
            xa11y_core::text_api::slice_utf16_units(&units, r)
        })
    }

    fn text_range_at(
        &self,
        e: &ElementData,
        offset: u32,
        unit: xa11y_core::text_api::TextUnit,
    ) -> Result<xa11y_core::text_api::TextRange> {
        uia_gate("text_range_at")?;
        use xa11y_core::text_api::{slice_utf16, utf16_len, TextRange, TextUnit};
        let doc = self.document(e)?;
        let text = rich::contents(&doc)?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let length = units.len() as u32;
        xa11y_core::text_api::check_in_bounds(
            TextRange {
                start: offset,
                end: offset,
            },
            length,
        )?;
        if length == 0 {
            return Ok(TextRange { start: 0, end: 0 });
        }
        // At the end, answer for the unit that ends there; inside a surrogate
        // pair, for the pair. Both as on Linux and macOS.
        let all_at_end = offset == length;
        let mut offset = offset.min(length - 1);
        let is_low = |u: u16| (0xDC00..=0xDFFF).contains(&u);
        let is_high = |u: u16| (0xD800..=0xDBFF).contains(&u);
        if offset > 0 && is_low(units[offset as usize]) && is_high(units[offset as usize - 1]) {
            offset -= 1;
        }
        if unit == TextUnit::Character {
            let width = if is_high(units[offset as usize]) && offset + 1 < length {
                2
            } else {
                1
            };
            return Ok(TextRange {
                start: offset,
                end: offset + width,
            });
        }
        let include_newline = matches!(unit, TextUnit::Line | TextUnit::Paragraph);
        let unit = match unit {
            TextUnit::Character => TextUnit_Character,
            TextUnit::Word => TextUnit_Word,
            TextUnit::Line => TextUnit_Line,
            TextUnit::Paragraph => TextUnit_Paragraph,
        };
        let mut r = rich::offsets(
            &doc,
            &rich::unit_at(&doc, &text, if all_at_end { length } else { offset }, unit)?,
        )?;
        // WPF excludes CR/LF from the enclosing line/paragraph. Include the
        // reported separators so paging can resume at the next unit. A
        // provider whose unit already ends in its terminator (RichEdit
        // paragraphs end in CR) keeps it as it is: appending the *next*
        // separator would swallow an empty line after it.
        let ends_with_terminator =
            r.end > r.start && r.end <= length && matches!(units[r.end as usize - 1], 0x0A | 0x0D);
        if include_newline && !ends_with_terminator {
            let tail = slice_utf16(
                &text,
                TextRange {
                    start: r.end,
                    end: utf16_len(&text),
                },
            )?;
            if tail.starts_with("\r\n") {
                r.end += 2;
            } else if tail.starts_with(['\r', '\n']) {
                r.end += 1;
            }
        }
        if r.end > utf16_len(&text) {
            return Err(Error::Platform {
                code: -1,
                message: "UIA returned a range outside its document".into(),
            });
        }
        Ok(r)
    }

    fn text_range_bounds(
        &self,
        e: &ElementData,
        r: xa11y_core::text_api::TextRange,
    ) -> Result<Option<xa11y_core::Rect>> {
        uia_gate("text_range_bounds")?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let range = rich::range(&self.document(e)?, r)?;
        crate::coords::text_range_bounds(&dpi, &range)
    }

    fn text_attributes_at(
        &self,
        e: &ElementData,
        offset: u32,
    ) -> Result<xa11y_core::text_api::TextRun> {
        uia_gate("text_attributes_at")?;
        let doc = self.document(e)?;
        let sample = rich::format_sample(&doc, offset)?;
        let range = uia_call(|| unsafe { sample.Clone() })?;
        uia_call(|| unsafe { range.ExpandToEnclosingUnit(TextUnit_Format) })?;
        Ok(xa11y_core::text_api::TextRun::new(
            rich::offsets(&doc, &range)?,
            rich::attributes(
                &sample,
                e.raw.get("uia_framework_id").and_then(|v| v.as_str()) == Some("WPF"),
            )?,
        ))
    }

    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()> {
        uia_gate("perform_action")?;
        let _dpi = &crate::dpi::DpiScope::enter()?;
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
            "enter_fullscreen" => self.enter_fullscreen(element),
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
        uia_gate("subscribe")?;
        self.subscribe_kinds(element, None)
    }

    /// Registers only the UIA handlers the requested kinds need (see
    /// `event_plan::HandlerPlan`); core filters delivery to those kinds.
    fn subscribe_kinds(
        &self,
        element: &ElementData,
        kinds: Option<&[&str]>,
    ) -> Result<Subscription> {
        uia_gate("subscribe_kinds")?;
        let plan = crate::event_plan::HandlerPlan::for_kinds(kinds);
        let dpi = &crate::dpi::DpiScope::enter()?;
        // A synthesized Application node resolves its pid from the identity
        // map; a plain element carries its pid directly. Either way, the
        // stale-process guard runs: subscribing to a reused pid would attach
        // the handler to an unrelated process, the same silent retarget the
        // children path guards against (tenet 1).
        let saved_app =
            if let Some(identity) = self.synthetic_app_identity_checked(element.handle)? {
                validate_synthetic_app_identity(
                    element.handle,
                    identity,
                    process_creation_time(identity.pid),
                )?;
                Some(SavedSyntheticAppIdentity {
                    handle: element.handle,
                    identity,
                })
            } else {
                None
            };
        let pid = match saved_app {
            Some(saved) => saved.identity.pid,
            None => element.pid.ok_or(Error::Platform {
                code: -1,
                message: "Element has no PID for subscribe".to_string(),
            })?,
        };
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(dpi, pid, app_name, saved_app, &plan)
    }
}

// ── Helper Functions ─────────────────────────────────────────────────────────

/// Read the value through a pre-fetched pattern interface. The range read is
/// returned for the numeric field of the same element snapshot.
///
/// RangeValuePattern, when present, is the element's value: a failed read
/// propagates rather than falling through to ValuePattern, which would
/// report a different property as the value (tenet 1, issue #597).
fn get_value(
    site: ReadSite<'_>,
    patterns: &ElementPatterns,
    password: bool,
) -> Result<(Option<String>, Option<f64>)> {
    // For checkboxes/radios, value is handled by state — skip
    if matches!(site.role, Role::CheckBox | Role::RadioButton) {
        return Ok((None, None));
    }

    // RangeValuePattern first (sliders, progress bars, spinners)
    if let Some(ref pattern) = patterns.range_value {
        let v = pattern_read(site, "RangeValuePattern.CurrentValue", || unsafe {
            pattern.CurrentValue()
        })?;
        return Ok((Some(v.to_string()), Some(v)));
    }

    // ValuePattern (text fields, combo boxes)
    if let Some(ref pattern) = patterns.value {
        let text = value_pattern_text(
            site,
            password,
            retry_transient(|| unsafe { pattern.CurrentValue() }),
        )?;
        return Ok((text, None));
    }

    Ok((None, None))
}

/// Classify a `ValuePattern.CurrentValue` read.
///
/// A password control (`password`, from UIA's `IsPassword`) may decline to
/// disclose its value, and UIA documents `UIA_E_INVALIDOPERATION` as how it
/// does so (WPF's `PasswordBoxAutomationPeer` raises
/// `InvalidOperationException`). There is no value to report then, so
/// `None`. The same HRESULT from a control that is not a password control
/// is a failed read, and propagates like any other failure (tenet 1). An
/// empty string is also `None`.
fn value_pattern_text(
    site: ReadSite<'_>,
    password: bool,
    read: windows::core::Result<windows::core::BSTR>,
) -> Result<Option<String>> {
    match read {
        Ok(v) => Ok(Some(v.to_string()).filter(|s| !s.is_empty())),
        Err(e) if password && is_password_value_withheld(&e) => Ok(None),
        Err(e) => Err(pattern_read_error(site, "ValuePattern.CurrentValue", &e)),
    }
}

/// Whether a failed `ValuePattern.CurrentValue` read on a password control
/// is the control withholding its value (see [`value_pattern_text`]).
fn is_password_value_withheld(e: &windows::core::Error) -> bool {
    e.code().0 == UIA_E_INVALIDOPERATION as i32
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

    let has_activation_pattern = patterns.invoke.is_some()
        || patterns.toggle.is_some()
        || patterns.selection_item.is_some()
        || patterns.expand_collapse.is_some();
    if legacy_default_action_is_activation(
        has_activation_pattern,
        uia_cached_bstr(element, UIA_LegacyIAccessibleDefaultActionPropertyId).as_deref(),
        uia_cached_bstr(element, UIA_FrameworkIdPropertyId).as_deref(),
    ) {
        actions.push("press".to_string());
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

/// Why an element is not visible, or `None` when it is.
///
/// What the provider established about the element's ancestors wins over
/// `IsOffscreen`, because it is the more specific answer: UIA sets
/// `IsOffscreen` on a WPF `ListBoxItem` scrolled out of its list and on the
/// content of a minimized window alike, and on a collapsed menu's items too,
/// so on its own it says only that the element is not drawn.
fn hidden_reason(
    offscreen: bool,
    hidden_by_ancestor: Option<HiddenReason>,
) -> Option<HiddenReason> {
    match hidden_by_ancestor {
        Some(reason) => Some(reason),
        None if offscreen => Some(HiddenReason::NotShowing),
        None => None,
    }
}

/// Parse UIA element properties into xa11y StateSet using pre-queried patterns.
#[allow(non_upper_case_globals)]
fn parse_states(
    element: &IUIAutomationElement,
    site: ReadSite<'_>,
    patterns: &ElementPatterns,
    password: bool,
    hidden_by_ancestor: Option<HiddenReason>,
) -> Result<StateSet> {
    let role = site.role;
    let enabled = unsafe { element.CachedIsEnabled() }
        .unwrap_or(BOOL(1))
        .as_bool();
    let offscreen = unsafe { element.CachedIsOffscreen() }
        .unwrap_or(BOOL(0))
        .as_bool();
    // A minimized window's content (issue #501) and an item scrolled out of
    // its list (issue #502) are not visible, even though UIA leaves
    // `IsOffscreen` false on them.
    let hidden_reason = hidden_reason(offscreen, hidden_by_ancestor);
    let visible = hidden_reason.is_none();
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

    // Checked: from TogglePattern. Every pattern read below propagates its
    // failure (`pattern_read`): a toggle whose state could not be read is
    // not `Off`, and a test waiting for `checked == off` must not pass
    // against an element it never read (tenet 1, issue #597).
    let checked = match role {
        Role::CheckBox | Role::RadioButton => {
            if let Some(ref pattern) = patterns.toggle {
                toggled_from_state(pattern_read(
                    site,
                    "TogglePattern.CurrentToggleState",
                    || unsafe { pattern.CurrentToggleState() },
                )?)
            } else if let Some(ref pattern) = patterns.selection_item {
                // For radio buttons, check SelectionItemPattern
                if pattern_read(site, "SelectionItemPattern.CurrentIsSelected", || unsafe {
                    pattern.CurrentIsSelected()
                })?
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
        expanded_from_state(pattern_read(
            site,
            "ExpandCollapsePattern.CurrentExpandCollapseState",
            || unsafe { pattern.CurrentExpandCollapseState() },
        )?)
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
        Some(ref pattern) => {
            pattern_read(site, "SelectionItemPattern.CurrentIsSelected", || unsafe {
                pattern.CurrentIsSelected()
            })?
            .as_bool()
        }
        None => legacy_state_selected(uia_cached_i32(
            element,
            UIA_LegacyIAccessibleStatePropertyId,
        )),
    };

    let editable = match role {
        Role::TextField | Role::TextArea => {
            if let Some(ref pattern) = patterns.value {
                !pattern_read(site, "ValuePattern.CurrentIsReadOnly", || unsafe {
                    pattern.CurrentIsReadOnly()
                })?
                .as_bool()
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
        Some(ref pattern) => {
            match pattern_read(site, "WindowPattern.CurrentWindowVisualState", || unsafe {
                pattern.CurrentWindowVisualState()
            })? {
                WindowVisualState_Minimized => (Some(true), Some(false)),
                WindowVisualState_Maximized => (Some(false), Some(true)),
                // Normal is the only other defined value (there is no Restored);
                // it clears both flags. A value outside the spec is unknown, not
                // "definitely not minimized/maximized" — `None`, never guessed.
                WindowVisualState_Normal => (Some(false), Some(false)),
                _ => (None, None),
            }
        }
        None => (None, None),
    };

    // Modal from WindowPattern.CurrentIsModal — the only authoritative UIA
    // signal. Previously hard-coded `false`, which reported every modal
    // dialog (e.g. a WinForms modal form) as non-modal.
    let modal = match patterns.window {
        Some(ref pattern) => pattern_read(site, "WindowPattern.CurrentIsModal", || unsafe {
            pattern.CurrentIsModal()
        })?
        .as_bool(),
        None => false,
    };

    Ok(StateParts {
        enabled,
        visible,
        hidden_reason,
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
        password,
    }
    .into())
}

/// Map a `ToggleState` to `checked`. A value outside UIA's three is unknown,
/// not `Off`.
#[allow(non_upper_case_globals)]
fn toggled_from_state(state: ToggleState) -> Option<Toggled> {
    match state {
        ToggleState_On => Some(Toggled::On),
        ToggleState_Off => Some(Toggled::Off),
        ToggleState_Indeterminate => Some(Toggled::Mixed),
        _ => None,
    }
}

/// The boolean a `StateChanged { Checked }` event carries for a
/// `ToggleState`, or `None` when the state has no boolean meaning.
///
/// Derived from [`toggled_from_state`] so an event and a re-read of the same
/// element cannot disagree: `Indeterminate` reads as `Mixed` and an
/// out-of-spec value as unknown, and neither is "became unchecked"
/// (issue #609).
fn checked_value_from_state(state: ToggleState) -> Option<bool> {
    match toggled_from_state(state)? {
        Toggled::On => Some(true),
        Toggled::Off => Some(false),
        Toggled::Mixed => None,
    }
}

/// Map an `ExpandCollapseState` to `expanded`. `PartiallyExpanded` and
/// `LeafNode` have no boolean answer, so they are unknown, not collapsed.
/// Shared by the snapshot and the property-change event paths (issue #609).
#[allow(non_upper_case_globals)]
fn expanded_from_state(state: ExpandCollapseState) -> Option<bool> {
    match state {
        ExpandCollapseState_Expanded => Some(true),
        ExpandCollapseState_Collapsed => Some(false),
        _ => None,
    }
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
pub(super) fn map_msaa_role(legacy_role: i32) -> Option<Role> {
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

/// Moves a COM interface into a `Send` closure. The UIA client objects
/// captured this way are agile (they aggregate the free-threaded
/// marshaler), so the pointer is valid on any thread; the registration
/// worker and the UIA callback threads that dereference it are also MTA.
///
/// Mirrors the `unsafe impl Send for WindowsProvider` assertion in this file:
/// the same agility holds for every COM type we need to capture.
///
/// Note the private inner field + accessor method: Rust 2021's disjoint
/// closure captures will grab `wrapper.0` (the inner `T`) if it's reachable,
/// which defeats the `Send` assertion on the wrapper. Going through `get()`
/// forces the full wrapper to be captured.
struct ComSend<T> {
    inner: T,
}

type RegistrationJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
struct RegistrationWorkerError {
    code: i64,
    message: String,
}

static REGISTRATION_WORKER: OnceLock<
    std::result::Result<std::sync::mpsc::Sender<RegistrationJob>, RegistrationWorkerError>,
> = OnceLock::new();

/// Return the process-wide UIA registration worker.
///
/// UI Automation requires every event-handler registration and removal to be
/// made from the same thread. Providers and subscriptions can be created and
/// dropped from arbitrary runtime threads, so the worker is process-wide,
/// long-lived, and explicitly enters the MTA before accepting work.
fn registration_worker() -> Result<std::sync::mpsc::Sender<RegistrationJob>> {
    REGISTRATION_WORKER
        .get_or_init(|| {
            let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<RegistrationJob>();
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            std::thread::Builder::new()
                .name("xa11y-uia-registration".to_string())
                .spawn(move || {
                    let initialized =
                        ensure_com_initialized().map_err(|e| RegistrationWorkerError {
                            code: e.code().0 as i64,
                            message: format!(
                                "COM initialization failed on UIA registration worker: {e}"
                            ),
                        });
                    // The worker is xa11y's and runs no host code, so it is
                    // made per-monitor aware once for its whole life rather
                    // than per job (issue #506).
                    let initialized = initialized.and_then(|()| {
                        crate::dpi::make_thread_per_monitor_aware().map_err(|e| {
                            RegistrationWorkerError {
                                code: -1,
                                message: format!(
                                    "making the UIA registration worker per-monitor DPI aware \
                                     failed: {e}"
                                ),
                            }
                        })
                    });
                    if ready_tx.send(initialized.clone()).is_err() || initialized.is_err() {
                        return;
                    }
                    for job in jobs_rx {
                        job();
                    }
                })
                .map_err(|e| RegistrationWorkerError {
                    code: -1,
                    message: format!("Failed to start UIA registration worker: {e}"),
                })?;
            ready_rx.recv().map_err(|e| RegistrationWorkerError {
                code: -1,
                message: format!("UIA registration worker exited during startup: {e}"),
            })??;
            Ok(jobs_tx)
        })
        .clone()
        .map_err(|e| Error::Platform {
            code: e.code,
            message: e.message,
        })
}

/// Run one registration operation on the shared worker and wait for its
/// result. Callbacks never use this synchronous form: a removal can wait for
/// an in-flight callback, so making a callback wait for the worker would form
/// a UIA callback/removal deadlock.
fn on_registration_worker<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let worker = registration_worker()?;
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    worker
        .send(Box::new(move || {
            let _ = result_tx.send(operation());
        }))
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("UIA registration worker is unavailable: {e}"),
        })?;
    result_rx.recv().map_err(|e| Error::Platform {
        code: -1,
        message: format!("UIA registration worker stopped before completing an operation: {e}"),
    })?
}

fn queue_registration_job(operation: impl FnOnce() + Send + 'static) -> Result<()> {
    registration_worker()?
        .send(Box::new(operation))
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("UIA registration worker is unavailable: {e}"),
        })
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
    /// What was added to this window, so removal removes exactly that.
    added: AddedHandlers,
}

/// The per-window registrations one window actually received. Built up as
/// `register_window_handlers` goes, so a partial failure removes only what
/// was added.
#[derive(Default)]
struct AddedHandlers {
    automation_ids: Vec<UIA_EVENT_ID>,
    property: bool,
    structure: bool,
    notification: bool,
}

/// The handlers a subscription attaches to each of its top-level windows,
/// and which event and property IDs it registers them for: the
/// [`HandlerPlan`](crate::event_plan::HandlerPlan) in UIA terms. A handler
/// the plan does not need is `None` and is never registered.
struct PerWindowHandlers {
    automation: IUIAutomationEventHandler,
    /// Empty when no requested kind needs an automation event.
    automation_ids: Vec<UIA_EVENT_ID>,
    /// The handler and its property IDs; `None` when no property is watched.
    property: Option<(
        IUIAutomationPropertyChangedEventHandler,
        Vec<UIA_PROPERTY_ID>,
    )>,
    structure: Option<IUIAutomationStructureChangedEventHandler>,
    /// `AddNotificationEventHandler` lives on `IUIAutomation5`, so the cast
    /// interface travels with the handler.
    notification: Option<(IUIAutomation5, IUIAutomationNotificationEventHandler)>,
}

impl PerWindowHandlers {
    /// Whether `WindowVisualState` is watched, which is the only reader of
    /// the per-window visual-state baseline.
    fn watches_visual_state(&self) -> bool {
        self.property
            .as_ref()
            .is_some_and(|(_, ids)| ids.contains(&UIA_WindowWindowVisualStatePropertyId))
    }
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
    handlers: ComSend<PerWindowHandlers>,
    /// Per-window registrations, keyed by HWND — the open/close watch diffs
    /// against this set and the cancel closure drains it.
    registered: Mutex<HashMap<usize, RegisteredWindow>>,
    /// Current `WindowVisualState` per top-level window; shared with
    /// [`PropertyHandler`] so the delta map and the add/remove paths agree.
    visual_states: Arc<Mutex<HashMap<usize, i32>>>,
    /// Proven frame identities shared with callbacks; populated only by the
    /// registration worker, where shell attribution is safe.
    frame_runtime_ids: Arc<Mutex<HashMap<usize, HashSet<Vec<i32>>>>>,
    /// Process-generation guard retained for the entire subscription. A
    /// desktop-scoped watch must never attach to a later process that reuses
    /// the subscribed app's pid.
    saved_app: Option<SavedSyntheticAppIdentity>,
    /// Keeps each subscription's bookkeeping transaction explicit. Native
    /// Add*/Remove* work is already serialized across *all* subscriptions by
    /// the process-wide registration worker; this lock also documents and
    /// protects the state transaction if non-UIA bookkeeping is later called
    /// from another path.
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
/// background reconciliation retries it and disconnects the event source if
/// readiness still cannot be established.
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

/// Called only on the registration worker. Native frame decorations belong
/// to ApplicationFrameHost too; cache their exact UIA identities while never
/// entering the app-owned CoreWindow subtree. Callbacks then need no walk or
/// shell lookup to distinguish two apps sharing one frame-host process.
fn frame_event_runtime_ids(
    autom: &IUIAutomation,
    frame: &IUIAutomationElement,
) -> Result<HashSet<Vec<i32>>> {
    fn neighbor(
        result: windows::core::Result<IUIAutomationElement>,
    ) -> Result<Option<IUIAutomationElement>> {
        match result {
            Ok(element) => Ok(Some(element)),
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(Error::Platform {
                code: i64::from(error.code().0),
                message: format!("reading UWP frame event-source children failed: {error}"),
            }),
        }
    }
    fn visit(
        walker: &IUIAutomationTreeWalker,
        element: &IUIAutomationElement,
        host_pid: i32,
        depth: usize,
        visited: &mut usize,
        ids: &mut HashSet<Vec<i32>>,
    ) -> Result<()> {
        *visited += 1;
        if *visited > 128 || depth > 8 {
            return Err(Error::Platform {
                code: -1,
                message: "UWP frame event-source inspection exceeded 128 nodes or depth 8".into(),
            }
            .diagnose(
                xa11y_core::Diagnosis::new()
                    .condition("bounded inspection of frame-host event sources")
                    .last_observed(format!("visited {visited} nodes; depth {depth}")),
            ));
        }
        if uia_call(|| unsafe { element.CurrentProcessId() })? != host_pid {
            return Ok(());
        }
        let id = WindowsProvider::runtime_id(element, 0)?;
        if id.is_empty() {
            return Err(Error::Platform {
                code: -1,
                message: "a UWP frame event source has no RuntimeId".into(),
            });
        }
        ids.insert(id);
        let mut child = neighbor(unsafe { walker.GetFirstChildElement(element) })?;
        while let Some(element) = child {
            visit(walker, &element, host_pid, depth + 1, visited, ids)?;
            child = neighbor(unsafe { walker.GetNextSiblingElement(&element) })?;
        }
        Ok(())
    }
    let host_pid = uia_call(|| unsafe { frame.CurrentProcessId() })?;
    let walker = uia_call(|| unsafe { autom.RawViewWalker() })?;
    let mut ids = HashSet::new();
    visit(&walker, frame, host_pid, 0, &mut 0, &mut ids)?;
    Ok(ids)
}

/// Register the subscription's per-window handlers (automation,
/// property-changed, structure-changed, notification — whichever its plan
/// needs) on one top-level window and seed its `WindowVisualState` baseline.
///
/// The caller stores the returned record in the subscription's per-window map.
/// A partial failure removes what was registered and returns the error (tenet
/// 1): a half-registered window would deliver only some event kinds and read
/// as a complete subscription. Seeding happens first so a
/// `WindowVisualState` event arriving mid-registration has a prior value to
/// delta against; a re-read after the handlers are attached then closes the
/// seed→attach gap, where a transition's event would otherwise be raised to
/// nobody and leave the baseline stale (see the reconciliation below). A
/// subscription that does not watch `WindowVisualState` seeds nothing.
///
/// A window gets a record even when the plan registers no per-window handler
/// (a focus-only subscription): the record carries the window's frame
/// identity, which the focus handler's PID filter needs for UWP apps.
///
/// The handle is passed in rather than re-read here: the caller already read
/// it (enumerating the window set), and a second read that fails after the
/// first succeeded would abort a registration over a transient property read.
fn register_window_handlers(
    autom: &IUIAutomation,
    window: &IUIAutomationElement,
    hwnd: usize,
    cache_request: &crate::coords::BoundsCacheRequest,
    ctx: &EventContext,
    handlers: &PerWindowHandlers,
    visual_states: &Mutex<HashMap<usize, i32>>,
) -> Result<RegisteredWindow> {
    let dpi = &crate::dpi::DpiScope::enter()?;
    let cache = cache_request.get(dpi);
    let frame_identity = if hwnd != 0 && crate::uwp::is_frame(HWND(hwnd as *mut _)) {
        Some(frame_event_runtime_ids(autom, window)?)
    } else {
        None
    };
    // Acquire the window pattern once: the baseline seed below and the
    // post-attach reconciliation both read `CurrentWindowVisualState` from
    // it, and a second acquisition could fail transiently between two reads
    // that must agree on the same window.
    //
    // Only an actually absent WindowPattern means "no baseline" (a window
    // without the pattern never raises visual-state events, so there is
    // nothing to seed). A transient or stale-provider failure propagates
    // instead of being swallowed — the PropertyHandler drops a state event
    // whose baseline is missing, so swallowing this read would silently
    // consume the first real minimize/maximize transition and miss the event
    // while the subscription reports success (tenet 1). Same classification
    // the window verbs apply via `pattern_acquisition_error`.
    let pattern = if hwnd != 0 && handlers.watches_visual_state() {
        match unsafe {
            window.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
        } {
            Ok(pattern) => Some(pattern),
            Err(e) if is_pattern_absent(&e) => None,
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
    } else {
        None
    };
    // Seed the visual-state baseline so the first WindowVisualState
    // notification after subscribe is already a true delta.
    let seeded = match &pattern {
        Some(pattern) => match unsafe { pattern.CurrentWindowVisualState() } {
            Ok(state) => {
                let mut states = visual_states.lock().unwrap_or_else(|e| e.into_inner());
                states.insert(hwnd, state.0);
                Some(state.0)
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
        None => None,
    };

    // Undo a partial registration: remove exactly what `added` records.
    let fail = |added: &AddedHandlers, err: Error| {
        remove_handlers_of(autom, window, handlers, added);
        remove_visual_state(visual_states, hwnd);
        err
    };
    let mut added = AddedHandlers::default();
    for eid in &handlers.automation_ids {
        if let Err(e) = unsafe {
            autom.AddAutomationEventHandler(
                *eid,
                window,
                TreeScope_Subtree,
                cache,
                &handlers.automation,
            )
        } {
            return Err(fail(
                &added,
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("AddAutomationEventHandler({:?}) failed: {e}", eid),
                },
            ));
        }
        added.automation_ids.push(*eid);
    }
    if let Some((property, ids)) = &handlers.property {
        if let Err(e) = unsafe {
            autom.AddPropertyChangedEventHandlerNativeArray(
                window,
                TreeScope_Subtree,
                cache,
                property,
                ids,
            )
        } {
            return Err(fail(
                &added,
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("AddPropertyChangedEventHandlerNativeArray failed: {e}"),
                },
            ));
        }
        added.property = true;
    }
    if let Some(structure) = &handlers.structure {
        if let Err(e) = unsafe {
            autom.AddStructureChangedEventHandler(window, TreeScope_Subtree, cache, structure)
        } {
            return Err(fail(
                &added,
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("AddStructureChangedEventHandler failed: {e}"),
                },
            ));
        }
        added.structure = true;
    }
    if let Some((autom5, notification)) = &handlers.notification {
        if let Err(e) = unsafe {
            autom5.AddNotificationEventHandler(window, TreeScope_Subtree, cache, notification)
        } {
            return Err(fail(
                &added,
                Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("AddNotificationEventHandler failed: {e}"),
                },
            ));
        }
        added.notification = true;
    }

    // Publish identity before the post-attach state read: callbacks arriving
    // after that read must already recognize frame-host event senders.
    if let Some(ids) = frame_identity {
        ctx.frame_runtime_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hwnd, ids);
    }

    // Close the seed→attach gap. The baseline is seeded before the property
    // handler is registered, so a visual-state transition that lands in
    // between raises its `PropertyChanged` to nobody — and the stale seed then
    // swallows the *next* transition's delta too (a restore after a missed
    // minimize compares Normal against Normal and emits nothing). Re-read now
    // that the handler is live: when no event has updated the baseline since
    // the seed, this read is the first observation after attachment and its
    // delta is emitted here. When an event did update it, the handler already
    // emitted the delta and this read is older than that observation — leave
    // the map alone. An in-flight duplicate of a transition this read already
    // folded in is suppressed by the same map comparison in `PropertyHandler`.
    if let (Some(pattern), Some(seeded)) = (&pattern, seeded) {
        match unsafe { pattern.CurrentWindowVisualState() } {
            Ok(current) => {
                let kinds = {
                    let mut states = visual_states.lock().unwrap_or_else(|e| e.into_inner());
                    match states.get(&hwnd).copied() {
                        Some(observed) if observed == seeded => {
                            states.insert(hwnd, current.0);
                            visual_state_delta_kinds(seeded, current.0)
                        }
                        // A handler observed (and emitted) a change after the
                        // seed, or the entry was torn down: the re-read must
                        // not overwrite a newer observation.
                        _ => Vec::new(),
                    }
                };
                if !kinds.is_empty() {
                    let target = ctx.snapshot_or_log(window, cache_request);
                    for kind in kinds {
                        ctx.emit(kind, target.clone());
                    }
                }
            }
            Err(e) => {
                // The handlers are attached but the gap could not be closed:
                // a half-reconciled window would silently drop transitions,
                // so undo the registration and surface the failure (tenet 1).
                let err = fail(
                    &added,
                    Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "CurrentWindowVisualState failed while reconciling the baseline of \
                             window {hwnd:#x} after attaching handlers: {e}"
                        ),
                    },
                );
                ctx.frame_runtime_ids
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&hwnd);
                return Err(err);
            }
        }
    }

    Ok(RegisteredWindow {
        element: ComSend::new(window.clone()),
        hwnd,
        added,
    })
}

fn remove_visual_state(visual_states: &Mutex<HashMap<usize, i32>>, hwnd: usize) {
    visual_states
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&hwnd);
}

/// Remove a window's previously registered handlers — exactly those `added`
/// records. Removal errors are ignored: a window that closed during
/// subscription (or was never fully registered) answers `Remove*` with an
/// error there is nothing to do about, and the callers treat removal as
/// best-effort teardown.
fn remove_handlers_of(
    autom: &IUIAutomation,
    window: &IUIAutomationElement,
    handlers: &PerWindowHandlers,
    added: &AddedHandlers,
) {
    for eid in &added.automation_ids {
        let _ = unsafe { autom.RemoveAutomationEventHandler(*eid, window, &handlers.automation) };
    }
    if let (true, Some((property, _))) = (added.property, &handlers.property) {
        let _ = unsafe { autom.RemovePropertyChangedEventHandler(window, property) };
    }
    if let (true, Some(structure)) = (added.structure, &handlers.structure) {
        let _ = unsafe { autom.RemoveStructureChangedEventHandler(window, structure) };
    }
    if let (true, Some((autom5, notification))) = (added.notification, &handlers.notification) {
        let _ = unsafe { autom5.RemoveNotificationEventHandler(window, notification) };
    }
}

/// Reconcile per-window registrations with the pid's current top-level
/// windows: attach handlers to windows that opened, tear down handlers of
/// windows that closed, and seed / drop their visual-state baselines.
///
/// Used by the open/close watch after each event and once by `subscribe_impl`
/// right after the watch is registered, so a window that opened during the
/// subscribe-time enumeration is attached too. The caller decides whether a
/// failure can be returned (subscribe) or should be retried and diagnosed
/// (the asynchronous watch path).
fn sync_registrations(
    state: &SubscriptionState,
    cache_request: &crate::coords::BoundsCacheRequest,
    ctx: &EventContext,
    pid: u32,
) -> Result<bool> {
    // Runs on the registration worker, which is per-monitor for life, so
    // this scope is the token the fetches below need rather than a switch.
    let dpi = &crate::dpi::DpiScope::enter()?;
    let cache = cache_request.get(dpi);
    // The process-wide worker serializes native registration operations
    // across subscriptions. This per-subscription guard makes the associated
    // diff and bookkeeping one transaction as well.
    let _guard = state
        .reconciliation
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.cancelled.load(Ordering::SeqCst) {
        return Ok(false);
    }
    let autom = state.automation.get();
    let current = match state.saved_app {
        Some(saved) => top_level_windows_of_saved_app_with(autom, cache, saved),
        None => top_level_windows_of_pid_with(autom, pid, cache),
    };
    let current = current?;
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
    for w in &current {
        current_by_hwnd.insert(window_handle(w)?, w);
    }
    let current_hwnds: HashSet<usize> = current_by_hwnd.keys().copied().collect();
    let registered_hwnds: HashSet<usize> = {
        let m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
        m.keys().copied().collect()
    };
    let (to_add, to_remove) = plan_window_registration_diff(&registered_hwnds, &current_hwnds);

    // Tear down closed windows first: any subsequent open keeps the remaining
    // registrations intact, and a closed window cannot accept new handlers.
    for hwnd in to_remove {
        ctx.frame_runtime_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hwnd);
        let reg = {
            let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
            m.remove(&hwnd)
        };
        if let Some(reg) = reg {
            remove_handlers_of(autom, reg.element.get(), state.handlers.get(), &reg.added);
        }
        let mut states = state
            .visual_states
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        states.remove(&hwnd);
    }

    for hwnd in to_add {
        let Some(window) = current_by_hwnd.get(&hwnd).copied() else {
            continue;
        };
        let reg = register_window_handlers(
            autom,
            window,
            hwnd,
            cache_request,
            ctx,
            state.handlers.get(),
            &state.visual_states,
        )?;
        let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(reg.hwnd, reg);
    }
    Ok(true)
}

/// Best-effort removal of every registration of a subscription: the desktop
/// watch, the focus handler (`None` when the plan registered none), and every
/// per-window record. Used on the
/// subscribe-time error path and on cancel; removal errors are ignored for
/// the same reason as in [`remove_handlers_of`] (a dead window or an
/// already-removed handler has nothing to remove).
fn cleanup_registrations(
    autom: &IUIAutomation,
    root: &IUIAutomationElement,
    watch: &IUIAutomationEventHandler,
    focus: Option<&IUIAutomationFocusChangedEventHandler>,
    state: &SubscriptionState,
) {
    // Stop reconciles from registering anything further and drain the
    // per-window records under the same bookkeeping guard used by sync.
    // Remove* stays outside that guard because UIA may wait for in-flight
    // callbacks; callbacks only enqueue worker jobs and never wait for them.
    let regs: Vec<RegisteredWindow> = {
        let _guard = state
            .reconciliation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.cancelled.store(true, Ordering::SeqCst);
        let mut m = state.registered.lock().unwrap_or_else(|e| e.into_inner());
        m.drain().map(|(_, r)| r).collect()
    };
    if let Some(focus) = focus {
        let _ = unsafe { autom.RemoveFocusChangedEventHandler(focus) };
    }
    for eid in WATCH_EVENT_IDS {
        let _ = unsafe { autom.RemoveAutomationEventHandler(*eid, root, watch) };
    }
    for reg in regs {
        remove_handlers_of(autom, reg.element.get(), state.handlers.get(), &reg.added);
    }
    state
        .visual_states
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    state
        .frame_runtime_ids
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Shared context passed to every UIA event handler.
///
/// `sender` is optional so a terminal background-registration failure can
/// disconnect the stream and become visible to a waiting consumer. The mutex
/// keeps that transition atomic with concurrent callback sends.
struct EventContext {
    sender: Mutex<Option<xa11y_core::EventSender>>,
    app_name: String,
    app_pid: u32,
    frame_runtime_ids: Arc<Mutex<HashMap<usize, HashSet<Vec<i32>>>>>,
    /// The saved generation for a synthetic App subscription. Global focus
    /// and desktop watch handlers re-check it for every event; plain element
    /// subscriptions have no saved-app identity and retain PID-only behavior.
    saved_app: Option<SavedSyntheticAppIdentity>,
    /// Clone of the provider's raw-view walker, so event-target snapshots
    /// resolve DataItem cells the same way tree traversal does. The walker
    /// is agile (free-threaded marshaler), so sharing the interface pointer
    /// with UIA callback threads is safe.
    walker: IUIAutomationTreeWalker,
}

// The COM walker pointer keeps EventContext from auto-deriving Send/Sync
// (it's shared via Arc with UIA's MTA callback threads). The same agility
// behind `unsafe impl Send for WindowsProvider` covers it.
unsafe impl Send for EventContext {}
unsafe impl Sync for EventContext {}

fn known_frame_sender(runtime_id: &[i32], frames: &HashMap<usize, HashSet<Vec<i32>>>) -> bool {
    !runtime_id.is_empty() && frames.values().any(|known| known.contains(runtime_id))
}

impl EventContext {
    fn emit(&self, kind: EventKind, target: Option<ElementData>) {
        self.emit_with(kind, target, None);
    }

    fn emit_with(
        &self,
        kind: EventKind,
        target: Option<ElementData>,
        announcement: Option<xa11y_core::AnnouncementInfo>,
    ) {
        let event: Event = EventParts {
            kind,
            target,
            app_name: self.app_name.clone(),
            app_pid: self.app_pid,
            timestamp: std::time::Instant::now(),
            // UIA TextChanged has no insertion/deletion payload. Do not infer
            // a delta by racing a fresh text read against subsequent edits.
            text_change: None,
            announcement,
            sequence: 0,
        }
        .into();
        if let Some(tx) = self
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            // Receiver may be dropped after close(); lost event is expected then.
            let _ = tx.send(event);
        }
    }

    /// Make a terminal background-registration failure visible to consumers.
    /// `wait_for` reports the disconnected source immediately rather than
    /// timing out against a subscription that can no longer meet its event
    /// contract.
    fn disconnect(&self) {
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Best-effort PID filter. `AddFocusChangedEventHandler` is process-wide,
    /// and scoped handlers occasionally leak events for sibling processes —
    /// checking the sender's PID keeps each subscription clean.
    /// Frame senders report ApplicationFrameHost's PID, so their exact UIA
    /// RuntimeId is matched against frames attributed on the worker. This
    /// also works while minimized/suspended, without shell calls in callbacks.
    fn matches_pid(&self, sender: &IUIAutomationElement) -> bool {
        let sender_matches = unsafe { sender.CurrentProcessId() }
            .map(|p| p as u32 == self.app_pid)
            .unwrap_or(false);
        let frame_matches = !sender_matches
            && WindowsProvider::runtime_id(sender, 0).is_ok_and(|id| {
                known_frame_sender(
                    &id,
                    &self
                        .frame_runtime_ids
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()),
                )
            });
        (sender_matches || frame_matches) && self.matches_generation()
    }

    fn matches_generation(&self) -> bool {
        self.saved_app.is_none_or(|saved| {
            validate_synthetic_app_identity(
                saved.handle,
                saved.identity,
                process_creation_time(saved.identity.pid),
            )
            .is_ok()
        })
    }

    /// Build a full ElementData snapshot from a UIA sender element.
    ///
    /// Event handlers are registered with a cache request, so the sender
    /// arrives with its properties prefetched, converted at the awareness of
    /// UIA's event thread. That prefetch is used as is when the thread was
    /// already per-monitor; when the scope had to switch it (or the cache is
    /// cold), the sender is refetched inside the scope so its bounds are
    /// physical. A refetch failure is the snapshot's failure, and the caller
    /// then delivers the event without a target.
    fn snapshot(
        &self,
        sender: &IUIAutomationElement,
        cache: &crate::coords::BoundsCacheRequest,
    ) -> Result<ElementData> {
        // UIA calls event handlers on its own threads, whose DPI awareness
        // xa11y does not control; the snapshot reads bounds (issue #506).
        let dpi = &crate::dpi::DpiScope::enter()?;
        let prefetched_is_physical =
            !dpi.switched() && unsafe { sender.CachedControlType() }.is_ok();
        let cached_element = if prefetched_is_physical {
            sender.clone()
        } else {
            unsafe { sender.BuildUpdatedCache(cache.get(dpi)) }.map_err(|e| Error::Platform {
                code: e.code().0 as i64,
                message: format!("BuildUpdatedCache for the event sender failed: {e}"),
            })?
        };
        // Event targets are not walked to their host window: the handler
        // must stay cheap. One with its own HWND is still checked, so a
        // windowless event target in a minimized window can report visible
        // where a tree read of the same element would not. Nor are they
        // clipped to their scrolling ancestors (issue #502), for the same
        // reason: an item scrolled out of view can report visible here.
        let in_minimized_window = unsafe { cached_element.CachedNativeWindowHandle() }
            .ok()
            .is_some_and(|h| !h.0.is_null() && hwnd_in_minimized_window(h));
        build_snapshot_data(
            dpi,
            &cached_element,
            Some(self.app_pid),
            0,
            Some(&self.walker),
            in_minimized_window.then_some(HiddenReason::WindowMinimized),
        )
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
        cache: &crate::coords::BoundsCacheRequest,
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

/// The `StateChanged` kinds for a `WindowVisualState` transition, given the
/// full state on each side.
///
/// UIA reports the whole visual state on every notification rather than a
/// delta, so the change-promise of `StateChanged` is enforced by deriving the
/// flags whose values differ. Empty when nothing changed or either side is an
/// unrecognized state — a state we cannot name is dropped rather than
/// reported (tenet 1). Shared by `PropertyHandler` and the post-attach
/// baseline reconciliation in [`register_window_handlers`], so both emit the
/// identical delta for the identical pair.
fn visual_state_delta_kinds(from: i32, to: i32) -> Vec<EventKind> {
    let (Some((was_minimized, was_maximized)), Some((minimized, maximized))) = (
        window_visual_state_to_flags(from),
        window_visual_state_to_flags(to),
    ) else {
        return Vec::new();
    };
    let mut kinds = Vec::with_capacity(2);
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
    kinds
}

// ── Handler implementations ──────────────────────────────────────────────────

#[implement(IUIAutomationFocusChangedEventHandler)]
struct FocusHandler {
    ctx: Arc<EventContext>,
    cache: crate::coords::BoundsCacheRequest,
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
    cache: crate::coords::BoundsCacheRequest,
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
            UIA_Text_TextSelectionChangedEventId => EventKind::TextSelectionChanged,
            UIA_SelectionItem_ElementSelectedEventId
            | UIA_SelectionItem_ElementAddedToSelectionEventId
            | UIA_SelectionItem_ElementRemovedFromSelectionEventId => EventKind::SelectionChanged,
            UIA_LiveRegionChangedEventId | UIA_SystemAlertEventId => EventKind::Announcement,
            _ => return Ok(()),
        };
        let target = self.ctx.snapshot_or_log(el, &self.cache);
        let announcement = uia_announcement(eventid, target.as_ref());
        self.ctx.emit_with(kind, target, announcement);
        Ok(())
    }
}

/// The payload of a UIA live-region or system-alert event, neither of which
/// carries text: read the target. A Notification (an explicit request) is
/// not an automation event here; [`NotificationHandler`] receives it with its
/// text through `AddNotificationEventHandler`.
#[allow(non_upper_case_globals)] // UIA constants use CamelCase in the windows crate
fn uia_announcement(
    eventid: UIA_EVENT_ID,
    target: Option<&ElementData>,
) -> Option<xa11y_core::AnnouncementInfo> {
    use xa11y_core::{AnnouncementOrigin, AnnouncementParts, Politeness};
    let (origin, politeness) = match eventid {
        // The region's own `LiveSetting` says how urgently.
        UIA_LiveRegionChangedEventId => (
            AnnouncementOrigin::LiveRegion,
            target
                .and_then(|t| t.live_region.as_ref())
                .and_then(|l| l.politeness),
        ),
        UIA_SystemAlertEventId => (AnnouncementOrigin::Alert, Some(Politeness::Assertive)),
        _ => return None,
    };
    Some(
        AnnouncementParts {
            origin,
            text: None,
            politeness,
            platform_priority: None,
            activity_id: None,
        }
        .into(),
    )
}

/// Receives UIA `Notification` events (`UiaRaiseNotificationEvent`, WinForms
/// `RaiseAutomationNotification`, XAML `RaiseNotificationEvent`) with their
/// arguments, which the generic automation-event handler never sees: the
/// display string becomes `announcement.text`, the processing value its
/// politeness and `platform_priority`, and the activity ID `activity_id`.
/// `NotificationKind` (item added, action completed, ...) has no field in
/// `AnnouncementInfo` and is dropped.
#[implement(IUIAutomationNotificationEventHandler)]
struct NotificationHandler {
    ctx: Arc<EventContext>,
    cache: crate::coords::BoundsCacheRequest,
}

impl IUIAutomationNotificationEventHandler_Impl for NotificationHandler_Impl {
    fn HandleNotificationEvent(
        &self,
        sender: windows::core::Ref<IUIAutomationElement>,
        _notificationkind: NotificationKind,
        notificationprocessing: NotificationProcessing,
        displaystring: &windows::core::BSTR,
        activityid: &windows::core::BSTR,
    ) -> windows::core::Result<()> {
        let Some(el) = sender.as_ref() else {
            return Ok(());
        };
        if !self.ctx.matches_pid(el) {
            return Ok(());
        }
        let target = self.ctx.snapshot_or_log(el, &self.cache);
        let announcement = crate::event_plan::notification_announcement(
            notificationprocessing.0,
            displaystring.to_string(),
            activityid.to_string(),
        );
        self.ctx
            .emit_with(EventKind::Announcement, target, Some(announcement));
        Ok(())
    }
}

/// Same native IID and ABI as IUIAutomationPropertyChangedEventHandler.
/// windows 0.62's generated trampoline takes an owned VARIANT and drops the
/// caller's borrowed payload after dispatch. ManuallyDrop preserves the
/// by-value ABI while keeping VariantClear exclusively with the native caller
/// (issue #589). The normal windows-core COM refcount/marshaling stays intact.
#[windows::core::interface("40cd37d4-c756-4b0c-8c6f-bddfeeb13b50")]
unsafe trait BorrowedPropertyChangedEventHandler: windows::core::IUnknown {
    fn property_changed(
        &self,
        sender: *mut core::ffi::c_void,
        propertyid: UIA_PROPERTY_ID,
        newvalue: std::mem::ManuallyDrop<VARIANT>,
    ) -> windows::core::HRESULT;
}

#[implement(BorrowedPropertyChangedEventHandler)]
struct PropertyHandler {
    ctx: Arc<EventContext>,
    cache: crate::coords::BoundsCacheRequest,
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

impl BorrowedPropertyChangedEventHandler_Impl for PropertyHandler_Impl {
    unsafe fn property_changed(
        &self,
        sender: *mut core::ffi::c_void,
        propertyid: UIA_PROPERTY_ID,
        newvalue: std::mem::ManuallyDrop<VARIANT>,
    ) -> windows::core::HRESULT {
        // UIA keeps sender and newvalue alive for the duration of this call;
        // neither borrowed parameter transfers a COM reference or allocation.
        let sender = unsafe { IUIAutomationElement::from_raw_borrowed(&sender) };
        self.handle_property_changed_event(sender.into(), propertyid, &newvalue)
            .into()
    }
}

impl PropertyHandler_Impl {
    #[allow(non_upper_case_globals)] // UIA constants use CamelCase in the windows crate
    fn handle_property_changed_event(
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
                // Only Off/On are a boolean change. An indeterminate or
                // out-of-spec state is not "became unchecked", so it raises
                // no StateChanged; the ValueChanged below still tells a
                // subscriber to re-read the element (issue #609).
                if let Some(value) =
                    variant_i32(newvalue).and_then(|v| checked_value_from_state(ToggleState(v)))
                {
                    kinds.push(EventKind::StateChanged {
                        flag: StateFlag::Checked,
                        value,
                    });
                }
                kinds.push(EventKind::ValueChanged);
            }
            UIA_ValueValuePropertyId | UIA_RangeValueValuePropertyId => {
                kinds.push(EventKind::ValueChanged);
            }
            UIA_ExpandCollapseExpandCollapseStatePropertyId => {
                // PartiallyExpanded and LeafNode are neither expanded nor
                // collapsed, so they raise no StateChanged (issue #609).
                if let Some(value) =
                    variant_i32(newvalue).and_then(|v| expanded_from_state(ExpandCollapseState(v)))
                {
                    kinds.push(EventKind::StateChanged {
                        flag: StateFlag::Expanded,
                        value,
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
                    // An unrecognized value is dropped before the baseline is
                    // touched: storing it would poison the next real
                    // transition's delta (tenet 1).
                    if window_visual_state_to_flags(v).is_none() {
                        return Ok(());
                    }
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
                        let prev = states.get(&hwnd).copied();
                        states.insert(hwnd, v);
                        prev
                    };
                    let Some(prev) = prev else {
                        // A window not seeded at subscription time (opened
                        // after subscribe): the prior state is unknown, and
                        // inventing one would be exactly the false delta
                        // `StateChanged` promises not to send. Drop the event
                        // — the current state is re-queryable.
                        return Ok(());
                    };
                    kinds.extend(visual_state_delta_kinds(prev, v));
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
    cache: crate::coords::BoundsCacheRequest,
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
    cache: crate::coords::BoundsCacheRequest,
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
                let matched = self.ctx.matches_pid(el);
                // A newly opened frame is not registered yet. Only classify
                // its native class here; shell/AUMID attribution belongs on
                // the registration worker, never a UIA callback thread.
                let deferred_frame = if !matched && eventid == UIA_Window_WindowOpenedEventId {
                    unsafe { el.CurrentNativeWindowHandle() }
                        .ok()
                        .filter(|hwnd| !hwnd.0.is_null() && crate::uwp::is_frame(*hwnd))
                        .map(|hwnd| hwnd.0 as usize)
                } else {
                    None
                };
                if !matched && deferred_frame.is_none() {
                    return Ok(());
                }
                let kind = if eventid == UIA_Window_WindowOpenedEventId {
                    EventKind::WindowOpened
                } else {
                    EventKind::WindowClosed
                };
                let target = if matched {
                    self.ctx.snapshot_or_log(el, &self.cache)
                } else {
                    None
                };
                let sender = ComSend::new(el.clone());
                let state = Arc::clone(&self.state);
                let ctx = Arc::clone(&self.ctx);
                let failure_ctx = Arc::clone(&self.ctx);
                let cache = ComSend::new(self.cache.clone());
                let pid = self.ctx.app_pid;

                // Never register from a UIA callback thread. Queue the whole
                // reconcile on the process-wide MTA worker and return so a
                // concurrent Remove* cannot wait on a callback that is itself
                // waiting for removal. WindowOpened is emitted only after the
                // new window's handlers and visual-state baseline are ready;
                // a consumer can therefore minimize it immediately without
                // racing attachment. A newly created provider can be briefly
                // unavailable, so retry the complete idempotent reconcile.
                if let Err(e) = queue_registration_job(move || {
                    let target = if let Some(hwnd) = deferred_frame {
                        if !ctx.matches_generation() {
                            return;
                        }
                        let hwnd = HWND(hwnd as *mut _);
                        // Open/close churn can destroy an unrelated frame
                        // before this worker sees its open notification.
                        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                            return;
                        }
                        let attributed = window_handle(sender.get()).and_then(|hwnd| {
                            crate::uwp::PidFilter::for_pid(pid).owns(HWND(hwnd as *mut _))
                        });
                        match attributed {
                            Ok(true) => ctx.snapshot_or_log(sender.get(), cache.get()),
                            Ok(false) => return,
                            Err(_) if !unsafe { IsWindow(Some(hwnd)) }.as_bool() => return,
                            Err(error) => {
                                eprintln!(
                                    "frame event attribution failed for pid {pid}: {error:?}"
                                );
                                ctx.disconnect();
                                return;
                            }
                        }
                    } else {
                        target
                    };
                    const READY_ATTEMPTS: usize = 4;
                    let mut last_error = None;
                    for attempt in 0..READY_ATTEMPTS {
                        match sync_registrations(&state, cache.get(), &ctx, pid) {
                            Ok(true) => {
                                ctx.emit(kind, target);
                                return;
                            }
                            Ok(false) => return,
                            Err(e) => last_error = Some(e),
                        }
                        if attempt + 1 < READY_ATTEMPTS {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                    }
                    if let Some(e) = last_error {
                        eprintln!(
                            "window-reconciliation failed after {READY_ATTEMPTS} attempts for pid {pid}; suppressing {kind:?} because event readiness was not established: {e:?}"
                        );
                        ctx.disconnect();
                    }
                }) {
                    eprintln!(
                        "failed to queue window-reconciliation for pid {}: {e:?}",
                        self.ctx.app_pid
                    );
                    failure_ctx.disconnect();
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// Event IDs registered through `AddAutomationEventHandler` on *each* top-level
// window's subtree, for the kinds a subscription asked for (see
// `event_plan::HandlerPlan`). `WindowOpened` / `WindowClosed` are deliberately
// NOT here: a per-window subtree registration would deliver a window's
// open/close twice (once from its own subtree scope, once from the desktop
// root's Subtree scope) — the open/close watch owns those two event IDs.
// `Notification` is not here either: `NotificationHandler` receives it with
// its text, and registering it here too would deliver each request twice.
fn automation_event_id(event: crate::event_plan::AutomationEvent) -> UIA_EVENT_ID {
    use crate::event_plan::AutomationEvent as E;
    match event {
        E::TextSelectionChanged => UIA_Text_TextSelectionChangedEventId,
        E::MenuOpened => UIA_MenuOpenedEventId,
        E::MenuClosed => UIA_MenuClosedEventId,
        E::TextChanged => UIA_Text_TextChangedEventId,
        E::ElementSelected => UIA_SelectionItem_ElementSelectedEventId,
        E::ElementAddedToSelection => UIA_SelectionItem_ElementAddedToSelectionEventId,
        E::ElementRemovedFromSelection => UIA_SelectionItem_ElementRemovedFromSelectionEventId,
        E::LiveRegionChanged => UIA_LiveRegionChangedEventId,
        // The design-doc-listed Announcement source for pre-Windows-10 alert
        // messages and some legacy providers that don't raise
        // NotificationEvent. Dispatched to EventKind::Announcement in
        // `AutomationHandler::HandleAutomationEvent`.
        E::SystemAlert => UIA_SystemAlertEventId,
    }
}

// Event IDs registered through `AddAutomationEventHandler` on the *desktop
// root* (TreeScope_Subtree): the open/close watch, whose handler reconciles
// the per-window registrations with the pid's current top-level windows.
// Subtree scope is required because UIA parents owned windows (dialogs,
// message boxes) under their owner rather than the desktop root.
const WATCH_EVENT_IDS: &[UIA_EVENT_ID] = &[
    UIA_Window_WindowOpenedEventId,
    UIA_Window_WindowClosedEventId,
];

// Property IDs watched via `AddPropertyChangedEventHandlerNativeArray`, for the
// kinds a subscription asked for. `WindowVisualState` is the canonical UIA
// notification for window minimize/maximize/restore: UIA has no dedicated
// event ID for it, so providers raise PropertyChanged(WindowVisualState) and a
// provider that doesn't watch the property never sees it.
fn watched_property_id(property: crate::event_plan::WatchedProperty) -> UIA_PROPERTY_ID {
    use crate::event_plan::WatchedProperty as P;
    match property {
        P::Name => UIA_NamePropertyId,
        P::IsEnabled => UIA_IsEnabledPropertyId,
        P::ToggleState => UIA_ToggleToggleStatePropertyId,
        P::Value => UIA_ValueValuePropertyId,
        P::RangeValue => UIA_RangeValueValuePropertyId,
        P::ExpandCollapseState => UIA_ExpandCollapseExpandCollapseStatePropertyId,
        P::WindowVisualState => UIA_WindowWindowVisualStatePropertyId,
    }
}

impl WindowsProvider {
    fn subscribe_impl(
        &self,
        dpi: &crate::dpi::DpiScope,
        pid: u32,
        app_name: String,
        saved_app: Option<SavedSyntheticAppIdentity>,
        plan: &crate::event_plan::HandlerPlan,
    ) -> Result<Subscription> {
        // `AddNotificationEventHandler` is on `IUIAutomation5`, which
        // `CUIAutomation8` implements from Windows 10 1709. Without it there
        // is no way to receive a Notification: asking for `announcement` by
        // name then fails here, before anything is registered. An all-kinds
        // subscription leaves the notification handler out instead of
        // failing every subscription on such a system; no other mechanism
        // stands in for it, and live-region and system-alert announcements
        // still arrive through the automation handler.
        let automation5 = if plan.notification {
            match self.automation.cast::<IUIAutomation5>() {
                Ok(automation5) => Some(automation5),
                Err(e) if plan.announcement_requested => {
                    return Err(Error::Unsupported {
                        feature: format!(
                            "announcement events on this system: UIA Notification events \
                             need IUIAutomation5 (Windows 10 version 1709 or later), which \
                             the UI Automation client does not implement ({e})"
                        ),
                    });
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let (tx, rx) = xa11y_core::event_channel();

        // Enumerate every current top-level window of the pid up front and
        // register the scoped handlers on *each* of them — the pre-C1 shape
        // resolved a single representative via `find_app_by_pid` (FindFirst)
        // and scoped the handlers to that one window's subtree, so events
        // from same-pid sibling top-level windows (a modal dialog next to the
        // main window, issue #304) were never delivered. The desktop-scoped
        // open/close watch below keeps the set in step as windows come and
        // go, so "app subscription" now means "the process".
        let windows = match saved_app {
            Some(saved) => top_level_windows_of_saved_app_with(
                &self.automation,
                self.batch_request.get(dpi),
                saved,
            )?,
            None => self.top_level_windows_of_pid(dpi, pid)?,
        };
        if windows.is_empty() {
            // Not reachable yet (or its last window just closed): surface it
            // as a selector miss so core's poll loop retries, matching
            // `find_app_by_pid`'s contract for a fresh process.
            return Err(Error::Platform {
                code: -1,
                message: format!("No top-level window found for PID {pid} while subscribing"),
            });
        }

        let frame_runtime_ids = Arc::new(Mutex::new(HashMap::new()));
        let ctx = Arc::new(EventContext {
            sender: Mutex::new(Some(tx)),
            app_name,
            app_pid: pid,
            frame_runtime_ids: Arc::clone(&frame_runtime_ids),
            saved_app,
            walker: self.raw_walker.clone(),
        });

        // Dedicated cache request: ensures event handlers receive elements
        // with the standard snapshot properties pre-fetched. A fresh request
        // avoids relying on a mutable handle held elsewhere.
        let cache = create_event_cache_request(&self.automation)?;

        let focus: Option<IUIAutomationFocusChangedEventHandler> = plan.focus.then(|| {
            FocusHandler {
                ctx: ctx.clone(),
                cache: cache.clone(),
            }
            .into()
        });
        let automation_handler: IUIAutomationEventHandler = AutomationHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
        }
        .into();
        let visual_states = Arc::new(Mutex::new(HashMap::<usize, i32>::new()));
        let property = if plan.properties.is_empty() {
            None
        } else {
            let borrowed_property: BorrowedPropertyChangedEventHandler = PropertyHandler {
                ctx: ctx.clone(),
                cache: cache.clone(),
                visual_state_by_hwnd: Arc::clone(&visual_states),
            }
            .into();
            let property: IUIAutomationPropertyChangedEventHandler =
                uia_call(|| borrowed_property.cast())?;
            let ids = plan
                .properties
                .iter()
                .map(|p| watched_property_id(*p))
                .collect();
            Some((property, ids))
        };
        let structure: Option<IUIAutomationStructureChangedEventHandler> =
            plan.structure.then(|| {
                StructureHandler {
                    ctx: ctx.clone(),
                    cache: cache.clone(),
                }
                .into()
            });
        let notification = automation5.map(|automation5| {
            let handler: IUIAutomationNotificationEventHandler = NotificationHandler {
                ctx: ctx.clone(),
                cache: cache.clone(),
            }
            .into();
            (automation5, handler)
        });
        let handlers = PerWindowHandlers {
            automation: automation_handler,
            automation_ids: plan
                .automation
                .iter()
                .map(|e| automation_event_id(*e))
                .collect(),
            property,
            structure,
            notification,
        };

        // The desktop root anchors the open/close watch below.
        let root = uia_call(|| unsafe { self.automation.GetRootElement() }).map_err(|e| {
            Error::Platform {
                code: -1,
                message: format!("GetRootElement failed while subscribing: {e}"),
            }
        })?;

        let state = Arc::new(SubscriptionState {
            automation: ComSend::new(self.automation.clone()),
            handlers: ComSend::new(handlers),
            registered: Mutex::new(HashMap::new()),
            visual_states,
            frame_runtime_ids,
            saved_app,
            reconciliation: Mutex::new(()),
            cancelled: AtomicBool::new(false),
        });
        let watch: IUIAutomationEventHandler = WatchHandler {
            ctx: ctx.clone(),
            cache: cache.clone(),
            state: Arc::clone(&state),
        }
        .into();

        // Every Add* call, including initial setup, runs on the same
        // process-wide MTA worker used by reconciliation and cancellation.
        // This is stricter than merely serializing each subscription: UIA's
        // registration contract is thread-affine across subscriptions too.
        let initial_automation = ComSend::new(self.automation.clone());
        let initial_windows = ComSend::new(windows);
        let initial_cache = ComSend::new(cache.clone());
        let initial_focus = ComSend::new(focus.clone());
        let initial_watch = ComSend::new(watch.clone());
        let initial_root = ComSend::new(root.clone());
        let initial_state = Arc::clone(&state);
        let initial_ctx = Arc::clone(&ctx);
        on_registration_worker(move || {
            let autom = initial_automation.get();
            let dpi = &crate::dpi::DpiScope::enter()?;
            let cache_request = initial_cache.get();
            let cache = cache_request.get(dpi);
            let focus = initial_focus.get().as_ref();
            let watch = initial_watch.get();
            let root = initial_root.get();

            if let Some(focus) = focus {
                unsafe { autom.AddFocusChangedEventHandler(cache, focus) }.map_err(|e| {
                    Error::Platform {
                        code: e.code().0 as i64,
                        message: format!("AddFocusChangedEventHandler failed: {e}"),
                    }
                })?;
            }

            let cleanup_error = |e: Error| {
                cleanup_registrations(autom, root, watch, focus, &initial_state);
                e
            };
            for window in initial_windows.get() {
                let hwnd = window_handle(window).map_err(&cleanup_error)?;
                let reg = register_window_handlers(
                    autom,
                    window,
                    hwnd,
                    cache_request,
                    &initial_ctx,
                    initial_state.handlers.get(),
                    &initial_state.visual_states,
                )
                .map_err(&cleanup_error)?;
                let mut registrations = initial_state
                    .registered
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                registrations.insert(reg.hwnd, reg);
            }

            for eid in WATCH_EVENT_IDS {
                if let Err(e) = unsafe {
                    autom.AddAutomationEventHandler(*eid, root, TreeScope_Subtree, cache, watch)
                } {
                    return Err(cleanup_error(Error::Platform {
                        code: e.code().0 as i64,
                        message: format!(
                            "AddAutomationEventHandler({eid:?}) on desktop root failed: {e}"
                        ),
                    }));
                }
            }

            // Close the enumeration/watch gap before subscribe returns.
            sync_registrations(&initial_state, cache_request, &initial_ctx, pid)
                .map(|_| ())
                .map_err(cleanup_error)
        })?;

        // Each captured COM interface is wrapped in ComSend so the cancel
        // closure satisfies CancelHandle::new's `Send` bound. See ComSend's
        // doc comment for the safety argument.
        let root_c = ComSend::new(root);
        let focus_c = ComSend::new(focus);
        let watch_c = ComSend::new(watch);
        let state_c = Arc::clone(&state);
        let cancel = CancelHandle::new(move || {
            if let Err(e) = on_registration_worker(move || {
                cleanup_registrations(
                    state_c.automation.get(),
                    root_c.get(),
                    watch_c.get(),
                    focus_c.get().as_ref(),
                    &state_c,
                );
                Ok(())
            }) {
                eprintln!("UIA subscription cancellation failed: {e:?}");
            }
        });

        Ok(Subscription::new(rx, cancel))
    }
}

#[cfg(test)]
#[allow(non_upper_case_globals)]
mod tests {
    #[test]
    fn only_registered_frame_runtime_ids_match_host_process_events() {
        let mut frames =
            HashMap::from([(100usize, HashSet::from([vec![42, 100], vec![42, 100, 1]]))]);
        assert!(known_frame_sender(&[42, 100], &frames));
        assert!(known_frame_sender(&[42, 100, 1], &frames));
        // A different frame of the same ApplicationFrameHost is unrelated.
        assert!(!known_frame_sender(&[42, 200], &frames));
        assert!(!known_frame_sender(&[], &frames));
        frames.remove(&100);
        assert!(!known_frame_sender(&[42, 100], &frames));
    }

    #[test]
    fn a_packaged_app_is_named_by_its_display_name() {
        assert_eq!(
            super::process_app_name(
                r"C:\Program Files\WindowsApps\Microsoft.WindowsCalculator_11.0_x64__8wekyb3d8bbwe\CalculatorApp.exe",
                Some("Calculator".into()),
            ),
            ("Calculator".to_string(), "package")
        );
    }

    #[test]
    fn an_unpackaged_app_is_named_by_its_executable() {
        assert_eq!(
            super::process_app_name(r"C:\Windows\System32\notepad.exe", None),
            ("notepad".to_string(), "process")
        );
    }

    #[implement(IUIAutomationEventHandler)]
    struct BorrowedPayloadProbe(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for BorrowedPayloadProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl IUIAutomationEventHandler_Impl for BorrowedPayloadProbe_Impl {
        fn HandleAutomationEvent(
            &self,
            _: windows::core::Ref<IUIAutomationElement>,
            _: UIA_EVENT_ID,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }
    #[implement(BorrowedPropertyChangedEventHandler)]
    struct NoopPropertyProbe;
    impl BorrowedPropertyChangedEventHandler_Impl for NoopPropertyProbe_Impl {
        unsafe fn property_changed(
            &self,
            _: *mut core::ffi::c_void,
            _: UIA_PROPERTY_ID,
            _: std::mem::ManuallyDrop<VARIANT>,
        ) -> windows::core::HRESULT {
            windows::core::HRESULT(0)
        }
    }
    #[test]
    fn property_callback_keeps_borrowed_variant_payload_alive() {
        use windows::core::Interface;
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let payload: IUIAutomationEventHandler = BorrowedPayloadProbe(Arc::clone(&drops)).into();
        let unknown: windows::core::IUnknown = payload.cast().expect("IUnknown");
        drop(payload);
        let value = VARIANT::from(unknown);
        let handler: BorrowedPropertyChangedEventHandler = NoopPropertyProbe.into();
        let handler: IUIAutomationPropertyChangedEventHandler =
            handler.cast().expect("same native IID");
        unsafe {
            handler.HandlePropertyChangedEvent(
                None::<&IUIAutomationElement>,
                UIA_NamePropertyId,
                &value,
            )
        }
        .expect("no-op callback");
        let prematurely_dropped = drops.load(Ordering::SeqCst);
        if prematurely_dropped != 0 {
            // The upstream trampoline already cleared our borrowed value;
            // clearing its dangling interface pointer a second time is unsafe.
            std::mem::forget(value);
        } else {
            drop(value);
        }
        assert_eq!(
            prematurely_dropped, 0,
            "callback must not release its borrowed payload"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    use super::*;
    use std::time::Duration;

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

    /// A provider for a live-desktop unit test.
    ///
    /// Every failure panics. This used to skip on `E_FAIL`, the first-touch
    /// race the multi-threaded test runner hit; `UIA_FIRST_TOUCH` fixes that
    /// race, and the skip would hide it coming back (issue #500). The
    /// `Option` keeps the call sites' shape.
    fn try_provider() -> Option<WindowsProvider> {
        Some(WindowsProvider::new().unwrap_or_else(|e| panic!("WindowsProvider::new failed: {e}")))
    }

    #[test]
    fn provider_new_succeeds() {
        WindowsProvider::new().expect("WindowsProvider::new must succeed on a desktop");
    }

    #[test]
    fn provider_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WindowsProvider>();
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn intersect_clip_narrows_and_none_clips_nothing() {
        let list = rect(0, 0, 100, 100);
        assert_eq!(intersect_clip(None, None), None);
        assert_eq!(intersect_clip(Some(list), None), Some(list));
        assert_eq!(intersect_clip(None, Some(list)), Some(list));
        assert_eq!(
            intersect_clip(Some(list), Some(rect(50, -20, 200, 60))),
            Some(rect(50, 0, 100, 60))
        );
    }

    #[test]
    fn hidden_reason_prefers_what_the_ancestors_say() {
        // Visible only when nothing hides it.
        assert_eq!(hidden_reason(false, None), None);
        // IsOffscreen alone says only "not drawn".
        assert_eq!(hidden_reason(true, None), Some(HiddenReason::NotShowing));
        // A WPF ListBoxItem scrolled out reports IsOffscreen too; the clip
        // is the more specific answer.
        for offscreen in [false, true] {
            assert_eq!(
                hidden_reason(offscreen, Some(HiddenReason::ScrolledOut)),
                Some(HiddenReason::ScrolledOut)
            );
            assert_eq!(
                hidden_reason(offscreen, Some(HiddenReason::WindowMinimized)),
                Some(HiddenReason::WindowMinimized)
            );
        }
    }

    #[test]
    fn clipped_out_needs_no_overlap_at_all() {
        let clip = Some(rect(0, 0, 100, 100));
        // Inside and partly inside: showing.
        assert!(!clipped_out(rect(10, 10, 90, 30), clip));
        assert!(!clipped_out(rect(10, 90, 90, 110), clip));
        // Scrolled below the viewport, or only touching its edge.
        assert!(clipped_out(rect(10, 120, 90, 140), clip));
        assert!(clipped_out(rect(10, 100, 90, 120), clip));
        // A separator line is judged too: inside, on the edge, and out.
        assert!(!clipped_out(rect(10, 50, 90, 50), clip));
        assert!(!clipped_out(rect(10, 100, 90, 100), clip));
        assert!(clipped_out(rect(10, 150, 90, 150), clip));
        assert!(clipped_out(rect(150, 10, 150, 90), clip));
        // No clip, or no bounds at all: UIA's answer stands.
        assert!(!clipped_out(rect(10, 120, 90, 140), None));
        assert!(!clipped_out(RECT::default(), clip));
        // Disjoint viewports leave an inverted clip that hides everything,
        // including an element spanning both.
        let empty = intersect_clip(clip, Some(rect(200, 200, 300, 300)));
        assert!(clipped_out(rect(10, 10, 90, 30), empty));
        assert!(clipped_out(rect(210, 210, 290, 290), empty));
        assert!(clipped_out(rect(10, 10, 290, 290), empty));
    }

    #[test]
    fn uia_timeouts_are_set_explicitly() {
        let Some(provider) = try_provider() else {
            return;
        };
        let automation2: IUIAutomation2 = windows::core::Interface::cast(&provider.automation)
            .expect("CUIAutomation8 implements IUIAutomation2");
        assert_eq!(
            unsafe { automation2.ConnectionTimeout() }.expect("ConnectionTimeout reads"),
            UIA_CONNECTION_TIMEOUT_MS
        );
        assert_eq!(
            unsafe { automation2.TransactionTimeout() }.expect("TransactionTimeout reads"),
            UIA_TRANSACTION_TIMEOUT_MS
        );
    }

    #[test]
    fn hung_windows_error_is_a_timeout_naming_each_window() {
        let windows: Vec<HungWindow> = (1..=12)
            .map(|i| HungWindow {
                hwnd: HWND(i as *mut core::ffi::c_void),
                pid: 4000 + i,
            })
            .collect();
        let err = hung_windows_error("the desktop to respond".into(), &windows);
        let Error::Timeout { elapsed, diagnosis } = &err else {
            panic!("expected Timeout, got {err:?}");
        };
        assert_eq!(*elapsed, OS_HUNG_THRESHOLD);
        let d = diagnosis.as_deref().expect("diagnosis attached");
        assert_eq!(d.condition.as_deref(), Some("the desktop to respond"));
        let observed = d.last_observed.as_deref().expect("last_observed");
        assert!(observed.contains("12 top-level window(s)"), "{observed}");
        assert!(observed.contains("pid 4001"), "{observed}");
        // Bounded: ten listed plus a remainder line.
        assert_eq!(d.candidates.len(), DIAG_HUNG_WINDOW_LIMIT + 1);
        assert!(d.candidates.last().is_some_and(|c| c.contains("+2 more")));
    }

    #[test]
    fn uia_timeout_maps_to_timeout_and_others_pass_through() {
        let timed_out = Error::Platform {
            code: i64::from(UIA_E_TIMEOUT as i32),
            message: "FindAll".into(),
        };
        let took = Duration::from_millis(2_031);
        let err = uia_timeout_as_timeout(timed_out, took, || "the desktop".into());
        let Error::Timeout { elapsed, diagnosis } = &err else {
            panic!("expected Timeout, got {err:?}");
        };
        // The wait that happened, not a constant (#599).
        assert_eq!(*elapsed, took);
        let d = diagnosis.as_deref().expect("diagnosis attached");
        assert_eq!(d.condition.as_deref(), Some("the desktop"));
        let observed = d.last_observed.as_deref().expect("last_observed");
        assert!(observed.contains("2.0s"), "{observed}");
        assert!(observed.contains("connection timeout"), "{observed}");
        assert!(observed.ends_with(": FindAll"), "{observed}");
        let other = Error::Platform {
            code: -1,
            message: "x".into(),
        };
        assert!(matches!(
            uia_timeout_as_timeout(other, took, || unreachable!()),
            Error::Platform { code: -1, .. }
        ));
    }

    #[test]
    fn uia_timeout_names_the_limit_its_elapsed_time_fits() {
        let txn = Duration::from_millis(u64::from(UIA_TRANSACTION_TIMEOUT_MS));
        assert!(uia_timeout_which(Duration::from_millis(2_000)).contains("connection timeout"));
        assert!(uia_timeout_which(txn - Duration::from_millis(1)).contains("connection timeout"));
        // A long wait fits the transaction timeout but does not prove it.
        assert!(uia_timeout_which(txn).starts_with("consistent with the 20000 ms transaction"));
        assert!(uia_timeout_which(txn + Duration::from_secs(1)).contains("transaction timeout"));
    }

    #[test]
    fn a_probe_miss_counts_as_unresponsive_only_on_timeout_or_hung() {
        let timeout = windows::core::HRESULT::from_win32(ERROR_TIMEOUT.0);
        let denied = windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
        assert_eq!(
            classify_probe_miss(timeout, false, true),
            ProbeMiss::Unresponsive
        );
        // SMTO_ABORTIFHUNG returns at once for a hung window, whatever the
        // last error says.
        assert_eq!(
            classify_probe_miss(denied, true, true),
            ProbeMiss::Unresponsive
        );
        // UIPI refusing a message to an elevated window is not a suspect.
        assert_eq!(classify_probe_miss(denied, false, true), ProbeMiss::Failed);
        assert_eq!(classify_probe_miss(denied, false, false), ProbeMiss::Closed);
    }

    #[test]
    fn probe_report_lists_failed_probes_apart_from_suspects() {
        let window = |i: u32| HungWindow {
            hwnd: HWND(i as usize as *mut core::ffi::c_void),
            pid: 9000 + i,
        };
        let report = ProbeReport {
            total: 3,
            probed: 3,
            unresponsive: vec![window(1)],
            failed: vec![(window(2), "Access is denied. (0x80070005)".to_string())],
            ..ProbeReport::default()
        };
        let summary = report.summary();
        assert!(summary.contains("1 did not answer"), "{summary}");
        assert!(
            summary.contains("1 probe(s) failed and say nothing about responsiveness"),
            "{summary}"
        );
        assert!(summary.contains("probe of HWND 0x2"), "{summary}");
        assert!(summary.contains("(pid 9002"), "{summary}");
        assert!(summary.contains("failed: Access is denied."), "{summary}");
        // Only the window that did not answer is a candidate.
        let candidates = report.candidates();
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert!(candidates[0].contains("pid 9001"), "{candidates:?}");

        let only_failed = ProbeReport {
            total: 1,
            probed: 1,
            failed: vec![(window(3), "x".to_string())],
            ..ProbeReport::default()
        };
        let summary = only_failed.summary();
        assert!(summary.contains("no probed window timed out"), "{summary}");
        assert!(
            !summary.contains("every probed window answered"),
            "{summary}"
        );
        assert!(only_failed.candidates().is_empty());
    }

    /// A fake desktop: (hwnd, control type, pid, name) per child, counting
    /// pid and name reads so the test can see which children were read.
    struct FakeDesktop {
        rows: Vec<(usize, i32, i32, &'static str)>,
        detail_reads: std::cell::RefCell<Vec<usize>>,
    }

    impl DesktopChildren for FakeDesktop {
        type Element = usize;
        fn len(&self) -> Result<i32> {
            Ok(self.rows.len() as i32)
        }
        fn element(&self, i: i32) -> Result<usize> {
            Ok(i as usize)
        }
        fn hwnd(&self, el: &usize, i: i32) -> Result<usize> {
            if self.rows[*el].3 == "<fail hwnd>" {
                return Err(Error::Platform {
                    code: -3,
                    message: format!("hwnd #{i}"),
                });
            }
            Ok(self.rows[*el].0)
        }
        fn control_type(&self, el: &usize, i: i32) -> Result<i32> {
            if self.rows[*el].3 == "<fail control type>" {
                return Err(Error::Platform {
                    code: -4,
                    message: format!("control type #{i}"),
                });
            }
            Ok(self.rows[*el].1)
        }
        fn pid(&self, el: &usize, _: i32) -> Result<i32> {
            self.detail_reads.borrow_mut().push(*el);
            Ok(self.rows[*el].2)
        }
        fn name(&self, el: &usize, i: i32) -> Result<String> {
            if self.rows[*el].3 == "<fail name>" {
                return Err(Error::Platform {
                    code: -5,
                    message: format!("name #{i}"),
                });
            }
            Ok(self.rows[*el].3.to_string())
        }
    }

    #[test]
    fn desktop_listing_keeps_named_owned_windows_in_order_and_returns_every_hwnd() {
        let window = UIA_WindowControlTypeId.0;
        let pane = UIA_PaneControlTypeId.0;
        let desktop = FakeDesktop {
            rows: vec![
                (0x10, pane, 832, "Taskbar"),
                (0x20, window, 100, "Second app"),
                (0x30, window, 0, "no owner"),
                (0x40, window, 200, ""),
                (0, pane, 300, "windowless pane"),
                (0x50, window, 100, "Second app, again"),
                (0x60, window, 400, "Third app"),
                (0x70, pane, 832, "Program Manager"),
            ],
            detail_reads: Default::default(),
        };
        let DesktopListing { windows, returned } =
            desktop_app_windows(&desktop).expect("fake reads succeed");
        // Exactly the children a `ControlType == Window` FindAll followed by
        // the pid and name filters kept, in the order UIA returned them.
        assert_eq!(
            windows,
            vec![(1, 100), (5, 100), (6, 400)],
            "the listing must match the Window-filtered one"
        );
        // Every HWND UIA returned counts, Panes included, except a Window
        // the pid or name filter dropped (0x30, 0x40), which the omission
        // check must probe. A windowless child has none.
        let expected: HashSet<usize> = [0x10, 0x20, 0x50, 0x60, 0x70].into();
        assert_eq!(returned, expected);
        // Non-Window children are not read past their control type.
        assert_eq!(*desktop.detail_reads.borrow(), vec![1, 2, 3, 5, 6]);
    }

    #[test]
    fn desktop_listing_propagates_a_failed_cached_read() {
        for (marker, code) in [
            ("<fail hwnd>", -3),
            ("<fail control type>", -4),
            ("<fail name>", -5),
        ] {
            let desktop = FakeDesktop {
                rows: vec![
                    (0x10, UIA_WindowControlTypeId.0, 7, "fine"),
                    (0x20, UIA_WindowControlTypeId.0, 8, marker),
                ],
                detail_reads: Default::default(),
            };
            match desktop_app_windows(&desktop) {
                Err(Error::Platform { code: c, message }) => {
                    assert_eq!(c, code, "{marker}: {message}");
                    assert!(message.ends_with("#1"), "{marker}: {message}");
                }
                Err(other) => panic!("{marker}: expected Platform, got {other}"),
                Ok(_) => panic!("{marker}: a failed cached read must fail the listing"),
            }
        }
    }

    #[test]
    fn omitted_windows_are_the_interactive_ones_uia_did_not_return() {
        let h = |v: usize| HWND(v as *mut core::ffi::c_void);
        let returned: HashSet<usize> = [0x10, 0x30, 0x99].into();
        let unowned = |hwnd: HWND| hwnd;
        assert_eq!(
            omitted_windows(&[h(0x10), h(0x20), h(0x30), h(0x40)], &returned, unowned),
            vec![h(0x20), h(0x40)]
        );
        assert!(omitted_windows(&[h(0x10)], &returned, unowned).is_empty());
        // Owned windows: UIA lists 0x21 under its owner 0x10, which was
        // returned; 0x41's owner 0x40 was not, so both of those are missing.
        // A window that closed meanwhile has a null root owner.
        let owners = |hwnd: HWND| match hwnd.0 as usize {
            0x21 => h(0x10),
            0x41 => h(0x40),
            0x50 => h(0),
            _ => hwnd,
        };
        assert_eq!(
            omitted_windows(
                &[h(0x10), h(0x21), h(0x40), h(0x41), h(0x50)],
                &returned,
                owners
            ),
            vec![h(0x40), h(0x41), h(0x50)]
        );
    }

    #[test]
    fn only_an_unresponsive_omitted_window_fails_the_listing() {
        let window = |i: u32| HungWindow {
            hwnd: HWND(i as usize as *mut core::ffi::c_void),
            pid: 6000 + i,
        };
        let elapsed = Duration::from_secs(40);
        let benign = ProbeReport {
            scope: "missing",
            total: 3,
            probed: 3,
            answered: vec![window(1)],
            closed: vec![window(2)],
            failed: vec![(window(3), "Access is denied.".to_string())],
            ..ProbeReport::default()
        };
        assert!(dropped_windows_error(&benign, 3, elapsed).is_none());

        let dropped = ProbeReport {
            scope: "missing",
            total: 2,
            probed: 2,
            unresponsive: vec![window(4)],
            answered: vec![window(1)],
            ..ProbeReport::default()
        };
        let err = dropped_windows_error(&dropped, 2, elapsed).expect("an error");
        let Error::Timeout {
            elapsed: reported, ..
        } = &err
        else {
            panic!("expected Error::Timeout, got: {err}");
        };
        assert_eq!(*reported, elapsed);
        let diagnosis = err.diagnosis().expect("a diagnosis");
        let observed = diagnosis.last_observed.clone().unwrap_or_default();
        assert!(
            observed.contains("returned after 40.0s without 2"),
            "{observed}"
        );
        assert!(
            observed.contains("probed 2 of 2 missing window(s)"),
            "{observed}"
        );
        assert!(observed.contains("1 did not answer"), "{observed}");
        assert_eq!(diagnosis.candidates.len(), 1, "{:?}", diagnosis.candidates);
        assert!(diagnosis.candidates[0].contains("(pid 6004"));
    }

    #[test]
    fn only_an_unresponsive_window_turns_a_pid_lookup_into_a_timeout() {
        let window = |i: u32| HungWindow {
            hwnd: HWND(i as usize as *mut core::ffi::c_void),
            pid: 7000,
        };
        let waited = Duration::from_secs(2);
        // No window owned (the process has not shown one yet), and windows
        // that all answer, keep the caller's not-found error.
        let none = ProbeReport {
            scope: "top-level",
            ..ProbeReport::default()
        };
        assert!(unanswered_pid_lookup_error(7000, 0, &none, waited).is_none());
        let answering = ProbeReport {
            scope: "top-level",
            total: 1,
            probed: 1,
            answered: vec![window(1)],
            ..ProbeReport::default()
        };
        assert!(unanswered_pid_lookup_error(7000, 1, &answering, waited).is_none());

        let frozen = ProbeReport {
            scope: "top-level",
            total: 2,
            probed: 2,
            unresponsive: vec![window(2)],
            answered: vec![window(1)],
            ..ProbeReport::default()
        };
        let err = unanswered_pid_lookup_error(7000, 2, &frozen, waited).expect("an error");
        let Error::Timeout {
            elapsed: reported, ..
        } = &err
        else {
            panic!("expected Error::Timeout, got: {err}");
        };
        assert_eq!(*reported, waited);
        let diagnosis = err.diagnosis().expect("a diagnosis");
        assert_eq!(
            diagnosis.condition.as_deref(),
            Some("a top-level window of pid 7000")
        );
        let observed = diagnosis.last_observed.clone().unwrap_or_default();
        assert!(
            observed.contains("UIA's search for the windows of pid 7000"),
            "{observed}"
        );
        assert!(observed.contains("owns 2 interactive"), "{observed}");
        assert_eq!(diagnosis.candidates.len(), 1, "{:?}", diagnosis.candidates);
        assert!(diagnosis.candidates[0].contains("(pid 7000"));
    }

    #[test]
    fn a_failed_window_scan_keeps_the_pid_lookup_not_found_and_records_why() {
        let scan = Error::Platform {
            code: -2147024891,
            message: "EnumWindows failed while probing the windows of a pid: denied".into(),
        };
        let err = not_found_after_failed_scan(7000, &scan);
        assert!(matches!(err, Error::SelectorNotMatched { .. }), "{err}");
        let observed = err
            .diagnosis()
            .and_then(|d| d.last_observed.clone())
            .unwrap_or_default();
        assert!(observed.contains("could not run"), "{observed}");
        assert!(observed.contains("EnumWindows failed"), "{observed}");
    }

    #[test]
    fn desktop_listing_timeout_passes_other_errors_through_unprobed() {
        let other = Error::Platform {
            code: -7,
            message: "x".into(),
        };
        assert!(matches!(
            desktop_listing_timeout(other, Duration::from_secs(1)),
            Error::Platform { code: -7, .. }
        ));
    }

    #[test]
    fn probe_report_is_bounded_and_says_what_it_skipped() {
        let report = ProbeReport {
            total: 80,
            probed: PROBE_WINDOW_LIMIT,
            unresponsive: (1..=12)
                .map(|i| HungWindow {
                    hwnd: HWND(i as *mut core::ffi::c_void),
                    pid: 7000 + i,
                })
                .collect(),
            stopped: Some(format!("window limit of {PROBE_WINDOW_LIMIT} reached")),
            ..ProbeReport::default()
        };
        let candidates = report.candidates();
        assert_eq!(candidates.len(), DIAG_HUNG_WINDOW_LIMIT + 1);
        assert!(candidates[0].contains("pid 7001"), "{candidates:?}");
        assert!(candidates.last().is_some_and(|c| c.contains("+2 more")));
        let summary = report.summary();
        assert!(
            summary.contains(&format!("probed {PROBE_WINDOW_LIMIT} of 80")),
            "{summary}"
        );
        assert!(summary.contains("stopped early: window limit"), "{summary}");
        assert!(summary.contains("12 did not answer"), "{summary}");

        let quiet = ProbeReport {
            total: 3,
            probed: 3,
            ..ProbeReport::default()
        };
        assert!(quiet.candidates().is_empty());
        assert!(
            quiet.summary().contains("every probed window answered"),
            "{}",
            quiet.summary()
        );
    }

    #[test]
    fn probe_of_the_live_desktop_stays_inside_its_budget() {
        let start = std::time::Instant::now();
        let report = probe_unresponsive_windows().expect("EnumWindows works on a desktop");
        let took = start.elapsed();
        // One window may start its 250 ms wait just inside the budget.
        assert!(
            took <= PROBE_BUDGET + Duration::from_millis(u64::from(PROBE_WINDOW_TIMEOUT_MS) + 250),
            "probe took {took:?}: {report:?}"
        );
        assert!(
            report.probed <= report.total.min(PROBE_WINDOW_LIMIT),
            "{report:?}"
        );
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
    fn synthetic_app_requires_matching_process_timestamps() {
        let verified = SyntheticAppIdentity {
            pid: 42,
            creation_time: Some(1000),
        };
        assert!(validate_synthetic_app_identity(7, verified, Some(1000)).is_ok());
        assert!(matches!(
            validate_synthetic_app_identity(7, verified, Some(2000)),
            Err(Error::ElementStale { .. })
        ));

        let unverified = SyntheticAppIdentity {
            pid: 42,
            creation_time: None,
        };
        for (identity, current) in [
            (verified, None),
            (unverified, Some(1000)),
            (unverified, None),
        ] {
            assert!(matches!(
                validate_synthetic_app_identity(7, identity, current),
                Err(Error::ElementStale { .. })
            ));
        }
    }

    #[test]
    fn bounded_handle_cache_evicts_oldest_without_reusing_handles() {
        let mut cache = BoundedHandleCache::new(3);
        for handle in 1..=100 {
            cache.insert(handle, handle * 10);
            assert!(cache.len() <= 3);
        }
        assert!(cache.get(&97).is_none());
        assert_eq!(cache.get(&98), Some(&980));
        assert_eq!(cache.get(&100), Some(&1000));
    }

    #[test]
    fn bounded_handle_cache_does_not_grow_when_a_key_is_updated() {
        let mut cache = BoundedHandleCache::new(2);
        cache.insert(1, "old");
        cache.insert(2, "other");
        cache.insert(1, "new");
        cache.insert(3, "newest");
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&1), Some(&"new"));
        assert!(cache.get(&2).is_none(), "refreshing 1 must make 2 oldest");
    }

    #[test]
    fn repeated_app_enumeration_and_process_churn_stay_bounded() {
        let mut cache = BoundedHandleCache::new(3);
        let live = SyntheticAppIdentity {
            pid: 42,
            creation_time: Some(1_000),
        };
        let first = cache_synthetic_app_identity(&mut cache, live);
        for _ in 0..10_000 {
            assert_eq!(cache_synthetic_app_identity(&mut cache, live), first);
            assert_eq!(cache.len(), 1, "a live process generation must be reused");
        }

        // Simulate a pid being reused by many successive process generations.
        for creation_time in 1_001..11_001 {
            let handle = cache_synthetic_app_identity(
                &mut cache,
                SyntheticAppIdentity {
                    pid: 42,
                    creation_time: Some(creation_time),
                },
            );
            assert_ne!(handle, first, "a reused pid must get a distinct handle");
            assert!(cache.len() <= 3);
        }
    }

    #[test]
    fn legacy_default_action_is_press_only_as_the_sole_activation() {
        // MSAA-only element with a default action: press is DoDefaultAction.
        for framework in [Some("Win32"), Some("WinForm"), Some(""), None] {
            assert!(
                legacy_default_action_is_activation(false, Some("Activate"), framework),
                "{framework:?} is MSAA-proxied"
            );
        }
        // Any UIA activation pattern wins; the default action is never a
        // second mechanism behind it.
        assert!(!legacy_default_action_is_activation(
            true,
            Some("Activate"),
            Some("Win32")
        ));
        // No default action, or a blank one, is nothing to press.
        assert!(!legacy_default_action_is_activation(
            false,
            None,
            Some("Win32")
        ));
        assert!(!legacy_default_action_is_activation(
            false,
            Some(""),
            Some("Win32")
        ));
        assert!(!legacy_default_action_is_activation(
            false,
            Some("  "),
            Some("Win32")
        ));
        // A native UIA provider that withheld activation keeps it withheld,
        // whatever its MSAA default action says (Chromium's "click ancestor").
        for framework in ["Chrome", "XAML", "WPF", "Qt", "DirectUI"] {
            assert!(
                !legacy_default_action_is_activation(
                    false,
                    Some("click ancestor"),
                    Some(framework)
                ),
                "{framework} is a native UIA provider"
            );
        }
    }

    #[test]
    fn default_action_is_batched_for_advertisement() {
        assert!(
            BATCH_PROPERTIES.contains(&UIA_LegacyIAccessibleDefaultActionPropertyId),
            "LegacyIAccessible.DefaultAction must be cached so get_actions can advertise press"
        );
    }

    #[test]
    fn event_cache_keeps_default_action_and_framework_id() {
        let props = event_cache_properties();
        assert!(props.contains(&UIA_LegacyIAccessibleDefaultActionPropertyId));
        assert!(props.contains(&UIA_FrameworkIdPropertyId));
    }

    #[test]
    fn default_action_and_framework_id_are_cached_together() {
        // Without FrameworkId, `is_msaa_framework(None)` reads any element as
        // MSAA-proxied, so a cached DefaultAction alone would advertise
        // `press` on native UIA providers. Every request must carry both or
        // neither.
        for props in [BATCH_PROPERTIES.to_vec(), event_cache_properties()] {
            assert_eq!(
                props.contains(&UIA_LegacyIAccessibleDefaultActionPropertyId),
                props.contains(&UIA_FrameworkIdPropertyId),
                "DefaultAction and FrameworkId must be cached together"
            );
        }
    }

    #[test]
    fn only_a_read_only_set_value_is_text_value_not_supported() {
        use windows::core::{Error as CoreError, HRESULT};
        let read_only = CoreError::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32));
        assert!(matches!(
            value_set_error(read_only, "SetValue"),
            Error::TextValueNotSupported
        ));
        // A dead element or wedged provider keeps its HRESULT (tenet 1).
        let failed = CoreError::from_hresult(HRESULT(E_FAIL.0));
        assert!(matches!(
            value_set_error(failed, "SetValue"),
            Error::Platform { code, .. } if code == E_FAIL.0 as i64
        ));
    }

    // ── Pattern-value reads (issue #597) ────────────────────────────────────

    const BOLD: ReadSite<'static> = ReadSite {
        role: Role::CheckBox,
        name: Some("Bold"),
    };

    #[test]
    fn pattern_read_propagates_a_failed_read_with_its_hresult() {
        use windows::core::{Error as CoreError, HRESULT};
        for hr in [E_FAIL, RPC_E_DISCONNECTED, E_ACCESSDENIED] {
            let result: Result<bool> =
                pattern_read(BOLD, "TogglePattern.CurrentToggleState", || {
                    Err(CoreError::from_hresult(HRESULT(hr.0)))
                });
            match result {
                Err(Error::Platform { code, message }) => {
                    assert_eq!(code, i64::from(hr.0));
                    // The error names the read and the element (tenet 6).
                    assert!(
                        message.contains("TogglePattern.CurrentToggleState on check_box \"Bold\""),
                        "the error names the read and the element: {message}"
                    );
                }
                other => panic!("expected Platform for {hr:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn read_site_names_an_unnamed_element_by_role() {
        let site = ReadSite {
            role: Role::Slider,
            name: None,
        };
        assert_eq!(site.to_string(), "slider");
        assert_eq!(BOLD.to_string(), "check_box \"Bold\"");
    }

    #[test]
    fn pattern_read_keeps_element_not_available_prunable() {
        use windows::core::{Error as CoreError, HRESULT};
        // A tree walk prunes an element that vanished mid-snapshot; that
        // only works while the read's error keeps UIA's exact HRESULT.
        let result: Result<()> = pattern_read(BOLD, "ValuePattern.CurrentIsReadOnly", || {
            Err(CoreError::from_hresult(HRESULT(
                UIA_E_ELEMENTNOTAVAILABLE as i32,
            )))
        });
        assert!(matches!(live_snapshot(result), Ok(None)));
    }

    #[test]
    fn pattern_read_retries_a_transient_failure() {
        let calls = std::cell::Cell::new(0);
        let result = pattern_read(BOLD, "WindowPattern.CurrentIsModal", || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(RPC_E_SERVERCALL_RETRYLATER.ok().expect_err("busy HRESULT"))
            } else {
                Ok(true)
            }
        });
        assert!(matches!(result, Ok(true)));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn value_pattern_text_reports_only_a_withheld_password_as_none() {
        use windows::core::{Error as CoreError, BSTR, HRESULT};
        let field = ReadSite {
            role: Role::TextField,
            name: Some("Password"),
        };
        for password in [false, true] {
            assert_eq!(
                value_pattern_text(field, password, Ok(BSTR::from("hello"))).expect("read"),
                Some("hello".to_string())
            );
            assert_eq!(
                value_pattern_text(field, password, Ok(BSTR::new())).expect("read"),
                None
            );
        }
        let withheld = || CoreError::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32));
        // A password control declines to disclose its value.
        assert_eq!(
            value_pattern_text(field, true, Err(withheld())).expect("withheld"),
            None
        );
        // The same HRESULT from an ordinary field is a failed read.
        assert!(matches!(
            value_pattern_text(field, false, Err(withheld())),
            Err(Error::Platform { code, .. }) if code == i64::from(UIA_E_INVALIDOPERATION as i32)
        ));
        // Anything else is a failed read even on a password field (tenet 1).
        let failed = CoreError::from_hresult(HRESULT(E_FAIL.0));
        assert!(matches!(
            value_pattern_text(field, true, Err(failed)),
            Err(Error::Platform { code, .. }) if code == i64::from(E_FAIL.0)
        ));
    }

    #[test]
    fn toggled_from_state_reports_an_unknown_state_as_unknown() {
        assert_eq!(toggled_from_state(ToggleState_On), Some(Toggled::On));
        assert_eq!(toggled_from_state(ToggleState_Off), Some(Toggled::Off));
        assert_eq!(
            toggled_from_state(ToggleState_Indeterminate),
            Some(Toggled::Mixed)
        );
        // A value outside UIA's three is not `Off`.
        assert_eq!(toggled_from_state(ToggleState(7)), None);
    }

    #[test]
    fn checked_value_from_state_drops_states_without_a_boolean() {
        assert_eq!(checked_value_from_state(ToggleState_On), Some(true));
        assert_eq!(checked_value_from_state(ToggleState_Off), Some(false));
        // Indeterminate is `Mixed` on a re-read, not unchecked (issue #609).
        assert_eq!(checked_value_from_state(ToggleState_Indeterminate), None);
        assert_eq!(checked_value_from_state(ToggleState(7)), None);
    }

    #[test]
    fn expanded_from_state_reports_only_expanded_and_collapsed() {
        assert_eq!(
            expanded_from_state(ExpandCollapseState_Expanded),
            Some(true)
        );
        assert_eq!(
            expanded_from_state(ExpandCollapseState_Collapsed),
            Some(false)
        );
        // Neither is collapsed (issue #609).
        assert_eq!(
            expanded_from_state(ExpandCollapseState_PartiallyExpanded),
            None
        );
        assert_eq!(expanded_from_state(ExpandCollapseState_LeafNode), None);
        assert_eq!(expanded_from_state(ExpandCollapseState(9)), None);
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
    fn expired_synthetic_identity_never_falls_back_to_element_pid() {
        let Some(provider) = try_provider() else {
            return;
        };
        let mut app = ElementData::for_role(Role::Application);
        app.handle = SYNTHETIC_APP_TAG | 9_999_999;
        app.pid = Some(std::process::id());

        assert!(matches!(
            provider.get_children(Some(&app)),
            Err(Error::ElementStale { .. })
        ));
        assert!(matches!(
            provider.subscribe(&app),
            Err(Error::ElementStale { .. })
        ));
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
        assert!(BATCH_PROPERTIES.contains(&UIA_IsEnabledPropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_ProcessIdPropertyId));
        assert!(BATCH_PROPERTIES.contains(&UIA_IsPasswordPropertyId));
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
    fn find_elements_empty_selector_is_invalid() {
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
        let result = provider.find_elements(&root, &empty_selector, None, None);
        assert!(
            matches!(result, Err(Error::InvalidSelector { .. })),
            "an empty selector must be rejected, got {result:?}"
        );
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
        // Repeated subscribe/cancel cycles must remain independent. This also
        // exercises teardown after the handler state maps have been seeded:
        // no cancelled subscription may poison or retain the next one.
        let Ok(first) = provider.subscribe(&el) else {
            return;
        };
        drop(first);
        for _ in 0..32 {
            let sub = provider
                .subscribe(&el)
                .expect("a prior cancellation must not break a new subscription");
            drop(sub);
        }
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
    fn registration_worker_serializes_callers_on_one_thread() {
        // Model concurrent subscriptions/cancellations without fabricating
        // COM handlers: every caller must execute on the same long-lived
        // worker, even when they arrive from different runtime threads.
        let callers: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn(|| {
                    on_registration_worker(|| Ok(std::thread::current().id()))
                        .expect("registration worker must accept a job")
                })
            })
            .collect();
        let worker_threads: Vec<_> = callers
            .into_iter()
            .map(|caller| caller.join().expect("caller thread must not panic"))
            .collect();
        assert!(
            worker_threads.windows(2).all(|pair| pair[0] == pair[1]),
            "every registration job must run on one shared thread"
        );
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
    fn registration_diff_stays_bounded_under_repeated_window_churn() {
        let mut registered = HashSet::new();
        for generation in 0..10_000usize {
            let current: HashSet<usize> =
                (0..4).map(|offset| 0x1000 + generation + offset).collect();
            let (to_add, to_remove) = plan_window_registration_diff(&registered, &current);
            for hwnd in to_remove {
                registered.remove(&hwnd);
            }
            registered.extend(to_add);
            assert_eq!(registered, current);
            assert_eq!(registered.len(), 4);
        }
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
        let all: Vec<UIA_EVENT_ID> = crate::event_plan::AutomationEvent::ALL
            .iter()
            .map(|e| automation_event_id(*e))
            .collect();
        assert!(all.contains(&UIA_MenuOpenedEventId));
        assert!(all.contains(&UIA_MenuClosedEventId));
        assert!(all.contains(&UIA_Text_TextChangedEventId));
        assert!(all.contains(&UIA_SelectionItem_ElementSelectedEventId));
        assert!(all.contains(&UIA_LiveRegionChangedEventId));
        assert!(all.contains(&UIA_SystemAlertEventId));
        // NotificationHandler owns Notification; registering it on the
        // automation handler as well would deliver each request twice.
        assert!(!all.contains(&UIA_NotificationEventId));
        assert!(!all.contains(&UIA_Window_WindowOpenedEventId));
        assert!(!all.contains(&UIA_Window_WindowClosedEventId));
        let unique: HashSet<i32> = all.iter().map(|e| e.0).collect();
        assert_eq!(unique.len(), all.len(), "two plan events share a UIA ID");
    }

    #[test]
    fn property_change_ids_covers_design_doc() {
        // Property IDs mandated by the events design doc for the Windows
        // PropertyChanged pathway.
        let all: Vec<UIA_PROPERTY_ID> = crate::event_plan::WatchedProperty::ALL
            .iter()
            .map(|p| watched_property_id(*p))
            .collect();
        assert!(all.contains(&UIA_NamePropertyId));
        assert!(all.contains(&UIA_IsEnabledPropertyId));
        assert!(all.contains(&UIA_ToggleToggleStatePropertyId));
        assert!(all.contains(&UIA_ValueValuePropertyId));
        assert!(all.contains(&UIA_RangeValueValuePropertyId));
        assert!(all.contains(&UIA_ExpandCollapseExpandCollapseStatePropertyId));
        assert!(all.contains(&UIA_WindowWindowVisualStatePropertyId));
        let unique: HashSet<i32> = all.iter().map(|p| p.0).collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "two plan properties share a UIA ID"
        );
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
    fn visual_state_delta_kinds_reports_only_the_flag_that_changed() {
        // UIA reports the whole visual state on every transition; the delta
        // helper must claim only the flags whose values differ, so a
        // Normal→Minimized transition does not also report Maximized cleared.
        // This is the same derivation `PropertyHandler` and the post-attach
        // baseline reconciliation both emit.
        let minimized = |value| EventKind::StateChanged {
            flag: StateFlag::Minimized,
            value,
        };
        let maximized = |value| EventKind::StateChanged {
            flag: StateFlag::Maximized,
            value,
        };
        assert_eq!(
            visual_state_delta_kinds(WindowVisualState_Normal.0, WindowVisualState_Minimized.0),
            vec![minimized(true)]
        );
        assert_eq!(
            visual_state_delta_kinds(WindowVisualState_Minimized.0, WindowVisualState_Normal.0),
            vec![minimized(false)]
        );
        assert_eq!(
            visual_state_delta_kinds(WindowVisualState_Normal.0, WindowVisualState_Maximized.0),
            vec![maximized(true)]
        );
        assert_eq!(
            visual_state_delta_kinds(WindowVisualState_Maximized.0, WindowVisualState_Normal.0),
            vec![maximized(false)]
        );
        // Minimized → Maximized changes both flags, in flag order.
        assert_eq!(
            visual_state_delta_kinds(WindowVisualState_Minimized.0, WindowVisualState_Maximized.0),
            vec![minimized(false), maximized(true)]
        );
        // No transition, or an unrecognized state on either side: no delta.
        assert!(
            visual_state_delta_kinds(WindowVisualState_Normal.0, WindowVisualState_Normal.0)
                .is_empty()
        );
        assert!(visual_state_delta_kinds(3, WindowVisualState_Normal.0).is_empty());
        assert!(visual_state_delta_kinds(WindowVisualState_Normal.0, 3).is_empty());
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

    #[test]
    fn live_snapshot_prunes_unavailable_candidates_and_keeps_siblings() {
        let unavailable = || Error::Platform {
            code: i64::from(UIA_E_ELEMENTNOTAVAILABLE as i32),
            message: "WindowPattern acquisition: element vanished".into(),
        };
        let candidates = [Ok("first"), Err(unavailable()), Ok("last")];
        let mut snapshots = Vec::new();
        for candidate in candidates {
            if let Some(snapshot) = live_snapshot(candidate).expect("live traversal") {
                snapshots.push(snapshot);
            }
        }
        assert_eq!(snapshots, ["first", "last"]);
    }

    #[test]
    fn live_snapshot_preserves_other_failures_and_process_identity_errors() {
        for hr in [
            E_FAIL,
            E_ACCESSDENIED,
            RPC_E_DISCONNECTED,
            RPC_E_SERVERCALL_RETRYLATER,
        ] {
            let result: Result<()> = Err(Error::Platform {
                code: i64::from(hr.0),
                message: "snapshot failed".into(),
            });
            assert!(
                matches!(live_snapshot(result), Err(Error::Platform { code, .. })
                if code == i64::from(hr.0))
            );
        }
        let result: Result<()> = Err(Error::stale(
            xa11y_core::StaleReason::ProcessRestarted,
            "saved App pid was reused",
        ));
        assert!(matches!(
            live_snapshot(result),
            Err(Error::ElementStale { .. })
        ));
        // A formatted message cannot masquerade as a vanished element.
        let result: Result<()> = Err(Error::Platform {
            code: i64::from(E_FAIL.0),
            message: "UIA_E_ELEMENTNOTAVAILABLE".into(),
        });
        assert!(live_snapshot(result).is_err());
    }

    #[test]
    fn live_snapshot_keeps_a_candidate_that_recovers_during_retry() {
        let calls = std::cell::Cell::new(0);
        let result = live_snapshot(uia_call(|| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(windows::core::HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32)
                    .ok()
                    .expect_err("unavailable HRESULT"))
            } else {
                Ok("recovered")
            }
        }))
        .expect("recovered snapshot");
        assert_eq!(result, Some("recovered"));
        assert_eq!(calls.get(), 2);
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
