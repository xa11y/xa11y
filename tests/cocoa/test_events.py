"""Integration tests: accessibility events from the Cocoa/AppKit test app via xa11y.

The macOS AXObserver backend registers real AX notifications and emits events for:
  AXValueChanged, AXFocusedUIElementChanged, AXWindowCreated/Miniaturized,
  AXUIElementDestroyed, AXSelectedTextChanged, AXMenuOpened/Closed, AXTitleChanged.

Note: AXEnabled changes (e.g. enabling a disabled button) do NOT fire AXValueChanged,
so button-press-enables-cancel is not a valid event trigger here.  Use slider
increment (AXValueChanged) or checkbox press (AXValueChanged on checked state)
as reliable triggers instead.
"""

from __future__ import annotations

import sys

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Cocoa/AppKit tests are macOS-only",
)


def test_subscribe_returns_subscription(cocoa_app):
    with cocoa_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(cocoa_app):
    """Text-field focus fires AXFocusedUIElementChanged."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('text_field[name="Search"]').first().focus()
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_value_change_event(cocoa_app):
    """Slider increment fires AXValueChanged."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('slider[name="Volume"]').first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.event_type is not None


def test_toggle_event(cocoa_app):
    """Checkbox press fires AXValueChanged (the checked state is the AXValue)."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('check_box[name="Agree to terms"]').first().press()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.event_type is not None


def test_event_has_target(cocoa_app):
    """AXValueChanged carries the changed element as the event target."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('check_box[name="Agree to terms"]').first().press()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.target is not None
        assert event.target.role is not None


def test_wait_for_event(cocoa_app):
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('spin_button[name="Quantity"]').first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(cocoa_app):
    sub = cocoa_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(cocoa_app):
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('slider[name="Volume"]').first().increment()
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == cocoa_app.pid


def test_subscription_drop_then_resubscribe(cocoa_app):
    with cocoa_app.subscribe():
        pass
    with cocoa_app.subscribe() as sub2:
        assert sub2.try_recv() is None
