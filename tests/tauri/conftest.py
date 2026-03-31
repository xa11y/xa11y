"""Fixtures for xa11y Tauri integration tests."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

import pytest

from tests.helpers import launch_test_app

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BINARY = str(PROJECT_ROOT / "target" / "debug" / "xa11y-tauri-test-app")
APP_NAMES = ["xa11y-tauri-test-app"]


def _ensure_built() -> None:
    """Build the Tauri test app if the binary doesn't exist."""
    if not Path(BINARY).exists():
        result = subprocess.run(
            ["cargo", "build", "-p", "xa11y-tauri-test-app"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Tauri test app:\n{result.stdout}\n{result.stderr}"
            )


@pytest.fixture(scope="session")
def tauri_app():
    """Build and launch the Tauri test app; return an xa11y Element handle."""
    _ensure_built()
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[BINARY],
        app_names=APP_NAMES,
    )
