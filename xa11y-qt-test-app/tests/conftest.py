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
import threading
import time
from pathlib import Path

import pytest
import xa11y

APP_SCRIPT = str(Path(__file__).resolve().parent.parent / "app.py")
STARTUP_TIMEOUT = 30  # seconds to wait for the app to appear
CALL_TIMEOUT = 5  # seconds per individual xa11y call


def _try_app_call(result_holder, **kwargs):
    """Run xa11y.app() in a thread so we can timeout if it blocks."""
    try:
        result_holder["app"] = xa11y.app(**kwargs)
    except Exception as e:
        result_holder["error"] = e


def _find_app_with_timeout(pid, names, timeout=CALL_TIMEOUT):
    """Try finding the app by PID then by name, with a timeout on each call."""
    # Try by PID
    holder: dict = {}
    t = threading.Thread(
        target=_try_app_call, kwargs={"result_holder": holder, "pid": pid}
    )
    t.start()
    t.join(timeout=timeout)
    if "app" in holder:
        return holder["app"]

    # Try by known names
    for name in names:
        holder = {}
        t = threading.Thread(
            target=_try_app_call, kwargs={"result_holder": holder, "name": name}
        )
        t.start()
        t.join(timeout=timeout)
        if "app" in holder:
            return holder["app"]

    return None


# Names the Qt app might register under depending on platform
APP_NAMES = ["xa11y-qt-test-app", "python3", "python", "Python"]


@pytest.fixture(scope="session")
def qt_app():
    """Launch the Qt test app and return an xa11y.App handle.

    The app is killed when the test session ends.
    """
    pid_file = tempfile.mktemp(suffix=".pid")

    env = os.environ.copy()
    # Ensure Qt uses the platform accessibility bridge
    env["QT_ACCESSIBILITY"] = "1"
    # The CI harness is responsible for providing a display (Xvfb on Linux).

    proc = subprocess.Popen(
        [sys.executable, APP_SCRIPT, "--pid-file", pid_file],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # Wait for the app to register with platform accessibility.
    app = None
    deadline = time.monotonic() + STARTUP_TIMEOUT
    while time.monotonic() < deadline:
        # Check the process hasn't died
        if proc.poll() is not None:
            out = proc.stdout.read().decode() if proc.stdout else ""
            err = proc.stderr.read().decode() if proc.stderr else ""
            pytest.fail(
                f"Qt test app exited early (code {proc.returncode}).\n"
                f"stdout: {out}\nstderr: {err}"
            )

        app = _find_app_with_timeout(proc.pid, APP_NAMES)
        if app is not None:
            break

        time.sleep(0.5)

    if app is None:
        proc.terminate()
        proc.wait(timeout=5)
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        pytest.fail(
            f"Qt test app (pid={proc.pid}) not found after {STARTUP_TIMEOUT}s.\n"
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
