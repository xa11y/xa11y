"""A table lists each of its cells once (macOS, Chromium).

Chromium lists every cell of a table under its ``AXRow`` and again under its
``AXColumn``, and a header cell a third time under the table's ``AXHeader``
group. The row is the cell's ``AXParent``, so the other copies are repeats.
The Electron app's "Users Table" has 3 rows of 2 cells, 2 columns and a header
group.
"""

from __future__ import annotations

import sys
from collections import Counter

import pytest


@pytest.fixture(autouse=True)
def macos_electron_only(app_name):
    if sys.platform != "darwin" or app_name != "electron":
        pytest.skip("AXColumn children are Chromium's, on macOS")


def _key(element) -> tuple:
    b = element.bounds
    return (element.name, None if b is None else (b.x, b.y, b.width, b.height))


def _cells_by_children(node) -> Counter:
    cells: Counter = Counter()
    for child in node.children():
        if child.role == "table_cell":
            cells[_key(child)] += 1
        cells += _cells_by_children(child)
    return cells


def test_a_table_lists_each_cell_once(app):
    table = app.locator('table[name="Users Table"]').element()
    children = table.children()
    rows = [c for c in children if c.role == "table_row"]
    groups = [c for c in children if c.role == "group"]

    cells = _cells_by_children(table)

    row_cells = [[c for c in r.children() if c.role == "table_cell"] for r in rows]
    assert [len(c) for c in row_cells] == [2, 2, 2]
    assert len(cells) == 6 and set(cells.values()) == {1}, cells
    assert [g.children() for g in groups] == [[], [], []]
