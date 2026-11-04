"""Raw CLI smoke tests need an on-display fixture, not a scrolled-away widget."""

import runpy
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def pointer_tests():
    path = Path(__file__).resolve().parents[1] / "suites" / "cli" / "test_input_sim.py"
    return runpy.run_path(str(path))


class App:
    def __init__(self, window_bounds):
        self.window_bounds = window_bounds
        self.selectors = []

    def locator(self, selector):
        self.selectors.append(selector)
        bounds = (
            self.window_bounds
            if selector == "window, dialog"
            else SimpleNamespace(x=200, y=-568, width=110, height=24)
        )
        return SimpleNamespace(element=lambda: SimpleNamespace(bounds=bounds))


def test_cocoa_target_uses_fresh_window_instead_of_offscreen_ok(
    pointer_tests, monkeypatch
):
    monkeypatch.setenv("XA11Y_TEST_APP", "cocoa")
    app = App(SimpleNamespace(x=100, y=50, width=700, height=650))
    assert pointer_tests["_pointer_center"](app) == (450, 375)
    app.window_bounds = SimpleNamespace(x=120, y=70, width=700, height=650)
    assert pointer_tests["_pointer_center"](app) == (470, 395)
    assert app.selectors == ["window, dialog", "window, dialog"]


def test_cocoa_window_on_negative_origin_display_is_not_clamped(
    pointer_tests, monkeypatch
):
    monkeypatch.setenv("XA11Y_TEST_APP", "cocoa")
    app = App(SimpleNamespace(x=-900, y=-700, width=700, height=650))
    assert pointer_tests["_pointer_center"](app) == (-550, -375)
    assert app.selectors == ["window, dialog"]


def test_cocoa_missing_window_bounds_fails_without_skip_or_other_target(
    pointer_tests, monkeypatch
):
    monkeypatch.setenv("XA11Y_TEST_APP", "cocoa")
    app = App(None)
    with pytest.raises(AssertionError, match="Cocoa main window has no bounds"):
        pointer_tests["_pointer_center"](app)
    assert app.selectors == ["window, dialog"]


@pytest.mark.parametrize("fixture", ["tauri", "qt", "winforms"])
def test_other_fixtures_keep_the_ok_button_target(pointer_tests, monkeypatch, fixture):
    monkeypatch.setenv("XA11Y_TEST_APP", fixture)
    app = App(None)
    assert pointer_tests["_pointer_center"](app) == (255, -556)
    assert app.selectors == ['button[name="OK"]']


def test_native_off_display_cli_assertions_reject_a_silent_success(pointer_tests):
    with pytest.raises(AssertionError, match="expected rejected target"):
        pointer_tests["test_cli_rejects_an_off_display_point"](
            lambda *args: (0, "ok", "")
        )
