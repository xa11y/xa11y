//! Windows desktop coordinates and DPI awareness.
//!
//! UI Automation, GDI capture, `SendInput`, and the UIA window transform
//! pattern use physical desktop pixels when the process is Per-Monitor-V2
//! aware. Windows exposes those same coordinates through xa11y. Keeping the
//! desktop space continuous makes ordinary rectangle arithmetic valid even
//! when a window crosses displays with different DPI scales.

#![cfg(target_os = "windows")]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Once;

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
    SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext, DPI_AWARENESS,
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    DPI_AWARENESS_PER_MONITOR_AWARE, DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE,
};

use xa11y_core::{CaptureMapping, CaptureMappingSegment, Error, Rect, Result};

static PROCESS_AWARENESS: Once = Once::new();

/// Ask for Per-Monitor-V2 awareness for the whole process, once.
///
/// This is the cheap path for a standalone binary or a binding host that has
/// not chosen an awareness: every thread then defaults to per-monitor and
/// [`DpiScope::enter`] never has to switch anything. Windows refuses a second
/// process-awareness setting, so a host that already fixed its own keeps it;
/// that refusal is expected rather than a failure, because each coordinate
/// call is covered by a [`DpiScope`] on its own thread instead (issue #506).
pub fn ensure_process_dpi_aware() {
    PROCESS_AWARENESS.call_once(|| {
        // SAFETY: plain Win32 call with a predefined context constant.
        // Refused when the host fixed its own awareness first; DpiScope
        // covers that case per call, so the result is deliberately unused.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    });
}

/// Per-Monitor-V2 DPI awareness on the calling thread for the lifetime of
/// the guard.
///
/// UIA, GDI capture, `GetSystemMetrics` and `SendInput` all answer in the
/// coordinate space of the *calling thread's* DPI awareness. A thread that is
/// per-monitor aware (V1 or V2) sees physical desktop pixels, which is what
/// xa11y reports. A system-aware thread sees coordinates scaled by
/// system DPI on any monitor whose DPI differs from it, and an unaware thread
/// sees them scaled on every monitor not at 100%. So every xa11y call that
/// reads or produces coordinates enters a scope: a per-monitor thread is left
/// alone, and any other thread is switched to Per-Monitor V2 with
/// `SetThreadDpiAwarenessContext` and switched back when the scope drops.
///
/// The scope covers xa11y's own work: the provider methods it wraps make no
/// direct call back into host code (core runs host predicates *between*
/// provider calls, outside any scope). One indirect case remains. On a host
/// thread that is a single-threaded COM apartment (a GUI thread, or Python
/// with comtypes loaded), an outgoing COM call made inside the scope pumps
/// that apartment's messages while it waits, so the host's own window
/// procedures can run inside the scope, at Per-Monitor V2. A host that reads
/// coordinates in such a window procedure sees physical pixels for its
/// duration.
///
/// It is also the token [`crate::coords`] demands: each coordinate-bearing
/// wrapper takes `&DpiScope`, so a call path without a scope does not
/// compile. The scope is `!Send` (thread awareness is per thread, so a scope
/// entered on one thread proves nothing on another).
#[must_use = "the awareness is restored when the scope drops"]
pub struct DpiScope {
    previous: Option<DPI_AWARENESS_CONTEXT>,
    _not_send: std::marker::PhantomData<*const ()>,
}

thread_local! {
    /// How many times a [`DpiScope`] on this thread actually switched its
    /// awareness. The observable the system-aware host test asserts on, since
    /// a single-monitor 96-DPI runner gives the same coordinates either way.
    static SCOPE_SWITCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// See [`SCOPE_SWITCHES`].
pub fn scope_switches() -> u64 {
    SCOPE_SWITCHES.with(std::cell::Cell::get)
}

/// Whether the calling thread is per-monitor DPI aware (V1 or V2).
pub fn thread_is_per_monitor() -> bool {
    // SAFETY: both calls only read the calling thread's DPI context.
    let awareness = unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
    awareness == DPI_AWARENESS_PER_MONITOR_AWARE
}

impl DpiScope {
    pub fn enter() -> Result<Self> {
        // SAFETY: both calls only read the calling thread's DPI context.
        let awareness =
            unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
        if awareness == DPI_AWARENESS_PER_MONITOR_AWARE {
            return Ok(Self {
                previous: None,
                _not_send: std::marker::PhantomData,
            });
        }
        // SAFETY: plain Win32 call; it returns the previous context, or null
        // when the switch was refused.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2) failed on a thread that \
                     is {}, so bounds, input points, and captures would be read in scaled \
                     coordinates",
                    awareness_name(awareness)
                ),
            });
        }
        SCOPE_SWITCHES.with(|n| n.set(n.get() + 1));
        Ok(Self {
            previous: Some(previous),
            _not_send: std::marker::PhantomData,
        })
    }
}

impl DpiScope {
    /// Whether entering this scope switched the thread's awareness (it was
    /// not already per-monitor).
    pub fn switched(&self) -> bool {
        self.previous.is_some()
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            // SAFETY: restores the context SetThreadDpiAwarenessContext
            // returned for this thread. Restoring a context the thread itself
            // held cannot be refused, and a destructor has nowhere to report
            // to, so a failure is only asserted in debug builds.
            let restored = unsafe { SetThreadDpiAwarenessContext(previous) };
            debug_assert!(
                !restored.0.is_null(),
                "restoring the host thread's DPI awareness failed"
            );
        }
    }
}

/// Make the calling thread Per-Monitor V2 for its whole life. For threads
/// xa11y owns (the UIA registration worker), which run only xa11y code.
pub fn make_thread_per_monitor_aware() -> Result<()> {
    let scope = DpiScope::enter()?;
    // Keep the switch: the thread is xa11y's and never runs host code.
    std::mem::forget(scope);
    Ok(())
}

fn awareness_name(awareness: DPI_AWARENESS) -> &'static str {
    match awareness {
        DPI_AWARENESS_UNAWARE => "DPI unaware",
        DPI_AWARENESS_SYSTEM_AWARE => "system DPI aware",
        DPI_AWARENESS_PER_MONITOR_AWARE => "per-monitor DPI aware",
        _ => "of unknown DPI awareness",
    }
}

/// UIA bounds are already in the Windows desktop coordinate space.
pub fn physical_rect_to_desktop(rect: RECT) -> Rect {
    Rect {
        x: rect.left,
        y: rect.top,
        width: (i64::from(rect.right) - i64::from(rect.left)).max(0) as u32,
        height: (i64::from(rect.bottom) - i64::from(rect.top)).max(0) as u32,
    }
}

/// Capture the physical virtual desktop with an identity coordinate mapping.
pub fn full_capture_plan(scope: &DpiScope) -> Result<(Rect, f32, CaptureMapping)> {
    let monitors = crate::coords::monitor_rects(scope)?;
    let physical = bounding_rect(monitors.iter().copied().map(physical_rect_to_desktop))
        .ok_or_else(|| Error::Platform {
            code: -1,
            message: "no displays were found for screenshot capture".into(),
        })?;
    let mapping = capture_mapping(&monitors, physical)?;
    Ok((physical, 1.0, mapping))
}

/// A requested region is already in physical desktop pixels.
pub fn region_capture_plan(scope: &DpiScope, rect: Rect) -> Result<(Rect, f32, CaptureMapping)> {
    let monitors = crate::coords::monitor_rects(scope)?;
    Ok((rect, 1.0, capture_mapping(&monitors, rect)?))
}

fn capture_mapping(monitors: &[RECT], capture: Rect) -> Result<CaptureMapping> {
    let segments = capture_segments(monitors, capture);
    if segments.is_empty() {
        return Err(Error::Platform {
            code: -1,
            message: "captured rectangle does not intersect any display".into(),
        });
    }
    Ok(CaptureMapping::with_layout_validation(
        segments,
        layout_token(monitors),
        validate_layout_token,
    ))
}

fn capture_segments(monitors: &[RECT], capture: Rect) -> Vec<CaptureMappingSegment> {
    monitors
        .iter()
        .filter_map(|monitor| {
            let left = i64::from(monitor.left).max(i64::from(capture.x));
            let top = i64::from(monitor.top).max(i64::from(capture.y));
            let right =
                i64::from(monitor.right).min(i64::from(capture.x) + i64::from(capture.width));
            let bottom =
                i64::from(monitor.bottom).min(i64::from(capture.y) + i64::from(capture.height));
            (right > left && bottom > top).then_some(CaptureMappingSegment {
                desktop: Rect {
                    x: left as i32,
                    y: top as i32,
                    width: (right - left) as u32,
                    height: (bottom - top) as u32,
                },
                image: Rect {
                    x: (left - i64::from(capture.x)) as i32,
                    y: (top - i64::from(capture.y)) as i32,
                    width: (right - left) as u32,
                    height: (bottom - top) as u32,
                },
            })
        })
        .collect()
}

fn validate_layout_token(expected: u64) -> Result<bool> {
    // Called back from `CaptureMapping` on whatever thread maps a point, so
    // it enters its own scope.
    let scope = DpiScope::enter()?;
    Ok(layout_token(&crate::coords::monitor_rects(&scope)?) == expected)
}

fn layout_token(monitors: &[RECT]) -> u64 {
    let mut values: Vec<_> = monitors
        .iter()
        .map(|m| (m.left, m.top, m.right, m.bottom))
        .collect();
    values.sort_unstable();
    let mut hasher = DefaultHasher::new();
    values.hash(&mut hasher);
    hasher.finish()
}

fn bounding_rect(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.fold(None, |bounds, rect| {
        Some(match bounds {
            None => rect,
            Some(bounds) => {
                let left = bounds.x.min(rect.x);
                let top = bounds.y.min(rect.y);
                let right = (i64::from(bounds.x) + i64::from(bounds.width))
                    .max(i64::from(rect.x) + i64::from(rect.width));
                let bottom = (i64::from(bounds.y) + i64::from(bounds.height))
                    .max(i64::from(rect.y) + i64::from(rect.height));
                Rect {
                    x: left,
                    y: top,
                    width: (right - i64::from(left)) as u32,
                    height: (bottom - i64::from(top)) as u32,
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xa11y_core::{anchor_point, Anchor, Point, Screenshot};

    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
        DPI_AWARENESS_CONTEXT_UNAWARE,
    };

    fn thread_awareness() -> DPI_AWARENESS {
        unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) }
    }

    /// Run `f` on a fresh thread whose awareness starts as `context`.
    fn on_thread_with(context: DPI_AWARENESS_CONTEXT, f: fn()) {
        let context = context.0 as isize;
        std::thread::spawn(move || {
            let previous =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT(context as _)) };
            assert!(!previous.0.is_null(), "test thread awareness can be set");
            f();
        })
        .join()
        .expect("test thread");
    }

    #[test]
    fn scope_switches_a_system_aware_thread_and_restores_it() {
        on_thread_with(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, || {
            assert_eq!(thread_awareness(), DPI_AWARENESS_SYSTEM_AWARE);
            {
                let _scope = DpiScope::enter().expect("scope enters");
                assert_eq!(thread_awareness(), DPI_AWARENESS_PER_MONITOR_AWARE);
            }
            assert_eq!(thread_awareness(), DPI_AWARENESS_SYSTEM_AWARE);
        });
    }

    #[test]
    fn scope_switches_an_unaware_thread_and_restores_it() {
        on_thread_with(DPI_AWARENESS_CONTEXT_UNAWARE, || {
            {
                let _scope = DpiScope::enter().expect("scope enters");
                assert_eq!(thread_awareness(), DPI_AWARENESS_PER_MONITOR_AWARE);
            }
            assert_eq!(thread_awareness(), DPI_AWARENESS_UNAWARE);
        });
    }

    #[test]
    fn scope_leaves_a_per_monitor_v1_thread_alone() {
        on_thread_with(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, || {
            let before = unsafe { GetThreadDpiAwarenessContext() };
            let scope = DpiScope::enter().expect("scope enters");
            assert!(
                scope.previous.is_none(),
                "a per-monitor thread is not switched"
            );
            drop(scope);
            assert_eq!(unsafe { GetThreadDpiAwarenessContext() }.0, before.0);
        });
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
    fn seam_spanning_bounds_center_maps_to_native_input_pixel() {
        // Two adjacent 200% displays. The center of this window is on the
        // second display and must remain within its physical frame.
        let window = physical_rect_to_desktop(rect(3800, 100, 4000, 300));
        assert_eq!(window.width, 200);
        let center = anchor_point(&window, Anchor::Center);
        assert_eq!((center.x, center.y), (3900, 200));
        assert!((3800..4000).contains(&center.x));
    }

    #[test]
    fn negative_origin_and_unequal_scale_have_continuous_geometry() {
        // Scale does not enter desktop geometry; a 150% display can sit to
        // the left of a 200% primary without a coordinate gap.
        let monitors = [rect(-2560, 0, 0, 1440), rect(0, 0, 3840, 2160)];
        let window = physical_rect_to_desktop(rect(-100, 100, 100, 300));
        let center = anchor_point(&window, Anchor::Center);
        assert_eq!((center.x, center.y), (0, 200));
        assert_eq!(window.width, 200);
        let desktop = bounding_rect(monitors.into_iter().map(physical_rect_to_desktop)).unwrap();
        assert_eq!((desktop.x, desktop.width), (-2560, 6400));
    }

    #[test]
    fn capture_region_mapping_is_identity_across_seam() {
        let region = physical_rect_to_desktop(rect(3800, 100, 4000, 300));
        let monitors = [rect(0, 0, 3840, 2160), rect(3840, 0, 7680, 2160)];
        let segments = capture_segments(&monitors, region);
        assert_eq!(segments.len(), 2);
        let shot = Screenshot::new(200, 200, vec![0; 200 * 200 * 4], 1.0)
            .with_mapping(CaptureMapping::new(segments));
        assert_eq!(
            shot.desktop_to_image(Point::new(3900, 200)).unwrap(),
            Point::new(100, 100)
        );
        assert_eq!(
            shot.image_to_desktop(Point::new(100, 100)).unwrap(),
            Point::new(3900, 200)
        );
        assert_eq!(
            shot.desktop_rect_to_image(physical_rect_to_desktop(rect(3820, 120, 3980, 280)))
                .unwrap(),
            vec![
                Rect {
                    x: 20,
                    y: 20,
                    width: 20,
                    height: 160
                },
                Rect {
                    x: 40,
                    y: 20,
                    width: 140,
                    height: 160
                },
            ]
        );
    }

    #[test]
    fn capture_mapping_excludes_holes_and_offscreen_pixels() {
        let monitors = [rect(-1920, 0, 0, 1080), rect(0, -1080, 1920, 0)];
        let capture = Rect {
            x: -1920,
            y: -1080,
            width: 3840,
            height: 2160,
        };
        let segments = capture_segments(&monitors, capture);
        assert_eq!(segments.len(), 2);
        let shot =
            Screenshot::new(3840, 2160, vec![], 1.0).with_mapping(CaptureMapping::new(segments));
        assert_eq!(
            shot.desktop_to_image(Point::new(-100, 100)).unwrap(),
            Point::new(1820, 1180)
        );
        assert!(shot.image_to_desktop(Point::new(100, 100)).is_err());
        assert!(shot.desktop_to_image(Point::new(100, 100)).is_err());
        assert!(capture_mapping(
            &monitors,
            Rect {
                x: 3000,
                y: 3000,
                width: 100,
                height: 100
            }
        )
        .is_err());
    }
}
