"""End-to-end plugin behaviour, driven through pytest's own test harness."""

from __future__ import annotations

import pytest
import xa11y

from pytest_xa11y.plugin import _worker_count


def test_missing_launcher_says_how_to_define_one(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        def test_needs_an_app(xa11y_app):
            assert xa11y_app
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines(["*pytest-xa11y needs an `xa11y_launcher` fixture*"])


def test_suite_without_an_app_never_launches_one(pytester: pytest.Pytester):
    # The autouse per-test fixture must stay inert, or every unrelated test in
    # a mixed repo would pay for (and fail on) a launch it never asked for.
    pytester.makepyfile(
        """
        def test_unrelated():
            assert True
        """
    )
    pytester.runpytest().assert_outcomes(passed=1)


def test_report_header_states_the_configuration(pytester: pytest.Pytester):
    pytester.makepyfile("def test_noop(): pass")
    result = pytester.runpytest("--xa11y-startup-timeout=45", "--xa11y-skip=screenshot")
    result.stdout.fnmatch_lines(["xa11y: startup timeout 45s*capabilities disabled: screenshot*"])


def test_timeout_option_sets_the_library_default(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import xa11y

        def test_default_timeout_applied():
            assert xa11y.get_default_timeout() == 12.5
        """
    )
    pytester.runpytest_inprocess("--xa11y-timeout=12.5").assert_outcomes(passed=1)
    # Restore, since set_default_timeout is process-wide.
    xa11y.set_default_timeout(5.0)


def test_requires_marker_skips_a_disabled_capability(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("screenshot")
        def test_needs_capture():
            raise AssertionError("must not run")
        """
    )
    result = pytester.runpytest("--xa11y-skip=screenshot", "-rs")
    result.assert_outcomes(skipped=1)
    result.stdout.fnmatch_lines(["*screenshot unavailable: disabled via --xa11y-skip=screenshot*"])


def test_markers_are_registered(pytester: pytest.Pytester):
    result = pytester.runpytest("--markers")
    result.stdout.fnmatch_lines(["*xa11y_requires(*capabilities)*"])
    result.stdout.fnmatch_lines(["*xa11y_frontmost*"])


def test_structured_diagnosis_becomes_its_own_report_section(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import xa11y

        class Timeout(xa11y.TimeoutError):
            elapsed = 5.0
            condition = "visible"
            selector = 'button[name="Save"]'
            last_observed = "selector never matched"
            candidates = ['button "Export"']
            scope = "window"

        def test_fails_with_a_diagnosis():
            raise Timeout("Timeout after 5.0s")
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(
        [
            "*xa11y diagnosis*",
            "*condition: visible*",
            '*selector: button[[]name="Save"[]]*',
            '*- button "Export"*',
        ]
    )


def test_plain_failures_get_no_diagnosis_section(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        def test_ordinary_failure():
            assert 1 == 2
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(failed=1)
    assert "xa11y diagnosis" not in result.stdout.str()


def test_capabilities_fixture_is_available(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        def test_caps(xa11y_capabilities):
            assert xa11y_capabilities.available("screenshot") is False
            assert "--xa11y-skip" in xa11y_capabilities.reason("screenshot")
        """
    )
    pytester.runpytest("--xa11y-skip=screenshot").assert_outcomes(passed=1)


def test_artifacts_fixture_is_none_by_default(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        def test_artifacts(xa11y_artifacts):
            assert xa11y_artifacts is None
        """
    )
    pytester.runpytest().assert_outcomes(passed=1)


def test_artifacts_fixture_resolves_the_directory(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        def test_artifacts(xa11y_artifacts):
            assert xa11y_artifacts is not None
            assert xa11y_artifacts.is_absolute()
        """
    )
    pytester.runpytest("--xa11y-artifacts=out").assert_outcomes(passed=1)


class _Config:
    def __init__(self, workerinput=None):
        if workerinput is not None:
            self.workerinput = workerinput


def test_worker_count_without_xdist():
    assert _worker_count(_Config()) == 1


def test_worker_count_reads_xdist_workerinput():
    assert _worker_count(_Config({"workercount": 4})) == 4


def test_worker_count_survives_a_malformed_workerinput():
    assert _worker_count(_Config({"workercount": "many"})) == 1


def test_requires_marker_with_no_capabilities_is_rejected(pytester: pytest.Pytester):
    # Guards nothing, but reads as guarded. pytest would otherwise run it.
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires()
        def test_guards_nothing():
            pass
        """
    )
    result = pytester.runpytest()
    result.stderr.fnmatch_lines(["*needs at least one capability*"])
    assert result.ret != 0


def test_typo_in_a_marker_name_is_rejected(pytester: pytest.Pytester):
    # pytest only warns about an unknown marker, so the test would run
    # unguarded. The xa11y_ prefix is reserved so this can be an error.
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_frontmst
        def test_typo():
            pass
        """
    )
    result = pytester.runpytest()
    result.stderr.fnmatch_lines(["*unknown marker @pytest.mark.xa11y_frontmst*"])
    assert result.ret != 0


def test_unknown_capability_is_rejected_at_collection(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("screenshsot")
        def test_typo():
            pass
        """
    )
    result = pytester.runpytest()
    result.stderr.fnmatch_lines(["*unknown capability 'screenshsot'*"])
    assert result.ret != 0


def test_every_bad_marker_is_reported_at_once(pytester: pytest.Pytester):
    # One run, every problem: fixing them one CI cycle at a time is the thing
    # collection-time validation is meant to avoid.
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("nope")
        def test_one():
            pass

        @pytest.mark.xa11y_requires()
        def test_two():
            pass
        """
    )
    result = pytester.runpytest()
    result.stderr.fnmatch_lines(["*unknown capability 'nope'*"])
    result.stderr.fnmatch_lines(["*needs at least one capability*"])


def test_valid_markers_collect_cleanly(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("screenshot", "input_sim")
        @pytest.mark.xa11y_frontmost
        def test_fine():
            pass
        """
    )
    pytester.runpytest("--xa11y-skip=screenshot").assert_outcomes(skipped=1)


FAKE_APP_CONFTEST = """
import os

import pytest
import pytest_xa11y.session as session_module
from pytest_xa11y import AppLauncher


class FakeApp:
    def __init__(self, name):
        self.name = name
        self.pid = 1

    def dump(self, max_depth=None):
        return "<tree of %s>" % self.name


class _Finder:
    def __init__(self):
        self.n = 0

    def __call__(self, predicate, timeout=None):
        self.n += 1
        return FakeApp("fake-%d" % self.n)


@pytest.fixture(scope="session", autouse=True)
def _fake_backend():
    import types, xa11y
    session_module.xa11y = types.SimpleNamespace(
        App=types.SimpleNamespace(find=_Finder(), list=lambda: []),
        TimeoutError=xa11y.TimeoutError,
        SelectorNotMatchedError=xa11y.SelectorNotMatchedError,
        PlatformError=xa11y.PlatformError,
        XA11yError=xa11y.XA11yError,
    )
    yield


@pytest.fixture(scope="session")
def xa11y_launcher():
    # Attach to this very process: it is certainly alive, so the plugin's
    # liveness check has something real to pass against.
    return AppLauncher(attach_pid=os.getpid())
"""


def test_fresh_app_failure_reports_the_app_the_test_drove(pytester: pytest.Pytester):
    # A report that confidently prints the wrong process's tree is worse than
    # printing none: the session app and the fresh app are different processes.
    pytester.makeconftest(FAKE_APP_CONFTEST)
    pytester.makepyfile(
        """
        def test_session_app_starts_first(xa11y_app):
            assert xa11y_app.name == "fake-1"

        def test_fresh_app_fails(xa11y_app, xa11y_fresh_app):
            assert xa11y_fresh_app.name == "nope"
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(passed=1, failed=1)
    output = result.stdout.str()
    assert "<tree of fake-2>" in output, "the fresh app's tree must be in the report"
    assert "<tree of fake-1>" in output, "the session app is still live and also reported"


def test_recorder_events_do_not_leak_into_a_later_test(pytester: pytest.Pytester):
    pytester.makeconftest(FAKE_APP_CONFTEST)
    pytester.makepyfile(
        """
        import pytest


        class Sub:
            def __init__(self):
                self.queue = [Ev()]

            def try_recv(self):
                return self.queue.pop(0) if self.queue else None

            def close(self):
                raise RuntimeError("app already gone")


        class Ev:
            event_type = "focus_changed"
            target = None


        def test_records_then_fails(xa11y_app, xa11y_events, monkeypatch):
            monkeypatch.setattr(type(xa11y_app), "subscribe", lambda self: Sub(), raising=False)
            with xa11y_events(xa11y_app) as events:
                events.drain(0.05)
                assert False, "first failure"


        def test_unrelated_failure(xa11y_app):
            assert False, "second failure"
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(failed=2)
    # A close() that raises must not strand the recorder in session state.
    second = result.stdout.str().split("test_unrelated_failure")[-1]
    assert "focus_changed" not in second


def test_frontmost_marker_rejects_arguments(pytester: pytest.Pytester):
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_frontmost("always")
        def test_args_ignored():
            pass
        """
    )
    result = pytester.runpytest()
    result.stderr.fnmatch_lines(["*xa11y_frontmost takes no arguments*"])
    assert result.ret != 0


def test_header_is_silent_in_an_unconfigured_project(pytester: pytest.Pytester):
    # The plugin auto-loads everywhere it is installed; a suite that never
    # launches an app should not get a line about accessibility timeouts.
    pytester.makepyfile("def test_noop(): pass")
    result = pytester.runpytest()
    assert "xa11y:" not in result.stdout.str()


def test_header_appears_once_something_is_configured(pytester: pytest.Pytester):
    pytester.makepyfile("def test_noop(): pass")
    result = pytester.runpytest("--xa11y-startup-timeout=45")
    result.stdout.fnmatch_lines(["xa11y: startup timeout 45s*"])


BROKEN_CAPTURE_CONFTEST = (
    FAKE_APP_CONFTEST
    + """

@pytest.fixture(scope="session", autouse=True)
def _broken_capture(_fake_backend):
    import pathlib
    import xa11y
    counter = pathlib.Path("capture-attempts.txt")
    counter.write_text("")
    def boom(**kwargs):
        with counter.open("a") as handle:
            handle.write("x")
        raise xa11y.PlatformError("compositor handshake returned garbage")
    xa11y.screenshot = boom
    yield
"""
)


def test_artifacts_never_turn_a_failure_into_an_internalerror(pytester: pytest.Pytester):
    # write_screenshot runs from pytest_runtest_makereport, where a raise is
    # fatal: pytest reports INTERNALERROR and the failing test's own assertion
    # is never printed.
    pytester.makeconftest(BROKEN_CAPTURE_CONFTEST)
    pytester.makepyfile(
        """
        def test_fails(xa11y_app):
            assert False, "the assertion the developer needs to see"
        """
    )
    result = pytester.runpytest("--xa11y-artifacts=out")
    result.assert_outcomes(failed=1)
    output = result.stdout.str()
    assert "INTERNALERROR" not in output
    assert "the assertion the developer needs to see" in output
    assert "screenshot failed" in output


def test_a_failing_probe_is_reported_once_not_per_test(pytester: pytest.Pytester):
    pytester.makeconftest(BROKEN_CAPTURE_CONFTEST)
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("screenshot")
        def test_one(): pass

        @pytest.mark.xa11y_requires("screenshot")
        def test_two(): pass

        @pytest.mark.xa11y_requires("screenshot")
        def test_three(): pass
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(errors=3)
    # One real capture attempt, not one per test. The failure is cached and
    # re-raised the way pytest caches a fixture error, so a thirty-test
    # screenshot module does not make thirty screen-capture attempts.
    attempts = (pytester.path / "capture-attempts.txt").read_text()
    assert len(attempts) == 1, f"probed {len(attempts)} times, expected 1"
    # And the error says what was happening and how to get out of it.
    result.stdout.fnmatch_lines(["*probing the 'screenshot' capability failed*"])
    result.stdout.fnmatch_lines(["*--xa11y-skip=screenshot*"])


FRONTMOST_CONFTEST = (
    FAKE_APP_CONFTEST
    + """

@pytest.fixture(scope="session", autouse=True)
def _refuse_the_front(_fake_backend):
    import pytest_xa11y.plugin as plugin
    plugin.ensure_macos_frontmost = lambda pid, **kw: (False, "front claim failed")
    yield
"""
)


def test_a_failed_front_claim_skips_one_test_not_the_suite(pytester: pytest.Pytester):
    # The claim happens inside the session-scoped app fixture. pytest caches a
    # session-scoped fixture's Skipped and re-raises it for every later
    # consumer, so skipping there would skip the whole suite — and exit 0,
    # reporting a green run that tested nothing.
    pytester.makeconftest(FRONTMOST_CONFTEST)
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_frontmost
        def test_a_wants_the_front(xa11y_app): pass

        def test_b_unrelated(xa11y_app): pass

        def test_c_unrelated(xa11y_app): pass
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(passed=2, skipped=1)


def test_a_factory_app_that_cannot_take_the_front_skips_the_test(pytester: pytest.Pytester):
    # `pytest_runtest_call` claims the front before the body runs, so an app
    # the test launches *itself* is past that point. The factory takes the
    # claim and acts on the answer — a discarded one would leave a marked test
    # driving an app that never got the front, which is the silent
    # misdirection the marker exists to prevent.
    pytester.makeconftest(FRONTMOST_CONFTEST)
    pytester.makepyfile(
        """
        import sys

        import pytest
        from pytest_xa11y import AppLauncher

        @pytest.mark.xa11y_frontmost
        def test_dialog_needs_the_front(xa11y_app_factory):
            launcher = AppLauncher(
                command=[sys.executable, "-c", "import time; time.sleep(5)"], label="dialog"
            )
            xa11y_app_factory(launcher)
            raise AssertionError("must not reach the body after a failed claim")
        """
    )
    result = pytester.runpytest("-rs")
    result.assert_outcomes(skipped=1)
    result.stdout.fnmatch_lines(["*front claim failed*"])


def test_a_factory_app_exiting_does_not_abort_the_run(pytester: pytest.Pytester):
    # Dismissing a dialog *is* its process exiting. An app the suite launched
    # itself must not end the run when it goes.
    pytester.makeconftest(FAKE_APP_CONFTEST)
    pytester.makepyfile(
        """
        import sys
        import pytest
        from pytest_xa11y import AppLauncher

        SHORT = [sys.executable, "-c", "import time; time.sleep(0.2)"]

        def test_launches_and_dismisses(xa11y_app_factory):
            xa11y_app_factory(AppLauncher(command=SHORT, label="dialog"))
            import time; time.sleep(0.5)

        def test_after_the_dialog_closed(xa11y_app):
            assert xa11y_app is not None

        def test_also_runs(xa11y_app):
            assert xa11y_app is not None
        """
    )
    pytester.runpytest().assert_outcomes(passed=3)


def test_the_app_under_test_dying_still_ends_the_run(pytester: pytest.Pytester):
    # The counterpart: the session app is the app under test, and every
    # remaining test would fail on a lookup against a process that is gone.
    pytester.makeconftest(
        FAKE_APP_CONFTEST.replace(
            "return AppLauncher(attach_pid=os.getpid())",
            "import subprocess, sys\n"
            "    proc = subprocess.Popen([sys.executable, '-c', 'pass'])\n"
            "    proc.wait()\n"
            "    return AppLauncher(attach_pid=proc.pid)",
        )
    )
    pytester.makepyfile(
        """
        def test_one(xa11y_app): pass
        def test_two(xa11y_app): pass
        """
    )
    result = pytester.runpytest()
    assert "Interrupted" in result.stdout.str()
