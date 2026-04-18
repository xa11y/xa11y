"""Integration tests: accessibility events from the Tauri (WebView) test app via xa11y."""

from __future__ import annotations

import xa11y


def test_subscribe_returns_subscription(tauri_app):
    with tauri_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator("button").first().focus()
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_value_change_event(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator('slider[name="Volume"]').first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.event_type is not None


def test_toggle_event(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator('check_box[name="Agree to terms"]').first().toggle()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.event_type is not None


def test_event_has_target(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator('button[name="OK"]').first().press()
        event = sub.recv(timeout=3.0)
        assert event.target is not None
        assert event.target.role is not None


def test_wait_for_event(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator('slider[name="Volume"]').first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(tauri_app):
    sub = tauri_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(tauri_app):
    with tauri_app.subscribe() as sub:
        tauri_app.locator('slider[name="Volume"]').first().increment()
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == tauri_app.pid


def test_subscription_drop_then_resubscribe(tauri_app):
    with tauri_app.subscribe():
        pass
    with tauri_app.subscribe() as sub2:
        assert sub2.try_recv() is None
