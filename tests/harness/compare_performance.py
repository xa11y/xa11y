"""Compare installed release bindings using identical head tests and Qt fixture.

Compilation and installation happen before this process. Each variant gets a
fresh instance of the same fixture, with startup excluded from suite timings.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from tests.harness import launch


def run_variant(label: str, python: str, output: Path) -> dict:
    proc, name, pid = launch._launch_app("qt")
    try:
        assert pid is not None, "Qt fixture must report a PID"
        env = os.environ.copy()
        env.update(
            XA11Y_TEST_APP="qt",
            XA11Y_TEST_APP_PID=str(pid),
            XA11Y_TEST_APP_NAME=name,
            XA11Y_REQUIRE_FRONTMOST="1",
        )
        summaries = []
        failed = False
        operations = output / f"{label}-operations.jsonl"
        operations.write_text("", encoding="utf-8")
        if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(summary, "a", encoding="utf-8") as stream:
                print(
                    f"\n### {platform.system()} {label} operation samples\n",
                    file=stream,
                )
        # Keep the standard suite sequence, including the final mutating
        # window suite. Run targeted measurements first on pristine widgets.
        for suite in ["performance", "python", "python-window"]:
            report = output / f"{label}-{suite}.xml"
            if suite == "performance":
                command = [
                    python,
                    "-m",
                    "pytest",
                    "tests/suites/python/test_performance.py",
                    "-v",
                    "-rxXs",
                    f"--junitxml={report}",
                ]
                env["XA11Y_PERF_JSONL"] = str(operations)
            else:
                command = launch._suite_command(suite, report)
                command[0] = python
                env.pop("XA11Y_PERF_JSONL", None)
            log = output / f"{label}-{suite}.log"
            print(f"{label}: {' '.join(command)}", flush=True)
            started = time.perf_counter()
            with log.open("w", encoding="utf-8") as stream:
                result = subprocess.run(
                    command,
                    env=env,
                    stdout=stream,
                    stderr=subprocess.STDOUT,
                    timeout=launch.SUITE_TIMEOUT_S,
                    check=False,
                )
            elapsed = time.perf_counter() - started
            print(log.read_text(encoding="utf-8"), flush=True)
            executed = launch._check_suite_ran_tests(suite, report)
            cases = list(ET.parse(report).getroot().iter("testcase"))
            summary = {
                "suite": suite,
                "elapsed_seconds": round(elapsed, 3),
                "tests": len(cases),
                "skipped": sum(case.find("skipped") is not None for case in cases),
                "failures": sum(case.find("failure") is not None for case in cases),
                "errors": sum(case.find("error") is not None for case in cases),
                "exit_code": result.returncode,
            }
            summaries.append(summary)
            # Write after each suite so failed comparisons retain evidence.
            (output / f"{label}-suites.json").write_text(
                json.dumps(summaries, indent=2) + "\n", encoding="utf-8"
            )
            if result.returncode or executed:
                failed = True
        return {
            "variant": label,
            "python": python,
            "suites": summaries,
            "failed": failed,
        }
    finally:
        launch._kill_app(proc)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-python", required=True)
    parser.add_argument("--head-python", required=True)
    parser.add_argument("--baseline-ref", required=True)
    parser.add_argument("--head-ref", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    results = []
    failures = []
    for label, python in [("base", args.baseline_python), ("head", args.head_python)]:
        try:
            result = run_variant(label, python, output)
            results.append(result)
            if result["failed"]:
                failures.append(
                    f"{label} has failing suites; see logs and JUnit reports"
                )
        except (
            RuntimeError,
            subprocess.TimeoutExpired,
            OSError,
            ET.ParseError,
        ) as error:
            print(f"{label}: {error}", file=sys.stderr, flush=True)
            failures.append(str(error))
    report = {
        "platform": platform.platform(),
        "base": args.baseline_ref,
        "head": args.head_ref,
        "results": results,
        "failures": failures,
    }
    (output / "comparison.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as stream:
            print(
                f"\n### {platform.system()} release desktop comparison\n", file=stream
            )
            print(
                f"Base `{args.baseline_ref}`; head `{args.head_ref}`. Same head Qt fixture/tests, fresh app per variant. Compilation/setup/startup excluded.\n",
                file=stream,
            )
            print(
                "| Variant | Suite | Seconds | Tests | Skipped | Failures | Errors |",
                file=stream,
            )
            print("| --- | --- | ---: | ---: | ---: | ---: | ---: |", file=stream)
            for result in results:
                for suite in result["suites"]:
                    print(
                        f"| {result['variant']} | {suite['suite']} | {suite['elapsed_seconds']} | {suite['tests']} | {suite['skipped']} | {suite['failures']} | {suite['errors']} |",
                        file=stream,
                    )
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
