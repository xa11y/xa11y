"""xa11y Cocoa/AppKit integration tests (macOS only).

Tests the standard widget set against the native AppKit test app via NSAccessibility.
"""

from __future__ import annotations

import sys
import warnings

import pytest
import xa11y

pytestmark = pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Cocoa/AppKit tests are macOS-only",
)


# ── Helpers ───────────────────────────────────────────────────────────────────


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


def test_tree_dump(cocoa_app: xa11y.Element) -> None:
    tree_text = dump_tree(cocoa_app)
    warnings.warn(f"\n=== Accessibility Tree (darwin/cocoa) ===\n{tree_text}\n=== End Tree ===")


# ── App / window ──────────────────────────────────────────────────────────────


def test_app_pid(cocoa_app: xa11y.Element) -> None:
    assert cocoa_app.pid is not None
    assert cocoa_app.pid > 0


def test_window_role_and_name(cocoa_app: xa11y.Element) -> None:
    win = find(cocoa_app, "window")
    assert win.role == "window"
    assert win.name is not None


# ── Buttons ───────────────────────────────────────────────────────────────────


def test_ok_button_properties(cocoa_app: xa11y.Element) -> None:
    ok = find(cocoa_app, 'button[name="OK"]')
    assert ok.role == "button"
    assert ok.name == "OK"
    assert ok.enabled is True
    assert ok.description == "Confirm the dialog"


def test_cancel_button_disabled(cocoa_app: xa11y.Element) -> None:
    cancel = find(cocoa_app, 'button[name="Cancel"]')
    assert cancel.role == "button"
    assert cancel.enabled is False


def test_button_press_enables_cancel(cocoa_app: xa11y.Element) -> None:
    cocoa_app.locator('button[name="OK"]').press()
    cancel = find(cocoa_app, 'button[name="Cancel"]')
    assert cancel.enabled is True


# ── Checkboxes ────────────────────────────────────────────────────────────────


def test_agree_checkbox_unchecked(cocoa_app: xa11y.Element) -> None:
    cb = find(cocoa_app, 'check_box[name="Agree to terms"]')
    assert cb.role == "check_box"
    assert cb.checked == "off"


def test_subscribe_checkbox_checked(cocoa_app: xa11y.Element) -> None:
    cb = find(cocoa_app, 'check_box[name="Subscribe"]')
    assert cb.checked == "on"


def test_checkbox_toggle(cocoa_app: xa11y.Element) -> None:
    cb_loc = cocoa_app.locator('check_box[name="Agree to terms"]')
    before = cb_loc.element().checked
    cb_loc.toggle()
    after = cb_loc.element().checked
    assert before != after


# ── Radio buttons ─────────────────────────────────────────────────────────────


def test_radio_a_selected(cocoa_app: xa11y.Element) -> None:
    radio = find(cocoa_app, 'radio_button[name="Option A"]')
    assert radio.checked == "on"


def test_radio_b_unselected(cocoa_app: xa11y.Element) -> None:
    radio = find(cocoa_app, 'radio_button[name="Option B"]')
    assert radio.checked == "off"


# ── ComboBox ──────────────────────────────────────────────────────────────────


def test_combobox_found(cocoa_app: xa11y.Element) -> None:
    combo = find(cocoa_app, 'combo_box[name="Fruit"]')
    assert combo.role == "combo_box"


# ── Range controls ────────────────────────────────────────────────────────────


def test_slider_properties(cocoa_app: xa11y.Element) -> None:
    slider = find(cocoa_app, 'slider[name="Volume"]')
    assert slider.role == "slider"
    assert slider.numeric_value == pytest.approx(50.0)


def test_slider_range(cocoa_app: xa11y.Element) -> None:
    slider = find(cocoa_app, 'slider[name="Volume"]')
    assert slider.min_value == pytest.approx(0.0)
    assert slider.max_value == pytest.approx(100.0)


def test_slider_increment(cocoa_app: xa11y.Element) -> None:
    slider_loc = cocoa_app.locator('slider[name="Volume"]')
    before = slider_loc.element().numeric_value
    slider_loc.increment()
    after = slider_loc.element().numeric_value
    assert after > before


def test_spinbutton_found(cocoa_app: xa11y.Element) -> None:
    spin = find(cocoa_app, 'spin_button[name="Quantity"]')
    assert spin.role == "spin_button"


def test_progress_bar(cocoa_app: xa11y.Element) -> None:
    pb = find(cocoa_app, 'progress_bar[name="Progress"]')
    assert pb.role == "progress_bar"
    # NSProgressIndicator exposes AXValue as a fraction in [0.0, 1.0]
    assert pb.numeric_value == pytest.approx(0.75)


# ── Text field ────────────────────────────────────────────────────────────────


def test_textfield_properties(cocoa_app: xa11y.Element) -> None:
    tf = find(cocoa_app, 'text_field[name="Search"]')
    assert tf.role == "text_field"
    assert tf.value == "hello world"


def test_textfield_set_value(cocoa_app: xa11y.Element) -> None:
    tf_loc = cocoa_app.locator('text_field[name="Search"]')
    tf_loc.set_value("new value")
    assert tf_loc.element().value == "new value"
    tf_loc.set_value("hello world")


# ── Text area ─────────────────────────────────────────────────────────────────


def test_textarea_found(cocoa_app: xa11y.Element) -> None:
    ta = find(cocoa_app, 'text_area[name="Notes"]')
    assert ta.role == "text_area"
    assert "Line 1" in (ta.value or "")


# ── Label ─────────────────────────────────────────────────────────────────────


def test_label_found(cocoa_app: xa11y.Element) -> None:
    label = find(cocoa_app, 'static_text[name="Heading Text"]')
    assert label.role == "static_text"


# ── List ──────────────────────────────────────────────────────────────────────


def test_list_has_items(cocoa_app: xa11y.Element) -> None:
    # NSTableView (used for list views) is exposed as AXTable, not AXList
    lst = find(cocoa_app, 'table[name="Items"]')
    assert len(lst.children()) >= 1
