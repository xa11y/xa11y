//! Shared Windows DPI handling.
//!
//! # Why this exists
//!
//! UI Automation reports element bounds, `GetSystemMetrics(SM_*VIRTUALSCREEN)`
//! reports the virtual desktop, and GDI `BitBlt` reads pixels — all in a
//! coordinate space that depends on the **process DPI awareness**. A
//! DPI-unaware process sees system-virtualized (logical) coordinates; a
//! Per-Monitor-V2 process sees true physical pixels. That awareness is a
//! process-wide, set-once flag.
//!
//! Previously it was set lazily on the first screenshot, which meant any UIA
//! bounds read *before* the first screenshot came back in a different
//! coordinate space than the screenshot itself (issue #300). We now:
//!
//! 1. Set Per-Monitor-V2 awareness **eagerly and exactly once**
//!    ([`ensure_process_dpi_aware`]), called from both `WindowsProvider::new`
//!    and `WindowsScreenshot::new`, so it is established before the first
//!    UIA bounds read regardless of which subsystem the consumer touches
//!    first.
//! 2. With awareness pinned to Per-Monitor-V2, UIA bounds and `BitBlt` are
//!    both in **physical** pixels. The provider then converts bounds down to
//!    **logical** coordinates (`xa11y_core::Rect::to_logical`) so that
//!    `Element::bounds` matches the cross-platform contract (logical points,
//!    same as macOS), and the screenshot/input backends convert back up to
//!    physical at the OS boundary using [`scale_for_logical_point`].
//!
//! # Multi-monitor
//!
//! The per-monitor effective DPI is queried for the monitor containing the
//! point of interest. For a uniform-DPI desktop this is exact everywhere. For
//! a **mixed-DPI** multi-monitor desktop the conversion is only exact within a
//! single monitor; a rectangle that straddles a DPI boundary is scaled by the
//! DPI of the monitor under its origin, which can be off by the DPI ratio near
//! the seam. Mixed-DPI straddling windows are rare and this is documented
//! rather than silently "corrected".

#![cfg(target_os = "windows")]

use std::sync::Once;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};

/// The DPI value Windows treats as "100%": one logical unit == one physical
/// pixel. `scale = effective_dpi / USER_DEFAULT_SCREEN_DPI`.
const USER_DEFAULT_SCREEN_DPI: f64 = 96.0;

static DPI_AWARENESS: Once = Once::new();

/// Set the process to Per-Monitor-V2 DPI awareness, at most once per process.
///
/// Idempotent and safe to call from any entry point. The first call sets the
/// awareness; later calls are cheap no-ops (the `Once` short-circuits, and the
/// underlying `SetProcessDpiAwarenessContext` would return `ERROR_ACCESS_DENIED`
/// anyway once awareness is pinned). If a host application already selected an
/// equal or higher awareness, this is a no-op and we keep theirs — we never
/// downgrade.
pub fn ensure_process_dpi_aware() {
    DPI_AWARENESS.call_once(|| {
        // Best-effort: the result is intentionally ignored. Success sets
        // Per-Monitor-V2; failure means awareness was already pinned (by an
        // earlier call or a manifest) to something at least as high, which is
        // exactly what we want. There is no coordinate correctness we could
        // recover by propagating this error — the only requirement is that
        // awareness is >= Per-Monitor-V2 before the first bounds read.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

/// Effective DPI scale (physical/logical) of the monitor containing the given
/// **physical** pixel. Used to convert UIA's physical bounds to logical.
///
/// Returns `1.0` if the monitor or its DPI can't be resolved — a best-effort
/// degradation to identity, matching the "no known scale" convention the
/// other backends use, rather than failing a tree read over a DPI query.
pub fn scale_for_physical_point(x: i32, y: i32) -> f64 {
    scale_for_point(x, y)
}

/// Effective DPI scale (physical/logical) of the monitor containing the given
/// **logical** point. Used to convert logical bounds/points up to physical for
/// `BitBlt` and `SendInput`.
///
/// On a uniform-DPI desktop the logical and physical coordinates identify the
/// same monitor, so this is exact. See the module docs for the mixed-DPI
/// caveat.
pub fn scale_for_logical_point(x: i32, y: i32) -> f64 {
    scale_for_point(x, y)
}

fn scale_for_point(x: i32, y: i32) -> f64 {
    // SAFETY: MonitorFromPoint takes a POINT by value and a flag; it always
    // returns a monitor handle (DEFAULTTONEAREST never returns null for a
    // real desktop). GetDpiForMonitor writes two u32s we own.
    let monitor: HMONITOR = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return 1.0;
    }
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    if hr.is_err() || dpi_x == 0 {
        return 1.0;
    }
    f64::from(dpi_x) / USER_DEFAULT_SCREEN_DPI
}
