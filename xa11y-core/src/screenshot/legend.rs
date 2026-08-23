//! The legend: what an annotated capture drew, and what it could not.
//!
//! [`super::annotate`] owns the pixels; this module owns the *description* of
//! them. The two are deliberately separate files, and neither depends on the
//! other beyond [`tag_for`].
//!
//! Nothing here resolves a selector — that is the `xa11y` umbrella crate's
//! half, and `xa11y::screenshot_annotated` is the only thing that builds an
//! [`Annotated`]. These types live in core anyway, for the reason every
//! boundary-crossing type does: `cargo xtask check-bindings-parity` reads
//! `xa11y-core`'s public API and nothing else, so a legend type in the
//! umbrella crate is a binding surface no check can see. `Screenshot` is a
//! core type while `screenshot()` is an umbrella function; the data belongs
//! here and the platform-touching function stays there.

use crate::element::Rect;

use super::{tag_for, Screenshot};

/// A capture plus the legend describing what was drawn on it.
///
/// Returned by `xa11y::screenshot_annotated`. The image carries boxes and
/// short tags; everything else — which element each box came from, and a
/// selector that acts on it — stays here, structured, rather than being
/// rendered into the pixels.
///
/// `#[non_exhaustive]`: capture metadata grows, and a reader must not break
/// when it does. Build one with [`Annotated::for_capture`].
#[non_exhaustive]
pub struct Annotated {
    /// The capture with the annotation boxes and tag badges drawn on it.
    pub screenshot: Screenshot,
    /// One entry per drawn box, in group order and then match order.
    pub legend: Vec<LegendEntry>,
    /// Elements that matched a selector but could not be drawn, each with the
    /// reason. Never silently dropped — a legend that disagreed with the
    /// picture with no way to find out why is the failure this exists to
    /// prevent.
    pub omitted: Vec<Omission>,
    /// How many matched elements were not described at all because the
    /// caller's annotation cap was reached. `0` when the cap did not bite.
    ///
    /// These are neither drawn, listed in `legend`, nor listed in `omitted`,
    /// and nothing is built for them: past the cap the resolver stops
    /// producing selectors and entries, so the per-match cost of a huge match
    /// set is bounded rather than merely hidden from the result.
    ///
    /// The tree read itself is not bounded by the cap. A provider returns
    /// every element its selector matched before the cap can be consulted, so
    /// `truncated > 0` still means a large query already ran — narrow the
    /// selector rather than relying on the cap to make it cheap.
    pub truncated: usize,
}

impl Annotated {
    /// Assemble a result for `screenshot`.
    ///
    /// Named `for_capture` rather than `new` on purpose: both bindings
    /// flatten this type onto their `Screenshot` class, where a `new` would
    /// shadow [`Screenshot::new`] and let one allowlist entry stand in for
    /// two operations. Same reason `ElementData::for_role` is not `new`.
    pub fn for_capture(
        screenshot: Screenshot,
        legend: Vec<LegendEntry>,
        omitted: Vec<Omission>,
        truncated: usize,
    ) -> Self {
        Self {
            screenshot,
            legend,
            omitted,
            truncated,
        }
    }
}

/// A hand-written `Debug` rather than a derive: [`Screenshot`]'s own `Debug`
/// prints the whole pixel buffer, so a derived one here would turn a single
/// failed assertion in a test into megabytes of hex. The legend is the part a
/// reader wants; the image is summarised by its dimensions.
impl std::fmt::Debug for Annotated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Annotated")
            .field(
                "screenshot",
                &format_args!(
                    "{}x{} @{}x",
                    self.screenshot.width, self.screenshot.height, self.screenshot.scale
                ),
            )
            .field("legend", &self.legend)
            .field("omitted", &self.omitted)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// One drawn box: the tag in the image, and the element it came from.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LegendEntry {
    /// What is drawn in the box — `"B7"`. See [`tag_for`].
    pub tag: String,
    /// 1-based, matching the position of this element's locator in the
    /// annotation groups.
    pub group: usize,
    /// 1-based, and exactly the `:nth(n)` argument in [`selector`](Self::selector).
    pub index: usize,
    /// A selector usable as-is against the same scope the group's locator
    /// had — `"button:nth(7)"`. This round-trip is the point of the feature:
    /// a model reads a tag off the image, and the caller acts on
    /// `app.locator(entry.selector)`.
    pub selector: String,
    /// The element's role, snake_case as everywhere else.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// The element's bounds in **logical** screen coordinates — the same
    /// space as `Element::bounds`, not the capture's pixel space.
    pub bounds: Rect,
    /// The box colour, RGB, for correlating a box with its entry by eye.
    pub color: [u8; 3],
}

impl LegendEntry {
    /// Describe the `index`-th match of annotation group `group`.
    ///
    /// [`tag`](Self::tag) is not an argument: it is `tag_for(group, index)`
    /// by definition, and a constructor that took it could be handed a tag
    /// that disagrees with the numbers beside it.
    pub fn new(
        group: usize,
        index: usize,
        selector: impl Into<String>,
        role: impl Into<String>,
        name: Option<String>,
        bounds: Rect,
        color: [u8; 3],
    ) -> Self {
        Self {
            tag: tag_for(group, index),
            group,
            index,
            selector: selector.into(),
            role: role.into(),
            name,
            bounds,
            color,
        }
    }
}

/// An element that matched a selector but is not in the image.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Omission {
    /// The selector that would reach this element, on the same terms as
    /// [`LegendEntry::selector`].
    pub selector: String,
    /// The element's role, snake_case.
    pub role: String,
    /// The element's accessible name, when it has one.
    pub name: Option<String>,
    /// Why it could not be drawn.
    pub reason: OmissionReason,
}

impl Omission {
    /// Record that `selector`'s element could not be drawn, and why.
    pub fn new(
        selector: impl Into<String>,
        role: impl Into<String>,
        name: Option<String>,
        reason: OmissionReason,
    ) -> Self {
        Self {
            selector: selector.into(),
            role: role.into(),
            name,
            reason,
        }
    }
}

/// Why an element that matched a selector is not in the image.
///
/// `#[non_exhaustive]`: the ways a tree node can fail to be a rectangle on the
/// captured display are a platform-shaped set, and a new one must not be a
/// breaking change for the bindings that map these to strings.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    /// The accessibility tree reports no bounds for this element at all.
    NoBounds,
    /// There is nothing to outline: the bounds have zero width or zero height,
    /// or they round to zero physical pixels on a capture whose
    /// [`Screenshot::scale`] is below 1 (`Rect::to_physical` rounds position
    /// and size to the nearest integer, so a 1×1 logical box at `0.25×` is
    /// 0×0 on the image).
    ZeroArea,
    /// The bounds are valid but fall outside the pixels that were captured —
    /// a monitor the capture did not cover, or anything outside an explicit
    /// `region`. Boxes are never clamped to the edge: a clamped box claims
    /// pixels that belong to something else.
    ///
    /// What a full capture covers is the backend's own answer — see
    /// [`crate::ScreenshotProvider::capture_full`] — so this is "not in the
    /// image", not "on a second monitor".
    OutsideCapture,
}

impl OmissionReason {
    /// The snake_case spelling every surface uses — `"no_bounds"`,
    /// `"zero_area"`, `"outside_capture"`.
    ///
    /// The CLI legend, the MCP result and both bindings render this rather
    /// than each inventing a name, so a reason means the same thing wherever
    /// a caller compares it against a literal.
    pub fn as_str(self) -> &'static str {
        match self {
            OmissionReason::NoBounds => "no_bounds",
            OmissionReason::ZeroArea => "zero_area",
            OmissionReason::OutsideCapture => "outside_capture",
        }
    }
}

impl std::fmt::Display for OmissionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn an_entrys_tag_is_derived_from_its_group_and_index() {
        let entry = LegendEntry::new(
            2,
            7,
            "button:nth(7)",
            "button",
            Some("Back".to_string()),
            rect(1, 2, 3, 4),
            [230, 159, 0],
        );
        assert_eq!(entry.tag, "B7");
        assert_eq!(entry.tag, tag_for(entry.group, entry.index));
        assert_eq!(entry.selector, "button:nth(7)");
        assert_eq!(entry.bounds, rect(1, 2, 3, 4));
    }

    #[test]
    fn an_omission_keeps_the_selector_that_would_reach_its_element() {
        let omission = Omission::new(
            "check_box:nth(1)",
            "check_box",
            Some("Agree".to_string()),
            OmissionReason::NoBounds,
        );
        assert_eq!(omission.selector, "check_box:nth(1)");
        assert_eq!(omission.reason, OmissionReason::NoBounds);
    }

    #[test]
    fn an_annotated_summarises_its_capture_rather_than_dumping_the_pixels() {
        // A derived Debug here would print every byte of the buffer, which is
        // what turns one failed assertion into megabytes of hex.
        let shot = Screenshot::new(4, 2, vec![0xAB; 4 * 2 * 4], 2.0);
        let annotated = Annotated::for_capture(shot, Vec::new(), Vec::new(), 3);

        assert_eq!(annotated.truncated, 3);
        assert!(annotated.legend.is_empty());
        let rendered = format!("{annotated:?}");
        assert!(rendered.contains("4x2 @2x"), "got {rendered}");
        assert!(!rendered.contains("171"), "pixels must not be printed");
    }

    #[test]
    fn omission_reasons_spell_themselves_in_snake_case() {
        assert_eq!(OmissionReason::NoBounds.as_str(), "no_bounds");
        assert_eq!(OmissionReason::ZeroArea.as_str(), "zero_area");
        assert_eq!(OmissionReason::OutsideCapture.as_str(), "outside_capture");
        assert_eq!(OmissionReason::ZeroArea.to_string(), "zero_area");
    }

    #[test]
    fn a_serialized_omission_reason_matches_its_string_spelling() {
        // The bindings and the CLI's `--legend json` must not disagree about
        // what a reason is called.
        for reason in [
            OmissionReason::NoBounds,
            OmissionReason::ZeroArea,
            OmissionReason::OutsideCapture,
        ] {
            let json = serde_json::to_string(&reason).expect("serialize");
            assert_eq!(json, format!("\"{}\"", reason.as_str()));
        }
    }
}
