//! macOS screen capture backend via ScreenCaptureKit.
//!
//! Returns physical (device) pixels as RGBA8. Requires the Screen Recording
//! TCC permission — checked at construction time, same as `MacOSProvider`.

use xa11y_core::{CaptureMapping, Error, Point, Rect, Result, Screenshot, ScreenshotProvider};

use crate::ax::MacOSProvider;

extern "C" {
    fn safe_cg_capture_rgba(
        use_rect: i32,
        rect_x: f64,
        rect_y: f64,
        rect_w: f64,
        rect_h: f64,
        out_pixels: *mut *mut u8,
        out_width: *mut u32,
        out_height: *mut u32,
        out_scale: *mut f64,
        out_origin_x: *mut f64,
        out_origin_y: *mut f64,
        step_timeout_ms: i64,
        total_timeout_ms: i64,
    ) -> i32;
    fn safe_cg_free_pixels(pixels: *mut u8);
}

/// How long each ScreenCaptureKit step may take to call back.
///
/// A capture normally takes about 200 ms in all. ScreenCaptureKit can leave a
/// completion handler uncalled (#682), and without a bound the caller would
/// wait forever with no error. Under a caller's deadline the waits also end
/// when it does, so a capture inside a bounded operation cannot outlast it.
const SCK_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct MacOSScreenshot;

impl MacOSScreenshot {
    pub fn new() -> Result<Self> {
        if !MacOSProvider::has_screen_recording_permission() {
            return Err(Error::PermissionDenied {
                instructions: "Enable Screen Recording in System Settings → Privacy & Security → \
                 Screen & System Audio Recording, then restart this process (or the terminal \
                 that launched it): the permission is read once per process."
                    .to_string(),
            });
        }
        Ok(Self)
    }

    /// Capture, and report the **logical** screen coordinate of the returned
    /// image's pixel `(0, 0)`.
    ///
    /// For a region that is the requested rect's origin. For a full capture it
    /// is the captured display's `frame.origin`, which is `(0, 0)` only when
    /// `SCShareableContent.displays[0]` happens to be the display at the
    /// coordinate-space origin — on a multi-display Mac it often is not, and
    /// assuming `(0, 0)` misplaces everything a caller maps onto these pixels.
    fn capture(&self, rect: Option<Rect>) -> Result<(Screenshot, Point)> {
        let (use_rect, rx, ry, rw, rh) = match rect {
            Some(r) => (
                1_i32,
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ),
            None => (0, 0.0, 0.0, 0.0, 0.0),
        };

        // The time left on the caller's deadline bounds both waits together
        // (`-1`: none open). Rounded up, so a deadline a fraction of a
        // millisecond away is not read as already passed.
        let deadline = xa11y_core::Deadline::current();
        let total_timeout_ms = match &deadline {
            None => -1,
            Some(d) => {
                let left = d.remaining();
                if left.is_zero() {
                    return Err(d.expired(Some("a ScreenCaptureKit capture".to_string())));
                }
                if left >= 2 * SCK_STEP_TIMEOUT {
                    // Further away than both steps' own bounds together, so
                    // it cannot end a wait (and stays far from overflowing
                    // the shim's nanosecond arithmetic).
                    -1
                } else {
                    // Below 20 s, so it fits.
                    left.as_micros().div_ceil(1000) as i64
                }
            }
        };

        let mut pixels: *mut u8 = std::ptr::null_mut();
        let mut width: u32 = 0;
        let mut height: u32 = 0;
        let mut scale: f64 = 1.0;
        let mut origin_x: f64 = 0.0;
        let mut origin_y: f64 = 0.0;

        let rc = unsafe {
            safe_cg_capture_rgba(
                use_rect,
                rx,
                ry,
                rw,
                rh,
                &mut pixels,
                &mut width,
                &mut height,
                &mut scale,
                &mut origin_x,
                &mut origin_y,
                SCK_STEP_TIMEOUT.as_millis() as i64,
                total_timeout_ms,
            )
        };

        if rc != 0 || pixels.is_null() {
            // -1: SCShareableContent query failed / returned no displays —
            // typically means the Screen Recording TCC grant is missing for
            // this binary. macOS 15+ silently denies without prompting once
            // a "no" decision is cached.
            let err = match rc {
                -1 => Error::PermissionDenied {
                    instructions: "ScreenCaptureKit could not enumerate displays. Enable Screen \
                                   Recording for this binary in System Settings → Privacy & \
                                   Security → Screen & System Audio Recording."
                        .to_string(),
                },
                -2 => Error::platform(-2, "SCScreenshotManager returned no image"),
                -3 => Error::platform(-3, "pixel buffer allocation failed"),
                -4 => Error::platform(-4, "bitmap context creation failed"),
                -5 => Error::platform(-5, "requested rect has zero/negative dimensions"),
                -6 => sck_timeout(deadline.as_ref(), "SCShareableContent.getShareableContent"),
                -7 => sck_timeout(
                    deadline.as_ref(),
                    "SCScreenshotManager.captureImageWithFilter",
                ),
                -9999 => Error::platform(-9999, "ObjC exception during capture"),
                other => Error::platform(
                    i64::from(other),
                    format!("SCK capture failed with code {other}"),
                ),
            };
            return Err(err);
        }
        if width == 0 || height == 0 {
            unsafe { safe_cg_free_pixels(pixels) };
            return Err(Error::platform(-2, "captured image has zero dimensions"));
        }

        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::platform(-1, "screenshot dimensions overflow"))?;
        let pixels_vec = unsafe { std::slice::from_raw_parts(pixels, size) }.to_vec();
        unsafe { safe_cg_free_pixels(pixels) };

        // The shim reports the origin in logical points; `Point` is integral,
        // matching `Rect` (and therefore `Element::bounds`), so round rather
        // than truncate — a display at y = -899.5 must not become -899.
        let origin = Point::new(origin_x.round() as i32, origin_y.round() as i32);

        Ok((
            Screenshot::new(width, height, pixels_vec, scale as f32)
                .with_mapping(CaptureMapping::single(origin, width, height, scale as f32)),
            origin,
        ))
    }
}

impl ScreenshotProvider for MacOSScreenshot {
    fn capture_full(&self) -> Result<(Screenshot, Point)> {
        self.capture(None)
    }

    fn capture_region(&self, rect: Rect) -> Result<Screenshot> {
        // The origin is `rect`'s own by contract, so there is nothing here the
        // caller does not already have.
        self.capture(Some(rect)).map(|(shot, _)| shot)
    }
}

/// The error for a ScreenCaptureKit step that did not call back in time: the
/// caller's deadline when that is what ended the wait, else the step's own
/// bound.
fn sck_timeout(deadline: Option<&xa11y_core::Deadline>, step: &str) -> Error {
    if let Some(d) = deadline.filter(|d| d.is_expired()) {
        return d.expired(Some(format!("ScreenCaptureKit {step}")));
    }
    Error::timeout(SCK_STEP_TIMEOUT).diagnose(
        xa11y_core::Diagnosis::new()
            .condition(format!(
                "ScreenCaptureKit {step} to call its completion handler"
            ))
            .last_observed(
                "no callback. ScreenCaptureKit has been seen to stall like this in a second \
                 process that captures after another process from the same app did (#682)",
            ),
    )
}
