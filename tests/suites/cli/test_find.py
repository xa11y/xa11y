"""CLI integration tests for ``xa11y find``.

Each test runs the CLI against a live Tauri test app and verifies exit
codes and basic output content.  Output format for the ``pretty`` formatter
is:  <role> [<name>] [value=...] [states] [bounds=(...)] ...
One element per line, followed by a summary ``(N match[es])`` line.
"""

from __future__ import annotations

import os
import re
import time

import pytest


# ── Helpers ───────────────────────────────────────────────────────────────────


def _assert_match_summary(stdout: str, at_least: int = 1) -> None:
    """Verify the ``(N match[es])`` summary line is present and N >= at_least."""
    lines = stdout.strip().splitlines()
    summary = lines[-1] if lines else ""
    assert "match" in summary, f"expected match summary, got: {summary!r}"
    # Extract the count — summary looks like "(3 matches)" or "(1 match)".
    count_str = summary.lstrip("(").split()[0]
    assert count_str.isdigit(), f"could not parse match count from {summary!r}"
    assert int(count_str) >= at_least, (
        f"expected >= {at_least} matches, got {count_str} in {summary!r}"
    )


# ── Tests ─────────────────────────────────────────────────────────────────────


def test_find_buttons_returns_results(run_cli, app_pid):
    """``xa11y find button`` should find at least one button in the test app."""
    rc, stdout, stderr = run_cli("find", "button", "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert stdout.strip(), "expected non-empty output"
    _assert_match_summary(stdout, at_least=1)
    # Every result line should mention the role.
    result_lines = stdout.strip().splitlines()[:-1]  # drop summary
    for line in result_lines:
        assert "button" in line, f"expected 'button' in line: {line!r}"


def test_find_specific_button_by_name(run_cli, app_pid):
    """``xa11y find 'button[name="OK"]'`` should find exactly the OK button."""
    rc, stdout, stderr = run_cli(
        "find", 'button[name="OK"]', "--pid", str(app_pid)
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    result_lines = stdout.strip().splitlines()[:-1]
    assert len(result_lines) >= 1, "expected at least one matching element"
    assert any("OK" in line for line in result_lines), (
        f"expected 'OK' in output, got:\n{stdout}"
    )


def test_find_window(run_cli, app_pid):
    """``xa11y find window`` should find the application window.

    Some toolkits (notably GTK4 under AT-SPI) don't expose a ``window`` role
    at the top level — the toplevel comes through as a ``group``. Treat
    "no matches" as a soft skip rather than a regression.
    """
    rc, stdout, stderr = run_cli("find", "window", "--pid", str(app_pid))
    if rc != 0 and "no elements matched" in stderr.lower():
        import pytest

        pytest.skip(
            "this toolkit does not expose a top-level `window` role"
            f" (stderr: {stderr.strip()})"
        )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    result_lines = stdout.strip().splitlines()[:-1]
    assert any("window" in line for line in result_lines), (
        f"expected 'window' role in output, got:\n{stdout}"
    )


def test_find_output_format_bounds(run_cli, app_pid):
    """``-o bounds`` should emit ``X,Y,W,H`` lines for elements with bounds."""
    rc, stdout, stderr = run_cli(
        "find", "button", "--pid", str(app_pid), "-o", "bounds"
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    # At least one line should be a comma-separated 4-tuple of integers.
    lines = [l for l in stdout.strip().splitlines() if l]
    assert lines, "expected at least one bounds line"
    for line in lines:
        parts = line.split(",")
        assert len(parts) == 4, f"expected X,Y,W,H but got: {line!r}"
        for part in parts:
            assert part.strip().lstrip("-").isdigit(), (
                f"expected integer coordinate in {line!r}, got {part!r}"
            )


def test_find_output_format_center(run_cli, app_pid):
    """``-o center`` should emit ``X,Y`` lines for elements with bounds."""
    rc, stdout, stderr = run_cli(
        "find", "button", "--pid", str(app_pid), "-o", "center"
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    lines = [l for l in stdout.strip().splitlines() if l]
    assert lines, "expected at least one center line"
    for line in lines:
        parts = line.split(",")
        assert len(parts) == 2, f"expected X,Y but got: {line!r}"
        for part in parts:
            assert part.strip().lstrip("-").isdigit(), (
                f"expected integer coordinate in {line!r}, got {part!r}"
            )


def test_find_nonexistent_selector_exits_nonzero(run_cli, app_pid):
    """A selector that matches nothing should produce a non-zero exit code."""
    rc, stdout, stderr = run_cli(
        "find", "menu_item[name='__no_such_element__']", "--pid", str(app_pid)
    )
    assert rc != 0, (
        f"expected non-zero exit for unmatched selector, got 0\nstdout: {stdout}"
    )


def test_find_check_box_exists(run_cli, app_pid):
    """The test app has checkboxes — verify they appear in find output."""
    rc, stdout, stderr = run_cli("find", "check_box", "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    result_lines = stdout.strip().splitlines()[:-1]
    assert any("check_box" in line for line in result_lines), (
        f"expected 'check_box' role in output:\n{stdout}"
    )


def test_find_pretty_output_contains_state_info(run_cli, app_pid):
    """Pretty output includes state information (enabled/disabled) for each element."""
    rc, stdout, stderr = run_cli("find", "button", "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    # The pretty formatter always emits a state block like [enabled visible ...].
    assert "enabled" in stdout or "disabled" in stdout, (
        f"expected state info in pretty output:\n{stdout}"
    )


# Apps whose test page shows a password entry named "Password" (#522).
PASSWORD_APPS = {"tauri", "electron"}


def test_find_lists_the_password_state(run_cli, app_pid, app_name):
    """``xa11y find`` shows `password` among a password entry's states."""
    if app_name not in PASSWORD_APPS:
        pytest.skip(f"the {app_name} test app shows no password entry")
    rc, stdout, stderr = run_cli(
        "find", 'text_field[password="true"]', "--pid", str(app_pid)
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    result_lines = stdout.strip().splitlines()[:-1]
    assert len(result_lines) == 1, stdout
    assert "Password" in result_lines[0], stdout
    states = re.search(r"\[([^\]]*)\]", result_lines[0])
    assert states and "password" in states.group(1).split(), result_lines[0]


def test_find_says_why_a_scrolled_out_element_is_hidden(run_cli, app_pid, app_name):
    """A hidden element's state block names the reason (#590).

    Only the WinForms app has a control scrolled out of its pane (the
    ``Long Pane`` fixture of issue #502); a visible one carries no reason.
    """
    if app_name != "winforms":
        pytest.skip("only the WinForms app has a scrolled-out control")
    rc, stdout, stderr = run_cli("find", '[name="Long Entry 10"]', "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "hidden=scrolled_out" in stdout, stdout
    rc, stdout, stderr = run_cli("find", '[name="Long Entry 1"]', "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert "hidden" not in stdout.splitlines()[0], stdout


# ── find --stable (#530) ──────────────────────────────────────────────────────
#
# "Start slide" restarts a two-second slide of "Slide target" that ends where
# the target already rests. Apps without the widget skip.

_START_SLIDE = 'button[name="Start slide"]'
_SLIDE_TARGET = 'button[name="Slide target"]'


def _stable_bounds(run_cli, app_pid) -> str:
    rc, stdout, stderr = run_cli(
        "find", _SLIDE_TARGET, "--pid", str(app_pid), "-o", "bounds", "--stable"
    )
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    lines = stdout.strip().splitlines()
    assert len(lines) == 1, f"--stable prints exactly the one match: {stdout!r}"
    return lines[0]


def test_find_stable_prints_the_bounds_a_slide_ends_at(run_cli, app, app_pid):
    """``find --stable`` waits out the slide and prints where it ended."""
    if app.locator(_START_SLIDE).count() == 0:
        pytest.skip("this app has no sliding widget")
    rest = _stable_bounds(run_cli, app_pid)

    app.locator(_START_SLIDE).press()
    started = time.monotonic()
    settled = _stable_bounds(run_cli, app_pid)
    waited = time.monotonic() - started
    assert settled == rest, f"--stable printed mid-slide bounds after {waited:.2f}s"
    assert waited >= 1.5, f"a 2 s slide cannot be proven stable in {waited:.2f}s"


def test_find_stable_refuses_an_ambiguous_selector(run_cli, app_pid):
    """With ``--stable`` the selector must match exactly one element."""
    rc, _stdout, stderr = run_cli(
        "find", "button", "--pid", str(app_pid), "-o", "center", "--stable"
    )
    assert rc == 1, f"expected exit 1, got {rc}\nstderr: {stderr}"
    assert "Ambiguous selector" in stderr, stderr
    assert "button" in stderr, f"the candidates should be listed: {stderr}"


def test_find_stable_reports_a_miss_after_the_wait(run_cli, app_pid):
    """A selector that never matches fails as not-found, not as a timeout."""
    env = {**os.environ, "XA11Y_DEFAULT_TIMEOUT": "0.5"}
    rc, stdout, stderr = run_cli(
        "find", 'button[name="NoSuchButton"]', "--pid", str(app_pid), "--stable",
        env=env,
    )
    assert rc == 1, f"expected exit 1, got {rc}\nstderr: {stderr}"
    assert not stdout.strip(), stdout
    assert "No element matched selector" in stderr, f"a miss is not a timeout: {stderr}"
    assert "NoSuchButton" in stderr, stderr
