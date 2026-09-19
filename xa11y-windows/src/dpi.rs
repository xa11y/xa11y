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

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use xa11y_core::{CaptureMapping, CaptureMappingSegment, Error, Rect, Result};

static DPI_AWARENESS: Once = Once::new();

/// Set process DPI awareness before the first UIA bounds read or capture.
pub fn ensure_process_dpi_aware() {
    DPI_AWARENESS.call_once(|| {
        // A host may already have fixed its process awareness. Windows then
        // rejects a second setting; the host-awareness limitation is documented.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
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
pub fn full_capture_plan() -> Result<(Rect, f32, CaptureMapping)> {
    let monitors = monitor_rects()?;
    let physical = bounding_rect(monitors.iter().copied().map(physical_rect_to_desktop))
        .ok_or_else(|| Error::Platform {
            code: -1,
            message: "no displays were found for screenshot capture".into(),
        })?;
    let mapping = capture_mapping(&monitors, physical)?;
    Ok((physical, 1.0, mapping))
}

/// A requested region is already in physical desktop pixels.
pub fn region_capture_plan(rect: Rect) -> Result<(Rect, f32, CaptureMapping)> {
    let monitors = monitor_rects()?;
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
    Ok(layout_token(&monitor_rects()?) == expected)
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

fn monitor_rects() -> Result<Vec<RECT>> {
    let mut monitors = Vec::new();
    // SAFETY: the callback receives the pointer unchanged; `monitors` lives
    // until EnumDisplayMonitors returns.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<RECT> as isize),
        )
    }
    .as_bool();
    if !ok {
        return Err(Error::Platform {
            code: -1,
            message: "EnumDisplayMonitors failed while reading desktop layout".into(),
        });
    }
    Ok(monitors)
}

unsafe extern "system" fn collect_monitor(
    _hmonitor: HMONITOR,
    _hdc: HDC,
    lprc: *mut RECT,
    user_data: LPARAM,
) -> windows::core::BOOL {
    let monitors = unsafe { &mut *(user_data.0 as *mut Vec<RECT>) };
    // A null HDC makes the supplied rectangle use virtual-screen pixels.
    // EnumDisplayMonitors keeps this pointer valid for the callback.
    monitors.push(unsafe { *lprc });
    windows::core::BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xa11y_core::{anchor_point, Anchor, Point, Screenshot};

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
