"""CLI integration tests for ``xa11y action``.

Tests perform accessibility actions against a live test app and verify
that the CLI exits 0 and prints ``ok`` on success.

Side-effect awareness: the Tauri test app is session-scoped, so actions
that mutate state (button press, toggle, focus) may affect later tests.
Tests are ordered so that non-mutating reads come first and state-changing
operations come last.
"""

from __future__ import annotations

import pytest


# ── Helpers ───────────────────────────────────────────────────────────────────


def _assert_ok(rc: int, stdout: str, stderr: str) -> None:
    """Assert the CLI exited 0 and printed ``ok``."""
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout, f"expected 'ok' in stdout, got: {stdout!r}"


# ── Read-only / focus actions ──────────────────────────────────────────────────


def test_action_focus_button(run_cli, app_pid):
    """``xa11y action focus button[name="OK"]`` should succeed."""
    rc, stdout, stderr = run_cli(
        "action", "focus", 'button[name="OK"]', "--pid", str(app_pid)
    )
    _assert_ok(rc, stdout, stderr)


def test_action_scroll_into_view(run_cli, app_pid):
    """``scroll-into-view`` on a visible element should exit 0."""
    rc, stdout, stderr = run_cli(
        "action", "scroll-into-view", 'button[name="OK"]', "--pid", str(app_pid)
    )
    # scroll-into-view may be unsupported on some element/platform combos;
    # tolerate ActionNotSupported and AT-SPI's "UnknownObject" (Qt under
    # AT-SPI doesn't always expose a stable accessible path for the
    # already-visible button). Other platform errors are test failures.
    if rc != 0:
        lower = stderr.lower()
        tolerated = (
            "not supported" in lower
            or "unsupported" in lower
            or "unknownobject" in lower
        )
        assert tolerated, (
            f"unexpected failure from scroll-into-view:\nstderr: {stderr}"
        )
    else:
        assert "ok" in stdout


def test_action_unknown_action_exits_nonzero(run_cli, app_pid):
    """An unrecognised action name must exit non-zero."""
    rc, stdout, stderr = run_cli(
        "action", "__no_such_action__", "button", "--pid", str(app_pid)
    )
    assert rc != 0, "expected non-zero exit for unknown action"


def test_action_missing_selector_exits_nonzero(run_cli):
    """``xa11y action press`` without a selector must exit non-zero.

    The argument-count check fires before the app is resolved, so no live
    app is needed here.
    """
    rc, stdout, stderr = run_cli("action", "press")
    assert rc != 0, "expected non-zero exit when selector is missing"


def test_action_without_app_exits_nonzero(run_cli):
    """``xa11y action press button`` without --app/--pid must exit non-zero."""
    rc, stdout, stderr = run_cli("action", "press", "button")
    assert rc != 0, "expected non-zero exit when no app is specified"
    assert "--app" in stderr or "--pid" in stderr, (
        f"expected --app/--pid hint in stderr:\n{stderr}"
    )


# ── State-mutating actions ────────────────────────────────────────────────────


def test_action_toggle_checkbox(run_cli, app_pid):
    """``toggle`` on a checkbox element should succeed."""
    rc, stdout, stderr = run_cli(
        "action", "toggle", 'check_box[name="Agree to terms"]', "--pid", str(app_pid)
    )
    _assert_ok(rc, stdout, stderr)


def test_action_press_button(run_cli, app_pid):
    """``press`` on the OK button should exit 0 and print ``ok``."""
    rc, stdout, stderr = run_cli(
        "action", "press", 'button[name="OK"]', "--pid", str(app_pid)
    )
    _assert_ok(rc, stdout, stderr)
