"""Integration tests: accessibility events from the Cocoa test app via xa11y.

These tests exercise the macOS push-based event subscription against the
native AppKit test app. Known platform limitations are flagged with
``@pytest.mark.xfail`` carrying a specific reason (AGENTS.md tenet 1 — no
silent fallbacks).
"""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


pytestmark = pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Cocoa/AppKit tests are macOS-only.",
)


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


def test_subscribe_returns_subscription(cocoa_app: xa11y.App) -> None:
    """Can create an event subscription."""
    with cocoa_app.subscribe() as sub:
        assert sub is not None


def test_subscription_close_is_idempotent(cocoa_app: xa11y.App) -> None:
    """Closing a subscription should be idempotent."""
    sub = cocoa_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_try_recv_returns_none_when_idle(cocoa_app: xa11y.App) -> None:
    """try_recv returns None (doesn't raise) when the queue is empty."""
    with cocoa_app.subscribe() as sub:
        _drain_for(sub, 0.3)
        assert sub.try_recv() is None


# ── FocusChanged ───────────────────────────────────────────────────────────


def test_focus_changed_event(cocoa_app: xa11y.App) -> None:
    """focus() on a focusable control fires a FocusChanged event.

    AppKit fires AXFocusedUIElementChanged on every focus move. We seed focus
    on OK first to ensure the next focus() is an actual change.
    """
    cocoa_app.locator('button[name="OK"]').focus()
    time.sleep(ACTION_SETTLE)
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('button[name="Submit"]').focus()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.FOCUS_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.FOCUS_CHANGED


# ── ValueChanged ───────────────────────────────────────────────────────────


def test_value_changed_event_slider(cocoa_app: xa11y.App) -> None:
    """Setting the slider value fires an AXValueChanged event."""
    sl = cocoa_app.locator('slider[name="Volume"]')
    with cocoa_app.subscribe() as sub:
        sl.set_numeric_value(73.0)
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED
    # Restore so widget tests see 50.0 again.
    sl.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


def test_value_changed_event_text_field(cocoa_app: xa11y.App) -> None:
    """Setting a text_field value fires a ValueChanged or TextChanged event."""
    tf = cocoa_app.locator('text_field[name="Search"]')
    with cocoa_app.subscribe() as sub:
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
    # Restore for downstream tests.
    tf.set_value("hello world")
    time.sleep(ACTION_SETTLE)


# ── StateChanged ───────────────────────────────────────────────────────────


def test_state_changed_event_checkbox(cocoa_app: xa11y.App) -> None:
    """Toggling a checkbox fires StateChanged (checked flag) or ValueChanged.

    AppKit raises AXValueChanged on an AXCheckBox when its state flips; the
    macOS backend may also synthesize StateChanged{checked}. Accept either.
    """
    cb = cocoa_app.locator('check_box[name="Subscribe"]')
    with cocoa_app.subscribe() as sub:
        cb.press()
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
    # Restore original state.
    cb.press()
    time.sleep(ACTION_SETTLE)


# ── NameChanged ────────────────────────────────────────────────────────────


def test_name_changed_event_status_label(cocoa_app: xa11y.App) -> None:
    """Submitting mutates the status label's accessible name → NameChanged.

    AppKit fires AXTitleChanged when an accessibility label changes.
    """
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('button[name="Submit"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.NAME_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.NAME_CHANGED


# ── StructureChanged ───────────────────────────────────────────────────────


@pytest.mark.xfail(
    reason=(
        "NSTableView reloadData() does not always fire AXUIElementDestroyed / "
        "row insertion notifications synchronously — AppKit can coalesce "
        "structure updates on the main runloop. Tracking macOS-specific "
        "behaviour; see also the Rust integ suite which covers StructureChanged "
        "via AccessKit on macOS."
    ),
    strict=False,
)
def test_structure_changed_event_add_item(cocoa_app: xa11y.App) -> None:
    """Appending a row to the NSTableView fires StructureChanged."""
    with cocoa_app.subscribe() as sub:
        cocoa_app.locator('button[name="Add Item"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED


# ── Event metadata ─────────────────────────────────────────────────────────


def test_event_has_app_metadata(cocoa_app: xa11y.App) -> None:
    """Events carry app_name and app_pid matching the source app."""
    sl = cocoa_app.locator('slider[name="Volume"]')
    with cocoa_app.subscribe() as sub:
        sl.set_numeric_value(60.0)
        event = sub.recv(timeout=5.0)
        assert event.app_pid == cocoa_app.pid
        assert event.app_name  # non-empty
    sl.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)
