"""Tables, collections and text by range in web content (issue #655).

The native fixtures (GTK 3 in ``xa11y/tests/integ/rich.rs``, Qt in
``test_qt_rich.py``, AppKit in ``test_cocoa_rich.py``) cannot make several
shapes real apps show every day. Chromium can, so the Electron test app's
"Rich" section (``test-apps/electron/index.html``) holds them:

- **Merged cells.** An HTML ``colspan`` / ``rowspan`` table. Qt reports no
  merge, and GTK 3's tree view cannot make one.
- **Virtualized content.** A grid that declares ``aria-rowcount="1000"`` and
  realizes three rows, and lists that declare ``aria-setsize`` totals
  (including ``-1``, unknown) for the few items on the page.
- **Tree depth** from ``aria-level``-style nesting, which GTK 3 does not
  report on Linux.
- **Text edge cases**: a combining sequence (``e`` + U+0301 is two
  characters), right-to-left text, lines that wrap in a ``textarea``, and
  ``contenteditable`` editors. It also checks that line bounds scale at a
  non-default device scale factor.

Each test pins what Chromium reports through the platform API. Where Linux
(AT-SPI) and macOS (AX) differ, the test says which one it describes. The
Chromium quirks the suite records instead of hiding are listed below.

- ``aria-rowcount="-1"`` is dropped, not passed on as ``-1``, so a web table
  never reads as ``Count.Unknown``. A list's ``aria-setsize="-1"`` is passed
  on.
- A word range includes its trailing space.
- In right-to-left text, the AT-SPI word boundary falls one character early,
  and per-character bounds are not in visual order. Logical reads (``read``,
  ``find``, selection, caret) are exact, and they are what this suite asserts.

On macOS, Chromium's AX tree differs from its AT-SPI one in ways that are
platform facts rather than xa11y choices:

- A table cell has no ``AXTitle``. Its text is in ``AXStaticText`` children,
  so the suite reads a cell's text with ``text_of``. Header cells carry no
  header marker either (see ``xa11y-macos/src/ax/rich.rs``).
- A list item is an ``AXGroup`` with no position (``collection`` is None).
- ``AXStyleRangeForIndex`` answers one run for the whole editor, so the bold
  run is not split out.
- A ``textarea`` answers ``AXRangeForLine`` with the whole text, so wrapped
  text is one line with one bounding rectangle.
- AX has no word unit, so ``range_at(.., "word")`` is unsupported.

Two macOS results look like xa11y bugs rather than Chromium facts, and are
``xfail(strict=True)`` until #665 is fixed: selection reads fail with
``kAXErrorFailure``, and a ``contenteditable`` reports no character bounds.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest
import xa11y

LINUX = sys.platform.startswith("linux")
MACOS = sys.platform == "darwin"

PROJECT_ROOT = Path(__file__).resolve().parents[3]
ELECTRON_DIR = PROJECT_ROOT / "test-apps" / "electron"

# "cafe" + U+0301 + " naïve " + "bold": the acute accent is its own character
# at offset 4, so "é" as written is the two characters 3..5.
RICH_TEXT = "café naïve bold"
RTL_TEXT = "שלום abc"
WRAPPED_TEXT = "alpha beta gamma delta epsilon zeta"


@pytest.fixture(autouse=True)
def electron_only(app_name):
    if app_name != "electron":
        pytest.skip("reads the Electron test app's Rich section")


# Chromium answers AXSelectedTextRanges with kAXErrorFailure (#665).
SELECTION_READ_FAILS_ON_MACOS = pytest.mark.xfail(
    MACOS, raises=xa11y.PlatformError, strict=True, reason="#665"
)


def r(start: int, end: int) -> xa11y.TextRange:
    return xa11y.TextRange(start, end)


def by_name(app, name: str):
    return app.locator(f'[name="{name}"]').element()


def text_of(element) -> str:
    """An element's text: its name, or else the text of its descendants.

    AT-SPI names a web cell from its content. AX gives the cell no title and
    keeps the text in ``AXStaticText`` children, whose value holds it.
    """
    if element.name:
        return element.name
    return "".join(c.name or c.value or text_of(c) for c in element.children())


def contains(outer, inner) -> bool:
    """Whether ``inner`` lies inside ``outer``, allowing a pixel of rounding."""
    return (
        inner.x >= outer.x - 1
        and inner.y >= outer.y - 1
        and inner.x + inner.width <= outer.x + outer.width + 1
        and inner.y + inner.height <= outer.y + outer.height + 1
    )


def settle(read, want, timeout=3.0):
    """Poll ``read()`` until it returns ``want``. Selection writes reach the
    renderer asynchronously, so the first read can see the old state."""
    deadline = time.monotonic() + timeout
    got = read()
    while got != want and time.monotonic() < deadline:
        time.sleep(0.05)
        got = read()
    return got


# ── Merged cells ──────────────────────────────────────────────────────────


@pytest.fixture
def span_table(app):
    return app.locator('table[name="Span Table"]').element()


def test_span_table_size(span_table):
    assert span_table.table.row_count == 4
    assert span_table.table.column_count == 3


def test_colspan_cell_answers_for_every_slot_it_covers(span_table):
    for column in (0, 1):
        cell = span_table.cell_at(1, column)
        assert cell is not None, f"no cell at (1, {column})"
        assert text_of(cell) == "Apple"
        assert cell.cell.row == 1
        assert cell.cell.column == 0, "a merged cell reports its top-left slot"
        assert cell.cell.column_span == 2
        assert cell.cell.row_span == 1


def test_rowspan_cell_answers_for_every_slot_it_covers(span_table):
    for row in (2, 3):
        cell = span_table.cell_at(row, 2)
        assert cell is not None, f"no cell at ({row}, 2)"
        assert text_of(cell) == "20"
        assert cell.cell.row == 2, "a merged cell reports its top-left slot"
        assert cell.cell.column == 2
        assert cell.cell.row_span == 2
        assert cell.cell.column_span == 1


def test_cells_after_a_span_keep_their_grid_column(span_table):
    # "10" follows a two-column cell, so it is column 2, not its DOM index 1.
    ten = span_table.cell_at(1, 2)
    assert ten is not None and text_of(ten) == "10"
    assert (ten.cell.row, ten.cell.column) == (1, 2)
    # "Red" sits beside the rowspan from the row above.
    red = span_table.cell_at(3, 1)
    assert red is not None and text_of(red) == "Red"


def test_span_table_column_headers(span_table):
    headers = span_table.column_headers()
    assert [text_of(h) for h in headers] == ["Fruit", "Colour", "Qty"]
    for column, header in enumerate(headers):
        assert header.cell.row == 0
        assert header.cell.column == column
        # AX marks no cell as a header; the table's header attribute is
        # what found these.
        assert header.cell.header == ("column" if LINUX else None)
    assert span_table.row_headers() == []


@pytest.mark.parametrize(
    ("row", "column", "header"),
    [(1, 0, "Fruit"), (1, 2, "Qty"), (2, 0, "Fruit"), (3, 2, "Qty"), (3, 1, "Colour")],
)
def test_data_cells_name_their_column_header(span_table, row, column, header):
    cell = span_table.cell_at(row, column)
    assert cell is not None
    assert [text_of(h) for h in cell.column_header_cells()] == [header]
    assert cell.row_header_cells() == []


# ── Virtualized grids ─────────────────────────────────────────────────────


@pytest.fixture
def paged_grid(app):
    return by_name(app, "Paged Grid")


@pytest.mark.skipif(not LINUX, reason="pins the AT-SPI rowcount object attribute")
def test_paged_grid_reports_the_declared_total(paged_grid):
    # Three rows exist; aria-rowcount says 1000. AT-SPI's Table.NRows
    # counts the realized rows, so the total comes from the rowcount
    # attribute Chromium sets.
    assert paged_grid.table.row_count == 1000
    assert paged_grid.table.column_count == 2
    rows = [c for c in paged_grid.children() if c.role == "table_row"]
    assert len(rows) == 3, "only the realized rows are children"


@pytest.mark.skipif(not LINUX, reason="pins Chromium's AT-SPI cell indices")
def test_paged_grid_cells_use_realized_indices(paged_grid):
    # Row 500 of the data set is the second realized row. CellInfo.row is
    # the grid index as laid out now, which is why it must not be compared
    # with row_count (see xa11y_core::table).
    cell = paged_grid.cell_at(1, 0)
    assert cell is not None and cell.name == "500"
    assert cell.cell.row == 1
    assert paged_grid.cell_at(499, 0) is None, "a slot that is not realized has no cell"


@pytest.mark.skipif(not LINUX, reason="pins Chromium's AT-SPI behaviour")
def test_unknown_row_total_is_not_passed_on(app):
    # aria-rowcount="-1" means "unknown". Chromium drops the attribute
    # rather than exposing -1, so the grid reports the rows it has. If a
    # Chromium release starts passing it on, this should read Unknown.
    grid = by_name(app, "Endless Grid")
    assert grid.table.row_count == 2
    assert grid.table.column_count == 2


# ── Collections ───────────────────────────────────────────────────────────


def _items(app, list_name):
    return app.locator(f'[name="{list_name}"] list_item').elements()


@pytest.mark.skipif(not LINUX, reason="pins AT-SPI posinset / setsize")
def test_list_reports_a_window_onto_a_larger_set(app):
    items = _items(app, "Paged List")
    assert [(i.collection.index, i.collection.size) for i in items] == [
        (40, 200),
        (41, 200),
    ]


@pytest.mark.skipif(not LINUX, reason="pins AT-SPI posinset / setsize")
def test_list_with_unknown_size_reports_unknown(app):
    items = _items(app, "Endless List")
    assert [(i.collection.index, i.collection.size) for i in items] == [
        (0, "unknown"),
        (1, "unknown"),
    ]


@pytest.mark.skipif(not MACOS, reason="pins Chromium's AX list items")
def test_web_list_items_report_no_collection_on_macos(app):
    # Chromium exposes an <li> to AX as an AXGroup with no position, so the
    # declared posinset / setsize do not reach AX clients at all.
    items = by_name(app, "Paged List").children()
    assert [text_of(i) for i in items] == ["Item 41", "Item 42"]
    assert all(i.collection is None for i in items)


@pytest.mark.skipif(not LINUX, reason="pins AT-SPI level / posinset / setsize")
def test_tree_items_report_depth_and_position(app):
    items = app.locator('[name="Outline"] tree_item').elements()
    got = {
        i.name.split()[0]: (i.collection.index, i.collection.size, i.collection.depth)
        for i in items
    }
    assert got == {
        "Docs": (0, 2, 0),
        "report.pdf": (0, 2, 1),
        "Archive": (1, 2, 1),
        "old.txt": (0, 1, 2),
        "Music": (1, 2, 0),
    }


# ── Text: combining sequences ─────────────────────────────────────────────


@pytest.fixture
def rich_editor(app):
    return by_name(app, "Rich Editor")


def test_combining_mark_is_its_own_character(rich_editor):
    text = rich_editor.text()
    assert text.length() == len(RICH_TEXT) == 16
    assert text.read_all() == RICH_TEXT
    assert text.read((4, 5)) == "́"
    # The platform's character unit is a code point, not a grapheme: the
    # base letter and its accent are separate one-character ranges.
    assert text.range_at(3, "character") == r(3, 4)
    assert text.range_at(4, "character") == r(4, 5)
    assert text.find("é") == r(3, 5)


def test_rich_editor_runs_split_at_bold(rich_editor):
    runs = rich_editor.text().runs((0, 16)).runs
    if MACOS:
        # Chromium's AXStyleRangeForIndex answers the whole editor.
        assert [run.range for run in runs] == [r(0, 16)]
        return
    assert [run.range for run in runs] == [r(0, 12), r(12, 16)]
    assert runs[1].attributes.font_weight == 700
    assert runs[0].attributes.font_weight != 700


def test_rich_editor_word_includes_trailing_space(rich_editor):
    # TextUnit.WORD follows the platform's boundaries; Chromium's include the
    # space after the word. AX has no word unit at all.
    if MACOS:
        with pytest.raises(xa11y.ActionNotSupportedError):
            rich_editor.text().range_at(0, "word")
        return
    assert rich_editor.text().range_at(0, "word") == r(0, 6)


@SELECTION_READ_FAILS_ON_MACOS
def test_rich_editor_selection_and_caret(rich_editor):
    text = rich_editor.text()
    rich_editor.select_text(1, 3)
    assert settle(text.selections, [r(1, 3)]) == [r(1, 3)]
    assert text.caret() == 3
    rich_editor.select_text(2, 2)
    assert settle(text.caret, 2) == 2
    assert text.selections() == []


def test_rich_editor_refuses_a_range_past_the_end(rich_editor):
    with pytest.raises(xa11y.InvalidTextRangeError):
        rich_editor.text().read((0, 17))


# ── Text: right-to-left ───────────────────────────────────────────────────


@pytest.mark.parametrize("name", ["RTL Editor", "RTL Notes"])
def test_rtl_text_reads_in_logical_order(app, name):
    # Offsets are logical (storage) order whatever the display direction:
    # the Hebrew word is 0..4, "abc" 5..8.
    element = by_name(app, name)
    text = element.text()
    assert text.length() == 8
    assert text.read_all() == RTL_TEXT
    assert text.read((0, 4)) == RTL_TEXT[:4]
    assert text.find("abc") == r(5, 8)


@SELECTION_READ_FAILS_ON_MACOS
@pytest.mark.parametrize("name", ["RTL Editor", "RTL Notes"])
def test_rtl_selection_uses_logical_offsets(app, name):
    element = by_name(app, name)
    text = element.text()
    element.select_text(5, 8)
    assert settle(text.selections, [r(5, 8)]) == [r(5, 8)]
    assert text.caret() == 8


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(
            "RTL Editor",
            marks=pytest.mark.xfail(MACOS, raises=AssertionError, strict=True, reason="#665"),
        ),
        "RTL Notes",
    ],
)
def test_rtl_character_bounds_lie_inside_the_element(app, name):
    # Visual order is not asserted: Chromium's per-character boxes in RTL
    # text do not follow it (see the module docs).
    element = by_name(app, name)
    text = element.text()
    for i in range(text.length()):
        rects = text.bounds((i, i + 1))
        assert rects, f"no bounds for character {i}"
        for rect in rects:
            assert contains(element.bounds, rect), (i, rect, element.bounds)


# ── Text: wrapped lines ───────────────────────────────────────────────────


@pytest.fixture
def wrapped(app):
    return by_name(app, "Wrapped Notes")


def test_wrapped_textarea_reports_visual_lines(wrapped):
    text = wrapped.text()
    assert text.read_all() == WRAPPED_TEXT
    lines = text.lines((0, len(WRAPPED_TEXT))).lines
    # Chromium answers a macOS textarea's AXRangeForLine with all its text.
    want = [r(0, 35)] if MACOS else [r(0, 17), r(17, 31), r(31, 35)]
    assert lines == want
    assert text.find("gamma") == r(11, 16)


def test_wrapped_line_bounds_stack_inside_the_element(wrapped):
    text = wrapped.text()
    rects = text.bounds((0, len(WRAPPED_TEXT)))
    if MACOS:
        # One line (see above), so one rectangle, tall enough for the three
        # lines it wraps onto.
        assert len(rects) == 1, rects
        assert contains(wrapped.bounds, rects[0]), (rects, wrapped.bounds)
        char = text.bounds((0, 1))[0]
        assert rects[0].height >= 2.5 * char.height, (rects, char)
        return
    assert len(rects) == 3, f"one rectangle per visual line, got {rects}"
    for rect in rects:
        assert contains(wrapped.bounds, rect), (rect, wrapped.bounds)
    ys = [rect.y for rect in rects]
    assert ys == sorted(ys) and len(set(ys)) == 3, ys


# ── Text: device scale factor ─────────────────────────────────────────────


def _electron_binary() -> str:
    """The Electron executable, not the ``.bin/electron`` node shim, so the
    pid is the app's (see test_web_accessibility.py)."""
    out = subprocess.run(
        ["node", "-e", 'process.stdout.write(require("electron"))'],
        cwd=ELECTRON_DIR,
        capture_output=True,
        check=True,
        text=True,
        timeout=30,
    )
    return out.stdout.strip()


@pytest.fixture
def electron_at_scale_2():
    """A second Electron test app with a device scale factor of 2."""
    proc = subprocess.Popen(
        [
            _electron_binary(),
            str(ELECTRON_DIR / "main.js"),
            "--no-sandbox",
            "--force-renderer-accessibility",
            "--force-device-scale-factor=2",
            "--xa11y-app-name=xa11y-electron-scale-2",
        ],
        cwd=ELECTRON_DIR,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        yield xa11y.App.by_pid(proc.pid, timeout=30)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


def _line_geometry(app):
    element = app.locator('[name="Wrapped Notes"]').element()
    text = element.text()
    rects = text.bounds((0, len(WRAPPED_TEXT)))
    char = text.bounds((0, 1))[0]
    return element.bounds, rects, char


@pytest.mark.skipif(not LINUX, reason="--force-device-scale-factor is pinned on X11 only")
def test_text_bounds_scale_with_the_device_scale_factor(app, electron_at_scale_2):
    # Screen coordinates are device pixels on X11, so at scale 2 every
    # measurement of the same text roughly doubles, and still lies inside
    # its element.
    scaled = electron_at_scale_2.locator('[name="Wrapped Notes"]').wait_attached(timeout=30)
    assert scaled is not None
    el1, lines1, char1 = _line_geometry(app)
    el2, lines2, char2 = _line_geometry(electron_at_scale_2)

    assert len(lines2) == len(lines1) == 3
    for rect in lines2:
        assert contains(el2, rect), (rect, el2)

    def ratio(a, b):
        return b / a

    for label, a, b in [
        ("element width", el1.width, el2.width),
        ("line height", lines1[0].height, lines2[0].height),
        ("character width", char1.width, char2.width),
    ]:
        assert 1.7 <= ratio(a, b) <= 2.3, f"{label}: {a} at scale 1, {b} at scale 2"
