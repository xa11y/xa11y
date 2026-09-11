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
//!    **logical** coordinates ([`physical_rect_to_logical`]) so that
//!    `Element::bounds` matches the cross-platform contract (logical points,
//!    same as macOS), and the screenshot/input backends convert back up to
//!    physical at the OS boundary ([`logical_point_to_physical`] /
//!    [`logical_rect_to_physical`]).
//!
//! # Multi-monitor — origin-preserving logical space
//!
//! Each monitor keeps its **physical origin** under the logical mapping; only
//! the *extent* is divided by that monitor's scale:
//!
//! ```text
//! logical  = monitor_origin + (physical − monitor_origin) / monitor_scale
//! physical = monitor_origin + (logical − monitor_origin) * monitor_scale
//! ```
//!
//! A physical point is converted by the monitor that contains it; a logical
//! point is converted by the monitor whose logical rect contains it. Shown on
//! a 100% primary `[0..1920)` + 200% secondary physical `[1920..4480)`: the
//! secondary's logical rect is `[1920..3200)` — **contiguous and
//! non-overlapping** with the primary's `[0..1920)`, so every logical point
//! belongs to at most one monitor and the mapping is invertible per monitor.
//! (An earlier model divided the physical origin too, which made the
//! secondary's logical rect `[960..2240)` and a bare logical point in the
//! `[960..1920)` seam genuinely ambiguous.)
//!
//! One property of origin preservation is worth documenting: for a monitor
//! placed *left of* the primary at a scale greater than 1, the logical space
//! presents a *gap* rather than an overlap — the monitor's logical rect
//! `[-2560..-1280)` does not reach the primary's `[0..1920)`. A logical point
//! in a gap has no physical locus; [`scale_for_logical_point`] /
//! [`logical_point_to_physical`] fall back to a physical query (which always
//! answers, `DEFAULTTONEAREST`), and the transform is only approximate there.
//! Gaps are strictly better than overlaps: membership is never ambiguous.
//!
//! A rectangle that straddles a DPI boundary is converted by the monitor under
//! its **origin** (the same rule as before): the origin is preserved and the
//! extent is scaled by that monitor's factor, which can be off by the DPI
//! ratio near the seam. Mixed-DPI straddling windows are rare and this is
//! documented rather than silently "corrected". Uniform-DPI desktops are
//! unaffected: with scale 1 everywhere the mapping is an identity.

#![cfg(target_os = "windows")]

use std::sync::Once;

use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};

use xa11y_core::{Error, Rect, Result};

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
/// **logical** point. Used to convert a logical *size* or scale factor up to
/// physical for `BitBlt`, `SendInput`, and the `TransformPattern` window
/// verbs — any conversion that needs only the ratio, not the monitor's origin.
///
/// `MonitorFromPoint` interprets its point in the caller's coordinate space,
/// and this process is pinned to Per-Monitor-V2, so it wants *physical*
/// pixels. A logical point therefore cannot be handed to it directly: on a
/// mixed-DPI desktop its numeric value can land inside another monitor's
/// physical range (the point (200, 200) logical on a 200% secondary monitor
/// is (400, 400) physical, which can fall inside a 100% primary's range).
/// Resolve the monitor against each monitor's logical rect instead (see
/// [`monitor_geometry`]). A monitor enumeration failure is an error, not a
/// fallback: interpreting the logical point as physical could silently pick
/// the wrong monitor's scale (tenet 1).
///
/// Under the origin-preserving model the monitors' logical rects never
/// overlap, so a point resolves to at most one monitor. A point outside every
/// logical rect (a logical gap, or off-screen) falls back to a physical
/// query — the mapping is undefined there and the nearest-monitor answer is
/// the documented approximation.
pub fn scale_for_logical_point(x: i32, y: i32) -> Result<f64> {
    match monitor_for_logical_point(&monitor_geometry()?, x, y) {
        Some(m) => Ok(m.scale),
        None => Ok(scale_for_point(x, y)),
    }
}

/// The monitor whose **logical** rect contains the given logical point.
///
/// `None` when the point lies in a logical gap (a monitor left of the primary
/// at a scale greater than 1 preserves its origin but not the full span
/// between it and the primary — see the module doc) or outside every monitor;
/// callers then fall back to the physical query, whose answer is approximate
/// but never ambiguous.
fn monitor_for_logical_point(
    monitors: &[MonitorGeometry],
    x: i32,
    y: i32,
) -> Option<MonitorGeometry> {
    for m in monitors {
        if logical_rect_contains(m.rect, m.scale, x, y) {
            return Some(MonitorGeometry {
                rect: m.rect,
                scale: m.scale,
            });
        }
    }
    None
}

/// Convert an origin-preserved logical point back to physical.
///
/// Monitor resolved by matching against each monitor's logical rect (which
/// never overlap under this model); a point in a logical gap falls back to
/// the physical query with the same documented approximation as
/// [`scale_for_logical_point`]. This is the inverse of the bounds production
/// in [`physical_rect_to_logical`], so `Element::bounds` fed into
/// `click` / `move_to` round-trips on the same monitor.
pub fn logical_point_to_physical(x: i32, y: i32) -> Result<(i32, i32)> {
    match monitor_for_logical_point(&monitor_geometry()?, x, y) {
        Some(m) => Ok(logical_to_physical_with((m.rect, m.scale), x, y)),
        None => Ok((
            scale_i32(x, scale_for_point(x, y)),
            scale_i32(y, scale_for_point(x, y)),
        )),
    }
}

/// Convert a logical rectangle back to physical (`BitBlt` capture regions).
///
/// The monitor is resolved from the rect's logical origin — the same
/// origin-monitor rule [`physical_rect_to_logical`] applies in the other
/// direction, so a rect that rounds-trips within one monitor is exact. A
/// rect that straddles monitors is scaled by its origin monitor's factor and
/// is approximate near the seam (documented in the module doc).
pub fn logical_rect_to_physical(rect: Rect) -> Result<Rect> {
    match monitor_for_logical_point(&monitor_geometry()?, rect.x, rect.y) {
        Some(m) => Ok(logical_rect_to_physical_with((m.rect, m.scale), rect)),
        None => Ok(rect.to_physical(scale_for_point(rect.x, rect.y))),
    }
}

/// Convert a **physical** UIA rectangle to the origin-preserving logical
/// space (the inverse of [`logical_rect_to_physical`] / [`logical_point_to_physical`]).
///
/// The monitor under the rect's physical origin decides the conversion (the
/// same origin rule the old single-scalar mapping used); that monitor's
/// **physical origin is preserved** and only the extent is divided by its
/// scale. A window fully on the 200% secondary at physical x = 2000 therefore
/// reports logical x = 1960 — not 1000 — and `Element::bounds` on a
/// mixed-DPI desktop is a continuous, non-overlapping space.
pub fn physical_rect_to_logical(rect: RECT) -> Rect {
    match monitor_containing_physical_point(rect.left, rect.top) {
        Some((m, scale)) => physical_to_logical_with((m, scale), rect),
        // The physical origin is not on any monitor (should not happen for a
        // real window): there is no monitor to preserve or scale by, so the
        // rect is reported as-is — the documented approximate fallback.
        None => Rect {
            x: rect.left,
            y: rect.top,
            width: (rect.right - rect.left) as u32,
            height: (rect.bottom - rect.top) as u32,
        },
    }
}

/// Pure physical→logical transform given the monitor (physical rect + scale)
/// under the rect's physical origin. Extracted from [`physical_rect_to_logical`]
/// so the mixed-DPI math is unit-testable without a live desktop.
fn physical_to_logical_with(monitor: (RECT, f64), rect: RECT) -> Rect {
    let (m, scale) = monitor;
    Rect {
        x: m.left + scale_i32(rect.left - m.left, 1.0 / scale),
        y: m.top + scale_i32(rect.top - m.top, 1.0 / scale),
        width: scale_u32((rect.right - rect.left) as u32, 1.0 / scale),
        height: scale_u32((rect.bottom - rect.top) as u32, 1.0 / scale),
    }
}

/// Pure logical→physical point transform given the monitor (physical rect +
/// scale) whose logical rect contains the point. Extracted for tests.
fn logical_to_physical_with(monitor: (RECT, f64), x: i32, y: i32) -> (i32, i32) {
    let (m, scale) = monitor;
    (
        m.left + scale_i32(x - m.left, scale),
        m.top + scale_i32(y - m.top, scale),
    )
}

/// Pure logical→physical rect transform given the monitor (physical rect +
/// scale) under the rect's logical origin. Extracted for tests.
fn logical_rect_to_physical_with(monitor: (RECT, f64), rect: Rect) -> Rect {
    let (m, scale) = monitor;
    Rect {
        x: m.left + scale_i32(rect.x - m.left, scale),
        y: m.top + scale_i32(rect.y - m.top, scale),
        width: scale_u32(rect.width, scale),
        height: scale_u32(rect.height, scale),
    }
}

/// Does a logical point lie inside this monitor's logical rect (its physical
/// origin preserved, extent divided by that monitor's scale)?
///
/// Used as the monitor-identity test for a value already known to belong to a
/// monitor — e.g. a window whose bounds were reported in its own monitor's
/// frame. The monitor's physical origin is part of the logical rect, so under
/// the origin-preserving model monitor logical rects never overlap and a
/// point belongs to at most one monitor.
pub fn logical_rect_contains(rect: RECT, scale: f64, x: i32, y: i32) -> bool {
    let left = rect.left;
    let top = rect.top;
    let right = rect.left + ((rect.right - rect.left) as f64 / scale).ceil() as i32;
    let bottom = rect.top + ((rect.bottom - rect.top) as f64 / scale).ceil() as i32;
    (left..right).contains(&x) && (top..bottom).contains(&y)
}

/// One monitor's physical rect and its effective-DPI scale. Used to build the
/// per-monitor logical rects that [`scale_for_logical_point`] matches against.
struct MonitorGeometry {
    rect: RECT,
    scale: f64,
}

/// All monitors' physical rects and effective-DPI scales, to resolve a logical
/// point against each monitor's logical rect.
fn monitor_geometry() -> Result<Vec<MonitorGeometry>> {
    let mut monitors = Vec::new();
    // SAFETY: the callback receives `user_data` back unchanged, and the
    // referenced `monitors` lives until EnumDisplayMonitors returns.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<MonitorGeometry> as isize),
        )
    }
    .as_bool();
    if !ok {
        // EnumDisplayMonitors returns FALSE only when the enumeration could
        // not run at all. Falling back to the physical query here can pick
        // the wrong monitor's scale on a mixed-DPI desktop — a silent wrong
        // answer, so it is an error (tenet 1).
        return Err(Error::Platform {
            code: -1,
            message: "EnumDisplayMonitors failed while resolving the monitor for a logical point"
                .to_string(),
        });
    }
    Ok(monitors)
}

/// The monitor (physical rect and scale) containing the given **physical**
/// point — e.g. the monitor a window sits on, resolved from its live physical
/// rect. Unlike a logical point, a physical point resolves unambiguously
/// (`MonitorFromPoint` takes physical pixels under Per-Monitor-V2).
pub fn monitor_containing_physical_point(x: i32, y: i32) -> Option<(RECT, f64)> {
    let monitor: HMONITOR = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        Some((info.rcMonitor, dpi_scale_for(monitor)))
    } else {
        None
    }
}

unsafe extern "system" fn collect_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprc: *mut RECT,
    user_data: LPARAM,
) -> windows::core::BOOL {
    let monitors = &mut *(user_data.0 as *mut Vec<MonitorGeometry>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(hmonitor, &mut info) }.as_bool() {
        monitors.push(MonitorGeometry {
            rect: info.rcMonitor,
            scale: dpi_scale_for(hmonitor),
        });
    }
    // BOOL(1) continues the enumeration; BOOL(0) would stop it.
    windows::core::BOOL(1)
}

fn scale_for_point(x: i32, y: i32) -> f64 {
    // SAFETY: MonitorFromPoint takes a POINT by value and a flag; it always
    // returns a monitor handle (DEFAULTTONEAREST never returns null for a
    // real desktop).
    let monitor: HMONITOR = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    dpi_scale_for(monitor)
}

fn dpi_scale_for(monitor: HMONITOR) -> f64 {
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

/// Round `value * scale` to the nearest integer, mirroring
/// `xa11y_core::Rect::to_physical`'s per-field rounding so logical bounds and
/// the physical values derived from them agree within one pixel.
fn scale_i32(value: i32, scale: f64) -> i32 {
    ((value as f64) * scale).round() as i32
}

fn scale_u32(value: u32, scale: f64) -> u32 {
    ((value as f64) * scale).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: i32, top: i32, right: i32, bottom: i32, scale: f64) -> MonitorGeometry {
        MonitorGeometry {
            rect: RECT {
                left,
                top,
                right,
                bottom,
            },
            scale,
        }
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    /// 100% primary (0,0)-(1920,1080); 200% secondary to its right, physical
    /// (1920,0)-(4480,1080) => origin-preserving logical (1920,0)-(3200,540).
    fn primary_plus_secondary() -> Vec<MonitorGeometry> {
        vec![
            monitor(0, 0, 1920, 1080, 1.0),
            monitor(1920, 0, 4480, 1080, 2.0),
        ]
    }

    #[test]
    fn logical_rect_contains_is_origin_preserving_and_non_overlapping() {
        let monitors = primary_plus_secondary();

        // The secondary's logical rect keeps its physical origin 1920: no
        // overlap with the primary's [0..1920), so every logical point is
        // unambiguous.
        assert!(logical_rect_contains(monitors[0].rect, 1.0, 1919, 500));
        assert!(!logical_rect_contains(monitors[0].rect, 1.0, 1920, 500));
        assert!(logical_rect_contains(monitors[1].rect, 2.0, 1920, 100));
        assert!(logical_rect_contains(monitors[1].rect, 2.0, 3199, 100));
        assert!(!logical_rect_contains(monitors[1].rect, 2.0, 3200, 100));
        // The seam point (1920, 100) resolves to the secondary by its own
        // logical rect, not by enumeration order.
        let m = monitor_for_logical_point(&monitors, 1920, 100).expect("seam must resolve");
        assert_eq!(m.scale, 2.0);
        assert_eq!(m.rect.left, 1920);
        // A former-overlap point (1000, 100) is unambiguously the primary's.
        let m =
            monitor_for_logical_point(&monitors, 1000, 100).expect("primary region must resolve");
        assert_eq!(m.scale, 1.0);
    }

    #[test]
    fn mixed_dpi_points_round_trip_physical_to_logical_and_back() {
        let secondary = (rect(1920, 0, 4480, 1080), 2.0);
        // Window on the secondary: physical origin 2000 (inside the
        // secondary), extent 1000x400.
        let phys = rect(2000, 200, 3000, 600);
        let logical = physical_to_logical_with(secondary, phys);
        // Physical (2000, 200) → logical (1920 + (2000-1920)/2, 0 + 200/2) = (1960, 100).
        assert_eq!(
            logical,
            xa11y_core::Rect {
                x: 1960,
                y: 100,
                width: (1000.0_f64 / 2.0).round() as u32,
                height: (400.0_f64 / 2.0).round() as u32,
            }
        );

        // Back up: the round-trip returns the physical bounds exactly.
        let back = logical_rect_to_physical_with(secondary, logical);
        assert_eq!(
            back,
            xa11y_core::Rect {
                x: phys.left,
                y: phys.top,
                width: (phys.right - phys.left) as u32,
                height: (phys.bottom - phys.top) as u32,
            }
        );

        // A point in the primary's region is identity at scale 1.
        let primary = (rect(0, 0, 1920, 1080), 1.0);
        let logical_primary = physical_to_logical_with(primary, rect(100, 50, 500, 350));
        assert_eq!(
            logical_primary,
            xa11y_core::Rect {
                x: 100,
                y: 50,
                width: 400,
                height: 300,
            }
        );
    }

    #[test]
    fn logical_points_round_trip_per_monitor() {
        let primary = (rect(0, 0, 1920, 1080), 1.0);
        let secondary = (rect(1920, 0, 4480, 1080), 2.0);

        // Primary: identity.
        assert_eq!(logical_to_physical_with(primary, 500, 500), (500, 500));
        // Secondary: origin preserved, extent scaled.
        assert_eq!(logical_to_physical_with(secondary, 2000, 100), (2080, 200));
        // Seam: the secondary's own transform.
        assert_eq!(logical_to_physical_with(secondary, 1920, 100), (1920, 200));
    }

    #[test]
    fn left_of_primary_higher_scale_has_a_logical_gap_but_no_overlap() {
        // 200% monitor left of the primary, physical (-2560,0)-(0,1080):
        // origin-preserving logical rect (-2560,0)-(-1280,540). The band
        // [-1280, 0) belongs to no monitor — a gap, not an overlap: a point
        // in it still resolves deterministically (fallback), and the
        // monitor's own region resolves unambiguously.
        let monitors = vec![
            monitor(-2560, 0, 0, 1080, 2.0),
            monitor(0, 0, 1920, 1080, 1.0),
        ];
        assert!(logical_rect_contains(monitors[0].rect, 2.0, -2000, 100));
        assert!(!logical_rect_contains(monitors[0].rect, 2.0, -1000, 100));
        assert!(logical_rect_contains(monitors[1].rect, 1.0, 0, 100));
        assert!(monitor_for_logical_point(&monitors, -1000, 100).is_none());
        // No point matches two monitors: the union is disjoint by
        // construction for every pair of regions tested.
        assert_eq!(
            monitor_for_logical_point(&monitors, -2000, 100)
                .unwrap()
                .scale,
            2.0
        );
        assert_eq!(
            monitor_for_logical_point(&monitors, 100, 100)
                .unwrap()
                .scale,
            1.0
        );
    }

    #[test]
    fn uniform_dpi_single_monitor_is_identity() {
        let monitors = [monitor(0, 0, 1920, 1080, 1.0)];
        assert!(logical_rect_contains(monitors[0].rect, 1.0, 1919, 1079));
        let primary = (monitors[0].rect, 1.0);
        let phys = rect(10, 20, 810, 620);
        let logical = physical_to_logical_with(primary, phys);
        assert_eq!(
            logical,
            xa11y_core::Rect {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
            }
        );
        let back = logical_rect_to_physical_with(primary, logical);
        assert_eq!(
            back,
            xa11y_core::Rect {
                x: phys.left,
                y: phys.top,
                width: (phys.right - phys.left) as u32,
                height: (phys.bottom - phys.top) as u32,
            }
        );
    }
}
