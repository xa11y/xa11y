"""Integration tests: accessibility events from the GTK4 test app via xa11y.

The Linux AT-SPI2 backend uses a polling strategy that emits two event types:
  - FocusChanged  — when the focused element changes between polls
  - StructureChanged — when the element count in the tree changes

To trigger FocusChanged reliably: focus element A so the polling thread
captures it as the current focused element, then focus element B so the
transition is detected on the next poll.
"""

from __future__ import annotations

import time


def _trigger_focus_change(app) -> None:
    """Focus OK button then spin_button — guaranteed focus transition."""
    app.locator('button[name="OK"]').first().focus()
    time.sleep(0.15)  # one poll cycle (100 ms) so prev_focused is populated
    app.locator("spin_button").first().focus()


def test_subscribe_returns_subscription(gtk_app):
    with gtk_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(gtk_app):
    with gtk_app.subscribe() as sub:
        _trigger_focus_change(gtk_app)
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_event_has_target(gtk_app):
    with gtk_app.subscribe() as sub:
        _trigger_focus_change(gtk_app)
        event = sub.recv(timeout=3.0)
        assert event.target is not None
        assert event.target.role is not None


def test_wait_for_event(gtk_app):
    with gtk_app.subscribe() as sub:
        _trigger_focus_change(gtk_app)
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(gtk_app):
    sub = gtk_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(gtk_app):
    with gtk_app.subscribe() as sub:
        _trigger_focus_change(gtk_app)
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == gtk_app.pid


def test_subscription_drop_then_resubscribe(gtk_app):
    with gtk_app.subscribe():
        pass
    with gtk_app.subscribe() as sub2:
        assert sub2.try_recv() is None
