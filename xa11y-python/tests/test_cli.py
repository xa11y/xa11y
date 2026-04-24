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


# ── Input simulation: argument validation ────────────────────────────────────
#
# These exercise the dispatch → arg-parsing path without synthesising any
# actual input. Each hits a validation error before reaching the platform
# backend, so they are safe to run in CI (no input events are generated).


def test_click_without_at_errors():
    result = run_cli("click")
    assert result.returncode != 0
    assert "--at" in result.stderr


def test_click_bad_at_format_errors():
    result = run_cli("click", "--at", "notacoord")
    assert result.returncode != 0
    assert "--at" in result.stderr


def test_click_bad_button_errors():
    result = run_cli("click", "--at", "10,20", "--button", "nope")
    assert result.returncode != 0
    assert "button" in result.stderr.lower()


def test_click_uppercase_char_in_held_errors():
    # Key::Char validation rejects ASCII uppercase — the CLI must surface that.
    result = run_cli("click", "--at", "10,20", "--held", "A")
    assert result.returncode != 0


def test_drag_missing_endpoints_errors():
    result = run_cli("drag", "--from", "0,0")
    assert result.returncode != 0
    assert "--to" in result.stderr


def test_scroll_without_at_errors():
    result = run_cli("scroll", "--dy", "3")
    assert result.returncode != 0
    assert "--at" in result.stderr


def test_key_without_name_errors():
    result = run_cli("key")
    assert result.returncode != 0
    assert "KEY" in result.stderr or "usage" in result.stderr.lower()


def test_key_unknown_name_errors():
    result = run_cli("key", "NotARealKey")
    assert result.returncode != 0


def test_type_without_text_errors():
    result = run_cli("type")
    assert result.returncode != 0


# ── Screenshot: argument validation ──────────────────────────────────────────


def test_screenshot_without_out_errors():
    result = run_cli("screenshot")
    assert result.returncode != 0
    assert "--out" in result.stderr


def test_screenshot_bad_region_errors():
    result = run_cli("screenshot", "--region", "10,20,30", "--out", "x.png")
    assert result.returncode != 0
    assert "--region" in result.stderr


# ── find -o output format ────────────────────────────────────────────────────


def test_find_unknown_output_format_errors():
    # -o is parsed after resolve_app, so this needs a valid --app. Use a name
    # that won't be found — the "app not found" error is fine; the point is
    # that the flag parser itself doesn't crash or accept the bogus format
    # silently. On platforms without a live provider this may still reach
    # resolve_app first; either way the command must exit non-zero.
    result = run_cli("find", "button", "--app", "NonexistentApp12345", "-o", "bogus")
    assert result.returncode != 0


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
