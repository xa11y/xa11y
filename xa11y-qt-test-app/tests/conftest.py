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
STARTUP_TIMEOUT = 30  # seconds to wait for the app to appear

# Names the Qt app might register under depending on platform
APP_NAMES = ["xa11y-qt-test-app", "python3", "python", "Python"]


@pytest.fixture(scope="session")
def qt_app():
    """Launch the Qt test app and return an xa11y.App handle."""
    pid_file = tempfile.mktemp(suffix=".pid")

    env = os.environ.copy()
    env["QT_ACCESSIBILITY"] = "1"

    proc = subprocess.Popen(
        [sys.executable, APP_SCRIPT, "--pid-file", pid_file],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    app = None
    deadline = time.monotonic() + STARTUP_TIMEOUT
    last_err = None
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            out = proc.stdout.read().decode() if proc.stdout else ""
            err = proc.stderr.read().decode() if proc.stderr else ""
            pytest.fail(
                f"Qt test app exited early (code {proc.returncode}).\n"
                f"stdout: {out}\nstderr: {err}"
            )

        # Try by PID first (most reliable), then by name
        candidate = None
        try:
            candidate = xa11y.app(pid=proc.pid)
        except (xa11y.AppNotFoundError, xa11y.PlatformError) as e:
            last_err = e

        if candidate is None:
            for name in APP_NAMES:
                try:
                    candidate = xa11y.app(name)
                    break
                except (xa11y.AppNotFoundError, xa11y.PlatformError) as e:
                    last_err = e

        if candidate is not None:
            app = candidate
            break

        time.sleep(0.5)

    if app is None:
        proc.terminate()
        proc.wait(timeout=5)
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        pytest.fail(
            f"Qt test app (pid={proc.pid}) not found after {STARTUP_TIMEOUT}s.\n"
            f"Last error: {last_err}\n"
            f"stdout: {out}\nstderr: {err}"
        )

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
