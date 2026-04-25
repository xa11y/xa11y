"""CLI integration tests for ``xa11y tree``.

Verifies that the tree command exits cleanly and produces recognisable
accessibility-tree output against a live test app.
"""

from __future__ import annotations

import pytest


def test_tree_exits_zero(run_cli, app_pid):
    """``xa11y tree --pid <pid>`` should exit 0."""
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid))
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"


def test_tree_output_is_non_empty(run_cli, app_pid):
    """Tree output should contain content."""
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid))
    assert rc == 0
    assert stdout.strip(), "expected non-empty tree output"


def test_tree_contains_window(run_cli, app_pid):
    """The tree should contain a top-level container.

    Some toolkits expose the toplevel as ``window`` (Cocoa/Tauri/UIA) and
    others as ``group`` (GTK4/AT-SPI) — accept either as evidence that we
    walked into the app's tree at all.
    """
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid))
    assert rc == 0
    assert "window" in stdout or "group" in stdout, (
        f"expected 'window' or 'group' in tree output:\n{stdout[:500]}"
    )


def test_tree_contains_button(run_cli, app_pid):
    """The test app exposes buttons — they should appear somewhere in the tree."""
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid))
    assert rc == 0
    assert "button" in stdout, f"expected 'button' in tree output:\n{stdout[:500]}"


def test_tree_output_has_tree_connectors(run_cli, app_pid):
    """The tree formatter uses box-drawing connectors (├──, └──)."""
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid))
    assert rc == 0
    # Either connector indicates the recursive tree printer ran correctly.
    assert "├──" in stdout or "└──" in stdout, (
        f"expected tree connectors in output:\n{stdout[:500]}"
    )


def test_tree_without_app_exits_nonzero(run_cli):
    """``xa11y tree`` with no --app/--pid flag must fail with a helpful message."""
    rc, stdout, stderr = run_cli("tree")
    assert rc != 0, "expected non-zero exit when no app is specified"
    assert "--app" in stderr or "--pid" in stderr, (
        f"expected --app/--pid hint in stderr:\n{stderr}"
    )
