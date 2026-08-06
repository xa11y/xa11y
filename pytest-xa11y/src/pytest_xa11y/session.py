"""Process lifetime for the app under test: start, find, ready, reset, stop."""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess
import sys
import tempfile
import time
from typing import IO, NoReturn

import xa11y

from .errors import AppDied, AppLaunchError
from .frontmost import ensure_macos_frontmost
from .launcher import AppLauncher

# One poll chunk of App.find, so a dead process is noticed within a second
# rather than at the end of the whole startup timeout.
#
# The chunking is a workaround, not the design. One App.find call for the
# full timeout, raising from inside the predicate on process death, would be
# strictly better: death detection improves to once per poll tick, and core
# stops building a full app enumeration for each timeout we discard as a
# retry signal (the anti-pattern tenet 6 names). It is blocked on
# xa11y/xa11y#358 — App.find holds the GIL for its entire poll loop, so a
# single long call would freeze the consumer's other threads for the whole
# startup wait. Chunking at least yields between calls. Revisit when #358
# lands.
_FIND_CHUNK = 1.0

# Bytes of captured stdout/stderr reported on failure. Diagnostics are
# bounded: a crash loop can emit megabytes, and the tail is the useful part.
_OUTPUT_TAIL = 4000

# Bounded listing of running apps included when the app is never found.
_MAX_APP_CANDIDATES = 40

_TERMINATE_GRACE = 5.0


def _pid_alive(pid: int) -> bool:
    """Whether ``pid`` still names a running process."""
    if sys.platform == "win32":
        # No signal 0 on Windows: ask the task list instead. tasklist is
        # present on every supported version and needs no extra dependency.
        result = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/NH"],
            capture_output=True,
            text=True,
        )
        return str(pid) in result.stdout
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Alive, but owned by another user — which is still alive.
        return True
    return True


def _tail(stream: IO[bytes] | None, limit: int = _OUTPUT_TAIL) -> str:
    """Read the last ``limit`` bytes of a captured output file."""
    if stream is None:
        return "<not captured>"
    try:
        stream.flush()
        size = stream.seek(0, 2)
        stream.seek(max(0, size - limit))
        data = stream.read()
    except (OSError, ValueError) as exc:
        return f"<unreadable: {exc!r}>"
    text = data.decode("utf-8", errors="replace")
    if size > limit:
        text = f"... (truncated, {size} bytes total)\n{text}"
    return text or "<empty>"


class AppSession:
    """A launched (or attached) application, and the handle tests drive.

    One of these exists per live app. The plugin keeps a session-scoped
    instance for ``xa11y_app`` and a short-lived one per ``xa11y_fresh_app``
    test or ``xa11y_app_factory`` call.
    """

    def __init__(self, launcher: AppLauncher, *, startup_timeout: float) -> None:
        self.launcher = launcher
        self.startup_timeout = (
            launcher.startup_timeout if launcher.startup_timeout is not None else startup_timeout
        )
        self.process: subprocess.Popen | None = None
        self.app: xa11y.App | None = None
        self._stdout: IO[bytes] | None = None
        self._stderr: IO[bytes] | None = None
        self._stopped = False

    # -- lifecycle ---------------------------------------------------------

    def start(self) -> xa11y.App:
        """Launch or attach, wait for readiness, and return the app handle."""
        if self.launcher.attach_pid is not None:
            pid = self.launcher.attach_pid
        else:
            self._spawn()
            assert self.process is not None  # set by _spawn
            pid = self.process.pid

        self.app = self._await_app(pid)
        if self.launcher.ready:
            self._await_ready(self.app, self.launcher.ready)
        if self.launcher.frontmost:
            ok, detail = ensure_macos_frontmost(self.app.pid or pid)
            if not ok:
                self.stop()
                raise AppLaunchError(detail)
        return self.app

    def _spawn(self) -> None:
        # Captured to temp files rather than pipes: a pipe nobody drains fills
        # its 64 KiB buffer and blocks the child forever, which turns a chatty
        # app into a hung test run. SIM115 does not apply — these deliberately
        # outlive the function, holding the app's output for as long as it
        # runs, and stop() closes them.
        self._stdout = tempfile.TemporaryFile()  # noqa: SIM115
        self._stderr = tempfile.TemporaryFile()  # noqa: SIM115
        try:
            self.process = subprocess.Popen(
                list(self.launcher.command),
                env=self.launcher.resolved_env(),
                cwd=str(self.launcher.cwd) if self.launcher.cwd else None,
                stdout=self._stdout,
                stderr=self._stderr,
            )
        except OSError as exc:
            raise AppLaunchError(
                f"Could not launch {self.launcher.display_name!r}: {exc}\n"
                f"  command: {list(self.launcher.command)}\n"
                f"  cwd: {self.launcher.cwd or '<inherited>'}"
            ) from exc

    def _await_app(self, pid: int) -> xa11y.App:
        """Poll until the app registers with the platform accessibility API."""
        names = self.launcher.app_names
        prefix = self.launcher.app_name_prefix

        def matches(candidate: xa11y.App) -> bool:
            pid_matches = candidate.pid == pid
            name = candidate.name or ""
            if prefix is not None:
                # Narrowing: one process registers several accessibility apps
                # and only one of them is the dialog under test.
                return pid_matches and name.startswith(prefix)
            if pid_matches:
                return True
            # Widening: the registering process is not the one we spawned.
            # Note this cannot express a preference between names — App.find
            # returns the first candidate the platform enumerates that
            # satisfies the predicate, so a list of names is a set, not an
            # order. Use app_name_prefix when precision matters.
            lowered = name.lower()
            return any(candidate_name.lower() in lowered for candidate_name in names)

        deadline = time.monotonic() + self.startup_timeout
        last_platform_error: Exception | None = None
        while time.monotonic() < deadline:
            self._raise_if_exited(during="startup")
            remaining = max(0.0, deadline - time.monotonic())
            try:
                return xa11y.App.find(matches, timeout=min(_FIND_CHUNK, remaining))
            except (xa11y.TimeoutError, xa11y.SelectorNotMatchedError):
                # Expected while the app is still registering — this is the
                # loop's retry signal, not a failure.
                continue
            except xa11y.PlatformError as exc:
                # The accessibility bus can legitimately error mid-registration
                # (AT-SPI in particular). Retry, but keep the error: if we
                # ultimately time out, the last one is reported rather than
                # discarded.
                last_platform_error = exc
                continue

        self._fail_not_found(pid, last_platform_error)

    def _fail_not_found(self, pid: int, last_platform_error: Exception | None) -> NoReturn:
        try:
            listed = [f"{a.name!r} (pid={a.pid})" for a in xa11y.App.list()]
        except xa11y.XA11yError as exc:
            listed = [f"<App.list() failed: {exc!r}>"]
        shown = listed[:_MAX_APP_CANDIDATES]
        if len(listed) > _MAX_APP_CANDIDATES:
            shown.append(f"... and {len(listed) - _MAX_APP_CANDIDATES} more")

        looked_for = f"  looked for: pid={pid}"
        if self.launcher.app_name_prefix:
            looked_for += f" and name starting with {self.launcher.app_name_prefix!r}"
        elif self.launcher.app_names:
            looked_for += f" or name containing {list(self.launcher.app_names)}"
        lines = [
            f"{self.launcher.display_name!r} did not register with the accessibility "
            f"API within {self.startup_timeout:.0f}s.",
            looked_for,
        ]
        if last_platform_error is not None:
            lines.append(f"  last accessibility error: {last_platform_error!r}")
        lines.append("  running apps: " + (", ".join(shown) if shown else "<none>"))
        if self.process is not None:
            lines.append(f"  process: alive (pid={self.process.pid})")
            lines.append(f"  stdout: {_tail(self._stdout)}")
            lines.append(f"  stderr: {_tail(self._stderr)}")
        if not self.launcher.app_names:
            lines.append(
                "  hint: if the process that registers is not the one launched "
                "(Electron helpers, launcher shims, re-execs), set "
                "AppLauncher(app_names=[...])."
            )
        self.stop()
        raise AppLaunchError("\n".join(lines))

    def _await_ready(self, app: xa11y.App, selector: str) -> None:
        """Poll until the readiness selector resolves."""
        deadline = time.monotonic() + self.startup_timeout
        last_error: Exception | None = None
        while time.monotonic() < deadline:
            self._raise_if_exited(during="content readiness")
            remaining = max(0.0, deadline - time.monotonic())
            try:
                app.locator(selector).wait_attached(timeout=min(_FIND_CHUNK, remaining))
                return
            except (xa11y.TimeoutError, xa11y.SelectorNotMatchedError) as exc:
                last_error = exc
                continue
            except xa11y.PlatformError as exc:
                last_error = exc
                continue

        detail = [
            f"{self.launcher.display_name!r} started but its content never became "
            f"ready: selector {selector!r} did not resolve within "
            f"{self.startup_timeout:.0f}s.",
        ]
        if last_error is not None:
            detail.append(f"  last error: {last_error!r}")
        try:
            detail.append("  tree:\n" + app.dump(max_depth=6))
        except xa11y.XA11yError as exc:
            detail.append(f"  tree: <dump failed: {exc!r}>")
        self.stop()
        raise AppLaunchError("\n".join(detail))

    # -- health ------------------------------------------------------------

    def _raise_if_exited(self, *, during: str) -> None:
        if self.process is None or self.process.poll() is None:
            return
        rc = self.process.returncode
        raise AppLaunchError(
            f"{self.launcher.display_name!r} exited during {during} "
            f"(code {rc}).\n"
            f"  command: {list(self.launcher.command)}\n"
            f"  stdout: {_tail(self._stdout)}\n"
            f"  stderr: {_tail(self._stderr)}"
        )

    def check_alive(self) -> None:
        """Raise if the app has exited since the last check.

        Called between tests. Without it, a mid-run crash surfaces as every
        remaining test failing on an unrelated selector error, and the actual
        cause — the process is gone, and here is what it printed — is nowhere
        in the report.

        Attach mode gets a signal-0 probe rather than nothing. The harness in
        this repository launches the app once and hands every language suite
        its pid, so attach mode is the *normal* path in CI; a liveness check
        that silently did nothing there would be a feature that never runs
        where it is most needed. There is no captured output to report in
        that mode, because the process is not ours.
        """
        if self._stopped:
            return

        if self.process is not None:
            if self.process.poll() is None:
                return
            raise AppDied(
                f"{self.launcher.display_name!r} exited mid-run (code "
                f"{self.process.returncode}).\n"
                f"  stdout: {_tail(self._stdout)}\n"
                f"  stderr: {_tail(self._stderr)}"
            )

        pid = self.launcher.attach_pid
        if pid is None or _pid_alive(pid):
            return
        raise AppDied(
            f"{self.launcher.display_name!r} (attached pid={pid}) is no longer "
            f"running. Its output was not captured by this process."
        )

    def run_reset(self) -> None:
        """Return the app to a known state before a test.

        Errors propagate: a reset that has stopped working is exactly the bug
        that makes the *next* test flake for an unrelated-looking reason.
        """
        if self.launcher.reset is None or self.app is None:
            return
        self.launcher.reset(self.app)

    # -- teardown ----------------------------------------------------------

    def stop(self) -> None:
        """Terminate the app, if this session started it."""
        if self._stopped:
            return
        proc = self.process
        if proc is not None and proc.poll() is None:
            try:
                if sys.platform == "win32":
                    proc.terminate()
                else:
                    proc.send_signal(signal.SIGTERM)
                proc.wait(timeout=_TERMINATE_GRACE)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                proc.kill()
                # Already SIGKILLed; if the wait still times out the OS will
                # reap it, and there is nothing further this process can do.
                with contextlib.suppress(subprocess.TimeoutExpired):
                    proc.wait(timeout=_TERMINATE_GRACE)
        for handle in (self._stdout, self._stderr):
            if handle is not None:
                with contextlib.suppress(OSError):
                    handle.close()
        # Set last: an exception on the way through here would otherwise leave
        # the session marked stopped with its handles open and its process
        # still running, and a second stop() would decline to try again.
        self._stopped = True

    # -- diagnostics -------------------------------------------------------

    def output_tails(self) -> list[str]:
        """Bounded stdout/stderr tails, for failure reports."""
        if self.process is None:
            return []
        return [f"stdout: {_tail(self._stdout)}", f"stderr: {_tail(self._stderr)}"]
