"""xa11y GTK4 integration tests.

Tests the standard widget set against the GTK4 test app via AT-SPI/NSAccessibility.
"""

from __future__ import annotations

import sys
import warnings

import time

import pytest
import xa11y


# ── Helpers ──────────────────────────────────────────────────────────────────


def find(app: xa11y.App, selector: str) -> xa11y.Element:
    return app.locator(selector).element()


def dump_tree(el: xa11y.Element, depth: int = 0, max_depth: int = 20) -> str:
    if depth > max_depth:
        return ""
    indent = "  " * depth
    info = el.role
    if el.name:
        info += f'  name="{el.name}"'
    if el.value:
        info += f'  value="{el.value}"'
    if el.numeric_value is not None:
        info += f"  num={el.numeric_value}"
    if el.checked is not None:
        info += f"  checked={el.checked}"
    if el.enabled is False:
        info += "  DISABLED"
    lines = [indent + info]
    for child in el.children():
        lines.append(dump_tree(child, depth + 1, max_depth))
    return "\n".join(lines)


# ── Diagnostics ───────────────────────────────────────────────────────────────


def test_tree_dump(gtk_app: xa11y.App) -> None:
    lines = [f"application  name=\"{gtk_app.name}\""]
    for child in gtk_app.children():
        lines.append(dump_tree(child, depth=1))
    tree_text = "\n".join(lines)
    warnings.warn(f"\n=== Accessibility Tree ({sys.platform}) ===\n{tree_text}\n=== End Tree ===")


# ── App / window ──────────────────────────────────────────────────────────────


def test_app_pid(gtk_app: xa11y.Element) -> None:
    assert gtk_app.pid is not None
    assert gtk_app.pid > 0



# ── Buttons ───────────────────────────────────────────────────────────────────


def test_ok_button_exists(gtk_app: xa11y.Element) -> None:
    ok = find(gtk_app, 'button[name="OK"]')
    assert ok.role == "button"
    assert ok.name == "OK"
    assert ok.enabled is True


def test_cancel_button_disabled(gtk_app: xa11y.Element) -> None:
    cancel = find(gtk_app, 'button[name="Cancel"]')
    assert cancel.role == "button"
    assert cancel.enabled is False


def test_button_press_enables_cancel(gtk_app: xa11y.Element) -> None:
    # Press OK — Cancel should become enabled.
    # Brief sleep lets the GTK event loop process the click before re-reading state.
    gtk_app.locator('button[name="OK"]').press()
    time.sleep(0.3)
    cancel = find(gtk_app, 'button[name="Cancel"]')
    assert cancel.enabled is True


# ── Checkboxes ────────────────────────────────────────────────────────────────


def test_agree_checkbox_unchecked(gtk_app: xa11y.Element) -> None:
    cb = find(gtk_app, 'check_box[name="Agree to terms"]')
    assert cb.role == "check_box"
    assert cb.checked == "off"


def test_subscribe_checkbox_checked(gtk_app: xa11y.Element) -> None:
    cb = find(gtk_app, 'check_box[name="Subscribe"]')
    assert cb.checked == "on"


@pytest.mark.skip(
    reason=(
        "GTK4 Gtk.CheckButton does not expose any AT-SPI2 Action interface actions "
        "(NActions=0 on Ubuntu 24.04 with GTK 4.14). toggle() requires an AT-SPI2 "
        "action such as 'toggle', 'click', 'activate', or 'check', none of which "
        "GTK4 checkboxes expose. This is a GTK4/AT-SPI2 platform limitation."
    )
)
def test_checkbox_press(gtk_app: xa11y.Element) -> None:
    cb_loc = gtk_app.locator('check_box[name="Agree to terms"]')
    before = cb_loc.element().checked
    cb_loc.press()
    after = cb_loc.element().checked
    assert before != after


# ── Radio buttons ─────────────────────────────────────────────────────────────


def test_radio_a_selected(gtk_app: xa11y.Element) -> None:
    # GTK4 Gtk.CheckButton with set_group() is exposed as check_box in AT-SPI2.
    radio = find(gtk_app, 'check_box[name="Option A"]')
    assert radio.checked == "on"


def test_radio_b_unselected(gtk_app: xa11y.Element) -> None:
    # GTK4 Gtk.CheckButton with set_group() is exposed as check_box in AT-SPI2.
    radio = find(gtk_app, 'check_box[name="Option B"]')
    assert radio.checked == "off"


# ── ComboBox ──────────────────────────────────────────────────────────────────


def test_combobox_found(gtk_app: xa11y.Element) -> None:
    combo = find(gtk_app, "combo_box")
    assert combo.role == "combo_box"


# ── Range controls ────────────────────────────────────────────────────────────
# GTK4's accessibility property APIs (set_accessible_label etc.) are not
# exposed via PyGObject GIR on all distributions, so slider, spin_button,
# and progress_bar are found by role alone (each appears exactly once).


def test_slider_properties(gtk_app: xa11y.Element) -> None:
    slider = find(gtk_app, "slider")
    assert slider.role == "slider"
    assert slider.numeric_value == pytest.approx(50.0)


def test_slider_range(gtk_app: xa11y.Element) -> None:
    slider = find(gtk_app, "slider")
    assert slider.min_value == pytest.approx(0.0)
    assert slider.max_value == pytest.approx(100.0)


def test_slider_increment(gtk_app: xa11y.Element) -> None:
    slider_loc = gtk_app.locator("slider")
    before = slider_loc.element().numeric_value
    slider_loc.increment()
    after = slider_loc.element().numeric_value
    assert after > before


def test_spinbutton_found(gtk_app: xa11y.Element) -> None:
    spin = find(gtk_app, "spin_button")
    assert spin.role == "spin_button"
    assert spin.numeric_value == pytest.approx(42.0)


def test_progress_bar(gtk_app: xa11y.Element) -> None:
    pb = find(gtk_app, "progress_bar")
    assert pb.role == "progress_bar"
    assert pb.numeric_value is not None
    assert pb.numeric_value > 0


# ── Text field ────────────────────────────────────────────────────────────────
# The Entry widget is the only text_field in the app; identified by its value.


def test_textfield_properties(gtk_app: xa11y.Element) -> None:
    # Only one text_field (Gtk.Entry) in the app.
    tf = find(gtk_app, "text_field")
    assert tf.role == "text_field"
    assert tf.value == "hello world"


def test_textfield_set_value(gtk_app: xa11y.Element) -> None:
    # Use a stable role-only locator — value changes after set_value so a
    # value-based selector would fail to re-find the element.
    tf_loc = gtk_app.locator("text_field")
    tf_loc.set_value("new value")
    assert tf_loc.element().value == "new value"
    # Restore
    tf_loc.set_value("hello world")


# ── Text area ─────────────────────────────────────────────────────────────────


def test_textarea_found(gtk_app: xa11y.Element) -> None:
    ta = find(gtk_app, "text_area")
    assert ta.role == "text_area"
    assert "Line 1" in (ta.value or "")


# ── Label ─────────────────────────────────────────────────────────────────────


def test_label_found(gtk_app: xa11y.Element) -> None:
    label = find(gtk_app, 'static_text[name="Heading Text"]')
    assert label.role == "static_text"


# ── List ──────────────────────────────────────────────────────────────────────


def test_switch_role(gtk_app: xa11y.Element) -> None:
    # Gtk.Switch exposes AT-SPI role "toggle button" (62) → must map to 'switch'.
    sw = find(gtk_app, "switch")
    assert sw.role == "switch"


def test_list_found(gtk_app: xa11y.Element) -> None:
    lst = find(gtk_app, "list")
    assert lst.role == "list"


def test_list_has_items(gtk_app: xa11y.Element) -> None:
    lst = find(gtk_app, "list")
    children = lst.children()
    assert len(children) >= 1


# ── Focus action ─────────────────────────────────────────────────────────────


def test_button_focus_action_consistency(gtk_app: xa11y.Element) -> None:
    """Test that if 'focus' is in actions, calling focus() works.

    Regression test for GitHub issue #98: On GTK4, elements may report 'focus'
    in their actions list even when GrabFocus() doesn't work, leading to
    ActionNotSupportedError. Elements should only report 'focus' if focusing
    actually works (i.e., the Action interface exposes a focus action).
    """
    ok = find(gtk_app, 'button[name="OK"]')
    if "focus" in ok.actions:
        # If focus is reported as available, it should work
        gtk_app.locator('button[name="OK"]').focus()
        # Verify focus was actually set
        ok_after = find(gtk_app, 'button[name="OK"]')
        assert ok_after.focused, "focus() should set focused state"
