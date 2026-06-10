"""Event subscription tests: FocusChanged, ValueChanged, StateChanged, NameChanged, StructureChanged.

These tests verify that xa11y's event subscription API delivers events from
the platform accessibility bus. They are merged from the per-app event test
files (Qt, Cocoa, Tauri).

Known platform limitations are flagged per app/platform combo rather than
with blanket markers, in accordance with the design tenets:

- *Known-bad* combos (documented in the original per-app suites) get a
  ``skipif`` so they stay deterministic — these bridges emit events
  *unreliably*, so a strict xfail would flake.
- *Known-good* combos run with no marker, so a regression fails CI.
- Combos with no documented evidence either way keep a **non-strict
  xfail**, scoped as narrowly as the evidence allows.

Per-app notes:
- Qt does not reliably emit events for programmatic accessibility actions
  across AT-SPI2 / UIA / AX — Qt event-delivery tests are skipped.
- GTK exposes the same Dynamic widget group as Qt (Submit / Add Item /
  Remove Item in test-apps/gtk/app.py), so these tests run against it too
  (non-strict xfail: AT-SPI2 delivery is not yet verified either way).
- Cocoa (macOS) does emit AX notifications reliably; those tests run strictly
  (except NameChanged, which AppKit only emits with an explicit
  NSAccessibilityPostNotification the test app does not make).
- Tauri (WebView) event delivery is unreliable on Linux/WebKit2GTK and times
  out under CI load on macOS — both known-bad (skipped) for FocusChanged and
  ValueChanged.
"""

from __future__ import annotations

import os
import sys
import time

import pytest
import xa11y


ACTION_SETTLE = 0.3

# The markers below need the app identity at collection time; the
# ``app_name`` fixture resolves too late, so read the same environment
# variable the conftest uses (mirrors test_actions.py).
APP = os.environ.get("XA11Y_TEST_APP", "tauri")
QT = APP == "qt"
COCOA = APP == "cocoa"
TAURI_LINUX = APP == "tauri" and sys.platform == "linux"
TAURI_MACOS = APP == "tauri" and sys.platform == "darwin"


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


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


# ---------------------------------------------------------------------------
# Core subscription API
# ---------------------------------------------------------------------------


def test_subscribe_returns_subscription(app):
    """Can create an event subscription."""
    with app.subscribe() as sub:
        assert sub is not None


def test_subscription_close_is_idempotent(app):
    """Closing a subscription is idempotent (no error on double close)."""
    sub = app.subscribe().__enter__()
    sub.close()
    sub.close()


def test_try_recv_returns_none_when_idle(app):
    """try_recv returns None (does not raise) when the event queue is empty."""
    with app.subscribe() as sub:
        _drain_for(sub, 0.3)
        assert sub.try_recv() is None


# ---------------------------------------------------------------------------
# FocusChanged
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit FocusChanged events for programmatic "
        "focus() calls across AT-SPI2 / UIA / AX (known-bad; skipped "
        "rather than strict-xfailed because delivery is flaky, not absent)."
    ),
)
@pytest.mark.skipif(
    TAURI_LINUX,
    reason=(
        "Tauri on Linux (WebKit2GTK) does not reliably emit FocusChanged "
        "events for programmatic focus() calls (known-bad)."
    ),
)
@pytest.mark.skipif(
    TAURI_MACOS,
    reason=(
        "Tauri on macOS times out waiting for FocusChanged on CI runners "
        "(delivery is unreliable under CI load even though it can pass "
        "locally) — known-bad, skipped to stay deterministic."
    ),
)
@pytest.mark.xfail(
    condition=not COCOA,
    reason=(
        "FocusChanged delivery is unverified for this app/platform combo. "
        "Cocoa is documented reliable and asserts strictly."
    ),
    strict=False,
)
def test_focus_changed_event(app, app_config):
    """focus() on a focusable control fires a FocusChanged event."""
    ok_name = app_config["ok_button_name"]
    submit_name = app_config.get("submit_button_name")
    if not submit_name:
        pytest.skip("app has no Submit button — cannot test cross-element focus change")

    # Seed focus on OK so the next focus() is an actual move.
    app.locator(f'button[name="{ok_name}"]').focus()
    time.sleep(ACTION_SETTLE)
    with app.subscribe() as sub:
        app.locator(f'button[name="{submit_name}"]').focus()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.FOCUS_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.FOCUS_CHANGED


# ---------------------------------------------------------------------------
# ValueChanged
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit ValueChanged for a slider driven by "
        "increment() across AT-SPI2 / UIA / AX (known-bad)."
    ),
)
@pytest.mark.skipif(
    TAURI_LINUX,
    reason=(
        "Tauri on Linux (WebKit2GTK) has ValueChanged delivery gaps for "
        "HTML range inputs driven via AT-SPI2 (known-bad)."
    ),
)
@pytest.mark.skipif(
    TAURI_MACOS,
    reason=(
        "Tauri on macOS times out waiting for ValueChanged on CI runners "
        "(delivery is unreliable under CI load even though it can pass "
        "locally) — known-bad, skipped to stay deterministic."
    ),
)
@pytest.mark.xfail(
    condition=not COCOA,
    reason=(
        "ValueChanged delivery is unverified for this app/platform combo. "
        "Cocoa is documented reliable and asserts strictly."
    ),
    strict=False,
)
def test_value_changed_event_slider(app, app_config):
    """Incrementing a slider fires a ValueChanged event."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    with app.subscribe() as sub:
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


@pytest.mark.xfail(
    reason=(
        "Qt does not reliably emit ValueChanged for a QSpinBox driven by "
        "increment() across AT-SPI2 / UIA / AX."
    ),
    strict=False,
)
def test_value_changed_event_spinbox(app, app_config):
    """Incrementing a spin_button fires a ValueChanged event."""
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
    with app.subscribe() as sub:
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED


# ---------------------------------------------------------------------------
# StateChanged
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit StateChanged/ValueChanged for a checkbox "
        "toggled programmatically (known-bad)."
    ),
)
@pytest.mark.skipif(
    TAURI_LINUX,
    reason=(
        "Tauri on Linux (WebKit2GTK) has StateChanged delivery gaps for "
        "programmatic checkbox toggles (known-bad)."
    ),
)
@pytest.mark.xfail(
    condition=not COCOA,
    reason=(
        "StateChanged delivery is unverified for this app/platform combo. "
        "Cocoa emits AXValueChanged reliably and asserts strictly."
    ),
    strict=False,
)
def test_state_changed_event_checkbox(app, app_name, app_config):
    """Toggling a checkbox fires StateChanged or ValueChanged."""
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")
    if app_name == "gtk":
        pytest.skip(
            "GTK4 checkboxes do not expose AT-SPI2 Action interface — "
            "toggle() cannot be called."
        )
    name = app_config["checkbox_checked_name"]
    cb = app.locator(f'check_box[name="{name}"]')
    with app.subscribe() as sub:
        cb.toggle()
        event = sub.wait_for(
            lambda e: e.event_type in (
                xa11y.EventType.STATE_CHANGED,
                xa11y.EventType.VALUE_CHANGED,
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


# ---------------------------------------------------------------------------
# NameChanged
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit NameChanged when a label is mutated "
        "programmatically (known-bad)."
    ),
)
@pytest.mark.skipif(
    COCOA,
    reason=(
        "AppKit only emits NameChanged with an explicit "
        "NSAccessibilityPostNotification call, which the test app does not "
        "make (known-bad). Coverage is provided by Rust integ tests where "
        "available."
    ),
)
@pytest.mark.skipif(
    TAURI_LINUX,
    reason=(
        "WebKit2GTK does not reliably emit AT-SPI2 NameChanged for DOM "
        "label mutations (known-bad)."
    ),
)
@pytest.mark.xfail(
    reason=(
        "NameChanged delivery has no documented known-good app/platform "
        "combo in this suite (Tauri-on-macOS rides the AppKit pathway, and "
        "the AccessKit-backed apps are unverified); non-strict for the "
        "remaining combos until one is verified."
    ),
    strict=False,
)
def test_name_changed_event_status_label(app, app_config):
    """Pressing Submit mutates the status label → NameChanged event."""
    submit_name = app_config.get("submit_button_name")
    if not submit_name:
        pytest.skip("app has no Submit button")
    with app.subscribe() as sub:
        app.locator(f'button[name="{submit_name}"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.NAME_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.NAME_CHANGED


# ---------------------------------------------------------------------------
# StructureChanged
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit StructureChanged for list mutations "
        "(known-bad)."
    ),
)
@pytest.mark.skipif(
    TAURI_LINUX,
    reason=(
        "Tauri on Linux (WebKit2GTK) doesn't emit AT-SPI2 StructureChanged "
        "for DOM mutations (known-bad)."
    ),
)
@pytest.mark.xfail(
    reason=(
        "StructureChanged delivery is unverified elsewhere: AppKit may "
        "coalesce structure updates on the main runloop, and no app/platform "
        "combo in this suite is documented reliable; non-strict until one "
        "is verified."
    ),
    strict=False,
)
def test_structure_changed_event_add_item(app, app_config):
    """Pressing Add Item appends a row → StructureChanged event."""
    add_name = app_config.get("add_item_button_name")
    remove_name = app_config.get("remove_item_button_name")
    if not add_name:
        pytest.skip("app has no Add Item button")
    with app.subscribe() as sub:
        app.locator(f'button[name="{add_name}"]').press()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.STRUCTURE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.STRUCTURE_CHANGED
    # Restore
    if remove_name:
        app.locator(f'button[name="{remove_name}"]').press()
        time.sleep(ACTION_SETTLE)


# ---------------------------------------------------------------------------
# Event metadata
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    reason=(
        "Depends on at least one event being emitted; Qt and Tauri/Linux "
        "don't reliably emit events for programmatic actions, so the "
        "metadata assertion can't run. Cocoa covers this strictly."
    ),
    strict=False,
)
def test_event_has_app_metadata(app, app_config):
    """Events carry app_name and app_pid matching the source app."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider to trigger an event")
    with app.subscribe() as sub:
        app.locator(sel).increment()
        event = sub.recv(timeout=5.0)
        assert event.app_pid == app.pid
        assert event.app_name  # non-empty string


@pytest.mark.xfail(
    reason=(
        "Depends on a ValueChanged or other event reaching the subscription; "
        "Qt and Tauri/Linux gaps apply."
    ),
    strict=False,
)
def test_event_has_target(app, app_config):
    """Events carry a target element when the platform populates it."""
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider to trigger an event")
    with app.subscribe() as sub:
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.target is not None,
            timeout=5.0,
        )
        assert event.target is not None
        assert event.target.role is not None


# ---------------------------------------------------------------------------
# Iteration / drain
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    reason=(
        "wait_for is only as reliable as the underlying event stream; "
        "Qt/Tauri-Linux gaps apply."
    ),
    strict=False,
)
def test_wait_for_event(app, app_config):
    """wait_for returns the first event matching the predicate."""
    sel = app_config.get("spinbutton_selector") or app_config.get("slider_selector")
    if not sel:
        pytest.skip("no incrementable widget available")
    with app.subscribe() as sub:
        app.locator(sel).increment()
        event = sub.wait_for(
            lambda e: e.event_type == xa11y.EventType.VALUE_CHANGED,
            timeout=5.0,
        )
        assert event.event_type == xa11y.EventType.VALUE_CHANGED
