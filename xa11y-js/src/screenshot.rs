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

use napi::bindgen_prelude::{AsyncTask, Buffer, ClassInstance, Env, Task};

use crate::element::Element;
use crate::locator::Locator;
use crate::map_err;
use crate::types::Rect;

/// One drawn annotation box: the tag in the image, and the element it came
/// from.
///
/// A model reads a tag off the PNG; `selector` is what acts on the element
/// that tag labels — `app.locator(entry.selector).press()`.
#[napi(object)]
#[derive(Clone)]
pub struct LegendEntry {
    /// What is drawn in the box — `"B7"`. A group letter and a 1-based index,
    /// with no separator that downscaling could lose.
    pub tag: String,
    /// 1-based, matching the position of this element's locator in
    /// `annotate`.
    pub group: u32,
    /// 1-based, and exactly the `:nth(n)` argument in `selector`.
    pub index: u32,
    /// A selector usable as-is against the same scope the group had.
    pub selector: String,
    /// The element's role, snake_case as everywhere else.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// The element's bounds in logical screen coordinates.
    pub bounds: Rect,
    /// The box colour as an `[r, g, b]` triple, for correlating a box with
    /// its entry by eye.
    pub color: Vec<u32>,
}

impl From<xa11y::LegendEntry> for LegendEntry {
    fn from(e: xa11y::LegendEntry) -> Self {
        Self {
            tag: e.tag,
            group: e.group as u32,
            index: e.index as u32,
            selector: e.selector,
            role: e.role,
            name: e.name,
            bounds: e.bounds.into(),
            color: e.color.iter().map(|c| u32::from(*c)).collect(),
        }
    }
}

/// An element that matched an `annotate` selector but is not in the image.
///
/// Reported rather than dropped: a legend that disagreed with the picture,
/// with no way to find out why, is what this exists to prevent.
#[napi(object)]
#[derive(Clone)]
pub struct Omission {
    /// The selector that would reach this element.
    pub selector: String,
    /// The element's role, snake_case.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// Why it could not be drawn: `no_bounds`, `zero_area` or
    /// `outside_capture`.
    pub reason: String,
}

impl From<xa11y::Omission> for Omission {
    fn from(o: xa11y::Omission) -> Self {
        Self {
            selector: o.selector,
            role: o.role,
            name: o.name,
            // `as_str` is core's own exhaustive match, so a new reason cannot
            // reach JS as a name this binding invented.
            reason: o.reason.as_str().to_string(),
        }
    }
}

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in physical pixels. `scale` is the physical-to-
/// logical ratio (1.0 on standard displays, 2.0 on typical Retina).
/// `pixels.length` equals `width * height * 4`.
///
/// `legend`, `omitted` and `truncated` describe what `annotate` drew. They
/// are `[]`, `[]` and `0` on an unannotated capture, so consumers need no
/// version check.
#[napi]
pub struct Screenshot {
    inner: xa11y::Screenshot,
    legend: Vec<LegendEntry>,
    omitted: Vec<Omission>,
    truncated: u32,
}

impl Screenshot {
    /// Wrap a plain capture — no annotations were requested.
    fn new(inner: xa11y::Screenshot) -> Self {
        Self {
            inner,
            legend: Vec::new(),
            omitted: Vec::new(),
            truncated: 0,
        }
    }

    /// Wrap an annotated capture together with its legend.
    fn annotated(result: xa11y::Annotated) -> Self {
        Self {
            inner: result.screenshot,
            legend: result.legend.into_iter().map(LegendEntry::from).collect(),
            omitted: result.omitted.into_iter().map(Omission::from).collect(),
            truncated: result.truncated as u32,
        }
    }
}

#[napi]
impl Screenshot {
    /// Image width in physical pixels.
    #[napi(getter)]
    pub fn width(&self) -> u32 {
        self.inner.width
    }

    /// Image height in physical pixels.
    #[napi(getter)]
    pub fn height(&self) -> u32 {
        self.inner.height
    }

    /// Physical-to-logical pixel ratio (1.0 on standard displays, 2.0 on
    /// typical Retina, 1.5 / 1.75 / 2.0 on common Windows / Linux HiDPI).
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

    /// One entry per drawn box, in group order and then match order. Empty
    /// unless `annotate` was passed.
    #[napi(getter)]
    pub fn legend(&self) -> Vec<LegendEntry> {
        self.legend.clone()
    }

    /// Elements that matched an `annotate` selector but could not be drawn,
    /// each with the reason.
    #[napi(getter)]
    pub fn omitted(&self) -> Vec<Omission> {
        self.omitted.clone()
    }

    /// How many matched elements were not described at all because the
    /// annotation cap was reached. `0` when the cap did not bite.
    #[napi(getter)]
    pub fn truncated(&self) -> u32 {
        self.truncated
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

// ── Annotated capture ──────────────────────────────────────────────────

/// Resolve `groups` and draw a box over every element each one matches.
///
/// Its own `Task` rather than a fourth `CaptureOp`: the annotated path
/// produces an `xa11y::Annotated`, not a bare `Screenshot`, and widening
/// `CaptureTask::Output` to cover both would make every plain capture carry
/// an empty legend through the worker pool.
pub struct AnnotateTask {
    region: Option<xa11y::Rect>,
    groups: Vec<xa11y::Locator>,
}

impl Task for AnnotateTask {
    type Output = xa11y::Annotated;
    type JsValue = Screenshot;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        xa11y::screenshot_annotated(self.region, &self.groups).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Screenshot::annotated(output))
    }
}

/// Capture with annotations, cropped to an element's bounds or an explicit
/// region.
///
/// `index.js` normalises `annotate` entries — a `Locator` or a selector
/// string — into locators before calling this, so the union never reaches
/// Rust. It also rejects `element` together with `region`, on the same terms
/// as the plain `screenshot()`.
///
/// A bare string normalises to a **rootless** locator, and
/// `xa11y::screenshot_annotated` refuses those: a rootless search runs once
/// per application and concatenates, so each legend entry's
/// `<selector>:nth(n)` would count within one application while the legend
/// counts across all of them. The refusal is core's, before any tree read or
/// capture, and its message names the fix (`app.locator(...)`). Groups must be
/// scoped to an application.
#[napi(
    js_name = "_screenshotAnnotated",
    ts_return_type = "Promise<Screenshot>"
)]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn screenshot_annotated(
    groups: Vec<ClassInstance<Locator>>,
    element: Option<ClassInstance<Element>>,
    region: Option<Rect>,
) -> napi::Result<AsyncTask<AnnotateTask>> {
    // `screenshot_annotated` crops by region, so an `element` target becomes
    // that element's bounds — the same rectangle `_screenshotElement` would
    // have captured, and the same `NoElementBounds` when it has none.
    let region = match element {
        Some(element) => Some(
            element
                .data
                .bounds
                .ok_or_else(|| map_err(xa11y::Error::NoElementBounds))?,
        ),
        None => region.map(|r| xa11y::Rect {
            x: r.x,
            y: r.y,
            width: r.width.max(0) as u32,
            height: r.height.max(0) as u32,
        }),
    };

    Ok(AsyncTask::new(AnnotateTask {
        region,
        groups: groups.iter().map(|g| g.core()).collect(),
    }))
}
