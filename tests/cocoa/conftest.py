"""Fixtures for xa11y Cocoa/AppKit integration tests (macOS only)."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

import pytest

from tests.helpers import launch_test_app

BINARY = str(Path(__file__).resolve().parent.parent.parent / "test-apps" / "cocoa" / "xa11y-cocoa-test-app")
APP_NAMES = ["xa11y-cocoa-test-app"]


def _ensure_built() -> None:
    """Build the Cocoa test app if the binary doesn't exist."""
    binary = Path(BINARY)
    if not binary.exists():
        makefile_dir = binary.parent
        result = subprocess.run(
            ["make", "build"],
            cwd=makefile_dir,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Cocoa test app:\n{result.stdout}\n{result.stderr}"
            )


@pytest.fixture(scope="session")
def cocoa_app():
    """Build and launch the Cocoa test app; return an xa11y App handle."""
    _ensure_built()
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[BINARY, "--headless", "--pid-file", pid_file],
        app_names=APP_NAMES,
    )
