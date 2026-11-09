"""CLI integration tests for ``xa11y events`` (#480)."""

from __future__ import annotations

import xa11y


def test_an_unknown_kind_is_a_usage_error(run_cli, app_pid):
    rc, _stdout, stderr = run_cli("events", "--kinds", "focus", "--pid", str(app_pid))
    assert rc == 2, stderr
    assert "focus_changed" in stderr, "the error lists the valid kinds"


def test_a_kind_this_platform_never_sends_fails_at_once(run_cli, app_pid):
    caps = xa11y.capabilities()
    unsupported = [
        name.removeprefix("event:")
        for name in caps.names()
        if name.startswith("event:")
        and name != "event:text_change"
        and not name.startswith("event:announcement:")
        and caps.support(name) == "unsupported"
    ]
    if not unsupported:
        return
    rc, _stdout, stderr = run_cli("events", "--kinds", unsupported[0], "--pid", str(app_pid))
    assert rc == 1, stderr
    assert unsupported[0] in stderr, stderr
