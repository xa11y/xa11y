"""Owned-profile isolation and native-probe failure cleanup without a desktop."""

from pathlib import Path
from types import SimpleNamespace

import pytest

from tests.harness import windows_ia2_browser as fixture


def report(active=fixture.MARKERS[0], inactive=fixture.MARKERS[1]):
    return {
        "active_marker": active,
        "inactive_marker": inactive,
        "document_name": active,
        "unique_id": -42,
        "window_handle": 0x23456789A,
        "pid": 42,
        "event_pid": 99,
        "examined_nodes": 30,
        "canonical_identity_preserved": True,
        "repeat_capture_identity_preserved": True,
        "negative_id_resolved": True,
    }


class Job:
    def __init__(self):
        self.command = None
        self.closed = False
        self.verified = []

    def start(self, command):
        self.command = command

    def window(self, markers):
        assert markers == fixture.MARKERS
        return (0x123456789, 42)

    def close(self):
        self.closed = True

    def verify_event_window(self, hwnd, pid, top):
        self.verified.append((hwnd, pid, top))

    def diagnostics(self, executable):
        return {
            "browser_version": "155.0.0.0",
            "job_active_processes": 3,
            "launch_root_exit_code": 259,
        }


def test_probe_twice_swaps_owned_tabs_and_removes_only_its_profile():
    job = Job()
    calls = []

    def probe(command, **kwargs):
        calls.append(command)
        assert kwargs["timeout"] == 30
        assert kwargs["check"] is False
        assert job.command is not None
        profile = Path(job.command[1].split("=", 1)[1])
        assert profile.name == "profile" and profile.parent.name.startswith(
            "xa11y-ia2-"
        )
        assert "--force-renderer-accessibility=complete" in job.command
        assert "--force-renderer-accessibility" not in job.command
        assert "--disable-backgrounding-occluded-windows" in job.command
        assert job.command[-2].startswith("file:")
        import json

        return SimpleNamespace(
            returncode=0, stdout=json.dumps(report(command[-3], command[-1])), stderr=""
        )

    result = fixture.run_fixture(
        Path("probe.exe"),
        Path("owned-browser.exe"),
        backend=lambda: job,
        run=probe,
    )
    assert result["hwnd"] == 0x123456789
    assert len(result["reports"]) == 2
    assert result["configuration"] == [
        "--force-renderer-accessibility=complete",
        "--disable-backgrounding-occluded-windows",
    ]
    assert calls[0][-4:] == [
        "--active-marker",
        fixture.MARKERS[0],
        "--inactive-marker",
        fixture.MARKERS[1],
    ]
    assert calls[1][-4:] == [
        "--active-marker",
        fixture.MARKERS[1],
        "--inactive-marker",
        fixture.MARKERS[0],
    ]
    assert job.closed
    assert job.verified == [(0x23456789A, 99, 0x123456789)] * 2
    assert not Path(job.command[1].split("=", 1)[1]).parent.exists()


@pytest.mark.parametrize(
    "failure", [TimeoutError("probe hung"), AssertionError("native error")]
)
def test_failed_probe_still_stops_owned_browser_and_cleans_profile(failure):
    job = Job()

    def probe(*args, **kwargs):
        raise failure

    with pytest.raises(type(failure), match=str(failure)):
        fixture.run_fixture(
            Path("probe"), Path("browser"), backend=lambda: job, run=probe
        )
    assert job.closed
    assert not Path(job.command[1].split("=", 1)[1]).parent.exists()


def test_native_error_is_not_a_successful_empty_report():
    job = Job()
    with pytest.raises(AssertionError, match="Native IA2 probe failed"):
        fixture.run_fixture(
            Path("probe"),
            Path("browser"),
            backend=lambda: job,
            run=lambda *a, **kw: SimpleNamespace(
                returncode=1, stdout="", stderr="native failure"
            ),
        )
    assert job.closed


def test_failed_job_shutdown_preserves_profile_for_safe_cleanup():
    class FailedJob(Job):
        def close(self):
            raise TimeoutError("still active")

    job = FailedJob()
    try:
        with pytest.raises(TimeoutError, match="still active") as failure:
            fixture.run_fixture(
                Path("probe"),
                Path("browser"),
                backend=lambda: job,
                run=lambda *a, **kw: SimpleNamespace(
                    returncode=1, stdout="", stderr="native error"
                ),
            )
        directory = Path(job.command[1].split("=", 1)[1]).parent
        assert directory.exists()
        assert isinstance(failure.value.__context__, AssertionError)
        assert "native error" in str(failure.value.__context__)
    finally:
        # The fake never starts any process; remove its exact generated directory.
        fixture.shutil.rmtree(Path(job.command[1].split("=", 1)[1]).parent)


def test_assignment_precedes_resume_and_thread_handle_closes():
    events = []
    job = fixture.BrowserJob.__new__(fixture.BrowserJob)
    job.job = "owned job"
    thread = SimpleNamespace(Close=lambda: events.append("close thread"))
    job.processes = SimpleNamespace(
        CreateProcess=lambda *a: ("owned process", thread, 42, 1),
        STARTUPINFO=lambda: None,
        ResumeThread=lambda t: events.append("resume"),
    )
    job.jobs = SimpleNamespace(
        AssignProcessToJobObject=lambda j, p: events.append("assign")
    )
    job.con = SimpleNamespace(CREATE_SUSPENDED=4)
    job.start(["browser.exe", "--user-data-dir=owned"])
    assert events == ["assign", "resume", "close thread"]


def test_explicit_missing_browser_never_uses_a_different_browser(monkeypatch, tmp_path):
    monkeypatch.setenv("XA11Y_IA2_BROWSER", str(tmp_path / "missing.exe"))
    with pytest.raises(FileNotFoundError, match="No owned IA2 browser"):
        fixture.browser_executable()


def test_cleanup_refuses_redirect_to_another_directory_with_same_temp_parent(
    monkeypatch, tmp_path
):
    directory = tmp_path / "xa11y-ia2-original"
    redirected = tmp_path / "xa11y-ia2-another"
    original = directory.resolve()
    real_resolve = Path.resolve
    removed = []
    monkeypatch.setattr(
        Path,
        "resolve",
        lambda path: redirected if path == directory else real_resolve(path),
    )
    monkeypatch.setattr(fixture.shutil, "rmtree", removed.append)
    with pytest.raises(RuntimeError, match="cleanup target changed"):
        fixture._remove_owned_directory(directory, original)
    assert removed == []


@pytest.mark.parametrize(
    "field,value",
    [
        ("pid", 99),
        ("unique_id", 42),
        ("unique_id", -(2**32)),
        ("unique_id", True),
        ("window_handle", 0),
        ("event_pid", 0),
        ("examined_nodes", 2049),
        ("negative_id_resolved", 1),
        ("repeat_capture_identity_preserved", False),
        ("document_name", fixture.MARKERS[1]),
    ],
)
def test_native_report_refuses_wrong_identity_or_unproved_contract(field, value):
    native = report()
    native[field] = value
    with pytest.raises(AssertionError):
        fixture._validate_report(native, 42, *fixture.MARKERS)


def test_native_report_requires_fields_instead_of_accepting_empty_success():
    with pytest.raises(AssertionError):
        fixture._validate_report({}, 42, *fixture.MARKERS)


@pytest.mark.parametrize(
    "same_pid,same_root,owned",
    [(False, True, True), (True, False, True), (True, True, False)],
)
def test_event_window_rejects_foreign_identity_even_for_positive_ids(
    same_pid, same_root, owned
):
    job = fixture.BrowserJob.__new__(fixture.BrowserJob)
    closed = []
    job.job = "owned job"
    job.gui = SimpleNamespace(
        IsWindow=lambda hwnd: True,
        GetAncestor=lambda hwnd, flag: 100 if same_root else 999,
    )
    job.processes = SimpleNamespace(
        GetWindowThreadProcessId=lambda hwnd: (1, 99 if same_pid else 88)
    )
    job.api = SimpleNamespace(
        OpenProcess=lambda *args: SimpleNamespace(Close=lambda: closed.append(True))
    )
    job.jobs = SimpleNamespace(IsProcessInJob=lambda process, native_job: owned)
    with pytest.raises(AssertionError):
        job.verify_event_window(200, 99, 100)
    assert closed == ([True] if same_pid and same_root else [])


def test_event_window_accepts_owned_cross_process_renderer_and_closes_handle():
    job = fixture.BrowserJob.__new__(fixture.BrowserJob)
    closed = []
    job.job = "owned job"
    job.gui = SimpleNamespace(
        IsWindow=lambda hwnd: True, GetAncestor=lambda hwnd, flag: 100
    )
    job.processes = SimpleNamespace(GetWindowThreadProcessId=lambda hwnd: (1, 99))
    job.api = SimpleNamespace(
        OpenProcess=lambda *args: SimpleNamespace(Close=lambda: closed.append(True))
    )
    job.jobs = SimpleNamespace(
        IsProcessInJob=lambda process, native_job: native_job == "owned job"
    )
    job.verify_event_window(200, 99, 100)
    assert closed == [True]


def _sharing_error(code=32):
    error = OSError("fixture file locked")
    error.winerror = code
    return error


@pytest.mark.parametrize("transient", [True, False])
def test_owned_cleanup_only_waits_boundedly_for_sharing_violation(
    monkeypatch, tmp_path, transient
):
    directory = tmp_path / "owned"
    original = directory.resolve()
    clock = [0.0]
    calls = []

    def remove(path):
        calls.append(path)
        if not transient or len(calls) == 1:
            raise _sharing_error()

    monkeypatch.setattr(fixture.shutil, "rmtree", remove)
    monkeypatch.setattr(fixture.time, "monotonic", lambda: clock[0])
    monkeypatch.setattr(
        fixture.time, "sleep", lambda seconds: clock.__setitem__(0, clock[0] + seconds)
    )
    if transient:
        fixture._remove_owned_directory(directory, original)
        assert len(calls) == 2
    else:
        with pytest.raises(OSError, match="fixture file locked"):
            fixture._remove_owned_directory(directory, original)
        assert 2 <= clock[0] < 2.2
    assert set(calls) == {directory}


def test_owned_cleanup_does_not_retry_access_denied(monkeypatch, tmp_path):
    directory = tmp_path / "owned"
    calls = []

    def remove(path):
        calls.append(path)
        raise _sharing_error(5)

    monkeypatch.setattr(fixture.shutil, "rmtree", remove)
    with pytest.raises(OSError):
        fixture._remove_owned_directory(directory, directory.resolve())
    assert calls == [directory]


def test_owned_log_tail_is_bounded_and_reports_truncation(tmp_path):
    log = tmp_path / "browser.log"
    log.write_bytes(b"a" * 9000 + b"owned-error")
    captured = fixture._owned_log_tail(log)
    assert len(captured["tail"].encode()) == 8192
    assert captured["tail"].endswith("owned-error")
    assert captured["truncated"] is True


def test_native_failure_context_uses_only_owned_browser_state(capsys):
    job = Job()

    def probe(*args, **kwargs):
        log = Path(
            next(
                arg.split("=", 1)[1]
                for arg in job.command
                if arg.startswith("--log-file=")
            )
        )
        assert log.parent == Path(job.command[1].split("=", 1)[1]).parent
        log.write_text("owned chrome diagnostic", encoding="utf-8")
        return SimpleNamespace(returncode=1, stdout="", stderr="busy native document")

    with pytest.raises(AssertionError, match="busy native document"):
        fixture.run_fixture(
            Path("probe"), Path("owned-browser"), backend=lambda: job, run=probe
        )
    error = capsys.readouterr().err
    assert "155.0.0.0" in error and "job_active_processes" in error and "259" in error
    assert "owned chrome diagnostic" in error
    assert job.closed


def test_owned_cleanup_retries_a_descendant_that_disappears(monkeypatch, tmp_path):
    directory = tmp_path / "owned"
    directory.mkdir()
    (directory / "file").write_text("owned")
    real_remove = fixture.shutil.rmtree
    calls = []

    def remove(path):
        calls.append(path)
        if len(calls) == 1:
            error = FileNotFoundError("browser released its lockfile")
            error.winerror = 2
            raise error
        real_remove(path)

    monkeypatch.setattr(fixture.shutil, "rmtree", remove)
    fixture._remove_owned_directory(directory, directory.resolve())
    assert calls == [directory, directory]
    assert not directory.exists()


def test_paused_browser_detaches_after_probe_failure(monkeypatch):
    import ctypes

    calls = []
    kernel = SimpleNamespace(
        DebugActiveProcess=lambda pid: calls.append(("pause", pid)) or True,
        DebugActiveProcessStop=lambda pid: calls.append(("detach", pid)) or True,
    )
    monkeypatch.setattr(ctypes, "WinDLL", lambda *a, **kw: kernel, raising=False)
    job = fixture.BrowserJob.__new__(fixture.BrowserJob)
    job.verify_event_window = lambda hwnd, pid, top: calls.append(("verify", pid))
    with pytest.raises(RuntimeError, match="probe failed"), job.paused_owner(100, 42):
        raise RuntimeError("probe failed")
    assert calls == [("verify", 42), ("pause", 42), ("detach", 42)]


def test_timeout_keeps_native_phase_and_partial_output_and_cleans_owned_job():
    import subprocess

    job = Job()

    def probe(command, **kwargs):
        raise subprocess.TimeoutExpired(
            command, 30, output=b"x" * 9000, stderr=b"IA2 phase: embedded objects"
        )

    with pytest.raises(AssertionError, match="exceeded 30s") as failure:
        fixture.run_fixture(
            Path("probe"), Path("browser"), backend=lambda: job, run=probe
        )
    assert "embedded objects" in str(failure.value)
    assert fixture.MARKERS[0] in str(failure.value)
    assert "pid=42" in str(failure.value)
    assert len(str(failure.value)) < 9000
    assert isinstance(failure.value.__cause__, subprocess.TimeoutExpired)
    assert job.closed


def test_consumers_get_their_own_budget_on_each_rich_tab(monkeypatch):
    import json

    monkeypatch.setenv("XA11Y_IA2_CONSUMERS", "1")
    job = Job()
    calls = []

    def run(command, **kwargs):
        calls.append(command)
        assert kwargs["timeout"] == 30
        if command[0] == "probe":
            value = report(command[-3], command[-1])
            value["rich_text_table"] = {"source": "ia2"}
        else:
            assert command[-1] == "42"
            assert command[-2].endswith("windows_ia2_consumers.py")
            value = dict.fromkeys(("python", "javascript", "cli", "mcp"), True)
        return SimpleNamespace(returncode=0, stdout=json.dumps(value), stderr="")

    result = fixture.run_fixture(
        Path("probe"), Path("browser"), backend=lambda: job, run=run
    )
    assert len(calls) == 4
    assert all(report["consumer_sources"]["mcp"] for report in result["reports"])
    assert job.closed


def test_consumer_failure_is_not_hidden_by_a_successful_native_probe(monkeypatch):
    import json

    monkeypatch.setenv("XA11Y_IA2_CONSUMERS", "1")
    job = Job()

    def run(command, **kwargs):
        if command[0] == "probe":
            value = report(command[-3], command[-1])
            value["rich_text_table"] = {"source": "ia2"}
            return SimpleNamespace(returncode=0, stdout=json.dumps(value), stderr="")
        return SimpleNamespace(returncode=1, stdout="", stderr="CLI read failed")

    with pytest.raises(AssertionError, match="consumer sources.*CLI read failed"):
        fixture.run_fixture(
            Path("probe"), Path("browser"), backend=lambda: job, run=run
        )
    assert job.closed
