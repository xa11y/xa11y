//! Small plain-data types exposed to JS: `Rect`, `TreeNode`, `EventKind`, etc.

/// A node in a recursive snapshot of the accessibility subtree.
///
/// Returned by `Element.tree()` and `Locator.tree()`. Each node carries the
/// role, display name, and value of one element, plus its children recursively.
/// `children` is empty when `maxDepth` was reached or the element is a leaf.
#[napi(object)]
#[derive(Clone)]
pub struct TreeNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub children: Vec<TreeNode>,
}

impl From<xa11y::TreeNode> for TreeNode {
    fn from(n: xa11y::TreeNode) -> Self {
        Self {
            role: n.role,
            name: n.name,
            value: n.value,
            children: n.children.into_iter().map(Into::into).collect(),
        }
    }
}

/// A bounding rectangle in desktop coordinates (physical pixels on Windows)
/// (device-independent points) on every platform. Origin is the top-left of
/// the primary display; negative `x` / `y` are valid on multi-monitor setups.
///
/// This is the same space accepted by `screenshotRegion` and the input layer,
/// so bounds can be passed straight through. To map onto captured pixels, use
/// `Screenshot.desktopRectToImage`; a single scale is insufficient on a
/// mixed-DPI desktop or after resizing.
#[napi(object)]
#[derive(Clone)]
pub struct Rect {
    /// Left edge, in screen coordinates.
    pub x: f64,
    /// Top edge, in screen coordinates.
    pub y: f64,
    /// Width in screen-coordinate units.
    pub width: f64,
    /// Height in screen-coordinate units.
    pub height: f64,
}

// The fields are `f64` rather than `i32` / `u32` because a `Rect` is also an
// argument (`screenshot({ region })`, `Screenshot.crop`, ...), and napi's
// integer conversions coerce silently: `NaN` becomes 0, `1.5` becomes 1 and
// a negative width wraps. `to_core` checks the raw numbers instead.
impl From<xa11y::Rect> for Rect {
    fn from(r: xa11y::Rect) -> Self {
        Self {
            x: f64::from(r.x),
            y: f64::from(r.y),
            width: f64::from(r.width),
            height: f64::from(r.height),
        }
    }
}

impl Rect {
    /// The core rect, refusing a non-finite, fractional or out-of-range
    /// coordinate and a negative size with `InvalidActionDataError`.
    /// `what` names the argument in the message (`"region"`, `"rect"`).
    pub fn to_core(&self, what: &str) -> napi::Result<xa11y::Rect> {
        Ok(xa11y::Rect {
            x: crate::checked_window_coord(self.x, &format!("{what}.x"))?,
            y: crate::checked_window_coord(self.y, &format!("{what}.y"))?,
            width: checked_offset(self.width, &format!("{what}.width"))?,
            height: checked_offset(self.height, &format!("{what}.height"))?,
        })
    }
}

/// Convert an `xa11y::EventKind` into a camelCase string used across the JS API.
pub fn event_kind_to_str(kind: &xa11y::EventKind) -> &'static str {
    match kind {
        xa11y::EventKind::FocusChanged => "focusChanged",
        xa11y::EventKind::ValueChanged => "valueChanged",
        xa11y::EventKind::NameChanged => "nameChanged",
        xa11y::EventKind::StateChanged { .. } => "stateChanged",
        xa11y::EventKind::StructureChanged => "structureChanged",
        xa11y::EventKind::WindowOpened => "windowOpened",
        xa11y::EventKind::WindowClosed => "windowClosed",
        xa11y::EventKind::WindowActivated => "windowActivated",
        xa11y::EventKind::WindowDeactivated => "windowDeactivated",
        xa11y::EventKind::SelectionChanged => "selectionChanged",
        xa11y::EventKind::MenuOpened => "menuOpened",
        xa11y::EventKind::MenuClosed => "menuClosed",
        xa11y::EventKind::TextChanged => "textChanged",
        xa11y::EventKind::TextSelectionChanged => "textSelectionChanged",
        xa11y::EventKind::Announcement => "announcement",
        // `EventKind` is `#[non_exhaustive]`; coverage is enforced by
        // `cargo xtask check-bindings-parity`, not by the compiler.
        _ => "unknown",
    }
}

/// Convert an `xa11y::StateFlag` to a camelCase string.
pub fn state_flag_to_str(flag: xa11y::StateFlag) -> &'static str {
    match flag {
        xa11y::StateFlag::Enabled => "enabled",
        xa11y::StateFlag::Visible => "visible",
        xa11y::StateFlag::Focused => "focused",
        xa11y::StateFlag::Checked => "checked",
        xa11y::StateFlag::Selected => "selected",
        xa11y::StateFlag::Expanded => "expanded",
        xa11y::StateFlag::Editable => "editable",
        xa11y::StateFlag::Focusable => "focusable",
        xa11y::StateFlag::Modal => "modal",
        xa11y::StateFlag::Required => "required",
        xa11y::StateFlag::Busy => "busy",
        xa11y::StateFlag::Minimized => "minimized",
        xa11y::StateFlag::Maximized => "maximized",
        xa11y::StateFlag::Fullscreen => "fullscreen",
        // `StateFlag` is `#[non_exhaustive]`; coverage is enforced by
        // `cargo xtask check-bindings-parity`, not by the compiler.
        _ => "unknown",
    }
}

/// Convert an `xa11y::Toggled` into a lower-case string (`"on"`/`"off"`/`"mixed"`).
pub fn toggled_to_str(t: xa11y::Toggled) -> &'static str {
    match t {
        xa11y::Toggled::Off => "off",
        xa11y::Toggled::On => "on",
        xa11y::Toggled::Mixed => "mixed",
    }
}

// ── Table, cell, row and collection metadata ───────────────────────────────

/// A total the platform reported: a number, or `'unknown'` when the platform
/// explicitly says it does not know (virtualized content). Spelled as in
/// Python.
pub type Count = napi::Either<u32, String>;

fn count_to_js(c: xa11y::Count) -> Count {
    match c {
        xa11y::Count::Known(n) => napi::Either::A(n),
        xa11y::Count::Unknown => napi::Either::B("unknown".to_string()),
    }
}

/// Size of a table, grid or tree grid, on the container element
/// (`Element.table`). A count is `null` when the platform did not report it.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct TableInfo {
    /// Number of rows, `'unknown'`, or `null` when not reported.
    pub row_count: Option<Count>,
    /// Number of columns, `'unknown'`, or `null` when not reported.
    pub column_count: Option<Count>,
}

impl From<xa11y::TableInfo> for TableInfo {
    fn from(t: xa11y::TableInfo) -> Self {
        Self {
            row_count: t.row_count.map(count_to_js),
            column_count: t.column_count.map(count_to_js),
        }
    }
}

/// Position of a table cell, including header cells (`Element.cell`). Every
/// index is 0-based; a field is `null` when the platform did not report it
/// (a missing span is never defaulted to `1`).
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct CellInfo {
    /// 0-based row index.
    pub row: Option<u32>,
    /// 0-based column index.
    pub column: Option<u32>,
    /// Rows this cell covers, at least 1.
    pub row_span: Option<u32>,
    /// Columns this cell covers, at least 1.
    pub column_span: Option<u32>,
    /// `'data'`, `'column_header'` or `'row_header'`; `null` when the
    /// platform does not say. Unlike `header`, tells a data cell from a cell
    /// whose kind is unknown.
    pub kind: Option<String>,
    /// `'column'` or `'row'` for a header cell; `null` for a data cell or
    /// when the platform does not say (read `kind` to tell them apart).
    pub header: Option<String>,
}

impl From<xa11y::CellInfo> for CellInfo {
    fn from(c: xa11y::CellInfo) -> Self {
        Self {
            row: c.row,
            column: c.column,
            row_span: c.row_span,
            column_span: c.column_span,
            kind: c.kind.map(cell_kind_to_str).map(str::to_string),
            header: c.header().map(header_kind_to_str).map(str::to_string),
        }
    }
}

/// The `snake_case` spelling of a cell kind. Spelled out arm by arm (not via
/// `to_snake_case`) so `[[types.variant_coverage]]` sees every variant here.
/// `CellKind` is `#[non_exhaustive]`; that check, not the compiler, is what
/// catches a new variant reaching the `_` arm.
pub fn cell_kind_to_str(kind: xa11y::CellKind) -> &'static str {
    match kind {
        xa11y::CellKind::Data => "data",
        xa11y::CellKind::ColumnHeader => "column_header",
        xa11y::CellKind::RowHeader => "row_header",
        other => other.to_snake_case(),
    }
}

/// The `snake_case` spelling of a header kind. Spelled out arm by arm (not via
/// `to_snake_case`) so `[[types.variant_coverage]]` sees every variant here.
pub fn header_kind_to_str(kind: xa11y::HeaderKind) -> &'static str {
    match kind {
        xa11y::HeaderKind::Column => "column",
        xa11y::HeaderKind::Row => "row",
    }
}

/// Position of a row in a table or tree grid (`Element.tableRow`).
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct RowInfo {
    /// 0-based row index in the table; header rows count. `null` when not
    /// reported.
    pub index: Option<u32>,
}

impl From<xa11y::RowInfo> for RowInfo {
    fn from(r: xa11y::RowInfo) -> Self {
        Self { index: r.index }
    }
}

/// Position of an item in a list, tree, tab list or radio group
/// (`Element.collection`).
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct CollectionInfo {
    /// 0-based position in the set (to show "3 of 10", add 1).
    pub index: Option<u32>,
    /// Number of items in the set, `'unknown'`, or `null` when not reported.
    pub size: Option<Count>,
    /// 0-based nesting depth: a top-level tree item is `0`.
    pub depth: Option<u32>,
}

impl From<xa11y::CollectionInfo> for CollectionInfo {
    fn from(c: xa11y::CollectionInfo) -> Self {
        Self {
            index: c.index,
            size: c.size.map(count_to_js),
            depth: c.depth,
        }
    }
}

// ── Text ranges ────────────────────────────────────────────────────────────

/// A half-open range `[start, end)` of UTF-16 code units — the same unit JS
/// string indices use, so `value.slice(r.start, r.end)` is the range's text.
///
/// Accepted wherever a range is a parameter, and returned as a plain object.
/// Both offsets must be whole, non-negative numbers.
#[napi(object)]
#[derive(Clone, Copy)]
pub struct TextRange {
    /// 0-based offset of the first unit.
    pub start: f64,
    /// 0-based offset one past the last unit.
    pub end: f64,
}

impl From<xa11y::TextRange> for TextRange {
    fn from(r: xa11y::TextRange) -> Self {
        Self {
            start: f64::from(r.start),
            end: f64::from(r.end),
        }
    }
}

impl TextRange {
    /// Validate both offsets and build the core range. Ordering is left to
    /// core, which reports a reversed range with its own error.
    pub fn to_core(self, what: &str) -> napi::Result<xa11y::TextRange> {
        Ok(xa11y::TextRange {
            start: checked_offset(self.start, &format!("{what}.start"))?,
            end: checked_offset(self.end, &format!("{what}.end"))?,
        })
    }
}

/// What a `textChanged` event inserted or deleted (`Event.textChange`).
///
/// Each field is what the platform supplied: `range` is `null` when it counts
/// offsets in a unit other than UTF-16 (on Linux, every toolkit but Qt), and
/// `text` is `null` when it does not send the text.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct TextChange {
    /// `'inserted'` or `'deleted'`. Spelled as in Python.
    pub kind: String,
    /// Where, in UTF-16 code units: for an insertion the range the new text
    /// now occupies, for a deletion the range it occupied. `null` when not
    /// reported in UTF-16.
    pub range: Option<TextRange>,
    /// The inserted or deleted text, or `null` when not supplied.
    pub text: Option<String>,
}

impl From<xa11y::TextChange> for TextChange {
    fn from(c: xa11y::TextChange) -> Self {
        Self {
            kind: text_change_kind_to_str(c.kind).to_string(),
            range: c.range.map(TextRange::from),
            text: c.text,
        }
    }
}

/// The `snake_case` spelling of a text change kind. Spelled out arm by arm
/// (not via `to_snake_case`) so `[[types.variant_coverage]]` sees every
/// variant here.
pub fn text_change_kind_to_str(kind: xa11y::TextChangeKind) -> &'static str {
    match kind {
        xa11y::TextChangeKind::Inserted => "inserted",
        xa11y::TextChangeKind::Deleted => "deleted",
    }
}

/// Validate a JS-supplied offset, index or count as a `u32`.
///
/// napi's own `u32` conversion applies `ToUint32`, so `-1` would arrive as
/// `4294967295` and `1.5` as `1`. Taking the raw number and checking it here
/// turns either into an `InvalidActionDataError` before any OS call.
pub fn checked_offset(value: f64, what: &str) -> napi::Result<u32> {
    if value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= f64::from(u32::MAX) {
        Ok(value as u32)
    } else {
        Err(crate::map_err(xa11y::Error::invalid_action_data(format!(
            "{what} must be a whole number in 0..=4294967295, got {value}"
        ))))
    }
}

/// Validate an optional `maxDepth` argument (`tree()` / `dump()`): a whole
/// number in `0..=4294967295`, so `-1` cannot become an unbounded depth.
pub fn checked_depth(value: Option<f64>) -> napi::Result<Option<usize>> {
    value
        .map(|d| checked_count(d, "maxDepth").map_err(crate::map_err))
        .transpose()
}

/// Validate a JS-supplied count or size option (`maxNodes`, `buffer`, ...)
/// as a whole number in `0..=4294967295`.
///
/// Same hazard as [`checked_offset`]: napi's `u32` conversion applies
/// `ToUint32`, so `-1` would arrive as `4294967295` (an unbounded limit) and
/// `NaN` as `0`. Returned as a core error so callers can either throw it now
/// or carry it into a task that rejects before its first OS call.
pub fn checked_count(value: f64, what: &str) -> xa11y::Result<usize> {
    if value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= f64::from(u32::MAX) {
        Ok(value as usize)
    } else {
        Err(xa11y::Error::invalid_action_data(format!(
            "{what} must be a whole number in 0..=4294967295, got {value}"
        )))
    }
}

/// The largest whole number of milliseconds a duration option accepts:
/// `Number.MAX_SAFE_INTEGER`, past which a JS number no longer holds every
/// integer.
const MAX_SAFE_MS: f64 = 9_007_199_254_740_991.0;

/// Validate a JS-supplied duration in milliseconds (`timeout`): a finite,
/// non-negative whole number. `-1` and `NaN` are refused rather than wrapped
/// by `ToUint32` into ~50 days or no wait at all.
pub fn checked_millis(value: f64, what: &str) -> xa11y::Result<std::time::Duration> {
    if value.is_finite() && value.fract() == 0.0 && (0.0..=MAX_SAFE_MS).contains(&value) {
        Ok(std::time::Duration::from_millis(value as u64))
    } else {
        Err(xa11y::Error::invalid_action_data(format!(
            "{what} must be a whole number of milliseconds in 0..=9007199254740991, got {value}"
        )))
    }
}

/// Options for the `Locator.wait*` methods and `setDefaultTimeout`.
#[napi(object)]
pub struct TimeoutOptions {
    /// Timeout in milliseconds: a finite, non-negative whole number. `0`
    /// means a single attempt with no polling. Omitted, the wait uses the
    /// process-wide default (see `setDefaultTimeout`).
    pub timeout: Option<f64>,
}

/// The process-wide default timeout, as `getDefaultTimeout()` reports it.
/// The same shape `setDefaultTimeout` takes, so the two round-trip.
#[napi(object)]
pub struct DefaultTimeout {
    /// The default timeout in whole milliseconds.
    pub timeout: f64,
}

/// Parse a JS `{ timeout }` options argument into an optional duration.
///
/// A bare number is refused rather than read as milliseconds: these methods
/// used to take seconds, and a caller still passing `5` must fail loudly
/// instead of waiting 5 ms (or, read the other way, 1000× longer than they
/// meant). Returned as a core error so a wait can carry it into its task and
/// reject before its first OS call.
pub fn parse_timeout_options(
    options: Option<napi::Either<f64, TimeoutOptions>>,
    method: &str,
) -> xa11y::Result<Option<std::time::Duration>> {
    match options {
        None => Ok(None),
        Some(napi::Either::A(n)) => Err(xa11y::Error::invalid_action_data(format!(
            "{method}: pass {{ timeout: ms }} — a bare number ({n}) is not accepted; \
             timeouts are milliseconds in an options object"
        ))),
        Some(napi::Either::B(TimeoutOptions { timeout: None })) => Ok(None),
        Some(napi::Either::B(TimeoutOptions { timeout: Some(ms) })) => {
            checked_millis(ms, "timeout").map(Some)
        }
    }
}

/// Parse `selectText`'s polymorphic arguments: `(start, end)` or one range
/// object. Shared by `Element.selectText` and `Locator.selectText`.
pub fn parse_select_range(
    start_or_range: napi::Either<f64, TextRange>,
    end: Option<f64>,
) -> napi::Result<(u32, u32)> {
    match (start_or_range, end) {
        (napi::Either::A(start), Some(end)) => {
            Ok((checked_offset(start, "start")?, checked_offset(end, "end")?))
        }
        (napi::Either::B(range), None) => {
            let r = range.to_core("range")?;
            Ok((r.start, r.end))
        }
        (napi::Either::A(_), None) => Err(crate::map_err(xa11y::Error::invalid_action_data(
            "selectText(start, end) needs an end offset, or pass one {start, end} range",
        ))),
        (napi::Either::B(_), Some(_)) => Err(crate::map_err(xa11y::Error::invalid_action_data(
            "selectText takes either (start, end) or a single {start, end} range, not both",
        ))),
    }
}

/// Formatting of a run of text. Every field is `null` when the platform did
/// not report it; a missing value is never defaulted.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct TextAttributes {
    /// Font family name.
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size: Option<f64>,
    /// Font weight on the CSS scale: 400 is normal, 700 is bold.
    pub font_weight: Option<u32>,
    /// Italic or oblique.
    pub italic: Option<bool>,
    /// Underlined (any style).
    pub underline: Option<bool>,
    /// Struck through.
    pub strikethrough: Option<bool>,
    /// Text colour, `'#rrggbb'` (opaque) or `'#rrggbbaa'`.
    pub foreground: Option<String>,
    /// Background colour, `'#rrggbb'` (opaque) or `'#rrggbbaa'`.
    pub background: Option<String>,
    /// `'baseline'`, `'superscript'` or `'subscript'`.
    pub script: Option<String>,
    /// Marked as a spelling error.
    pub spelling_error: Option<bool>,
}

/// The `snake_case` spelling of a text script position. Arm by arm so
/// `[[types.variant_coverage]]` can see every variant.
pub fn text_script_to_str(script: xa11y::TextScript) -> &'static str {
    match script {
        xa11y::TextScript::Baseline => "baseline",
        xa11y::TextScript::Superscript => "superscript",
        xa11y::TextScript::Subscript => "subscript",
    }
}

impl From<xa11y::TextAttributes> for TextAttributes {
    fn from(a: xa11y::TextAttributes) -> Self {
        Self {
            font_family: a.font_family,
            font_size: a.font_size,
            font_weight: a.font_weight.map(u32::from),
            italic: a.italic,
            underline: a.underline,
            strikethrough: a.strikethrough,
            foreground: a.foreground.map(|c| c.to_hex()),
            background: a.background.map(|c| c.to_hex()),
            script: a.script.map(text_script_to_str).map(str::to_string),
            spelling_error: a.spelling_error,
        }
    }
}

/// A run of text sharing one set of attributes.
#[napi(object)]
#[derive(Clone)]
pub struct TextRun {
    /// The text the run covers.
    pub range: TextRange,
    /// Its formatting.
    pub attributes: TextAttributes,
}

impl From<xa11y::TextRun> for TextRun {
    fn from(r: xa11y::TextRun) -> Self {
        Self {
            range: r.range.into(),
            attributes: r.attributes.into(),
        }
    }
}

/// One page of rendered lines, from `Text.lines`.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct TextLines {
    /// The lines, each clipped to the requested range.
    pub lines: Vec<TextRange>,
    /// Whether the limit stopped the walk before the end of the range.
    pub truncated: bool,
    /// Where to start the next page; non-null exactly when `truncated`.
    pub next_start: Option<u32>,
}

impl From<xa11y::TextLines> for TextLines {
    fn from(l: xa11y::TextLines) -> Self {
        Self {
            lines: l.lines.into_iter().map(Into::into).collect(),
            truncated: l.truncated,
            next_start: l.next_start,
        }
    }
}

/// One page of formatting runs, from `Text.runs`.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct TextRuns {
    /// The runs, each clipped to the requested range.
    pub runs: Vec<TextRun>,
    /// Whether the limit stopped the walk before the end of the range.
    pub truncated: bool,
    /// Where to start the next page; non-null exactly when `truncated`.
    pub next_start: Option<u32>,
}

impl From<xa11y::TextRuns> for TextRuns {
    fn from(r: xa11y::TextRuns) -> Self {
        Self {
            runs: r.runs.into_iter().map(Into::into).collect(),
            truncated: r.truncated,
            next_start: r.next_start,
        }
    }
}

/// Options for `Text.find`. Each field may be omitted, `undefined` or `null`.
#[napi(object)]
pub struct TextFindOptions {
    /// Which match to return, 0-based: `0` (the default) is the first.
    pub occurrence: Option<napi::Either<f64, napi::bindgen_prelude::Null>>,
    /// Search only inside this range. Defaults to the whole text.
    pub within: Option<napi::Either<TextRange, napi::bindgen_prelude::Null>>,
}

/// An optional object field that may also be `null`: napi's own `Option`
/// field conversion accepts only `undefined` for an absent value.
pub fn nullable<T>(v: Option<napi::Either<T, napi::bindgen_prelude::Null>>) -> Option<T> {
    match v {
        Some(napi::Either::A(t)) => Some(t),
        Some(napi::Either::B(_)) | None => None,
    }
}

/// Live-region settings of an element (`Element.liveRegion`). Each field is
/// `null` when not reported.
#[napi(object, use_nullable = true)]
#[derive(Clone)]
pub struct LiveRegionInfo {
    /// `'off'`, `'polite'` or `'assertive'`.
    pub politeness: Option<String>,
    /// Whether the whole region is announced when part of it changes.
    pub atomic: Option<bool>,
    /// Which changes are announced, as the platform spells it.
    pub relevant: Option<String>,
}

impl From<xa11y::LiveRegionInfo> for LiveRegionInfo {
    fn from(l: xa11y::LiveRegionInfo) -> Self {
        Self {
            politeness: l.politeness.map(|p| politeness_name(p).to_string()),
            atomic: l.atomic,
            relevant: l.relevant,
        }
    }
}

/// The JS spelling of a politeness value, the same as Python's.
pub(crate) fn politeness_name(p: xa11y::Politeness) -> &'static str {
    match p {
        xa11y::Politeness::Off => "off",
        xa11y::Politeness::Polite => "polite",
        xa11y::Politeness::Assertive => "assertive",
    }
}

/// The JS spelling of a landmark kind (its ARIA role name), the same as
/// Python's.
pub(crate) fn landmark_name(l: xa11y::Landmark) -> &'static str {
    match l {
        xa11y::Landmark::Banner => "banner",
        xa11y::Landmark::Complementary => "complementary",
        xa11y::Landmark::ContentInfo => "contentinfo",
        xa11y::Landmark::Form => "form",
        xa11y::Landmark::Main => "main",
        xa11y::Landmark::Navigation => "navigation",
        xa11y::Landmark::Region => "region",
        xa11y::Landmark::Search => "search",
        xa11y::Landmark::Unknown => "unknown",
        // `Landmark` is `#[non_exhaustive]`; variant coverage in the parity
        // check fails the build before a new one reaches this arm.
        other => other.to_snake_case(),
    }
}
