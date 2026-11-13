"""Tables, collections and text by range, against the mock fixture.

The fixture is ``xa11y_core::mock::build_table_provider``: a 3x3 "Prices"
table with column and row headers and a cell spanning two columns, a
virtualized "Results" list, a "Folders" tree and a "Notes" text area holding
``"Hello 😀 world\\nsecond line"``. Offsets are UTF-16 code units, so the emoji
counts as two.
"""

import pytest
import xa11y
from xa11y import _native


def one(selector):
    return _native._make_test_table_locator(selector).element()


def names(elements):
    return [e.name for e in elements]


# ── Tables and collections ───────────────────────────────────────────────────


def test_table_info():
    info = one("table").table
    assert isinstance(info, xa11y.TableInfo)
    assert info.row_count == 3
    assert info.column_count == 3


def test_cell_info_and_unreported_spans():
    price = one('table_cell[name="Price"]').cell
    assert isinstance(price, xa11y.CellInfo)
    assert (price.row, price.column, price.row_span, price.column_span) == (0, 1, 1, 2)
    assert price.header == "column"
    assert price.kind == "column_header"
    assert one('table_cell[name="Widget"]').cell.kind == "row_header"
    # A data cell says so: kind "data", header None.
    data = one('table_cell[name="1.00"]').cell
    assert (data.kind, data.header) == ("data", None)
    assert "kind='column_header'" in repr(price)
    # A span the platform did not report is None, never defaulted to 1.
    assert one('table_cell[name="3.00"]').cell.row_span is None
    assert one('table_cell[name="1.00"]').cell.header is None


def test_row_info():
    rows = _native._make_test_table_locator("table_row").elements()
    assert [r.table_row.index for r in rows] == [0, 1, 2]
    assert isinstance(rows[0].table_row, xa11y.RowInfo)


def test_collection_info_unknown_size_and_depth():
    first = one('list_item[name="First"]').collection
    assert isinstance(first, xa11y.CollectionInfo)
    assert (first.index, first.size, first.depth) == (0, "unknown", None)
    child = one('tree_item[name="Child"]').collection
    assert (child.index, child.size, child.depth) == (0, 1, 1)


def test_non_table_elements_have_no_table_metadata():
    button = _native._make_test_locator().descendant("button").first().element()
    assert button.table is None
    assert button.cell is None
    assert button.table_row is None
    assert button.collection is None


def test_numeric_selectors():
    assert one('table_cell[row="1"][column="2"]').name == "2.00"
    assert one('list_item[size="unknown"][index="1"]').name == "Second"
    # 0-based attribute next to the 1-based :nth.
    assert one('table_cell[row="0"]:nth(2)').name == "Price"
    with pytest.raises(xa11y.InvalidSelectorError):
        one('table_cell[row="first"]')
    with pytest.raises(xa11y.InvalidSelectorError):
        one('table_cell[row*="1"]')


def test_cell_at_returns_merged_cell():
    table = one("table")
    assert table.cell_at(0, 2).name == "Price"
    assert table.cell_at(1, 2).name == "2.00"
    assert table.cell_at(9, 9) is None


def test_headers_and_selection():
    table = one("table")
    assert names(table.column_headers()) == ["Product", "Price"]
    assert names(table.row_headers()) == ["Widget", "Gadget"]
    cell = one('table_cell[name="1.00"]')
    assert names(cell.column_header_cells()) == ["Price"]
    assert names(cell.row_header_cells()) == ["Widget"]
    assert names(table.selected_items()) == ["2.00"]


def test_table_queries_on_a_non_table_raise():
    with pytest.raises(xa11y.ActionNotSupportedError):
        one('list[name="Results"]').column_headers()


# ── Text ─────────────────────────────────────────────────────────────────────


def test_text_refused_on_an_element_without_text():
    button = _native._make_test_locator().descendant("button").first().element()
    # Like ``Locator.text()``, nothing is read until a method is called.
    text = button.text()
    assert isinstance(text, xa11y.Text)
    with pytest.raises(xa11y.ActionNotSupportedError):
        text.length()


def test_lengths_and_offsets_are_utf16():
    t = one("text_area").text()
    assert isinstance(t, xa11y.Text)
    assert t.length() == 26
    world = t.find("world")
    assert world == xa11y.TextRange(9, 14)
    assert t.read(world) == "world"
    assert t.read((6, 8)) == "😀"
    assert t.read_all() == "Hello 😀 world\nsecond line"


def test_bad_ranges_raise_structured_errors():
    t = one("text_area").text()
    with pytest.raises(xa11y.InvalidTextRangeError) as info:
        t.read((0, 99))
    assert (info.value.start, info.value.end, info.value.length) == (0, 99, 26)
    # A split surrogate pair is refused, not returned as half an emoji.
    with pytest.raises(xa11y.InvalidTextRangeError):
        t.read((6, 7))
    # It is also an InvalidActionDataError, so existing handlers catch it.
    with pytest.raises(xa11y.InvalidActionDataError):
        t.read((5, 4))
    with pytest.raises(xa11y.InvalidTextRangeError):
        xa11y.TextRange(5, 4)
    with pytest.raises(TypeError):
        t.read("not a range")


def test_find_occurrence_and_within():
    t = one("text_area").text()
    assert t.find("o") == xa11y.TextRange(4, 5)
    assert t.find("o", occurrence=1) == xa11y.TextRange(10, 11)
    assert t.find("o", within=(11, 15)) is None
    assert t.find("missing") is None


def test_range_at_units():
    t = one("text_area").text()
    assert t.range_at(7, "character") == xa11y.TextRange(6, 8)
    assert t.range_at(10, "word") == xa11y.TextRange(9, 15)
    assert t.range_at(20, "line") == xa11y.TextRange(15, 26)
    with pytest.raises(xa11y.InvalidActionDataError):
        t.range_at(0, "sentence")


def test_caret_and_selection_follow_select_text():
    el = one("text_area")
    t = el.text()
    assert t.caret() is None
    assert t.selections() == []
    el.select_text(t.find("world"))
    assert t.selections() == [xa11y.TextRange(9, 14)]
    assert t.caret() == 14
    el.select_text(0, 5)
    assert t.selections() == [xa11y.TextRange(0, 5)]
    el.select_text((1, 2))
    assert t.caret() == 2


def test_lines_bounds_and_runs_page():
    t = one("text_area").text()
    lines = t.lines((0, 26))
    assert isinstance(lines, xa11y.TextLines)
    assert lines.lines == [xa11y.TextRange(0, 15), xa11y.TextRange(15, 26)]
    assert not lines.truncated and lines.next_start is None
    # A page that holds nothing would never advance a paging loop.
    with pytest.raises(xa11y.InvalidActionDataError):
        t.lines((0, 26), limit=0)
    with pytest.raises(xa11y.InvalidActionDataError):
        t.runs((0, 26), limit=0)
    page = t.lines((0, 26), limit=1)
    assert page.truncated and page.next_start == 15
    rects = t.bounds((9, 20))
    assert len(rects) == 2 and all(isinstance(r, xa11y.Rect) for r in rects)
    runs = t.runs((0, 26), limit=2)
    assert isinstance(runs, xa11y.TextRuns)
    assert [r.range for r in runs.runs] == [xa11y.TextRange(0, 6), xa11y.TextRange(6, 9)]
    first = runs.runs[0].attributes
    assert isinstance(first, xa11y.TextAttributes)
    assert (first.font_family, first.font_weight, first.italic) == ("MockSans", 700, False)
    assert first.underline is None and first.foreground is None
    assert runs.truncated and runs.next_start == 9


def test_locator_text_re_resolves():
    t = _native._make_test_table_locator("text_area").text()
    assert t.length() == 26
    missing = _native._make_test_table_locator('text_area[name="Nope"]').text()
    with pytest.raises(xa11y.TimeoutError):
        missing.length()


def test_text_range_value_semantics():
    r = xa11y.TextRange(2, 5)
    assert (r.start, r.end, r.len(), len(r), r.is_empty()) == (2, 5, 3, 3, False)
    assert r == xa11y.TextRange(2, 5)
    assert hash(r) == hash(xa11y.TextRange(2, 5))
    assert xa11y.TextRange(3, 3).is_empty()


def test_embedded_object_mapping_is_explicitly_unsupported_on_mock():
    text = _native._make_test_table_locator("text_area").text()
    with pytest.raises(xa11y.ActionNotSupportedError):
        text.object_at(0)
    with pytest.raises(xa11y.InvalidTextRangeError):
        text.object_at(text.length())
