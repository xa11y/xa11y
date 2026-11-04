"""Check the real input fixture's chord predicate and platform tap selection."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def input_events(monkeypatch):
    monkeypatch.setitem(sys.modules, "xa11y", SimpleNamespace())
    path = (
        Path(__file__).resolve().parents[1] / "suites" / "python" / "test_input_sim.py"
    )
    spec = importlib.util.spec_from_file_location("input_meta_events", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize(
    ("log", "tap", "expected"),
    [
        ("keyup key=a code=KeyA mods=-", "F13", False),
        ("keydown key=; code=Semicolon mods=meta", "F13", False),
        ("keydown key=F13 code=F13 mods=-", "F13", False),
        ("keydown key=F130 code=F13 mods=meta", "F13", False),
        (
            "keyup key=a code=KeyA mods=meta\nkeydown key=F13 code=F13 mods=-",
            "F13",
            False,
        ),
        ("keydown key=F13 code=F13 mods=meta", "F13", True),
        ("keydown key=F13 code=F13 mods=shift,meta", "F13", True),
        ("keydown key=; code=Semicolon mods=meta", ";", True),
        (
            "keyup key=; code=Semicolon mods=-\nkeydown key=Super code=Super mods=-",
            ";",
            True,
        ),
    ],
)
def test_command_chord_requires_the_exact_tap_and_modifier(
    input_events, log, tap, expected
):
    assert input_events._command_chord_seen(log, tap) is expected


@pytest.mark.parametrize(
    ("platform", "tap", "log"),
    [
        ("win32", "F13", "keydown key=F13 code=F13 mods=meta"),
        ("darwin", ";", "keydown key=; code=Semicolon mods=meta"),
        (
            "linux",
            ";",
            "keyup key=; code=Semicolon mods=-\nkeydown key=Super code=Super mods=-",
        ),
    ],
)
def test_actual_meta_chord_uses_only_the_windows_replacement(
    input_events, monkeypatch, platform, tap, log
):
    calls = []
    monkeypatch.setattr(input_events, "sys", SimpleNamespace(platform=platform))
    monkeypatch.setattr(input_events, "_clear_log", lambda app: calls.append("clear"))
    monkeypatch.setattr(
        input_events, "_focus_typed_field", lambda app: calls.append("focus")
    )

    def wait(app, predicate):
        assert not predicate("keyup key=a code=KeyA mods=-")
        assert predicate(log)
        return log

    monkeypatch.setattr(input_events, "_wait_for_log", wait)
    sim = SimpleNamespace(
        type_text=lambda text: calls.append(("type", text)),
        chord=lambda key, modifiers: calls.append(("chord", key, modifiers)),
    )
    input_events.test_platform_meta_chord(object(), sim)
    assert calls == ["clear", "focus", ("type", "hello"), ("chord", tap, ["Meta"])]
