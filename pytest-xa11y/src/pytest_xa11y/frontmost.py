"""Claiming the macOS frontmost slot.

``CGEventPost`` delivers synthetic input to whichever application is
frontmost, and OS-level focus assertions read the same slot. CI runner images
routinely boot with an onboarding or background process holding it (Setup
Assistant, Notification Center, Software Update), which silently misdirects
every synthetic event — the symptom is an empty event log and a test that
asserts against ``''``.

Rather than maintain a kill-list of offenders, which rots with every runner
image, actively claim the front and poll until our PID is verified there. On
failure the offender is named, so the failure report points at the cause
instead of at the assertion.
"""

from __future__ import annotations

import contextlib
import subprocess
import sys
import time

_POLL_INITIAL = 0.1
_POLL_CAP = 1.0


def _osascript(script: str, timeout: float = 5.0) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["osascript", "-e", script],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def macos_frontmost() -> tuple[int | None, str]:
    """Return ``(pid, name)`` of the current macOS frontmost app process.

    ``pid`` is ``None`` when the lookup itself failed; ``name`` then carries a
    tag describing why, so callers can report the failure rather than treat it
    as "nothing is frontmost".
    """
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


def macos_visible_processes() -> str:
    """Names of all foreground-capable macOS processes, for diagnostics."""
    try:
        result = _osascript(
            'tell application "System Events" to '
            "get name of (every application process whose visible is true)"
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as exc:
        return f"<error: {exc!r}>"
    if result.returncode != 0:
        return f"<osascript rc={result.returncode}: {result.stderr.strip()}>"
    return result.stdout.strip() or "<empty>"


def ensure_macos_frontmost(pid: int, *, timeout: float = 10.0) -> tuple[bool, str]:
    """Make process ``pid`` frontmost and verify it stuck.

    No-op off macOS. Returns ``(ok, detail)``; on failure ``detail`` names
    whichever process holds the front instead.
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
    delay = _POLL_INITIAL
    while time.monotonic() < deadline:
        # Transient failures are expected: System Events is not always
        # immediately scriptable on a freshly booted runner. Retry until the
        # deadline — a persistent failure surfaces below as "not frontmost",
        # with the offender named, so nothing is actually swallowed here.
        with contextlib.suppress(subprocess.TimeoutExpired, FileNotFoundError, OSError):
            _osascript(activate)
        front_pid, front_name = macos_frontmost()
        if front_pid == pid:
            return True, f"frontmost (pid={pid})"
        time.sleep(delay)
        delay = min(delay * 2, _POLL_CAP)

    return False, (
        f"app (pid={pid}) is not frontmost after {timeout:.0f}s; frontmost is "
        f"{front_name!r} (pid={front_pid}). On macOS CGEventPost delivers "
        f"synthetic events to the frontmost app, so input simulation and "
        f"OS-focus tests cannot run reliably until this process holds the front."
    )
