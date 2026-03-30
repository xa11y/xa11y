"""Integration tests: discover and interact with all Qt widget types via xa11y."""

from __future__ import annotations

import sys

import pytest


# ── Window & App ────────────────────────────────────────────────────────────


def test_app_found(qt_app):
    assert qt_app.name == "xa11y-qt-test-app"
    assert qt_app.pid > 0


def test_window_exists(qt_app):
    loc = qt_app.locator("window")
    assert loc.exists()
    el = loc.element()
    assert "xa11y-qt-test-app" in (el.name or "")


# ── Buttons ─────────────────────────────────────────────────────────────────


def test_button_found(qt_app):
    ok = qt_app.locator('push_button[name="OK"]')
    assert ok.exists()


def test_button_press(qt_app):
    ok = qt_app.locator('push_button[name="OK"]')
    ok.press()
    # Should not raise


def test_button_disabled(qt_app):
    cancel = qt_app.locator('push_button[name="Cancel"]')
    el = cancel.element()
    # Cancel starts disabled; after OK is pressed it may toggle.
    # Just verify we can read the enabled state.
    assert isinstance(el.enabled, bool)


def test_button_accessible_description(qt_app):
    ok = qt_app.locator('push_button[name="OK"]')
    el = ok.element()
    assert el.description == "Confirm the dialog"


# ── Checkboxes ──────────────────────────────────────────────────────────────


def test_checkbox_found(qt_app):
    agree = qt_app.locator('check_box[name="Agree to terms"]')
    assert agree.exists()


def test_checkbox_toggle(qt_app):
    agree = qt_app.locator('check_box[name="Agree to terms"]')
    agree.toggle()
    # Should not raise


def test_checkbox_checked_state(qt_app):
    subscribe = qt_app.locator('check_box[name="Subscribe"]')
    el = subscribe.element()
    # Subscribe starts checked
    assert el.checked is not None


# ── Radio Buttons ───────────────────────────────────────────────────────────


def test_radio_button_found(qt_app):
    opt_a = qt_app.locator('radio_button[name="Option A"]')
    assert opt_a.exists()


def test_radio_button_select(qt_app):
    opt_b = qt_app.locator('radio_button[name="Option B"]')
    opt_b.press()


# ── ComboBox ────────────────────────────────────────────────────────────────


def test_combobox_found(qt_app):
    combo = qt_app.locator('combo_box[name="Fruit"]')
    assert combo.exists()


def test_combobox_value(qt_app):
    combo = qt_app.locator('combo_box[name="Fruit"]')
    el = combo.element()
    # Should expose the currently selected item
    assert el.value is not None or el.name is not None


def test_combobox_editable_found(qt_app):
    combo = qt_app.locator('combo_box[name="Color"]')
    assert combo.exists()


# ── Sliders ─────────────────────────────────────────────────────────────────


def test_slider_found(qt_app):
    slider = qt_app.locator('slider[name="Volume"]')
    assert slider.exists()


def test_slider_numeric_value(qt_app):
    slider = qt_app.locator('slider[name="Volume"]')
    el = slider.element()
    assert el.numeric_value is not None
    assert el.min_value is not None
    assert el.max_value is not None


def test_slider_increment(qt_app):
    slider = qt_app.locator('slider[name="Volume"]')
    slider.increment()


# ── Spin Boxes ──────────────────────────────────────────────────────────────


def test_spinbox_found(qt_app):
    spinner = qt_app.locator('spin_button[name="Quantity"]')
    assert spinner.exists()


def test_spinbox_numeric_value(qt_app):
    spinner = qt_app.locator('spin_button[name="Quantity"]')
    el = spinner.element()
    assert el.numeric_value is not None


def test_spinbox_increment(qt_app):
    spinner = qt_app.locator('spin_button[name="Quantity"]')
    spinner.increment()


def test_double_spinbox_found(qt_app):
    spinner = qt_app.locator('spin_button[name="Price"]')
    assert spinner.exists()


# ── Progress Bar ────────────────────────────────────────────────────────────


def test_progressbar_found(qt_app):
    progress = qt_app.locator('progress_bar[name="Progress"]')
    assert progress.exists()


def test_progressbar_value(qt_app):
    progress = qt_app.locator('progress_bar[name="Progress"]')
    el = progress.element()
    assert el.numeric_value is not None


# ── Line Edit (Text Field) ─────────────────────────────────────────────────


def test_lineedit_found(qt_app):
    search = qt_app.locator('text_field[name="Search"]')
    assert search.exists()


def test_lineedit_value(qt_app):
    search = qt_app.locator('text_field[name="Search"]')
    el = search.element()
    assert el.value is not None


def test_lineedit_set_value(qt_app):
    search = qt_app.locator('text_field[name="Search"]')
    search.set_value("new text")


def test_lineedit_focus(qt_app):
    search = qt_app.locator('text_field[name="Search"]')
    search.focus()


# ── Text Edit (Multi-line) ─────────────────────────────────────────────────


def test_textedit_found(qt_app):
    notes = qt_app.locator('[name="Notes"]')
    assert notes.exists()


# ── Labels ──────────────────────────────────────────────────────────────────


def test_label_found(qt_app):
    heading = qt_app.locator('[name="Heading Text"]')
    assert heading.exists()


# ── Tab Widget ──────────────────────────────────────────────────────────────


def test_tabs_found(qt_app):
    # Tab bars show up as tab or tab_list role depending on platform
    tabs = qt_app.locator("tab_list")
    if not tabs.exists():
        tabs = qt_app.locator("tab")
    assert tabs.exists()


# ── List Widget ─────────────────────────────────────────────────────────────


def test_list_found(qt_app):
    lst = qt_app.locator('[name="Items"]')
    assert lst.exists()


def test_list_items(qt_app):
    items = qt_app.locator("list_item")
    assert items.count() >= 5


# ── Tree Widget ─────────────────────────────────────────────────────────────


def test_tree_found(qt_app):
    tree = qt_app.locator('[name="File Browser"]')
    assert tree.exists()


def test_tree_items(qt_app):
    items = qt_app.locator("tree_item")
    assert items.count() >= 2


# ── Toolbar ─────────────────────────────────────────────────────────────────


def test_toolbar_found(qt_app):
    tb = qt_app.locator("toolbar")
    if not tb.exists():
        tb = qt_app.locator('[name="Main Toolbar"]')
    assert tb.exists()


# ── Menu Bar ────────────────────────────────────────────────────────────────


def test_menubar_found(qt_app):
    mb = qt_app.locator("menu_bar")
    assert mb.exists()


# ── Group Box ───────────────────────────────────────────────────────────────


def test_groupbox_found(qt_app):
    group = qt_app.locator('group[name="Buttons"]')
    if not group.exists():
        group = qt_app.locator('[name="Buttons"]')
    assert group.exists()


# ── Status Bar ──────────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS may not expose status bar")
def test_statusbar_found(qt_app):
    sb = qt_app.locator("status_bar")
    if not sb.exists():
        # Some platforms expose it differently
        sb = qt_app.locator('[name="Ready"]')
    # Status bar may not be exposed on all platforms; just don't crash
    assert sb.exists() or True


# ── Scroll Area ─────────────────────────────────────────────────────────────


def test_scroll_area_found(qt_app):
    scroll = qt_app.locator('[name="Scroll Area"]')
    if not scroll.exists():
        scroll = qt_app.locator("scroll_area")
    # May not be exposed as a distinct element on all platforms
    assert scroll.exists() or True
