//! Drawing annotations onto a capture: boxes, tag badges, and the bitmap font
//! that writes the tags.
//!
//! This module is the *pure pixel* half of the annotated-screenshot feature.
//! It takes rectangles in logical screen coordinates, a short tag string, and
//! a colour, and it writes RGBA bytes. It knows nothing about selectors,
//! providers, or platforms — the `xa11y` umbrella crate resolves elements into
//! [`Annotation`]s and owns the legend that maps a tag back to an element.
//!
//! Everything here is testable and fuzzable with no display, no application,
//! and no permissions, which is where essentially all of the arithmetic risk
//! in the feature lives.
//!
//! # No new dependency
//!
//! Boxes and badges are written straight into the RGBA buffer with no
//! blending, and tags are drawn from an embedded 5×7 bitmap font covering
//! `0-9` and `A-Z` — 36 glyphs, one `const` table. A TrueType rasteriser plus an
//! embedded font file would cost a few hundred KB in every binary and both
//! wheels for two character classes.
//!
//! # Overflow
//!
//! [`Rect`] is `i32`, [`Screenshot::scale`] is `f32`, and the products index a
//! `Vec<u8>`. Every coordinate is widened to `i64` before any arithmetic, and
//! every write goes through one `set_px`, which range-checks against the
//! image and then converts to a buffer index with `checked_*`. Nothing in this
//! module panics or writes out of bounds for any input; `annotate_ops` in
//! `xa11y/fuzz/fuzz_targets/` asserts that against arbitrary input.

use crate::element::{sane_scale, Rect};
use crate::error::{Error, Result};
use crate::input::Point;
use crate::screenshot::Screenshot;

// ── Public surface ───────────────────────────────────────────────────────

/// Colour-blind-safe qualitative palette (Okabe–Ito, minus black).
///
/// Eight colours in the original; black is dropped because a black box is
/// indistinguishable from dark UI chrome and from the badge foreground.
/// Every entry clears WCAG AA (4.5:1) against the black or white that
/// [`Annotation`]'s badge picks for it — asserted by a unit test in this
/// module.
///
/// Callers assign by group order and wrap with `% ANNOTATION_PALETTE.len()`.
pub const ANNOTATION_PALETTE: [[u8; 3]; 7] = [
    [0xE6, 0x9F, 0x00], // orange          #E69F00
    [0x56, 0xB4, 0xE9], // sky blue        #56B4E9
    [0x00, 0x9E, 0x73], // bluish green    #009E73
    [0xF0, 0xE4, 0x42], // yellow          #F0E442
    [0x00, 0x72, 0xB2], // blue            #0072B2
    [0xD5, 0x5E, 0x00], // vermillion      #D55E00
    [0xCC, 0x79, 0xA7], // reddish purple  #CC79A7
];

/// One box to draw: where, what to write in it, and in what colour.
///
/// `rect` is in **logical** screen coordinates, the same space as
/// `Element::bounds`. [`Screenshot::annotate`] translates it by the capture's
/// origin and converts it to physical pixels.
///
/// `#[non_exhaustive]`: an annotation is built in another crate (the `xa11y`
/// umbrella resolves locators into these), so it owes callers a constructor
/// and a chained setter rather than a struct literal — the same shape as
/// `ClickOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Annotation {
    /// Where to draw, in **logical** screen coordinates.
    pub rect: Rect,
    /// The text drawn in the badge — `"B7"`. See [`tag_for`].
    ///
    /// Characters outside `0-9` and `A-Z` (after ASCII uppercasing) have no
    /// glyph in the embedded font and are not drawn. A tag with no drawable
    /// character gets no badge at all, only the box.
    pub tag: String,
    /// Box and badge colour, RGB. Defaults to `ANNOTATION_PALETTE[0]`.
    pub color: [u8; 3],
}

impl Annotation {
    /// An annotation in the first palette colour. Chain [`Annotation::color`]
    /// to pick another.
    pub fn new(rect: Rect, tag: impl Into<String>) -> Self {
        Self {
            rect,
            tag: tag.into(),
            color: ANNOTATION_PALETTE[0],
        }
    }

    /// Set the box and badge colour.
    #[must_use]
    pub fn color(mut self, rgb: [u8; 3]) -> Self {
        self.color = rgb;
        self
    }
}

/// The tag drawn in an annotation box: a letter for the group, a 1-based
/// number within it — `A1`, `B7`, `C12`.
///
/// **This function is the single place the tag format is decided.** Every
/// other surface (the CLI legend, the MCP result, both bindings) formats tags
/// by calling it, so a change here is a change everywhere and the tests in
/// this module are the only other place it lands.
///
/// A letter followed by digits is *self-delimiting*: there is no separator to
/// lose to compression or downscaling, so no two distinct tags can render to
/// the same glyph sequence. `A12` and `AB2` stay distinct; `1-12` and `11-2`
/// do not. The number is exactly the `:nth(n)` argument for the element.
///
/// Both arguments are **1-based**. Groups past 26 continue `AA`, `AB`, … in
/// the bijective base-26 everyone knows from spreadsheet columns. A `group`
/// or `index` of `0` is read as `1`: this function has no failure channel,
/// and every index in xa11y counts from one.
///
/// ```
/// use xa11y_core::screenshot::tag_for;
///
/// assert_eq!(tag_for(1, 1), "A1");
/// assert_eq!(tag_for(2, 7), "B7");
/// assert_eq!(tag_for(27, 3), "AA3");
/// ```
pub fn tag_for(group: usize, index: usize) -> String {
    let mut n = group.max(1);
    let mut letters = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        // `rem` is 0..=25, so the sum stays inside `b'A'..=b'Z'`.
        letters.push(char::from(b'A' + rem as u8));
        n = (n - 1) / 26;
    }

    let mut tag: String = letters.into_iter().rev().collect();
    tag.push_str(&index.max(1).to_string());
    tag
}

// ── Screenshot::annotate ─────────────────────────────────────────────────

impl Screenshot {
    /// Draw `annotations` onto a copy of this capture.
    ///
    /// `origin` is the logical top-left of what this capture covers, and the
    /// caller takes it from whichever call produced the pixels. For
    /// [`crate::ScreenshotProvider::capture_region`] it is the rect that was
    /// passed in, by that method's contract. For a full capture it is the
    /// [`Point`] returned alongside the image by
    /// [`crate::ScreenshotProvider::capture_full`], which is **not** reliably
    /// `(0, 0)`: Windows captures the virtual desktop, whose top-left goes
    /// negative as soon as a monitor sits left of or above the primary one,
    /// and macOS captures a display that need not be the one at the
    /// coordinate-space origin.
    ///
    /// Each annotation's rect is translated by `-origin` and then scaled to
    /// physical pixels by [`Screenshot::scale`]. Passing `(0, 0)` for a
    /// capture that does not start there shifts every box by the difference,
    /// and nothing here can detect it: the shifted rects still land inside a
    /// capture that wide, so they are drawn over the wrong pixels.
    ///
    /// Returns a **new** [`Screenshot`] (the pixels are cloned; `self` is
    /// never mutated) and the indices of the annotations that were not drawn.
    ///
    /// An annotation whose physical rect does not intersect the image is
    /// **skipped, not clamped**, and its index is in the returned `Vec`. A box
    /// clamped to the edge would claim the wrong pixels, and a zero-area rect
    /// covers nothing at all, so it is skipped for the same reason. An
    /// annotation that is only partly inside is drawn clipped.
    ///
    /// Nothing is deduplicated: two annotations with the same rect are two
    /// boxes, because two selectors matching one element is information the
    /// caller asked for.
    ///
    /// # Drawing
    ///
    /// Boxes are a `stroke`-px outline drawn inside the rect, where `stroke`
    /// is `clamp(round(scale), 1, 4)`. The tag goes in a filled badge in the
    /// same colour, written in whichever of black or white has the higher
    /// WCAG contrast ratio against that colour, and outlined in that same
    /// colour so the badge reads against busy UI. Glyphs are drawn at
    /// `2 × stroke`, so a tag is ten by fourteen *logical* pixels on every
    /// display rather than the five by seven that is illegible at 1×.
    ///
    /// The badge is placed **outside** the box — by preference immediately
    /// above its top-left corner, bottom edge touching the box's top edge —
    /// so it never covers the content the box points at. See [`badge_spot`]
    /// for the fallback order; a badge only lands inside the box when no
    /// outside position is visible, which is what happens to a box that fills
    /// the capture.
    ///
    /// Annotations are drawn largest-area-first so small elements land on top
    /// of the containers that hold them, and a badge that would cover one
    /// already placed moves to another position rather than hiding it.
    ///
    /// A badge may fall outside the image even when its box is visible; it is
    /// clipped like everything else, and it never moves the box or turns a
    /// drawn annotation into a skipped one.
    ///
    /// # Errors
    ///
    /// [`Error::Platform`] when `pixels` does not hold exactly
    /// `width * height * 4` bytes — the same check [`Screenshot::to_png`]
    /// makes, reported here rather than drawn into a buffer whose shape is
    /// unknown.
    pub fn annotate(
        &self,
        annotations: &[Annotation],
        origin: Point,
    ) -> Result<(Screenshot, Vec<usize>)> {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: "annotate: screenshot dimensions overflow".into(),
            })?;
        if self.pixels.len() != expected {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "annotate: screenshot pixel buffer size {} does not match {}x{} RGBA ({} bytes)",
                    self.pixels.len(),
                    self.width,
                    self.height,
                    expected
                ),
            });
        }

        // `sane_scale` collapses a non-finite or non-positive scale to 1.0,
        // the same treatment `Rect::to_physical` applies internally — taking
        // it here keeps the stroke width consistent with the geometry.
        let scale = sane_scale(f64::from(self.scale));
        // In 1.0..=4.0 after the clamp, so the cast is exact.
        let unit = scale.round().clamp(1.0, 4.0) as i64;
        // Badge glyphs are drawn at twice the stroke unit — floor 2, so a tag
        // is 10×14 px on a 1× display instead of the 5×7 that is legible only
        // zoomed in, and the apparent size stays constant as `scale` grows.
        // `unit` is 1..=4, so this is 2..=8 with no risk of overflow.
        let glyph_unit = unit * 2;

        let mut canvas = Canvas {
            width: i64::from(self.width),
            height: i64::from(self.height),
            pixels: self.pixels.clone(),
        };
        let bounds = canvas.bounds();

        let mut skipped = Vec::new();
        let mut visible: Vec<(usize, &Annotation, PxRect)> = Vec::new();
        for (i, ann) in annotations.iter().enumerate() {
            // Saturating: a rect at `i32::MIN` translated by a positive
            // origin stays at `i32::MIN`, which is still far outside any
            // image, so saturation cannot pull an off-screen box into view.
            let translated = Rect {
                x: ann.rect.x.saturating_sub(origin.x),
                y: ann.rect.y.saturating_sub(origin.y),
                width: ann.rect.width,
                height: ann.rect.height,
            };
            let physical = PxRect::from_rect(translated.to_physical(scale));
            if physical.overlaps(bounds) {
                visible.push((i, ann, physical));
            } else {
                skipped.push(i);
            }
        }

        // Largest first, so a small element's box and badge land on top of the
        // window or group that contains it. Ties keep input order.
        visible.sort_by(|(ai, _, ar), (bi, _, br)| br.area().cmp(&ar.area()).then(ai.cmp(bi)));

        let mut placed: Vec<PxRect> = Vec::new();
        for (_, ann, rect) in visible {
            draw_box(&mut canvas, rect, unit, ann.color);
            if let Some(badge) = BadgeMetrics::for_tag(&ann.tag, glyph_unit, unit) {
                let spot = badge_spot(rect, badge.width, badge.height, unit, bounds, &placed);
                let fg = foreground_for(ann.color);
                // Outline first, then the fill inset into it: two rectangles
                // rather than four bands, and `fill` clips both.
                canvas.fill(spot, fg);
                canvas.fill(badge.fill_area(spot), ann.color);
                draw_tag(
                    &mut canvas,
                    &ann.tag,
                    spot.x0.saturating_add(badge.inset),
                    spot.y0.saturating_add(badge.inset),
                    badge.unit,
                    fg,
                );
                placed.push(spot);
            }
        }

        Ok((
            Screenshot::new(self.width, self.height, canvas.pixels, self.scale),
            skipped,
        ))
    }
}

// ── Geometry ─────────────────────────────────────────────────────────────

/// A half-open rectangle in physical pixels, widened to `i64`.
///
/// Every coordinate in this module lives here rather than in [`Rect`]: `i32`
/// positions plus `u32` sizes plus a scale factor overflow `i32` easily, and
/// `i64` holds any product of them without a checked operation per line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PxRect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl PxRect {
    fn from_rect(r: Rect) -> Self {
        let x0 = i64::from(r.x);
        let y0 = i64::from(r.y);
        // i32 + u32 both widened: the sums cannot overflow i64.
        Self {
            x0,
            y0,
            x1: x0 + i64::from(r.width),
            y1: y0 + i64::from(r.height),
        }
    }

    fn at(x0: i64, y0: i64, width: i64, height: i64) -> Self {
        Self {
            x0,
            y0,
            x1: x0.saturating_add(width),
            y1: y0.saturating_add(height),
        }
    }

    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    fn area(self) -> i64 {
        if self.is_empty() {
            0
        } else {
            (self.x1 - self.x0).saturating_mul(self.y1 - self.y0)
        }
    }

    /// The part of `self` inside `clip`, empty when they do not meet.
    fn clipped_to(self, clip: Self) -> Self {
        Self {
            x0: self.x0.max(clip.x0),
            y0: self.y0.max(clip.y0),
            x1: self.x1.min(clip.x1),
            y1: self.y1.min(clip.y1),
        }
    }

    /// True when every pixel of `self` is inside `other`. An empty rectangle
    /// has no pixels outside anything, so it is contained.
    fn contained_by(self, other: Self) -> bool {
        self.is_empty()
            || (self.x0 >= other.x0
                && self.y0 >= other.y0
                && self.x1 <= other.x1
                && self.y1 <= other.y1)
    }

    /// True when the two rectangles share at least one pixel. An empty
    /// rectangle overlaps nothing, which is what makes a zero-area annotation
    /// skipped rather than drawn.
    fn overlaps(self, other: Self) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x0 < other.x1
            && other.x0 < self.x1
            && self.y0 < other.y1
            && other.y0 < self.y1
    }
}

// ── Canvas ───────────────────────────────────────────────────────────────

/// The RGBA8 buffer being drawn into, plus its dimensions as `i64`.
struct Canvas {
    width: i64,
    height: i64,
    pixels: Vec<u8>,
}

impl Canvas {
    fn bounds(&self) -> PxRect {
        PxRect {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        }
    }

    /// Write one opaque pixel. Out-of-range coordinates, an index that does
    /// not fit `usize`, and a buffer shorter than the index all return without
    /// writing — this is the one place bytes are stored, so it is the one
    /// place that has to be right.
    fn set_px(&mut self, x: i64, y: i64, rgb: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let Some(index) = y
            .checked_mul(self.width)
            .and_then(|row| row.checked_add(x))
            .and_then(|offset| offset.checked_mul(4))
        else {
            return;
        };
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(end) = index.checked_add(4) else {
            return;
        };
        if end > self.pixels.len() {
            return;
        }
        self.pixels[index..end].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }

    /// Fill `rect`, clipped to the image. A rect entirely outside costs one
    /// comparison, so a caller may pass wildly out-of-range geometry.
    fn fill(&mut self, rect: PxRect, rgb: [u8; 3]) {
        let x0 = rect.x0.max(0);
        let y0 = rect.y0.max(0);
        let x1 = rect.x1.min(self.width);
        let y1 = rect.y1.min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                self.set_px(x, y, rgb);
            }
        }
    }
}

// ── Boxes and badges ─────────────────────────────────────────────────────

/// Draw a `stroke`-px outline *inside* `rect`. On a box thinner than the
/// stroke the bands collapse onto each other and the box is drawn solid,
/// which is the honest rendering of a 2px-wide element at 4× stroke.
fn draw_box(canvas: &mut Canvas, rect: PxRect, stroke: i64, rgb: [u8; 3]) {
    if rect.is_empty() {
        return;
    }
    let sx = stroke.min(rect.x1 - rect.x0);
    let sy = stroke.min(rect.y1 - rect.y0);

    canvas.fill(PxRect::at(rect.x0, rect.y0, rect.x1 - rect.x0, sy), rgb);
    canvas.fill(
        PxRect::at(rect.x0, rect.y1 - sy, rect.x1 - rect.x0, sy),
        rgb,
    );
    canvas.fill(PxRect::at(rect.x0, rect.y0, sx, rect.y1 - rect.y0), rgb);
    canvas.fill(
        PxRect::at(rect.x1 - sx, rect.y0, sx, rect.y1 - rect.y0),
        rgb,
    );
}

/// The size of one tag badge and where its glyphs sit inside it.
///
/// A badge is an `outline`-px border in the tag's foreground colour, then
/// `unit` px of padding, then the glyphs. The border is what separates a badge
/// from whatever UI it lands on: the fill alone is one flat colour that a
/// screenshot of a colourful toolbar can swallow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BadgeMetrics {
    width: i64,
    height: i64,
    /// Glyph pixel size.
    unit: i64,
    /// Border thickness, drawn in the foreground colour.
    outline: i64,
    /// `outline + padding`: the glyph origin, relative to the badge's
    /// top-left.
    inset: i64,
}

impl BadgeMetrics {
    /// Metrics for `tag`, or `None` when the tag has no drawable glyph — an
    /// empty tag gets a box and no badge rather than an empty coloured square.
    fn for_tag(tag: &str, unit: i64, outline: i64) -> Option<Self> {
        // Saturating rather than fallible: a tag longer than `i64::MAX` glyphs
        // is a width the badge clips to the image anyway, so there is no
        // failure to report — only a number too large to represent.
        let glyphs = i64::try_from(tag.chars().filter_map(glyph_index).count()).unwrap_or(i64::MAX);
        if glyphs == 0 {
            return None;
        }
        // Glyphs, one `unit` of tracking between them, one `unit` of padding
        // and `outline` px of border on every side. Saturating because
        // `glyphs` is caller-controlled; `unit` is 2..=8 and `outline` 1..=4,
        // so every other product here is small and exact.
        let text_w = glyphs
            .saturating_mul(GLYPH_W * unit)
            .saturating_add((glyphs - 1).saturating_mul(unit));
        let inset = unit + outline;
        Some(Self {
            width: text_w.saturating_add(2 * inset),
            height: GLYPH_H * unit + 2 * inset,
            unit,
            outline,
            inset,
        })
    }

    /// The coloured fill inside the outline, for a badge placed at `spot`.
    fn fill_area(self, spot: PxRect) -> PxRect {
        PxRect {
            x0: spot.x0.saturating_add(self.outline),
            y0: spot.y0.saturating_add(self.outline),
            x1: spot.x1.saturating_sub(self.outline),
            y1: spot.y1.saturating_sub(self.outline),
        }
    }
}

/// Pick where a badge goes, preferring **outside** the box so it never covers
/// the element it points at.
///
/// The badge used to sit at the box's inner top-left, which on a toolbar
/// button put `A1` squarely on top of the button's own label — the annotation
/// destroying the content it exists to point at. Candidates are now tried in
/// this order:
///
/// 1. above the box, left-aligned (bottom edge touching the box's top edge),
/// 2. above the box, right-aligned,
/// 3. below the box, left-aligned, then right-aligned,
/// 4. beside the box, to its left, then to its right,
/// 5. the four inner corners, as a last resort.
///
/// Each candidate is scored `(is visible at all, covers no placed badge, is
/// wholly on-image, visible area)`, compared in that order, and the first
/// candidate holding the best score wins. So an off-image preferred position
/// yields to the next one that fits, a box filling the capture — which has no
/// outside position on the image at all — falls through to the inner corners,
/// and a badge still lands somewhere visible rather than nowhere.
///
/// Collision avoidance is why the *placed* badges are consulted: nested
/// elements (window ⊃ group ⊃ button) share corners, and without it every
/// badge in a stack but the last would be unreadable.
fn badge_spot(
    rect: PxRect,
    width: i64,
    height: i64,
    stroke: i64,
    image: PxRect,
    placed: &[PxRect],
) -> PxRect {
    let outer_left = rect.x0;
    let outer_right = rect.x1.saturating_sub(width);
    let above = rect.y0.saturating_sub(height);
    let below = rect.y1;
    let inner_left = rect.x0.saturating_add(stroke);
    let inner_top = rect.y0.saturating_add(stroke);
    let inner_right = rect.x1.saturating_sub(stroke).saturating_sub(width);
    let inner_bottom = rect.y1.saturating_sub(stroke).saturating_sub(height);

    let candidates = [
        PxRect::at(outer_left, above, width, height),
        PxRect::at(outer_right, above, width, height),
        PxRect::at(outer_left, below, width, height),
        PxRect::at(outer_right, below, width, height),
        PxRect::at(rect.x0.saturating_sub(width), rect.y0, width, height),
        PxRect::at(rect.x1, rect.y0, width, height),
        PxRect::at(inner_left, inner_top, width, height),
        PxRect::at(inner_right, inner_top, width, height),
        PxRect::at(inner_left, inner_bottom, width, height),
        PxRect::at(inner_right, inner_bottom, width, height),
    ];

    let mut best: Option<((bool, bool, bool, i64), PxRect)> = None;
    for candidate in candidates {
        let visible = candidate.clipped_to(image).area();
        let free = !placed.iter().any(|p| p.overlaps(candidate));
        let key = (visible > 0, free, candidate.contained_by(image), visible);
        // Strictly greater, so an earlier candidate keeps a tied score and the
        // preference order above decides.
        if best.is_none_or(|(best_key, _)| key > best_key) {
            best = Some((key, candidate));
        }
    }
    // `candidates` is non-empty, so the loop always sets `best`; the fallback
    // keeps that from being an `unwrap`.
    best.map_or(candidates[0], |(_, spot)| spot)
}

// ── Contrast ─────────────────────────────────────────────────────────────

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

/// WCAG relative luminance: sRGB channels linearised, then weighted
/// `0.2126 R + 0.7152 G + 0.0722 B`.
fn relative_luminance(rgb: [u8; 3]) -> f64 {
    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// WCAG contrast ratio `(L1 + 0.05) / (L2 + 0.05)`, lighter over darker.
fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Black or white, whichever reads better on `background`.
fn foreground_for(background: [u8; 3]) -> [u8; 3] {
    if contrast_ratio(BLACK, background) >= contrast_ratio(WHITE, background) {
        BLACK
    } else {
        WHITE
    }
}

// ── Text ─────────────────────────────────────────────────────────────────

/// Glyph width in font pixels.
const GLYPH_W: i64 = 5;
/// Glyph height in font pixels.
const GLYPH_H: i64 = 7;

/// Index into [`FONT`] for a character, or `None` when the font has no glyph.
///
/// `0-9` map to `0..10` and `A-Z` to `10..36`; lowercase ASCII is uppercased
/// first. Tags come from [`tag_for`], which only ever produces those.
fn glyph_index(c: char) -> Option<usize> {
    let c = c.to_ascii_uppercase();
    match c {
        '0'..='9' => Some(c as usize - '0' as usize),
        'A'..='Z' => Some(10 + c as usize - 'A' as usize),
        _ => None,
    }
}

/// Draw `tag` with its top-left at (`x`, `y`), each font pixel an
/// `unit`×`unit` square.
fn draw_tag(canvas: &mut Canvas, tag: &str, x: i64, y: i64, unit: i64, rgb: [u8; 3]) {
    let advance = GLYPH_W * unit + unit;
    let mut pen = x;
    for glyph in tag.chars().filter_map(glyph_index) {
        if pen >= canvas.width {
            // Glyphs only advance rightwards, so nothing after this is visible.
            break;
        }
        draw_glyph(canvas, glyph, pen, y, unit, rgb);
        pen = pen.saturating_add(advance);
    }
}

fn draw_glyph(canvas: &mut Canvas, glyph: usize, x: i64, y: i64, unit: i64, rgb: [u8; 3]) {
    let Some(rows) = FONT.get(glyph) else {
        return;
    };
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..GLYPH_W {
            // Bit `GLYPH_W - 1` is the leftmost column.
            if *bits & (1_u8 << (GLYPH_W - 1 - col)) == 0 {
                continue;
            }
            let px = x.saturating_add(col * unit);
            let py = y.saturating_add(row as i64 * unit);
            canvas.fill(PxRect::at(px, py, unit, unit), rgb);
        }
    }
}

/// A 5×7 bitmap font: `0-9` then `A-Z`, one `u8` per row with bit 4 the
/// leftmost column. Indexed by [`glyph_index`].
///
/// Deliberately the whole typographic story of this module. Rendering two
/// character classes does not justify a TrueType rasteriser and an embedded
/// font file in every binary and both wheels.
#[rustfmt::skip]
const FONT: [[u8; 7]; 36] = [
    // '0'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10011, // #..##
        0b10101, // #.#.#
        0b11001, // ##..#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // '1'
    [
        0b00100, // ..#..
        0b01100, // .##..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b01110, // .###.
    ],
    // '2'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b00001, // ....#
        0b00010, // ...#.
        0b00100, // ..#..
        0b01000, // .#...
        0b11111, // #####
    ],
    // '3'
    [
        0b11111, // #####
        0b00010, // ...#.
        0b00100, // ..#..
        0b00010, // ...#.
        0b00001, // ....#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // '4'
    [
        0b00010, // ...#.
        0b00110, // ..##.
        0b01010, // .#.#.
        0b10010, // #..#.
        0b11111, // #####
        0b00010, // ...#.
        0b00010, // ...#.
    ],
    // '5'
    [
        0b11111, // #####
        0b10000, // #....
        0b11110, // ####.
        0b00001, // ....#
        0b00001, // ....#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // '6'
    [
        0b00110, // ..##.
        0b01000, // .#...
        0b10000, // #....
        0b11110, // ####.
        0b10001, // #...#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // '7'
    [
        0b11111, // #####
        0b00001, // ....#
        0b00010, // ...#.
        0b00100, // ..#..
        0b01000, // .#...
        0b01000, // .#...
        0b01000, // .#...
    ],
    // '8'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // '9'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b01111, // .####
        0b00001, // ....#
        0b00010, // ...#.
        0b01100, // .##..
    ],
    // 'A'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b11111, // #####
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
    ],
    // 'B'
    [
        0b11110, // ####.
        0b10001, // #...#
        0b10001, // #...#
        0b11110, // ####.
        0b10001, // #...#
        0b10001, // #...#
        0b11110, // ####.
    ],
    // 'C'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
        0b10001, // #...#
        0b01110, // .###.
    ],
    // 'D'
    [
        0b11100, // ###..
        0b10010, // #..#.
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10010, // #..#.
        0b11100, // ###..
    ],
    // 'E'
    [
        0b11111, // #####
        0b10000, // #....
        0b10000, // #....
        0b11110, // ####.
        0b10000, // #....
        0b10000, // #....
        0b11111, // #####
    ],
    // 'F'
    [
        0b11111, // #####
        0b10000, // #....
        0b10000, // #....
        0b11110, // ####.
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
    ],
    // 'G'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10000, // #....
        0b10111, // #.###
        0b10001, // #...#
        0b10001, // #...#
        0b01111, // .####
    ],
    // 'H'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b11111, // #####
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
    ],
    // 'I'
    [
        0b01110, // .###.
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b01110, // .###.
    ],
    // 'J'
    [
        0b00111, // ..###
        0b00010, // ...#.
        0b00010, // ...#.
        0b00010, // ...#.
        0b00010, // ...#.
        0b10010, // #..#.
        0b01100, // .##..
    ],
    // 'K'
    [
        0b10001, // #...#
        0b10010, // #..#.
        0b10100, // #.#..
        0b11000, // ##...
        0b10100, // #.#..
        0b10010, // #..#.
        0b10001, // #...#
    ],
    // 'L'
    [
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
        0b11111, // #####
    ],
    // 'M'
    [
        0b10001, // #...#
        0b11011, // ##.##
        0b10101, // #.#.#
        0b10101, // #.#.#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
    ],
    // 'N'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b11001, // ##..#
        0b10101, // #.#.#
        0b10011, // #..##
        0b10001, // #...#
        0b10001, // #...#
    ],
    // 'O'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // 'P'
    [
        0b11110, // ####.
        0b10001, // #...#
        0b10001, // #...#
        0b11110, // ####.
        0b10000, // #....
        0b10000, // #....
        0b10000, // #....
    ],
    // 'Q'
    [
        0b01110, // .###.
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10101, // #.#.#
        0b10010, // #..#.
        0b01101, // .##.#
    ],
    // 'R'
    [
        0b11110, // ####.
        0b10001, // #...#
        0b10001, // #...#
        0b11110, // ####.
        0b10100, // #.#..
        0b10010, // #..#.
        0b10001, // #...#
    ],
    // 'S'
    [
        0b01111, // .####
        0b10000, // #....
        0b10000, // #....
        0b01110, // .###.
        0b00001, // ....#
        0b00001, // ....#
        0b11110, // ####.
    ],
    // 'T'
    [
        0b11111, // #####
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
    ],
    // 'U'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b01110, // .###.
    ],
    // 'V'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b01010, // .#.#.
        0b00100, // ..#..
    ],
    // 'W'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b10001, // #...#
        0b10101, // #.#.#
        0b10101, // #.#.#
        0b11011, // ##.##
        0b10001, // #...#
    ],
    // 'X'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b01010, // .#.#.
        0b00100, // ..#..
        0b01010, // .#.#.
        0b10001, // #...#
        0b10001, // #...#
    ],
    // 'Y'
    [
        0b10001, // #...#
        0b10001, // #...#
        0b01010, // .#.#.
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
        0b00100, // ..#..
    ],
    // 'Z'
    [
        0b11111, // #####
        0b00001, // ....#
        0b00010, // ...#.
        0b00100, // ..#..
        0b01000, // .#...
        0b10000, // #....
        0b11111, // #####
    ],
];

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BG: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 3] = [255, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 255];

    /// A fully transparent capture, so any written pixel is visible as a
    /// change from `BG`.
    fn blank(width: u32, height: u32, scale: f32) -> Screenshot {
        let len = (width as usize) * (height as usize) * 4;
        Screenshot::new(width, height, vec![0; len], scale)
    }

    fn px(shot: &Screenshot, x: u32, y: u32) -> [u8; 4] {
        let i = ((y as usize) * (shot.width as usize) + x as usize) * 4;
        [
            shot.pixels[i],
            shot.pixels[i + 1],
            shot.pixels[i + 2],
            shot.pixels[i + 3],
        ]
    }

    fn opaque(rgb: [u8; 3]) -> [u8; 4] {
        [rgb[0], rgb[1], rgb[2], 255]
    }

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// An annotation with no tag draws a box and no badge, which is what lets
    /// the stroke tests assert on an otherwise untouched interior.
    fn boxed(r: Rect, color: [u8; 3]) -> Annotation {
        Annotation::new(r, "").color(color)
    }

    fn annotate(shot: &Screenshot, anns: &[Annotation]) -> (Screenshot, Vec<usize>) {
        shot.annotate(anns, Point::new(0, 0))
            .expect("annotate on a well-formed capture")
    }

    // ── Annotation / palette / tag_for ───────────────────────────────────

    #[test]
    fn new_takes_the_first_palette_colour_and_color_overrides_it() {
        let a = Annotation::new(rect(1, 2, 3, 4), "A1");
        assert_eq!(a.color, ANNOTATION_PALETTE[0]);
        assert_eq!(a.tag, "A1");
        assert_eq!(a.rect, rect(1, 2, 3, 4));

        let b = a.color(RED);
        assert_eq!(b.color, RED);
    }

    #[test]
    fn tag_for_letters_the_group_and_numbers_the_index() {
        assert_eq!(tag_for(1, 1), "A1");
        assert_eq!(tag_for(2, 7), "B7");
        assert_eq!(tag_for(1, 12), "A12");
        assert_eq!(tag_for(3, 999), "C999");
    }

    #[test]
    fn tag_for_group_boundaries_roll_over_like_spreadsheet_columns() {
        assert_eq!(tag_for(1, 1), "A1");
        assert_eq!(tag_for(26, 1), "Z1");
        assert_eq!(tag_for(27, 1), "AA1");
        assert_eq!(tag_for(28, 1), "AB1");
        assert_eq!(tag_for(52, 1), "AZ1");
        assert_eq!(tag_for(53, 1), "BA1");
        assert_eq!(tag_for(702, 1), "ZZ1");
        assert_eq!(tag_for(703, 1), "AAA1");
    }

    #[test]
    fn tag_for_reads_zero_as_one_since_every_index_is_one_based() {
        assert_eq!(tag_for(0, 0), "A1");
        assert_eq!(tag_for(0, 5), "A5");
        assert_eq!(tag_for(2, 0), "B1");
    }

    #[test]
    fn tag_for_never_produces_a_separator_that_could_be_lost() {
        // The property the format exists for: no two (group, index) pairs
        // render to the same glyph sequence.
        let mut seen = std::collections::HashSet::new();
        for group in 1..=30_usize {
            for index in 1..=30_usize {
                assert!(
                    seen.insert(tag_for(group, index)),
                    "collision at group {group}, index {index}"
                );
            }
        }
    }

    #[test]
    fn every_palette_colour_clears_wcag_aa_against_its_foreground() {
        for (i, &color) in ANNOTATION_PALETTE.iter().enumerate() {
            let fg = foreground_for(color);
            let ratio = contrast_ratio(fg, color);
            assert!(
                ratio >= 4.5,
                "ANNOTATION_PALETTE[{i}] {color:?} against {fg:?} is {ratio:.2}:1, below 4.5:1"
            );
        }
    }

    #[test]
    fn foreground_picks_black_on_light_and_white_on_dark() {
        assert_eq!(foreground_for([0xF0, 0xE4, 0x42]), BLACK); // yellow
        assert_eq!(foreground_for([0x00, 0x72, 0xB2]), WHITE); // blue
    }

    #[test]
    fn relative_luminance_matches_the_wcag_endpoints() {
        assert!((relative_luminance(WHITE) - 1.0).abs() < 1e-9);
        assert!(relative_luminance(BLACK).abs() < 1e-9);
        // Pure blue: 0.0722 × 1.0, the B coefficient exactly.
        assert!((relative_luminance(BLUE) - 0.0722).abs() < 1e-9);
        assert!((contrast_ratio(WHITE, BLACK) - 21.0).abs() < 1e-9);
    }

    // ── Stroke geometry ──────────────────────────────────────────────────

    #[test]
    fn stroke_lands_on_the_box_edges_and_nowhere_else() {
        let shot = blank(16, 16, 1.0);
        // Physical box spans x 2..8, y 3..8 at scale 1.
        let (out, skipped) = annotate(&shot, &[boxed(rect(2, 3, 6, 5), RED)]);
        assert!(skipped.is_empty());

        for x in 2..8 {
            assert_eq!(px(&out, x, 3), opaque(RED), "top edge at x={x}");
            assert_eq!(px(&out, x, 7), opaque(RED), "bottom edge at x={x}");
        }
        for y in 3..8 {
            assert_eq!(px(&out, 2, y), opaque(RED), "left edge at y={y}");
            assert_eq!(px(&out, 7, y), opaque(RED), "right edge at y={y}");
        }
        for y in 4..7 {
            for x in 3..7 {
                assert_eq!(px(&out, x, y), BG, "interior at ({x}, {y})");
            }
        }
        assert_eq!(px(&out, 1, 3), BG, "one left of the box");
        assert_eq!(px(&out, 8, 3), BG, "one right of the box");
        assert_eq!(px(&out, 2, 2), BG, "one above the box");
        assert_eq!(px(&out, 2, 8), BG, "one below the box");
    }

    #[test]
    fn origin_translates_the_rect_into_capture_space() {
        let shot = blank(16, 16, 1.0);
        // Same box as above, expressed in screen coordinates for a capture
        // whose top-left is (10, 10).
        let (out, skipped) = shot
            .annotate(&[boxed(rect(12, 13, 6, 5), RED)], Point::new(10, 10))
            .expect("annotate");
        assert!(skipped.is_empty());

        assert_eq!(px(&out, 2, 3), opaque(RED), "translated top-left corner");
        assert_eq!(
            px(&out, 7, 7),
            opaque(RED),
            "translated bottom-right corner"
        );
        assert_eq!(px(&out, 12, 13), BG, "untranslated position must be clean");
    }

    #[test]
    fn scale_converts_logical_coordinates_and_thickens_the_stroke() {
        let shot = blank(16, 16, 2.0);
        // Logical (1, 1, 4, 4) → physical (2, 2, 8, 8); stroke = round(2) = 2.
        let (out, skipped) = annotate(&shot, &[boxed(rect(1, 1, 4, 4), RED)]);
        assert!(skipped.is_empty());

        for x in 2..10 {
            assert_eq!(px(&out, x, 2), opaque(RED), "outer top row at x={x}");
            assert_eq!(px(&out, x, 3), opaque(RED), "inner top row at x={x}");
            assert_eq!(px(&out, x, 8), opaque(RED), "inner bottom row at x={x}");
            assert_eq!(px(&out, x, 9), opaque(RED), "outer bottom row at x={x}");
        }
        for y in 2..10 {
            assert_eq!(px(&out, 2, y), opaque(RED), "outer left col at y={y}");
            assert_eq!(px(&out, 3, y), opaque(RED), "inner left col at y={y}");
            assert_eq!(px(&out, 8, y), opaque(RED), "inner right col at y={y}");
            assert_eq!(px(&out, 9, y), opaque(RED), "outer right col at y={y}");
        }
        for y in 4..8 {
            for x in 4..8 {
                assert_eq!(px(&out, x, y), BG, "interior at ({x}, {y})");
            }
        }
        assert_eq!(px(&out, 1, 2), BG, "one left of the scaled box");
        assert_eq!(px(&out, 10, 2), BG, "one right of the scaled box");
    }

    // ── Clipping ─────────────────────────────────────────────────────────

    #[test]
    fn a_box_straddling_the_left_edge_is_clipped_not_moved() {
        let shot = blank(8, 8, 1.0);
        // x -3..3: the left stroke is off-image, the right stroke is at x=2.
        let (out, skipped) = annotate(&shot, &[boxed(rect(-3, 2, 6, 4), RED)]);
        assert!(skipped.is_empty(), "partly visible must not be skipped");

        assert_eq!(px(&out, 0, 2), opaque(RED), "top edge survives clipping");
        assert_eq!(px(&out, 2, 2), opaque(RED), "right stroke stays at x=2");
        assert_eq!(px(&out, 3, 2), BG, "nothing drawn past the box");
        assert_eq!(px(&out, 0, 3), BG, "no left stroke pulled to the edge");
    }

    #[test]
    fn a_box_straddling_the_right_edge_is_clipped_not_moved() {
        let shot = blank(8, 8, 1.0);
        // x 5..11: the left stroke is at x=5, the right stroke is off-image.
        let (out, skipped) = annotate(&shot, &[boxed(rect(5, 2, 6, 4), RED)]);
        assert!(skipped.is_empty());

        assert_eq!(px(&out, 5, 2), opaque(RED), "top edge survives clipping");
        assert_eq!(
            px(&out, 7, 2),
            opaque(RED),
            "top edge reaches the last column"
        );
        assert_eq!(px(&out, 7, 3), BG, "no right stroke pulled to the edge");
        assert_eq!(px(&out, 4, 2), BG, "nothing drawn left of the box");
    }

    #[test]
    fn a_box_straddling_the_top_edge_is_clipped_not_moved() {
        let shot = blank(8, 8, 1.0);
        // y -3..3: the top stroke is off-image, the bottom stroke is at y=2.
        let (out, skipped) = annotate(&shot, &[boxed(rect(2, -3, 4, 6), RED)]);
        assert!(skipped.is_empty());

        assert_eq!(px(&out, 2, 0), opaque(RED), "left edge survives clipping");
        assert_eq!(px(&out, 2, 2), opaque(RED), "bottom stroke stays at y=2");
        assert_eq!(px(&out, 3, 0), BG, "no top stroke pulled to the edge");
        assert_eq!(px(&out, 2, 3), BG, "nothing drawn below the box");
    }

    #[test]
    fn a_box_straddling_the_bottom_edge_is_clipped_not_moved() {
        let shot = blank(8, 8, 1.0);
        // y 5..11: the top stroke is at y=5, the bottom stroke is off-image.
        let (out, skipped) = annotate(&shot, &[boxed(rect(2, 5, 4, 6), RED)]);
        assert!(skipped.is_empty());

        assert_eq!(px(&out, 2, 5), opaque(RED), "top stroke stays at y=5");
        assert_eq!(
            px(&out, 2, 7),
            opaque(RED),
            "left edge reaches the last row"
        );
        assert_eq!(px(&out, 3, 7), BG, "no bottom stroke pulled to the edge");
        assert_eq!(px(&out, 2, 4), BG, "nothing drawn above the box");
    }

    // ── Skipping ─────────────────────────────────────────────────────────

    #[test]
    fn rects_outside_the_image_are_reported_not_clamped() {
        let shot = blank(8, 8, 1.0);
        let anns = [
            boxed(rect(100, 0, 4, 4), RED), // 0: right of the image
            boxed(rect(0, 0, 4, 4), RED),   // 1: visible
            boxed(rect(-50, 0, 4, 4), RED), // 2: left of the image
            boxed(rect(0, -50, 4, 4), RED), // 3: above the image
            boxed(rect(0, 200, 4, 4), RED), // 4: below the image
            boxed(rect(8, 8, 4, 4), RED),   // 5: one pixel past the corner
        ];
        let (out, skipped) = annotate(&shot, &anns);

        assert_eq!(skipped, vec![0, 2, 3, 4, 5]);
        assert_eq!(
            px(&out, 0, 0),
            opaque(RED),
            "the visible one is still drawn"
        );
        // Nothing was clamped onto an edge it does not belong to.
        assert_eq!(px(&out, 7, 7), BG);
        assert_eq!(px(&out, 7, 0), BG);
    }

    #[test]
    fn zero_area_rects_are_skipped() {
        let shot = blank(8, 8, 1.0);
        let anns = [
            boxed(rect(2, 2, 0, 4), RED),
            boxed(rect(2, 2, 4, 0), RED),
            boxed(rect(2, 2, 0, 0), RED),
        ];
        let (out, skipped) = annotate(&shot, &anns);

        assert_eq!(skipped, vec![0, 1, 2]);
        assert_eq!(out.pixels, shot.pixels, "nothing drawn at all");
    }

    // ── Badges and glyphs ────────────────────────────────────────────────

    #[test]
    fn a_tag_glyph_renders_in_the_contrasting_foreground() {
        let shot = blank(40, 20, 1.0);
        // Blue box: white is the higher-contrast foreground. The box fills the
        // capture, so no outside position is on-image and the badge falls back
        // to the box's inner top-left.
        let (out, skipped) = annotate(
            &shot,
            &[Annotation::new(rect(0, 0, 40, 20), "A").color(BLUE)],
        );
        assert!(skipped.is_empty());

        // Badge: 1px stroke, so its top-left is (1, 1); a 1px outline plus 2px
        // of padding puts the glyph's top-left at (4, 4), each font pixel a
        // 2×2 square. 'A' is:
        //   .###.  #...#  #...#  #####  #...#  #...#  #...#
        assert_eq!(
            px(&out, 1, 1),
            opaque(WHITE),
            "badge outline is the foreground colour"
        );
        assert_eq!(
            px(&out, 2, 2),
            opaque(BLUE),
            "inside the outline, the badge is the box colour"
        );
        for x in 6..12 {
            assert_eq!(px(&out, x, 4), opaque(WHITE), "top bar of 'A' at x={x}");
            assert_eq!(px(&out, x, 5), opaque(WHITE), "top bar, second row, x={x}");
        }
        assert_eq!(px(&out, 5, 4), opaque(BLUE), "left of the top bar is unset");
        assert_eq!(
            px(&out, 12, 4),
            opaque(BLUE),
            "right of the top bar is unset"
        );
        for x in 4..14 {
            assert_eq!(px(&out, x, 10), opaque(WHITE), "crossbar of 'A' at x={x}");
        }
        assert_eq!(px(&out, 4, 6), opaque(WHITE), "left stem of 'A'");
        assert_eq!(px(&out, 12, 6), opaque(WHITE), "right stem of 'A'");
        assert_eq!(px(&out, 8, 6), opaque(BLUE), "counter of 'A' is not filled");
        assert_eq!(px(&out, 4, 16), opaque(WHITE), "last row, left stem");
        assert_eq!(
            px(&out, 16, 4),
            opaque(WHITE),
            "the badge's right outline column"
        );
        assert_eq!(
            px(&out, 17, 4),
            BG,
            "the badge is only as wide as one glyph"
        );
    }

    #[test]
    fn a_tag_with_no_drawable_glyph_gets_no_badge() {
        let shot = blank(20, 20, 1.0);
        let (with_tag, _) = annotate(
            &shot,
            &[Annotation::new(rect(0, 0, 20, 20), "!?-").color(RED)],
        );
        let (no_tag, _) = annotate(&shot, &[boxed(rect(0, 0, 20, 20), RED)]);
        assert_eq!(with_tag.pixels, no_tag.pixels);
    }

    #[test]
    fn a_colliding_badge_moves_to_another_corner() {
        let shot = blank(60, 40, 1.0);
        let outer = Annotation::new(rect(0, 0, 60, 40), "A1").color(BLUE);
        let inner = Annotation::new(rect(0, 0, 30, 20), "B1").color(RED);
        let (out, skipped) = annotate(&shot, &[outer, inner]);
        assert!(skipped.is_empty());

        // A two-glyph badge is 2×10 + 2 tracking + 2×(2 padding + 1 outline)
        // = 28 wide and 7×2 + 6 = 20 tall. The outer box fills the capture, so
        // it has no outside position on-image and lands at its inner top-left,
        // (1, 1)..(29, 21).
        assert_eq!(
            px(&out, 1, 1),
            opaque(WHITE),
            "outer badge's outline holds the corner"
        );
        assert_eq!(px(&out, 2, 2), opaque(BLUE), "outer badge's fill");
        assert_eq!(
            px(&out, 28, 1),
            opaque(WHITE),
            "outer badge's right outline column"
        );

        // The inner box has room below, but that would cover the outer badge,
        // so the inner badge takes the next free position: beside the box, to
        // its right, at (30, 0)..(58, 20).
        assert_eq!(
            px(&out, 30, 0),
            opaque(BLACK),
            "inner badge's outline, beside the box"
        );
        assert_eq!(px(&out, 31, 1), opaque(RED), "inner badge's fill");
        assert_eq!(
            px(&out, 57, 19),
            opaque(BLACK),
            "inner badge's far outline corner"
        );
        assert_eq!(
            px(&out, 29, 1),
            opaque(RED),
            "the inner box's own right stroke, not the badge"
        );
    }

    #[test]
    fn identical_rects_are_both_drawn_never_deduplicated() {
        let shot = blank(60, 40, 1.0);
        let first = Annotation::new(rect(0, 0, 60, 40), "A1").color(BLUE);
        let second = Annotation::new(rect(0, 0, 60, 40), "B1").color(RED);
        let (out, skipped) = annotate(&shot, &[first, second]);

        assert!(skipped.is_empty(), "a duplicate rect is not a skip");
        // Both boxes fill the capture, so both badges fall inside. Equal area:
        // input order breaks the tie, so A1 keeps the inner top-left and B1 is
        // nudged to the inner top-right, x = 60 - 1 - 28 = 31.
        assert_eq!(px(&out, 1, 1), opaque(WHITE), "first badge's outline");
        assert_eq!(px(&out, 2, 2), opaque(BLUE), "first badge's fill");
        assert_eq!(
            px(&out, 31, 1),
            opaque(BLACK),
            "second badge's outline, nudged"
        );
        assert_eq!(px(&out, 32, 2), opaque(RED), "second badge's fill");
    }

    // ── Badge placement ──────────────────────────────────────────────────

    #[test]
    fn a_badge_sits_outside_the_box_so_it_cannot_cover_the_element() {
        let shot = blank(60, 60, 1.0);
        // A small control with room on every side: the badge goes above it,
        // left-aligned, its bottom edge touching the box's top edge.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(10, 30, 20, 10), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (10, 10)..(38, 30).
        assert_eq!(px(&out, 10, 10), opaque(BLACK), "badge's top-left outline");
        assert_eq!(px(&out, 11, 11), opaque(color), "badge's fill");
        assert_eq!(
            px(&out, 20, 29),
            opaque(BLACK),
            "badge's bottom outline touches the box's top edge"
        );
        assert_eq!(px(&out, 20, 30), opaque(color), "the box's own top stroke");
        assert_eq!(px(&out, 39, 29), BG, "nothing right of the badge");

        // The point of the whole change: the element's own pixels are clean.
        for y in 31..39 {
            for x in 11..29 {
                assert_eq!(px(&out, x, y), BG, "box interior at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_badge_with_no_room_above_drops_below_the_box() {
        let shot = blank(60, 60, 1.0);
        // Flush against the top edge: both above-positions are off-image.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(10, 0, 20, 10), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (10, 10)..(38, 30), below the box.
        assert_eq!(px(&out, 10, 10), opaque(BLACK), "badge's top-left outline");
        assert_eq!(px(&out, 11, 11), opaque(color), "badge's fill");
        assert_eq!(px(&out, 11, 5), BG, "box interior is untouched");
    }

    #[test]
    fn a_badge_wider_than_the_room_to_its_right_is_right_aligned() {
        let shot = blank(60, 60, 1.0);
        // x 40..58: above-left would run to x = 68, off the right edge, so
        // above-right (aligned to the box's right edge) wins.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(40, 30, 18, 10), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (30, 10)..(58, 30).
        assert_eq!(px(&out, 30, 10), opaque(BLACK), "badge's top-left outline");
        assert_eq!(px(&out, 31, 11), opaque(color), "badge's fill");
        assert_eq!(
            px(&out, 57, 29),
            opaque(BLACK),
            "badge's far outline corner"
        );
        assert_eq!(px(&out, 58, 29), BG, "the badge stops at the box's right");
    }

    #[test]
    fn a_badge_with_no_room_above_or_below_goes_beside_the_box() {
        let shot = blank(40, 24, 1.0);
        // Full-height box at the left edge: nothing above, nothing below, and
        // nothing to the left, so the badge takes the position to its right.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(0, 0, 10, 24), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (10, 0)..(38, 20).
        assert_eq!(px(&out, 10, 0), opaque(BLACK), "badge's top-left outline");
        assert_eq!(px(&out, 11, 1), opaque(color), "badge's fill");
        assert_eq!(px(&out, 5, 5), BG, "box interior is untouched");
        assert_eq!(px(&out, 38, 1), BG, "nothing past the badge");
    }

    #[test]
    fn a_badge_falls_inside_the_box_only_when_nothing_outside_is_on_image() {
        let shot = blank(40, 30, 1.0);
        // The box fills the capture: every outside position is entirely
        // off-image, so the last-resort inner top-left is used.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(0, 0, 40, 30), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (1, 1)..(29, 21), inside the 1px box stroke.
        assert_eq!(px(&out, 1, 1), opaque(BLACK), "badge's top-left outline");
        assert_eq!(px(&out, 2, 2), opaque(color), "badge's fill");
        assert_eq!(px(&out, 29, 21), BG, "the badge ends inside the box");
    }

    #[test]
    fn a_box_in_the_image_corner_still_gets_a_readable_badge() {
        let shot = blank(60, 60, 1.0);
        // (0, 0) is the worst case for an outside badge: above and to the left
        // are both off-image, so it lands below the box.
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(0, 0, 10, 10), "A1")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // Badge 28×20 at (0, 10)..(28, 30); glyphs start at (3, 13).
        assert_eq!(px(&out, 0, 10), opaque(BLACK), "badge's top-left outline");
        // The 'A' is drawn, in full, on-image: top bar then crossbar.
        for x in 5..11 {
            assert_eq!(px(&out, x, 13), opaque(BLACK), "top bar of 'A' at x={x}");
        }
        assert_eq!(px(&out, 4, 13), opaque(color), "left of the top bar");
        for x in 3..13 {
            assert_eq!(px(&out, x, 19), opaque(BLACK), "crossbar of 'A' at x={x}");
        }
        assert_eq!(px(&out, 5, 5), BG, "box interior is untouched");
    }

    #[test]
    fn a_badge_that_falls_off_the_image_is_clipped_not_a_skip() {
        let shot = blank(20, 40, 1.0);
        // The badge is wider than the whole capture, so wherever it goes part
        // of it is off-image. That must not skip the annotation or move the box.
        let (out, skipped) = annotate(
            &shot,
            &[Annotation::new(rect(4, 0, 16, 8), "A1").color(RED)],
        );

        assert!(skipped.is_empty(), "a clipped badge is not a skipped box");
        assert_eq!(px(&out, 4, 0), opaque(RED), "the box's top-left stroke");
        assert_eq!(
            px(&out, 19, 7),
            opaque(RED),
            "the box's bottom-right stroke"
        );
        // Best visible position is below-right, at (-8, 8): the right part of
        // its outline row lands on-image.
        assert_eq!(px(&out, 0, 8), opaque(BLACK), "clipped badge's top row");
        assert_eq!(px(&out, 19, 8), opaque(BLACK), "…all the way across");
        assert_eq!(px(&out, 0, 9), opaque(RED), "and its fill below that");
    }

    #[test]
    fn badge_spot_prefers_sitting_on_top_of_the_box_top_edge() {
        let box_rect = PxRect::at(100, 100, 50, 20);
        let image = PxRect::at(0, 0, 1000, 1000);
        let spot = badge_spot(box_rect, 28, 20, 1, image, &[]);

        assert_eq!(spot.x0, box_rect.x0, "left-aligned with the box");
        assert_eq!(spot.y1, box_rect.y0, "bottom edge touches the box's top");
        assert!(!spot.overlaps(box_rect), "and covers none of the box");
    }

    #[test]
    fn a_tag_is_drawn_at_twice_the_stroke_unit_so_it_is_legible_at_1x() {
        let shot = blank(60, 60, 1.0);
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(10, 30, 20, 10), "A")]);
        assert!(skipped.is_empty());
        let color = ANNOTATION_PALETTE[0];

        // One glyph: badge 16×20 at (10, 10)..(26, 30), glyphs at (13, 13).
        // A 5×7 font at unit 2 is 10×14 physical pixels.
        assert_eq!(px(&out, 13, 19), opaque(BLACK), "crossbar, first row");
        assert_eq!(px(&out, 22, 20), opaque(BLACK), "crossbar, second row");
        assert_eq!(px(&out, 13, 25), opaque(BLACK), "last glyph row, left stem");
        assert_eq!(px(&out, 13, 26), opaque(BLACK), "…which is two pixels tall");
        assert_eq!(px(&out, 13, 27), opaque(color), "and ends after 14 rows");
    }

    #[test]
    fn smaller_boxes_draw_on_top_of_larger_ones() {
        let shot = blank(20, 20, 1.0);
        let big = boxed(rect(0, 0, 20, 20), BLUE);
        let small = boxed(rect(0, 0, 4, 4), RED);
        // Input order puts the small one first; area order must still win.
        let (out, _) = annotate(&shot, &[small, big]);

        assert_eq!(px(&out, 0, 0), opaque(RED), "the small box overwrites");
        assert_eq!(px(&out, 19, 19), opaque(BLUE), "the big box is still there");
    }

    // ── Purity and errors ────────────────────────────────────────────────

    #[test]
    fn annotate_never_mutates_the_receiver() {
        let shot = blank(16, 16, 1.0);
        let before = shot.pixels.clone();
        let (out, _) = annotate(&shot, &[Annotation::new(rect(0, 0, 16, 16), "A1")]);

        assert_eq!(shot.pixels, before, "self must be untouched");
        assert_ne!(out.pixels, before, "the copy must have been drawn on");
        assert_eq!(out.width, shot.width);
        assert_eq!(out.height, shot.height);
        assert!((out.scale - shot.scale).abs() < f32::EPSILON);
    }

    #[test]
    fn no_annotations_returns_an_identical_copy() {
        let shot = blank(8, 8, 1.0);
        let (out, skipped) = annotate(&shot, &[]);

        assert!(skipped.is_empty());
        assert_eq!(out.pixels, shot.pixels);
    }

    #[test]
    fn a_pixel_buffer_that_disagrees_with_the_dimensions_is_an_error() {
        let shot = Screenshot::new(4, 4, vec![0; 10], 1.0);
        let err = shot
            .annotate(&[], Point::new(0, 0))
            .expect_err("a short buffer must be reported, not drawn into");

        match err {
            Error::Platform { message, .. } => {
                assert!(message.contains("does not match"), "message was {message}");
            }
            other => panic!("expected Error::Platform, got {other:?}"),
        }
    }

    // ── Degenerate input ─────────────────────────────────────────────────

    #[test]
    fn extreme_coordinates_scales_and_dimensions_do_not_panic() {
        let rects = [
            rect(0, 0, 0, 0),
            rect(i32::MIN, i32::MIN, u32::MAX, u32::MAX),
            rect(i32::MAX, i32::MAX, u32::MAX, u32::MAX),
            rect(i32::MIN, i32::MAX, 1, 1),
            rect(i32::MAX, i32::MIN, u32::MAX, 1),
            rect(-1, -1, 3, 3),
            rect(0, 0, u32::MAX, 1),
            rect(2, 2, 1, 1),
        ];
        let origins = [
            Point::new(0, 0),
            Point::new(i32::MIN, i32::MIN),
            Point::new(i32::MAX, i32::MAX),
            Point::new(-7, 9),
        ];
        let scales = [
            1.0_f32,
            0.0,
            -1.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            f32::MAX,
            1.5,
            2.0,
            4.0,
            1e30,
        ];
        let dims = [(0_u32, 0_u32), (1, 1), (0, 8), (8, 0), (9, 5)];

        for &(w, h) in &dims {
            for &scale in &scales {
                let shot = blank(w, h, scale);
                for &r in &rects {
                    let anns = [
                        Annotation::new(r, "A1"),
                        Annotation::new(r, ""),
                        Annotation::new(r, "!!"),
                        Annotation::new(r, "zz09"),
                        Annotation::new(r, "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW"),
                    ];
                    for &origin in &origins {
                        let (out, skipped) = shot
                            .annotate(&anns, origin)
                            .expect("a well-formed capture must annotate");
                        assert_eq!(
                            out.pixels.len(),
                            shot.pixels.len(),
                            "{w}x{h} scale {scale} rect {r:?} origin {origin:?}"
                        );
                        assert!(skipped.len() <= anns.len());
                        assert!(skipped.iter().all(|&i| i < anns.len()));
                    }
                }
            }
        }
    }

    #[test]
    fn a_zero_sized_image_draws_nothing_and_skips_everything() {
        let shot = blank(0, 0, 1.0);
        let (out, skipped) = annotate(&shot, &[Annotation::new(rect(0, 0, 10, 10), "A1")]);

        assert!(out.pixels.is_empty());
        assert_eq!(skipped, vec![0]);
    }

    #[test]
    fn a_box_larger_than_the_image_shows_only_its_interior() {
        let shot = blank(4, 4, 1.0);
        let (out, skipped) = annotate(&shot, &[boxed(rect(-10, -10, 100, 100), RED)]);

        assert!(skipped.is_empty());
        for y in 0..4 {
            for x in 0..4 {
                // Every stroke band is off-image, so what shows through the
                // 4×4 window is the box's interior and nothing else.
                assert_eq!(px(&out, x, y), BG, "interior at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_box_thinner_than_the_stroke_is_drawn_solid() {
        let shot = blank(8, 8, 4.0);
        // Logical 1×1 at scale 4 → physical 4×4, stroke 4: the four bands
        // cover the whole box.
        let (out, skipped) = annotate(&shot, &[boxed(rect(0, 0, 1, 1), RED)]);

        assert!(skipped.is_empty());
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(px(&out, x, y), opaque(RED), "({x}, {y})");
            }
        }
        assert_eq!(px(&out, 4, 0), BG, "and nothing past it");
    }
}
