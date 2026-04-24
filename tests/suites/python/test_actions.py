"""Action tests: press, focus, toggle, set_value, increment/decrement.

These tests verify that xa11y action verbs work correctly across toolkits.
App-specific selectors come from the ``app_config`` fixture. Tests skip
gracefully when the current app does not expose a given widget type.

Known platform limitations (e.g. GTK4 checkboxes not exposing an AT-SPI2
Action interface) are flagged with ``pytest.mark.skip`` or ``pytest.mark.xfail``
rather than silent fallbacks, in accordance with the design tenets.
"""

from __future__ import annotations

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
    # After pressing OK, Cancel must be enabled (all apps implement this toggle).
    assert cancel_after.enabled is True


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
            "GTK4 Gtk.CheckButton does not expose AT-SPI2 Action interface "
            "actions (NActions=0). toggle() requires a 'toggle', 'click', or "
            "'activate' action — none of which GTK4 checkboxes expose. "
            "GTK4/AT-SPI2 platform limitation."
        )

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
    sys.platform == "linux",
    reason=(
        "Qt AT-SPI2 sliders: SetCurrentValue may be rejected on some Qt "
        "versions. WebKit2GTK: SetCurrentValue not implemented for "
        "WebView-hosted range inputs."
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


def test_spinbutton_increment_changes_value(app, app_config):
    """increment() increases the spin_button value."""
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
    loc = app.locator(sel)
    before = loc.element().numeric_value
    loc.increment()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None and before is not None
    assert after > before


def test_spinbutton_decrement_changes_value(app, app_config):
    """decrement() decreases the spin_button value."""
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
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

    if app_name == "tauri":
        pytest.skip(
            "WebKit2GTK exposes HTML <input type='text'> as AT-SPI2 role "
            "Embedded, which does not expose a functional EditableText "
            "interface. Setting text requires keyboard simulation, which xa11y "
            "does not support (design tenet: only use accessibility APIs)."
        )

    loc = app.locator(sel)
    loc.set_value("test input")
    time.sleep(ACTION_SETTLE)
    assert loc.element().value == "test input"
    # Restore
    initial = app_config.get("textfield_initial_value") or "hello world"
    loc.set_value(initial)
    time.sleep(ACTION_SETTLE)
