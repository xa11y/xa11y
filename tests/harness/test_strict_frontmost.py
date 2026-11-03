"""Self-tests for tests/strict_frontmost.py (XA11Y_REQUIRE_FRONTMOST).

The real hook only matters on a macOS integ cell, where a wrong
classification shows up as a red cell or a silent skip. These drive it with
pytester against a stand-in ``plugin.py`` whose claim always fails, so the
classification is checked on any platform without pytest-xa11y installed.
"""

from __future__ import annotations

import pytest

pytest_plugins = ["pytester"]

_FAKE_PLUGIN = '''
import pytest

_REASON = "app (pid=1) is not frontmost after 10s; frontmost is 'Finder'"


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_call(item):
    if item.get_closest_marker("xa11y_frontmost") is not None:
        if item.get_closest_marker("claim_ok") is None:
            pytest.skip(_REASON)
    yield


@pytest.fixture
def xa11y_app_factory():
    def launch():
        pytest.skip(_REASON)

    return launch


def guard():
    """Stands in for Capabilities.guard(): same file, not a claim."""
    pytest.skip("input_sim unsupported in this session")
'''

_CONFTEST = """
from pathlib import Path

import pytest

import fake_xa11y.plugin as plugin
from tests import strict_frontmost

pytest_plugins = ["fake_xa11y.plugin"]


def pytest_configure(config):
    config.addinivalue_line("markers", "xa11y_frontmost: claim")
    config.addinivalue_line("markers", "claim_ok: the fake claim succeeds")


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    strict_frontmost.enforce(
        item, call, outcome.get_result(), Path(plugin.__file__), lambda: "Finder, Dock"
    )
"""

_TESTS = """
import pytest

from fake_xa11y.plugin import guard


@pytest.mark.xa11y_frontmost
def test_claim_in_hook():
    pass


@pytest.mark.xa11y_frontmost
@pytest.mark.claim_ok
def test_claim_in_factory(xa11y_app_factory):
    xa11y_app_factory()


@pytest.mark.xa11y_frontmost
@pytest.mark.claim_ok
def test_body_skip():
    pytest.skip("belongs to another cell")


@pytest.mark.xa11y_frontmost
@pytest.mark.claim_ok
def test_capability_guard_skip():
    guard()


@pytest.mark.xa11y_frontmost
@pytest.mark.skipif(True, reason="setup-phase skipif")
def test_setup_skipif():
    pass


def test_unmarked_body_skip():
    pytest.skip("unmarked")
"""


@pytest.fixture
def project(pytester: pytest.Pytester) -> pytest.Pytester:
    pkg = pytester.mkpydir("fake_xa11y")
    (pkg / "plugin.py").write_text(_FAKE_PLUGIN)
    pytester.makeconftest(_CONFTEST)
    pytester.makepyfile(test_cases=_TESTS)
    pytester.syspathinsert()
    return pytester


def _run(project: pytest.Pytester) -> pytest.HookRecorder:
    # The conftest imports the stand-in plugin before registering it, which
    # pytest notes as a module it cannot assert-rewrite; that is irrelevant.
    return project.inline_run("-W", "ignore::pytest.PytestAssertRewriteWarning")


def _outcomes(result: pytest.HookRecorder) -> dict[str, str]:
    reports = result.getreports("pytest_runtest_logreport")
    out: dict[str, str] = {}
    for rep in reports:
        name = rep.nodeid.split("::")[-1]
        if rep.when == "call" or (rep.when == "setup" and not rep.passed):
            out[name] = rep.outcome
    return out


def test_strict_converts_only_claim_skips(project, monkeypatch):
    monkeypatch.setenv("XA11Y_REQUIRE_FRONTMOST", "1")
    result = _run(project)
    assert _outcomes(result) == {
        "test_claim_in_hook": "failed",
        "test_claim_in_factory": "failed",
        "test_body_skip": "skipped",
        "test_capability_guard_skip": "skipped",
        "test_setup_skipif": "skipped",
        "test_unmarked_body_skip": "skipped",
    }
    failed = [r for r in result.getreports("pytest_runtest_logreport") if r.failed]
    message = str(failed[0].longrepr)
    assert "XA11Y_REQUIRE_FRONTMOST" in message
    assert "not frontmost" in message
    assert "visible processes: Finder, Dock" in message


def test_without_env_every_skip_stays_a_skip(project, monkeypatch):
    monkeypatch.delenv("XA11Y_REQUIRE_FRONTMOST", raising=False)
    result = _run(project)
    assert set(_outcomes(result).values()) == {"skipped"}
