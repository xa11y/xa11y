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

# Pause between retries after a transient accessibility-bus error. Core
# returns those immediately rather than polling through them, so a retry loop
# with no throttle spins: measured at ~270k App.find calls per second against
# a machine with no session bus, pinning a core for the whole startup budget.
_BUS_RETRY_INTERVAL = 0.25

# Bytes of captured stdout/stderr reported on failure. Diagnostics are
# bounded: a crash loop can emit megabytes, and the tail is the useful part.
_OUTPUT_TAIL = 4000

# Bounded listing of running apps included when the app is never found.
_MAX_APP_CANDIDATES = 40

_TERMINATE_GRACE = 5.0


def _pid_alive(pid: int) -> bool:
    """Whether ``pid`` still names a running process.

    Reports alive whenever it cannot tell. This check exists to convert a
    mid-run crash into one clear message, and it ends the whole run when it
    says "dead" — so an inconclusive probe must never be the thing that
    stops a suite. A false "alive" costs the old behaviour (tests fail on
    lookups against a gone process); a false "dead" invents a failure.
    """
    if sys.platform == "win32":
        # No signal 0 on Windows: ask the task list instead. tasklist ships
        # with every supported version, so a missing one means an unusual
        # environment rather than a dead app.
        #
        # This costs one subprocess per live app per test on Windows, where
        # the other platforms cost a syscall. Worth it to keep the check real
        # in attach mode — which is the normal path under a harness that
        # launches the app once — but it is not free.
        try:
            result = subprocess.run(
                ["tasklist", "/FI", f"PID eq {pid}", "/NH"],
                capture_output=True,
                text=True,
                timeout=10,
            )
        except (OSError, subprocess.SubprocessError):
            return True
        if result.returncode != 0:
            return True
        return str(pid) in result.stdout
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Alive, but owned by another user — which is still alive.
        return True
    except OSError:
        return True
    return True


def _open_capture() -> tuple[str, IO[bytes]]:
    """A file to capture one output stream into, as ``(path, write handle)``."""
    fd, path = tempfile.mkstemp(prefix="pytest-xa11y-", suffix=".log")
    return path, os.fdopen(fd, "wb")


def _tail(path: str | None, limit: int = _OUTPUT_TAIL) -> str:
    """Read the last ``limit`` bytes of a captured output file.

    Opens its own handle rather than seeking the one the child was given.
    ``subprocess`` hands the child a *dup* of that descriptor (a duplicated
    handle on Windows), and a dup shares the file **offset** — so seeking it
    here moves where the app's next write lands, and a tail taken while the
    app is still running can leave a hole in its own log. A separate open has
    an independent offset and cannot.
    """
    if path is None:
        return "<not captured>"
    try:
        with open(path, "rb") as handle:
            size = handle.seek(0, 2)
            handle.seek(max(0, size - limit))
            # Bounded explicitly: a read to EOF would return everything the
            # app wrote *during* the read, which on a crash loop is unbounded.
            data = handle.read(limit)
    except OSError as exc:
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

    def __init__(
        self, launcher: AppLauncher, *, startup_timeout: float, critical: bool = True
    ) -> None:
        self.launcher = launcher
        # Whether this app's death should end the run. True for the app under
        # test; False for the ad-hoc ones a suite launches itself, whose exit
        # is often the point of the test — dismissing a dialog *is* its
        # process exiting, and that must not abort everything after it.
        self.critical = critical
        self.startup_timeout = (
            launcher.startup_timeout if launcher.startup_timeout is not None else startup_timeout
        )
        # Whether this session has a process whose lifetime is the app's, and
        # can therefore tell a crash from a clean handoff.
        #
        # Deliberately *not* inferred from `app_names`. That field widens the
        # accessibility-tree match — the predicate still tries the spawned PID
        # first — so an app needing it (Electron, a Qt app whose AT-SPI name
        # lags its PID) is still an app we launched and can still watch. Only
        # `spawns_and_exits` says the launched process hands off and goes; only
        # that switches death detection off. Attach mode always has a process
        # to watch: the caller named its PID outright.
        self._watch_process = launcher.attach_pid is not None or not launcher.spawns_and_exits
        self.process: subprocess.Popen | None = None
        self.app: xa11y.App | None = None
        # Paths rather than handles: `_tail` opens its own, because the handle
        # handed to the child shares its file offset (see `_tail`).
        self._stdout: IO[bytes] | None = None
        self._stderr: IO[bytes] | None = None
        self._stdout_path: str | None = None
        self._stderr_path: str | None = None
        self._stopped = False

    # -- lifecycle ---------------------------------------------------------

    def start(self) -> xa11y.App:
        """Launch or attach, wait for readiness, and return the app handle."""
        if self.launcher.attach_pid is not None:
            pid = self.launcher.attach_pid
        else:
            self._spawn()
            if self.process is None:  # pragma: no cover - _spawn sets it or raises
                raise AppLaunchError(f"{self.launcher.display_name!r} produced no process handle.")
            pid = self.process.pid

        # One budget for "appear and become ready", which is what
        # AppLauncher.startup_timeout says it is. A deadline per phase would
        # let start() block for twice the documented time.
        deadline = time.monotonic() + self.startup_timeout
        self.app = self._await_app(pid, deadline)
        if self.launcher.ready:
            self._await_ready(self.app, self.launcher.ready, deadline)
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
        # runs, and stop() closes and removes them.
        self._stdout_path, self._stdout = _open_capture()
        self._stderr_path, self._stderr = _open_capture()
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

    def _await_app(self, pid: int, deadline: float) -> xa11y.App:
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

        def match_or_abort(candidate: xa11y.App) -> bool:
            # Checking for process death inside the predicate is what lets
            # this be a single App.find call. A raising predicate aborts the
            # search immediately and propagates, so a crashed app is reported
            # as a crash within one poll tick instead of at the end of the
            # whole timeout.
            self._raise_if_exited(during="startup")
            return matches(candidate)

        predicate = match_or_abort if self._watch_process else matches
        last_platform_error: Exception | None = None
        bus_error_was_terminal = False
        while True:
            self._raise_if_exited(during="startup")
            remaining = max(0.0, deadline - time.monotonic())
            if remaining <= 0:
                break
            try:
                return xa11y.App.find(predicate, timeout=remaining)
            except (xa11y.TimeoutError, xa11y.SelectorNotMatchedError):
                # The whole budget is spent. Not a retry signal — App.find
                # polls internally for the full duration, so this is terminal.
                # Retrying it would also be the tenet-6 anti-pattern: core
                # attaches a full app enumeration to each timeout, and a loop
                # that discards them pays for one per iteration.
                bus_error_was_terminal = False
                break
            except xa11y.PlatformError as exc:
                # The accessibility bus can legitimately error mid-registration
                # (AT-SPI in particular), and core propagates that immediately
                # rather than polling through it — in about a millisecond. This
                # is the only looping branch, so it carries the throttle: an
                # unthrottled retry here pins a core for the whole budget on
                # any machine where the bus is simply absent.
                last_platform_error = exc
                bus_error_was_terminal = True
                time.sleep(min(_BUS_RETRY_INTERVAL, max(0.0, deadline - time.monotonic())))
                continue

        # A process that died without ever being enumerated never reached the
        # predicate, so check once more before blaming accessibility
        # registration. "exited during startup (code 3)" with the captured
        # output is a diagnosis; "did not register with the accessibility API"
        # for a process that is not running is a wrong one.
        self._raise_if_exited(during="startup")
        self._fail_not_found(pid, last_platform_error, bus_error_was_terminal)

    def _fail_not_found(
        self,
        pid: int,
        last_platform_error: Exception | None,
        bus_error_was_terminal: bool = False,
    ) -> NoReturn:
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
        if bus_error_was_terminal and last_platform_error is not None:
            # The app never registered because there was nothing to register
            # with. Saying "did not register with the accessibility API" here
            # blames the app for the session's missing bus, and buries the
            # actual cause three lines down (tenet 6).
            headline = (
                f"the accessibility API is not usable in this session, so "
                f"{self.launcher.display_name!r} could not be found: {last_platform_error}"
            )
        else:
            headline = (
                f"{self.launcher.display_name!r} did not register with the accessibility "
                f"API within {self.startup_timeout:.0f}s."
            )
        lines = [headline, looked_for]
        if last_platform_error is not None:
            lines.append(f"  last accessibility error: {last_platform_error!r}")
        lines.append("  running apps: " + (", ".join(shown) if shown else "<none>"))
        if self.process is not None:
            # Asked, not assumed. `_raise_if_exited` ran just above and would
            # have taken over for a watched process — but a `spawns_and_exits`
            # launcher is not watched, and printing "alive" for a process that
            # is gone is a diagnosis pointing away from the actual failure.
            rc = self.process.poll()
            state = "alive" if rc is None else f"exited (code {rc})"
            lines.append(f"  process: {state} (pid={self.process.pid})")
            lines.append(f"  stdout: {_tail(self._stdout_path)}")
            lines.append(f"  stderr: {_tail(self._stderr_path)}")
        if not self.launcher.app_names:
            lines.append(
                "  hint: if the process that registers is not the one launched "
                "(Electron helpers, launcher shims, re-execs), set "
                "AppLauncher(app_names=[...])."
            )
        self.stop()
        raise AppLaunchError("\n".join(lines))

    def _await_ready(self, app: xa11y.App, selector: str, deadline: float) -> None:
        """Wait for the readiness selector to resolve.

        One ``wait_attached`` call for the whole budget, for the same reason
        ``_await_app`` makes one ``App.find`` call: the wait polls internally,
        releases the GIL, and attaches a bounded diagnosis to its timeout —
        which a caller that loops would build and discard every iteration.

        Unlike ``App.find`` there is no predicate to hook, so a process that
        dies *during* content load is noticed when the wait ends rather than
        within a second of dying. The reported error is still the crash, with
        the exit code and captured output; only the promptness differs.
        """
        last_error: Exception | None = None
        while True:
            self._raise_if_exited(during="content readiness")
            remaining = max(0.0, deadline - time.monotonic())
            if remaining <= 0:
                break
            try:
                app.locator(selector).wait_attached(timeout=remaining)
                return
            except (xa11y.TimeoutError, xa11y.SelectorNotMatchedError) as exc:
                # The full budget is spent; the wait polled throughout it.
                last_error = exc
                break
            except xa11y.PlatformError as exc:
                # Transient bus errors are not polled through by core, so this
                # is the one case worth retrying with the remaining time — and,
                # as in _await_app, the one that needs the throttle.
                last_error = exc
                time.sleep(min(_BUS_RETRY_INTERVAL, max(0.0, deadline - time.monotonic())))
                continue

        self._raise_if_exited(during="content readiness")

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
        if not self._watch_process:
            return
        if self.process is None or self.process.poll() is None:
            return
        rc = self.process.returncode
        # Read the output *before* stopping: stop() removes the capture files.
        message = (
            f"{self.launcher.display_name!r} exited during {during} "
            f"(code {rc}).\n"
            f"  command: {list(self.launcher.command)}\n"
            f"  stdout: {_tail(self._stdout_path)}\n"
            f"  stderr: {_tail(self._stderr_path)}"
        )
        # Tear down before raising, as _fail_not_found and _await_ready do.
        # Otherwise the half-started session stays registered and its exit is
        # re-reported later as a mid-run death, killing the run a second time
        # with a message that contradicts the launch failure already shown.
        self.stop()
        raise AppLaunchError(message)

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

        The only launcher this declines to check is ``spawns_and_exits``,
        where there is genuinely no process whose lifetime is the app's.
        """
        if self._stopped or not self._watch_process:
            return

        if self.process is not None:
            if self.process.poll() is None:
                return
            raise AppDied(
                f"{self.launcher.display_name!r} exited mid-run (code "
                f"{self.process.returncode}).\n"
                f"  stdout: {_tail(self._stdout_path)}\n"
                f"  stderr: {_tail(self._stderr_path)}"
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
        # Close the write handles first, then unlink: on Windows a file cannot
        # be removed while a handle to it is open, and the app's handle has
        # just gone with the process above.
        for handle in (self._stdout, self._stderr):
            if handle is not None:
                with contextlib.suppress(OSError):
                    handle.close()
        for path in (self._stdout_path, self._stderr_path):
            if path is not None:
                with contextlib.suppress(OSError):
                    os.unlink(path)
        self._stdout_path = None
        self._stderr_path = None
        # Set last: an exception on the way through here would otherwise leave
        # the session marked stopped with its handles open and its process
        # still running, and a second stop() would decline to try again.
        self._stopped = True

    # -- diagnostics -------------------------------------------------------

    def output_tails(self) -> list[str]:
        """Bounded stdout/stderr tails, for failure reports."""
        if self.process is None:
            return []
        return [f"stdout: {_tail(self._stdout_path)}", f"stderr: {_tail(self._stderr_path)}"]
