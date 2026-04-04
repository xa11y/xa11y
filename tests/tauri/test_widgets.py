"""xa11y Tauri integration tests.

Tests the standard widget set against the Tauri (WebView-based) test app.
Note: web content is exposed through the platform's WebView accessibility bridge,
so roles may differ from native apps — e.g. checkboxes appear as check_box,
text inputs as text_field, but containers may add extra nesting layers.
"""

from __future__ import annotations

import warnings

import pytest
import xa11y


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


def test_tree_dump(tauri_app: xa11y.Element) -> None:
    tree_text = dump_tree(tauri_app)
    warnings.warn(f"\n=== Accessibility Tree (tauri) ===\n{tree_text}\n=== End Tree ===")


# ── App / window ──────────────────────────────────────────────────────────────


def test_app_pid(tauri_app: xa11y.Element) -> None:
    assert tauri_app.pid is not None
    assert tauri_app.pid > 0


def test_window_found(tauri_app: xa11y.Element) -> None:
    win = find(tauri_app, "window")
    assert win.role == "window"


# ── Buttons ───────────────────────────────────────────────────────────────────


def test_ok_button_exists(tauri_app: xa11y.Element) -> None:
    ok = find(tauri_app, 'button[name="OK"]')
    assert ok.role == "button"
    assert ok.enabled is True


def test_cancel_button_disabled(tauri_app: xa11y.Element) -> None:
    cancel = find(tauri_app, 'button[name="Cancel"]')
    assert cancel.enabled is False


def test_button_press_enables_cancel(tauri_app: xa11y.Element) -> None:
    tauri_app.locator('button[name="OK"]').press()
    cancel = find(tauri_app, 'button[name="Cancel"]')
    assert cancel.enabled is True


# ── Checkboxes ────────────────────────────────────────────────────────────────


def test_agree_checkbox_unchecked(tauri_app: xa11y.Element) -> None:
    cb = find(tauri_app, 'check_box[name="Agree to terms"]')
    assert cb.checked == "off"


def test_subscribe_checkbox_checked(tauri_app: xa11y.Element) -> None:
    cb = find(tauri_app, 'check_box[name="Subscribe"]')
    assert cb.checked == "on"


def test_checkbox_press(tauri_app: xa11y.Element) -> None:
    cb_loc = tauri_app.locator('check_box[name="Agree to terms"]')
    before = cb_loc.element().checked
    cb_loc.press()
    after = cb_loc.element().checked
    assert before != after


# ── Radio buttons ─────────────────────────────────────────────────────────────


def test_radio_a_selected(tauri_app: xa11y.Element) -> None:
    radio = find(tauri_app, 'radio_button[name="Option A"]')
    assert radio.checked == "on"


def test_radio_b_unselected(tauri_app: xa11y.Element) -> None:
    radio = find(tauri_app, 'radio_button[name="Option B"]')
    assert radio.checked == "off"


# ── ComboBox ──────────────────────────────────────────────────────────────────


def test_combobox_found(tauri_app: xa11y.Element) -> None:
    combo = find(tauri_app, 'combo_box[name="Fruit"]')
    assert combo.role == "combo_box"


# ── Range controls ────────────────────────────────────────────────────────────


def test_slider_properties(tauri_app: xa11y.Element) -> None:
    slider = find(tauri_app, 'slider[name="Volume"]')
    assert slider.role == "slider"
    assert slider.numeric_value == pytest.approx(50.0)


def test_slider_range(tauri_app: xa11y.Element) -> None:
    slider = find(tauri_app, 'slider[name="Volume"]')
    assert slider.min_value == pytest.approx(0.0)
    assert slider.max_value == pytest.approx(100.0)


def test_slider_increment(tauri_app: xa11y.Element) -> None:
    slider_loc = tauri_app.locator('slider[name="Volume"]')
    before = slider_loc.element().numeric_value
    slider_loc.increment()
    after = slider_loc.element().numeric_value
    assert after > before


def test_progress_bar(tauri_app: xa11y.Element) -> None:
    pb = find(tauri_app, 'progress_bar[name="Progress"]')
    assert pb.role == "progress_bar"
    assert pb.numeric_value is not None


# ── Text field ────────────────────────────────────────────────────────────────


def test_textfield_properties(tauri_app: xa11y.Element) -> None:
    tf = find(tauri_app, 'text_field[name="Search"]')
    assert tf.role == "text_field"
    assert tf.value == "hello world"


@pytest.mark.skip(
    reason=(
        "WebKit2GTK exposes HTML <input type='text'> as AT-SPI2 role 78 (Embedded). "
        "This role does not expose a functional EditableText interface — neither "
        "SetTextContents nor InsertText succeeds. Setting text in WebKit-embedded "
        "text fields requires keyboard simulation, which xa11y does not support "
        "(design tenet: only use accessibility APIs, not input simulation)."
    )
)
def test_textfield_set_value(tauri_app: xa11y.Element) -> None:
    tf_loc = tauri_app.locator('text_field[name="Search"]')
    tf_loc.set_value("new value")
    assert tf_loc.element().value == "new value"
    tf_loc.set_value("hello world")


# ── Text area ─────────────────────────────────────────────────────────────────


def test_textarea_found(tauri_app: xa11y.Element) -> None:
    ta = find(tauri_app, 'text_area[name="Notes"]')
    assert ta.role == "text_area"
    assert "Line 1" in (ta.value or "")


# ── List ──────────────────────────────────────────────────────────────────────


def test_list_found(tauri_app: xa11y.Element) -> None:
    lst = find(tauri_app, 'list[name="Items"]')
    assert lst.role == "list"


def test_list_has_items(tauri_app: xa11y.Element) -> None:
    lst = find(tauri_app, 'list[name="Items"]')
    children = lst.children()
    assert len(children) >= 1
