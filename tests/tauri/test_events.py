"""Integration tests: accessibility events from the Tauri (WebView) test app via xa11y.

On Linux (WebKit2GTK + AT-SPI2), xa11y uses the same polling strategy as for
native GTK apps: only FocusChanged and StructureChanged events are emitted.
To trigger StructureChanged reliably, press the "Add Item" button which appends
a new list option to the DOM — safe and known not to hang (no GrabFocus call).
"""

from __future__ import annotations

import time


def _add_item(app) -> None:
    """Press 'Add Item' to append a list option — triggers StructureChanged."""
    time.sleep(0.15)  # one poll cycle so prev_element_count is populated
    app.locator('button[name="Add Item"]').first().press()


def test_subscribe_returns_subscription(tauri_app):
    with tauri_app.subscribe() as sub:
        assert sub is not None


def test_structure_changed_event(tauri_app):
    with tauri_app.subscribe() as sub:
        _add_item(tauri_app)
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_wait_for_event(tauri_app):
    with tauri_app.subscribe() as sub:
        _add_item(tauri_app)
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(tauri_app):
    sub = tauri_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(tauri_app):
    with tauri_app.subscribe() as sub:
        _add_item(tauri_app)
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == tauri_app.pid


def test_subscription_drop_then_resubscribe(tauri_app):
    with tauri_app.subscribe():
        pass
    with tauri_app.subscribe() as sub2:
        assert sub2.try_recv() is None
