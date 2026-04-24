//! JS `Screenshotter` / `Screenshot` classes: pixel-level screen capture.
//!
//! Capture runs on the napi worker pool because the underlying platform
//! APIs (ScreenCaptureKit, X11 `GetImage`, BitBlt) can block for tens of
//! milliseconds.

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

/// Screenshot capture façade. Cheap to clone — constructed via the
/// module-level `screenshotter()`.
#[napi]
pub struct Screenshotter {
    inner: xa11y::Screenshotter,
}

impl Screenshotter {
    pub(crate) fn from_inner(inner: xa11y::Screenshotter) -> Self {
        Self { inner }
    }
}

#[napi]
impl Screenshotter {
    /// Capture the full primary display.
    #[napi(ts_return_type = "Promise<Screenshot>")]
    pub fn capture(&self) -> AsyncTask<CaptureTask> {
        AsyncTask::new(CaptureTask {
            inner: self.inner.clone(),
            op: CaptureOp::Full,
        })
    }

    /// Capture a sub-rectangle given as `{ x, y, width, height }` in logical
    /// screen coordinates (same coordinate space as `Element.bounds`).
    #[napi(ts_return_type = "Promise<Screenshot>")]
    pub fn capture_region(&self, rect: crate::types::Rect) -> AsyncTask<CaptureTask> {
        AsyncTask::new(CaptureTask {
            inner: self.inner.clone(),
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
    #[napi(ts_return_type = "Promise<Screenshot>")]
    pub fn capture_element(&self, element: &Element) -> AsyncTask<CaptureTask> {
        let el = xa11y::Element::new(element.data.clone(), element.provider.clone());
        AsyncTask::new(CaptureTask {
            inner: self.inner.clone(),
            op: CaptureOp::Element(el),
        })
    }
}

pub enum CaptureOp {
    Full,
    Region(xa11y::Rect),
    Element(xa11y::Element),
}

pub struct CaptureTask {
    inner: xa11y::Screenshotter,
    op: CaptureOp,
}

impl Task for CaptureTask {
    type Output = xa11y::Screenshot;
    type JsValue = Screenshot;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        match &self.op {
            CaptureOp::Full => self.inner.capture(),
            CaptureOp::Region(r) => self.inner.capture_region(*r),
            CaptureOp::Element(el) => self.inner.capture_element(el),
        }
        .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Screenshot::new(output))
    }
}

/// Construct a [`Screenshotter`] backed by the platform's native capture API.
#[napi(js_name = "screenshotter")]
pub fn make_screenshotter() -> napi::Result<Screenshotter> {
    let inner = xa11y::screenshotter().map_err(map_err)?;
    Ok(Screenshotter::from_inner(inner))
}
