"""Subscribe-time kinds (#480) against the real test app, on every cell: a
kind the capability table marks unsupported fails at once instead of never
arriving, and a supported one subscribes."""

from __future__ import annotations

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
