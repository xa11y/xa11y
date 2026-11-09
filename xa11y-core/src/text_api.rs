//! Reading text by range: caret, selection, ranges, rendered lines, geometry
//! and formatting.
//!
//! [`Element::text`] and [`Locator::text`] return a [`Text`], which reads
//! from the platform on every call and caches nothing, so polling
//! [`Text::caret`] sees the current caret without fetching the whole value.
//!
//! # Offsets are UTF-16 code units
//!
//! Every offset, on every surface (Rust, Python, JS, the CLI and MCP), counts
//! UTF-16 code units, which is what macOS AX, Qt, IAccessible2 and JS strings
//! use natively. A character outside the Basic Multilingual Plane, such as most
//! emoji, is two units. Python strings index by code point, so slicing
//! `element.value` with these offsets is wrong around emoji: use
//! [`Text::read`] instead, which returns the string for a range.
//!
//! A range that would split a surrogate pair is rejected with
//! [`Error::InvalidTextRange`], as is one past the end of the text.
//!
//! **Cost.** Backends whose platform counts something else convert, and
//! converting an offset means reading the text before it. GTK on Linux
//! reports code points, and UIA on Windows has no numeric offsets at all, so
//! on those a caret or selection read costs a read of the text up to the
//! caret. macOS and Qt need no conversion.
//!
//! # Ranges are values
//!
//! A [`TextRange`] is two integers. It does not follow edits: after the text
//! changes, an old range may cover different text or fall past the end, in
//! which case the read fails with [`Error::InvalidTextRange`] rather than
//! returning something else.
//!
//! [`Element::text`]: crate::element::Element::text
//! [`Locator::text`]: crate::locator::Locator::text

use serde::{Deserialize, Serialize};

use crate::element::{reader_writer_pair, Element, Rect};
use crate::error::{Error, Result};
use crate::locator::Locator;

/// A half-open range `[start, end)` of UTF-16 code units.
#[allow(
    clippy::exhaustive_structs,
    reason = "Closed domain: a half-open interval is a start and an end."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextRange {
    /// 0-based offset of the first unit.
    pub start: u32,
    /// 0-based offset one past the last unit.
    pub end: u32,
}

impl TextRange {
    /// A range from `start` to `end`. Fails with
    /// [`Error::InvalidTextRange`] when `start > end`.
    pub fn new(start: u32, end: u32) -> Result<Self> {
        if start > end {
            return Err(Error::InvalidTextRange {
                start,
                end,
                length: None,
                message: "start must be <= end".to_string(),
            });
        }
        Ok(Self { start, end })
    }

    /// Number of UTF-16 code units covered.
    pub fn len(&self) -> u32 {
        // Saturating: the fields are public, so a reversed range can exist.
        self.end.saturating_sub(self.start)
    }

    /// Whether the range covers nothing (a caret position).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Whether this range lies within `[0, length]`.
    pub fn fits(&self, length: u32) -> bool {
        self.start <= self.end && self.end <= length
    }
}

impl std::fmt::Display for TextRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// A unit of text for [`Text::range_at`].
///
/// Exhaustive: each backend maps every variant to a platform boundary type,
/// so a new unit must fail their build rather than reach them as a runtime
/// error.
#[allow(
    clippy::exhaustive_enums,
    reason = "Capability enum: every backend translates each variant into a \
              platform boundary type (AT-SPI granularity, UIA TextUnit, AX \
              parameterized attribute), so growth must break the build."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextUnit {
    /// The Unicode scalar value at the offset: one UTF-16 unit, or two for a
    /// surrogate pair. Not a grapheme cluster.
    Character,
    /// The word containing the offset, by the platform's word boundaries.
    Word,
    /// The rendered line containing the offset. A wrapped paragraph is several
    /// lines.
    Line,
    /// The newline-delimited paragraph containing the offset.
    Paragraph,
}

impl TextUnit {
    /// The `snake_case` name this unit crosses the bindings as.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Character => "character",
            Self::Word => "word",
            Self::Line => "line",
            Self::Paragraph => "paragraph",
        }
    }

    /// Parse the `snake_case` name. Unknown names fail with
    /// [`Error::InvalidActionData`] listing the valid ones.
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "character" => Self::Character,
            "word" => Self::Word,
            "line" => Self::Line,
            "paragraph" => Self::Paragraph,
            other => {
                return Err(Error::invalid_action_data(format!(
                    "unknown text unit {other:?}; expected one of character, word, line, paragraph"
                )))
            }
        })
    }
}

/// An sRGB colour with alpha.
///
/// Crosses the bindings, the CLI and MCP as a hex string (see
/// [`Color::to_hex`]).
#[allow(
    clippy::exhaustive_structs,
    reason = "Closed domain: an RGBA colour is four channels."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// `#rrggbb` when fully opaque, `#rrggbbaa` otherwise.
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let hex = s
            .strip_prefix('#')
            .ok_or_else(|| serde::de::Error::custom("colour must start with '#'"))?;
        let byte = |i: usize| {
            hex.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| serde::de::Error::custom(format!("bad colour {s:?}")))
        };
        match hex.len() {
            6 => Ok(Self {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: 255,
            }),
            8 => Ok(Self {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: byte(6)?,
            }),
            _ => Err(serde::de::Error::custom(format!("bad colour {s:?}"))),
        }
    }
}

/// Where a run sits relative to the baseline.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: text sits on the baseline, above it, or below it."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextScript {
    /// On the baseline.
    Baseline,
    /// Raised (superscript).
    Superscript,
    /// Lowered (subscript).
    Subscript,
}

impl TextScript {
    /// The `snake_case` name this crosses the bindings as.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Superscript => "superscript",
            Self::Subscript => "subscript",
        }
    }
}

reader_writer_pair! {
    /// Formatting of a run of text.
    ///
    /// Every field is `None` when the platform did not report it. A missing
    /// value is never defaulted: `bold` text on a platform that reports no
    /// weight reads `font_weight: None`, not `400`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct TextAttributes;

    /// Every attribute a provider must decide on. Exhaustive for the same
    /// reason as `ElementParts`. Not public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for TextAttributes; see ElementParts."
    )]
    #[derive(Debug, Clone)]
    pub struct TextAttributeParts;

    fields {
        /// Font family name.
        pub font_family: Option<String>,
        /// Font size in points.
        pub font_size: Option<f64>,
        /// Font weight on the CSS scale: 400 is normal, 700 is bold.
        pub font_weight: Option<u16>,
        /// Italic or oblique.
        pub italic: Option<bool>,
        /// Underlined (any style).
        pub underline: Option<bool>,
        /// Struck through.
        pub strikethrough: Option<bool>,
        /// Text colour.
        pub foreground: Option<Color>,
        /// Background colour.
        pub background: Option<Color>,
        /// Position relative to the baseline.
        pub script: Option<TextScript>,
        /// Marked as a spelling error.
        pub spelling_error: Option<bool>,
    }
}

impl TextAttributes {
    /// Attributes with nothing reported. For partial construction (tests,
    /// mocks); providers use `TextAttributeParts`.
    pub fn unreported() -> Self {
        TextAttributeParts {
            font_family: None,
            font_size: None,
            font_weight: None,
            italic: None,
            underline: None,
            strikethrough: None,
            foreground: None,
            background: None,
            script: None,
            spelling_error: None,
        }
        .into()
    }
}

/// A run of text sharing one set of attributes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    /// The text the run covers.
    pub range: TextRange,
    /// Its formatting.
    pub attributes: TextAttributes,
}

impl TextRun {
    /// A run covering `range` with `attributes`.
    pub fn new(range: TextRange, attributes: TextAttributes) -> Self {
        Self { range, attributes }
    }
}

/// One page of rendered lines, from [`Text::lines`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextLines {
    /// The lines, each clipped to the requested range.
    pub lines: Vec<TextRange>,
    /// Whether the limit stopped the walk before the end of the range.
    pub truncated: bool,
    /// Where to start the next page. `Some` exactly when `truncated`.
    pub next_start: Option<u32>,
}

/// One page of formatting runs, from [`Text::runs`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRuns {
    /// The runs, each clipped to the requested range.
    pub runs: Vec<TextRun>,
    /// Whether the limit stopped the walk before the end of the range.
    pub truncated: bool,
    /// Where to start the next page. `Some` exactly when `truncated`.
    pub next_start: Option<u32>,
}

/// Options for [`Text::find_with`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindOptions {
    /// Which match to return, 0-based: `0` is the first.
    pub occurrence: usize,
    /// Search only this range. `None` searches the whole text.
    pub within: Option<TextRange>,
}

impl FindOptions {
    /// Defaults: the first match in the whole text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the `n`-th match, 0-based.
    pub fn occurrence(mut self, n: usize) -> Self {
        self.occurrence = n;
        self
    }

    /// Search only `range`.
    pub fn within(mut self, range: TextRange) -> Self {
        self.within = Some(range);
        self
    }
}

/// Most rendered-line fragments [`Text::bounds`] walks before refusing.
pub const MAX_BOUNDS_LINES: usize = 1000;

/// UTF-16 units [`Text::find`] reads per platform call.
const FIND_CHUNK: u32 = 16 * 1024;

#[derive(Clone)]
enum Source {
    Element(Element),
    Locator(Locator),
}

/// Text of one element, read by range.
///
/// Returned by [`Element::text`] (bound to that element: a re-rendered
/// control fails with [`Error::ElementStale`]) and [`Locator::text`]
/// (re-resolved on every call, waiting until the selector matches; reads do
/// not require the element to be visible or enabled).
///
/// Every method is a live platform read. Offsets are UTF-16 code units; see
/// the [module docs](self).
#[derive(Clone)]
pub struct Text {
    source: Source,
}

impl Text {
    pub(crate) fn for_element(element: Element) -> Self {
        Self {
            source: Source::Element(element),
        }
    }

    pub(crate) fn for_locator(locator: Locator) -> Self {
        Self {
            source: Source::Locator(locator),
        }
    }

    fn target(&self) -> Result<Element> {
        match &self.source {
            Source::Element(e) => Ok(e.clone()),
            Source::Locator(l) => l.resolve_for_read("read_text"),
        }
    }

    /// Length of the text in UTF-16 code units. Reads the text only where
    /// the platform counts something else (GTK on Linux, Windows).
    pub fn length(&self) -> Result<u32> {
        let el = self.target()?;
        el.provider().text_length(el.data())
    }

    /// The caret offset, or `None` when the element has no caret (it is not
    /// focused, or not editable). Fails with [`Error::Unsupported`] when the
    /// platform cannot report a caret at all.
    pub fn caret(&self) -> Result<Option<u32>> {
        let el = self.target()?;
        el.provider().text_caret(el.data())
    }

    /// The selected ranges, in document order. Empty when nothing is
    /// selected; a caret alone is not a selection. Most controls allow one
    /// range; some (browsers, word processors) report several.
    pub fn selections(&self) -> Result<Vec<TextRange>> {
        let el = self.target()?;
        el.provider().text_selections(el.data())
    }

    /// The text in `range`, exactly as the platform reports it (bidi controls
    /// are not stripped, unlike `value`).
    pub fn read(&self, range: TextRange) -> Result<String> {
        check_ordered(range)?;
        let el = self.target()?;
        el.provider().text_read(el.data(), range)
    }

    /// The whole text. Reads every unit, so prefer [`Self::read`] on a large
    /// document.
    pub fn read_all(&self) -> Result<String> {
        let el = self.target()?;
        let p = el.provider();
        let len = p.text_length(el.data())?;
        p.text_read(el.data(), TextRange { start: 0, end: len })
    }

    /// The first exact match of `needle`. See [`Self::find_with`].
    pub fn find(&self, needle: &str) -> Result<Option<TextRange>> {
        self.find_with(needle, FindOptions::new())
    }

    /// Find a match of `needle`.
    ///
    /// Matching is exact on UTF-16 code units: case-sensitive, with no
    /// Unicode normalization (a precomposed `é` does not match `e` plus a
    /// combining accent). Matches do not overlap. Backends read chunks or one text observation;
    /// an edit made while the search runs is not detected.
    ///
    /// An empty `needle` fails with [`Error::InvalidActionData`].
    pub fn find_with(&self, needle: &str, options: FindOptions) -> Result<Option<TextRange>> {
        if needle.is_empty() {
            return Err(Error::invalid_action_data("find needle must not be empty"));
        }
        let el = self.target()?;
        el.provider().text_find(el.data(), needle, options)
    }

    /// The embedded control or link at `offset`, in UTF-16 code units.
    /// Returns a live element that can be compared, navigated and acted on.
    /// `None` means the character has no object. `offset == length` and
    /// offsets past it are invalid. Backends without an object mapping
    /// return `Unsupported` rather than an empty result.
    pub fn object_at(&self, offset: u32) -> Result<Option<crate::Element>> {
        let el = self.target()?;
        let length = el.provider().text_length(el.data())?;
        if offset >= length {
            return Err(Error::InvalidTextRange {
                start: offset,
                end: offset,
                length: Some(length),
                message: "object offset must be inside the text".into(),
            });
        }
        Ok(el
            .provider()
            .text_object_at(el.data(), offset)?
            .map(|data| crate::Element::new(data, el.provider().clone())))
    }

    /// The `unit` (character, word, rendered line, paragraph) containing
    /// `offset`. At `offset == length`, the unit that ends there.
    pub fn range_at(&self, offset: u32, unit: TextUnit) -> Result<TextRange> {
        let el = self.target()?;
        el.provider().text_range_at(el.data(), offset, unit)
    }

    /// The rendered lines that cover `range`, each clipped to it, at most
    /// `limit` per page. A `limit` of 0 fails with
    /// [`Error::InvalidActionData`]: a page that can hold nothing never
    /// advances.
    pub fn lines(&self, range: TextRange, limit: usize) -> Result<TextLines> {
        check_ordered(range)?;
        check_limit(limit)?;
        let el = self.target()?;
        let p = el.provider();
        let data = el.data();
        let (lines, next_start) = walk(
            range,
            limit,
            |pos| p.text_range_at(data, pos, TextUnit::Line),
            || p.text_length(data),
        )?;
        Ok(TextLines {
            truncated: next_start.is_some(),
            lines,
            next_start,
        })
    }

    /// Screen rectangles of `range`: one per rendered line fragment, in the
    /// same desktop coordinates as `ElementData::bounds`.
    ///
    /// A fragment the platform reports no geometry for (scrolled out, not
    /// laid out) is left out, so an empty result means no part of the range
    /// has geometry. A range covering more than [`MAX_BOUNDS_LINES`] lines
    /// fails with [`Error::InvalidTextRange`]: page with [`Self::lines`] and
    /// ask per page instead.
    pub fn bounds(&self, range: TextRange) -> Result<Vec<Rect>> {
        check_ordered(range)?;
        let el = self.target()?;
        let p = el.provider();
        let data = el.data();
        if range.is_empty() {
            return Ok(p.text_range_bounds(data, range)?.into_iter().collect());
        }
        let (lines, next_start) = walk(
            range,
            MAX_BOUNDS_LINES,
            |pos| p.text_range_at(data, pos, TextUnit::Line),
            || p.text_length(data),
        )?;
        if next_start.is_some() {
            return Err(Error::InvalidTextRange {
                start: range.start,
                end: range.end,
                length: None,
                message: format!(
                    "range covers more than {MAX_BOUNDS_LINES} rendered lines; page it with lines()"
                ),
            });
        }
        let mut rects = Vec::with_capacity(lines.len());
        for line in lines {
            if let Some(rect) = p.text_range_bounds(data, line)? {
                rects.push(rect);
            }
        }
        Ok(rects)
    }

    /// Formatting runs covering `range`, each clipped to it, at most `limit`
    /// per page. A `limit` of 0 fails like it does for [`Self::lines`].
    pub fn runs(&self, range: TextRange, limit: usize) -> Result<TextRuns> {
        check_ordered(range)?;
        check_limit(limit)?;
        let el = self.target()?;
        let p = el.provider();
        let data = el.data();
        let mut runs = Vec::new();
        let (ranges, next_start) = walk(
            range,
            limit,
            |pos| {
                let run = p.text_attributes_at(data, pos)?;
                let r = run.range;
                runs.push(run);
                Ok(r)
            },
            || p.text_length(data),
        )?;
        // `walk` clips each range; carry the clipping over to the runs.
        let runs = runs
            .into_iter()
            .zip(ranges)
            .map(|(run, r)| TextRun::new(r, run.attributes))
            .collect();
        Ok(TextRuns {
            truncated: next_start.is_some(),
            runs,
            next_start,
        })
    }
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Element(e) => write!(f, "Text({e})"),
            Source::Locator(l) => write!(f, "Text(locator {:?})", l.selector()),
        }
    }
}

/// Shared exact UTF-16 search for providers with a single text observation.
/// Implementation support, not a separate consumer search API.
#[doc(hidden)]
pub fn find_with_reader(
    needle: &str,
    options: FindOptions,
    length: u32,
    mut read: impl FnMut(TextRange) -> Result<String>,
) -> Result<Option<TextRange>> {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    if needle.is_empty() {
        return Err(Error::invalid_action_data("find needle must not be empty"));
    }
    let scope = options.within.unwrap_or(TextRange {
        start: 0,
        end: length,
    });
    check_in_bounds(scope, length)?;
    find_in_chunks(&mut read, &needle, scope, options.occurrence, FIND_CHUNK)
}

fn check_limit(limit: usize) -> Result<()> {
    if limit == 0 {
        return Err(Error::invalid_action_data("page limit must be at least 1"));
    }
    Ok(())
}

fn check_ordered(range: TextRange) -> Result<()> {
    TextRange::new(range.start, range.end).map(|_| ())
}

/// Reject a range outside `[0, length]`. Backends call this before a read.
pub fn check_in_bounds(range: TextRange, length: u32) -> Result<()> {
    if range.fits(length) {
        Ok(())
    } else {
        Err(Error::InvalidTextRange {
            start: range.start,
            end: range.end,
            length: Some(length),
            message: if range.start > range.end {
                "start must be <= end".to_string()
            } else {
                format!("range extends past the end of the text (length {length})")
            },
        })
    }
}

/// Walk `range` unit by unit with `unit_at(pos)`, clipping each unit to the
/// range. Returns the clipped units and, when `limit` stopped the walk, the
/// offset to resume from.
///
/// `length` is read only when a unit fails to advance: at `pos == length` a
/// backend reports the unit that *ends* there, so a range past the end of the
/// text surfaces as that, and is then reported as
/// [`Error::InvalidTextRange`] like [`Text::read`] reports it. Reading the
/// length up front would cost every successful page a length read, which
/// some platforms only answer by reading the whole text.
fn walk(
    range: TextRange,
    limit: usize,
    mut unit_at: impl FnMut(u32) -> Result<TextRange>,
    length: impl FnOnce() -> Result<u32>,
) -> Result<(Vec<TextRange>, Option<u32>)> {
    let mut out = Vec::new();
    let mut pos = range.start;
    while pos < range.end {
        if out.len() == limit {
            return Ok((out, Some(pos)));
        }
        let unit = unit_at(pos)?;
        if unit.end <= pos || unit.start > pos {
            // The range runs past the text: say so, as `read` does.
            check_in_bounds(range, length()?)?;
            // A unit that does not contain `pos` and move forward would loop
            // forever; report the backend's answer instead.
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "text unit at offset {pos} reported as {unit}, which does not advance past it"
                ),
            });
        }
        let clipped = TextRange {
            start: pos,
            end: unit.end.min(range.end),
        };
        out.push(clipped);
        pos = clipped.end;
    }
    Ok((out, None))
}

/// Find the `occurrence`-th non-overlapping match of `needle` in `scope`,
/// reading `chunk` units at a time.
///
/// Consecutive chunks overlap by `needle.len() - 1` units so a match that
/// straddles a boundary is seen. A chunk end that would split a surrogate pair
/// is refused by the backend with [`Error::InvalidTextRange`]; the read is
/// retried once, one unit shorter, which cannot split it.
pub(crate) fn find_in_chunks(
    read: &mut dyn FnMut(TextRange) -> Result<String>,
    needle: &[u16],
    scope: TextRange,
    occurrence: usize,
    chunk: u32,
) -> Result<Option<TextRange>> {
    let n = needle.len() as u32;
    if n > scope.len() {
        return Ok(None);
    }
    let chunk = chunk.max(n * 2);
    // `buf` holds units from `buf_start`; matches before `min_start` were
    // already counted (or overlap a counted one).
    let mut buf: Vec<u16> = Vec::new();
    let mut buf_start = scope.start;
    let mut read_pos = scope.start;
    let mut min_start = scope.start;
    let mut seen = 0usize;
    while read_pos < scope.end {
        let mut end = read_pos.saturating_add(chunk).min(scope.end);
        let text = match read(TextRange {
            start: read_pos,
            end,
        }) {
            Ok(t) => t,
            Err(Error::InvalidTextRange { .. }) if end < scope.end && end - read_pos > 1 => {
                end -= 1;
                read(TextRange {
                    start: read_pos,
                    end,
                })?
            }
            Err(e) => return Err(e),
        };
        // Match offsets are computed from the requested range, so a read that
        // came back a different length (a backend that stripped or replaced
        // units) would shift every later match silently. Refuse it instead.
        let got = utf16_len(&text);
        if got != end - read_pos {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "reading {read_pos}..{end} returned {got} UTF-16 units, expected {}",
                    end - read_pos
                ),
            });
        }
        buf.extend(text.encode_utf16());
        read_pos = end;
        let mut i = (min_start - buf_start) as usize;
        while i + needle.len() <= buf.len() {
            if buf[i..i + needle.len()] == *needle {
                let start = buf_start + i as u32;
                if seen == occurrence {
                    return Ok(Some(TextRange {
                        start,
                        end: start + n,
                    }));
                }
                seen += 1;
                i += needle.len();
                min_start = buf_start + i as u32;
            } else {
                i += 1;
                min_start = min_start.max(buf_start + i as u32);
            }
        }
        // Keep only the tail a straddling match could still start in.
        let keep = (n as usize - 1).min(buf.len());
        let drop = buf.len() - keep;
        buf.drain(..drop);
        buf_start += drop as u32;
        min_start = min_start.max(buf_start);
    }
    Ok(None)
}

/// UTF-16 length of `s`.
pub fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Convert a UTF-16 offset into `s` to a code-point (`char`) offset.
///
/// Fails with [`Error::InvalidTextRange`] when the offset is past the end of
/// `s` or splits a surrogate pair. Backends whose platform counts code points
/// (GTK over AT-SPI) use this with the text before the offset.
pub fn utf16_to_char_offset(s: &str, utf16: u32) -> Result<u32> {
    let mut units = 0u32;
    for (chars, c) in s.chars().enumerate() {
        if units == utf16 {
            return Ok(chars as u32);
        }
        units += c.len_utf16() as u32;
        if units > utf16 {
            return Err(Error::InvalidTextRange {
                start: utf16,
                end: utf16,
                length: Some(utf16_len(s)),
                message: format!("offset {utf16} splits a surrogate pair"),
            });
        }
    }
    if units == utf16 {
        Ok(s.chars().count() as u32)
    } else {
        Err(Error::InvalidTextRange {
            start: utf16,
            end: utf16,
            length: Some(units),
            message: format!("offset {utf16} is past the end of the text (length {units})"),
        })
    }
}

/// Convert a code-point offset into `s` to a UTF-16 offset. An offset past
/// the end fails with [`Error::InvalidTextRange`]; it is never clamped.
pub fn char_to_utf16_offset(s: &str, chars: u32) -> Result<u32> {
    let mut units = 0u32;
    let mut count = 0u32;
    for c in s.chars() {
        if count == chars {
            return Ok(units);
        }
        units += c.len_utf16() as u32;
        count += 1;
    }
    if count == chars {
        Ok(units)
    } else {
        Err(Error::InvalidTextRange {
            start: chars,
            end: chars,
            length: Some(count),
            message: format!("code-point offset {chars} is past the end (length {count})"),
        })
    }
}

/// The text of `s` in the UTF-16 range `range`.
///
/// Fails with [`Error::InvalidTextRange`] when the range is out of bounds or
/// either end splits a surrogate pair. For backends that hold the whole text
/// (the mock, small values).
pub fn slice_utf16(s: &str, range: TextRange) -> Result<String> {
    let units: Vec<u16> = s.encode_utf16().collect();
    slice_utf16_units(&units, range)
}

/// Slice an encoded provider observation without re-encoding every chunk.
#[doc(hidden)]
pub fn slice_utf16_units(units: &[u16], range: TextRange) -> Result<String> {
    let length = units.len() as u32;
    check_in_bounds(range, length)?;
    let piece = &units[range.start as usize..range.end as usize];
    let split = |i: usize| {
        i > 0 && i < units.len() && (0xDC00..=0xDFFF).contains(&units[i]) && {
            (0xD800..=0xDBFF).contains(&units[i - 1])
        }
    };
    if split(range.start as usize) || split(range.end as usize) {
        return Err(Error::InvalidTextRange {
            start: range.start,
            end: range.end,
            length: Some(length),
            message: "range splits a surrogate pair".to_string(),
        });
    }
    String::from_utf16(piece).map_err(|e| Error::InvalidTextRange {
        start: range.start,
        end: range.end,
        length: Some(length),
        message: format!("range is not valid UTF-16: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finder(text: &str, chunk: u32) -> impl Fn(&str, usize) -> Option<TextRange> + '_ {
        move |needle, occ| {
            let mut read = |r: TextRange| slice_utf16(text, r);
            let needle: Vec<u16> = needle.encode_utf16().collect();
            let scope = TextRange {
                start: 0,
                end: utf16_len(text),
            };
            find_in_chunks(&mut read, &needle, scope, occ, chunk).unwrap()
        }
    }

    #[test]
    fn find_across_chunk_boundaries() {
        let text = "abcabcabc total: 42 abc";
        for chunk in [1, 2, 3, 4, 7, 100] {
            let f = finder(text, chunk);
            assert_eq!(
                f("total:", 0),
                Some(TextRange { start: 10, end: 16 }),
                "chunk {chunk}"
            );
            assert_eq!(f("abc", 0), Some(TextRange { start: 0, end: 3 }));
            assert_eq!(f("abc", 3), Some(TextRange { start: 20, end: 23 }));
            assert_eq!(f("abc", 4), None, "chunk {chunk}");
            assert_eq!(f("nope", 0), None);
        }
    }

    #[test]
    fn find_matches_do_not_overlap() {
        let f = finder("aaaa", 2);
        assert_eq!(f("aa", 1), Some(TextRange { start: 2, end: 4 }));
        assert_eq!(f("aa", 2), None);
    }

    #[test]
    fn find_counts_utf16_units_and_survives_surrogates_at_chunk_ends() {
        // "😀" is two UTF-16 units, so "x" after it is at 2 and 5.
        let text = "😀x😀x";
        for chunk in [1, 2, 3, 4] {
            let f = finder(text, chunk);
            assert_eq!(f("x", 0), Some(TextRange { start: 2, end: 3 }));
            assert_eq!(f("x", 1), Some(TextRange { start: 5, end: 6 }));
            assert_eq!(f("😀", 1), Some(TextRange { start: 3, end: 5 }));
        }
    }

    #[test]
    fn slice_rejects_split_surrogates_and_out_of_bounds() {
        let text = "a😀b";
        assert_eq!(
            slice_utf16(text, TextRange { start: 1, end: 3 }).unwrap(),
            "😀"
        );
        assert!(matches!(
            slice_utf16(text, TextRange { start: 2, end: 3 }),
            Err(Error::InvalidTextRange { .. })
        ));
        assert!(matches!(
            slice_utf16(text, TextRange { start: 1, end: 2 }),
            Err(Error::InvalidTextRange { .. })
        ));
        assert!(matches!(
            slice_utf16(text, TextRange { start: 0, end: 9 }),
            Err(Error::InvalidTextRange {
                length: Some(4),
                ..
            })
        ));
    }

    #[test]
    fn offset_conversions_round_trip() {
        let text = "a😀é";
        assert_eq!(utf16_to_char_offset(text, 0).unwrap(), 0);
        assert_eq!(utf16_to_char_offset(text, 1).unwrap(), 1);
        assert_eq!(utf16_to_char_offset(text, 3).unwrap(), 2);
        assert_eq!(utf16_to_char_offset(text, 4).unwrap(), 3);
        assert!(utf16_to_char_offset(text, 2).is_err());
        assert!(utf16_to_char_offset(text, 5).is_err());
        assert_eq!(char_to_utf16_offset(text, 2).unwrap(), 3);
        assert_eq!(char_to_utf16_offset(text, 3).unwrap(), 4);
        assert!(char_to_utf16_offset(text, 4).is_err());
    }

    #[test]
    fn walk_clips_and_pages() {
        // Lines of 4 units over 0..10.
        let line = |pos: u32| Ok(TextRange::new(pos / 4 * 4, pos / 4 * 4 + 4).unwrap());
        let r = TextRange { start: 2, end: 10 };
        let (lines, next) = walk(r, 10, line, || Ok(10)).unwrap();
        assert_eq!(
            lines,
            vec![
                TextRange { start: 2, end: 4 },
                TextRange { start: 4, end: 8 },
                TextRange { start: 8, end: 10 }
            ]
        );
        assert_eq!(next, None);
        let (lines, next) = walk(r, 2, line, || Ok(10)).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(next, Some(8));
    }

    #[test]
    fn walk_refuses_a_unit_that_does_not_advance() {
        let stuck = |_pos: u32| Ok(TextRange { start: 0, end: 0 });
        assert!(matches!(
            walk(TextRange { start: 0, end: 5 }, 10, stuck, || Ok(5)),
            Err(Error::Platform { .. })
        ));
    }

    #[test]
    fn walk_reports_a_range_past_the_end_as_invalid() {
        // Lines of 4 units over a 6-unit text: at 6 the backend reports the
        // line that ends there, which does not advance.
        let line =
            |pos: u32| Ok(TextRange::new(pos.min(5) / 4 * 4, (pos / 4 * 4 + 4).min(6)).unwrap());
        assert!(matches!(
            walk(TextRange { start: 0, end: 9 }, 10, line, || Ok(6)),
            Err(Error::InvalidTextRange {
                length: Some(6),
                ..
            })
        ));
    }

    #[test]
    fn colour_hex_round_trip() {
        let c = Color {
            r: 0x1a,
            g: 0x2b,
            b: 0x3c,
            a: 255,
        };
        assert_eq!(c.to_hex(), "#1a2b3c");
        let t = Color { a: 0x80, ..c };
        assert_eq!(t.to_hex(), "#1a2b3c80");
        assert_eq!(serde_json::from_str::<Color>("\"#1a2b3c80\"").unwrap(), t);
    }

    #[test]
    fn text_unit_parse() {
        assert_eq!(TextUnit::parse("line").unwrap(), TextUnit::Line);
        assert!(matches!(
            TextUnit::parse("sentence"),
            Err(Error::InvalidActionData { .. })
        ));
    }
}

/// The table, collection and text APIs end to end against the mock.
#[cfg(test)]
mod api_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::mock::{build_provider, build_table_provider};
    use crate::provider::Provider;
    use crate::table::{Count, HeaderKind};

    fn locator(p: Arc<dyn Provider>, selector: &str) -> Locator {
        let root = p.list_apps().expect("mock apps").remove(0);
        Locator::new_for_app(p, root, selector).with_timeout(Duration::ZERO)
    }

    fn one(selector: &str) -> Element {
        locator(build_table_provider(), selector)
            .element()
            .expect("fixture element")
    }

    fn r(start: u32, end: u32) -> TextRange {
        TextRange { start, end }
    }

    #[test]
    fn embedded_object_mapping_distinguishes_unsupported_and_invalid_offsets() {
        let p = build_table_provider();
        let field = locator(p, "text_area").element().expect("mock text field");
        let text = field.text().expect("mock text");
        assert!(matches!(text.object_at(0), Err(Error::Unsupported { .. })));
        assert!(matches!(
            text.object_at(text.length().unwrap()),
            Err(Error::InvalidTextRange { .. })
        ));
    }

    #[test]
    fn table_metadata_reads_from_element_data() {
        let table = one("table");
        let info = table.table.expect("table info");
        assert_eq!(info.row_count, Some(Count::Known(3)));
        let price = one(r#"table_cell[name="Price"]"#);
        let c = price.cell.expect("cell info");
        assert_eq!(
            (c.row, c.column, c.column_span),
            (Some(0), Some(1), Some(2))
        );
        assert_eq!(c.header, Some(HeaderKind::Column));
        // A span the platform did not report stays None.
        assert_eq!(
            one(r#"table_cell[name="3.00"]"#).cell.unwrap().row_span,
            None
        );
        let first = one(r#"list_item[name="First"]"#).collection.unwrap();
        assert_eq!((first.index, first.size), (Some(0), Some(Count::Unknown)));
        assert_eq!(
            one(r#"tree_item[name="Child"]"#).collection.unwrap().depth,
            Some(1)
        );
    }

    #[test]
    fn cell_at_returns_the_merged_cell_for_a_covered_slot() {
        let table = one("table");
        let at = |r, c| table.cell_at(r, c).unwrap().and_then(|e| e.name.clone());
        assert_eq!(at(0, 2).as_deref(), Some("Price"));
        assert_eq!(at(1, 2).as_deref(), Some("2.00"));
        assert_eq!(at(9, 9), None);
    }

    #[test]
    fn headers_and_selection() {
        let table = one("table");
        let names = |v: Vec<Element>| {
            v.into_iter()
                .filter_map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(table.column_headers().unwrap()), ["Product", "Price"]);
        assert_eq!(names(table.row_headers().unwrap()), ["Widget", "Gadget"]);
        let cell = one(r#"table_cell[name="1.00"]"#);
        assert_eq!(names(cell.column_header_cells().unwrap()), ["Price"]);
        assert_eq!(names(cell.row_header_cells().unwrap()), ["Widget"]);
        // "Price" spans columns 1-2, so it heads column 2 as well.
        let spanned = one(r#"table_cell[name="4.00"]"#);
        assert_eq!(names(spanned.column_header_cells().unwrap()), ["Price"]);
        assert_eq!(names(table.selected_items().unwrap()), ["2.00"]);
        // A non-table refuses table queries.
        assert!(matches!(
            one(r#"list[name="Results"]"#).column_headers(),
            Err(Error::ActionNotSupported { .. })
        ));
    }

    #[test]
    fn element_text_refuses_a_node_without_text() {
        let button = locator(build_provider(), "button").element().unwrap();
        assert!(matches!(
            button.text(),
            Err(Error::ActionNotSupported { action, .. }) if action == "text"
        ));
    }

    #[test]
    fn text_reads_use_utf16_offsets() {
        let t = one("text_area").text().unwrap();
        // "Hello 😀 world\nsecond line": the emoji is two units.
        assert_eq!(t.length().unwrap(), 26);
        let world = t.find("world").unwrap().unwrap();
        assert_eq!(world, r(9, 14));
        assert_eq!(t.read(world).unwrap(), "world");
        assert_eq!(t.read(r(6, 8)).unwrap(), "😀");
        assert!(matches!(
            t.read(r(6, 7)),
            Err(Error::InvalidTextRange { .. })
        ));
        assert!(matches!(
            t.read(r(0, 99)),
            Err(Error::InvalidTextRange {
                length: Some(26),
                ..
            })
        ));
        assert_eq!(t.find("o").unwrap(), Some(r(4, 5)));
        assert_eq!(
            t.find_with("o", FindOptions::new().occurrence(1)).unwrap(),
            Some(r(10, 11))
        );
        assert_eq!(
            t.find_with("o", FindOptions::new().within(r(11, 15)))
                .unwrap(),
            None
        );
        assert_eq!(t.range_at(7, TextUnit::Character).unwrap(), r(6, 8));
        assert_eq!(t.range_at(10, TextUnit::Word).unwrap(), r(9, 15));
    }

    #[test]
    fn caret_and_selection_follow_select_text() {
        let el = one("text_area");
        let t = el.text().unwrap();
        assert_eq!(t.caret().unwrap(), None);
        assert!(t.selections().unwrap().is_empty());
        el.select_text(9, 14).unwrap();
        assert_eq!(t.selections().unwrap(), vec![r(9, 14)]);
        assert_eq!(t.caret().unwrap(), Some(14));
        assert!(matches!(
            el.select_text(0, 99),
            Err(Error::InvalidTextRange { .. })
        ));
    }

    #[test]
    fn lines_bounds_and_runs_page_and_clip() {
        let t = one("text_area").text().unwrap();
        let all = r(0, 26);
        let lines = t.lines(all, 10).unwrap();
        assert_eq!(lines.lines, vec![r(0, 15), r(15, 26)]);
        assert!(!lines.truncated);
        assert!(matches!(
            t.lines(all, 0),
            Err(Error::InvalidActionData { .. })
        ));
        assert!(matches!(
            t.runs(all, 0),
            Err(Error::InvalidActionData { .. })
        ));
        let page = t.lines(all, 1).unwrap();
        assert!(page.truncated);
        assert_eq!(page.next_start, Some(15));
        let rects = t.bounds(r(9, 20)).unwrap();
        assert_eq!(rects.len(), 2, "one rect per line fragment: {rects:?}");
        let runs = t.runs(r(0, 26), 2).unwrap();
        assert_eq!(runs.runs.len(), 2);
        assert_eq!(runs.runs[0].range, r(0, 6));
        assert_eq!(runs.runs[0].attributes.font_weight, Some(700));
        assert_eq!(runs.runs[1].attributes.font_weight, Some(400));
        assert_eq!(runs.next_start, Some(9));
        assert_eq!(runs.runs[1].attributes.underline, None);
    }

    #[test]
    fn lines_bounds_and_runs_past_the_end_are_invalid_ranges() {
        let t = one("text_area").text().unwrap();
        let past = r(0, 99);
        let invalid = |res: Result<()>| {
            assert!(
                matches!(
                    res,
                    Err(Error::InvalidTextRange {
                        length: Some(26),
                        ..
                    })
                ),
                "{res:?}"
            )
        };
        invalid(t.lines(past, 10).map(|_| ()));
        invalid(t.runs(past, 10).map(|_| ()));
        invalid(t.bounds(past).map(|_| ()));
    }

    #[test]
    fn locator_text_re_resolves_and_ignores_disabled_state() {
        let p = build_table_provider();
        let t = locator(p, "text_area").text();
        assert_eq!(t.length().unwrap(), 26);
        let missing = locator(build_table_provider(), r#"text_area[name="Nope"]"#).text();
        assert!(matches!(missing.length(), Err(Error::Timeout { .. })));
    }

    #[test]
    fn empty_find_needle_is_refused_before_resolving_the_target() {
        // The locator matches nothing, so resolving it would time out: the
        // argument error must come first.
        let missing = locator(build_table_provider(), r#"text_area[name="Nope"]"#).text();
        assert!(matches!(
            missing.find(""),
            Err(Error::InvalidActionData { .. })
        ));
    }
}
