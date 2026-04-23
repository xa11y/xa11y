"""Integration tests: accessibility events from the Qt test app via xa11y.

These tests assert that the platform accessibility bus delivers events for
each of the xa11y event kinds we publicly expose (FocusChanged, ValueChanged,
StateChanged, NameChanged, StructureChanged). If an event does not arrive on
a given platform we fail the test (AGENTS.md tenet 1 — no silent fallbacks);
legitimate platform limitations are flagged with ``@pytest.mark.xfail``.
"""

from __future__ import annotations

import sys
import time

import pytest
import xa11y


# Small pause after UI actions so AT-SPI2/UIA has a chance to dispatch the
# event on the event bus before the subscription drains.
ACTION_SETTLE = 0.3


def _drain_for(sub: xa11y.Subscription, duration: float) -> list[xa11y.Event]:
    """Collect all events received over ``duration`` seconds."""
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


def test_subscribe_returns_subscription(qt_app):
    """Can create an event subscription."""
    with qt_app.subscribe() as sub:
        assert sub is not None


def test_subscription_close(qt_app):
    """Closing a subscription should be idempotent."""
    sub = qt_app.subscribe().__enter__()
    sub.close()
    sub.close()


# ── FocusChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_focus_changed_event(qt_app):
    """Focusing an unfocused control fires a FocusChanged event."""
    # Seed focus on OK so the subsequent focus() on Cancel is an actual change.
    qt_app.locator('button[name="OK"]').focus()
    time.sleep(ACTION_SETTLE)
    with qt_app.subscribe() as sub:
        try:
            qt_app.locator('spin_button[name="Quantity"]').focus()
        except xa11y.ActionNotSupportedError:
            pytest.fail("focus() must be supported on QSpinBox")
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.FOCUS_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.FOCUS_CHANGED


# ── ValueChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_value_changed_event_slider(qt_app):
    """Incrementing a slider fires a ValueChanged event."""
    with qt_app.subscribe() as sub:
        qt_app.locator('slider[name="Volume"]').increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_value_changed_event_spinbox(qt_app):
    """Incrementing a spin_button fires a ValueChanged event."""
    with qt_app.subscribe() as sub:
        qt_app.locator('spin_button[name="Quantity"]').increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


# ── StateChanged ───────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_state_changed_event_checkbox(qt_app):
    """Toggling a checkbox fires either StateChanged or ValueChanged.

    The xa11y event model may surface a checkbox toggle as StateChanged
    (with the checked flag) or as ValueChanged (AT-SPI2 raises AXValueChanged
    on checkboxes; on Windows UIA it's PropertyChanged(ToggleState)). We
    accept either here — the test's point is that SOME event is delivered
    and that it's not a silent no-op.
    """
    cb = qt_app.locator('check_box[name="Subscribe"]')
    with qt_app.subscribe() as sub:
        cb.toggle()
        event = sub.wait_for(
            lambda e: (
                e.event_type
                in (
                    xa11y.EventType.STATE_CHANGED,
                    xa11y.EventType.VALUE_CHANGED,
                )
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


# ── NameChanged ────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_name_changed_event(qt_app):
    """Pressing Submit mutates the status label → NameChanged event."""
    with qt_app.subscribe() as sub:
        qt_app.locator('button[name="Submit"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.NAME_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.NAME_CHANGED


# ── StructureChanged ───────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_structure_changed_event_add_item(qt_app):
    """Pressing Add Item appends a list row → StructureChanged event."""
    with qt_app.subscribe() as sub:
        qt_app.locator('button[name="Add Item"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED
    # Restore the list length so downstream tests see the original count.
    qt_app.locator('button[name="Remove Item"]').press()
    time.sleep(ACTION_SETTLE)


# ── Event metadata ─────────────────────────────────────────────────────────


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_event_has_app_metadata(qt_app):
    """Events carry app_name / app_pid metadata."""
    with qt_app.subscribe() as sub:
        qt_app.locator('button[name="OK"]').press()
        event = sub.recv(timeout=5.0)
        assert event.app_pid == qt_app.pid
        assert event.app_name  # non-empty string


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_event_has_target(qt_app):
    """Events carry a target element when the platform populates it."""
    with qt_app.subscribe() as sub:
        qt_app.locator('slider[name="Volume"]').increment()
        event = sub.wait_for(
            lambda e: e.target is not None,
            timeout=5.0,
        )
        assert event.target is not None
        assert event.target.role is not None


# ── Iteration / drain ──────────────────────────────────────────────────────


def test_try_recv_returns_none_when_idle(qt_app):
    """try_recv returns None (doesn't raise) when the queue is empty."""
    with qt_app.subscribe() as sub:
        # Drain any stragglers from prior tests first.
        _drain_for(sub, 0.3)
        assert sub.try_recv() is None


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="Qt on macOS has known AX tree issues — see ci.yml comment.",
    strict=False,
)
def test_wait_for_event(qt_app):
    """wait_for should return the first event matching the predicate."""
    with qt_app.subscribe() as sub:
        qt_app.locator('spin_button[name="Quantity"]').increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED
