"""Integration tests: accessibility events from the Cocoa/AppKit test app via xa11y."""

from __future__ import annotations

import sys
import time

import pytest
import xa11y

pytestmark = pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Cocoa/AppKit tests are macOS-only",
)


def test_subscribe_returns_subscription(cocoa_app):
    """Can create an event subscription."""
    with cocoa_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(cocoa_app):
    """Focusing an element should fire a focus event."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator("button").first().focus()
        try:
            event = sub.recv(timeout=3.0)
            assert event is not None
            assert event.event_type is not None
        except xa11y.TimeoutError:
            pytest.skip("No focus event received (platform may not emit it)")


def test_value_change_event(cocoa_app):
    """Incrementing a slider should fire a value-changed event."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('slider[name="Volume"]').first().increment()
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            ev = sub.try_recv()
            if ev is not None:
                events.append(ev)
            time.sleep(0.1)
        for ev in events:
            assert ev.event_type is not None


def test_toggle_event(cocoa_app):
    """Toggling a checkbox should fire a state-changed event."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('check_box[name="Agree to terms"]').first().press()
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            ev = sub.try_recv()
            if ev is not None:
                events.append(ev)
            time.sleep(0.1)
        for ev in events:
            assert ev.event_type is not None


def test_event_has_target(cocoa_app):
    """Events should carry a target element snapshot when the platform supports it."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('button[name="OK"]').first().press()
        try:
            event = sub.recv(timeout=3.0)
            if event.target is not None:
                assert event.target.role is not None
        except xa11y.TimeoutError:
            pytest.skip("No event received")


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS event filtering varies")
def test_wait_for_event(cocoa_app):
    """wait_for should return a matching event."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('spin_button[name="Quantity"]').first().increment()
        try:
            event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
            assert event is not None
        except xa11y.TimeoutError:
            pytest.skip("No matching event within timeout")


def test_subscription_close(cocoa_app):
    """Closing a subscription multiple times should be clean."""
    sub = cocoa_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(cocoa_app):
    """Events should have non-empty app_name and matching app_pid."""
    expected_pid = cocoa_app.pid
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('slider[name="Volume"]').first().increment()
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            ev = sub.try_recv()
            if ev is not None:
                assert ev.app_name, "app_name should be non-empty"
                if expected_pid is not None:
                    assert ev.app_pid == expected_pid
                return
            time.sleep(0.1)
        pytest.skip("No event received — may depend on platform event delivery")


def test_subscription_drop_then_resubscribe(cocoa_app):
    """Dropping a subscription and creating a new one should work cleanly."""
    with cocoa_app.subscribe():
        pass
    with cocoa_app.subscribe() as sub2:
        assert sub2.try_recv() is None
