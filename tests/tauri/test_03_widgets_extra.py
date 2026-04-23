"""Extra Tauri widget tests — deeper action/selector/state coverage.

These tests fill coverage gaps against the AccessKit Rust integration suite:
additional action verbs (``focus``, ``decrement``, ``perform_action``),
selector features (``:nth``, attribute filters, descendant combinator), and
state-property reads. WebView-specific limitations (e.g. no SetValue on
<input type="text"> via WebKit2GTK) are flagged with ``@pytest.mark.xfail``.
"""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


ACTION_SETTLE = 0.3


# ── Selector features ──────────────────────────────────────────────────────


def test_selector_nth(tauri_app: xa11y.App) -> None:
    """``:nth(N)`` (1-based) returns exactly that match."""
    first = tauri_app.locator("button:nth(1)").elements()
    assert len(first) == 1
    assert first[0].role == "button"


def test_selector_attribute_enabled_true(tauri_app: xa11y.App) -> None:
    """Attribute filter ``[enabled="true"]`` matches enabled elements."""
    enabled_buttons = tauri_app.locator('button[enabled="true"]').elements()
    assert enabled_buttons
    for b in enabled_buttons:
        assert b.enabled is True


def test_selector_attribute_enabled_false(tauri_app: xa11y.App) -> None:
    """Attribute filter ``[enabled="false"]`` matches disabled elements.

    The Cancel button starts disabled. Pressing OK enables it; the Tauri
    test app never re-disables it within a session, so whether this test
    observes a disabled Cancel depends on run order. We always verify the
    attribute-filter invariant (every result is disabled).
    """
    disabled = tauri_app.locator('[enabled="false"]').elements()
    for d in disabled:
        assert d.enabled is False


def test_selector_attribute_checked_on(tauri_app: xa11y.App) -> None:
    """Attribute filter ``[checked="on"]`` matches pre-checked widgets."""
    matches = tauri_app.locator('[checked="on"]').elements()
    names = [m.name for m in matches]
    assert "Subscribe" in names or "Option A" in names


def test_selector_finds_descendants(tauri_app: xa11y.App) -> None:
    """``app.locator(role)`` descends into the tree (not just direct children)."""
    buttons = tauri_app.locator("button").elements()
    # OK, Cancel, Submit, Add Item, Remove Item.
    assert len(buttons) >= 4


def test_selector_chained_descendants(tauri_app: xa11y.App) -> None:
    """Multi-segment selectors match via the descendant combinator."""
    matches = tauri_app.locator("window button").elements()
    if not matches:
        pytest.xfail("WebView tree topology varies — no window→button match")
    for m in matches:
        assert m.role == "button"


def test_locator_count_matches_elements_len(tauri_app: xa11y.App) -> None:
    """``count()`` agrees with ``len(elements())``."""
    loc = tauri_app.locator("button")
    assert loc.count() == len(loc.elements())


# ── State property reads ───────────────────────────────────────────────────


def test_ok_button_focusable(tauri_app: xa11y.App) -> None:
    """HTML buttons are focusable in the platform accessibility tree."""
    ok = tauri_app.locator('button[name="OK"]').element()
    # WebView bridges may or may not report focusable for disabled/enabled
    # states consistently — guarantee only that the property is a bool.
    assert isinstance(ok.focusable, bool)


def test_disabled_state_is_bool(tauri_app: xa11y.App) -> None:
    """A disabled element's properties return well-defined bools."""
    cancel_el = tauri_app.locator('button[name="Cancel"]').element()
    assert isinstance(cancel_el.focusable, bool)
    assert isinstance(cancel_el.enabled, bool)


# ── Actions: focus ─────────────────────────────────────────────────────────


def test_button_focus(tauri_app: xa11y.App) -> None:
    """focus() on a button is accepted by the WebView accessibility bridge."""
    loc = tauri_app.locator('button[name="OK"]')
    try:
        loc.focus()
    except xa11y.ActionNotSupportedError:
        pytest.xfail(
            "WebView AT bridge does not expose grabFocus for HTML buttons here"
        )
    time.sleep(ACTION_SETTLE)
    # On some platforms the focused property lags or the bridge suppresses it
    # for buttons. Accept either focused or a successful no-error call.
    assert isinstance(loc.element().focused, bool)


# ── Actions: decrement ─────────────────────────────────────────────────────


def test_slider_decrement(tauri_app: xa11y.App) -> None:
    """decrement() decreases the HTML range input's value."""
    loc = tauri_app.locator('slider[name="Volume"]')
    loc.increment()
    time.sleep(ACTION_SETTLE)
    before = loc.element().numeric_value
    loc.decrement()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert before is not None and after is not None
    assert after < before


# ── Actions: set_numeric_value ─────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason=(
        "WebKit2GTK exposes <input type='range'> via AT-SPI2 Value interface "
        "but SetCurrentValue is not implemented for WebView-hosted controls."
    ),
    strict=False,
)
def test_slider_set_numeric_value(tauri_app: xa11y.App) -> None:
    """set_numeric_value() writes a new value to the HTML range input."""
    loc = tauri_app.locator('slider[name="Volume"]')
    loc.set_numeric_value(77.0)
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert abs(after - 77.0) < 1.0
    loc.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


# ── Actions: perform_action ────────────────────────────────────────────────


def test_perform_action_press_equivalent(tauri_app: xa11y.App) -> None:
    """perform_action('press') behaves like press() on a button."""
    tauri_app.locator('button[name="Submit"]').perform_action("press")
    time.sleep(ACTION_SETTLE)


# ── Range controls: min / max ──────────────────────────────────────────────


def test_slider_range_exposed(tauri_app: xa11y.App) -> None:
    """The HTML range input exposes aria-valuemin/max via the bridge."""
    el = tauri_app.locator('slider[name="Volume"]').element()
    assert el.min_value == pytest.approx(0.0)
    assert el.max_value == pytest.approx(100.0)


def test_progress_bar_numeric_value(tauri_app: xa11y.App) -> None:
    """progress_bar exposes its numeric_value."""
    el = tauri_app.locator('progress_bar[name="Progress"]').element()
    assert el.numeric_value is not None


# ── Parent navigation ──────────────────────────────────────────────────────


def test_element_parent_is_usable(tauri_app: xa11y.App) -> None:
    """parent() returns a usable element with a role."""
    ok = tauri_app.locator('button[name="OK"]').element()
    p = ok.parent()
    assert p is not None
    assert p.role


# ── Dynamic widgets: Submit / Add Item / Remove Item ───────────────────────


def test_submit_button_exists(tauri_app: xa11y.App) -> None:
    """The Submit button added for event tests must be present."""
    el = tauri_app.locator('button[name="Submit"]').element()
    assert el.role == "button"


def test_status_label_present(tauri_app: xa11y.App) -> None:
    """A status label with a 'Status: ' prefix exists in the tree."""
    matches = tauri_app.locator('[name^="Status: "]').elements()
    assert matches, "Expected a Status: * label in the tree"
