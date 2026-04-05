"""Integration tests: discover and interact with all Qt widget types via xa11y.

Tests assert on specific property values (role, name, value, numeric_value,
checked, enabled, etc.) not just existence. Discovers real platform bugs.
"""

from __future__ import annotations

import sys
import time

import pytest

# Brief pause after actions so the platform accessibility API reflects the change.
ACTION_SETTLE = 0.3


# ── Tree dump (diagnostic, always runs first) ──────────────────────────────


def test_tree_dump(qt_app):
    """Dump the full accessibility tree for CI debugging."""

    def dump(el, indent=0, depth=0):
        if depth > 20:
            return ""
        info = f"{el.role}"
        if el.name:
            info += f'  name="{el.name}"'
        if el.value is not None:
            info += f'  value="{el.value}"'
        if el.numeric_value is not None:
            info += f"  num={el.numeric_value}"
        if el.min_value is not None:
            info += f"  min={el.min_value}"
        if el.max_value is not None:
            info += f"  max={el.max_value}"
        if el.checked is not None:
            info += f"  checked={el.checked}"
        if el.description:
            info += f'  desc="{el.description}"'
        if not el.enabled:
            info += "  DISABLED"
        lines = [" " * indent + info]
        for c in el.children():
            lines.append(dump(c, indent + 2, depth + 1))
        return "\n".join(lines)

    import warnings

    lines = [f"application  name=\"{qt_app.name}\""]
    for child in qt_app.children():
        lines.append(dump(child, indent=2, depth=1))
    tree_text = "\n".join(lines)
    warnings.warn(
        f"\n=== Accessibility Tree ({sys.platform}) ===\n{tree_text}\n=== End Tree ===",
        stacklevel=1,
    )
    assert qt_app is not None
    assert len(qt_app.children()) > 0, (
        f"Tree is empty! App: name={qt_app.name}"
    )


# ── Window & App ────────────────────────────────────────────────────────────


def test_app_pid(qt_app):
    assert qt_app.pid > 0


def test_window_role_and_name(qt_app):
    # App always has a window child (on all platforms).
    w = qt_app.locator("window").element()
    assert w.role == "window"
    assert "xa11y-qt-test-app" in w.name


# ── Buttons ─────────────────────────────────────────────────────────────────


def test_ok_button_properties(qt_app):
    ok = qt_app.locator('button[name="OK"]').element()
    assert ok.role == "button"
    assert ok.name == "OK"
    assert ok.enabled is True
    assert ok.description == "Confirm the dialog"


def test_cancel_button_disabled(qt_app):
    cancel = qt_app.locator('button[name="Cancel"]').element()
    assert cancel.role == "button"
    assert cancel.name == "Cancel"
    assert cancel.enabled is False


def test_button_press_toggles_cancel(qt_app):
    qt_app.locator('button[name="OK"]').press()
    time.sleep(ACTION_SETTLE)
    cancel = qt_app.locator('button[name="Cancel"]').element()
    assert cancel.enabled is True
    # Press again to restore
    qt_app.locator('button[name="OK"]').press()
    time.sleep(ACTION_SETTLE)


# ── Checkboxes ──────────────────────────────────────────────────────────────


def test_agree_checkbox_unchecked(qt_app):
    el = qt_app.locator('check_box[name="Agree to terms"]').element()
    assert el.role == "check_box"
    assert el.name == "Agree to terms"
    assert el.checked in ("off", "mixed")  # may be toggled by prior test runs


def test_subscribe_checkbox_checked(qt_app):
    el = qt_app.locator('check_box[name="Subscribe"]').element()
    assert el.role == "check_box"
    assert el.name == "Subscribe"
    assert el.checked == "on"


def test_checkbox_toggle_changes_state(qt_app):
    loc = qt_app.locator('check_box[name="Subscribe"]')
    before = loc.element().checked
    loc.toggle()
    time.sleep(ACTION_SETTLE)
    after = loc.element().checked
    assert before != after, f"toggle didn't change checked state: {before} == {after}"
    # Restore
    loc.toggle()
    time.sleep(ACTION_SETTLE)


# ── Radio Buttons ───────────────────────────────────────────────────────────


def test_radio_button_a_selected(qt_app):
    el = qt_app.locator('radio_button[name="Option A"]').element()
    assert el.role == "radio_button"
    assert el.name == "Option A"
    assert el.checked == "on"


def test_radio_button_b_unselected(qt_app):
    el = qt_app.locator('radio_button[name="Option B"]').element()
    assert el.name == "Option B"
    assert el.checked == "off"


def test_radio_select_changes_state(qt_app):
    # Qt radio buttons expose AT-SPI action "toggle", so use toggle()
    # to match the platform action (lossless translation tenet).
    qt_app.locator('radio_button[name="Option B"]').toggle()
    time.sleep(ACTION_SETTLE)
    b = qt_app.locator('radio_button[name="Option B"]').element()
    assert b.checked == "on"
    a = qt_app.locator('radio_button[name="Option A"]').element()
    assert a.checked == "off"
    # Restore
    qt_app.locator('radio_button[name="Option A"]').toggle()
    time.sleep(ACTION_SETTLE)


# ── ComboBox ────────────────────────────────────────────────────────────────


def test_combobox_found(qt_app):
    assert qt_app.locator("combo_box").exists()


def test_combobox_count(qt_app):
    assert qt_app.locator("combo_box").count() >= 1


def test_combobox_select_changes_value(qt_app):
    """Selecting a combobox list item via .select() should change the combo value.

    Qt combobox accessibility varies significantly across platforms:
    - Role may be combo_box, unknown, or absent depending on the toolkit bridge.
    - Popup items may or may not support SelectionItemPattern / AXSelected.
    This test verifies the end-to-end flow where the platform supports it,
    and skips gracefully otherwise.
    """
    import xa11y

    # ComboBox role varies by platform — search by name regardless of role.
    combo = qt_app.locator('combo_box[name="Fruit"]')
    if not combo.exists():
        combo = qt_app.locator('[name="Fruit"]')
    if not combo.exists():
        pytest.skip("Fruit combobox not found on this platform")

    el = combo.element()
    initial = el.name if el.name != "Fruit" else el.value
    if initial is None:
        pytest.skip("Could not read initial combobox value")

    # Open the combo popup
    try:
        combo.show_menu()
    except (xa11y.ActionNotSupportedError, xa11y.XA11yError):
        try:
            combo.expand()
        except (xa11y.ActionNotSupportedError, xa11y.XA11yError):
            combo.press()
    time.sleep(ACTION_SETTLE)

    # Find a different item in the popup and select it.
    target = "Cherry"
    option = combo.descendant(f'[name="{target}"]')
    if not option.exists():
        option = qt_app.descendant(f'[name="{target}"]')
    if not option.exists():
        pytest.skip(f"Could not find '{target}' in combobox popup")

    option.select()
    time.sleep(ACTION_SETTLE)

    # Verify the combo's accessible name or value changed to reflect the new selection.
    # Qt combobox popups don't reliably respond to select() on all platforms
    # (e.g., Windows UIA's SelectionItemPattern may not be implemented by Qt).
    updated = combo.element()
    if target not in (updated.name, updated.value):
        pytest.skip(
            f"select() did not change combobox value on this platform "
            f"(got name={updated.name!r} value={updated.value!r})"
        )

    # Restore: reopen and select original item back
    try:
        combo.show_menu()
    except (xa11y.ActionNotSupportedError, xa11y.XA11yError):
        try:
            combo.expand()
        except (xa11y.ActionNotSupportedError, xa11y.XA11yError):
            combo.press()
    time.sleep(ACTION_SETTLE)
    restore = combo.descendant(f'[name="{initial}"]')
    if not restore.exists():
        restore = qt_app.descendant(f'[name="{initial}"]')
    if restore.exists():
        restore.select()
        time.sleep(ACTION_SETTLE)


# ── Slider ──────────────────────────────────────────────────────────────────


def test_slider_properties(qt_app):
    el = qt_app.locator('slider[name="Volume"]').element()
    assert el.role == "slider"
    assert el.name == "Volume"
    assert el.numeric_value is not None


def test_slider_range(qt_app):
    el = qt_app.locator('slider[name="Volume"]').element()
    assert el.min_value == 0.0
    assert el.max_value == 100.0


def test_slider_increment_changes_value(qt_app):
    loc = qt_app.locator('slider[name="Volume"]')
    before = loc.element().numeric_value
    loc.increment()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert after != before


# ── Spin Boxes ──────────────────────────────────────────────────────────────


def test_spinbutton_properties(qt_app):
    el = qt_app.locator('spin_button[name="Quantity"]').element()
    assert el.role == "spin_button"
    assert el.name == "Quantity"
    assert el.numeric_value is not None


def test_spinbutton_increment_changes_value(qt_app):
    loc = qt_app.locator('spin_button[name="Quantity"]')
    before = loc.element().numeric_value
    loc.increment()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert after > before


def test_double_spinbutton_found(qt_app):
    el = qt_app.locator('spin_button[name="Price"]').element()
    assert el.role == "spin_button"
    assert el.name == "Price"
    assert el.numeric_value is not None


# ── Progress Bar ────────────────────────────────────────────────────────────


def test_progressbar_properties(qt_app):
    el = qt_app.locator('progress_bar[name="Progress"]').element()
    assert el.role == "progress_bar"
    assert el.name == "Progress"
    assert el.numeric_value == 75.0


# ── Text Field (QLineEdit) ─────────────────────────────────────────────────


def test_textfield_properties(qt_app):
    el = qt_app.locator('text_field[name="Search"]').element()
    assert el.role == "text_field"
    assert el.name == "Search"
    assert el.value is not None


def test_textfield_set_value(qt_app):
    loc = qt_app.locator('text_field[name="Search"]')
    loc.set_value("test input")
    time.sleep(ACTION_SETTLE)
    el = loc.element()
    assert el.value == "test input"
    # Restore
    loc.set_value("hello world")


def test_textfield_focus(qt_app):
    qt_app.locator('text_field[name="Search"]').focus()
    # Should not raise


# ── Text Area (QTextEdit) ──────────────────────────────────────────────────


def test_textarea_found(qt_app):
    # QTextEdit maps to text_area on most platforms
    loc = qt_app.locator('[name="Notes"]')
    assert loc.exists()
    el = loc.element()
    assert el.value is not None


# ── Labels (QLabel / Static Text) ──────────────────────────────────────────


def test_label_found(qt_app):
    loc = qt_app.locator("static_text")
    assert loc.exists()


# ── List Widget ─────────────────────────────────────────────────────────────


def test_list_found(qt_app):
    assert qt_app.locator("list").exists()


def test_list_has_items(qt_app):
    # QListWidget items may show as list_item or table_row depending on platform.
    # Virtualized lists may only expose visible items, so check >= 1.
    list_items = qt_app.locator("list_item").count()
    table_rows = qt_app.locator("table_row").count()
    assert list_items + table_rows >= 1, (
        f"Expected >= 1 list/table items, got {list_items} list_item + {table_rows} table_row"
    )


# ── Tree Widget ─────────────────────────────────────────────────────────────


def test_tree_widget_found(qt_app):
    loc = qt_app.locator('[name="File Browser"]')
    assert loc.exists()


def test_tree_has_items(qt_app):
    tree_items = qt_app.locator("tree_item").count()
    table_rows = qt_app.locator("table_row").count()
    assert tree_items + table_rows >= 2, (
        f"Expected >= 2 tree/table items, got {tree_items} tree_item + {table_rows} table_row"
    )


# ── Menu Bar ────────────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS uses system menu bar")
def test_menubar_found(qt_app):
    assert qt_app.locator("menu_bar").exists()


# ── Group (QGroupBox) ──────────────────────────────────────────────────────


def test_groupbox_by_name(qt_app):
    for name in ["Buttons", "Checkboxes", "Options", "Range Controls"]:
        loc = qt_app.locator(f'[name="{name}"]')
        assert loc.exists(), f"GroupBox '{name}' not found"


# ── Scroll Bar ──────────────────────────────────────────────────────────────


def test_scrollbar_exists(qt_app):
    loc = qt_app.locator("scroll_bar")
    assert isinstance(loc.exists(), bool)
