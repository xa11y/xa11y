//! JS `screenshot` surface: pixel-level screen capture.
//!
//! Capture runs on the napi worker pool because the underlying platform
//! APIs (ScreenCaptureKit, X11 `GetImage`, BitBlt) can block for tens of
//! milliseconds.
//!
//! The public JS entry point is a single async function — `screenshot(opts?)`
//! — with optional `element` and `region` fields. Dispatch across the three
//! capture shapes happens in `index.js`, which calls one of three
//! underscore-prefixed napi exports here depending on which fields are set.

use napi::bindgen_prelude::{AsyncTask, Buffer, Env, Task};

use crate::element::Element;
use crate::map_err;

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in physical pixels. `scale` is the physical-to-
/// logical ratio (1.0 on standard displays, 2.0 on typical Retina).
/// `pixels.length` equals `width * height * 4`.
#[napi]
pub struct Screenshot {
    inner: xa11y::Screenshot,
}

impl Screenshot {
    fn new(inner: xa11y::Screenshot) -> Self {
        Self { inner }
    }
}

#[napi]
impl Screenshot {
    #[napi(getter)]
    pub fn width(&self) -> u32 {
        self.inner.width
    }

    #[napi(getter)]
    pub fn height(&self) -> u32 {
        self.inner.height
    }

    #[napi(getter)]
    pub fn scale(&self) -> f64 {
        self.inner.scale as f64
    }

    /// Raw RGBA8 pixel bytes (`width * height * 4`).
    #[napi(getter)]
    pub fn pixels(&self) -> Buffer {
        self.inner.pixels.clone().into()
    }

    /// Encode the image as a PNG and return the bytes.
    #[napi]
    pub fn to_png(&self) -> napi::Result<Buffer> {
        let bytes = self.inner.to_png().map_err(map_err)?;
        Ok(bytes.into())
    }

    /// Encode as PNG and write to `path`.
    #[napi]
    pub fn save_png(&self, path: String) -> napi::Result<()> {
        self.inner.save_png(&path).map_err(map_err)
    }
}

pub enum CaptureOp {
    Full,
    Region(xa11y::Rect),
    // Box the Element (~280 bytes) so the enum stays small for hot paths.
    Element(Box<xa11y::Element>),
}

pub struct CaptureTask {
    op: CaptureOp,
}

impl Task for CaptureTask {
    type Output = xa11y::Screenshot;
    type JsValue = Screenshot;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        match &self.op {
            CaptureOp::Full => xa11y::screenshot(),
            CaptureOp::Region(r) => xa11y::screenshot_region(*r),
            CaptureOp::Element(el) => xa11y::screenshot_element(el),
        }
        .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Screenshot::new(output))
    }
}

// ── napi entry points ──────────────────────────────────────────────────
//
// These three free functions correspond 1:1 to the Rust umbrella crate's
// `xa11y::screenshot*` fns. `index.js` hides the split behind a single
// `screenshot(opts?)` wrapper so JS callers never see the underscored names.

/// Capture the full primary display.
#[napi(js_name = "_screenshot", ts_return_type = "Promise<Screenshot>")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn screenshot_full() -> AsyncTask<CaptureTask> {
    AsyncTask::new(CaptureTask {
        op: CaptureOp::Full,
    })
}

/// Capture a sub-rectangle given as `{ x, y, width, height }` in logical
/// screen coordinates (same coordinate space as `Element.bounds`).
#[napi(js_name = "_screenshotRegion", ts_return_type = "Promise<Screenshot>")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn screenshot_region(rect: crate::types::Rect) -> AsyncTask<CaptureTask> {
    AsyncTask::new(CaptureTask {
        op: CaptureOp::Region(xa11y::Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width.max(0) as u32,
            height: rect.height.max(0) as u32,
        }),
    })
}

/// Capture the pixels under an element's current bounds. The target
/// window is **not** raised — see the core docs for rationale.
#[napi(js_name = "_screenshotElement", ts_return_type = "Promise<Screenshot>")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn screenshot_element(element: &Element) -> AsyncTask<CaptureTask> {
    let el = xa11y::Element::new(element.data.clone(), element.provider.clone());
    AsyncTask::new(CaptureTask {
        op: CaptureOp::Element(Box::new(el)),
    })
}
