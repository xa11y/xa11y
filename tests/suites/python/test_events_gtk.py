"""GTK4 event-subscription tests (strict).

This suite closes the ``gtk_events`` coverage gap. The shared
``test_events.py`` suite marks its value/state/name/structure tests
``xfail`` because Qt and Tauri (WebKit2GTK) do not reliably emit AT-SPI2
events for programmatic accessibility actions. GTK4's AT-SPI2 bridge *does*
deliver ``Object:ValueChanged`` for range widgets driven through xa11y, so
here we assert those events **strictly** — a missed event fails the run
rather than passing silently, in keeping with the no-silent-fallbacks tenet.

The whole module is skipped on non-GTK apps via the ``_gtk_only`` autouse
fixture, so it is a no-op in the other compat-matrix cells.

Covered:
- Subscription mechanics (drains to idle, ``try_recv`` returns ``None``).
- ``ValueChanged`` from ``increment()`` on a slider and a spin_button.
- ``ValueChanged`` from ``set_numeric_value()`` on a slider.
- Event metadata (``app_pid`` / ``app_name``) and target population.
- ``wait_for`` predicate matching.
"""

from __future__ import annotations

import time

import pytest
import xa11y


ACTION_SETTLE = 0.3
EVENT_TIMEOUT = 5.0


@pytest.fixture(autouse=True)
def _gtk_only(app_name):
    """Restrict this suite to the GTK test app."""
    if app_name != "gtk":
        pytest.skip("GTK-specific event suite")


def _drain(sub: xa11y.Subscription, duration: float = 0.3) -> None:
    """Discard any events queued during subscription setup.

    AT-SPI2 frequently replays focus/state bits to a freshly attached
    subscriber, so tests that assert on a *specific* triggered event drain
    first to avoid matching stale signals.
    """
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        if sub.try_recv() is None:
            time.sleep(0.05)


# ---------------------------------------------------------------------------
# Subscription mechanics
# ---------------------------------------------------------------------------


def test_subscription_drains_to_idle(app):
    """After draining, try_recv returns None rather than raising."""
    with app.subscribe() as sub:
        _drain(sub)
        assert sub.try_recv() is None


# ---------------------------------------------------------------------------
# ValueChanged — the events GTK4 reliably emits
# ---------------------------------------------------------------------------


def test_value_changed_event_slider(app, app_config):
    """increment() on the slider fires a ValueChanged event."""
    sel = app_config["slider_selector"]
    with app.subscribe() as sub:
        _drain(sub)
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=EVENT_TIMEOUT,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


def test_value_changed_event_spinbutton(app, app_config):
    """increment() on the spin_button fires a ValueChanged event."""
    sel = app_config["spinbutton_selector"]
    with app.subscribe() as sub:
        _drain(sub)
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=EVENT_TIMEOUT,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


def test_value_changed_event_set_numeric_value(app, app_config):
    """set_numeric_value() on the slider fires a ValueChanged event."""
    sel = app_config["slider_selector"]
    loc = app.locator(sel)
    current = loc.element().numeric_value or 0.0
    # Pick a target distinct from the current value so a change is guaranteed.
    target = 73.0 if abs(current - 73.0) > 1.0 else 23.0
    with app.subscribe() as sub:
        _drain(sub)
        loc.set_numeric_value(target)
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=EVENT_TIMEOUT,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED
    # Restore.
    loc.set_numeric_value(50.0)
    time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Event metadata and target
# ---------------------------------------------------------------------------


def test_event_carries_app_metadata(app, app_config):
    """A delivered event reports the source app's pid and a non-empty name."""
    sel = app_config["slider_selector"]
    with app.subscribe() as sub:
        _drain(sub)
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=EVENT_TIMEOUT,
        )
        assert event.app_pid == app.pid
        assert event.app_name


def test_value_changed_event_has_slider_target(app, app_config):
    """The slider's ValueChanged event carries a target with the slider role."""
    sel = app_config["slider_selector"]
    with app.subscribe() as sub:
        _drain(sub)
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: (
                e.event_type == xa11y.EventType.VALUE_CHANGED
                and e.target is not None
                and e.target.role == "slider"
            ),
            timeout=EVENT_TIMEOUT,
        )
        assert event.target is not None
        assert event.target.role == "slider"


# ---------------------------------------------------------------------------
# wait_for predicate
# ---------------------------------------------------------------------------


def test_wait_for_returns_first_matching_event(app, app_config):
    """wait_for returns the first event matching the predicate."""
    sel = app_config["spinbutton_selector"]
    with app.subscribe() as sub:
        _drain(sub)
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=EVENT_TIMEOUT,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED
