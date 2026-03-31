"""Fixtures for xa11y Qt integration tests.

Launches the PySide6 test app as a subprocess, waits for it to register
with the platform accessibility API, then yields an xa11y Element handle.
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
APP_NAMES = ["xa11y-qt-test-app", "xa11y", "python3", "python", "Python", "app.py"]


@pytest.fixture(scope="session")
def qt_app():
    """Launch the Qt test app and return an xa11y Element handle."""
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

        # Try each known app name
        for name in APP_NAMES:
            try:
                loc = xa11y.locator(f'application[name*="{name}"]')
                candidate = loc.element()
                app = candidate
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError) as e:
                last_err = e

        if app is not None:
            break

        time.sleep(0.5)

    if app is None:
        # Dump available apps for debugging
        try:
            all_apps = xa11y.locator("application").elements()
            app_list = [(a.name, a.pid) for a in all_apps]
        except Exception:
            app_list = "<failed to list>"
        proc.terminate()
        proc.wait(timeout=5)
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        pytest.fail(
            f"Qt test app (pid={proc.pid}) not found after {STARTUP_TIMEOUT}s.\n"
            f"Last error: {last_err}\n"
            f"Available apps: {app_list}\n"
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
