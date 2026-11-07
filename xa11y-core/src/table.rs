//! Table, grid and collection metadata carried on [`ElementData`].
//!
//! A role alone (`table_cell`, `list_item`) says what an element is, not where
//! it sits. These types carry the position the platform reports: a cell's row
//! and column, a table's size, an item's place in its list and its nesting
//! depth in a tree.
//!
//! # Index bases
//!
//! Every index here is **0-based**, like every index in xa11y except `nth`
//! (see the "Index bases" section of `AGENTS.md`). Platforms that report a
//! 1-based value (UIA `PositionInSet` / `Level`, the AT-SPI `posinset` /
//! `level` object attributes, ARIA) are converted by the backend.
//!
//! # Absent, unknown, not applicable
//!
//! - The whole struct is `None` on [`ElementData`] when the element has no
//!   such semantics, or the platform does not expose them.
//! - A field is `None` when the platform did not report it. A backend never
//!   fills one in from elsewhere: a missing span is not defaulted to `1`, and
//!   a missing column is not derived from the cell's position among its
//!   siblings.
//! - A total is [`Count::Unknown`] when the platform explicitly says it does
//!   not know it, which is how virtualized or partially loaded content
//!   reports its size (UIA and AT-SPI `-1`, `aria-rowcount="-1"`).
//!
//! # Realized index versus total
//!
//! [`CellInfo::row`] and [`RowInfo::index`] are the platform's grid index of
//! the element as it is laid out now. In virtualized content they can shift
//! as the view scrolls, while [`TableInfo::row_count`] may be the logical total
//! of a data set that is only partly realized. Do not compare the two in
//! virtualized content. `children()` returns what is realized now, which can be
//! fewer elements than `row_count`.
//!
//! [`ElementData`]: crate::element::ElementData

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::element::reader_writer_pair;

/// A total the platform reported: a number, or an explicit "unknown".
///
/// `None` on the field that holds a `Count` means the platform reported
/// nothing; `Unknown` means it reported that the total is not known.
///
/// Serializes as the number, or as the string `"unknown"`. The bindings use
/// the same spelling: an `int` or `"unknown"` in Python, a `number` or
/// `'unknown'` in JS.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a reported total is either a number or an \
              explicit statement that it is unknown. No platform reports an \
              estimate, and an estimate would be a third, separately typed \
              concept rather than a variant of this one."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Count {
    /// The platform reported this total.
    Known(u32),
    /// The platform reported that the total is unknown.
    Unknown,
}

impl Count {
    /// The number, when known.
    pub fn known(self) -> Option<u32> {
        match self {
            Self::Known(n) => Some(n),
            Self::Unknown => None,
        }
    }

    /// Map a platform total where a negative value means "unknown" (UIA,
    /// AT-SPI and ARIA all use `-1`).
    pub fn from_signed(n: i64) -> Self {
        match u32::try_from(n) {
            Ok(n) => Self::Known(n),
            Err(_) => Self::Unknown,
        }
    }
}

impl std::fmt::Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Known(n) => write!(f, "{n}"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

impl Serialize for Count {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Known(n) => s.serialize_u32(*n),
            Self::Unknown => s.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Known(u32),
            Text(String),
        }
        match Wire::deserialize(d)? {
            Wire::Known(n) => Ok(Self::Known(n)),
            Wire::Text(s) if s == "unknown" => Ok(Self::Unknown),
            Wire::Text(s) => Err(serde::de::Error::custom(format!(
                "expected a count or \"unknown\", got {s:?}"
            ))),
        }
    }
}

/// Which kind of header a header cell is.
///
/// Exhaustive: `Provider::table_headers` and `Provider::cell_headers` take a
/// kind, and each backend turns every variant into a platform call, so a new
/// kind must break their build. The bindings spell each variant as a
/// `snake_case` string, guarded by `[[types.variant_coverage]]`.
#[allow(
    clippy::exhaustive_enums,
    reason = "Capability enum: backends translate each variant into a \
              platform header query, so growth must break the build."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderKind {
    /// Heads a column (UIA `HeaderItem` in a column header row, AT-SPI
    /// `COLUMN_HEADER`, AX `AXColumnHeaderUIElements` member).
    Column,
    /// Heads a row (AT-SPI `ROW_HEADER`, AX `AXRowHeaderUIElements` member).
    Row,
}

impl HeaderKind {
    /// The `snake_case` name this kind crosses the bindings, the CLI and MCP
    /// as (`"column"`, `"row"`).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::Row => "row",
        }
    }
}

impl std::fmt::Display for HeaderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

reader_writer_pair! {
    /// Size of a table, grid or tree grid, on the container element.
    ///
    /// Each count is the platform's total, which can exceed what
    /// `children()` returns in virtualized content (see the
    /// [module docs](self)).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TableInfo;

    /// Every field a provider must decide on for a table. Exhaustive for the
    /// same reason as `ElementParts`. Not public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for TableInfo; see ElementParts."
    )]
    #[derive(Debug, Clone, Copy)]
    pub struct TableParts;

    fields {
        /// Number of rows. `None` when not reported.
        pub row_count: Option<Count>,
        /// Number of columns. `None` when not reported.
        pub column_count: Option<Count>,
    }
}

reader_writer_pair! {
    /// Position of a table cell, including header cells.
    ///
    /// `row` and `column` are 0-based grid indices. A merged cell reports the
    /// top-left slot it covers plus its spans.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CellInfo;

    /// Every field a provider must decide on for a cell. Not public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for CellInfo; see ElementParts."
    )]
    #[derive(Debug, Clone, Copy)]
    pub struct CellParts;

    fields {
        /// 0-based row index. `None` when not reported.
        pub row: Option<u32>,
        /// 0-based column index. `None` when not reported.
        pub column: Option<u32>,
        /// Rows this cell covers, at least 1. `None` when not reported; never
        /// defaulted to 1.
        pub row_span: Option<u32>,
        /// Columns this cell covers, at least 1. `None` when not reported.
        pub column_span: Option<u32>,
        /// `Some` for a header cell. `None` for a data cell, or when the
        /// platform does not say.
        pub header: Option<HeaderKind>,
    }
}

reader_writer_pair! {
    /// Position of a row in a table or tree grid.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RowInfo;

    /// Every field a provider must decide on for a row. Not public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for RowInfo; see ElementParts."
    )]
    #[derive(Debug, Clone, Copy)]
    pub struct RowParts;

    fields {
        /// 0-based row index in the table. Header rows count. `None` when not
        /// reported.
        pub index: Option<u32>,
    }
}

reader_writer_pair! {
    /// Position of an item in a list, tree, tab list or radio group.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CollectionInfo;

    /// Every field a provider must decide on for a collection item. Not
    /// public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for CollectionInfo; see ElementParts."
    )]
    #[derive(Debug, Clone, Copy)]
    pub struct CollectionParts;

    fields {
        /// 0-based position in the set: the first item is `0`. `None` when not
        /// reported. To show "3 of 10", add 1.
        pub index: Option<u32>,
        /// Number of items in the set. `None` when not reported.
        pub size: Option<Count>,
        /// 0-based nesting depth: a top-level tree item is `0`. `None` when not
        /// reported. Unrelated to heading level, which is a label (1 = h1).
        pub depth: Option<u32>,
    }
}

impl TableInfo {
    /// Whether every field is `None`. Backends report `None` for the whole
    /// struct instead of an empty one.
    pub fn is_empty(&self) -> bool {
        self.row_count.is_none() && self.column_count.is_none()
    }
}

impl CellInfo {
    /// Whether every field is `None`.
    pub fn is_empty(&self) -> bool {
        self.row.is_none()
            && self.column.is_none()
            && self.row_span.is_none()
            && self.column_span.is_none()
            && self.header.is_none()
    }
}

impl RowInfo {
    /// Whether every field is `None`.
    pub fn is_empty(&self) -> bool {
        self.index.is_none()
    }
}

impl CollectionInfo {
    /// Whether every field is `None`.
    pub fn is_empty(&self) -> bool {
        self.index.is_none() && self.size.is_none() && self.depth.is_none()
    }
}

/// Convert a platform 1-based ordinal to a 0-based index.
///
/// UIA reports an unset `PositionInSet` / `Level` as `0`, and AT-SPI reports
/// an unparsable or missing attribute as no value at all, so `0` and anything
/// negative mean "not reported" here, not "index -1".
pub fn index_from_one_based(n: i64) -> Option<u32> {
    if n >= 1 {
        u32::try_from(n - 1).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_serializes_as_number_or_unknown() {
        assert_eq!(serde_json::to_string(&Count::Known(7)).unwrap(), "7");
        assert_eq!(
            serde_json::to_string(&Count::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::from_str::<Count>("12").unwrap(),
            Count::Known(12)
        );
        assert_eq!(
            serde_json::from_str::<Count>("\"unknown\"").unwrap(),
            Count::Unknown
        );
        assert!(serde_json::from_str::<Count>("\"lots\"").is_err());
    }

    #[test]
    fn negative_totals_are_unknown() {
        assert_eq!(Count::from_signed(-1), Count::Unknown);
        assert_eq!(Count::from_signed(0), Count::Known(0));
        assert_eq!(Count::from_signed(40), Count::Known(40));
    }

    #[test]
    fn one_based_ordinals_convert_and_zero_is_unset() {
        assert_eq!(index_from_one_based(1), Some(0));
        assert_eq!(index_from_one_based(3), Some(2));
        assert_eq!(index_from_one_based(0), None);
        assert_eq!(index_from_one_based(-1), None);
    }

    #[test]
    fn parts_convert_to_readers() {
        let cell: CellInfo = CellParts {
            row: Some(2),
            column: Some(1),
            row_span: None,
            column_span: Some(2),
            header: Some(HeaderKind::Column),
        }
        .into();
        assert_eq!(cell.row, Some(2));
        assert_eq!(cell.row_span, None);
        assert!(!cell.is_empty());
    }
}
