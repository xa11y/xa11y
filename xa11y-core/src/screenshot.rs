//! Screenshot capture: pixel-level snapshots of the screen or a region.
//!
//! Screenshot is **separate from** both the accessibility action layer
//! ([`crate::Provider`]) and the input-synthesis layer ([`crate::InputProvider`]).
//! Backends that only capture pixels do not know how to read the a11y tree,
//! synthesise input, or raise/activate windows — they are pure pixel readers.
//!
//! # What you get
//!
//! The caller-facing entry points in the `xa11y` umbrella crate
//! (`xa11y::screenshot()`, `xa11y::screenshot_region()`,
//! `xa11y::screenshot_element()`) all return a [`Screenshot`] carrying raw
//! RGBA8 pixels in **physical** (device) pixels — the same resolution the
//! compositor renders at. On HiDPI displays that means pixel dimensions
//! exceed the logical bounds you passed in; [`Screenshot::scale`] records
//! the ratio. Call [`Screenshot::to_png`] or [`Screenshot::save_png`] to
//! encode.
//!
//! # No auto-raise
//!
//! Capturing an element that is occluded or off-screen returns whatever pixels
//! are at those coordinates — the target window is **not** raised or
//! activated. If you need the element in the foreground, do that explicitly
//! before calling `xa11y::screenshot_element`.

use std::path::Path;

use crate::element::Rect;
use crate::error::{Error, Result};

/// Platform backend trait for screen capture.
///
/// Implementors snapshot pixels from a display or a sub-region. They must
/// return **physical** (device) pixels — never downscaled to logical points —
/// and report the scale factor alongside the pixel buffer.
///
/// # Errors
///
/// - [`Error::PermissionDenied`] when the OS denies the capture permission
///   (e.g. macOS Screen Recording).
/// - [`Error::Unsupported`] when the current session has no capture path
///   (e.g. Linux with neither X11 DISPLAY nor a working Wayland portal).
/// - [`Error::Platform`] for raw OS / FFI failures.
pub trait ScreenshotProvider: Send + Sync {
    /// Capture the primary display in full.
    fn capture_full(&self) -> Result<Screenshot>;

    /// Capture a sub-rectangle specified in logical screen coordinates
    /// (the same coordinate space as [`Rect`] in `Element::bounds`).
    fn capture_region(&self, rect: Rect) -> Result<Screenshot>;
}

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in **physical** pixels. `scale` is the ratio of
/// physical to logical (1.0 on standard displays, 2.0 on typical Retina /
/// 1.5/1.75/2.0 on common Windows/Linux HiDPI configurations). `pixels.len()`
/// equals `width * height * 4`.
#[derive(Debug, Clone)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub scale: f32,
}

impl Screenshot {
    /// Encode as PNG and return the bytes.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: "screenshot dimensions overflow".into(),
            })?;
        if self.pixels.len() != expected {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "screenshot pixel buffer size {} does not match {}x{} RGBA ({} bytes)",
                    self.pixels.len(),
                    self.width,
                    self.height,
                    expected
                ),
            });
        }

        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(png_err)?;
            writer.write_image_data(&self.pixels).map_err(png_err)?;
        }
        Ok(out)
    }

    /// Encode as PNG and write to `path`.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.to_png()?;
        std::fs::write(path, bytes).map_err(|e| Error::Platform {
            code: e.raw_os_error().unwrap_or(-1) as i64,
            message: format!("save_png: {e}"),
        })
    }
}

fn png_err(e: png::EncodingError) -> Error {
    Error::Platform {
        code: -1,
        message: format!("png encode: {e}"),
    }
}

// The public entry points — `xa11y::screenshot()`, `screenshot_region()`,
// `screenshot_element()` — live in the umbrella crate (`xa11y/src/lib.rs`)
// so they can construct the platform-specific `ScreenshotProvider` backend
// and memoize it across calls. Keep this file focused on the data (Screenshot)
// and the backend trait (ScreenshotProvider); the umbrella crate composes
// them into the caller-facing API.
