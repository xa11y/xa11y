"""Tests for the xa11y CLI (Rust implementation via _cli_main)."""

import subprocess
import sys

import pytest


def run_cli(*args: str) -> subprocess.CompletedProcess:
    """Run the xa11y CLI via the Python entry point."""
    return subprocess.run(
        [sys.executable, "-m", "xa11y._cli", *args],
        capture_output=True,
        text=True,
        timeout=10,
    )


# ── Usage / help ─────────────────────────────────────────────────────────────


def test_no_args_prints_usage():
    result = run_cli()
    assert result.returncode == 0
    assert "xa11y" in result.stderr
    assert "apps" in result.stderr
    assert "tree" in result.stderr
    assert "find" in result.stderr
    assert "action" in result.stderr
    assert "events" in result.stderr


def test_unknown_command_prints_usage():
    result = run_cli("bogus")
    assert result.returncode == 0
    assert "Usage" in result.stderr


# ── Error cases ──────────────────────────────────────────────────────────────


def test_tree_without_app_flag_errors():
    result = run_cli("tree")
    assert result.returncode != 0
    assert "--app" in result.stderr or "--pid" in result.stderr


def test_find_without_selector_errors():
    result = run_cli("find", "--app", "NonexistentApp12345")
    assert result.returncode != 0


def test_action_missing_args_errors():
    result = run_cli("action")
    assert result.returncode != 0


def test_action_without_app_errors():
    result = run_cli("action", "press", "button")
    assert result.returncode != 0
    assert "--app" in result.stderr or "--pid" in result.stderr


# ── Rust CLI module is importable ────────────────────────────────────────────


def test_cli_main_importable():
    from xa11y._native import _cli_main

    assert callable(_cli_main)


def test_cli_main_no_args_does_not_crash():
    """Calling _cli_main with no args should print usage, not crash."""
    from xa11y._native import _cli_main

    # No args → prints usage to stderr, returns Ok(())
    _cli_main([])


def test_cli_main_tree_no_app_raises():
    """tree without --app should raise an error."""
    from xa11y._native import _cli_main

    with pytest.raises(Exception, match=r"--app|--pid"):
        _cli_main(["tree"])


def test_cli_main_action_missing_args_raises():
    from xa11y._native import _cli_main

    with pytest.raises(Exception, match=r"usage|ACTION"):
        _cli_main(["action"])
