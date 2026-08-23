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
//!
//! # Annotation
//!
//! [`annotate`] draws boxes and tags onto a capture: [`Screenshot::annotate`]
//! takes [`Annotation`]s in logical screen coordinates, plus the logical
//! coordinate the capture's own pixel `(0, 0)` sits at, and returns a new
//! capture with them drawn in. That second argument is why
//! [`ScreenshotProvider::capture_full`] returns a pair: what a full capture
//! covers differs per platform, and so does where it starts. That module is
//! pure pixels — it knows nothing about selectors, providers, or platforms.
//!
//! [`legend`] carries the other half of the result: [`Annotated`],
//! [`LegendEntry`], [`Omission`] and [`OmissionReason`] describe *what* was
//! drawn and what could not be. They are built by
//! `xa11y::screenshot_annotated`, which is where selectors are resolved, and
//! live here so the language bindings that surface them are covered by
//! `cargo xtask check-bindings-parity`.

use std::path::Path;

use crate::element::Rect;
use crate::error::{Error, Result};
use crate::input::Point;

pub mod annotate;
pub mod legend;

pub use annotate::{tag_for, Annotation, ANNOTATION_PALETTE};
pub use legend::{Annotated, LegendEntry, Omission, OmissionReason};

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
    /// Capture everything this backend treats as "the screen", and report
    /// **where** those pixels are.
    ///
    /// The returned [`Point`] is the logical screen coordinate that the
    /// capture's pixel `(0, 0)` sits at. It is not always the origin:
    ///
    /// - Windows captures the whole **virtual desktop**, whose top-left is
    ///   `(SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN)` — negative whenever a
    ///   monitor is arranged left of or above the primary one.
    /// - macOS captures one `SCDisplay`, whose `frame.origin` is only `(0, 0)`
    ///   when it is the display at the coordinate-space origin.
    /// - Linux X11 captures the root window, which is at `(0, 0)`.
    ///
    /// Reporting it is the whole reason this method returns a pair. Anything
    /// that maps a logical rectangle (an `Element::bounds`) onto these pixels
    /// — [`Screenshot::annotate`] above all — must subtract this origin
    /// first, and a backend that guessed `(0, 0)` drew every box one
    /// monitor's width out of place with nothing to report it.
    fn capture_full(&self) -> Result<(Screenshot, Point)>;

    /// Capture a sub-rectangle specified in logical screen coordinates
    /// (the same coordinate space as [`Rect`] in `Element::bounds`).
    ///
    /// No origin is returned because `rect` **is** it: an implementation must
    /// capture the pixels at `rect`, so the capture's pixel `(0, 0)` is at
    /// `(rect.x, rect.y)` by contract.
    fn capture_region(&self, rect: Rect) -> Result<Screenshot>;
}

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in **physical** pixels. `scale` is the ratio of
/// physical to logical (1.0 on standard displays, 2.0 on typical Retina /
/// 1.5/1.75/2.0 on common Windows/Linux HiDPI configurations). `pixels.len()`
/// equals `width * height * 4`.
///
/// `#[non_exhaustive]`: capture metadata grows — which display the pixels came
/// from, and the colour space they are in, are both things a backend could
/// start reporting. Build one with [`Screenshot::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub scale: f32,
}

impl Screenshot {
    /// A capture of `width` × `height` physical pixels in RGBA8.
    ///
    /// No validation here — [`Screenshot::to_png`] is where a `pixels` length
    /// that disagrees with the dimensions is reported, so a backend that
    /// builds one can still hand back a partial buffer for inspection.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>, scale: f32) -> Self {
        Self {
            width,
            height,
            pixels,
            scale,
        }
    }

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
