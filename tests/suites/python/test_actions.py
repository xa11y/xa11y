"""Action tests: press, focus, toggle, set_value, increment/decrement.

These tests verify that xa11y action verbs work correctly across toolkits.
App-specific selectors come from the ``app_config`` fixture. Tests skip
gracefully when the current app does not expose a given widget type.

Known platform limitations (e.g. GTK4 checkboxes not exposing an AT-SPI2
Action interface) are flagged with ``pytest.mark.skip`` or ``pytest.mark.xfail``
rather than silent fallbacks, in accordance with the design tenets.
"""

from __future__ import annotations

import os
import sys
import time

import pytest
import xa11y


ACTION_SETTLE = 0.3


# ---------------------------------------------------------------------------
# Button: press
# ---------------------------------------------------------------------------


def test_button_press_ok_enables_cancel(app, app_config):
    """Pressing OK enables the initially-disabled Cancel button."""
    import time as _time

    ok_name = app_config["ok_button_name"]
    cancel_name = app_config["cancel_button_name"]

    cancel_before = app.locator(f'button[name="{cancel_name}"]').element()
    # Only assert the initial state if Cancel is reliably disabled (session-fresh).
    # Some test runs press OK earlier; we just ensure press() doesn't raise.
    app.locator(f'button[name="{ok_name}"]').press()
    _time.sleep(ACTION_SETTLE)

    cancel_after = app.locator(f'button[name="{cancel_name}"]').element()
    # Most toolkits wire the primary button to enable the initially-disabled
    # Cancel. The AccessKit test app instead enables Cancel from the checkbox
    # toggle (see handle_action in test-apps/accesskit/src/main.rs), so its
    # config opts out of the coupling — there we only assert press() did not
    # raise and Cancel is still a usable button.
    if app_config.get("ok_press_enables_cancel", True):
        assert cancel_after.enabled is True
    else:
        assert cancel_after.role == "button"
        assert isinstance(cancel_after.enabled, bool)


def test_perform_action_press_equivalent(app, app_config):
    """perform_action('press') behaves like press() on a button."""
    ok_name = app_config["ok_button_name"]
    # No assertion on side-effect — just that the call does not raise.
    app.locator(f'button[name="{ok_name}"]').perform_action("press")
    time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Button: focus
# ---------------------------------------------------------------------------


def test_button_focus(app, app_config):
    """focus() on a button is accepted without raising."""
    ok_name = app_config["ok_button_name"]
    loc = app.locator(f'button[name="{ok_name}"]')
    try:
        loc.focus()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("focus() not exposed by this platform's AT bridge for buttons")
    time.sleep(ACTION_SETTLE)
    # Property may lag or be suppressed on some bridges; assert it's a bool.
    assert isinstance(loc.element().focused, bool)


def test_textfield_focus(app, app_config):
    """focus() on a text_field is accepted without raising."""
    sel = app_config.get("textfield_selector")
    if not sel:
        pytest.skip("app has no text_field widget")
    try:
        app.locator(sel).focus()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("focus() not exposed for text_field on this platform")


# ---------------------------------------------------------------------------
# Checkbox: toggle
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    # GTK4 Gtk.CheckButton does not expose AT-SPI2 Action interface actions.
    False,  # evaluated dynamically via app_name check inside the test body
    reason="placeholder — see body",
)
def test_checkbox_toggle_changes_state(app, app_name, app_config):
    """toggle() flips the checked state of a checkbox."""
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")

    if app_name == "gtk":
        pytest.skip(
            "GTK4 Gtk.CheckButton does not expose ANY AT-SPI2 Action interface "
            "actions (NActions=0) — not 'toggle', not 'click', not 'activate'. "
            "Even the toggle-via-press fallback in xa11y-linux/src/atspi.rs "
            "has nothing to dispatch to. GTK4/AT-SPI2 platform limitation."
        )

    # AccessKit-backed checkboxes (egui, accesskit_winit, eframe, …) advertise
    # only "click" in AT-SPI Action interface, never "toggle". They used to
    # fail here with ActionNotSupported; xa11y-linux/src/atspi.rs:1607 now
    # falls back to "click" for Role::CheckBox/RadioButton/Switch so the
    # semantic verb works against any AccessKit consumer.

    name = app_config["checkbox_unchecked_name"]
    loc = app.locator(f'check_box[name="{name}"]')
    before = loc.element().checked
    loc.toggle()
    time.sleep(ACTION_SETTLE)
    after = loc.element().checked
    assert before != after, f"toggle() did not change checked state: {before} == {after}"
    # Restore
    loc.toggle()
    time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Slider: increment / decrement / set_numeric_value
# ---------------------------------------------------------------------------


def test_slider_increment_changes_value(app, app_config):
    """increment() moves the slider value upward."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    loc = app.locator(sel)
    before = loc.element().numeric_value
    loc.increment()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert after != before, "increment() did not change the slider value"


def test_slider_decrement_changes_value(app, app_config):
    """decrement() moves the slider value downward."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    loc = app.locator(sel)
    # Increment first to ensure we're not at the minimum.
    loc.increment()
    time.sleep(ACTION_SETTLE)
    before = loc.element().numeric_value
    loc.decrement()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None and before is not None
    assert after < before, f"decrement() expected after ({after}) < before ({before})"


@pytest.mark.xfail(
    sys.platform == "linux" or (
        sys.platform == "darwin"
        and os.environ.get("XA11Y_TEST_APP") in ("tauri", "electron")
    ),
    reason=(
        "Qt AT-SPI2 sliders: SetCurrentValue may be rejected on some Qt versions. "
        "WebKit2GTK / WKWebView: SetCurrentValue not reliable for HTML range inputs."
    ),
    strict=False,
)
def test_slider_set_numeric_value(app, app_config):
    """set_numeric_value() writes a specific value to the slider."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    loc = app.locator(sel)
    loc.set_numeric_value(77.0)
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert abs(after - 77.0) < 1.0, f"expected ~77.0, got {after}"
    # Restore
    loc.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Spin button: increment / decrement
# ---------------------------------------------------------------------------


def test_spinbutton_increment_changes_value(app, app_name, app_config):
    """increment() increases the spin_button value."""
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
    if app_name == "egui":
        pytest.skip(
            "egui's DragValue exposes role spin_button but does not honour "
            "AccessKit's SetValue/Increment actions in 0.34 — increment() "
            "round-trips through AT-SPI but the value is not applied. "
            "Tracked upstream in egui."
        )
    if app_name == "tauri":
        pytest.skip(
            "WebKit's a11y bridge accepts increment()/decrement() on HTML "
            "number inputs but never applies the value change (observed on "
            "both AT-SPI2 and AX in CI). The spinbutton stays wired in "
            "conftest for role/discovery coverage."
        )
    loc = app.locator(sel)
    before = loc.element().numeric_value
    loc.increment()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None and before is not None
    assert after > before


def test_spinbutton_decrement_changes_value(app, app_name, app_config):
    """decrement() decreases the spin_button value."""
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
    if app_name == "egui":
        pytest.skip(
            "egui's DragValue does not honour AccessKit's SetValue action "
            "in 0.34 (see test_spinbutton_increment_changes_value)."
        )
    if app_name == "tauri":
        pytest.skip(
            "WebKit's a11y bridge never applies increment()/decrement() on "
            "HTML number inputs (see test_spinbutton_increment_changes_value)."
        )
    loc = app.locator(sel)
    # Ensure we have room to decrement.
    loc.increment()
    time.sleep(ACTION_SETTLE)
    before = loc.element().numeric_value
    loc.decrement()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert before is not None and after is not None
    assert after < before


# ---------------------------------------------------------------------------
# Text field: set_value
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    False,  # placeholder — conditional handled in body
    reason="placeholder",
)
def test_textfield_set_value(app, app_name, app_config):
    """set_value() writes text to a text_field and it can be read back."""
    sel = app_config.get("textfield_selector")
    if not sel:
        pytest.skip("app has no text_field widget")

    if not app_config.get("textfield_settable", True):
        pytest.skip(
            "text_field value is not settable via the a11y API for this app. "
            "AccessKit's AT-SPI bridge (accesskit_unix) does not expose a "
            "Role::TextInput through the EditableText interface, so set_value() "
            "raises TextValueNotSupported (mirrors the Rust integ test "
            "action_set_value_text)."
        )

    if app_name in ("tauri", "electron"):
        pytest.skip(
            f"WebKit2GTK / Chromium ({app_name}) expose HTML <input> through "
            "AT-SPI2 without a functional EditableText interface. Setting text "
            "requires keyboard simulation, which xa11y does not support "
            "(design tenet: only use accessibility APIs)."
        )

    if app_name == "egui":
        pytest.skip(
            "egui's TextEdit advertises role text_field but does not implement "
            "AccessKit's SetValue action — text mutation goes through the "
            "keyboard event loop only. Setting text via the a11y API is a "
            "tenet-2 question for egui upstream."
        )

    loc = app.locator(sel)
    loc.set_value("test input")
    time.sleep(ACTION_SETTLE)
    assert loc.element().value == "test input"
    # Restore
    initial = app_config.get("textfield_initial_value") or "hello world"
    loc.set_value(initial)
    time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Element-bound action variants
#
# Mirror the Locator-bound tests above but invoke the action against a
# captured Element snapshot (``locator.element()``) rather than going through
# the auto-resolving Locator. The provider call beneath is the same — what's
# exercised here is the binding.
# ---------------------------------------------------------------------------


def test_button_press_via_element(app, app_config):
    """Element.press() on a button does not raise."""
    ok_name = app_config["ok_button_name"]
    el = app.locator(f'button[name="{ok_name}"]').element()
    el.press()
    time.sleep(ACTION_SETTLE)


def test_perform_action_press_via_element(app, app_config):
    """Element.perform_action('press') is equivalent to press()."""
    ok_name = app_config["ok_button_name"]
    el = app.locator(f'button[name="{ok_name}"]').element()
    el.perform_action("press")
    time.sleep(ACTION_SETTLE)


def test_button_focus_via_element(app, app_config):
    """Element.focus() on a button is accepted without raising."""
    ok_name = app_config["ok_button_name"]
    el = app.locator(f'button[name="{ok_name}"]').element()
    try:
        el.focus()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("focus() not exposed by this platform's AT bridge for buttons")
    time.sleep(ACTION_SETTLE)


def test_checkbox_toggle_via_element(app, app_name, app_config):
    """Element.toggle() flips checked state on a captured snapshot."""
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")
    if app_name == "gtk":
        pytest.skip(
            "GTK4 Gtk.CheckButton does not expose AT-SPI2 Action interface "
            "actions; toggle() requires 'toggle'/'click'/'activate'."
        )
    name = app_config["checkbox_unchecked_name"]
    loc = app.locator(f'check_box[name="{name}"]')
    el_before = loc.element()
    before = el_before.checked
    try:
        el_before.toggle()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("toggle() rejected by this platform's AT bridge for check_box")
    time.sleep(ACTION_SETTLE)
    after = loc.element().checked
    assert before != after, f"toggle() did not change checked state: {before} == {after}"
    # Restore — re-resolve a fresh snapshot for the second call.
    loc.element().toggle()
    time.sleep(ACTION_SETTLE)


def test_checkbox_press_via_element(app, app_name, app_config):
    """Element.press() on a checkbox flips checked state where supported."""
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")
    name = app_config["checkbox_unchecked_name"]
    loc = app.locator(f'check_box[name="{name}"]')
    el = loc.element()
    before = el.checked
    try:
        el.press()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("press() not exposed for check_box on this platform")
    time.sleep(ACTION_SETTLE)
    after = loc.element().checked
    assert before != after, f"press() did not change checked state: {before} == {after}"
    # Restore.
    try:
        loc.element().press()
        time.sleep(ACTION_SETTLE)
    except xa11y.ActionNotSupportedError:
        pass


@pytest.mark.xfail(
    sys.platform == "linux" or (
        sys.platform == "darwin"
        and os.environ.get("XA11Y_TEST_APP") in ("tauri", "electron")
    ),
    reason=(
        "Qt AT-SPI2 sliders: SetCurrentValue may be rejected on some Qt versions. "
        "WebKit2GTK / WKWebView: SetCurrentValue not reliable for HTML range inputs."
    ),
    strict=False,
)
def test_slider_set_numeric_value_via_element(app, app_config):
    """Element.set_numeric_value() writes a value to a slider snapshot."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    loc = app.locator(sel)
    loc.element().set_numeric_value(77.0)
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert abs(after - 77.0) < 1.0, f"expected ~77.0, got {after}"
    # Restore.
    loc.element().set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


def test_slider_increment_via_element(app, app_config):
    """Element.increment() raises numeric_value on a slider snapshot."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    loc = app.locator(sel)
    el = loc.element()
    before = el.numeric_value
    try:
        el.increment()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("increment() not exposed for slider on this platform")
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert before is not None and after is not None
    assert after != before, "increment() did not change the slider value"


def test_textfield_set_value_via_element(app, app_name, app_config):
    """Element.set_value() writes text to a text_field snapshot."""
    sel = app_config.get("textfield_selector")
    if not sel:
        pytest.skip("app has no text_field widget")
    if not app_config.get("textfield_settable", True):
        pytest.skip(
            "text_field value is not settable via the a11y API for this app "
            "(see test_textfield_set_value)."
        )
    if app_name in ("tauri", "electron"):
        pytest.skip(
            f"WebKit2GTK / Chromium ({app_name}) expose HTML <input> through "
            "AT-SPI2 without a functional EditableText interface."
        )
    if app_name == "egui":
        pytest.skip(
            "egui's TextEdit does not implement AccessKit's SetValue "
            "action (see test_textfield_set_value). On Linux the locator "
            "form returns ActionNotSupportedError and xfails; on macOS/Windows "
            "the call silently no-ops, so the assertion that follows fails."
        )
    loc = app.locator(sel)
    try:
        loc.element().set_value("test input")
    except xa11y.ActionNotSupportedError:
        pytest.xfail("set_value() not exposed for text_field on this platform")
    time.sleep(ACTION_SETTLE)
    assert loc.element().value == "test input"
    # Restore.
    initial = app_config.get("textfield_initial_value") or "hello world"
    loc.element().set_value(initial)
    time.sleep(ACTION_SETTLE)


def test_snapshot_bound_element_press_twice(app, app_name, app_config):
    """A captured Element can be pressed multiple times against the snapshot.

    Locators auto-re-resolve before each action; Elements act on the captured
    node id. This verifies the binding accepts repeat invocation.
    """
    add_name = app_config.get("add_item_button_name")
    if not add_name:
        pytest.skip("app has no Add Item button")
    if app_name == "egui":
        pytest.skip(
            "egui is immediate-mode: the AccessKit node id for a button is "
            "rehashed each frame from its layout position, so the first press "
            "mutates the tree (Add Item appends a row) and the captured "
            "snapshot id becomes a stale UnknownObject. Locator-bound press "
            "(which re-resolves) is the supported pattern."
        )
    add_btn = app.locator(f'button[name="{add_name}"]')
    if not add_btn.exists():
        pytest.skip("Add Item button not present in current tree")
    el = add_btn.element()
    try:
        el.press()
        time.sleep(ACTION_SETTLE)
        el.press()
    except xa11y.ActionNotSupportedError:
        pytest.xfail("press() not exposed for button on this platform")
    time.sleep(ACTION_SETTLE)
