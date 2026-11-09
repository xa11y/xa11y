"""Failure-report construction: tree dumps, structured diagnosis, collectors."""

from __future__ import annotations

from typing import ClassVar

import pytest
import xa11y

from pytest_xa11y import diagnostics as diagnostics_module
from pytest_xa11y import register_diagnostic
from pytest_xa11y.diagnostics import clear_diagnostics, collect, render_diagnosis


class FakeApp:
    def __init__(self, name="my-app", pid=4242, dump="window\n  button"):
        self.name = name
        self.pid = pid
        self._dump = dump

    def dump(self, max_depth=None):
        return self._dump


class BrokenApp(FakeApp):
    def dump(self, max_depth=None):
        raise xa11y.PlatformError("Platform error (1): tree vanished")


class FakeTimeout(xa11y.TimeoutError):
    """Stands in for a real timeout: PyO3 sets the diagnosis fields natively."""

    elapsed = 5.0
    condition = "visible"
    selector = 'dialog[name^="Submit"]'
    last_observed = "selector never matched"
    hidden_reason = None
    auto_scroll = None
    pending = None
    candidates: ClassVar[list] = ['window "Untitled"', 'button "Export"']
    scope = "window\n  button"


@pytest.fixture(autouse=True)
def _clean_registry():
    clear_diagnostics()
    yield
    clear_diagnostics()


def test_collect_reports_identity_and_tree():
    block = collect(FakeApp(), dump_depth=6)
    assert "app: 'my-app' (pid=4242)" in block
    assert "accessibility tree (depth<=6)" in block
    assert "button" in block


def test_collect_without_an_app():
    assert "never started" in collect(None, dump_depth=6)


def test_collect_survives_an_unreadable_tree():
    block = collect(BrokenApp(), dump_depth=6)
    assert "dump failed" in block
    assert "tree vanished" in block


def test_collect_includes_process_output_and_events():
    block = collect(
        FakeApp(),
        dump_depth=4,
        process_output=["stdout: hello", "stderr: <empty>"],
        events="events recorded (1):\n  focus_changed",
    )
    assert "stdout: hello" in block
    assert "focus_changed" in block


def test_platform_state_is_opt_out_so_it_is_not_repeated_per_app(monkeypatch):
    # Which app holds the macOS front describes the desktop, not one app. A
    # report covering several live apps asks once; repeating it would be the
    # same answer at the price of two more osascript round trips per app.
    calls = []
    monkeypatch.setattr(diagnostics_module.sys, "platform", "darwin")
    monkeypatch.setattr(
        diagnostics_module, "macos_frontmost", lambda: (calls.append("front"), (7, "Finder"))[1]
    )
    monkeypatch.setattr(
        diagnostics_module, "macos_visible_processes", lambda: (calls.append("visible"), "x")[1]
    )

    first = collect(FakeApp(), dump_depth=4)
    assert "macOS frontmost" in first
    assert calls == ["front", "visible"]

    second = collect(FakeApp(), dump_depth=4, platform_state=False)
    assert "macOS frontmost" not in second
    assert calls == ["front", "visible"]


def test_registered_collector_contributes():
    register_diagnostic("event log", lambda app: "click at 10,10")
    assert "event log: click at 10,10" in collect(FakeApp(), dump_depth=4)


def test_broken_collector_is_reported_not_swallowed():
    def broken(app):
        raise RuntimeError("collector is out of date")

    register_diagnostic("event log", broken)
    block = collect(FakeApp(), dump_depth=4)
    assert "collector raised" in block
    assert "out of date" in block


def test_render_diagnosis_labels_every_field():
    rendered = render_diagnosis(FakeTimeout("Timeout after 5.0s"))
    assert "FakeTimeout diagnosis:" in rendered
    assert "condition: visible" in rendered
    assert 'selector: dialog[name^="Submit"]' in rendered
    assert "elapsed: 5.00s" in rendered
    assert '- button "Export"' in rendered
    assert "search scope (bounded):" in rendered


def test_render_diagnosis_says_why_a_matched_element_was_hidden():
    class HiddenTimeout(FakeTimeout):
        last_observed = 'matched button "Save" (visible=false, enabled=true, focused=false)'
        hidden_reason = "scrolled_out"

    assert "hidden_reason: scrolled_out" in render_diagnosis(HiddenTimeout("Timeout after 5.0s"))
    # Absent on a timeout that observed nothing hidden.
    assert "hidden_reason" not in render_diagnosis(FakeTimeout("Timeout after 5.0s"))


def test_render_diagnosis_reports_the_delivery_disposition():
    class Undelivered(FakeTimeout):
        delivery = "not_delivered"

    assert "delivery: not_delivered" in render_diagnosis(Undelivered("Timeout after 5.0s"))


def test_render_diagnosis_preserves_plain_pointer_geometry():
    error = xa11y.SelectorNotMatchedError("point matches no display")
    geometry = {
        "point": {"x": 1000, "y": 1000},
        "element_bounds": {"x": 900, "y": 900, "width": 50, "height": 50},
        "anchor": {"offset": {"dx": 100, "dy": 100}},
        "checked_point": {"x": 2000, "y": 2000},
        "coordinate_space": "x11 physical root pixels",
        "displays": [{"x": -100, "y": 0, "width": 800, "height": 600}],
        "omitted_displays": 2,
    }
    error.pointer_target = geometry
    error.delivery = "not_delivered"
    rendered = render_diagnosis(error)
    assert f"pointer_target: {geometry}" in rendered
    assert "delivery: not_delivered" in rendered
    assert error.pointer_target is geometry
    assert str(error) == "point matches no display"
    assert "pointer_target" not in render_diagnosis(FakeTimeout("Timeout after 5.0s"))


def test_render_diagnosis_says_how_an_auto_scroll_ended():
    class RefusedTimeout(FakeTimeout):
        hidden_reason = "scrolled_out"
        auto_scroll = "refused"

    assert "auto_scroll: refused" in render_diagnosis(RefusedTimeout("Timeout after 5.0s"))
    assert "auto_scroll" not in render_diagnosis(FakeTimeout("Timeout after 5.0s"))


def test_render_diagnosis_names_the_call_a_timeout_cut_short():
    class HungTimeout(FakeTimeout):
        pending = 'GetChildren on button "OK"'

    assert 'pending: GetChildren on button "OK"' in render_diagnosis(
        HungTimeout("Timeout after 5.0s")
    )
    assert "pending" not in render_diagnosis(FakeTimeout("Timeout after 5.0s"))


def test_render_diagnosis_ignores_foreign_exceptions():
    assert render_diagnosis(ValueError("not an xa11y error")) is None


def test_render_diagnosis_ignores_errors_without_a_diagnosis():
    assert render_diagnosis(xa11y.InvalidSelectorError("$$$")) is None
