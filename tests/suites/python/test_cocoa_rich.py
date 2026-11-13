"""Text by range, tables and collections through macOS accessibility.

Reads the Cocoa test app (``test-apps/cocoa/main.swift``): its "Rich" box
(attributed ``NSTextView`` "Rich Text", two-column ``NSOutlineView``
"Produce Tree") and the view-based ``NSTableView`` "Users Table".

What AX shows, and so what is pinned here:

- **Offsets are UTF-16 natively.** AX indexes ``NSString``, so nothing is
  converted; what the provider adds is refusing a range that splits a
  surrogate pair, which AX would happily serve.
- **No word or paragraph unit.** AX has parameterized attributes for a
  rendered line only. A character is one Unicode scalar, as on every
  platform, not AX's composed character sequence.
- **Colours are the app's components.** AX drops the colour space in transit,
  so ``#ff0000`` set in sRGB arrives as those components.
- **Header cells are AppKit sort buttons**, not cells, so they carry no
  ``cell`` metadata; the header queries still return them.
- **Cells have no name.** The text is the static text inside each cell.
- **An outline is a list that answers the table APIs.** ``AXOutline`` keeps
  the ``list`` role, so selectors written against it keep matching, but an
  ``NSOutlineView`` is an ``NSTableView`` underneath: it reports ``table``
  dimensions and answers ``cell_at`` and the header queries like the users
  table does. Its row count is the rows on show, which grows as items expand.
- **A cell-based table cannot locate a cell.** The legacy ``NSCell`` "Items"
  list has no ``AXCell`` layer, so ``cell_at`` raises rather than answering
  ``None`` for every slot.

macOS only: the widgets are AppKit's.
"""

from __future__ import annotations

import sys

import pytest
import xa11y

# Mirrors AppDelegate.richText in test-apps/cocoa/main.swift. UTF-16 offsets:
# "😀" 12..14, "bold" 15..19, "italic" 20..26, newline 26; "Second" 27, "red"
# 45..48, "𝄞" 49..51, "end" 52..55, newline 55; the empty line 56..57;
# "Final paragraph" 57..72.
TEXT = "Hello wörld \U0001f600 bold italic\nSecond line under red \U0001d11e end\n\nFinal paragraph"
LENGTH = 72
LINES = [(0, 27), (27, 56), (56, 57), (57, LENGTH)]


@pytest.fixture(autouse=True)
def macos_cocoa_only(app_name):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("pins AppKit's AX behaviour; requires the Cocoa app on macOS")


def r(start: int, end: int) -> xa11y.TextRange:
    return xa11y.TextRange(start, end)


def content(element) -> list[str]:
    """The names of the static text under an AppKit cell or row."""
    found = []
    for child in element.children():
        if child.role == "static_text" and child.name:
            found.append(child.name)
        else:
            found.extend(content(child))
    return found


@pytest.fixture
def rich(app):
    return app.locator('text_area[name="Rich Text"]').element()


@pytest.fixture
def users(app):
    return app.locator('table[name="Users Table"]').element()


@pytest.fixture
def tree(app):
    return app.locator('[name="Produce Tree"]').element()


# ── Text: reads and UTF-16 offsets ────────────────────────────────────────


def test_length_and_reads_count_utf16_units(rich):
    assert len(TEXT.encode("utf-16-le")) // 2 == LENGTH, "fixture constant drifted"
    text = rich.text()
    assert text.length() == LENGTH
    assert text.read_all() == TEXT
    assert text.read((12, 14)) == "\U0001f600"
    assert text.read(r(15, 19)) == "bold"
    assert text.read((49, 51)) == "\U0001d11e"
    assert text.read((49, 56)) == "\U0001d11e end\n"


@pytest.mark.parametrize(
    "bad",
    [(13, 15), (12, 13), (50, 52), (13, 13), (70, LENGTH + 1), (5, 2)],
    ids=[
        "starts-mid-emoji",
        "ends-mid-emoji",
        "starts-mid-clef",
        "empty-mid-emoji",
        "past-end",
        "reversed",
    ],
)
def test_reads_that_split_a_pair_or_overrun_are_refused(rich, bad):
    with pytest.raises(xa11y.InvalidTextRangeError):
        rich.text().read(bad)


def test_find_reports_utf16_ranges(rich):
    text = rich.text()
    assert text.find("\U0001f600") == r(12, 14)
    assert text.find("bold") == r(15, 19)
    assert text.find("end") == r(52, 55)
    assert text.find("paragraph") == r(63, 72)
    assert text.find("absent") is None


def test_locator_text_resolves_the_element(app):
    text = app.locator('text_area[name="Rich Text"]').text()
    assert text.length() == LENGTH


def test_range_at_characters_cover_whole_surrogate_pairs(rich):
    text = rich.text()
    assert text.range_at(12, "character") == r(12, 14)
    assert text.range_at(13, "character") == r(12, 14)
    assert text.range_at(50, "character") == r(49, 51)
    assert text.range_at(7, "character") == r(7, 8)
    assert text.range_at(LENGTH, "character") == r(71, 72)


def test_words_and_paragraphs_are_unsupported(rich):
    """AX has no word or paragraph boundary attribute for text elements."""
    for unit in ("word", "paragraph"):
        with pytest.raises(xa11y.ActionNotSupportedError, match="Unsupported"):
            rich.text().range_at(3, unit)


def test_lines_tile_the_text(rich):
    text = rich.text()
    for start, end in LINES:
        assert text.range_at(start, "line") == r(start, end)
        # The newline belongs to the line it ends.
        assert text.range_at(end - 1, "line") == r(start, end)
    assert text.range_at(LENGTH, "line") == r(*LINES[-1])

    every = text.lines((0, LENGTH))
    assert every.lines == [r(*line) for line in LINES]
    assert not every.truncated and every.next_start is None

    page = text.lines((0, LENGTH), 1)
    assert page.lines == [r(0, 27)]
    assert page.truncated and page.next_start == 27

    assert text.lines((5, 30)).lines == [r(5, 27), r(27, 30)]


def test_bounds_are_one_rect_per_line_inside_the_view(rich):
    text = rich.text()
    outer = rich.bounds
    assert outer is not None

    def inside(rect):
        return (
            rect.x >= outer.x - 1
            and rect.y >= outer.y - 1
            and rect.x + rect.width <= outer.x + outer.width + 1
            and rect.y + rect.height <= outer.y + outer.height + 1
        )

    emoji = text.bounds((12, 14))
    assert len(emoji) == 1, emoji
    assert emoji[0].width > 0 and emoji[0].height > 0, emoji
    assert inside(emoji[0]), (emoji, outer)

    lines = text.bounds((0, LENGTH))
    # The empty third line may report no width; the others report one each.
    assert len(lines) >= 3, lines
    assert all(b.y > a.y for a, b in zip(lines, lines[1:])), lines
    assert all(inside(rect) for rect in lines), (lines, outer)


# ── Text: formatting ──────────────────────────────────────────────────────


def test_runs_tile_and_report_the_formatting(rich):
    runs = rich.text().runs((0, LENGTH))
    assert not runs.truncated
    assert runs.runs[0].range.start == 0
    assert runs.runs[-1].range.end == LENGTH
    for before, after in zip(runs.runs, runs.runs[1:]):
        assert before.range.end == after.range.start, runs.runs

    def at(offset):
        found = [run for run in runs.runs if run.range.start <= offset < run.range.end]
        assert len(found) == 1, (offset, runs.runs)
        return found[0]

    plain = at(0).attributes
    assert plain.font_family, plain
    assert plain.font_size == pytest.approx(13), plain
    assert plain.font_weight == 400, plain
    assert plain.italic is False, plain
    assert plain.underline is False, plain
    assert plain.script == "baseline", plain
    assert plain.foreground is not None and plain.foreground == "#000000", plain

    bold = at(16)
    assert bold.range == r(15, 19), runs.runs
    assert bold.attributes.font_weight == 700, bold

    italic = at(21)
    assert italic.range == r(20, 26), runs.runs
    assert italic.attributes.italic is True, italic

    red = at(46)
    assert red.range == r(45, 48), runs.runs
    assert red.attributes.underline is True, red
    assert red.attributes.foreground == "#ff0000", red

    page = rich.text().runs((0, LENGTH), 1)
    assert page.truncated
    assert page.next_start == runs.runs[0].range.end


# ── Text: caret and selection ─────────────────────────────────────────────


def test_select_text_takes_utf16_offsets(rich):
    text = rich.text()
    # A caret belongs to the focused text element; unfocused, it is None.
    rich.focus()
    rich.select_text(12, 14)
    assert text.selections() == [r(12, 14)]
    # AX reports a selected range without saying which end is active, so a
    # selected range means no caret.
    assert text.caret() is None

    rich.select_text(r(49, LENGTH))
    assert text.selections() == [r(49, LENGTH)]

    for bad in [(13, 15), (12, 13), (70, LENGTH + 1)]:
        with pytest.raises(xa11y.InvalidTextRangeError):
            rich.select_text(*bad)

    rich.select_text(27, 27)
    assert text.selections() == []
    assert text.caret() == 27


def test_selecting_text_reports_a_text_selection_event(app, rich):
    rich.select_text(0, 0)
    with app.subscribe() as sub:
        rich.select_text(15, 19)
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.TEXT_SELECTION_CHANGED,
            timeout=5.0,
        )
    assert event.event_type == "text_selection_changed"


def test_a_button_has_no_text_or_table_metadata(app):
    button = app.locator('button[name="OK"]').element()
    with pytest.raises(xa11y.ActionNotSupportedError):
        button.text().length()
    assert button.table is None
    assert button.cell is None
    assert button.collection is None


# ── Tables ────────────────────────────────────────────────────────────────


def test_table_reports_its_dimensions(users):
    info = users.table
    assert info is not None
    assert (info.row_count, info.column_count) == (2, 2)


def test_cells_carry_zero_based_positions_and_unit_spans(users):
    alice = users.cell_at(0, 0)
    assert alice is not None and content(alice) == ["Alice"]
    assert (alice.cell.row, alice.cell.column) == (0, 0)
    assert (alice.cell.row_span, alice.cell.column_span) == (1, 1)
    assert alice.cell.header is None

    user = users.cell_at(1, 1)
    assert user is not None and content(user) == ["User"]
    assert (user.cell.row, user.cell.column) == (1, 1)

    assert users.cell_at(2, 0) is None
    assert users.cell_at(0, 2) is None


def test_a_cell_based_table_refuses_cell_at(app):
    # The "Items" list is a legacy cell-based NSTableView: text hangs straight
    # off each AXRow with no AXCell layer and no index ranges, so no slot can
    # be located. That must not read as an empty cell.
    items = app.locator('table[name="Items"]').element()
    assert items.table is not None and items.table.row_count >= 1
    with pytest.raises(xa11y.ActionNotSupportedError, match="cell-based"):
        items.cell_at(0, 0)


def test_selector_matches_cell_positions(app):
    matched = app.locator(
        'table[name="Users Table"] table_cell[row="1"][column="0"]'
    ).elements()
    assert len(matched) == 1
    assert content(matched[0]) == ["Bob"]


def test_rows_report_their_index(app):
    rows = app.locator('table[name="Users Table"] table_row').elements()
    assert [row.table_row.index for row in rows] == [0, 1]


def test_headers(users):
    """AppKit's headers are the sort buttons of the header row."""
    assert [h.name for h in users.column_headers()] == ["Name", "Role"]
    assert users.row_headers() == []

    user = users.cell_at(1, 1)
    assert [h.name for h in user.column_header_cells()] == ["Role"]
    assert user.row_header_cells() == []


# Mirrors produceRoots and the "Kind" column in test-apps/cocoa/main.swift,
# in expanded row order.
PRODUCE = [
    ("Vegetables", "Group"),
    ("Carrot", "Item"),
    ("Leek", "Item"),
    ("Dairy", "Group"),
    ("Milk", "Item"),
]


def test_outline_keeps_the_list_role_and_reports_table_dimensions(tree):
    assert tree.role == "list"
    info = tree.table
    assert info is not None
    assert (info.row_count, info.column_count) == (len(PRODUCE), 2)


def test_outline_cell_at_reads_every_column(tree):
    for row, (name, kind) in enumerate(PRODUCE):
        first = tree.cell_at(row, 0)
        assert first is not None, row
        assert content(first) == [name]
        assert (first.cell.row, first.cell.column) == (row, 0)

        second = tree.cell_at(row, 1)
        assert second is not None, row
        assert content(second) == [kind]
        assert (second.cell.row, second.cell.column) == (row, 1)

    assert tree.cell_at(len(PRODUCE), 0) is None
    assert tree.cell_at(0, 2) is None


def test_outline_selector_matches_cell_positions(app):
    matched = app.locator(
        'list[name="Produce Tree"] table_cell[row="3"][column="1"]'
    ).elements()
    assert len(matched) == 1
    assert content(matched[0]) == ["Group"]


def test_outline_headers(tree):
    assert [h.name for h in tree.column_headers()] == ["Produce", "Kind"]
    assert tree.row_headers() == []

    leek = tree.cell_at(2, 1)
    assert leek is not None
    assert [h.name for h in leek.column_header_cells()] == ["Kind"]
    assert leek.row_header_cells() == []


def test_a_non_table_refuses_table_calls(rich):
    with pytest.raises(xa11y.ActionNotSupportedError):
        rich.cell_at(0, 0)
    with pytest.raises(xa11y.ActionNotSupportedError):
        rich.column_headers()


# ── Collections ───────────────────────────────────────────────────────────


def test_outline_rows_report_depth_and_row_index(app):
    rows = app.locator('[name="Produce Tree"] > tree_item').elements()
    # A row holds one cell per column, in column order.
    assert [content(row) for row in rows] == [list(pair) for pair in PRODUCE]
    assert [row.collection.depth for row in rows] == [0, 1, 1, 0, 1]
    assert [row.table_row.index for row in rows] == [0, 1, 2, 3, 4]
    # AppKit reports no position within the parent or set size.
    assert all(row.collection.index is None for row in rows)
    assert all(row.collection.size is None for row in rows)


def test_selected_items_reports_the_selected_row(tree):
    selected = tree.selected_items()
    assert [content(row) for row in selected] == [["Carrot", "Item"]]
    assert selected[0].table_row.index == 1
