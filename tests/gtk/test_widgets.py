"""xa11y GTK4 integration tests.

Tests the standard widget set against the GTK4 test app via AT-SPI/NSAccessibility.
"""

from __future__ import annotations

import sys
import warnings

import pytest
import xa11y


# ── Helpers ──────────────────────────────────────────────────────────────────


def find(app: xa11y.Element, selector: str) -> xa11y.Element:
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


def test_tree_dump(gtk_app: xa11y.Element) -> None:
    tree_text = dump_tree(gtk_app)
    warnings.warn(f"\n=== Accessibility Tree ({sys.platform}) ===\n{tree_text}\n=== End Tree ===")


# ── App / window ──────────────────────────────────────────────────────────────


def test_app_pid(gtk_app: xa11y.Element) -> None:
    assert gtk_app.pid is not None
    assert gtk_app.pid > 0


def test_window_role_and_name(gtk_app: xa11y.Element) -> None:
    win = find(gtk_app, "window")
    assert win.role == "window"
    assert win.name is not None


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
    # Press OK — Cancel should become enabled
    gtk_app.locator('button[name="OK"]').press()
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


def test_checkbox_toggle(gtk_app: xa11y.Element) -> None:
    cb_loc = gtk_app.locator('check_box[name="Agree to terms"]')
    before = cb_loc.element().checked
    cb_loc.toggle()
    after = cb_loc.element().checked
    assert before != after


# ── Radio buttons ─────────────────────────────────────────────────────────────


def test_radio_a_selected(gtk_app: xa11y.Element) -> None:
    radio = find(gtk_app, 'radio_button[name="Option A"]')
    assert radio.checked == "on"


def test_radio_b_unselected(gtk_app: xa11y.Element) -> None:
    radio = find(gtk_app, 'radio_button[name="Option B"]')
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
    tf = find(gtk_app, 'text_field[value="hello world"]')
    assert tf.role == "text_field"
    assert tf.value == "hello world"


def test_textfield_set_value(gtk_app: xa11y.Element) -> None:
    tf_loc = gtk_app.locator('text_field[value="hello world"]')
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


def test_list_found(gtk_app: xa11y.Element) -> None:
    lst = find(gtk_app, "list")
    assert lst.role == "list"


def test_list_has_items(gtk_app: xa11y.Element) -> None:
    lst = find(gtk_app, "list")
    children = lst.children()
    assert len(children) >= 1
