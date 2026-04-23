"""Integration tests: accessibility events from the Tauri test app via xa11y.

Tauri wraps a platform WebView (WebKit2GTK on Linux, WKWebView on macOS,
WebView2 on Windows). Event fidelity depends on the bridge each WebView
exposes; known limitations are flagged with ``@pytest.mark.xfail`` carrying
a specific reason (AGENTS.md tenet 1 — no silent fallbacks).
"""

from __future__ import annotations

import sys
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


def test_subscribe_returns_subscription(tauri_app: xa11y.App) -> None:
    """Can create an event subscription."""
    with tauri_app.subscribe() as sub:
        assert sub is not None


def test_subscription_close_is_idempotent(tauri_app: xa11y.App) -> None:
    """Closing a subscription should be idempotent."""
    sub = tauri_app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_try_recv_returns_none_when_idle(tauri_app: xa11y.App) -> None:
    """try_recv returns None (doesn't raise) when the queue is empty."""
    with tauri_app.subscribe() as sub:
        _drain_for(sub, 0.3)
        assert sub.try_recv() is None


# ── FocusChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason="Tauri on Linux uses WebKit2GTK, which doesn't reliably emit AT-SPI2 FocusChanged events for programmatic focus moves in tests.",
    strict=False,
)
def test_focus_changed_event(tauri_app: xa11y.App) -> None:
    """focus() on a webview control fires a FocusChanged event.

    Seeds focus on OK first so the subsequent focus() is an actual move.
    """
    tauri_app.locator('button[name="OK"]').focus()
    time.sleep(ACTION_SETTLE)
    with tauri_app.subscribe() as sub:
        tauri_app.locator('button[name="Submit"]').focus()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.FOCUS_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.FOCUS_CHANGED


# ── ValueChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason="Tauri on Linux uses WebKit2GTK, which doesn't reliably emit AT-SPI2 ValueChanged for HTML range sliders driven by increment().",
    strict=False,
)
def test_value_changed_event_slider(tauri_app: xa11y.App) -> None:
    """Incrementing a slider fires a ValueChanged event."""
    sl = tauri_app.locator('slider[name="Volume"]')
    with tauri_app.subscribe() as sub:
        sl.increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


# ── StateChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason="Tauri on Linux uses WebKit2GTK, which doesn't reliably emit AT-SPI2 StateChanged/ValueChanged for HTML checkbox toggles.",
    strict=False,
)
def test_state_changed_event_checkbox(tauri_app: xa11y.App) -> None:
    """Toggling an HTML <input type="checkbox"> fires StateChanged or ValueChanged."""
    cb = tauri_app.locator('check_box[name="Agree to terms"]')
    with tauri_app.subscribe() as sub:
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
    # Restore original state.
    cb.toggle()
    time.sleep(ACTION_SETTLE)


@pytest.mark.xfail(
    reason=(
        "The Tauri test app's OK → Cancel transition only fires once per "
        "session (Cancel is never re-disabled), so whether this test sees a "
        "state-change event depends on run order. Rust integ covers "
        "disabled→enabled StateChanged on AccessKit."
    ),
    strict=False,
)
def test_state_changed_event_enable_cancel(tauri_app: xa11y.App) -> None:
    """Pressing OK enables Cancel — disabled→enabled fires a state change."""
    with tauri_app.subscribe() as sub:
        tauri_app.locator('button[name="OK"]').press()
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


@pytest.mark.xfail(
    sys.platform == "linux",
    reason=(
        "WebKit2GTK does not reliably fire AT-SPI2 object:property-change:"
        "accessible-name when aria-label changes on an element. Tracked as "
        "a WebKit/AT-SPI2 bridge limitation."
    ),
    strict=False,
)
def test_name_changed_event_status_label(tauri_app: xa11y.App) -> None:
    """Submitting mutates the status label's aria-label → NameChanged."""
    with tauri_app.subscribe() as sub:
        tauri_app.locator('button[name="Submit"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.NAME_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.NAME_CHANGED


# ── StructureChanged ───────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason="Tauri on Linux uses WebKit2GTK, which doesn't reliably emit AT-SPI2 StructureChanged for DOM mutations driven by button presses.",
    strict=False,
)
def test_structure_changed_event_add_item(tauri_app: xa11y.App) -> None:
    """Appending a list option fires a StructureChanged event."""
    with tauri_app.subscribe() as sub:
        tauri_app.locator('button[name="Add Item"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED
    # Restore so downstream tests see the original list length.
    tauri_app.locator('button[name="Remove Item"]').press()
    time.sleep(ACTION_SETTLE)


# ── Event metadata ─────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "linux",
    reason="Depends on a ValueChanged event being emitted, which WebKit2GTK doesn't reliably do — see test_value_changed_event_slider.",
    strict=False,
)
def test_event_has_app_metadata(tauri_app: xa11y.App) -> None:
    """Events carry app_name and app_pid matching the source app."""
    with tauri_app.subscribe() as sub:
        tauri_app.locator('slider[name="Volume"]').increment()
        event = sub.recv(timeout=5.0)
        assert event.app_pid == tauri_app.pid
        assert event.app_name  # non-empty
