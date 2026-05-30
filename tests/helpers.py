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
FRONTMOST_TIMEOUT = 10  # seconds


def _osascript(script: str, timeout: float = 5.0) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["osascript", "-e", script],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _macos_frontmost() -> tuple[int | None, str]:
    """Return (unix pid, name) of the current macOS frontmost app process."""
    try:
        result = _osascript(
            'tell application "System Events" to tell '
            "(first application process whose frontmost is true) "
            'to return (unix id as text) & "\t" & name'
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as exc:
        return None, f"<osascript error: {exc!r}>"
    if result.returncode != 0:
        return None, f"<rc={result.returncode}: {result.stderr.strip()}>"
    pid_str, _, name = result.stdout.strip().partition("\t")
    try:
        return int(pid_str), name or "<unknown>"
    except ValueError:
        return None, result.stdout.strip() or "<empty>"


def ensure_macos_frontmost(
    pid: int, *, timeout: float = FRONTMOST_TIMEOUT
) -> tuple[bool, str]:
    """Make process ``pid`` the frontmost macOS app and verify it stuck.

    On macOS ``CGEventPost`` (input_sim) and OS-level focus both target
    whichever app is frontmost. Runner images occasionally boot with an
    onboarding or background process holding the front slot (Setup Assistant,
    Notification Center, Software Update, …), which silently misdirects every
    synthetic event so the test reads an empty event log.

    Rather than maintain a hardcoded kill-list of offenders — which rots every
    time GitHub rolls a new runner image — actively claim the front via System
    Events and poll until our PID is verified frontmost. This is
    runner-image-agnostic: it doesn't matter *what* grabbed focus, we take it
    back, and on failure we name the offender so CI points straight at it.

    No-op on non-macOS. Returns ``(ok, detail)``.
    """
    if sys.platform != "darwin":
        return True, "not macOS"

    activate = (
        'tell application "System Events" to set frontmost of '
        f"(first process whose unix id is {pid}) to true"
    )
    deadline = time.monotonic() + timeout
    front_pid: int | None = None
    front_name = "<unknown>"
    while time.monotonic() < deadline:
        try:
            _osascript(activate)
        except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
            pass  # transient — retry until the deadline
        front_pid, front_name = _macos_frontmost()
        if front_pid == pid:
            return True, f"frontmost (pid={pid})"
        time.sleep(0.3)

    return False, (
        f"test app (pid={pid}) is not frontmost after {timeout:.0f}s; frontmost "
        f"is {front_name!r} (pid={front_pid}). On macOS CGEventPost delivers "
        f"synthetic events to the frontmost app, so input_sim and OS-focus tests "
        f"cannot run reliably. A runner-image process grabbed the front slot — "
        f"see https://github.com/xa11y/xa11y/issues/230."
    )


def launch_test_app(
    command: list[str],
    app_names: list[str],
    env_overrides: dict[str, str] | None = None,
    startup_timeout: int = STARTUP_TIMEOUT,
    content_ready_selector: str | None = None,
    require_frontmost: bool = False,
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
        require_frontmost: On macOS, actively bring the app to the front and
            verify it before yielding (see ensure_macos_frontmost). Set this for
            apps with a real, activatable window whose tests depend on holding
            the frontmost slot (input_sim via CGEventPost, OS-focus assertions).
            No-op off macOS.

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

        # Try exact match first (fast path on Linux/macOS)
        for name in app_names:
            try:
                app = xa11y.App.by_name(name)
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError):
                pass

        # Fall back to listing all apps and substring matching (needed when
        # the app name includes extra text like the PID)
        if app is None:
            try:
                all_running = xa11y.App.list()
                for name in app_names:
                    for candidate in all_running:
                        if name.lower() in (candidate.name or "").lower():
                            app = candidate
                            break
                    if app is not None:
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

    if require_frontmost and sys.platform == "darwin":
        ok, detail = ensure_macos_frontmost(app.pid)
        if not ok:
            proc.terminate()
            proc.wait(timeout=5)
            pytest.fail(detail)

    yield app

    try:
        if sys.platform == "win32":
            proc.terminate()
        else:
            proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        proc.kill()
