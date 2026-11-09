"""Wait diagnosis stays owned, bounded, and useful without privileged access."""

from tests.harness import linux_process_diagnostics as diagnostic


def _process(root, pid, children=""):
    task = root / str(pid) / "task" / str(pid)
    task.mkdir(parents=True)
    for name, value in {
        "children": children,
        "stat": f"{pid} (owned fixture) D 1",
        "wchan": "io_schedule",
        "syscall": "0 0 0",
        "stack": "native wait resource",
    }.items():
        (task / name).write_text(value)
    return task


def test_snapshot_follows_only_owned_descendants_and_records_native_waits(tmp_path):
    _process(tmp_path, 42, "43")
    _process(tmp_path, 43)
    _process(tmp_path, 99)
    result = diagnostic.snapshot(42, proc_root=tmp_path)
    assert [p["pid"] for p in result["processes"]] == [42, 43]
    assert result["processes"][0]["threads"][0]["wchan"] == "io_schedule"
    assert "D" in result["processes"][0]["threads"][0]["stat"]
    assert "FileNotFoundError" in result["processes"][0]["status"]
    assert result["processes_truncated"] is False


def test_snapshot_bounds_children_and_reports_truncation(tmp_path):
    _process(tmp_path, 42, " ".join(map(str, range(43, 60))))
    for pid in range(43, 60):
        _process(tmp_path, pid)
    result = diagnostic.snapshot(42, proc_root=tmp_path)
    assert len(result["processes"]) == 8
    assert result["processes_truncated"] is True


def test_missing_trace_tool_is_reported_without_losing_process_evidence(monkeypatch):
    monkeypatch.setattr(diagnostic, "snapshot", lambda pid: {"owned_pid": pid})
    monkeypatch.setattr(diagnostic.shutil, "which", lambda _: None)
    result = diagnostic.collect(42)
    assert '"owned_pid": 42' in result
    assert '"unavailable"' in result and "strace" in result


def test_trace_watchdog_preserves_partial_evidence_and_has_a_hard_kill(monkeypatch):
    import subprocess

    monkeypatch.setattr(diagnostic, "snapshot", lambda pid: {"owned_pid": pid})
    monkeypatch.setattr(diagnostic.shutil, "which", lambda tool: tool)

    def blocked_trace(command, **kwargs):
        assert "--kill-after=0.25s" in command
        assert command[command.index("-p") + 1] == "42"
        assert kwargs["timeout"] == 3
        kwargs["stderr"].write(b"native ioctl remained blocked")
        raise subprocess.TimeoutExpired(command, 3)

    monkeypatch.setattr(diagnostic.subprocess, "run", blocked_trace)
    result = diagnostic.collect(42)
    assert "native ioctl remained blocked" in result
    assert "TimeoutExpired" in result
    assert '"owned_pid": 42' in result
