"""THROWAWAY experiment for the WPF type_text flake. Not for merge."""

from __future__ import annotations

import os
import sys
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")
NOTES = 'text_field[name="Notes"]'
ORIGINAL = {"winforms": "Line 1\r\nLine 2\r\nLine 3", "wpf": "Line 1\nLine 2\nLine 3"}

pytestmark = [
    pytest.mark.xa11y_requires("input_sim"),
    pytest.mark.skipif(
        sys.platform != "win32" or APP not in ("winforms", "wpf"), reason="experiment"
    ),
]


def _norm(v: str) -> str:
    return v.replace("\r\n", "\n").replace("\r", "\n")


def _settled(app, expected, timeout=3.0):
    deadline = time.monotonic() + timeout
    value = ""
    while time.monotonic() < deadline:
        value = app.locator(NOTES).element().value or ""
        if _norm(value) == expected:
            return value
        time.sleep(0.05)
    return value


@pytest.mark.parametrize(
    "click,text",
    [
        (True, "line1\rline2"),
        (False, "line1\rline2"),
        (True, "abcdefghijklm\rnopqrstuvwxyz0123456789"),
        (False, "abcdefghijklm\rnopqrstuvwxyz0123456789"),
        (True, "abcdefghijklmnopqrstuvwxyz0123456789"),
        (False, "abcdefghijklmnopqrstuvwxyz0123456789"),
    ],
    ids=["click-short", "noclick-short", "click-long", "noclick-long", "click-nobreak", "noclick-nobreak"],
)
def test_experiment(app, click, text):
    reps = 25
    bad = []
    expected = _norm(text)
    notes = app.locator(NOTES)
    try:
        for i in range(reps):
            notes.set_value("")
            _settled(app, "")
            notes.focus()
            notes.wait_focused(timeout=3.0)
            if click:
                r = notes.element().bounds
                xa11y.input_sim().click((r.x + r.width // 2, r.y + r.height // 2))
            xa11y.input_sim().type_text(text)
            got = _settled(app, expected)
            if _norm(got) != expected:
                bad.append((i, got))
    finally:
        notes.set_value(ORIGINAL[APP])
    pytest.fail(f"EXPERIMENT {APP} click={click} text={text!r}: {len(bad)}/{reps} bad: {bad!r}")
