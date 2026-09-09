"""Tests for the window-management surface: Element/Locator window verbs,
window state getters, and ``App.windows()``.

All backed by the shared mock provider, which models window state mutations
in place (``minimize`` marks the window minimized and off-screen;
``restore`` reverses both).
"""

import pytest
import xa11y
from xa11y._native import _make_test_action_probe, _make_test_app


def _window(probe):
    return probe.locator("application").descendant("window").element()


def _last_action(probe):
    actions = probe.actions()
    assert actions, "expected at least one recorded action"
    return actions[-1]


# ── Element window verbs ─────────────────────────────────────────────────────


def test_element_window_verbs_record_names():
    probe = _make_test_action_probe()
    win = _window(probe)
    # `raise` is a Python keyword — the binding exposes it as `raise_`; the
    # platform action it dispatches is still "raise".
    for method, action in (
        ("raise_", "raise"),
        ("minimize", "minimize"),
        ("maximize", "maximize"),
        ("restore", "restore"),
        ("close", "close"),
    ):
        probe.clear()
        getattr(win, method)()
        assert _last_action(probe)[1] == action


def test_element_move_to_records_coordinates():
    probe = _make_test_action_probe()
    win = _window(probe)
    probe.clear()
    win.move_to(10, 20)
    _, name, data = _last_action(probe)
    assert name == "move_to"
    assert data == "10,20"


def test_element_resize_to_records_dimensions():
    probe = _make_test_action_probe()
    win = _window(probe)
    probe.clear()
    win.resize_to(640, 480)
    _, name, data = _last_action(probe)
    assert name == "resize_to"
    assert data == "640x480"


def test_element_resize_to_rejects_zero_before_provider():
    probe = _make_test_action_probe()
    win = _window(probe)
    with pytest.raises(xa11y.InvalidActionDataError):
        win.resize_to(0, 100)
    assert probe.actions() == []


def test_minimize_restore_roundtrip_updates_state():
    probe = _make_test_action_probe()
    win = _window(probe)
    win.minimize()
    minimized = _window(probe)
    assert minimized.minimized is True
    # The mock decided the window is not maximized: the real-provider
    # tri-state (UIA WindowVisualState_Minimized → minimized=Some(true),
    # maximized=Some(false)) reports decided-false, not unknown-None.
    assert minimized.maximized is False
    assert minimized.visible is False
    minimized.restore()
    restored = _window(probe)
    assert restored.minimized is False
    assert restored.visible is True


def test_window_state_getters_default_to_none():
    probe = _make_test_action_probe()
    win = _window(probe)
    assert win.minimized is None
    assert win.maximized is None
    assert win.fullscreen is None


# ── Locator window verbs (enabled-only auto-wait gate) ───────────────────────


def test_locator_window_verbs_act_on_minimized_window():
    probe = _make_test_action_probe()
    probe.locator("application").descendant("window").minimize()
    # The window is now visible=False. The locator's window verbs use an
    # enabled-only gate, so restore must act immediately; a visible-gated
    # wait would time out after the process-wide default (5s).
    probe.locator("application").descendant("window").restore()
    assert any(a[1] == "restore" for a in probe.actions())


def test_locator_resize_to_rejects_zero_before_auto_wait():
    probe = _make_test_action_probe()
    loc = probe.locator('window[name="never-matches"]')
    with pytest.raises(xa11y.InvalidActionDataError):
        loc.resize_to(0, 100)


# ── App.windows() ────────────────────────────────────────────────────────────


def test_app_windows_lists_window_children():
    app = _make_test_app()
    windows = app.windows()
    assert len(windows) == 1
    assert windows[0].role == "window"
    assert windows[0].name == "Main Window"
    assert windows[0].minimized is None
