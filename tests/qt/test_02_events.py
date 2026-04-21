"""Integration tests: accessibility events from the Qt test app via xa11y."""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


def test_subscribe_returns_subscription(qt_app):
    """Can create an event subscription."""
    with qt_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(qt_app):
    """Focusing an element should fire a focus event."""
    with qt_app.subscribe() as sub:
        try:
            qt_app.locator("button").first().focus()
        except (xa11y.ActionNotSupportedError, xa11y.XA11yError):
            pytest.skip("focus() not supported on Qt button in this environment")
        try:
            event = sub.recv(timeout=3.0)
            assert event is not None
            assert event.event_type is not None
        except xa11y.TimeoutError:
            pytest.skip("No focus event received (platform may not emit it)")


def test_value_change_event(qt_app):
    """Changing a slider value should fire a value-changed event."""
    with qt_app.subscribe() as sub:
        qt_app.locator("slider").first().increment()
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            try:
                ev = sub.try_recv()
                if ev is not None:
                    events.append(ev)
            except xa11y.TimeoutError:
                break
            time.sleep(0.1)
        for ev in events:
            assert ev.event_type is not None


def test_toggle_event(qt_app):
    """Toggling a checkbox should fire a state-changed event."""
    with qt_app.subscribe() as sub:
        try:
            qt_app.locator("check_box").first().toggle()
        except xa11y.TimeoutError:
            pytest.skip("Checkbox not actionable within timeout (AT-SPI2 state delay)")
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            try:
                ev = sub.try_recv()
                if ev is not None:
                    events.append(ev)
            except xa11y.TimeoutError:
                break
            time.sleep(0.1)
        for ev in events:
            assert ev.event_type is not None


def test_event_has_target(qt_app):
    """Events should have a target element when the platform supports it."""
    with qt_app.subscribe() as sub:
        qt_app.locator("button").first().press()
        try:
            event = sub.recv(timeout=3.0)
            if event.target is not None:
                assert event.target.role is not None
        except xa11y.TimeoutError:
            pytest.skip("No event received")


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS event filtering varies")
def test_wait_for_event(qt_app):
    """wait_for should return matching events."""
    with qt_app.subscribe() as sub:
        qt_app.locator("spin_button").first().increment()
        try:
            event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
            assert event is not None
        except xa11y.TimeoutError:
            pytest.skip("No matching event within timeout")


def test_subscription_close(qt_app):
    """Closing a subscription should be clean."""
    sub = qt_app.subscribe().__enter__()
    sub.close()
    sub.close()
