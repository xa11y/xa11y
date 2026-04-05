"""Shared fixtures for xa11y integration test suites.

Provides launch_test_app(), a reusable generator that:
  - Launches a test app subprocess
  - Polls the accessibility API until the app appears
  - Yields an xa11y App handle
  - Kills the process on teardown
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from typing import Generator

import pytest
import xa11y

STARTUP_TIMEOUT = 30  # seconds


def launch_test_app(
    command: list[str],
    app_names: list[str],
    env_overrides: dict[str, str] | None = None,
    startup_timeout: int = STARTUP_TIMEOUT,
    content_ready_selector: str | None = None,
) -> Generator[xa11y.App, None, None]:
    """Launch a test app and yield its xa11y App handle.

    Args:
        command: The subprocess command to launch the app.
        app_names: Names to search for via accessibility API (tried in order).
        env_overrides: Extra environment variables to set for the subprocess.
        startup_timeout: Seconds to wait for the app to appear.
        content_ready_selector: If set, keep polling until this selector matches
            within the app's tree (useful for WebView apps where UI content
            loads asynchronously after the app window appears).

    Yields:
        The xa11y App for the running application.
    """
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)

    proc = subprocess.Popen(
        command,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    app = None
    deadline = time.monotonic() + startup_timeout
    last_err = None
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            out = proc.stdout.read().decode() if proc.stdout else ""
            err = proc.stderr.read().decode() if proc.stderr else ""
            pytest.fail(
                f"Test app (pid={proc.pid}) exited early (code {proc.returncode}).\n"
                f"stdout: {out}\nstderr: {err}"
            )

        for name in app_names:
            try:
                candidate = xa11y.App.by_name(name)
                app = candidate
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError) as e:
                last_err = e
        if app is not None:
            break

        time.sleep(0.5)

    if app is None:
        try:
            all_apps = xa11y.App.list()
            app_list = [(a.name, a.pid) for a in all_apps]
        except Exception:
            app_list = "<failed to list>"
        proc.terminate()
        proc.wait(timeout=5)
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        pytest.fail(
            f"Test app (pid={proc.pid}) not found after {startup_timeout}s.\n"
            f"Last error: {last_err}\n"
            f"Available apps: {app_list}\n"
            f"stdout: {out}\nstderr: {err}"
        )

    if content_ready_selector is not None:
        content_deadline = time.monotonic() + startup_timeout
        while time.monotonic() < content_deadline:
            try:
                app.locator(content_ready_selector).element()
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError,
                    xa11y.TimeoutError):
                time.sleep(0.5)
        else:
            proc.terminate()
            proc.wait(timeout=5)
            pytest.fail(
                f"Content not ready: selector {content_ready_selector!r} "
                f"not found after {startup_timeout}s."
            )

    yield app

    try:
        if sys.platform == "win32":
            proc.terminate()
        else:
            proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        proc.kill()
