"""CLI integration tests for input-simulation commands.

Input simulation (click, move, drag, scroll, key, type) requires OS-level
permissions (Accessibility + Input Monitoring on macOS, XTest on X11, no
grant on Windows). When the backend is unavailable the CLI exits non-zero
with an "unsupported" or "permission" message; those cases are skipped.

The tests synthesise events into screen coordinates derived from the live
test app's accessibility tree, so they need a running app — but they do NOT
rely on the app processing the events (that would be an input-sim integration
test; see tests/tauri/test_input_sim.py).  Here we only check that the CLI
command itself exits 0 (or gracefully skips when unsupported).

Argument-validation error paths (missing --at, --to, etc.) are covered by
the unit tests in xa11y-python/tests/test_cli.py and are not repeated here.
"""

from __future__ import annotations

import os

import pytest


pytestmark = pytest.mark.skipif(
    os.environ.get("XA11Y_SKIP_INPUT_SIM") == "1",
    reason="Input simulation disabled (XA11Y_SKIP_INPUT_SIM=1)",
)


# ── Helpers ───────────────────────────────────────────────────────────────────


def _skip_if_unsupported(rc: int, stderr: str) -> None:
    """Skip when the backend reports the operation is unavailable."""
    if rc != 0:
        lower = stderr.lower()
        if any(kw in lower for kw in ("unsupported", "permission", "access denied",
                                      "not supported", "axisundefined")):
            pytest.skip(f"input simulation not available in this session: {stderr.strip()}")


def _button_center(app) -> tuple[int, int]:
    """Return the screen-coordinate centre of the OK button."""
    el = app.locator('button[name="OK"]').element()
    r = el.bounds
    if r is None:
        pytest.skip("OK button has no on-screen bounds — cannot derive click target")
    return (r.x + r.width // 2, r.y + r.height // 2)


# ── click ─────────────────────────────────────────────────────────────────────


def test_cli_click_left_button(run_cli, app):
    """``xa11y click --at X,Y`` should exit 0."""
    cx, cy = _button_center(app)
    rc, stdout, stderr = run_cli("click", "--at", f"{cx},{cy}")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


def test_cli_click_right_button(run_cli, app):
    """``xa11y click --at X,Y --button right`` should exit 0."""
    cx, cy = _button_center(app)
    rc, stdout, stderr = run_cli("click", "--at", f"{cx},{cy}", "--button", "right")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


# ── move ──────────────────────────────────────────────────────────────────────


def test_cli_move(run_cli, app):
    """``xa11y move --at X,Y`` should exit 0."""
    cx, cy = _button_center(app)
    rc, stdout, stderr = run_cli("move", "--at", f"{cx},{cy}")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


# ── drag ──────────────────────────────────────────────────────────────────────


def test_cli_drag(run_cli, app):
    """``xa11y drag --from X,Y --to X,Y`` should exit 0."""
    cx, cy = _button_center(app)
    rc, stdout, stderr = run_cli(
        "drag", "--from", f"{cx},{cy}", "--to", f"{cx + 5},{cy + 5}"
    )
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


# ── scroll ────────────────────────────────────────────────────────────────────


def test_cli_scroll(run_cli, app):
    """``xa11y scroll --at X,Y --dy 3`` should exit 0."""
    cx, cy = _button_center(app)
    rc, stdout, stderr = run_cli("scroll", "--at", f"{cx},{cy}", "--dy", "3")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


# ── key ───────────────────────────────────────────────────────────────────────


def test_cli_key_named(run_cli, app):
    """``xa11y key Tab`` should exit 0."""
    rc, stdout, stderr = run_cli("key", "Tab")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


def test_cli_key_char(run_cli, app):
    """``xa11y key a`` (single char) should exit 0."""
    rc, stdout, stderr = run_cli("key", "a")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout


# ── type ──────────────────────────────────────────────────────────────────────


def test_cli_type_text(run_cli, app):
    """``xa11y type TEXT`` should exit 0."""
    rc, stdout, stderr = run_cli("type", "hello")
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "ok" in stdout
