"""Unit tests for the shared integration-test harness.

These are plain unit tests — they never launch a test app. They exist because
the harness is the one piece of test infrastructure whose bugs are invisible by
construction: when it declines to run a suite, the matrix cell stays green and
nothing else in CI notices.

Issue #327 is the worked example. `_find_cli_binary()` probed
``target/debug/xa11y`` with no ``.exe`` suffix, so on Windows it never found
the binary `cargo build --workspace` had just produced, and the harness skipped
the CLI suite with a warning. Four Windows matrix cells claimed CLI coverage
they had never once executed.

Run with:  cargo xtask test-harness   (or: python -m pytest tests/harness)
"""

from __future__ import annotations

import subprocess
import inspect
import sys
from pathlib import Path

import pytest

from tests.harness import launch


# ── CLI binary discovery (issue #327) ────────────────────────────────────────


def test_cli_binary_name_carries_the_platform_exe_suffix():
    """The probed filename must match what cargo actually emits per platform."""
    if sys.platform == "win32":
        assert launch.CLI_BINARY_NAME == "xa11y.exe"
    else:
        assert launch.CLI_BINARY_NAME == "xa11y"


def test_cli_binary_candidates_probe_debug_then_release():
    candidates = launch.cli_binary_candidates()
    assert [p.parent.name for p in candidates] == ["debug", "release"]
    assert all(p.name == launch.CLI_BINARY_NAME for p in candidates)
    assert all(p.parent.parent.name == "target" for p in candidates)


def test_find_cli_binary_finds_a_debug_build(tmp_path, monkeypatch):
    """A binary at target/debug/<name> is found, suffix and all."""
    debug_dir = tmp_path / "target" / "debug"
    debug_dir.mkdir(parents=True)
    built = debug_dir / launch.CLI_BINARY_NAME
    built.write_text("")

    monkeypatch.setattr(launch, "PROJECT_ROOT", tmp_path)
    assert launch.find_cli_binary() == str(built)


def test_find_cli_binary_prefers_debug_over_release(tmp_path, monkeypatch):
    for profile in ("debug", "release"):
        d = tmp_path / "target" / profile
        d.mkdir(parents=True)
        (d / launch.CLI_BINARY_NAME).write_text("")

    monkeypatch.setattr(launch, "PROJECT_ROOT", tmp_path)
    assert Path(launch.find_cli_binary()).parent.name == "debug"


def test_find_cli_binary_ignores_a_suffixless_file_on_windows(tmp_path, monkeypatch):
    """Guards the inverse of #327: on Windows a bare `xa11y` is not the binary.

    On Windows `cargo build` never emits a suffixless `xa11y`, so if one is
    present it is not the executable and must not be handed to the suite.
    """
    if sys.platform != "win32":
        pytest.skip("suffix disambiguation only differs on Windows")
    debug_dir = tmp_path / "target" / "debug"
    debug_dir.mkdir(parents=True)
    (debug_dir / "xa11y").write_text("")

    monkeypatch.setattr(launch, "PROJECT_ROOT", tmp_path)
    monkeypatch.setattr(launch.shutil, "which", lambda _name: None)
    assert launch.find_cli_binary() is None


def test_cli_binary_not_found_message_names_every_probed_location(tmp_path, monkeypatch):
    monkeypatch.setattr(launch, "PROJECT_ROOT", tmp_path)
    message = launch.cli_binary_not_found_message()
    assert launch.CLI_BINARY_NAME in message
    for candidate in launch.cli_binary_candidates():
        assert str(candidate) in message
    assert "cargo build -p xa11y" in message


# ── A requested suite that cannot run is an error, never a warning ───────────


class _FakeProc:
    pid = 4242


def test_missing_cli_binary_fails_the_run(tmp_path, monkeypatch, capsys):
    """The core regression guard: no binary, no silent skip.

    Before #327 this path printed a WARNING and `continue`d, so the harness
    exited 0 having run nothing.
    """
    monkeypatch.setattr(launch, "find_cli_binary", lambda: None)

    rc = launch._run_suites("qt", ["cli"], _FakeProc(), "xa11y-qt-test-app")

    assert rc != 0, "a requested suite that could not run must fail the harness"
    out = capsys.readouterr().out
    assert "ERROR" in out
    assert launch._DID_NOT_RUN in out


def test_declared_skip_is_not_an_error(monkeypatch, capsys):
    """Declared per-app skips stay green — they are documented exclusions."""
    assert "cli" in launch.declared_suite_skips("accesskit")

    rc = launch._run_suites("accesskit", ["cli"], _FakeProc(), "xa11y-test-app")

    assert rc == 0
    out = capsys.readouterr().out
    assert launch._DECLARED_SKIP in out


def test_no_app_declares_a_skip_except_accesskit():
    """Keeps the exclusion list from quietly growing."""
    for app in launch.VALID_APPS:
        if app == "accesskit":
            continue
        assert launch.declared_suite_skips(app) == set(), (
            f"{app} declares suite skips; add it to this test (and to "
            f"tests/matrix.yaml) deliberately"
        )


def test_ledger_reports_every_requested_suite(monkeypatch, capsys):
    """Every requested suite appears in the recap with an explicit outcome."""
    monkeypatch.setattr(launch, "find_cli_binary", lambda: None)
    monkeypatch.setattr(
        launch.subprocess,
        "run",
        lambda *a, **kw: subprocess.CompletedProcess(a[0] if a else [], 1),
    )

    launch._run_suites("qt", ["python", "js", "cli"], _FakeProc(), "app")

    out = capsys.readouterr().out
    assert "=== suite ledger for qt ===" in out
    for suite in ("python", "js", "cli"):
        assert suite in out


# ── Zero-executed-test guard ─────────────────────────────────────────────────


def _write(tmp_path: Path, xml: str) -> Path:
    path = tmp_path / "report.xml"
    path.write_text(xml)
    return path


def test_all_skipped_pytest_report_fails(tmp_path):
    report = _write(
        tmp_path,
        """<testsuites><testsuite name="pytest" tests="2" skipped="2">
             <testcase name="a"><skipped message="nope"/></testcase>
             <testcase name="b"><skipped message="nope"/></testcase>
           </testsuite></testsuites>""",
    )
    assert launch._check_suite_ran_tests("python", report) == 1


def test_partially_skipped_pytest_report_passes(tmp_path):
    report = _write(
        tmp_path,
        """<testsuites><testsuite name="pytest" tests="2" skipped="1">
             <testcase name="a"><skipped message="nope"/></testcase>
             <testcase name="b"/>
           </testsuite></testsuites>""",
    )
    assert launch._check_suite_ran_tests("python", report) == 0


def test_node_junit_top_level_cases_are_counted(tmp_path):
    """Node emits top-level cases as direct children of <testsuites>.

    Counting the `tests=`/`skipped=` attributes of <testsuite> — as the old
    implementation did — would see zero here and fail a healthy JS run.
    """
    report = _write(
        tmp_path,
        """<testsuites>
             <testcase name="one" classname="test"/>
             <testcase name="two" classname="test"><skipped type="skipped"/></testcase>
             <testsuite name="grp" tests="1" skipped="0">
               <testcase name="three" classname="test"/>
             </testsuite>
           </testsuites>""",
    )
    assert launch._check_suite_ran_tests("js", report) == 0


def test_empty_report_fails(tmp_path):
    report = _write(tmp_path, "<testsuites></testsuites>")
    assert launch._check_suite_ran_tests("js", report) == 1


def test_unreadable_report_fails(tmp_path):
    assert launch._check_suite_ran_tests("js", tmp_path / "does-not-exist.xml") == 1


# ── Every suite is audited ───────────────────────────────────────────────────


@pytest.mark.parametrize("suite", launch.VALID_SUITES)
def test_every_suite_writes_a_junit_report(suite, tmp_path):
    """No suite may opt out of the did-anything-run audit."""
    report = tmp_path / "report.xml"
    cmd = launch._suite_command(suite, report)
    assert any(str(report) in arg for arg in cmd), (
        f"{suite} suite command does not write a junit report: {cmd}"
    )


# ── Startup and content-readiness budgets ────────────────────────────────────


def test_discovery_and_readiness_have_independent_budgets():
    """Readiness must not inherit whatever discovery left over.

    The two shared a deadline, so an app that took most of the startup budget
    to register left the content wait its 1-second floor. That is how the
    Tauri suites came to run against a Windows window holding nothing but
    Minimize/Maximize/Close: WebView2 had not painted the page inside one
    second, and the gate gave up.
    """
    assert launch.CONTENT_READY_TIMEOUT >= launch.STARTUP_TIMEOUT, (
        "content readiness gets its own full budget, not the residue of discovery"
    )


def test_windows_gets_a_longer_startup_budget():
    """Both phases are measurably slower on Windows, and both have overrun.

    UIA registration for the egui app finished at the 30s boundary — the app
    appeared in the very next enumeration — and WebView2 content load has
    overrun as well.
    """
    expected = 60.0 if sys.platform == "win32" else 30.0
    assert launch._DEFAULT_STARTUP_TIMEOUT == expected


def test_both_budgets_are_overridable_by_environment():
    """A slow machine must be able to buy more time without a code change."""
    source = inspect.getsource(launch)
    assert "XA11Y_TEST_STARTUP_TIMEOUT" in source
    assert "XA11Y_TEST_CONTENT_TIMEOUT" in source


def test_readiness_failure_is_fatal_not_a_warning():
    """A gate that continues is not a gate.

    "Proceeding anyway" turned one clear "the app never became ready" into a
    scatter of unrelated-looking failures in whichever suite ran first, while
    the app finished loading behind them — the shape of issue #327, where a
    cell reported on work it had not really done.
    """
    source = inspect.getsource(launch._launch_app)
    assert "proceeding anyway" not in source, (
        "the content-readiness gate must fail rather than warn and continue"
    )
    # The message is wrapped across f-string lines, so match a contiguous
    # fragment and the raise itself rather than the whole sentence.
    assert "content never became " in source
    assert "raise RuntimeError(" in source
    assert "XA11Y_TEST_CONTENT_TIMEOUT" in source
