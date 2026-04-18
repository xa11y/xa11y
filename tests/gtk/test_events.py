"""Integration tests: accessibility events from the GTK4 test app via xa11y.

The Linux AT-SPI2 backend uses a polling strategy that emits two event types:
  - FocusChanged  — when the focused element changes between polls
  - StructureChanged — when the element count in the tree changes

To trigger StructureChanged reliably: press the "Add Item" button (known-safe,
no D-Bus GrabFocus hang) and wait one poll cycle. The first poll establishes
prev_element_count > 0; the second detects the new element and fires the event.
"""

from __future__ import annotations

import time


def _add_item(app) -> None:
    """Press 'Add Item' to append a list row — triggers StructureChanged."""
    time.sleep(0.15)  # one poll cycle so prev_element_count is populated
    app.locator('button[name="Add Item"]').first().press()


def test_subscribe_returns_subscription(gtk_app):
    with gtk_app.subscribe() as sub:
        assert sub is not None


def test_structure_changed_event(gtk_app):
    with gtk_app.subscribe() as sub:
        _add_item(gtk_app)
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_wait_for_event(gtk_app):
    with gtk_app.subscribe() as sub:
        _add_item(gtk_app)
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(gtk_app):
    sub = gtk_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(gtk_app):
    with gtk_app.subscribe() as sub:
        _add_item(gtk_app)
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == gtk_app.pid


def test_subscription_drop_then_resubscribe(gtk_app):
    with gtk_app.subscribe():
        pass
    with gtk_app.subscribe() as sub2:
        assert sub2.try_recv() is None
