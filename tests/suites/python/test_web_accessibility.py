"""App.enable_web_accessibility against a real Electron app (issue #511).

On macOS the method asks the app to build its web content's accessibility
tree. The suite's own Electron instance is launched with that tree already on,
so this test launches a second one with it off, the way a user's Electron app
starts, calls the method, and checks that the page's button is reachable.
It does not assert an empty tree beforehand: Chromium builds the tree lazily
on the first AX query, so the off switch gives no deterministic pre-state.

Linux and Windows have no such request: Chromium's tree there is a launch-time
flag (Linux) or built on demand for any UI Automation client (Windows). The
method says so with a clear refusal, and that refusal is what those cells
check.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

import xa11y

PROJECT_ROOT = Path(__file__).resolve().parents[3]
ELECTRON_DIR = PROJECT_ROOT / "test-apps" / "electron"
OK_BUTTON = 'button[name="OK"]'


def _electron_binary() -> str:
    """The Electron executable itself, not the `.bin/electron` node shim.

    The shim spawns Electron as a child, so its pid is not the app's.
    `require("electron")` from the app's directory returns the real path.
    """
    out = subprocess.run(
        ["node", "-e", 'process.stdout.write(require("electron"))'],
        cwd=ELECTRON_DIR,
        capture_output=True,
        check=True,
        text=True,
        timeout=30,
    )
    return out.stdout.strip()


@pytest.fixture
def electron_without_web_accessibility():
    """A second Electron test app, started with web accessibility off."""
    proc = subprocess.Popen(
        [
            _electron_binary(),
            str(ELECTRON_DIR / "main.js"),
            "--xa11y-web-accessibility=off",
            "--xa11y-app-name=xa11y-electron-web-a11y-off",
        ],
        cwd=ELECTRON_DIR,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        yield xa11y.App.by_pid(proc.pid, timeout=30)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@pytest.fixture
def electron_only(app_name):
    if app_name != "electron":
        pytest.skip("enable_web_accessibility is exercised against the Electron app")


@pytest.mark.skipif(sys.platform != "darwin", reason="the request exists only on macOS")
def test_enable_web_accessibility_makes_web_content_reachable(
    electron_only, electron_without_web_accessibility
):
    app = electron_without_web_accessibility
    # No "empty before" check: Chromium builds its web tree lazily on the first
    # AX query, so the off switch gives no deterministic empty pre-state.
    app.enable_web_accessibility(timeout=30)

    assert app.locator(OK_BUTTON).element().name == "OK"


@pytest.mark.skipif(sys.platform != "darwin", reason="the request exists only on macOS")
def test_enable_web_accessibility_is_idempotent(electron_only, app):
    # The suite's app was launched with the tree already on: the request is
    # still taken, and its content is already there.
    app.enable_web_accessibility(timeout=10)
    app.enable_web_accessibility(timeout=10)
    assert app.locator(OK_BUTTON).element().name == "OK"


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS takes the request")
def test_enable_web_accessibility_is_refused_with_guidance(electron_only, app):
    with pytest.raises(xa11y.ActionNotSupportedError) as refused:
        app.enable_web_accessibility(timeout=0)
    message = str(refused.value)
    if sys.platform.startswith("linux"):
        assert "--force-renderer-accessibility" in message, message
        assert "ACCESSIBILITY_ENABLED=1" in message, message
    else:
        assert "UI Automation" in message, message
