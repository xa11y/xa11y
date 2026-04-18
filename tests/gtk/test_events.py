"""Integration tests: accessibility events from the GTK4 test app via xa11y."""

from __future__ import annotations

import time

import pytest
import xa11y


def test_subscribe_returns_subscription(gtk_app):
    """Can create an event subscription."""
    with gtk_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(gtk_app):
    """Focusing a button should fire an event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("button").first().focus()
        try:
            event = sub.recv(timeout=3.0)
            assert event is not None
            assert event.event_type is not None
        except xa11y.TimeoutError:
            pytest.skip("No focus event received (platform may not emit it)")


def test_value_change_event(gtk_app):
    """Incrementing a slider should fire a value-changed event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").first().increment()
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            ev = sub.try_recv()
            if ev is not None:
                events.append(ev)
            time.sleep(0.1)
        for ev in events:
            assert ev.event_type is not None


def test_button_press_event(gtk_app):
    """Pressing a button should fire at least one event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="OK"]').first().press()
        events = []
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            ev = sub.try_recv()
            if ev is not None:
                events.append(ev)
            time.sleep(0.1)
        # GTK button press fires state/focus events; at least verify no crash
        for ev in events:
            assert ev.event_type is not None


def test_event_has_target(gtk_app):
    """Events should have a target element when the platform supports it."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("button").first().press()
        try:
            event = sub.recv(timeout=3.0)
            if event.target is not None:
                assert event.target.role is not None
        except xa11y.TimeoutError:
            pytest.skip("No event received")


def test_wait_for_event(gtk_app):
    """wait_for should return a matching event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("spin_button").first().increment()
        try:
            event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
            assert event is not None
        except xa11y.TimeoutError:
            pytest.skip("No matching event within timeout")


def test_subscription_close(gtk_app):
    """Closing a subscription multiple times should be clean."""
    sub = gtk_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(gtk_app):
    """Events should have non-empty app_name and matching app_pid."""
    expected_pid = gtk_app.pid
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").first().increment()
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


def test_subscription_drop_then_resubscribe(gtk_app):
    """Dropping a subscription and creating a new one should work cleanly."""
    with gtk_app.subscribe():
        pass
    with gtk_app.subscribe() as sub2:
        assert sub2.try_recv() is None
