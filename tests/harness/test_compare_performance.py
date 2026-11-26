"""The comparison must retain failing suites and still execute the full workload."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from tests.harness import compare_performance as compare


def test_failed_suite_keeps_counts_timings_and_remaining_workload(
    monkeypatch, tmp_path
):
    fixture = object()
    cleaned = []
    calls = []
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    monkeypatch.setattr(compare.launch, "_launch_app", lambda _: (fixture, "Qt", 1234))
    monkeypatch.setattr(compare.launch, "_kill_app", cleaned.append)
    clock = iter([10, 11, 20, 22, 30, 33])
    monkeypatch.setattr(compare.time, "perf_counter", lambda: next(clock))

    def run(command, *, env, stdout, **kwargs):
        calls.append((command, env.copy()))
        report = next(
            arg.removeprefix("--junitxml=")
            for arg in command
            if arg.startswith("--junitxml=")
        )
        # A real failing performance run must not quietly drop the ordinary
        # and window suites, or disappear from the exported measurements.
        failed = len(calls) == 1
        failure = '<failure message="wrong value"/>' if failed else ""
        Path(report).write_text(
            f'<testsuite><testcase name="checked">{failure}</testcase>'
            '<testcase name="unsupported"><skipped/></testcase></testsuite>',
            encoding="utf-8",
        )
        stdout.write("pytest log\n")
        return subprocess.CompletedProcess(command, int(failed))

    monkeypatch.setattr(compare.subprocess, "run", run)
    result = compare.run_variant("base", "baseline-python", tmp_path)
    assert result["failed"]
    assert [suite["suite"] for suite in result["suites"]] == [
        "performance",
        "python",
        "python-window",
    ]
    assert [suite["elapsed_seconds"] for suite in result["suites"]] == [1, 2, 3]
    assert [suite["failures"] for suite in result["suites"]] == [1, 0, 0]
    assert all(
        suite["tests"] == 2 and suite["skipped"] == 1 for suite in result["suites"]
    )
    assert all(command[0] == "baseline-python" for command, _ in calls)
    assert all(env["XA11Y_TEST_APP_PID"] == "1234" for _, env in calls)
    assert "XA11Y_PERF_JSONL" in calls[0][1]
    assert "XA11Y_PERF_JSONL" not in calls[1][1]
    assert cleaned == [fixture]
    assert json.loads((tmp_path / "base-suites.json").read_text()) == result["suites"]
