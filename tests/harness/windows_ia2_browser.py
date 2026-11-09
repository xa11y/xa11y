"""Own an isolated Chromium fixture and bound each native IA2 probe process.

No existing browser profile or window is activated or terminated. Requires the
Windows-only pywin32 test dependency; the product does not depend on it.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

MARKERS = ("xa11y IA2 document A", "xa11y IA2 document B")
BROWSER_FLAGS = (
    "--force-renderer-accessibility=complete",
    # Keep this owned fixture's document published when its window is occluded.
    # The otherwise-identical local control exposed a busy, empty document.
    "--disable-backgrounding-occluded-windows",
)


def browser_executable() -> Path:
    explicit = os.environ.get("XA11Y_IA2_BROWSER")
    candidates = (
        [Path(explicit)]
        if explicit
        else [
            Path(os.environ.get("PROGRAMFILES", "C:/Program Files"))
            / "Google/Chrome/Application/chrome.exe",
            Path(os.environ.get("PROGRAMFILES(X86)", "C:/Program Files (x86)"))
            / "Microsoft/Edge/Application/msedge.exe",
        ]
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise FileNotFoundError(f"No owned IA2 browser executable found: {candidates}")


class BrowserJob:
    """Assign a suspended child before it can create browser descendants."""

    def __init__(self):
        import win32api
        import win32con
        import win32event
        import win32gui
        import win32job
        import win32process

        self.api, self.con, self.event = win32api, win32con, win32event
        self.gui, self.jobs, self.processes = win32gui, win32job, win32process
        self.job = win32job.CreateJobObject(None, "")
        self.process = None
        try:
            limits = win32job.QueryInformationJobObject(
                self.job, win32job.JobObjectExtendedLimitInformation
            )
            limits["BasicLimitInformation"]["LimitFlags"] = (
                win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            )
            win32job.SetInformationJobObject(
                self.job, win32job.JobObjectExtendedLimitInformation, limits
            )
        except BaseException:
            self.job.Close()
            raise

    @contextmanager
    def paused_owner(self, hwnd, pid):
        """Pause only this Job Object's proved browser owner for the stall test."""
        import ctypes
        from ctypes import wintypes

        self.verify_event_window(hwnd, pid, hwnd)
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        for name in ("DebugActiveProcess", "DebugActiveProcessStop"):
            method = getattr(kernel, name)
            method.argtypes = [wintypes.DWORD]
            method.restype = wintypes.BOOL
        if not kernel.DebugActiveProcess(pid):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            # Windows suspends the debuggee's threads until debug events are
            # continued. Do not continue them: this is a real nonresponding
            # accessibility server, confined to the fixture's owned process.
            yield
        finally:
            if not kernel.DebugActiveProcessStop(pid):
                raise ctypes.WinError(ctypes.get_last_error())

    def start(self, command):
        process, thread, _pid, _tid = self.processes.CreateProcess(
            command[0],
            subprocess.list2cmdline(command),
            None,
            None,
            False,
            self.con.CREATE_SUSPENDED,
            None,
            None,
            self.processes.STARTUPINFO(),
        )
        self.process = process
        try:
            self.jobs.AssignProcessToJobObject(self.job, process)
            self.processes.ResumeThread(thread)
        except BaseException:
            # This exact still-owned process may not yet belong to the job.
            self.api.TerminateProcess(process, 1)
            if self.event.WaitForSingleObject(process, 5000) != self.con.WAIT_OBJECT_0:
                raise TimeoutError("Suspended fixture process did not terminate")
            raise
        finally:
            thread.Close()

    def window(self, markers):
        found = []

        def visit(hwnd, _context):
            if not self.gui.IsWindowVisible(hwnd):
                return
            if self.gui.GetClassName(hwnd) != "Chrome_WidgetWin_1":
                return
            title = self.gui.GetWindowText(hwnd)
            if not any(marker in title for marker in markers):
                return
            _thread, pid = self.processes.GetWindowThreadProcessId(hwnd)
            process = self.api.OpenProcess(0x1000, False, pid)
            try:
                if self.jobs.IsProcessInJob(process, self.job):
                    found.append((hwnd, pid))
            finally:
                process.Close()

        self.gui.EnumWindows(visit, None)
        if len(found) > 1:
            raise AssertionError(f"Multiple owned IA2 fixture windows: {found[:8]}")
        return found[0] if found else None

    def verify_event_window(self, hwnd: int, pid: int, top: int):
        assert self.gui.IsWindow(hwnd), "IA2 event window no longer exists"
        _thread, actual_pid = self.processes.GetWindowThreadProcessId(hwnd)
        assert actual_pid == pid, "IA2 event HWND/PID identity mismatch"
        assert self.gui.GetAncestor(hwnd, 2) == top, (
            "IA2 event HWND is outside owned top window"
        )
        process = self.api.OpenProcess(0x1000, False, pid)
        try:
            assert self.jobs.IsProcessInJob(process, self.job), (
                "IA2 event PID is outside owned browser job"
            )
        finally:
            process.Close()

    def diagnostics(self, executable: Path):
        def sample(query):
            try:
                return query()
            except Exception as exc:  # noqa: BLE001 - preserve native failure
                return f"<probe failed: {type(exc).__name__}: {str(exc)[:512]}>"

        def version():
            info = self.api.GetFileVersionInfo(str(executable), "\\")
            ms, ls = info["FileVersionMS"], info["FileVersionLS"]
            return f"{ms >> 16}.{ms & 65535}.{ls >> 16}.{ls & 65535}"

        return {
            "browser_version": sample(version),
            "job_active_processes": sample(
                lambda: self.jobs.QueryInformationJobObject(
                    self.job, self.jobs.JobObjectBasicAccountingInformation
                )["ActiveProcesses"]
            ),
            "launch_root_exit_code": sample(
                lambda: self.processes.GetExitCodeProcess(self.process)
            )
            if self.process is not None
            else "not started",
        }

    def close(self):
        try:
            self.jobs.TerminateJobObject(self.job, 0)
            deadline = time.monotonic() + 5
            while self.jobs.QueryInformationJobObject(
                self.job, self.jobs.JobObjectBasicAccountingInformation
            )["ActiveProcesses"]:
                if time.monotonic() >= deadline:
                    raise TimeoutError("Owned browser job still has active processes")
                time.sleep(0.05)
        finally:
            if self.process is not None:
                self.process.Close()
            self.job.Close()


def _remove_owned_directory(directory: Path, original: Path):
    deadline = time.monotonic() + 2
    while True:
        if directory.resolve() != original:
            raise RuntimeError(
                f"Fixture cleanup target changed from {original}: {directory}"
            )
        try:
            shutil.rmtree(directory)
            return
        except OSError as exc:
            # Native browser job exit can precede release of filesystem
            # handles. Retry only ERROR_SHARING_VIOLATION, on this exact
            # already-stopped fixture directory, for at most two seconds.
            if isinstance(exc, FileNotFoundError) and not directory.exists():
                return
            if (
                getattr(exc, "winerror", None) not in (2, 32)
                or time.monotonic() >= deadline
            ):
                raise
            time.sleep(0.1)


def _validate_report(report, pid: int, active: str, inactive: str):
    assert isinstance(report, dict), "IA2 probe report must be an object"
    assert report.get("active_marker") == active, "IA2 active marker mismatch"
    assert report.get("inactive_marker") == inactive, "IA2 inactive marker mismatch"
    document = report.get("document_name")
    assert (
        isinstance(document, str) and active in document and inactive not in document
    ), "IA2 document does not identify the intended active tab"
    assert type(report.get("pid")) is int and report["pid"] == pid, (
        "IA2 browser PID mismatch"
    )
    unique_id = report.get("unique_id")
    assert type(unique_id) is int and -(2**31) <= unique_id < 0, (
        "IA2 unique ID must be signed negative LONG"
    )
    for name in ("window_handle", "event_pid"):
        assert type(report.get(name)) is int and report[name] > 0, (
            f"IA2 {name} must be positive"
        )
    count = report.get("examined_nodes")
    assert type(count) is int and 1 <= count <= 2048, (
        "IA2 node count exceeds capture bound"
    )
    for name in (
        "canonical_identity_preserved",
        "repeat_capture_identity_preserved",
        "negative_id_resolved",
    ):
        assert report.get(name) is True, f"IA2 probe did not prove {name}"
    # The native probe proves event-HWND ancestry. Renderer PID/HWND need not
    # equal the top-level browser PID/HWND in Chromium's multi-process model.
    return report


def _owned_log_tail(log: Path):
    try:
        with log.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - 8192))
            return {
                "tail": stream.read(8192).decode("utf-8", errors="replace"),
                "truncated": size > 8192,
            }
    except FileNotFoundError:
        return {"status": "owned browser log not created"}
    except OSError as exc:
        return {"error": f"{type(exc).__name__}: {str(exc)[:512]}"}


def _run_phase(run, command, label, *, timeout=30):
    started = time.monotonic()
    print(f"IA2 phase started: {label}", file=sys.stderr, flush=True)
    try:
        result = run(
            command,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        # TimeoutExpired may carry bytes even with text=True. Keep the native
        # phase markers: the old bare exception lost the only step evidence.
        def bounded(value):
            if isinstance(value, bytes):
                value = value.decode("utf-8", errors="replace")
            return (value or "")[-8000:]

        raise AssertionError(
            f"IA2 {label} exceeded {timeout}s; "
            f"stdout tail={bounded(exc.stdout)!r}; stderr tail={bounded(exc.stderr)!r}"
        ) from exc
    if result.returncode != 0:
        raise AssertionError(
            f"Native IA2 probe failed during {label} ({result.returncode}): "
            f"stdout={result.stdout[-8000:]!r}; stderr={result.stderr[-8000:]!r}"
        )
    print(
        f"IA2 phase passed: {label} ({time.monotonic() - started:.2f}s)",
        file=sys.stderr,
        flush=True,
    )
    return result


def run_fixture(
    probe: Path,
    executable: Path,
    *,
    backend=BrowserJob,
    run=subprocess.run,
    exercise_timeout=False,
):
    directory = Path(tempfile.mkdtemp(prefix="xa11y-ia2-"))
    original_directory = directory.resolve()
    browser_log = directory / "browser.log"
    job = None
    try:
        urls = []
        source = Path(__file__).resolve().parents[2] / "test-apps/ia2/document.html"
        template = source.read_text(encoding="utf-8")
        for index, marker in enumerate(MARKERS):
            document = directory / f"document-{index}.html"
            document.write_text(
                template.replace("FIXTURE_MARKER", marker), encoding="utf-8"
            )
            urls.append(document.as_uri())
        command = [
            str(executable),
            f"--user-data-dir={directory / 'profile'}",
            *BROWSER_FLAGS,
            "--no-first-run",
            "--no-default-browser-check",
            "--enable-logging",
            f"--log-file={browser_log}",
            "--new-window",
            *urls,
        ]
        job = backend()
        job.start(command)
        deadline = time.monotonic() + 20
        while (window := job.window(MARKERS)) is None:
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    "No visible owned IA2 browser fixture window within 20s"
                )
            time.sleep(0.1)
        hwnd, pid = window
        reports = []
        for active, inactive in (MARKERS, MARKERS[::-1]):
            result = _run_phase(
                run,
                [
                    str(probe),
                    "--hwnd",
                    str(hwnd),
                    "--pid",
                    str(pid),
                    "--active-marker",
                    active,
                    "--inactive-marker",
                    inactive,
                ],
                f"native probe pid={pid} active={active!r}",
            )
            report = _validate_report(json.loads(result.stdout), pid, active, inactive)
            job.verify_event_window(report["window_handle"], report["event_pid"], hwnd)
            if os.environ.get("XA11Y_IA2_CONSUMERS") and "rich_text_table" in report:
                consumers = _run_phase(
                    run,
                    [
                        sys.executable,
                        str(Path(__file__).with_name("windows_ia2_consumers.py")),
                        str(pid),
                    ],
                    f"consumer sources pid={pid} active={active!r}",
                )
                sources = json.loads(consumers.stdout)
                assert sources == dict.fromkeys(
                    ("python", "javascript", "cli", "mcp"), True
                ), sources
                report["consumer_sources"] = sources
            reports.append(report)
        timeout_report = None
        if exercise_timeout:
            with job.paused_owner(hwnd, pid):
                stalled = run(
                    [str(probe), "--hung", str(pid)],
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    timeout=12,
                    check=False,
                )
                if stalled.returncode != 0:
                    raise AssertionError(
                        f"Hung IA2 probe failed: {stalled.stderr[:8000]}"
                    )
                timeout_report = json.loads(stalled.stdout)
                if timeout_report.get("worker_disabled") is not True:
                    raise AssertionError(
                        f"Hung probe did not prove worker disabled: {timeout_report}"
                    )
        return {
            "browser": str(executable),
            "configuration": list(BROWSER_FLAGS),
            "browser_state": job.diagnostics(executable),
            "pid": pid,
            "hwnd": hwnd,
            "reports": reports,
            "hung_provider": timeout_report,
        }
    except BaseException:
        # Read only this fixture's state/log while the owned job is still alive.
        # Diagnosis must never replace the primary native probe failure.
        try:
            context = {
                "browser": str(executable),
                "configuration": list(BROWSER_FLAGS),
                "browser_state": job.diagnostics(executable)
                if job is not None
                else "not started",
                "owned_browser_log": _owned_log_tail(browser_log),
            }
            print(
                "IA2_BROWSER_FAILURE " + json.dumps(context),
                file=sys.stderr,
                flush=True,
            )
        except Exception as exc:  # noqa: BLE001 - preserve primary error
            print(
                f"IA2 browser diagnosis failed: {type(exc).__name__}: {str(exc)[:512]}",
                file=sys.stderr,
            )
        raise
    finally:
        if job is not None:
            job.close()
        # Only the exact original mkdtemp target, after the owned job is empty.
        _remove_owned_directory(directory, original_directory)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--exercise-timeout", action="store_true")
    args = parser.parse_args()
    print(
        json.dumps(
            run_fixture(
                args.probe.resolve(),
                browser_executable(),
                exercise_timeout=args.exercise_timeout,
            )
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()
