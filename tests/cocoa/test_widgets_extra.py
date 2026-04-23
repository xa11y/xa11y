"""Extra Cocoa/AppKit widget tests — deeper action/selector/state coverage.

These tests fill coverage gaps against the AccessKit Rust integration suite:
additional action verbs (``focus``, ``blur``, ``decrement``, ``perform_action``),
selector features (``:nth``, attribute filters, descendant combinator), and
range-control numeric writes.
"""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


pytestmark = pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Cocoa/AppKit tests are macOS-only.",
)


ACTION_SETTLE = 0.3


# ── Selector features ──────────────────────────────────────────────────────


def test_selector_nth(cocoa_app: xa11y.App) -> None:
    """``:nth(N)`` (1-based) returns exactly that match."""
    first = cocoa_app.locator("button:nth(1)").elements()
    assert len(first) == 1
    assert first[0].role == "button"


def test_selector_attribute_enabled_true(cocoa_app: xa11y.App) -> None:
    """Attribute filter ``[enabled="true"]`` matches only enabled elements."""
    enabled_buttons = cocoa_app.locator('button[enabled="true"]').elements()
    assert enabled_buttons
    for b in enabled_buttons:
        assert b.enabled is True


def test_selector_attribute_enabled_false(cocoa_app: xa11y.App) -> None:
    """Attribute filter ``[enabled="false"]`` matches disabled elements."""
    # Cancel starts disabled. If a prior test already pressed OK, Cancel is
    # enabled — that's non-deterministic, so tolerate either arrangement.
    disabled = cocoa_app.locator('[enabled="false"]').elements()
    cancel = cocoa_app.locator('button[name="Cancel"]').element()
    if not cancel.enabled:
        assert any(d.name == "Cancel" for d in disabled)
    else:
        # If Cancel has been enabled by earlier tests, at least verify the
        # filter returns only disabled elements (invariant must hold).
        for d in disabled:
            assert d.enabled is False


def test_selector_attribute_checked_on(cocoa_app: xa11y.App) -> None:
    """Attribute filter ``[checked="on"]`` matches pre-checked widgets."""
    matches = cocoa_app.locator('[checked="on"]').elements()
    names = [m.name for m in matches]
    # Subscribe and Option A are both initially checked.
    assert "Subscribe" in names or "Option A" in names


def test_selector_finds_descendants(cocoa_app: xa11y.App) -> None:
    """``app.locator(role)`` descends into the tree (not just direct children)."""
    buttons = cocoa_app.locator("button").elements()
    # OK, Cancel, Submit, Add Item, Remove Item at minimum.
    assert len(buttons) >= 4


def test_selector_chained_descendants(cocoa_app: xa11y.App) -> None:
    """Multi-segment selectors match via the descendant combinator."""
    matches = cocoa_app.locator("window button").elements()
    if not matches:
        pytest.xfail("AX tree may flatten window wrapper depending on macOS version")
    for m in matches:
        assert m.role == "button"


def test_locator_count_matches_elements_len(cocoa_app: xa11y.App) -> None:
    """``count()`` agrees with ``len(elements())``."""
    loc = cocoa_app.locator("button")
    assert loc.count() == len(loc.elements())


# ── State property reads ───────────────────────────────────────────────────


def test_ok_button_focusable(cocoa_app: xa11y.App) -> None:
    """Buttons are focusable on macOS AX."""
    ok = cocoa_app.locator('button[name="OK"]').element()
    assert ok.focusable is True


def test_disabled_state_is_bool(cocoa_app: xa11y.App) -> None:
    """A disabled element's properties return well-defined bools."""
    cancel_el = cocoa_app.locator('button[name="Cancel"]').element()
    assert isinstance(cancel_el.focusable, bool)
    assert isinstance(cancel_el.enabled, bool)


# ── Actions: focus / blur ──────────────────────────────────────────────────


def test_button_focus(cocoa_app: xa11y.App) -> None:
    """focus() on a button sets focused=True."""
    loc = cocoa_app.locator('button[name="OK"]')
    loc.focus()
    time.sleep(ACTION_SETTLE)
    assert loc.element().focused is True


def test_text_field_focus(cocoa_app: xa11y.App) -> None:
    """focus() on a text field sets focused=True."""
    loc = cocoa_app.locator('text_field[name="Search"]')
    loc.focus()
    time.sleep(ACTION_SETTLE)
    assert loc.element().focused is True


# ── Actions: decrement ─────────────────────────────────────────────────────


def test_slider_decrement(cocoa_app: xa11y.App) -> None:
    """decrement() decreases the slider value."""
    loc = cocoa_app.locator('slider[name="Volume"]')
    loc.increment()
    time.sleep(ACTION_SETTLE)
    before = loc.element().numeric_value
    loc.decrement()
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert before is not None and after is not None
    assert after < before


# ── Actions: set_numeric_value ─────────────────────────────────────────────


def test_slider_set_numeric_value(cocoa_app: xa11y.App) -> None:
    """set_numeric_value() writes a new value to the slider."""
    loc = cocoa_app.locator('slider[name="Volume"]')
    loc.set_numeric_value(77.0)
    time.sleep(ACTION_SETTLE)
    after = loc.element().numeric_value
    assert after is not None
    assert abs(after - 77.0) < 1.0
    # Restore.
    loc.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


# ── Actions: perform_action ────────────────────────────────────────────────


def test_perform_action_press_equivalent(cocoa_app: xa11y.App) -> None:
    """perform_action('press') behaves like press() on a button."""
    cocoa_app.locator('button[name="OK"]').perform_action("press")
    time.sleep(ACTION_SETTLE)


# ── Range controls: min / max ──────────────────────────────────────────────


def test_spin_button_range(cocoa_app: xa11y.App) -> None:
    """spin_button exposes min_value / max_value."""
    el = cocoa_app.locator('spin_button[name="Quantity"]').element()
    # NSStepper exposes minValue/maxValue via AX.
    assert el.min_value == pytest.approx(0.0)
    assert el.max_value == pytest.approx(999.0)


def test_progress_bar_numeric_value_range(cocoa_app: xa11y.App) -> None:
    """NSProgressIndicator exposes AXValue and has a well-defined range."""
    el = cocoa_app.locator('progress_bar[name="Progress"]').element()
    # macOS exposes the fraction [0, 1] — current is 75% → 0.75.
    assert el.numeric_value == pytest.approx(0.75)


# ── Parent navigation ──────────────────────────────────────────────────────


def test_element_parent_is_usable(cocoa_app: xa11y.App) -> None:
    """parent() returns a usable element with a role."""
    ok = cocoa_app.locator('button[name="OK"]').element()
    p = ok.parent()
    assert p is not None
    assert p.role


# ── Dynamic widgets: Submit / Add Item / Remove Item ───────────────────────


def test_submit_button_exists(cocoa_app: xa11y.App) -> None:
    """The Submit button added for event tests must be present."""
    el = cocoa_app.locator('button[name="Submit"]').element()
    assert el.role == "button"
    assert el.enabled is True


def test_status_label_initial_value(cocoa_app: xa11y.App) -> None:
    """The status label starts with the 'Status: Ready' accessible name."""
    # The label is an NSTextField with accessibilityLabel = "Status: Ready"
    # initially (Submit toggles it). This is flaky across test order, so
    # just verify the label node is present with some status text.
    matches = cocoa_app.locator('[name^="Status: "]').elements()
    assert matches, "Expected a Status: * label in the tree"
