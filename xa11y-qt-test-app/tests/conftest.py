"""Fixtures for xa11y Qt integration tests.

Launches the PySide6 test app as a subprocess, waits for it to register
with the platform accessibility API, then yields an xa11y.App handle.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest
import xa11y

APP_SCRIPT = str(Path(__file__).resolve().parent.parent / "app.py")
STARTUP_TIMEOUT = 15  # seconds to wait for the app to appear


@pytest.fixture(scope="session")
def qt_app():
    """Launch the Qt test app and return an xa11y.App handle.

    The app is killed when the test session ends.
    """
    pid_file = tempfile.mktemp(suffix=".pid")

    env = os.environ.copy()
    # Ensure Qt uses the platform accessibility bridge
    env["QT_ACCESSIBILITY"] = "1"
    # Use offscreen platform on headless CI if no display is available,
    # but prefer xcb/cocoa/windows when a display exists since offscreen
    # doesn't expose accessibility.
    # The CI harness is responsible for providing a display (Xvfb on Linux).

    proc = subprocess.Popen(
        [sys.executable, APP_SCRIPT, "--pid-file", pid_file],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # Wait for the app to register with platform accessibility
    app = None
    deadline = time.monotonic() + STARTUP_TIMEOUT
    last_err = None
    while time.monotonic() < deadline:
        try:
            app = xa11y.app("xa11y-qt-test-app")
            break
        except (xa11y.AppNotFoundError, xa11y.PlatformError) as e:
            last_err = e
            # Check the process hasn't died
            if proc.poll() is not None:
                out = proc.stdout.read().decode() if proc.stdout else ""
                err = proc.stderr.read().decode() if proc.stderr else ""
                pytest.fail(
                    f"Qt test app exited early (code {proc.returncode}).\n"
                    f"stdout: {out}\nstderr: {err}"
                )
            time.sleep(0.5)

    if app is None:
        proc.terminate()
        proc.wait(timeout=5)
        pytest.fail(f"Qt test app not found after {STARTUP_TIMEOUT}s: {last_err}")

    yield app

    # Teardown
    try:
        if sys.platform == "win32":
            proc.terminate()
        else:
            proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        proc.kill()
    finally:
        try:
            os.unlink(pid_file)
        except FileNotFoundError:
            pass
