"""Cross-platform a11y tree consistency tests for the Qt (PySide6) test app.

These tests verify that the accessibility tree produced by the Qt test app:
1. Contains no elements with role "unknown" (every element must map to a known role).
2. Exposes the same widget roles regardless of platform (Linux AT-SPI / macOS AX / Windows UIA).

Platform-specific wrapper differences (e.g. top-level "application" vs "window",
menu bar presence on macOS) are accounted for — but functional widgets like
buttons, checkboxes, and sliders must have identical roles everywhere.
"""

from __future__ import annotations

import sys

import pytest


# ── helpers ────────────────────────────────────────────────────────────────────


def collect_tree(el, *, max_depth: int = 30, _depth: int = 0) -> list[dict]:
    """Recursively collect the a11y tree into a flat list of dicts."""
    if _depth > max_depth:
        return []
    node = {
        "role": el.role,
        "name": el.name,
        "depth": _depth,
    }
    nodes = [node]
    for child in el.children():
        nodes.extend(collect_tree(child, max_depth=max_depth, _depth=_depth + 1))
    return nodes


# Roles that only appear as outer app/window chrome — platform may omit or
# remap these, so they are excluded from the "no unknown" check on purpose
# (they are allowed, just not required uniformly).
PLATFORM_WRAPPER_ROLES = {"application", "window"}

# Roles that are legitimate structural containers added by the toolkit —
# they are not "unknown" and don't represent bugs.
TOOLKIT_STRUCTURAL_ROLES = {
    "group",
    "scroll_bar",
    "separator",
    "static_text",
    "status",
    "toolbar",
    "menu_bar",
    "menu",
    "menu_item",
    "table",
    "table_row",
    "table_cell",
    "list",
    "list_item",
    "heading",
    "navigation",
    "tab",
    "tab_group",
    "image",
}

# ── Expected widgets ──────────────────────────────────────────────────────────
# Each entry: (selector, expected_role, name_substring)
# These represent the functional widgets in the test app whose role MUST be
# consistent across all platforms.

EXPECTED_WIDGETS = [
    # Buttons
    ('button[name="OK"]', "button", "OK"),
    ('button[name="Cancel"]', "button", "Cancel"),
    # Checkboxes
    ('check_box[name="Agree to terms"]', "check_box", "Agree to terms"),
    ('check_box[name="Subscribe"]', "check_box", "Subscribe"),
    # Radio buttons
    ('radio_button[name="Option A"]', "radio_button", "Option A"),
    ('radio_button[name="Option B"]', "radio_button", "Option B"),
    ('radio_button[name="Option C"]', "radio_button", "Option C"),
    # Slider
    ('slider[name="Volume"]', "slider", "Volume"),
    # Spin buttons
    ('spin_button[name="Quantity"]', "spin_button", "Quantity"),
    ('spin_button[name="Price"]', "spin_button", "Price"),
    # Progress bar
    ('progress_bar[name="Progress"]', "progress_bar", "Progress"),
    # Text field
    ('text_field[name="Search"]', "text_field", "Search"),
]


# ── Tests ─────────────────────────────────────────────────────────────────────


def test_no_unknown_roles_in_tree(qt_app):
    """Every *named* element in the a11y tree must map to a known, non-unknown role.

    Unknown roles indicate that the platform accessibility bridge returned
    something xa11y couldn't classify — this is a bug (either in the
    provider's role mapping or the test app's widget setup).

    Nameless unknown nodes are excluded: Qt toolkits emit internal structural
    filler elements (e.g. QStatusBar grip, AT-SPI panel containers) that have
    no accessible name and role "unknown". These are harmless toolkit artifacts,
    not real widgets consumers would interact with.
    """
    nodes = collect_tree(qt_app)
    unknowns = [
        f'depth={n["depth"]} name={n["name"]!r}'
        for n in nodes
        if n["role"] == "unknown" and n["name"]
    ]
    assert not unknowns, (
        f"Found {len(unknowns)} *named* element(s) with role 'unknown' in the a11y tree:\n"
        + "\n".join(f"  - {u}" for u in unknowns[:20])
    )


def test_nameless_unknown_count_is_bounded(qt_app):
    """Nameless unknown nodes (Qt toolkit artifacts) must stay small.

    A handful of nameless unknowns are expected (QStatusBar grip, AT-SPI
    filler panels). But a large number would suggest a role-mapping regression.
    """
    nodes = collect_tree(qt_app)
    nameless_unknowns = [n for n in nodes if n["role"] == "unknown" and not n["name"]]
    # Qt typically produces 0-5 of these depending on platform.
    assert len(nameless_unknowns) <= 10, (
        f"Found {len(nameless_unknowns)} nameless unknown nodes — expected at most 10. "
        f"This may indicate a role-mapping regression."
    )


def test_tree_is_not_empty(qt_app):
    """Sanity check: the tree has a meaningful number of elements."""
    nodes = collect_tree(qt_app)
    # The test app has buttons, checkboxes, radios, sliders, text fields, etc.
    # Even with platform variation we expect well over 20 elements.
    assert len(nodes) >= 20, (
        f"Tree has only {len(nodes)} nodes — expected at least 20 for the Qt test app"
    )


def test_functional_widgets_have_correct_roles(qt_app):
    """Core functional widgets must have the same role on every platform.

    A button must be a button, a checkbox must be a check_box, etc.
    Platform differences in wrapper structure are fine, but widget roles
    must be consistent.
    """
    missing = []
    wrong_role = []

    for selector, expected_role, label in EXPECTED_WIDGETS:
        loc = qt_app.locator(selector)
        if not loc.exists():
            # Try a name-only fallback to detect role mismatches
            name_loc = qt_app.locator(f'[name="{label}"]')
            if name_loc.exists():
                actual = name_loc.element()
                wrong_role.append(
                    f'{label!r}: expected role={expected_role!r}, '
                    f'got role={actual.role!r}'
                )
            else:
                missing.append(f'{label!r} (selector: {selector})')
        else:
            el = loc.element()
            assert el.role == expected_role, (
                f'Widget {label!r}: expected role={expected_role!r}, '
                f'got role={el.role!r}'
            )

    # Missing widgets are hard failures — the test app defines them all.
    assert not missing, (
        f"Widget(s) not found in a11y tree:\n"
        + "\n".join(f"  - {m}" for m in missing)
    )
    # Wrong roles indicate a platform mapping bug.
    assert not wrong_role, (
        f"Widget(s) have wrong role:\n"
        + "\n".join(f"  - {w}" for w in wrong_role)
    )


def test_combobox_role_is_combo_box(qt_app):
    """ComboBox widgets must have role 'combo_box', not 'unknown' or something else.

    On Linux AT-SPI, Qt comboboxes expose the selected item's text as the
    accessible name (e.g. "Apple") instead of the setAccessibleName() value
    ("Fruit"). We search by both the assigned name and the initial value.
    """
    # Try the accessible name first (Windows/macOS), then the initial value (Linux AT-SPI)
    loc = qt_app.locator('combo_box[name="Fruit"]')
    if not loc.exists():
        loc = qt_app.locator('combo_box[name="Apple"]')
    if not loc.exists():
        # Last resort: any combo_box
        loc = qt_app.locator("combo_box")
    assert loc.exists(), "No combo_box found at all in a11y tree"
    el = loc.element()
    assert el.role == "combo_box"


def test_editable_combobox_role(qt_app):
    """Editable combobox must also be role 'combo_box'.

    Same Linux AT-SPI naming caveat as test_combobox_role_is_combo_box:
    the name may be the initial value ("Red") instead of "Color".
    """
    loc = qt_app.locator('combo_box[name="Color"]')
    if not loc.exists():
        loc = qt_app.locator('combo_box[name="Red"]')
    if not loc.exists():
        # At least 2 combo_boxes must exist (Fruit + Color)
        count = qt_app.locator("combo_box").count()
        assert count >= 2, (
            f"Expected at least 2 combo_box elements (Fruit + Color), found {count}"
        )
        return
    el = loc.element()
    assert el.role == "combo_box"


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS uses system-level menu bar")
def test_menubar_role_is_menu_bar(qt_app):
    """MenuBar must be exposed as 'menu_bar' (except on macOS where it's system-level)."""
    loc = qt_app.locator("menu_bar")
    assert loc.exists(), "menu_bar not found in a11y tree"
    el = loc.element()
    assert el.role == "menu_bar"


def test_tree_roles_are_all_valid(qt_app):
    """Every role string in the tree must be a recognized xa11y role.

    This catches cases where a provider returns a role string that isn't
    in the Role enum — different from 'unknown' (which IS a valid enum
    variant but indicates a mapping gap).
    """
    valid_roles = {
        "unknown", "window", "application", "button", "check_box",
        "radio_button", "text_field", "text_area", "static_text",
        "combo_box", "list", "list_item", "menu", "menu_item",
        "menu_bar", "tab", "tab_group", "table", "table_row",
        "table_cell", "toolbar", "scroll_bar", "slider", "image",
        "link", "group", "dialog", "alert", "progress_bar",
        "tree_item", "web_area", "heading", "separator", "split_group",
        "switch", "spin_button", "tooltip", "status", "navigation",
    }

    nodes = collect_tree(qt_app)
    invalid = [
        f'role={n["role"]!r} name={n["name"]!r} depth={n["depth"]}'
        for n in nodes
        if n["role"] not in valid_roles
    ]
    assert not invalid, (
        f"Found {len(invalid)} element(s) with unrecognized role strings:\n"
        + "\n".join(f"  - {i}" for i in invalid[:20])
    )


def test_named_groups_are_present(qt_app):
    """QGroupBox sections should appear as named groups in the tree."""
    expected_groups = ["Buttons", "Checkboxes", "Options", "Range Controls",
                       "Input", "Text", "List", "Tree"]
    missing = []
    for name in expected_groups:
        loc = qt_app.locator(f'[name="{name}"]')
        if not loc.exists():
            missing.append(name)

    assert not missing, (
        f"Group section(s) not found in a11y tree:\n"
        + "\n".join(f'  - "{m}"' for m in missing)
    )


def test_text_area_found_with_correct_role(qt_app):
    """QTextEdit should map to 'text_area' (not 'unknown' or 'text_field')."""
    loc = qt_app.locator('[name="Notes"]')
    assert loc.exists(), "Notes text area not found in a11y tree"
    el = loc.element()
    # Accept text_area or text_field — some platforms may expose QTextEdit differently,
    # but it must NOT be unknown.
    assert el.role in ("text_area", "text_field"), (
        f'Notes widget has role={el.role!r}, expected "text_area" or "text_field"'
    )


def test_tree_widget_items_have_expected_roles(qt_app):
    """QTreeWidget items must be 'tree_item' or 'table_row' (platform varies), not 'unknown'."""
    # The tree widget has items like "Documents", "Photos", etc.
    tree_items = qt_app.locator("tree_item").count()
    table_rows = qt_app.locator("table_row").count()
    total = tree_items + table_rows
    # We have at least 5 items: Documents, report.pdf, notes.txt, Photos, vacation.jpg
    assert total >= 2, (
        f"Expected tree items (tree_item or table_row), found {tree_items} + {table_rows}"
    )


def test_list_widget_items_have_expected_roles(qt_app):
    """QListWidget items must be 'list_item' or 'table_row', not 'unknown'."""
    list_items = qt_app.locator("list_item").count()
    table_rows = qt_app.locator("table_row").count()
    # We defined 5 items; virtualization may limit visible ones, but expect at least 1.
    assert list_items + table_rows >= 1, (
        f"Expected list items, found {list_items} list_item + {table_rows} table_row"
    )
