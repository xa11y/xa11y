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


def test_wpf_compares_full_suites_on_fresh_fixtures_and_keeps_typing_evidence(
    monkeypatch, tmp_path
):
    fixtures = [object(), object()]
    launched = []
    cleaned = []
    calls = []
    typing_report = {
        "control": {"plain": {"attempts": 60, "bad": 2}},
        "paced": {"plain": {"attempts": 30, "bad": 0}},
    }
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)

    def launch(app):
        launched.append(app)
        return fixtures[len(launched) - 1], "WPF", 1000 + len(launched)

    monkeypatch.setattr(compare.launch, "_launch_app", launch)
    monkeypatch.setattr(compare.launch, "_kill_app", cleaned.append)
    clock = iter([10, 11, 20, 22, 30, 33, 40, 44])
    monkeypatch.setattr(compare.time, "perf_counter", lambda: next(clock))

    def run(command, *, env, stdout, **kwargs):
        calls.append((command, env.copy()))
        report = next(
            arg.removeprefix("--junitxml=")
            for arg in command
            if arg.startswith("--junitxml=")
        )
        Path(report).write_text(
            '<testsuite><testcase name="checked"/></testsuite>', encoding="utf-8"
        )
        if "tests/suites/python/" in command:
            stdout.write(
                "WINDOWS_TYPING_COMPARISON " + json.dumps(typing_report) + "\n"
            )
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(compare.subprocess, "run", run)
    results = [
        compare.run_variant(label, f"{label}-python", tmp_path, "wpf")
        for label in ("base", "head")
    ]
    assert launched == ["wpf", "wpf"]
    assert cleaned == fixtures
    assert all(result["app"] == "wpf" and not result["failed"] for result in results)
    assert all(
        [suite["suite"] for suite in result["suites"]] == ["python", "python-window"]
        for result in results
    )
    assert [
        suite["elapsed_seconds"] for result in results for suite in result["suites"]
    ] == [1, 2, 3, 4]
    assert all(
        result["suites"][0]["typing_comparisons"] == [typing_report]
        for result in results
    )
    assert [command[0] for command, _ in calls] == ["base-python"] * 2 + [
        "head-python"
    ] * 2
    assert [env["XA11Y_TEST_APP_PID"] for _, env in calls] == ["1001"] * 2 + [
        "1002"
    ] * 2
    assert all(env["XA11Y_TEST_APP"] == "wpf" for _, env in calls)
    assert all("XA11Y_PERF_JSONL" not in env for _, env in calls)
    assert all(
        "tests/suites/python/test_performance.py" not in command for command, _ in calls
    )
