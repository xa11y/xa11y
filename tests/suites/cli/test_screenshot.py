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
