"""Tests for Element action methods: press, focus, set_value, etc.

The 16 actions on `Element` mirror those on `Locator` but operate on the
captured snapshot rather than re-resolving the selector. We use the shared
mock provider (via `_make_test_action_probe`) so we can assert each call
landed on the provider with the right name and argument.
"""

import math

import pytest
import xa11y
from xa11y._native import _make_test_action_probe


# ── Helpers ──────────────────────────────────────────────────────────────────


def _resolve(probe, selector):
    """Resolve a Locator (rooted at the test app) to its first Element."""
    return probe.locator("application").descendant(selector).element()


def _last_action(probe):
    actions = probe.actions()
    assert actions, "expected at least one recorded action"
    return actions[-1]


# ── Basic actions (no arguments) ─────────────────────────────────────────────


def test_element_press():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.press()
    _, name, data = _last_action(probe)
    assert name == "press"
    assert data is None


def test_element_focus():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.focus()
    assert _last_action(probe)[1] == "focus"


def test_element_blur():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.blur()
    assert _last_action(probe)[1] == "blur"


def test_element_toggle():
    probe = _make_test_action_probe()
    cb = _resolve(probe, "check_box")
    probe.clear()
    cb.toggle()
    assert _last_action(probe)[1] == "toggle"


def test_element_select():
    probe = _make_test_action_probe()
    item = _resolve(probe, 'list_item[name="Item 1"]')
    probe.clear()
    item.select()
    assert _last_action(probe)[1] == "select"


def test_element_expand():
    probe = _make_test_action_probe()
    lst = _resolve(probe, "list")
    probe.clear()
    lst.expand()
    assert _last_action(probe)[1] == "expand"


def test_element_collapse():
    probe = _make_test_action_probe()
    lst = _resolve(probe, "list")
    probe.clear()
    lst.collapse()
    assert _last_action(probe)[1] == "collapse"


def test_element_show_menu():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.show_menu()
    assert _last_action(probe)[1] == "show_menu"


def test_element_increment():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    slider.increment()
    assert _last_action(probe)[1] == "increment"


def test_element_decrement():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    slider.decrement()
    assert _last_action(probe)[1] == "decrement"


def test_element_scroll_into_view():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.scroll_into_view()
    assert _last_action(probe)[1] == "scroll_into_view"


# ── Actions with arguments ───────────────────────────────────────────────────


def test_element_set_value():
    probe = _make_test_action_probe()
    field = _resolve(probe, "text_field")
    probe.clear()
    field.set_value("new value")
    _, name, data = _last_action(probe)
    assert name == "set_value"
    assert data == "new value"


def test_element_set_numeric_value():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    slider.set_numeric_value(42.0)
    _, name, data = _last_action(probe)
    assert name == "set_numeric_value"
    assert data == "42"


def test_element_type_text():
    probe = _make_test_action_probe()
    field = _resolve(probe, "text_field")
    probe.clear()
    field.type_text("hello")
    _, name, data = _last_action(probe)
    assert name == "type_text"
    assert data == "hello"


def test_element_select_text():
    probe = _make_test_action_probe()
    field = _resolve(probe, "text_field")
    probe.clear()
    field.select_text(0, 3)
    _, name, data = _last_action(probe)
    assert name == "set_text_selection"
    assert data == "0..3"


def test_element_perform_action():
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.perform_action("press")
    assert _last_action(probe)[1] == "press"


# ── Validation rejection paths (handled in core) ─────────────────────────────


def test_set_numeric_value_rejects_nan():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    with pytest.raises(xa11y.InvalidActionDataError):
        slider.set_numeric_value(math.nan)
    # Validation rejects before the provider is touched.
    assert probe.actions() == []


def test_set_numeric_value_rejects_positive_inf():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    with pytest.raises(xa11y.InvalidActionDataError):
        slider.set_numeric_value(math.inf)
    assert probe.actions() == []


def test_set_numeric_value_rejects_negative_inf():
    probe = _make_test_action_probe()
    slider = _resolve(probe, "slider")
    probe.clear()
    with pytest.raises(xa11y.InvalidActionDataError):
        slider.set_numeric_value(-math.inf)
    assert probe.actions() == []


def test_select_text_rejects_start_after_end():
    probe = _make_test_action_probe()
    field = _resolve(probe, "text_field")
    probe.clear()
    with pytest.raises(xa11y.InvalidActionDataError):
        field.select_text(5, 2)
    assert probe.actions() == []


# ── Snapshot semantics ───────────────────────────────────────────────────────


def test_element_action_uses_captured_snapshot():
    """Element actions act on the captured ElementData, so the same Element
    handle keeps working across multiple calls without re-resolving."""
    probe = _make_test_action_probe()
    btn = _resolve(probe, 'button[name="Back"]')
    probe.clear()
    btn.press()
    btn.focus()
    btn.press()
    names = [entry[1] for entry in probe.actions()]
    assert names == ["press", "focus", "press"]
    # All three reached the same handle (the captured Back button).
    handles = {entry[0] for entry in probe.actions()}
    assert len(handles) == 1
