"""Integration tests: accessibility events from the GTK4 test app via xa11y.

GTK4 exposes events over AT-SPI2. These tests fail (rather than skip) when a
platform genuinely emits no event; known platform limitations are flagged
with ``@pytest.mark.xfail`` carrying a specific reason (AGENTS.md tenet 1 —
no silent fallbacks).
"""

from __future__ import annotations

import time

import pytest
import xa11y


ACTION_SETTLE = 0.3


def _drain_for(sub: xa11y.Subscription, duration: float) -> list[xa11y.Event]:
    """Collect all events over ``duration`` seconds."""
    events: list[xa11y.Event] = []
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        ev = sub.try_recv()
        if ev is not None:
            events.append(ev)
        else:
            time.sleep(0.05)
    return events


# ── Core subscription API ──────────────────────────────────────────────────


def test_subscribe_returns_subscription(gtk_app: xa11y.App) -> None:
    """Can create an event subscription."""
    with gtk_app.subscribe() as sub:
        assert sub is not None


def test_subscription_close_is_idempotent(gtk_app: xa11y.App) -> None:
    """Closing a subscription should be idempotent."""
    sub = gtk_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_try_recv_returns_none_when_idle(gtk_app: xa11y.App) -> None:
    """try_recv returns None (doesn't raise) when the queue is empty."""
    with gtk_app.subscribe() as sub:
        _drain_for(sub, 0.3)
        assert sub.try_recv() is None


# ── FocusChanged ───────────────────────────────────────────────────────────


def test_focus_changed_event(gtk_app: xa11y.App) -> None:
    """focus() on a focusable widget fires a FocusChanged event."""
    # Seed focus elsewhere first so the subsequent focus() is an actual move.
    gtk_app.locator('button[name="OK"]').focus()
    time.sleep(ACTION_SETTLE)
    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="Submit"]').focus()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.FOCUS_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.FOCUS_CHANGED


# ── ValueChanged ───────────────────────────────────────────────────────────


def test_value_changed_event_slider(gtk_app: xa11y.App) -> None:
    """Incrementing a slider fires a ValueChanged event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


def test_value_changed_event_text_set_value(gtk_app: xa11y.App) -> None:
    """Setting a Gtk.Entry value fires a ValueChanged or TextChanged event."""
    tf = gtk_app.locator("text_field")
    with gtk_app.subscribe() as sub:
        tf.set_value("event test value")
        event = sub.wait_for(
            lambda e: (
                e.event_type
                in (xa11y.EventType.VALUE_CHANGED, xa11y.EventType.TEXT_CHANGED)
            ),
            timeout=5.0,
        )
        assert event.event_type in (
            xa11y.EventType.VALUE_CHANGED,
            xa11y.EventType.TEXT_CHANGED,
        )
    # Restore so subsequent test_widgets tests still see "hello world".
    tf.set_value("hello world")
    time.sleep(ACTION_SETTLE)


# ── StateChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    reason=(
        "GTK4 Gtk.CheckButton does not expose any AT-SPI2 Action interface "
        "actions, so xa11y cannot toggle the checkbox to drive a StateChanged "
        "event. Tracked as a GTK4/AT-SPI2 platform limitation (see also "
        "test_widgets.test_checkbox_press)."
    ),
    strict=False,
)
def test_state_changed_event_checkbox(gtk_app: xa11y.App) -> None:
    """Toggling a checkbox fires StateChanged/ValueChanged."""
    cb = gtk_app.locator('check_box[name="Subscribe"]')
    with gtk_app.subscribe() as sub:
        cb.toggle()
        event = sub.wait_for(
            lambda e: (
                e.event_type
                in (xa11y.EventType.STATE_CHANGED, xa11y.EventType.VALUE_CHANGED)
            ),
            timeout=5.0,
        )
        assert event.event_type in (
            xa11y.EventType.STATE_CHANGED,
            xa11y.EventType.VALUE_CHANGED,
        )


def test_state_changed_event_enable_cancel(gtk_app: xa11y.App) -> None:
    """Pressing OK enables Cancel — that enabled-state transition should fire
    either a StateChanged or (on toolkits that coalesce) a ValueChanged event.
    """
    # First make sure Cancel is currently disabled (tests may have run before
    # and flipped it). If it's already enabled, press OK to toggle-off.
    cancel = gtk_app.locator('button[name="Cancel"]').element()
    if cancel.enabled:
        # OK toggles Cancel's sensitivity each press in the test app.
        gtk_app.locator('button[name="OK"]').press()
        time.sleep(ACTION_SETTLE)

    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="OK"]').press()
        event = sub.wait_for(
            lambda e: (
                e.event_type
                in (xa11y.EventType.STATE_CHANGED, xa11y.EventType.VALUE_CHANGED)
            ),
            timeout=5.0,
        )
        assert event.event_type in (
            xa11y.EventType.STATE_CHANGED,
            xa11y.EventType.VALUE_CHANGED,
        )


# ── NameChanged ────────────────────────────────────────────────────────────


def test_name_changed_event_status_label(gtk_app: xa11y.App) -> None:
    """Submitting mutates the status label's accessible name → NameChanged."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="Submit"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.NAME_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.NAME_CHANGED


# ── StructureChanged ───────────────────────────────────────────────────────


def test_structure_changed_event_add_item(gtk_app: xa11y.App) -> None:
    """Adding a row to the ListBox fires a StructureChanged event."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="Add Item"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED
    # Restore the list so other tests see the original count.
    gtk_app.locator('button[name="Remove Item"]').press()
    time.sleep(ACTION_SETTLE)


def test_structure_changed_event_remove_item(gtk_app: xa11y.App) -> None:
    """Removing a row from the ListBox fires a StructureChanged event."""
    # Add first so we have something to remove without underflowing.
    gtk_app.locator('button[name="Add Item"]').press()
    time.sleep(ACTION_SETTLE)

    with gtk_app.subscribe() as sub:
        gtk_app.locator('button[name="Remove Item"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED


# ── Event metadata ─────────────────────────────────────────────────────────


def test_event_has_app_metadata(gtk_app: xa11y.App) -> None:
    """Events carry app_name and app_pid matching the source app."""
    with gtk_app.subscribe() as sub:
        gtk_app.locator("slider").increment()
        event = sub.recv(timeout=5.0)
        assert event.app_pid == gtk_app.pid
        assert event.app_name  # non-empty
