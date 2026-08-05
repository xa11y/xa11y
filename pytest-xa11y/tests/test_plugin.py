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


def test_requires_marker_rejects_an_unknown_capability(pytester: pytest.Pytester):
    # A typo in a marker must fail the test rather than silently never skipping.
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.xa11y_requires("screenshsot")
        def test_typo():
            pass
        """
    )
    result = pytester.runpytest()
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines(["*Unknown capability*screenshsot*"])


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
