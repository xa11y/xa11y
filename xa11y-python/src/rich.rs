//! Python surface for table / collection metadata and text by range.
//!
//! Offsets are UTF-16 code units, as on every other surface. Values cross as
//! the core's spellings: counts as an `int` or `"unknown"`, header kinds and
//! text units as `snake_case` strings, colours as `"#rrggbb"` / `"#rrggbbaa"`.

use std::sync::Arc;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;

use crate::{make_py_element, to_py_err, Element};

/// A count as Python sees it: an `int`, or the string `"unknown"`.
pub(crate) fn count_to_py(py: Python<'_>, count: Option<xa11y::Count>) -> PyResult<Py<PyAny>> {
    Ok(match count {
        None => py.None(),
        Some(xa11y::Count::Known(n)) => n.into_pyobject(py)?.into_any().unbind(),
        Some(xa11y::Count::Unknown) => "unknown".into_pyobject(py)?.into_any().unbind(),
    })
}

/// Size of a table, grid or tree grid. Counts are an ``int``, ``"unknown"``
/// (the platform said it does not know), or ``None`` (not reported).
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct TableInfo {
    inner: xa11y::TableInfo,
}

#[pymethods]
impl TableInfo {
    /// Number of rows: ``int``, ``"unknown"``, or ``None``.
    #[getter]
    fn row_count(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        count_to_py(py, self.inner.row_count)
    }
    /// Number of columns: ``int``, ``"unknown"``, or ``None``.
    #[getter]
    fn column_count(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        count_to_py(py, self.inner.column_count)
    }
    fn __repr__(&self) -> String {
        format!(
            "TableInfo(row_count={}, column_count={})",
            opt_count(self.inner.row_count),
            opt_count(self.inner.column_count)
        )
    }
}

/// Row, column and spans of a table cell. Every index is 0-based; a field
/// the platform did not report is ``None``.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct CellInfo {
    inner: xa11y::CellInfo,
}

#[pymethods]
impl CellInfo {
    /// 0-based row index.
    #[getter]
    fn row(&self) -> Option<u32> {
        self.inner.row
    }
    /// 0-based column index.
    #[getter]
    fn column(&self) -> Option<u32> {
        self.inner.column
    }
    /// Rows the cell covers (at least 1), or ``None`` when not reported.
    #[getter]
    fn row_span(&self) -> Option<u32> {
        self.inner.row_span
    }
    /// Columns the cell covers (at least 1), or ``None`` when not reported.
    #[getter]
    fn column_span(&self) -> Option<u32> {
        self.inner.column_span
    }
    /// ``"column"`` or ``"row"`` for a header cell, else ``None``.
    #[getter]
    fn header(&self) -> Option<&'static str> {
        self.inner.header.map(header_name)
    }
    fn __repr__(&self) -> String {
        format!(
            "CellInfo(row={}, column={}, row_span={}, column_span={}, header={})",
            opt(self.inner.row),
            opt(self.inner.column),
            opt(self.inner.row_span),
            opt(self.inner.column_span),
            self.inner
                .header
                .map(|h| format!("'{}'", header_name(h)))
                .unwrap_or_else(|| "None".into())
        )
    }
}

/// Row index of a table or tree-grid row (0-based).
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct RowInfo {
    inner: xa11y::RowInfo,
}

#[pymethods]
impl RowInfo {
    /// 0-based row index in the table, or ``None`` when not reported.
    #[getter]
    fn index(&self) -> Option<u32> {
        self.inner.index
    }
    fn __repr__(&self) -> String {
        format!("RowInfo(index={})", opt(self.inner.index))
    }
}

/// Position of an item in a list, tree, tab list or radio group.
///
/// ``index`` and ``depth`` are 0-based: the first item is ``0``, a top-level
/// tree item has depth ``0``. To show "3 of 10", add 1 to ``index``.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct CollectionInfo {
    inner: xa11y::CollectionInfo,
}

#[pymethods]
impl CollectionInfo {
    /// 0-based position in the set.
    #[getter]
    fn index(&self) -> Option<u32> {
        self.inner.index
    }
    /// Number of items in the set: ``int``, ``"unknown"``, or ``None``.
    #[getter]
    fn size(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        count_to_py(py, self.inner.size)
    }
    /// 0-based nesting depth.
    #[getter]
    fn depth(&self) -> Option<u32> {
        self.inner.depth
    }
    fn __repr__(&self) -> String {
        format!(
            "CollectionInfo(index={}, size={}, depth={})",
            opt(self.inner.index),
            opt_count(self.inner.size),
            opt(self.inner.depth)
        )
    }
}

fn header_name(h: xa11y::HeaderKind) -> &'static str {
    h.to_snake_case()
}

fn opt(v: Option<u32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "None".into())
}

fn opt_count(v: Option<xa11y::Count>) -> String {
    match v {
        None => "None".into(),
        Some(xa11y::Count::Known(n)) => n.to_string(),
        Some(xa11y::Count::Unknown) => "'unknown'".into(),
    }
}

pub(crate) fn table_info(d: &xa11y::ElementData) -> Option<TableInfo> {
    d.table.map(|inner| TableInfo { inner })
}
pub(crate) fn cell_info(d: &xa11y::ElementData) -> Option<CellInfo> {
    d.cell.map(|inner| CellInfo { inner })
}
pub(crate) fn row_info(d: &xa11y::ElementData) -> Option<RowInfo> {
    d.table_row.map(|inner| RowInfo { inner })
}
pub(crate) fn collection_info(d: &xa11y::ElementData) -> Option<CollectionInfo> {
    d.collection.map(|inner| CollectionInfo { inner })
}

// ── Text ────────────────────────────────────────────────────────────────

/// A half-open range ``[start, end)`` of UTF-16 code units.
///
/// Anywhere a range is accepted, a ``(start, end)`` tuple works too.
#[pyclass(frozen, eq, hash, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TextRange {
    /// 0-based offset of the first unit.
    #[pyo3(get)]
    start: u32,
    /// 0-based offset one past the last unit.
    #[pyo3(get)]
    end: u32,
}

#[pymethods]
impl TextRange {
    /// Raises ``InvalidTextRangeError`` when ``start > end``.
    #[new]
    fn new(start: u32, end: u32) -> PyResult<Self> {
        xa11y::TextRange::new(start, end).map_err(to_py_err)?;
        Ok(Self { start, end })
    }
    /// Number of UTF-16 code units covered.
    fn len(&self) -> u32 {
        self.end - self.start
    }
    /// Whether the range covers nothing (a caret position).
    fn is_empty(&self) -> bool {
        self.start == self.end
    }
    fn __len__(&self) -> usize {
        (self.end - self.start) as usize
    }
    fn __repr__(&self) -> String {
        format!("TextRange(start={}, end={})", self.start, self.end)
    }
}

impl From<xa11y::TextRange> for TextRange {
    fn from(r: xa11y::TextRange) -> Self {
        Self {
            start: r.start,
            end: r.end,
        }
    }
}

/// Accept a `TextRange` or a `(start, end)` tuple, validating the order.
pub(crate) fn extract_range(obj: &Bound<'_, PyAny>) -> PyResult<xa11y::TextRange> {
    if let Ok(r) = obj.cast::<TextRange>() {
        let r = r.get();
        return Ok(xa11y::TextRange {
            start: r.start,
            end: r.end,
        });
    }
    match obj.extract::<(u32, u32)>() {
        Ok((start, end)) => xa11y::TextRange::new(start, end).map_err(to_py_err),
        Err(_) => Err(PyTypeError::new_err(
            "expected a TextRange or a (start, end) tuple of non-negative ints",
        )),
    }
}

/// Formatting of a run of text. Every attribute is ``None`` when the
/// platform did not report it; nothing is defaulted.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct TextAttributes {
    inner: xa11y::TextAttributes,
}

#[pymethods]
impl TextAttributes {
    /// Font family name.
    #[getter]
    fn font_family(&self) -> Option<String> {
        self.inner.font_family.clone()
    }
    /// Font size in points.
    #[getter]
    fn font_size(&self) -> Option<f64> {
        self.inner.font_size
    }
    /// Font weight on the CSS scale: 400 normal, 700 bold.
    #[getter]
    fn font_weight(&self) -> Option<u16> {
        self.inner.font_weight
    }
    /// Italic or oblique.
    #[getter]
    fn italic(&self) -> Option<bool> {
        self.inner.italic
    }
    /// Underlined.
    #[getter]
    fn underline(&self) -> Option<bool> {
        self.inner.underline
    }
    /// Struck through.
    #[getter]
    fn strikethrough(&self) -> Option<bool> {
        self.inner.strikethrough
    }
    /// Text colour as ``"#rrggbb"`` or ``"#rrggbbaa"``.
    #[getter]
    fn foreground(&self) -> Option<String> {
        self.inner.foreground.map(|c| c.to_hex())
    }
    /// Background colour as ``"#rrggbb"`` or ``"#rrggbbaa"``.
    #[getter]
    fn background(&self) -> Option<String> {
        self.inner.background.map(|c| c.to_hex())
    }
    /// ``"baseline"``, ``"superscript"`` or ``"subscript"``.
    #[getter]
    fn script(&self) -> Option<&'static str> {
        self.inner.script.map(|s| s.to_snake_case())
    }
    /// Marked as a spelling error.
    #[getter]
    fn spelling_error(&self) -> Option<bool> {
        self.inner.spelling_error
    }
    fn __repr__(&self) -> String {
        format!("TextAttributes({:?})", self.inner)
    }
}

/// A run of text sharing one set of attributes.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct TextRun {
    /// The text the run covers.
    #[pyo3(get)]
    range: TextRange,
    /// Its formatting.
    #[pyo3(get)]
    attributes: TextAttributes,
}

#[pymethods]
impl TextRun {
    fn __repr__(&self) -> String {
        format!(
            "TextRun(range={}, attributes={})",
            self.range.__repr__(),
            self.attributes.__repr__()
        )
    }
}

/// One page of rendered lines from ``Text.lines``.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct TextLines {
    /// The lines, each clipped to the requested range.
    #[pyo3(get)]
    lines: Vec<TextRange>,
    /// Whether the limit stopped the walk before the end of the range.
    #[pyo3(get)]
    truncated: bool,
    /// Where to start the next page; set exactly when ``truncated``.
    #[pyo3(get)]
    next_start: Option<u32>,
}

#[pymethods]
impl TextLines {
    fn __repr__(&self) -> String {
        format!(
            "TextLines({} lines, truncated={})",
            self.lines.len(),
            if self.truncated { "True" } else { "False" }
        )
    }
}

/// One page of formatting runs from ``Text.runs``.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct TextRuns {
    /// The runs, each clipped to the requested range.
    #[pyo3(get)]
    runs: Vec<TextRun>,
    /// Whether the limit stopped the walk before the end of the range.
    #[pyo3(get)]
    truncated: bool,
    /// Where to start the next page; set exactly when ``truncated``.
    #[pyo3(get)]
    next_start: Option<u32>,
}

#[pymethods]
impl TextRuns {
    fn __repr__(&self) -> String {
        format!(
            "TextRuns({} runs, truncated={})",
            self.runs.len(),
            if self.truncated { "True" } else { "False" }
        )
    }
}

/// An element's text, read by range. Every method is a live platform read;
/// offsets are UTF-16 code units.
///
/// From ``Element.text()`` it stays bound to that element. From
/// ``Locator.text()`` it re-resolves the locator on every read.
#[pyclass(frozen)]
pub(crate) struct Text {
    inner: xa11y::Text,
}

impl Text {
    pub(crate) fn new(inner: xa11y::Text) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl Text {
    /// Length in UTF-16 code units, without reading the text where the
    /// platform allows.
    fn length(&self, py: Python<'_>) -> PyResult<u32> {
        let t = self.inner.clone();
        py.detach(move || t.length()).map_err(to_py_err)
    }
    /// The caret offset, or ``None`` when the element has no caret.
    fn caret(&self, py: Python<'_>) -> PyResult<Option<u32>> {
        let t = self.inner.clone();
        py.detach(move || t.caret()).map_err(to_py_err)
    }
    /// The selected ranges in document order; empty when nothing is selected.
    fn selections(&self, py: Python<'_>) -> PyResult<Vec<TextRange>> {
        let t = self.inner.clone();
        let ranges = py.detach(move || t.selections()).map_err(to_py_err)?;
        Ok(ranges.into_iter().map(TextRange::from).collect())
    }
    /// The text in ``range`` (a ``TextRange`` or ``(start, end)``), exactly
    /// as the platform reports it.
    fn read(&self, py: Python<'_>, range: &Bound<'_, PyAny>) -> PyResult<String> {
        let r = extract_range(range)?;
        let t = self.inner.clone();
        py.detach(move || t.read(r)).map_err(to_py_err)
    }
    /// The whole text.
    fn read_all(&self, py: Python<'_>) -> PyResult<String> {
        let t = self.inner.clone();
        py.detach(move || t.read_all()).map_err(to_py_err)
    }
    /// The ``occurrence``-th (0-based) exact match of ``needle``, searching
    /// ``within`` if given. Case-sensitive, no Unicode normalization.
    #[pyo3(signature = (needle, *, occurrence=0, within=None))]
    fn find(
        &self,
        py: Python<'_>,
        needle: &str,
        occurrence: usize,
        within: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<TextRange>> {
        let mut options = xa11y::FindOptions::new().occurrence(occurrence);
        if let Some(w) = within {
            options = options.within(extract_range(w)?);
        }
        let t = self.inner.clone();
        let needle = needle.to_string();
        let found = py
            .detach(move || t.find_with(&needle, options))
            .map_err(to_py_err)?;
        Ok(found.map(TextRange::from))
    }
    /// The ``unit`` (``"character"``, ``"word"``, ``"line"``,
    /// ``"paragraph"``) containing ``offset``. ``"line"`` is the rendered line.
    fn range_at(&self, py: Python<'_>, offset: u32, unit: &str) -> PyResult<TextRange> {
        let unit = xa11y::TextUnit::parse(unit).map_err(to_py_err)?;
        let t = self.inner.clone();
        let r = py
            .detach(move || t.range_at(offset, unit))
            .map_err(to_py_err)?;
        Ok(r.into())
    }
    /// Rendered lines covering ``range``, at most ``limit`` per page.
    #[pyo3(signature = (range, limit=200))]
    fn lines(&self, py: Python<'_>, range: &Bound<'_, PyAny>, limit: usize) -> PyResult<TextLines> {
        let r = extract_range(range)?;
        let t = self.inner.clone();
        let page = py.detach(move || t.lines(r, limit)).map_err(to_py_err)?;
        Ok(TextLines {
            lines: page.lines.into_iter().map(TextRange::from).collect(),
            truncated: page.truncated,
            next_start: page.next_start,
        })
    }
    /// Screen rectangles of ``range``, one per rendered line fragment, in the
    /// same coordinates as ``Element.bounds``.
    fn bounds(&self, py: Python<'_>, range: &Bound<'_, PyAny>) -> PyResult<Vec<crate::Rect>> {
        let r = extract_range(range)?;
        let t = self.inner.clone();
        let rects = py.detach(move || t.bounds(r)).map_err(to_py_err)?;
        Ok(rects.into_iter().map(crate::Rect::from).collect())
    }
    /// Formatting runs covering ``range``, at most ``limit`` per page.
    #[pyo3(signature = (range, limit=200))]
    fn runs(&self, py: Python<'_>, range: &Bound<'_, PyAny>, limit: usize) -> PyResult<TextRuns> {
        let r = extract_range(range)?;
        let t = self.inner.clone();
        let page = py.detach(move || t.runs(r, limit)).map_err(to_py_err)?;
        Ok(TextRuns {
            runs: page
                .runs
                .into_iter()
                .map(|run| TextRun {
                    range: run.range.into(),
                    attributes: TextAttributes {
                        inner: run.attributes,
                    },
                })
                .collect(),
            truncated: page.truncated,
            next_start: page.next_start,
        })
    }
    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }
}

/// Wrap a list of core elements.
pub(crate) fn wrap_elements(
    py: Python<'_>,
    elements: Vec<xa11y::Element>,
    provider: &Arc<dyn xa11y::Provider>,
) -> PyResult<Vec<Py<Element>>> {
    elements
        .iter()
        .map(|e| make_py_element(py, e.data(), provider.clone()))
        .collect()
}
