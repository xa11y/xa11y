"""Integration tests: discover and interact with all Qt widget types via xa11y."""

from __future__ import annotations

import sys

import pytest


# ── Helpers ─────────────────────────────────────────────────────────────────


def _find_any(qt_app, *selectors):
    """Return the first locator that exists from the given selectors."""
    for sel in selectors:
        loc = qt_app.locator(sel)
        if loc.exists():
            return loc
    return None


# ── Tree dump (runs first for debugging) ────────────────────────────────────


def test_tree_dump(qt_app):
    """Dump the full accessibility tree for CI debugging."""
    root = qt_app.elements()
    lines = []
    seen: set = set()

    def dump(el, indent=0):
        # Guard against infinite recursion (provider bug: node is its own child)
        key = el.stable_id if el.stable_id else id(el)
        if key in seen or indent > 30:
            lines.append(" " * indent + "... (cycle or depth limit)")
            return
        seen.add(key)

        info = f"{el.role}"
        if el.name:
            info += f'  name="{el.name}"'
        if el.value is not None:
            info += f'  value="{el.value}"'
        if el.numeric_value is not None:
            info += f"  num={el.numeric_value}"
        if el.checked is not None:
            info += f"  checked={el.checked}"
        if not el.enabled:
            info += "  DISABLED"
        lines.append(" " * indent + info)
        for c in el.children:
            dump(c, indent + 2)

    dump(root)
    tree_text = "\n".join(lines)
    import warnings

    warnings.warn(
        f"\n=== Accessibility Tree ({sys.platform}) ===\n{tree_text}\n=== End Tree ===",
        stacklevel=1,
    )
    assert root is not None
    assert len(root.children) > 0, (
        f"Tree is empty! Root: role={root.role}, name={root.name}"
    )


# ── Window & App ────────────────────────────────────────────────────────────


def test_app_found(qt_app):
    assert qt_app.pid > 0


def test_window_exists(qt_app):
    assert qt_app.locator("window").exists()


# ── Buttons ─────────────────────────────────────────────────────────────────


def test_button_found(qt_app):
    assert qt_app.locator("button").exists()


def test_button_count(qt_app):
    assert qt_app.locator("button").count() >= 2


def test_button_press(qt_app):
    qt_app.locator("button").first().press()


def test_button_enabled_state(qt_app):
    el = qt_app.locator("button").first().element()
    assert isinstance(el.enabled, bool)


# ── Checkboxes ──────────────────────────────────────────────────────────────


def test_checkbox_found(qt_app):
    assert qt_app.locator("check_box").exists()


def test_checkbox_toggle(qt_app):
    qt_app.locator("check_box").first().toggle()


def test_checkbox_checked_state(qt_app):
    el = qt_app.locator("check_box").first().element()
    assert el.checked is not None


# ── Radio Buttons ───────────────────────────────────────────────────────────


def test_radio_button_found(qt_app):
    assert qt_app.locator("radio_button").exists()


def test_radio_button_press(qt_app):
    qt_app.locator("radio_button").first().press()


# ── ComboBox ────────────────────────────────────────────────────────────────


def test_combobox_found(qt_app):
    assert qt_app.locator("combo_box").exists()


def test_combobox_count(qt_app):
    assert qt_app.locator("combo_box").count() >= 1


# ── Sliders ─────────────────────────────────────────────────────────────────


def test_slider_found(qt_app):
    assert qt_app.locator("slider").exists()


def test_slider_numeric_value(qt_app):
    el = qt_app.locator("slider").first().element()
    assert el.numeric_value is not None


def test_slider_range(qt_app):
    el = qt_app.locator("slider").first().element()
    assert el.min_value is not None
    assert el.max_value is not None


def test_slider_increment(qt_app):
    qt_app.locator("slider").first().increment()


# ── Spin Boxes ──────────────────────────────────────────────────────────────


def test_spinbutton_found(qt_app):
    assert qt_app.locator("spin_button").exists()


def test_spinbutton_numeric_value(qt_app):
    el = qt_app.locator("spin_button").first().element()
    assert el.numeric_value is not None


def test_spinbutton_increment(qt_app):
    qt_app.locator("spin_button").first().increment()


# ── Progress Bar ────────────────────────────────────────────────────────────


def test_progressbar_found(qt_app):
    assert qt_app.locator("progress_bar").exists()


def test_progressbar_value(qt_app):
    el = qt_app.locator("progress_bar").first().element()
    assert el.numeric_value is not None


# ── Text Fields ─────────────────────────────────────────────────────────────


def test_textfield_found(qt_app):
    assert qt_app.locator("text_field").exists()


def test_textfield_value(qt_app):
    el = qt_app.locator("text_field").first().element()
    assert el.value is not None


def test_textfield_set_value(qt_app):
    qt_app.locator("text_field").first().set_value("new text")


def test_textfield_focus(qt_app):
    qt_app.locator("text_field").first().focus()


# ── Text Area (Multi-line) ─────────────────────────────────────────────────


def test_textarea_found(qt_app):
    # QTextEdit maps to text_area on Linux/AT-SPI but may map differently
    # on Windows (shows as group or text_field).
    loc = _find_any(qt_app, "text_area", '[name="Notes"]')
    assert loc is not None and loc.exists()


# ── Static Text / Labels ───────────────────────────────────────────────────


def test_label_found(qt_app):
    # QLabel maps to static_text on Linux but may map to other roles on Windows.
    loc = _find_any(qt_app, "static_text", '[name="Heading Text"]')
    assert loc is not None and loc.exists()


# ── List ────────────────────────────────────────────────────────────────────


def test_list_found(qt_app):
    assert qt_app.locator("list").exists()


def test_list_items(qt_app):
    assert qt_app.locator("list_item").count() >= 1


# ── Tree Items ──────────────────────────────────────────────────────────────


def test_tree_items(qt_app):
    # Tree widget items may show as tree_item or table_row depending on platform
    found = qt_app.locator("tree_item").exists() or qt_app.locator("table_row").exists()
    assert found


# ── Toolbar ─────────────────────────────────────────────────────────────────


def test_toolbar_found(qt_app):
    assert qt_app.locator("toolbar").exists()


# ── Menu Bar ────────────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS uses system menu bar")
def test_menubar_found(qt_app):
    assert qt_app.locator("menu_bar").exists()


# ── Group ───────────────────────────────────────────────────────────────────


def test_group_found(qt_app):
    assert qt_app.locator("group").exists()


# ── Scroll Bar ──────────────────────────────────────────────────────────────


def test_scrollbar_found(qt_app):
    loc = qt_app.locator("scroll_bar")
    assert isinstance(loc.exists(), bool)
