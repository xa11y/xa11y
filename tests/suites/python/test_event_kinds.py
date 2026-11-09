"""Subscribe-time kinds (#480) against the real test app, on every cell: a
kind the capability table marks unsupported fails at once instead of never
arriving, and a supported one subscribes. On an app with a button that raises
an announcement request (WinForms), the request's text arrives and a
subscription for other kinds does not receive it."""

from __future__ import annotations

import time

import pytest

import xa11y


def _event_rows():
    caps = xa11y.capabilities()
    return [
        (name.removeprefix("event:"), caps.support(name))
        for name in caps.names()
        if name.startswith("event:")
        and name != "event:text_change"
        and not name.startswith("event:announcement:")
    ]


def test_an_undeliverable_kind_fails_at_subscribe(app):
    unsupported = [name for name, support in _event_rows() if support == "unsupported"]
    if not unsupported:
        pytest.skip("this platform delivers every event kind")
    with pytest.raises(xa11y.ActionNotSupportedError, match=unsupported[0]):
        app.subscribe(kinds=[unsupported[0]])


def test_a_supported_kind_subscribes_and_reports_no_drops(app):
    supported = [name for name, support in _event_rows() if support == "supported"]
    with app.subscribe(kinds=supported[:2], buffer=64) as sub:
        assert sub.dropped == 0
        sub.try_recv()


def _announce(app, app_config):
    """Press the app's notification-raising button, or skip on an app that
    has none (see ``announce_button_name`` in conftest.APP_CONFIGS)."""
    name = app_config.get("announce_button_name")
    if not name:
        pytest.skip("app has no button that raises an announcement request")
    app.locator(f'button[name="{name}"]').press()


def _is_request(event) -> bool:
    return (
        event.event_type == xa11y.EventType.ANNOUNCEMENT
        and event.announcement is not None
        and event.announcement.origin == "request"
    )


def test_an_announcement_request_carries_its_text(app, app_config):
    """A UIA Notification arrives as an announcement with its text and
    processing (#480): the WinForms app raises ``"Saved"`` with
    ``ImportantMostRecent``, which is assertive."""
    with app.subscribe(kinds=["announcement"]) as sub:
        _announce(app, app_config)
        event = sub.wait_for(_is_request, timeout=5.0)
    info = event.announcement
    assert info.text == app_config["announce_text"]
    assert info.origin == "request"
    assert info.politeness == app_config["announce_politeness"]
    assert info.platform_priority == app_config["announce_platform_priority"]
    assert event.app_pid == app.pid


def test_a_subscription_for_other_kinds_receives_no_announcement(app, app_config):
    """``kinds`` filters: while an announcement-only subscription sees the
    request, a focus-only one opened alongside it receives no announcement."""
    with app.subscribe(kinds=["announcement"]) as announcements:
        with app.subscribe(kinds=["focus_changed"]) as focus:
            _announce(app, app_config)
            # Proves the request was raised during this window, so the
            # absence below is filtering and not a quiet app.
            announcements.wait_for(_is_request, timeout=5.0)
            deadline = time.monotonic() + 1.0
            while time.monotonic() < deadline:
                event = focus.try_recv()
                if event is None:
                    time.sleep(0.05)
                    continue
                assert event.event_type == xa11y.EventType.FOCUS_CHANGED, (
                    f"a focus_changed subscription delivered {event.event_type}"
                )
