"""Integration tests: accessibility events from the Tauri (WebView) test app via xa11y.

On Linux (WebKit2GTK + AT-SPI2), xa11y uses the same polling strategy as for
native GTK apps: only FocusChanged and StructureChanged events are emitted.
To trigger FocusChanged reliably, focus two distinct elements in sequence so
the polling thread captures the transition.
"""

from __future__ import annotations

import time


def _trigger_focus_change(app) -> None:
    """Focus OK button then slider — guaranteed focus transition."""
    app.locator('button[name="OK"]').first().focus()
    time.sleep(0.15)  # one poll cycle (100 ms) so prev_focused is populated
    app.locator('slider[name="Volume"]').first().focus()


def test_subscribe_returns_subscription(tauri_app):
    with tauri_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(tauri_app):
    with tauri_app.subscribe() as sub:
        _trigger_focus_change(tauri_app)
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_event_has_target(tauri_app):
    with tauri_app.subscribe() as sub:
        _trigger_focus_change(tauri_app)
        event = sub.recv(timeout=3.0)
        assert event.target is not None
        assert event.target.role is not None


def test_wait_for_event(tauri_app):
    with tauri_app.subscribe() as sub:
        _trigger_focus_change(tauri_app)
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(tauri_app):
    sub = tauri_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(tauri_app):
    with tauri_app.subscribe() as sub:
        _trigger_focus_change(tauri_app)
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == tauri_app.pid


def test_subscription_drop_then_resubscribe(tauri_app):
    with tauri_app.subscribe():
        pass
    with tauri_app.subscribe() as sub2:
        assert sub2.try_recv() is None
