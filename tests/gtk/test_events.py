"""Integration tests: accessibility events from the GTK4 test app via xa11y."""

from __future__ import annotations

import xa11y


def test_subscribe_returns_subscription(gtk_app):
    with gtk_app.subscribe() as sub:
        assert sub is not None


def test_focus_event(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator("button").first().focus()
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_value_change_event(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event.event_type is not None


def test_button_press_event(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="OK"]').first().press()
        event = sub.recv(timeout=3.0)
        assert event.event_type is not None


def test_event_has_target(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator("button").first().press()
        event = sub.recv(timeout=3.0)
        assert event.target is not None
        assert event.target.role is not None


def test_wait_for_event(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator("spin_button").first().increment()
        event = sub.wait_for(lambda e: e.event_type is not None, timeout=3.0)
        assert event is not None


def test_subscription_close(gtk_app):
    sub = gtk_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_event_metadata_populated(gtk_app):
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").first().increment()
        event = sub.recv(timeout=3.0)
        assert event.app_name
        assert event.app_pid == gtk_app.pid


def test_subscription_drop_then_resubscribe(gtk_app):
    with gtk_app.subscribe():
        pass
    with gtk_app.subscribe() as sub2:
        assert sub2.try_recv() is None
