"""CLI integration tests for ``xa11y screenshot``.

Screenshot requires pixel-capture permission (Screen Recording on macOS,
a compositor or X11 DISPLAY on Linux, no grant on Windows). When the
current session has no capture path the backend exits non-zero with an
"unsupported" or "permission" message; those cases are treated as skips so
the tests remain useful across headless and headed CI runners.

Argument-validation error paths (missing --out, bad --region) are covered by
the unit tests in xa11y-python/tests/test_cli.py and are not repeated here.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


# PNG magic bytes used to validate output files.
_PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


# ── Helpers ───────────────────────────────────────────────────────────────────


def _skip_if_unsupported(rc: int, stderr: str) -> None:
    """Skip the test when the CLI reports that capture is unavailable."""
    if rc != 0:
        lower = stderr.lower()
        if any(kw in lower for kw in ("unsupported", "permission", "access denied",
                                      "getimage", "badmatch")):
            pytest.skip(f"screen capture not available in this session: {stderr.strip()}")


# ── Tests ─────────────────────────────────────────────────────────────────────


def test_screenshot_full_display_to_file(run_cli, app_pid, tmp_path):
    """``xa11y screenshot --out <path>`` should write a valid PNG."""
    out = tmp_path / "shot.png"
    rc, stdout, stderr = run_cli("screenshot", "--out", str(out))
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert out.exists(), f"expected output file to exist at {out}"
    data = out.read_bytes()
    assert data[:8] == _PNG_MAGIC, "output file does not start with PNG magic bytes"
    assert len(data) > 100, "PNG file is suspiciously small"


def test_screenshot_region_to_file(run_cli, app_pid, tmp_path):
    """``xa11y screenshot --region X,Y,W,H --out <path>`` should write a valid PNG."""
    out = tmp_path / "region.png"
    rc, stdout, stderr = run_cli(
        "screenshot", "--region", "0,0,100,80", "--out", str(out)
    )
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert out.exists(), f"expected output file to exist at {out}"
    data = out.read_bytes()
    assert data[:8] == _PNG_MAGIC, "output file does not start with PNG magic bytes"


def test_screenshot_to_stdout(run_cli, cli_bin, app_pid):
    """``xa11y screenshot --out -`` should write PNG bytes to stdout."""
    # Run in binary mode so we can inspect the raw PNG magic bytes.
    result = subprocess.run(
        cli_bin + ["screenshot", "--out", "-"],
        capture_output=True,
        timeout=30,
    )
    _skip_if_unsupported(result.returncode, result.stderr.decode(errors="replace"))
    assert result.returncode == 0, (
        f"expected exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr.decode(errors='replace')}"
    )
    assert result.stdout[:8] == _PNG_MAGIC, (
        "stdout PNG magic bytes not found; got: " + result.stdout[:16].hex()
    )


def test_screenshot_stderr_reports_dimensions(run_cli, app_pid, tmp_path):
    """When writing to a file, the CLI reports dimensions on stderr."""
    out = tmp_path / "dims.png"
    rc, stdout, stderr = run_cli("screenshot", "--out", str(out))
    _skip_if_unsupported(rc, stderr)
    assert rc == 0
    # The CLI prints "wrote <path> (WxH @Sx)" to stderr.
    assert "wrote" in stderr, (
        f"expected 'wrote ...' message in stderr:\n{stderr}"
    )


# ── Annotation flags ──────────────────────────────────────────────────────────
#
# `--annotate` opts the command into the accessibility tree: it resolves
# selectors against a target and prints a legend on stdout mapping each drawn
# box back to a selector that acts on it. Without it, nothing here changes.
#
# Argument validation happens before any capture, so the refusal tests below
# need neither a display nor capture permission and never call
# `_skip_if_unsupported`.


def _annotated_legend(run_cli, app_pid, tmp_path, *extra: str) -> tuple[str, str]:
    """Run an annotated capture, skipping when this session cannot capture."""
    out = tmp_path / "annotated.png"
    rc, stdout, stderr = run_cli(
        "screenshot", "--pid", str(app_pid), "--annotate", "button",
        "--out", str(out), *extra,
    )
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    assert out.exists(), f"expected output file to exist at {out}"
    assert out.read_bytes()[:8] == _PNG_MAGIC
    return stdout, stderr


def test_annotate_writes_a_png_and_a_text_legend(run_cli, app_pid, tmp_path):
    """``--annotate`` still produces a PNG, plus a legend on stdout."""
    stdout, _ = _annotated_legend(run_cli, app_pid, tmp_path)
    # The group header is always present, even when nothing matched, so the
    # flag never reads as if it had been ignored.
    assert stdout.startswith("A  button"), f"expected a group header:\n{stdout}"
    assert "annotated" in stdout


def test_annotate_legend_none_prints_nothing(run_cli, app_pid, tmp_path):
    """``--legend none`` draws the boxes but prints no legend."""
    stdout, _ = _annotated_legend(run_cli, app_pid, tmp_path, "--legend", "none")
    assert stdout == "", f"expected no legend on stdout, got:\n{stdout}"


def test_annotate_legend_json_is_parseable(run_cli, app_pid, tmp_path):
    """``--legend json`` prints one JSON object with the documented keys."""
    stdout, _ = _annotated_legend(run_cli, app_pid, tmp_path, "--legend", "json")
    doc = json.loads(stdout)
    assert set(doc) >= {"groups", "legend", "omitted", "truncated", "cap"}
    assert doc["groups"][0]["letter"] == "A"
    assert doc["groups"][0]["selector"] == "button"
    assert doc["groups"][0]["color_hex"].startswith("#")
    assert isinstance(doc["truncated"], int)
    for entry in doc["legend"]:
        # The round trip the feature exists for.
        assert entry["selector"] == f"button:nth({entry['index']})"
        assert entry["tag"] == f"A{entry['index']}"
        assert entry["group"] == 1
        assert set(entry["bounds"]) == {"x", "y", "width", "height"}
    for omission in doc["omitted"]:
        assert omission["reason"] in ("no_bounds", "zero_area", "outside_capture")


def test_repeated_annotate_makes_two_groups(run_cli, app_pid, tmp_path):
    """Each ``--annotate`` occurrence is its own group, letter and colour."""
    out = tmp_path / "two-groups.png"
    rc, stdout, stderr = run_cli(
        "screenshot", "--pid", str(app_pid),
        "--annotate", "button", "--annotate", "text_field",
        "--out", str(out), "--legend", "json",
    )
    _skip_if_unsupported(rc, stderr)
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    doc = json.loads(stdout)
    assert [g["letter"] for g in doc["groups"]] == ["A", "B"]
    assert [g["selector"] for g in doc["groups"]] == ["button", "text_field"]
    assert doc["groups"][0]["color"] != doc["groups"][1]["color"]
    for entry in doc["legend"]:
        letter = "A" if entry["group"] == 1 else "B"
        assert entry["tag"] == f"{letter}{entry['index']}"


def test_annotate_without_a_target_is_a_usage_error(run_cli, tmp_path):
    """``--annotate`` needs something to search, and the error names the flags."""
    out = tmp_path / "never.png"
    rc, stdout, stderr = run_cli("screenshot", "--annotate", "button", "--out", str(out))
    assert rc == 2, f"expected exit 2 (usage), got {rc}\nstderr: {stderr}"
    assert "--app NAME" in stderr and "--pid PID" in stderr, stderr
    assert not out.exists(), "a usage error must not leave a capture behind"


def test_annotate_to_stdout_with_a_legend_is_a_usage_error(run_cli, app_pid):
    """PNG bytes and legend text cannot share stdout; the CLI names both fixes."""
    rc, stdout, stderr = run_cli(
        "screenshot", "--pid", str(app_pid), "--annotate", "button", "--out", "-"
    )
    assert rc == 2, f"expected exit 2 (usage), got {rc}\nstderr: {stderr}"
    assert "--out FILE" in stderr, stderr
    assert "--legend none" in stderr, stderr
    assert stdout == "", "nothing may reach stdout on a usage error"


def test_unknown_legend_value_is_a_usage_error(run_cli, app_pid, tmp_path):
    """``--legend`` accepts exactly text|json|none."""
    out = tmp_path / "never.png"
    rc, _stdout, stderr = run_cli(
        "screenshot", "--pid", str(app_pid), "--annotate", "button",
        "--legend", "yaml", "--out", str(out),
    )
    assert rc == 2, f"expected exit 2 (usage), got {rc}\nstderr: {stderr}"
    assert "text|json|none" in stderr, stderr


def test_legend_without_annotate_is_a_usage_error(run_cli, tmp_path):
    """``--legend`` alone has nothing to describe rather than silently doing nothing."""
    out = tmp_path / "never.png"
    rc, _stdout, stderr = run_cli("screenshot", "--legend", "json", "--out", str(out))
    assert rc == 2, f"expected exit 2 (usage), got {rc}\nstderr: {stderr}"
    assert "--annotate SELECTOR" in stderr, stderr


def test_plain_capture_still_prints_no_legend(run_cli, app_pid, tmp_path):
    """Without ``--annotate`` the command is exactly what it was before."""
    out = tmp_path / "plain.png"
    rc, stdout, stderr = run_cli("screenshot", "--out", str(out))
    _skip_if_unsupported(rc, stderr)
    assert rc == 0
    assert stdout == "", f"an unannotated capture writes nothing to stdout:\n{stdout}"
