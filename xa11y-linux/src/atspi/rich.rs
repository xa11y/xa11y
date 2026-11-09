//! Tables, collections and text by range over AT-SPI.
//!
//! # Table metadata
//!
//! Read while building each element, gated on what `GetInterfaces` already
//! reported, so a node without `Table` / `TableCell` costs nothing extra:
//!
//! - `Table.NRows` / `NColumns` → [`TableInfo`] (negative means unknown),
//!   unless the table declares its total in the `rowcount` / `colcount`
//!   object attributes (`aria-rowcount` / `aria-colcount`). `NRows` counts
//!   the rows that exist; the attribute is the size of the data set, which
//!   is what [`TableInfo`] reports when a virtualized grid has both.
//! - `TableCell.GetRowColumnSpan` → [`CellInfo`] row, column and spans.
//! - The `column header` / `row header` roles → [`CellInfo::header`]. GTK 3
//!   tree-view column headers have the role but no `TableCell` interface, so
//!   they carry a header kind and no position.
//! - Object attributes `posinset` / `setsize` / `level` (1-based, as in ARIA)
//!   → [`CollectionInfo`], read only for list items, tree items, tabs, radio
//!   buttons and rows.
//!
//! AT-SPI has no row-index API for a row object, so [`RowInfo`] is never
//! populated on Linux; a row's cells carry the row.
//!
//! Like every other per-field read in the element snapshot, a read that fails
//! leaves its field `None` rather than failing the whole element.
//!
//! # Text offsets
//!
//! The public unit is UTF-16 code units. The AT-SPI specification counts
//! characters (code points), which GTK, Chromium, Gecko and LibreOffice
//! follow; Qt's adaptor counts UTF-16 code units instead. The unit is chosen
//! from `Application.ToolkitName` ([`offset_unit_for`]). For code-point
//! toolkits every conversion reads the text before the offset, and only as
//! far as the operation needs, which is the accepted cost of one public
//! unit. A UTF-16 toolkit needs no conversion, so its reads fetch only the
//! units they are about (see `TextAccess`).
//!
//! # Line ends
//!
//! A rendered line or paragraph includes the line terminator that ends it,
//! as GTK reports it. Qt's `GetStringAtOffset` leaves the terminator out of
//! every line and, asked at the terminator itself, answers the line before
//! it, a range that does not contain the offset asked about. The terminator
//! is appended to such a line ([`include_line_terminator`]), so a line walk
//! advances and every toolkit reports the same lines.

use xa11y_core::table::index_from_one_based;
use xa11y_core::text_api::{
    char_to_utf16_offset, check_in_bounds, slice_utf16, utf16_len, utf16_to_char_offset, Color,
    TextAttributeParts, TextRange, TextRun, TextUnit,
};
use xa11y_core::{
    CellInfo, CellParts, CollectionInfo, CollectionParts, Count, HeaderKind, RowInfo, TableInfo,
    TableParts,
};

use super::*;

/// What [`LinuxProvider::read_table_metadata`] found for one element.
#[derive(Default)]
pub(super) struct TableMeta {
    pub(super) table: Option<TableInfo>,
    pub(super) cell: Option<CellInfo>,
    pub(super) table_row: Option<RowInfo>,
    pub(super) collection: Option<CollectionInfo>,
    pub(super) semantics: Semantics,
    pub(super) link_url: Option<String>,
}

/// The offset unit a toolkit's `org.a11y.atspi.Text` speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetUnit {
    /// The AT-SPI specification's unit: code points.
    CodePoints,
    /// Qt's adaptor: UTF-16 code units (issue #493).
    Utf16,
}

/// The offset unit for a toolkit name. `None` (no `ToolkitName` at all) is
/// the specification's code points.
pub(crate) fn offset_unit_for(toolkit: Option<&str>) -> OffsetUnit {
    match toolkit {
        Some(t) if t.eq_ignore_ascii_case("Qt") => OffsetUnit::Utf16,
        _ => OffsetUnit::CodePoints,
    }
}

/// Read `bus`'s offset unit from `proxy`, which must address the
/// application object's `org.a11y.atspi.Application` interface. Shared by
/// the provider's per-bus cache and the event subscription, which resolves
/// each application's unit once when it subscribes.
pub(crate) fn read_offset_unit(proxy: &Proxy<'_>, bus: &str) -> Result<OffsetUnit> {
    read_offset_unit_unless_gone(proxy, bus)?.ok_or_else(|| Error::Platform {
        code: -1,
        message: format!(
            "reading Application.ToolkitName of {bus} to choose the text offset unit failed: \
             the application has gone"
        ),
    })
}

/// [`read_offset_unit`], answering `None` when the application has left the
/// bus. The event subscription reads every sender's unit; an application
/// that exits between being found and being asked has no events to
/// interpret, and must not fail the whole subscription.
pub(crate) fn read_offset_unit_unless_gone(
    proxy: &Proxy<'_>,
    bus: &str,
) -> Result<Option<OffsetUnit>> {
    let toolkit = match proxy.get_property::<String>("ToolkitName") {
        Ok(name) => Some(name),
        Err(e) if is_property_absent(&e) => None,
        Err(e) if crate::atspi::is_gone(&e) => return Ok(None),
        Err(e) => {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "reading Application.ToolkitName of {bus} to choose the text offset \
                     unit failed: {e}"
                ),
            })
        }
    };
    Ok(Some(offset_unit_for(toolkit.as_deref())))
}

/// The header kind an AT-SPI role names, if any.
pub(super) fn header_kind_for(role_num: u32, role_name: &str) -> Option<HeaderKind> {
    match (role_num, role_name) {
        (10, _) | (_, "column header" | "table column header") => Some(HeaderKind::Column),
        (47, _) | (_, "row header" | "table row header") => Some(HeaderKind::Row),
        _ => None,
    }
}

/// Collection position from object attributes (1-based `posinset` /
/// `level`, `setsize` with `-1` for unknown).
pub(super) fn collection_from_attributes(
    attrs: &HashMap<String, String>,
) -> Option<CollectionInfo> {
    let int = |key: &str| attrs.get(key).and_then(|v| v.trim().parse::<i64>().ok());
    let info: CollectionInfo = CollectionParts {
        index: int("posinset").and_then(index_from_one_based),
        size: int("setsize").map(Count::from_signed),
        depth: int("level").and_then(index_from_one_based),
    }
    .into();
    (!info.is_empty()).then_some(info)
}

/// A table total the content declares: the `rowcount` / `colcount` object
/// attribute, which is how `aria-rowcount` / `aria-colcount` reach AT-SPI
/// (Core-AAM). It is the size of the whole data set, where `Table.NRows`
/// counts only the rows that exist now, so a virtualized grid reports its
/// total rather than its window. `-1` is an explicit unknown. An attribute
/// that is not a number is not a declaration, and leaves the count to the
/// `Table` interface.
pub(super) fn declared_count(attrs: &HashMap<String, String>, key: &str) -> Option<Count> {
    attrs
        .get(key)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map(Count::from_signed)
}

/// Roles whose elements can sit in a set.
/// Roles whose object attributes can carry document semantics: heading
/// level, landmark kind, ARIA role and live-region settings. Live regions sit
/// on generic sections (`group`) and status / alert elements; a bound keeps
/// the extra read off everything else.
fn carries_document_semantics(role: Role) -> bool {
    matches!(
        role,
        Role::Heading
            | Role::Landmark
            | Role::Link
            | Role::Group
            | Role::Status
            | Role::Alert
            | Role::WebArea
    )
}

/// Document semantics read from object attributes (#476).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Semantics {
    pub(crate) heading_level: Option<u32>,
    pub(crate) landmark_type: Option<xa11y_core::Landmark>,
    pub(crate) web_role: Option<String>,
    pub(crate) live_region: Option<xa11y_core::LiveRegionInfo>,
}

impl Semantics {
    /// What the role alone says, with no attributes read: a landmark whose
    /// kind is unknown.
    pub(crate) fn for_role(role: Role) -> Self {
        Self {
            landmark_type: (role == Role::Landmark).then_some(xa11y_core::Landmark::Unknown),
            ..Self::default()
        }
    }
}

/// Parse the object attributes that carry document semantics:
///
/// - `level` on a heading (a label: `1` is h1, kept as reported);
/// - `xml-roles`, the ARIA role, which also names a landmark's kind;
/// - `live` / `atomic` / `relevant` on a live region, or their `container-`
///   forms, which the region's descendants carry.
pub(crate) fn semantics_from_attributes(role: Role, attrs: &HashMap<String, String>) -> Semantics {
    let get = |k: &str| attrs.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let web_role = get("xml-roles").map(str::to_string);
    let heading_level = if role == Role::Heading {
        get("level").and_then(|v| v.parse::<u32>().ok())
    } else {
        None
    };
    let landmark_type = (role == Role::Landmark).then(|| {
        web_role
            .as_deref()
            .and_then(|r| {
                r.split_whitespace()
                    .find_map(xa11y_core::Landmark::from_aria_role)
            })
            .unwrap_or(xa11y_core::Landmark::Unknown)
    });
    let own_or_container = |k: &str| get(k).or_else(|| get(&format!("container-{k}")));
    let live: xa11y_core::LiveRegionInfo = xa11y_core::LiveRegionParts {
        politeness: own_or_container("live").and_then(xa11y_core::Politeness::parse),
        atomic: own_or_container("atomic").and_then(|v| match v {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
        relevant: own_or_container("relevant").map(str::to_string),
    }
    .into();
    Semantics {
        heading_level,
        landmark_type,
        web_role,
        live_region: (!live.is_empty()).then_some(live),
    }
}

fn is_collection_role(role: Role) -> bool {
    matches!(
        role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::RadioButton | Role::TableRow
    )
}

/// Parse an AT-SPI colour attribute: `"r,g,b"` with 8-bit channels (GTK 4,
/// Qt, WebKitGTK) or 16-bit channels (GTK 3's widgets). Values above 255
/// mark the 16-bit form.
///
/// The width is read from the values, not from the toolkit, because the
/// toolkit cannot tell: WebKitGTK's web process (a Tauri app's page) reports
/// `ToolkitName` "GTK" as GTK 3 does, and sends 8-bit channels. Reading every
/// "GTK" colour as 16-bit would turn its text black. The values can tell
/// instead: GTK 3 scales a colour's 0..=255 channels by 257, so a channel it
/// sends is 0 or at least 257, and never in 1..=255. Only a colour whose
/// channels are all 0 is ambiguous, and it is black in either width.
pub(super) fn parse_atspi_color(v: &str) -> Option<Color> {
    let parts: Vec<u32> = v
        .split(',')
        .map(|p| p.trim().parse::<u32>().ok())
        .collect::<Option<_>>()?;
    let [r, g, b] = parts[..] else { return None };
    let wide = r > 255 || g > 255 || b > 255;
    let ch = |c: u32| -> Option<u8> {
        if wide {
            u8::try_from(c / 257).ok()
        } else {
            u8::try_from(c).ok()
        }
    };
    Some(Color {
        r: ch(r)?,
        g: ch(g)?,
        b: ch(b)?,
        a: 255,
    })
}

/// Map an AT-SPI attribute run to [`TextAttributeParts`]. Anything absent
/// stays `None`.
pub(super) fn attributes_from_atspi(attrs: &HashMap<String, String>) -> TextAttributeParts {
    let get = |k: &str| attrs.get(k).map(|s| s.trim());
    TextAttributeParts {
        font_family: get("family-name")
            .map(unquote_family)
            .filter(|s| !s.is_empty())
            .map(String::from),
        font_size: get("size").and_then(|s| s.parse::<f64>().ok()),
        font_weight: get("weight").and_then(parse_weight),
        italic: get("style").map(|s| s == "italic" || s == "oblique"),
        underline: get("underline").map(|s| s != "none" && s != "false"),
        strikethrough: get("strikethrough").map(|s| s == "true"),
        foreground: get("fg-color").and_then(parse_atspi_color),
        background: get("bg-color").and_then(parse_atspi_color),
        // ATK reports `rise` in Pango units with no stated baseline, so a
        // script position cannot be read reliably.
        script: None,
        spelling_error: get("invalid").map(|s| s == "spelling"),
    }
}

/// A font weight: numeric as ATK reports it, or the CSS keyword Qt sends
/// (`"bold"`; Qt leaves the attribute out for normal text). The relative
/// keywords `bolder` / `lighter` name no weight on their own, so they are
/// not reported.
fn parse_weight(v: &str) -> Option<u16> {
    match v {
        "normal" => Some(400),
        "bold" => Some(700),
        n => n.parse::<u16>().ok(),
    }
}

/// A family name without the CSS quotes Qt wraps it in (`"Sans Serif"`
/// with the quotes, from Qt's IAccessible2-style `font-family`).
fn unquote_family(v: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|&q| v.strip_prefix(q).and_then(|r| r.strip_suffix(q)))
        .unwrap_or(v)
        .trim()
}

/// Whether a UTF-16 unit ends a line. `\r\n` is handled by the caller.
fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// `line`, a line or paragraph reported by the toolkit, with the terminator
/// that follows it appended when the toolkit left it out (Qt; see the module
/// docs). A line whose last unit is already a terminator, or that is not
/// followed by one (a soft wrap, the end of the text), is returned as is.
pub(super) fn include_line_terminator(units: &[u16], line: TextRange) -> TextRange {
    let end = line.end as usize;
    let last = if end > line.start as usize {
        end.checked_sub(1).and_then(|i| units.get(i)).copied()
    } else {
        None
    };
    terminate_line(line, last, units.get(end..).unwrap_or(&[]))
}

/// [`include_line_terminator`] from the unit before `line`'s end (`None`
/// for an empty line) and the units after it.
fn terminate_line(line: TextRange, last: Option<u16>, after: &[u16]) -> TextRange {
    let Some(&next) = after.first() else {
        return line;
    };
    if last.is_some_and(is_line_terminator) || !is_line_terminator(next) {
        return line;
    }
    let width = if next == 0x0D && after.get(1) == Some(&0x0A) {
        2
    } else {
        1
    };
    TextRange {
        start: line.start,
        end: line.end + width,
    }
}

/// An `(so)` object reference reply, or `None` for AT-SPI's null object.
fn object_ref(reply: zbus::Message, what: &str) -> Result<Option<AccessibleRef>> {
    let (bus_name, path): (String, zbus::zvariant::OwnedObjectPath) =
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("{what} deserialize failed: {e}"),
        })?;
    Ok(live_ref(bus_name, path.to_string()))
}

fn live_ref(bus_name: String, path: String) -> Option<AccessibleRef> {
    if path == "/org/a11y/atspi/null" || bus_name.is_empty() || path.is_empty() {
        None
    } else {
        Some(AccessibleRef { bus_name, path })
    }
}

fn call_failed(what: &str, e: zbus::Error) -> Error {
    Error::Platform {
        code: -1,
        message: format!("{what} failed: {e}"),
    }
}

/// Most header objects [`LinuxProvider::rich_table_headers`] asks for, one per
/// row or column, before refusing.
const MAX_HEADER_PROBES: i32 = 1000;

/// The `org.a11y.atspi.Text` calls the text layer makes, in the toolkit's
/// native offset unit. A trait so the offset arithmetic, and how much text
/// each operation reads, can be tested without a bus (issue #649).
trait TextSource {
    /// `CharacterCount`.
    fn character_count(&self) -> Result<i32>;
    /// `GetText(start, end)`.
    fn get_text(&self, start: i32, end: i32) -> Result<String>;
    /// The range `GetStringAtOffset(offset, granularity)` reports.
    fn string_at_offset(&self, offset: i32, granularity: u32) -> Result<(i32, i32)>;
    /// `GetAttributeRun(offset, true)`: the attributes and the run's range.
    fn attribute_run(&self, offset: i32) -> Result<(HashMap<String, String>, i32, i32)>;
}

impl TextSource for Proxy<'_> {
    fn character_count(&self) -> Result<i32> {
        self.get_property::<i32>("CharacterCount")
            .map_err(|e| call_failed("Text.CharacterCount", e))
    }

    fn get_text(&self, start: i32, end: i32) -> Result<String> {
        let reply = self
            .call_method("GetText", &(start, end))
            .map_err(|e| call_failed("Text.GetText", e))?;
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Text.GetText deserialize failed: {e}"),
        })
    }

    fn string_at_offset(&self, offset: i32, granularity: u32) -> Result<(i32, i32)> {
        let reply = self
            .call_method("GetStringAtOffset", &(offset, granularity))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::Unsupported {
                        feature: format!(
                            "Text.GetStringAtOffset is not implemented by this toolkit: {e}"
                        ),
                    }
                } else {
                    call_failed("Text.GetStringAtOffset", e)
                }
            })?;
        let (_text, start, end): (String, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetStringAtOffset deserialize failed: {e}"),
            })?;
        Ok((start, end))
    }

    fn attribute_run(&self, offset: i32) -> Result<(HashMap<String, String>, i32, i32)> {
        let reply = self
            .call_method("GetAttributeRun", &(offset, true))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::Unsupported {
                        feature: format!(
                            "Text.GetAttributeRun is not implemented by this toolkit: {e}"
                        ),
                    }
                } else {
                    call_failed("Text.GetAttributeRun", e)
                }
            })?;
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Text.GetAttributeRun deserialize failed: {e}"),
        })
    }
}

/// A text object and the unit its toolkit counts in.
///
/// # What each operation reads
///
/// A UTF-16 toolkit (Qt) needs no conversion, so an operation reads its
/// `CharacterCount` and only the units it is about: a range read fetches the
/// range, and a pair or line-terminator check fetches one or two units.
///
/// A code-point toolkit (GTK) can only convert an offset with the text
/// before it, so an operation reads a prefix from 0, and only as far as its
/// highest offset needs ([`Self::prefix_covering`]). The text's UTF-16
/// length is the one read that needs all of it.
struct TextAccess<S> {
    source: S,
    unit: OffsetUnit,
}

/// The text from offset 0, as far as an operation needed it.
struct Prefix {
    text: String,
    /// Whether `text` is all of the text.
    whole: bool,
    /// The toolkit's `CharacterCount`, in code points.
    count: i32,
}

impl Prefix {
    /// The text's UTF-16 length, known only when the prefix is all of it.
    fn length(&self) -> Option<u32> {
        self.whole.then(|| utf16_len(&self.text))
    }

    /// `e` reporting the text's length, or none: the prefix's own length is
    /// not the text's unless the prefix is all of it.
    fn fix(&self, e: Error) -> Error {
        match e {
            Error::InvalidTextRange {
                start,
                end,
                message,
                ..
            } => Error::InvalidTextRange {
                start,
                end,
                length: self.length(),
                message,
            },
            other => other,
        }
    }
}

/// A native offset the toolkit reported, refused when negative.
fn non_negative(n: i32, what: &str) -> Result<u32> {
    u32::try_from(n).map_err(|_| Error::Platform {
        code: -1,
        message: format!("{what} reported a negative text offset {n}"),
    })
}

/// A UTF-16 offset as the platform's 32-bit offset.
fn to_i32(n: u32, length: Option<u32>) -> Result<i32> {
    i32::try_from(n).map_err(|_| Error::InvalidTextRange {
        start: n,
        end: n,
        length,
        message: "offset does not fit the platform's 32-bit offset".to_string(),
    })
}

/// `ATSPI_TEXT_GRANULARITY_*` for a unit `GetStringAtOffset` answers.
fn granularity(unit: TextUnit) -> u32 {
    match unit {
        TextUnit::Character => 0,
        TextUnit::Word => 1,
        TextUnit::Line => 3,
        TextUnit::Paragraph => 4,
    }
}

impl<S: TextSource> TextAccess<S> {
    /// A UTF-16 toolkit's length: its `CharacterCount`.
    fn utf16_count(&self) -> Result<u32> {
        non_negative(self.source.character_count()?, "Text.CharacterCount")
    }

    /// The text from 0 to at least UTF-16 offset `end` (to the end of the
    /// text if it is shorter). Code-point toolkits only: `end` code points
    /// always cover `end` units, so one read suffices.
    fn prefix_covering(&self, end: u32) -> Result<Prefix> {
        let count = self.source.character_count()?.max(0);
        let want = i32::try_from(end).unwrap_or(i32::MAX).min(count);
        let text = self.source.get_text(0, want)?;
        if want == count {
            return Ok(Prefix {
                text,
                whole: true,
                count,
            });
        }
        if utf16_len(&text) >= end {
            return Ok(Prefix {
                text,
                whole: false,
                count,
            });
        }
        // Only reachable if the toolkit's count disagrees with its text.
        Ok(Prefix {
            text: self.source.get_text(0, count)?,
            whole: true,
            count,
        })
    }

    /// Whether UTF-16 units `i` and `i + 1` are a surrogate pair. UTF-16
    /// toolkits only. Reads just those two units: a pair comes back as one
    /// scalar value. Two scalars, or a lone half the toolkit had to replace
    /// to send it over D-Bus, are not a pair.
    fn pair_at(&self, i: u32, len: u32) -> Result<bool> {
        if i.saturating_add(2) > len {
            return Ok(false);
        }
        let two = self
            .source
            .get_text(to_i32(i, Some(len))?, to_i32(i + 2, Some(len))?)?;
        let mut chars = two.chars();
        Ok(matches!(
            (chars.next(), chars.next()),
            (Some(c), None) if c.len_utf16() == 2
        ))
    }

    /// Whether UTF-16 offset `at` falls inside a surrogate pair. UTF-16
    /// toolkits only.
    fn splits_pair(&self, at: u32, len: u32) -> Result<bool> {
        Ok(at > 0 && at < len && self.pair_at(at - 1, len)?)
    }

    /// Validate `range` as [`slice_utf16`] does (ordered, in bounds, no end
    /// splitting a pair) without reading it. UTF-16 toolkits only.
    fn check_range16(&self, range: TextRange, len: u32) -> Result<()> {
        check_in_bounds(range, len)?;
        let split = self.splits_pair(range.start, len)?
            || (range.end != range.start && self.splits_pair(range.end, len)?);
        if split {
            return Err(Error::InvalidTextRange {
                start: range.start,
                end: range.end,
                length: Some(len),
                message: "range splits a surrogate pair".to_string(),
            });
        }
        Ok(())
    }

    /// The text's length in UTF-16 units.
    fn length(&self) -> Result<u32> {
        match self.unit {
            OffsetUnit::Utf16 => self.utf16_count(),
            OffsetUnit::CodePoints => {
                let count = self.source.character_count()?.max(0);
                Ok(utf16_len(&self.source.get_text(0, count)?))
            }
        }
    }

    /// Convert native offsets to UTF-16. A code-point toolkit needs the text
    /// before the largest; `known`, a prefix already read, is used when it
    /// reaches that far.
    fn to_utf16(&self, native: &[i32], known: Option<&str>) -> Result<Vec<u32>> {
        let what = "the toolkit";
        match self.unit {
            OffsetUnit::Utf16 => native.iter().map(|&n| non_negative(n, what)).collect(),
            OffsetUnit::CodePoints => {
                let max = native.iter().copied().max().unwrap_or(0).max(0);
                let fetched;
                let text = match known {
                    Some(t) if t.chars().count() >= max as usize => t,
                    _ => {
                        fetched = self.source.get_text(0, max)?;
                        fetched.as_str()
                    }
                };
                native
                    .iter()
                    .map(|&n| char_to_utf16_offset(text, non_negative(n, what)?))
                    .collect()
            }
        }
    }

    /// The text of `range`, validated: past the end or splitting a surrogate
    /// pair fails with `InvalidTextRange`.
    fn read_range(&self, range: TextRange) -> Result<String> {
        match self.unit {
            OffsetUnit::Utf16 => {
                let len = self.utf16_count()?;
                self.check_range16(range, len)?;
                if range.start == range.end {
                    return Ok(String::new());
                }
                let text = self.source.get_text(
                    to_i32(range.start, Some(len))?,
                    to_i32(range.end, Some(len))?,
                )?;
                let got = utf16_len(&text);
                if got != range.end - range.start {
                    return Err(Error::Platform {
                        code: -1,
                        message: format!(
                            "Text.GetText({}, {}) returned {got} UTF-16 units, expected {}",
                            range.start,
                            range.end,
                            range.end - range.start
                        ),
                    });
                }
                Ok(text)
            }
            OffsetUnit::CodePoints => {
                // One unit past the end tells whether the end splits a pair.
                let p = self.prefix_covering(range.end.saturating_add(1))?;
                slice_utf16(&p.text, range).map_err(|e| p.fix(e))
            }
        }
    }

    /// The native range `GetStringAtOffset` reports for `unit` at native
    /// offset `native`.
    fn native_unit_at(&self, native: i32, unit: TextUnit) -> Result<(i32, i32)> {
        let (start, end) = self.source.string_at_offset(native, granularity(unit))?;
        // `(-1, -1)` for an in-bounds offset is a toolkit with no notion of
        // this unit: GTK 3's text view answers it for paragraphs.
        if start < 0 || end < 0 {
            return Err(Error::Unsupported {
                feature: format!(
                    "Text.GetStringAtOffset({native}, {}) returned ({start}, {end}): this \
                     toolkit does not report {} boundaries",
                    unit.to_snake_case(),
                    unit.to_snake_case()
                ),
            });
        }
        Ok((start, end))
    }

    /// The `unit` containing UTF-16 `offset`. At the end of the text it is
    /// the unit that ends there, and an offset on the second half of a
    /// surrogate pair answers for the pair, for every unit and toolkit (GTK's
    /// code-point conversion would otherwise refuse it while Qt's UTF-16
    /// offsets pass it through).
    fn range_at(&self, offset: u32, unit: TextUnit) -> Result<TextRange> {
        let point = TextRange {
            start: offset,
            end: offset,
        };
        let what = "Text.GetStringAtOffset";
        match self.unit {
            OffsetUnit::Utf16 => {
                let len = self.utf16_count()?;
                check_in_bounds(point, len)?;
                if len == 0 {
                    return Ok(TextRange { start: 0, end: 0 });
                }
                let at = if offset == len {
                    if len >= 2 && self.pair_at(len - 2, len)? {
                        len - 2
                    } else {
                        len - 1
                    }
                } else if self.splits_pair(offset, len)? {
                    offset - 1
                } else {
                    offset
                };
                if unit == TextUnit::Character {
                    let width = if self.pair_at(at, len)? { 2 } else { 1 };
                    return Ok(TextRange {
                        start: at,
                        end: (at + width).min(len),
                    });
                }
                let (start, end) = self.native_unit_at(to_i32(at, Some(len))?, unit)?;
                let range = TextRange {
                    start: non_negative(start, what)?,
                    end: non_negative(end, what)?,
                };
                match unit {
                    TextUnit::Line | TextUnit::Paragraph => self.terminated16(range, len),
                    TextUnit::Word | TextUnit::Character => Ok(range),
                }
            }
            OffsetUnit::CodePoints => {
                // Two units past `offset` cover the pair check and a
                // character's width.
                let p = self.prefix_covering(offset.saturating_add(2))?;
                if let Some(len) = p.length() {
                    check_in_bounds(point, len)?;
                    if len == 0 {
                        return Ok(TextRange { start: 0, end: 0 });
                    }
                }
                let units: Vec<u16> = p.text.encode_utf16().collect();
                let at = if p.length() == Some(offset) {
                    utf16_last_scalar_start(&p.text)
                } else {
                    offset
                };
                // `at` indexes `units`: a whole prefix holds `offset` < its
                // length (or `at` is its last scalar), and a partial one holds
                // at least `offset + 2` units.
                let at = if at > 0 && (0xDC00..=0xDFFF).contains(&units[at as usize]) {
                    at - 1
                } else {
                    at
                };
                if unit == TextUnit::Character {
                    let width = if (0xD800..=0xDBFF).contains(&units[at as usize]) {
                        2
                    } else {
                        1
                    };
                    return Ok(TextRange {
                        start: at,
                        end: (at + width).min(units.len() as u32),
                    });
                }
                let native = utf16_to_char_offset(&p.text, at).map_err(|e| p.fix(e))?;
                let (start, end) = self.native_unit_at(to_i32(native, p.length())?, unit)?;
                let (start, end) = (non_negative(start, what)?, non_negative(end, what)?);
                // Converting `end`, and the terminator check after it, need
                // the text up to two code points past `end`.
                let need = end.saturating_add(2);
                let text = if p.whole || p.text.chars().count() as u64 >= u64::from(need) {
                    p.text
                } else {
                    let want = i32::try_from(need).unwrap_or(i32::MAX).min(p.count);
                    self.source.get_text(0, want)?
                };
                let range = TextRange {
                    start: char_to_utf16_offset(&text, start)?,
                    end: char_to_utf16_offset(&text, end)?,
                };
                Ok(match unit {
                    TextUnit::Line | TextUnit::Paragraph => {
                        let units: Vec<u16> = text.encode_utf16().collect();
                        include_line_terminator(&units, range)
                    }
                    TextUnit::Word | TextUnit::Character => range,
                })
            }
        }
    }

    /// [`include_line_terminator`] for a UTF-16 toolkit, reading only the
    /// unit before `line`'s end and the two after it.
    fn terminated16(&self, line: TextRange, len: u32) -> Result<TextRange> {
        if line.end >= len {
            return Ok(line);
        }
        let end = to_i32(line.end, Some(len))?;
        // A lone surrogate half comes back replaced, never as a terminator,
        // so reading one at either edge cannot add a terminator wrongly.
        let last = if line.end > line.start {
            self.source.get_text(end - 1, end)?.encode_utf16().last()
        } else {
            None
        };
        let after_end = to_i32(line.end.saturating_add(2).min(len), Some(len))?;
        let after: Vec<u16> = self
            .source
            .get_text(end, after_end)?
            .encode_utf16()
            .collect();
        Ok(terminate_line(line, last, &after))
    }

    /// `range` validated as a read is (ordered, in bounds, no end splitting
    /// a surrogate pair) and converted to native offsets. Selection and
    /// bounds share it, so every toolkit refuses the same ranges.
    fn native_range(&self, range: TextRange) -> Result<(i32, i32)> {
        match self.unit {
            OffsetUnit::Utf16 => {
                let len = self.utf16_count()?;
                self.check_range16(range, len)?;
                Ok((
                    to_i32(range.start, Some(len))?,
                    to_i32(range.end, Some(len))?,
                ))
            }
            OffsetUnit::CodePoints => {
                let p = self.prefix_covering(range.end)?;
                // A partial prefix reaches `range.end`, so only an unordered
                // range can fail here; the fix drops the prefix's length.
                check_in_bounds(range, utf16_len(&p.text)).map_err(|e| p.fix(e))?;
                let conv = |n: u32| -> Result<i32> {
                    let c = utf16_to_char_offset(&p.text, n).map_err(|e| p.fix(e))?;
                    to_i32(c, p.length())
                };
                Ok((conv(range.start)?, conv(range.end)?))
            }
        }
    }

    /// The attribute run at UTF-16 `offset`.
    fn run_at(&self, offset: u32) -> Result<TextRun> {
        let past_end = |length: Option<u32>| Error::InvalidTextRange {
            start: offset,
            end: offset,
            length,
            message: "no text at or after this offset".to_string(),
        };
        let (native, prefix) = match self.unit {
            OffsetUnit::Utf16 => {
                let len = self.utf16_count()?;
                if offset >= len {
                    return Err(past_end(Some(len)));
                }
                (to_i32(offset, Some(len))?, None)
            }
            OffsetUnit::CodePoints => {
                let p = self.prefix_covering(offset.saturating_add(1))?;
                if let Some(len) = p.length().filter(|&len| offset >= len) {
                    return Err(past_end(Some(len)));
                }
                let n = utf16_to_char_offset(&p.text, offset).map_err(|e| p.fix(e))?;
                (to_i32(n, p.length())?, Some(p))
            }
        };
        let (attrs, start, end) = self.source.attribute_run(native)?;
        if start < 0 || end < start {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "Text.GetAttributeRun at {native} reported the run {start}..{end}"
                ),
            });
        }
        let ends = self.to_utf16(&[start, end], prefix.as_ref().map(|p| p.text.as_str()))?;
        let [start16, end16] = ends[..] else {
            return Err(Error::Platform {
                code: -1,
                message: "Text.GetAttributeRun range did not convert".to_string(),
            });
        };
        Ok(TextRun::new(
            TextRange {
                start: start16,
                end: end16,
            },
            attributes_from_atspi(&attrs).into(),
        ))
    }
}

impl LinuxProvider {
    /// Table, cell and collection metadata for one element; see the module
    /// docs. Called from `build_element_data_in`.
    pub(super) fn read_table_metadata(
        &self,
        aref: &AccessibleRef,
        role: Role,
        role_num: u32,
        role_name: &str,
        ifaces: Interfaces,
    ) -> TableMeta {
        // Like the rest of the snapshot, a failed read leaves its field
        // absent, but the failure is logged rather than dropped: a signature
        // mismatch would otherwise read as "not a cell" (issue #650).
        let unread = |what: &str, e: Error| {
            log::debug!("{what} of {}{} unread: {e}", aref.bus_name, aref.path);
        };
        let header = header_kind_for(role_num, role_name);
        // The header kind comes from the role, so it stands whether or not
        // the position can be read.
        let header_only = || {
            header.map(|h| {
                CellParts {
                    row: None,
                    column: None,
                    row_span: None,
                    column_span: None,
                    header: Some(h),
                }
                .into()
            })
        };
        let cell = if ifaces.table_cell {
            match self.read_cell_info(aref, header) {
                Ok(cell) => cell,
                Err(e) => {
                    unread("TableCell.GetRowColumnSpan", e);
                    header_only()
                }
            }
        } else {
            header_only()
        };
        // One GetAttributes read serves the collection position, the document
        // semantics (#476) and a table's declared totals, and only for nodes
        // that can carry them, so other nodes pay nothing.
        let attrs = if is_collection_role(role) || carries_document_semantics(role) || ifaces.table
        {
            match self.scan_attributes(aref) {
                Ok(Some(Some(attrs))) => Some(attrs),
                // The object has gone, or has no attributes: nothing to read.
                Ok(None) | Ok(Some(None)) => None,
                Err(e) => {
                    unread("attributes", e);
                    None
                }
            }
        } else {
            None
        };
        let table = if ifaces.table {
            self.read_table_info(aref, attrs.as_ref())
                .map_err(|e| unread("Table", e))
                .ok()
                .flatten()
        } else {
            None
        };
        let collection = attrs
            .as_ref()
            .filter(|_| is_collection_role(role))
            .and_then(collection_from_attributes);
        let semantics = attrs
            .as_ref()
            .map(|a| semantics_from_attributes(role, a))
            .unwrap_or_else(|| Semantics::for_role(role));
        let link_url = if role == Role::Link && ifaces.hyperlink {
            self.read_link_url(aref)
        } else {
            None
        };
        TableMeta {
            table,
            cell,
            table_row: None,
            collection,
            semantics,
            link_url,
        }
    }

    /// A link's URL: `Hyperlink.GetURI(0)`. `None` when the toolkit reports
    /// none (an empty string); a failed read is logged and left absent, like
    /// the other per-field reads in a snapshot.
    fn read_link_url(&self, aref: &AccessibleRef) -> Option<String> {
        let read = || -> Result<String> {
            let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Hyperlink")?;
            let reply = proxy
                .call_method("GetURI", &(0i32,))
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("Hyperlink.GetURI: {e}"),
                })?;
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Hyperlink.GetURI reply: {e}"),
            })
        };
        match read() {
            Ok(uri) => Some(uri).filter(|u| !u.is_empty()),
            Err(e) => {
                log::debug!(
                    "Hyperlink.GetURI of {}{} unread: {e}",
                    aref.bus_name,
                    aref.path
                );
                None
            }
        }
    }

    /// A table's size. A total the table declares in its object attributes
    /// (`rowcount` / `colcount`, see [`declared_count`]) is the one reported;
    /// `Table.NRows` / `NColumns` answer otherwise.
    fn read_table_info(
        &self,
        aref: &AccessibleRef,
        attrs: Option<&HashMap<String, String>>,
    ) -> Result<Option<TableInfo>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")?;
        // Each count is its own field: one that fails is logged and left
        // absent without hiding the other.
        let count = |attr: &str, p: &str| {
            if let Some(declared) = attrs.and_then(|a| declared_count(a, attr)) {
                return Some(declared);
            }
            match proxy.get_property::<i32>(p) {
                Ok(n) => Some(Count::from_signed(n.into())),
                Err(e) => {
                    log::debug!("Table.{p} of {}{} unread: {e}", aref.bus_name, aref.path);
                    None
                }
            }
        };
        let info: TableInfo = TableParts {
            row_count: count("rowcount", "NRows"),
            column_count: count("colcount", "NColumns"),
        }
        .into();
        Ok((!info.is_empty()).then_some(info))
    }

    fn read_cell_info(
        &self,
        aref: &AccessibleRef,
        header: Option<HeaderKind>,
    ) -> Result<Option<CellInfo>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.TableCell")?;
        let reply = proxy
            .call_method("GetRowColumnSpan", &())
            .map_err(|e| call_failed("TableCell.GetRowColumnSpan", e))?;
        let (row, column, row_span, column_span): (i32, i32, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("TableCell.GetRowColumnSpan deserialize failed: {e}"),
            })?;
        let index = |n: i32| u32::try_from(n).ok();
        let span = |n: i32| u32::try_from(n).ok().filter(|&s| s >= 1);
        let info: CellInfo = CellParts {
            row: index(row),
            column: index(column),
            row_span: span(row_span),
            column_span: span(column_span),
            header,
        }
        .into();
        Ok((!info.is_empty()).then_some(info))
    }

    /// The live object behind `element`, refusing one without `iface`.
    fn with_interface(
        &self,
        element: &ElementData,
        iface: &'static str,
        action: &str,
    ) -> Result<AccessibleRef> {
        let aref = self.get_cached(element.handle)?;
        let ifaces = self.get_interfaces(&aref)?;
        let has = match iface {
            "org.a11y.atspi.Table" => ifaces.table,
            "org.a11y.atspi.TableCell" => ifaces.table_cell,
            "org.a11y.atspi.Selection" => ifaces.selection,
            "org.a11y.atspi.Text" => ifaces.text,
            other => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("no interface gate for {other}"),
                })
            }
        };
        if has {
            Ok(aref)
        } else {
            Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            })
        }
    }

    fn build_refs(&self, refs: Vec<AccessibleRef>, pid: Option<u32>) -> Vec<ElementData> {
        refs.iter()
            .map(|r| self.build_element_data(r, pid))
            .collect()
    }

    pub(super) fn rich_table_cell_at(
        &self,
        table: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        let aref = self.with_interface(table, "org.a11y.atspi.Table", "table")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")?;
        let rows: i32 = proxy
            .get_property("NRows")
            .map_err(|e| call_failed("Table.NRows", e))?;
        let cols: i32 = proxy
            .get_property("NColumns")
            .map_err(|e| call_failed("Table.NColumns", e))?;
        // Out of range is an answer, not an error; AT-SPI toolkits disagree on
        // what GetAccessibleAt returns there, so ask only in range.
        let (Ok(r), Ok(c)) = (i32::try_from(row), i32::try_from(column)) else {
            return Ok(None);
        };
        // A negative count is "unknown" (virtualized content): that axis has
        // no known bound, so the platform is asked rather than every slot
        // being answered as out of range.
        if (rows >= 0 && r >= rows) || (cols >= 0 && c >= cols) {
            return Ok(None);
        }
        let reply = proxy
            .call_method("GetAccessibleAt", &(r, c))
            .map_err(|e| call_failed("Table.GetAccessibleAt", e))?;
        Ok(object_ref(reply, "Table.GetAccessibleAt")?
            .map(|cell| self.build_element_data(&cell, table.pid)))
    }

    pub(super) fn rich_table_headers(
        &self,
        table: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(table, "org.a11y.atspi.Table", "table")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Table")?;
        let (count_prop, method) = match kind {
            HeaderKind::Column => ("NColumns", "GetColumnHeader"),
            HeaderKind::Row => ("NRows", "GetRowHeader"),
        };
        let n: i32 = proxy
            .get_property(count_prop)
            .map_err(|e| call_failed(count_prop, e))?;
        // A negative count is "unknown" (virtualized content): there is no
        // range to probe, and an empty answer would read as "no headers".
        if n < 0 {
            return Err(Error::Unsupported {
                feature: format!(
                    "{method} on a table whose {count_prop} is unknown ({n}); read the headers \
                     of individual cells with column_header_cells / row_header_cells"
                ),
            });
        }
        if n > MAX_HEADER_PROBES {
            return Err(Error::Unsupported {
                feature: format!(
                    "{method} over {n} {}s: more than {MAX_HEADER_PROBES}; read the headers \
                     of individual cells with column_header_cells / row_header_cells",
                    kind.to_snake_case()
                ),
            });
        }
        let mut refs = Vec::new();
        for i in 0..n.max(0) {
            let reply = proxy
                .call_method(method, &(i,))
                .map_err(|e| call_failed(method, e))?;
            if let Some(r) = object_ref(reply, method)? {
                // A header spanning several columns is returned once per column.
                if !refs
                    .iter()
                    .any(|x: &AccessibleRef| x.path == r.path && x.bus_name == r.bus_name)
                {
                    refs.push(r);
                }
            }
        }
        Ok(self.build_refs(refs, table.pid))
    }

    pub(super) fn rich_cell_headers(
        &self,
        cell: &ElementData,
        kind: HeaderKind,
    ) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(cell, "org.a11y.atspi.TableCell", "cell_headers")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.TableCell")?;
        let method = match kind {
            HeaderKind::Column => "GetColumnHeaderCells",
            HeaderKind::Row => "GetRowHeaderCells",
        };
        let reply = proxy
            .call_method(method, &())
            .map_err(|e| call_failed(method, e))?;
        let list: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("{method} deserialize failed: {e}"),
            })?;
        let refs = list
            .into_iter()
            .filter_map(|(b, p)| live_ref(b, p.to_string()))
            .collect();
        Ok(self.build_refs(refs, cell.pid))
    }

    pub(super) fn rich_selected_items(&self, container: &ElementData) -> Result<Vec<ElementData>> {
        let aref = self.with_interface(container, "org.a11y.atspi.Selection", "selected_items")?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Selection")?;
        let n: i32 = proxy
            .get_property("NSelectedChildren")
            .map_err(|e| call_failed("Selection.NSelectedChildren", e))?;
        // Each item is a full element build (about ten round trips), so a
        // select-all over a large view is refused rather than walked.
        if n > MAX_HEADER_PROBES {
            return Err(Error::Unsupported {
                feature: format!(
                    "selected_items over {n} selected children: more than {MAX_HEADER_PROBES}"
                ),
            });
        }
        let mut refs = Vec::new();
        for i in 0..n.max(0) {
            let reply = proxy
                .call_method("GetSelectedChild", &(i,))
                .map_err(|e| call_failed("Selection.GetSelectedChild", e))?;
            if let Some(r) = object_ref(reply, "Selection.GetSelectedChild")? {
                refs.push(r);
            }
        }
        Ok(self.build_refs(refs, container.pid))
    }

    // ── Text ────────────────────────────────────────────────────────

    /// The offset unit of `bus`'s toolkit, cached per bus.
    pub(super) fn offset_unit(&self, bus: &str) -> Result<OffsetUnit> {
        if let Some(unit) = self
            .gtk4
            .offset_units
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(bus)
        {
            return Ok(*unit);
        }
        let proxy = self.make_proxy(bus, APP_ROOT_PATH, "org.a11y.atspi.Application")?;
        let unit = read_offset_unit(&proxy, bus)?;
        let mut units = self
            .gtk4
            .offset_units
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Bus names are never reused, so entries for exited apps would pile
        // up in a long-running process; bound the map like `toplevels`.
        if units.len() >= GTK4_TOPLEVEL_CACHE_LIMIT {
            units.clear();
        }
        units.insert(bus.to_owned(), unit);
        Ok(unit)
    }

    fn text_access(&self, element: &ElementData) -> Result<TextAccess<Proxy<'_>>> {
        let aref = self.with_interface(element, "org.a11y.atspi.Text", "text")?;
        let unit = self.offset_unit(&aref.bus_name)?;
        let source = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Text")?;
        Ok(TextAccess { source, unit })
    }

    pub(super) fn rich_text_length(&self, element: &ElementData) -> Result<u32> {
        self.text_access(element)?.length()
    }

    pub(super) fn rich_text_caret(&self, element: &ElementData) -> Result<Option<u32>> {
        let t = self.text_access(element)?;
        let caret: i32 = t
            .source
            .get_property("CaretOffset")
            .map_err(|e| call_failed("Text.CaretOffset", e))?;
        if caret < 0 {
            return Ok(None);
        }
        Ok(Some(t.to_utf16(&[caret], None)?[0]))
    }

    pub(super) fn rich_text_selections(&self, element: &ElementData) -> Result<Vec<TextRange>> {
        let t = self.text_access(element)?;
        let n: i32 = t
            .source
            .call_method("GetNSelections", &())
            .map_err(|e| call_failed("Text.GetNSelections", e))?
            .body()
            .deserialize()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetNSelections deserialize failed: {e}"),
            })?;
        let mut native = Vec::new();
        for i in 0..n.max(0) {
            let (start, end): (i32, i32) = t
                .source
                .call_method("GetSelection", &(i,))
                .map_err(|e| call_failed("Text.GetSelection", e))?
                .body()
                .deserialize()
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("Text.GetSelection deserialize failed: {e}"),
                })?;
            if start != end {
                native.push(start.min(end));
                native.push(start.max(end));
            }
        }
        let utf16 = t.to_utf16(&native, None)?;
        let mut ranges: Vec<TextRange> = utf16
            .chunks(2)
            .map(|p| TextRange {
                start: p[0],
                end: p[1],
            })
            .collect();
        ranges.sort_by_key(|r| r.start);
        Ok(ranges)
    }

    pub(super) fn rich_text_read(&self, element: &ElementData, range: TextRange) -> Result<String> {
        self.text_access(element)?.read_range(range)
    }

    pub(super) fn rich_text_range_at(
        &self,
        element: &ElementData,
        offset: u32,
        unit: TextUnit,
    ) -> Result<TextRange> {
        self.text_access(element)?.range_at(offset, unit)
    }

    pub(super) fn rich_text_range_bounds(
        &self,
        element: &ElementData,
        range: TextRange,
    ) -> Result<Option<Rect>> {
        let t = self.text_access(element)?;
        let (start, end) = t.native_range(range)?;
        let aref = self.get_cached(element.handle)?;
        let gtk4 = self.is_gtk4(&aref.bus_name)?;
        // GTK 4's SCREEN coordinates are wrong (issue #487): ask for WINDOW
        // coordinates and add the window's origin, as element bounds do.
        let coord: u32 = if gtk4 { 1 } else { 0 };
        let reply = t
            .source
            .call_method("GetRangeExtents", &(start, end, coord))
            .map_err(|e| call_failed("Text.GetRangeExtents", e))?;
        let (x, y, w, h): (i32, i32, i32, i32) =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetRangeExtents deserialize failed: {e}"),
            })?;
        let rect = if gtk4 {
            // Element bounds go absent here; a text range's bounds fail,
            // because `Text::bounds` would otherwise drop the line from its
            // answer without a word (issue #650).
            let (ox, oy) =
                self.gtk4_window_origin_or_reason(&aref)
                    .map_err(|reason| Error::Platform {
                        code: -1,
                        message: format!(
                            "GTK 4 reports text extents relative to the window, and the \
                         window's screen origin is unknown: {reason}"
                        ),
                    })?;
            match (x.checked_add(ox), y.checked_add(oy)) {
                (Some(x), Some(y)) => extents_to_rect(x, y, w, h),
                _ => None,
            }
        } else {
            extents_to_rect(x, y, w, h)
        };
        Ok(rect.map(|r| r.to_logical(crate::scale::coordinate_scale())))
    }

    pub(super) fn rich_text_attributes_at(
        &self,
        element: &ElementData,
        offset: u32,
    ) -> Result<TextRun> {
        self.text_access(element)?.run_at(offset)
    }

    /// `Text.SetSelection` with UTF-16 offsets.
    pub(super) fn rich_set_text_selection(
        &self,
        element: &ElementData,
        start: u32,
        end: u32,
    ) -> Result<()> {
        let t = self.text_access(element)?;
        let (native_start, native_end) = t.native_range(TextRange { start, end })?;
        let target = self.get_cached(element.handle)?;
        // `SetSelection(0, ..)` changes selection 0, so it needs one to exist:
        // GTK 3 answers FALSE when nothing is selected. `AddSelection` is the
        // call that creates one. Which applies is a state read, not a retry.
        let existing: i32 = t
            .source
            .call_method("GetNSelections", &())
            .map_err(|e| call_failed("Text.GetNSelections", e))?
            .body()
            .deserialize()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.GetNSelections deserialize failed: {e}"),
            })?;
        let (member, reply) = if existing > 0 {
            let reply = t
                .source
                .call_method("SetSelection", &(0i32, native_start, native_end))
                .map_err(|e| call_failed("Text.SetSelection", e))?;
            ("SetSelection", reply)
        } else {
            let reply = t
                .source
                .call_method("AddSelection", &(native_start, native_end))
                .map_err(|e| call_failed("Text.AddSelection", e))?;
            ("AddSelection", reply)
        };
        atspi_bool_reply(
            reply,
            &format!("Text.{member}({start}, {end})"),
            &target,
            element,
        )
    }
}

/// UTF-16 offset of the last scalar value of a non-empty string.
fn utf16_last_scalar_start(s: &str) -> u32 {
    let len = utf16_len(s);
    let last = s.chars().last().map(|c| c.len_utf16() as u32).unwrap_or(0);
    len - last
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as Attrs;

    fn attrs(pairs: &[(&str, &str)]) -> Attrs<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn document_semantics_come_from_object_attributes() {
        let heading = super::semantics_from_attributes(
            Role::Heading,
            &attrs(&[("level", "2"), ("xml-roles", "heading")]),
        );
        assert_eq!(heading.heading_level, Some(2));
        assert_eq!(heading.web_role.as_deref(), Some("heading"));
        assert_eq!(heading.landmark_type, None);

        let main =
            super::semantics_from_attributes(Role::Landmark, &attrs(&[("xml-roles", "main")]));
        assert_eq!(main.landmark_type, Some(xa11y_core::Landmark::Main));
        let odd = super::semantics_from_attributes(Role::Landmark, &attrs(&[("xml-roles", "x")]));
        assert_eq!(odd.landmark_type, Some(xa11y_core::Landmark::Unknown));

        // A live region's own settings, and a descendant's inherited ones.
        let own = super::semantics_from_attributes(
            Role::Group,
            &attrs(&[
                ("live", "polite"),
                ("atomic", "true"),
                ("relevant", "additions text"),
            ]),
        );
        let live = own.live_region.expect("live region");
        assert_eq!(live.politeness, Some(xa11y_core::Politeness::Polite));
        assert_eq!(live.atomic, Some(true));
        assert_eq!(live.relevant.as_deref(), Some("additions text"));
        let inherited = super::semantics_from_attributes(
            Role::Group,
            &attrs(&[("container-live", "assertive")]),
        );
        assert_eq!(
            inherited.live_region.and_then(|l| l.politeness),
            Some(xa11y_core::Politeness::Assertive)
        );

        // Nothing relevant: nothing reported, and a level on a non-heading
        // is not a heading level.
        let plain = super::semantics_from_attributes(Role::Group, &attrs(&[("level", "3")]));
        assert_eq!(plain, super::Semantics::default());
    }

    use super::*;

    #[test]
    fn qt_counts_utf16_and_everything_else_code_points() {
        assert_eq!(offset_unit_for(Some("Qt")), OffsetUnit::Utf16);
        assert_eq!(offset_unit_for(Some("GTK")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(Some("Chromium")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(Some("Gecko")), OffsetUnit::CodePoints);
        assert_eq!(offset_unit_for(None), OffsetUnit::CodePoints);
    }

    #[test]
    fn header_roles() {
        assert_eq!(header_kind_for(10, ""), Some(HeaderKind::Column));
        assert_eq!(
            header_kind_for(0, "table row header"),
            Some(HeaderKind::Row)
        );
        assert_eq!(header_kind_for(56, "table cell"), None);
    }

    #[test]
    fn collection_attributes_are_one_based_and_minus_one_is_unknown() {
        let attrs: HashMap<String, String> = [("posinset", "3"), ("setsize", "-1"), ("level", "2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let c = collection_from_attributes(&attrs).unwrap();
        assert_eq!(
            (c.index, c.size, c.depth),
            (Some(2), Some(Count::Unknown), Some(1))
        );
        assert!(collection_from_attributes(&HashMap::new()).is_none());
    }

    #[test]
    fn declared_table_totals_are_counts_and_minus_one_is_unknown() {
        let attrs: HashMap<String, String> =
            [("rowcount", "1000"), ("colcount", "-1"), ("setsize", "x")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        assert_eq!(declared_count(&attrs, "rowcount"), Some(Count::Known(1000)));
        assert_eq!(declared_count(&attrs, "colcount"), Some(Count::Unknown));
        // Absent or not a number: no declaration, so the Table interface answers.
        assert_eq!(declared_count(&attrs, "rowindex"), None);
        assert_eq!(declared_count(&attrs, "setsize"), None);
    }

    #[test]
    fn colours_in_both_widths() {
        assert_eq!(
            parse_atspi_color("65535,0,32896").unwrap().to_hex(),
            "#ff0080"
        );
        assert_eq!(parse_atspi_color("255,0,128").unwrap().to_hex(), "#ff0080");
        // GTK 3 sends 8-bit colours scaled by 257: never a channel in 1..=255.
        assert_eq!(parse_atspi_color("257,0,0").unwrap().to_hex(), "#010000");
        assert_eq!(parse_atspi_color("0,0,0").unwrap().to_hex(), "#000000");
        assert!(parse_atspi_color("1,2").is_none());
    }

    #[test]
    fn attribute_runs_leave_absent_fields_unreported() {
        let attrs: HashMap<String, String> = [("weight", "700"), ("style", "italic")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let a: xa11y_core::TextAttributes = attributes_from_atspi(&attrs).into();
        assert_eq!(a.font_weight, Some(700));
        assert_eq!(a.italic, Some(true));
        assert_eq!(a.underline, None);
        assert_eq!(a.spelling_error, None);
    }

    #[test]
    fn qt_weight_keywords_and_quoted_families() {
        // What Qt 6.12 sends for a bold run (captured over D-Bus).
        let attrs: HashMap<String, String> = [
            ("family-name", "\"Sans Serif\""),
            ("size", "9"),
            ("style", "normal"),
            ("weight", "bold"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let a: xa11y_core::TextAttributes = attributes_from_atspi(&attrs).into();
        assert_eq!(a.font_family.as_deref(), Some("Sans Serif"));
        assert_eq!(a.font_weight, Some(700));
        assert_eq!(a.font_size, Some(9.0));
        assert_eq!(a.italic, Some(false));
        assert_eq!(parse_weight("normal"), Some(400));
        assert_eq!(parse_weight("bolder"), None);
        assert_eq!(unquote_family("'Noto Sans'"), "Noto Sans");
        assert_eq!(unquote_family("Cantarell"), "Cantarell");
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_line_without_its_terminator_gets_it() {
        // Qt: "Hi 😀 wörld 𝄞\nsecond line" reports the first line as 0..14,
        // also when asked at the newline (14) itself.
        let text = u16s("Hi \u{1F600} w\u{f6}rld \u{1D11E}\nsecond line");
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 0, end: 14 }),
            TextRange { start: 0, end: 15 }
        );
        // The last line has no terminator to take.
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 15, end: 26 }),
            TextRange { start: 15, end: 26 }
        );
        // `\r\n` is one terminator.
        let crlf = u16s("ab\r\ncd");
        assert_eq!(
            include_line_terminator(&crlf, TextRange { start: 0, end: 2 }),
            TextRange { start: 0, end: 4 }
        );
        // An empty line between two newlines.
        let blank = u16s("a\n\nb");
        assert_eq!(
            include_line_terminator(&blank, TextRange { start: 2, end: 2 }),
            TextRange { start: 2, end: 3 }
        );
    }

    #[test]
    fn a_line_that_has_its_terminator_is_left_alone() {
        // GTK: "a\n\nb" reports 0..2 and then the blank line 2..3; the
        // newline after the first line is the blank line's, not its own.
        let text = u16s("a\n\nb");
        assert_eq!(
            include_line_terminator(&text, TextRange { start: 0, end: 2 }),
            TextRange { start: 0, end: 2 }
        );
        // A soft-wrapped line is followed by text, not a terminator.
        let wrapped = u16s("one two\n");
        assert_eq!(
            include_line_terminator(&wrapped, TextRange { start: 0, end: 4 }),
            TextRange { start: 0, end: 4 }
        );
    }

    // ── Text by range over a fake `Text` object (issue #649) ─────────

    /// A `Text` object holding `text`, answering in `unit` as Qt (UTF-16,
    /// lines without their terminator) or GTK (code points, lines with it)
    /// does, and counting what it is asked to send.
    struct FakeText {
        /// The text in the native unit: UTF-16 units or scalar values.
        items: Vec<u32>,
        unit: OffsetUnit,
        get_text_calls: std::cell::Cell<usize>,
        items_sent: std::cell::Cell<usize>,
    }

    impl FakeText {
        fn new(text: &str, unit: OffsetUnit) -> TextAccess<FakeText> {
            let items = match unit {
                OffsetUnit::Utf16 => text.encode_utf16().map(u32::from).collect(),
                OffsetUnit::CodePoints => text.chars().map(u32::from).collect(),
            };
            TextAccess {
                source: FakeText {
                    items,
                    unit,
                    get_text_calls: Default::default(),
                    items_sent: Default::default(),
                },
                unit,
            }
        }

        fn n(&self) -> i32 {
            self.items.len() as i32
        }

        fn is_newline(&self, i: i32) -> bool {
            self.items.get(i as usize) == Some(&0x0A)
        }

        fn is_space(&self, i: i32) -> bool {
            matches!(self.items.get(i as usize), Some(&0x20) | Some(&0x0A))
        }
    }

    impl TextSource for FakeText {
        fn character_count(&self) -> Result<i32> {
            Ok(self.n())
        }

        fn get_text(&self, start: i32, end: i32) -> Result<String> {
            let (s, e) = (
                start.clamp(0, self.n()) as usize,
                end.clamp(0, self.n()) as usize,
            );
            let piece = &self.items[s..e.max(s)];
            self.get_text_calls.set(self.get_text_calls.get() + 1);
            self.items_sent.set(self.items_sent.get() + piece.len());
            Ok(match self.unit {
                // A lone surrogate half cannot cross D-Bus: Qt replaces it.
                OffsetUnit::Utf16 => {
                    String::from_utf16_lossy(&piece.iter().map(|&u| u as u16).collect::<Vec<_>>())
                }
                OffsetUnit::CodePoints => piece.iter().filter_map(|&c| char::from_u32(c)).collect(),
            })
        }

        fn string_at_offset(&self, offset: i32, granularity: u32) -> Result<(i32, i32)> {
            let n = self.n();
            match granularity {
                // Word: a run of non-space, or a single space.
                1 => {
                    if self.is_space(offset) {
                        return Ok((offset, offset + 1));
                    }
                    let mut s = offset;
                    while s > 0 && !self.is_space(s - 1) {
                        s -= 1;
                    }
                    let mut e = offset;
                    while e < n && !self.is_space(e) {
                        e += 1;
                    }
                    Ok((s, e))
                }
                // Line / paragraph: the text has no soft wraps.
                3 | 4 => {
                    let mut s = offset;
                    while s > 0 && !self.is_newline(s - 1) {
                        s -= 1;
                    }
                    let mut e = offset;
                    while e < n && !self.is_newline(e) {
                        e += 1;
                    }
                    match self.unit {
                        // Qt: no terminator; at the terminator itself, the
                        // line it ends.
                        OffsetUnit::Utf16 => Ok((s, e)),
                        OffsetUnit::CodePoints => Ok((s, (e + 1).min(n))),
                    }
                }
                other => panic!("granularity {other}"),
            }
        }

        fn attribute_run(&self, offset: i32) -> Result<(HashMap<String, String>, i32, i32)> {
            // Runs of four native units.
            let start = offset / 4 * 4;
            Ok((HashMap::new(), start, (start + 4).min(self.n())))
        }
    }

    const MIXED: &str = "Hi \u{1F600} w\u{f6}rld \u{1D11E}\nsecond line\n\nlast";

    /// Ok values equal, or both errors.
    fn same<T: PartialEq + std::fmt::Debug>(a: Result<T>, b: Result<T>, what: &str) {
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{what}"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!("{what}: UTF-16 {a:?} vs code points {b:?}"),
        }
    }

    #[test]
    fn both_units_answer_every_offset_alike() {
        let qt = FakeText::new(MIXED, OffsetUnit::Utf16);
        let gtk = FakeText::new(MIXED, OffsetUnit::CodePoints);
        let len = utf16_len(MIXED);
        assert_eq!(qt.length().unwrap(), len);
        assert_eq!(gtk.length().unwrap(), len);
        for offset in 0..=len + 1 {
            for unit in [
                TextUnit::Character,
                TextUnit::Word,
                TextUnit::Line,
                TextUnit::Paragraph,
            ] {
                same(
                    qt.range_at(offset, unit),
                    gtk.range_at(offset, unit),
                    &format!("range_at({offset}, {unit:?})"),
                );
            }
            for end in offset..=len + 1 {
                let range = TextRange { start: offset, end };
                let what = format!("{range:?}");
                same(qt.read_range(range), gtk.read_range(range), &what);
                same(
                    qt.read_range(range),
                    slice_utf16(MIXED, range),
                    &format!("read {what} against the whole text"),
                );
                same(
                    qt.native_range(range).map(|_| ()),
                    gtk.native_range(range).map(|_| ()),
                    &format!("bounds {what}"),
                );
            }
        }
    }

    #[test]
    fn range_at_anchors() {
        for t in [
            FakeText::new(MIXED, OffsetUnit::Utf16),
            FakeText::new(MIXED, OffsetUnit::CodePoints),
        ] {
            let r = |start, end| TextRange { start, end };
            // The second half of 😀 answers for the pair.
            assert_eq!(t.range_at(4, TextUnit::Character).unwrap(), r(3, 5));
            assert_eq!(t.range_at(3, TextUnit::Word).unwrap(), r(3, 5));
            // The first line, asked at its newline, includes it.
            assert_eq!(t.range_at(14, TextUnit::Line).unwrap(), r(0, 15));
            // The blank line is its newline.
            assert_eq!(t.range_at(27, TextUnit::Line).unwrap(), r(27, 28));
            // At the end: the last line, which has no terminator.
            assert_eq!(t.range_at(32, TextUnit::Line).unwrap(), r(28, 32));
            assert_eq!(t.range_at(32, TextUnit::Character).unwrap(), r(31, 32));
            assert!(t.range_at(33, TextUnit::Character).is_err());
            assert!(t.read_range(r(4, 6)).is_err(), "starts mid-pair");
            assert!(t.native_range(r(0, 13)).is_err(), "ends mid-pair");
        }
        let ends_in_pair = FakeText::new("a\u{1F600}", OffsetUnit::Utf16);
        assert_eq!(
            ends_in_pair.range_at(3, TextUnit::Character).unwrap(),
            TextRange { start: 1, end: 3 }
        );
    }

    #[test]
    fn runs_and_ranges_convert_to_utf16() {
        let gtk = FakeText::new(MIXED, OffsetUnit::CodePoints);
        // Code points 0..4 are "Hi 😀": five UTF-16 units.
        assert_eq!(gtk.run_at(0).unwrap().range, TextRange { start: 0, end: 5 });
        assert_eq!(
            gtk.native_range(TextRange { start: 5, end: 14 }).unwrap(),
            (4, 12)
        );
        let qt = FakeText::new(MIXED, OffsetUnit::Utf16);
        assert_eq!(qt.run_at(5).unwrap().range, TextRange { start: 4, end: 8 });
        assert!(qt.run_at(32).is_err());
        assert!(gtk.run_at(32).is_err());
    }

    fn long_document(lines: usize) -> String {
        (0..lines)
            .map(|i| format!("line {i:05} of the document\n"))
            .collect()
    }

    #[test]
    fn a_utf16_line_walk_reads_a_few_units_per_line() {
        let lines = 2000;
        let doc = long_document(lines);
        let qt = FakeText::new(&doc, OffsetUnit::Utf16);
        let len = utf16_len(&doc);
        let mut pos = 0;
        let mut walked = 0;
        while pos < len {
            pos = qt.range_at(pos, TextUnit::Line).unwrap().end;
            walked += 1;
        }
        assert_eq!(walked, lines);
        // Before #649 every step read the whole text: about 2000 × 54 000
        // units. Now a step reads the unit before the line's end and the two
        // after it, plus the pair checks.
        let sent = qt.source.items_sent.get();
        assert!(sent <= 8 * lines, "{sent} units for {lines} lines");
    }

    #[test]
    fn a_utf16_chunked_read_reads_each_unit_once() {
        let doc = long_document(2000);
        let qt = FakeText::new(&doc, OffsetUnit::Utf16);
        let len = utf16_len(&doc);
        let mut text = String::new();
        let mut pos = 0;
        while pos < len {
            let end = (pos + 4096).min(len);
            text.push_str(&qt.read_range(TextRange { start: pos, end }).unwrap());
            pos = end;
        }
        assert_eq!(text, doc);
        // Each chunk, plus four units for the pair checks at its ends.
        let sent = qt.source.items_sent.get();
        assert!(sent <= len as usize + 4 * 14, "{sent} units for {len}");
    }

    #[test]
    fn a_code_point_read_near_the_start_reads_only_the_start() {
        let doc = long_document(2000);
        let gtk = FakeText::new(&doc, OffsetUnit::CodePoints);
        let first = gtk.range_at(10, TextUnit::Line).unwrap();
        assert_eq!(first, TextRange { start: 0, end: 27 });
        gtk.run_at(5).unwrap();
        gtk.native_range(TextRange { start: 0, end: 20 }).unwrap();
        gtk.read_range(TextRange { start: 0, end: 20 }).unwrap();
        let sent = gtk.source.items_sent.get();
        assert!(sent < 200, "{sent} units of {}", doc.len());
    }

    #[test]
    fn last_scalar_start() {
        assert_eq!(utf16_last_scalar_start("ab"), 1);
        assert_eq!(utf16_last_scalar_start("a😀"), 1);
    }
}
