"""Fixtures for xa11y Tauri integration tests."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

import pytest

from tests.helpers import launch_test_app

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
# The Tauri app is not part of the Cargo workspace, so cargo builds it
# into its own target directory rather than the workspace target dir.
BINARY = str(PROJECT_ROOT / "test-apps" / "tauri" / "target" / "debug" / "xa11y-tauri-test-app")
APP_NAMES = ["xa11y-tauri-test-app"]


def _ensure_built() -> None:
    """Build the Tauri test app if the binary doesn't exist."""
    if not Path(BINARY).exists():
        result = subprocess.run(
            ["cargo", "build", "--manifest-path", str(PROJECT_ROOT / "test-apps" / "tauri" / "Cargo.toml")],
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
    """Build and launch the Tauri test app; return an xa11y App handle."""
    _ensure_built()
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[BINARY],
        app_names=APP_NAMES,
        content_ready_selector='button[name="OK"]',
    )


@pytest.fixture(scope="module")
def tauri_input_app():
    """Launch the Tauri app and navigate to the input-events page.

    A module-scoped fixture (separate process) so the event log starts
    empty and focus state doesn't bleed in from the widget tests.
    """
    import time

    _ensure_built()
    gen = launch_test_app(
        command=[BINARY],
        app_names=APP_NAMES,
        content_ready_selector='button[name="OK"]',
    )
    app = next(gen)
    # Click the link to navigate to input-events.html, then wait for the
    # hit target to appear in the a11y tree before handing off to tests.
    app.locator('link[name="input events →"]').press()
    for _ in range(50):
        if app.locator('region[name="Hit target"]').exists():
            break
        time.sleep(0.1)
    else:
        pytest.fail("input-events page did not load within 5s")
    try:
        yield app
    finally:
        # Advance the launch generator to run its teardown.
        for _ in gen:
            pass
