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


def test_tree_max_nodes_cuts_and_says_so(run_cli, app_pid):
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--max-nodes", "3")
    assert rc == 0, stderr
    lines = stdout.strip().splitlines()
    assert lines[-1].startswith("(truncated: 3 nodes shown, cut by the node limit of 3"), stdout
    assert any("more (cut by --max-nodes)" in line for line in lines), stdout


def test_tree_max_depth_zero_shows_only_the_application(run_cli, app_pid):
    rc, stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--max-depth", "0")
    assert rc == 0, stderr
    lines = stdout.strip().splitlines()
    assert lines[0].startswith("application"), stdout
    assert "more (cut by --max-depth)" in lines[1], stdout
    assert lines[-1].startswith("(truncated:"), stdout


def test_tree_detail_prints_its_readings_under_each_element(run_cli, app_pid):
    """`--detail` mirrors MCP `tree`'s `detail`; its lines never read as nodes."""
    rc, stdout, stderr = run_cli(
        "tree", "--pid", str(app_pid), "--detail", "formatting,relations"
    )
    assert rc == 0, stderr
    lines = stdout.strip().splitlines()
    assert lines[0].startswith("application"), stdout
    for line in lines[1:]:
        body = line.lstrip(" │")
        # Every line is a node, a cut marker, a summary, or a detail reading.
        assert body.startswith(
            ("├── ", "└── ", "(truncated", "text (", "run [", "<")
        ) or (" → " in body), line


def test_tree_detail_unknown_name_is_a_usage_error(run_cli, app_pid):
    rc, _stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--detail", "styles")
    assert rc == 2, stderr
    assert "--detail" in stderr, stderr


def test_tree_max_nodes_zero_is_a_usage_error(run_cli, app_pid):
    rc, _stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--max-nodes", "0")
    assert rc == 2, stderr


def test_tree_refuses_a_flag_it_does_not_read(run_cli, app_pid):
    """A flag another command reads used to parse here and be ignored, so
    `tree --limit 5` printed the whole tree."""
    rc, _stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--limit", "5")
    assert rc == 2, stderr
    assert "--limit does not apply" in stderr, stderr


def test_an_unknown_backend_is_a_usage_error_naming_the_flag(run_cli, app_pid):
    rc, _stdout, stderr = run_cli("tree", "--pid", str(app_pid), "--backend", "foo")
    assert rc == 2, stderr
    assert "--backend" in stderr, stderr
    assert "XA11Y_ACCESSIBILITY_SOURCE" not in stderr, stderr
