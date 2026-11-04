"""Exercise the actual Rebuild staleness test without a desktop or bindings."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest


class MissingError(Exception):
    pass


class ProviderError(Exception):
    pass


@pytest.fixture
def actions(monkeypatch):
    # Load the real integration module with a temporary xa11y stub, so these
    # cases exercise its actual wait and retained stale/fresh assertions.
    monkeypatch.setitem(
        sys.modules,
        "xa11y",
        SimpleNamespace(SelectorNotMatchedError=MissingError),
    )
    path = Path(__file__).resolve().parents[1] / "suites" / "python" / "test_actions.py"
    spec = importlib.util.spec_from_file_location("replacement_wait_actions", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Clock:
    def __init__(self):
        self.ticks = 0

    def monotonic(self):
        return self.ticks / 10

    def sleep(self, duration):
        assert duration == 0.1
        self.ticks += 1


class RebuildLocator:
    def __init__(self, outcomes, *, missing_forever=False, stale=True):
        self.outcomes = iter(outcomes)
        self.missing_forever = missing_forever
        self.stale = stale
        self.first = True
        self.old_presses = 0
        self.fresh_presses = 0

    def count(self):
        return 1

    def _press_old(self):
        self.old_presses += 1
        if self.old_presses > 1 and self.stale:
            raise MissingError("stale")

    def element(self):
        if self.first:
            self.first = False
            return SimpleNamespace(description="old", press=self._press_old)
        if self.missing_forever:
            raise MissingError("not published")
        outcome = next(self.outcomes, "old")
        if isinstance(outcome, BaseException):
            raise outcome
        return SimpleNamespace(description=outcome)

    def press(self):
        self.fresh_presses += 1


def run_rebuild_test(actions, monkeypatch, locator):
    clock = Clock()
    monkeypatch.setattr(actions, "time", clock)

    def locate(selector):
        assert selector == 'button[name="Rebuild"]'
        return locator

    app = SimpleNamespace(locator=locate)
    return clock, lambda: actions.test_action_on_a_destroyed_widget_is_stale(app)


def test_rebuild_wait_allows_missing_then_replacement(actions, monkeypatch):
    locator = RebuildLocator(["old", MissingError("not published"), "new"])
    clock, run = run_rebuild_test(actions, monkeypatch, locator)
    run()
    assert clock.ticks == 2
    assert locator.old_presses == 2
    assert locator.fresh_presses == 1


@pytest.mark.parametrize("missing_forever", [False, True])
def test_rebuild_wait_keeps_five_second_deadline(actions, monkeypatch, missing_forever):
    locator = RebuildLocator([], missing_forever=missing_forever)
    clock, run = run_rebuild_test(actions, monkeypatch, locator)
    with pytest.raises(AssertionError, match="the Rebuild button never swapped"):
        run()
    assert clock.monotonic() == 5.0
    assert locator.old_presses == 1
    assert locator.fresh_presses == 0


def test_rebuild_wait_propagates_provider_errors(actions, monkeypatch):
    error = ProviderError("provider disconnected")
    locator = RebuildLocator([error])
    clock, run = run_rebuild_test(actions, monkeypatch, locator)
    with pytest.raises(ProviderError) as caught:
        run()
    assert caught.value is error
    assert clock.ticks == 0
    assert locator.old_presses == 1
    assert locator.fresh_presses == 0


def test_rebuild_wait_still_requires_old_handle_to_be_stale(actions, monkeypatch):
    locator = RebuildLocator(["new"], stale=False)
    _, run = run_rebuild_test(actions, monkeypatch, locator)
    with pytest.raises(pytest.fail.Exception, match="DID NOT RAISE"):
        run()
    assert locator.old_presses == 2
    assert locator.fresh_presses == 0
