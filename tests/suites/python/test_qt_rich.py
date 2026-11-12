"""Text by range, tables and collections through Qt's AT-SPI adaptor.

The GTK 3 fixture (``xa11y/tests/integ/rich.rs``) cannot reach two things this
suite exists for:

- **UTF-16 offsets on the wire.** Qt's adaptor counts text offsets in UTF-16
  code units where GTK counts code points, so here the provider's UTF-16 path
  runs end to end: a non-BMP character is two units in every read, find,
  range and selection, and a ``text_changed`` event carries its range.
- **Merged table cells**, which GTK 3's tree view cannot make. The fixture
  merges two, and the suite records what Qt does with them: it reports no
  merge at all (see ``test_qt_reports_no_merged_cells``).

Linux only: the widgets are in ``test-apps/qt/app.py``'s "Rich" group on every
platform, but what is asserted here is Qt's AT-SPI behaviour, measured on Qt
6.12. Qt's UIA and AX bridges are not verified to say the same.
"""

from __future__ import annotations

import sys

import pytest
import xa11y

# Mirrors UNICODE_NOTES_TEXT in test-apps/qt/app.py. UTF-16 offsets: "😀" is
# 3..5, "wörld" 6..11 (bold), "𝄞" 12..14, the newline 14, "second line"
# 15..26.
TEXT = "Hi \U0001f600 wörld \U0001d11e\nsecond line"
LENGTH = 26


@pytest.fixture(autouse=True)
def linux_qt_only(app_name):
    if not sys.platform.startswith("linux") or app_name != "qt":
        pytest.skip("pins Qt's AT-SPI adaptor; requires Qt on Linux")


def r(start: int, end: int) -> xa11y.TextRange:
    return xa11y.TextRange(start, end)


@pytest.fixture
def notes(app):
    return app.locator('text_area[name="Unicode Notes"]').element()


@pytest.fixture
def span_table(app):
    return app.locator('table[name="Span Table"]').element()


# ── Text: UTF-16 offsets ──────────────────────────────────────────────────


def test_length_and_reads_count_utf16_units(notes):
    assert len(TEXT.encode("utf-16-le")) // 2 == LENGTH, "fixture constant drifted"
    text = notes.text()
    assert text.length() == LENGTH
    assert text.read_all() == TEXT
    assert text.read((3, 5)) == "\U0001f600"
    assert text.read(r(6, 11)) == "wörld"
    assert text.read((12, 14)) == "\U0001d11e"
    assert text.read((12, 26)) == "\U0001d11e\nsecond line"


@pytest.mark.parametrize(
    "bad",
    [(4, 6), (3, 4), (13, 15), (20, LENGTH + 1)],
    ids=["starts-mid-emoji", "ends-mid-emoji", "starts-mid-clef", "past-end"],
)
def test_reads_that_split_a_pair_or_overrun_are_refused(notes, bad):
    with pytest.raises(xa11y.InvalidTextRangeError):
        notes.text().read(bad)


def test_find_reports_utf16_ranges(notes):
    text = notes.text()
    assert text.find("\U0001f600") == r(3, 5)
    assert text.find("\U0001d11e") == r(12, 14)
    assert text.find("wörld") == r(6, 11)
    assert text.find("line") == r(22, 26)
    assert text.find("l") == r(9, 10)
    assert text.find("l", occurrence=1) == r(22, 23)
    assert text.find("l", within=(11, LENGTH)) == r(22, 23)
    assert text.find("absent") is None


def test_locator_text_resolves_the_element(app):
    text = app.locator('text_area[name="Unicode Notes"]').text()
    assert text.length() == LENGTH
    assert text.find("second") == r(15, 21)


def test_range_at_characters_cover_whole_surrogate_pairs(notes):
    text = notes.text()
    # Either half of a pair answers the whole pair.
    assert text.range_at(3, "character") == r(3, 5)
    assert text.range_at(4, "character") == r(3, 5)
    assert text.range_at(13, "character") == r(12, 14)
    assert text.range_at(7, "character") == r(7, 8)
    # At the end, the character that ends there.
    assert text.range_at(LENGTH, "character") == r(25, 26)


def test_range_at_words(notes):
    text = notes.text()
    assert text.range_at(0, "word") == r(0, 2)
    assert text.range_at(4, "word") == r(3, 5)
    assert text.range_at(7, "word") == r(6, 11)
    assert text.range_at(20, "word") == r(15, 21)


def test_range_at_lines_and_paragraphs_include_their_newline(notes):
    """Qt leaves the newline out of a line and, asked at the newline itself,
    answers the line before it; the provider appends the newline so the line
    contains the offset and matches what GTK reports."""
    text = notes.text()
    for unit in ("line", "paragraph"):
        assert text.range_at(0, unit) == r(0, 15), unit
        assert text.range_at(13, unit) == r(0, 15), unit
        assert text.range_at(14, unit) == r(0, 15), unit
        assert text.range_at(15, unit) == r(15, 26), unit
        assert text.range_at(LENGTH, unit) == r(15, 26), unit


def test_lines_page_through_the_text(notes):
    text = notes.text()
    every = text.lines((0, LENGTH))
    assert every.lines == [r(0, 15), r(15, 26)]
    assert not every.truncated
    assert every.next_start is None

    page = text.lines((0, LENGTH), 1)
    assert page.lines == [r(0, 15)]
    assert page.truncated
    assert page.next_start == 15

    assert text.lines((5, 20)).lines == [r(5, 15), r(15, 20)]


def test_bounds_are_one_rect_per_line_inside_the_view(notes):
    text = notes.text()
    outer = notes.bounds
    assert outer is not None

    def inside(rect):
        return (
            rect.x >= outer.x
            and rect.y >= outer.y
            and rect.x + rect.width <= outer.x + outer.width
            and rect.y + rect.height <= outer.y + outer.height
        )

    emoji = text.bounds((3, 5))
    assert len(emoji) == 1, emoji
    assert emoji[0].width > 0 and emoji[0].height > 0, emoji
    assert inside(emoji[0]), (emoji, outer)

    lines = text.bounds((0, LENGTH))
    assert len(lines) == 2, lines
    assert lines[1].y > lines[0].y, lines
    assert all(inside(rect) for rect in lines), (lines, outer)


def test_runs_report_the_bold_word(notes):
    runs = notes.text().runs((0, LENGTH))
    assert not runs.truncated
    # The runs tile the text with no gap or overlap.
    assert runs.runs[0].range.start == 0
    assert runs.runs[-1].range.end == LENGTH
    for before, after in zip(runs.runs, runs.runs[1:]):
        assert before.range.end == after.range.start, runs.runs

    bold = [run for run in runs.runs if run.range.start <= 7 < run.range.end]
    assert [run.range for run in bold] == [r(6, 11)], runs.runs
    assert bold[0].attributes.font_weight == 700

    for run in runs.runs:
        a = run.attributes
        # Qt sends the family in CSS quotes; the provider removes them.
        assert a.font_family and '"' not in a.font_family, a
        assert a.font_size is not None and a.font_size > 0, a
        assert a.italic is False, a
        if run.range != r(6, 11):
            # Qt sends `weight` only when it is not normal, so plain text
            # reports no weight rather than 400.
            assert a.font_weight is None, run
        # Qt sends no underline, strikethrough or colour for this text.
        assert a.underline is None and a.foreground is None, a

    page = notes.text().runs((0, LENGTH), 2)
    assert page.truncated
    assert page.next_start == 11
    assert [run.range for run in page.runs] == [r(0, 6), r(6, 11)]


def test_select_text_takes_utf16_offsets(notes):
    text = notes.text()
    # A caret belongs to the focused element.
    notes.focus()
    notes.select_text(3, 5)
    assert text.selections() == [r(3, 5)]
    assert text.caret() == 5

    notes.select_text(r(12, LENGTH))
    selections = text.selections()
    assert selections == [r(12, LENGTH)]
    assert text.read(selections[0]) == "\U0001d11e\nsecond line"

    with pytest.raises(xa11y.InvalidTextRangeError):
        notes.select_text(4, 6)

    # An empty range places the caret and clears the selection.
    notes.select_text(15, 15)
    assert text.selections() == []
    assert text.caret() == 15


def test_typing_reports_a_utf16_text_change(app):
    """Qt counts UTF-16, so a text_changed event carries its range."""
    pad = app.locator('text_area[name="Edit Pad"]')
    pad.focus()
    start = pad.text().caret()
    assert start is not None
    typed = "a\U0001f600b"
    with app.subscribe() as sub:
        pad.type_text(typed)
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.TEXT_CHANGED
            and e.text_change is not None
            and e.text_change.kind == "inserted",
            timeout=5.0,
        )
    change = event.text_change
    assert event.target is not None and event.target.name == "Edit Pad", event
    assert change.text == typed, change
    assert change.range == r(start, start + 4), change
    assert pad.text().read(change.range) == typed


# ── Tables ────────────────────────────────────────────────────────────────


def test_table_reports_its_dimensions(span_table):
    info = span_table.table
    assert info is not None
    assert (info.row_count, info.column_count) == (3, 3)


def test_cells_carry_zero_based_positions(span_table):
    banana = span_table.cell_at(1, 0)
    assert banana is not None and banana.name == "Banana"
    assert (banana.cell.row, banana.cell.column) == (1, 0)
    assert banana.cell.header is None
    red = span_table.cell_at(2, 1)
    assert red is not None and red.name == "Red"
    assert (red.cell.row, red.cell.column) == (2, 1)
    assert span_table.cell_at(3, 0) is None
    assert span_table.cell_at(0, 3) is None


def test_qt_reports_no_merged_cells(span_table):
    """The fixture merges "Apple" over two columns and "20" over two rows
    (``QTableWidget.setSpan``). Qt's adaptor reports neither: every extent it
    answers (``Table.GetRowExtentAt`` / ``GetColumnExtentAt``,
    ``TableCell.GetRowColumnSpan``) is 1, and each covered slot is a cell of
    its own. If this starts failing, Qt now reports spans: assert them."""
    apple = span_table.cell_at(0, 0)
    assert apple is not None and apple.name == "Apple"
    assert (apple.cell.row_span, apple.cell.column_span) == (1, 1)
    covered = span_table.cell_at(0, 1)
    assert covered is not None and covered.name != "Apple"
    assert (covered.cell.row, covered.cell.column) == (0, 1)

    twenty = span_table.cell_at(1, 2)
    assert twenty is not None and twenty.name == "20"
    assert (twenty.cell.row_span, twenty.cell.column_span) == (1, 1)
    below = span_table.cell_at(2, 2)
    assert below is not None and (below.cell.row, below.cell.column) == (2, 2)


def test_headers(span_table):
    columns = span_table.column_headers()
    assert [h.name for h in columns] == ["Fruit", "Colour", "Qty"]
    for h in columns:
        assert h.cell is not None and h.cell.header == "column", h
        assert (h.cell.row, h.cell.column) == (None, None), h
    rows = span_table.row_headers()
    assert [h.name for h in rows] == ["1", "2", "3"]
    assert all(h.cell is not None and h.cell.header == "row" for h in rows)

    banana = span_table.cell_at(1, 0)
    assert [h.name for h in banana.column_header_cells()] == ["Fruit"]
    assert [h.name for h in banana.row_header_cells()] == ["2"]


def test_selected_items_reports_the_current_cell(span_table):
    selected = span_table.selected_items()
    assert [s.name for s in selected] == ["Yellow"]
    assert (selected[0].cell.row, selected[0].cell.column) == (1, 1)


# ── Collections ───────────────────────────────────────────────────────────


def test_list_items_report_rows_not_set_positions(app):
    """Qt sends no ``posinset`` / ``setsize`` / ``level`` object attributes on
    list or tree items, so ``collection`` is not reported. A QListWidget row
    is an AT-SPI table cell instead, and carries its position as ``cell``."""
    items = app.locator('list[name="Items"]').element().children()
    assert [i.name for i in items[:3]] == ["Item 1", "Item 2", "Item 3"]
    for index, item in enumerate(items[:3]):
        assert item.collection is None, item
        assert item.cell is not None, item
        assert (item.cell.row, item.cell.column) == (index, 0), item


def test_tree_items_report_rows_not_depth(app):
    report = app.locator('[name="File Browser"] > [name="report.pdf"]').element()
    assert report.collection is None, report
    # A tree item's row counts the visible (expanded) rows, not its depth.
    assert (report.cell.row, report.cell.column) == (1, 0)
