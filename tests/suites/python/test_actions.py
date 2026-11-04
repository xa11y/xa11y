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


def test_legacy_default_action_press(app, app_config):
    """``press`` on an MSAA-only element reaches its default action.

    The element has no Invoke/Toggle/SelectionItem/ExpandCollapse pattern,
    only LegacyIAccessible with a default action. ``press`` must be advertised
    and must run that default action, which the app counts in a status label
    (issue #505).
    """
    name = app_config.get("legacy_press_name")
    if not name:
        pytest.skip("app has no element whose only activation is the MSAA default action")
    prefix = app_config["legacy_press_status_prefix"]

    tile = app.locator(f'[name="{name}"]')
    element = tile.element()
    # Guard: the tile must have no UIA activation pattern, or the press below
    # would go through Invoke/Toggle/... and never reach DoDefaultAction.
    patterns = element.raw.get("uia_activation_patterns")
    assert patterns == [], (
        f"{name!r} must expose no UIA activation pattern so press reaches "
        f"LegacyIAccessible.DoDefaultAction; it has {patterns!r}"
    )
    actions = element.actions
    assert "press" in actions, f"{name!r} must advertise press; actions were {actions}"

    def count() -> int:
        labels = [
            e.name for e in app.locator(f'[name^="{prefix}"]').elements() if e.name
        ]
        assert len(labels) == 1, f"expected one {prefix!r} label, found {labels}"
        return int(labels[0][len(prefix) :])

    before = count()
    tile.press()
    deadline = time.monotonic() + 3.0
    after = count()
    while after == before and time.monotonic() < deadline:
        time.sleep(0.05)
        after = count()
    assert after == before + 1, (
        f"press on {name!r} should run its default action exactly once; "
        f"activation count went {before} -> {after}"
    )


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
            "tenet-2 question for egui upstream. Documented at "
            "docs/site/src/content/docs/explanation/accessibility-quirks.mdx "
            "under 'egui text fields accept SetValue and ignore it'."
        )

    loc = app.locator(sel)
    loc.set_value("test input")
    time.sleep(ACTION_SETTLE)
    assert loc.element().value == "test input"
    # Restore
    initial = app_config.get("textfield_initial_value") or "hello world"
    loc.set_value(initial)
    time.sleep(ACTION_SETTLE)


@pytest.mark.parametrize(
    "text",
    ["café naïve", "emoji 😀 ok", "日本語"],
    ids=["accented", "non_bmp_emoji", "cjk"],
)
def test_textfield_type_text_non_ascii_round_trips(app, app_name, app_config, text):
    """type_text() with non-ASCII text reads back exactly (issue #493).

    On Linux ``type_text`` is AT-SPI ``EditableText.InsertText``, whose
    ``length`` argument is in the toolkit's own unit: bytes for GTK, UTF-16
    code units for Qt. A wrong unit pads the text with NULs or splits a
    surrogate pair, so the read-back differs from what was written.
    """
    if not sys.platform.startswith("linux") or app_name not in ("qt", "gtk"):
        pytest.skip("InsertText length units are an AT-SPI toolkit concern (Qt, GTK)")
    sel = app_config.get("textfield_selector")
    if not sel:
        pytest.skip("app has no text_field widget")
    loc = app.locator(sel)
    try:
        loc.set_value("")
        time.sleep(ACTION_SETTLE)
        loc.type_text(text)
        time.sleep(ACTION_SETTLE)
        assert loc.element().value == text
    finally:
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
            "action (see test_textfield_set_value and the accessibility-quirks "
            "page it cites). On Linux the locator "
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


# ---------------------------------------------------------------------------
# press → AXOpen (macOS)
# ---------------------------------------------------------------------------


def test_press_dispatches_to_axopen_when_it_is_the_only_activation(app, app_name):
    """An element exposing only AXOpen advertises press, and press fires AXOpen.

    The Cocoa fixture's "Open Target" lists AXOpen and no AXPress (the Finder
    sidebar shape) and counts each AXOpen in its value (issue #517).
    """
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the AXOpen-only fixture belongs to the macOS Cocoa cell")

    target = app.locator('[name="Open Target"]')
    el = target.element()
    assert "press" in el.actions, f"press not advertised; actions={el.actions!r}"
    assert "open" in el.actions, f"native open not listed; actions={el.actions!r}"
    before = int((el.value or "").removeprefix("opened "))

    el.press()

    deadline = time.monotonic() + 2.0
    value = el.value
    while time.monotonic() < deadline:
        value = target.element().value
        if value == f"opened {before + 1}":
            break
        time.sleep(0.05)
    assert value == f"opened {before + 1}", f"AXOpen never arrived; value={value!r}"


def test_action_on_a_destroyed_widget_is_stale(app):
    """Acting on a handle whose widget was destroyed fails as stale (#491).

    GTK 3 keeps a destroyed widget's accessible answering for ~15-20 s and
    reports ``DoAction`` as performed while doing nothing, so a press on a
    handle read just before a re-render claimed success. The GTK app's
    "Rebuild" button swaps itself for a fresh one on every click; the old
    handle must then raise the stale-element error, and a locator must
    re-resolve to the new widget.
    """
    locator = app.locator('button[name="Rebuild"]')
    if locator.count() == 0:
        pytest.skip("this app has no self-replacing Rebuild button")
    old = locator.element()
    before = old.description
    old.press()
    deadline = time.monotonic() + 5.0
    while locator.element().description == before:
        assert time.monotonic() < deadline, "the Rebuild button never swapped"
        time.sleep(0.1)
    with pytest.raises(xa11y.SelectorNotMatchedError, match="stale"):
        old.press()
    locator.press()


# ---------------------------------------------------------------------------
# wait_stable (#530): a widget that moves
# ---------------------------------------------------------------------------

_START_SLIDE = 'button[name="Start slide"]'
_SLIDE_TARGET = 'button[name="Slide target"]'


def _slide_app(app, app_config):
    if not app_config.get("has_slide_widget"):
        pytest.skip("this app has no sliding widget")
    # Required where the config claims it: a skip on a missing widget would
    # pass just as well when the widget, or the selector, broke (#383).
    assert app.locator(_START_SLIDE).count() == 1, (
        "the app's Start slide button is missing"
    )
    target = app.locator(_SLIDE_TARGET)
    rest = target.wait_stable(timeout=10).bounds
    assert rest is not None, "the slide target reports no bounds"
    return target, rest


def test_wait_stable_returns_where_a_slide_ends(app, app_config):
    """``wait_stable`` returns the bounds the widget comes to rest at.

    "Start slide" restarts a three-second slide that ends where the target
    already rests, so the bounds a correct wait returns equal the resting
    ones, and the wait cannot finish in much under the slide's length. The
    1.5 s floor leaves the rest for press latency.
    """
    target, rest = _slide_app(app, app_config)
    app.locator(_START_SLIDE).press()
    started = time.monotonic()
    settled = target.wait_stable(timeout=10)
    waited = time.monotonic() - started
    assert settled.bounds == rest, (
        f"wait_stable returned mid-slide after {waited:.2f}s: "
        f"{settled.bounds} vs rest {rest}"
    )
    assert waited >= 1.5, f"a 3 s slide cannot be proven stable in {waited:.2f}s"


def test_wait_stable_timeout_names_the_moving_bounds(app, app_config):
    """A slide outlasting the timeout fails with both rectangles named."""
    target, _ = _slide_app(app, app_config)
    app.locator(_START_SLIDE).press()
    try:
        with pytest.raises(xa11y.TimeoutError) as info:
            # 1.5 s leaves room for several polls where a resolve is slow
            # (UIA), and still ends inside the 3 s slide.
            target.wait_stable(timeout=1.5)
        err = info.value
        assert "two consecutive polls" in err.condition, err.condition
        assert err.last_observed.startswith("bounds still changing:"), (
            err.last_observed
        )
    finally:
        # Leave the target at rest for whatever test runs next.
        target.wait_stable(timeout=10)
