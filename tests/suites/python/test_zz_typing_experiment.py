"""THROWAWAY experiment for the WPF type_text flake. Not for merge.

Compares the current one-batch ``type_text`` ("batched") with the same text
split at each line break into one ``type_text`` call per run of characters and
one ``press("Enter")`` per break ("split"). The split is built from the public
API so the library is untouched.
"""

from __future__ import annotations

import os
import re
import sys
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")
NOTES = 'text_field[name="Notes"]'
ORIGINAL = {"winforms": "Line 1\r\nLine 2\r\nLine 3", "wpf": "Line 1\nLine 2\nLine 3"}
TEXT = "abcdefghijklm\rnopqrstuvwxyz0123456789"

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


def _type_split(sim, text):
    parts = re.split(r"\r\n|\r|\n", text)
    for i, part in enumerate(parts):
        if i:
            sim.press("Enter")
        if part:
            sim.type_text(part)


@pytest.mark.parametrize("mode", ["batched", "split"])
def test_experiment(app, mode):
    reps = 150
    bad = []
    expected = _norm(TEXT)
    notes = app.locator(NOTES)
    sim = xa11y.input_sim()
    try:
        for i in range(reps):
            notes.set_value("")
            _settled(app, "")
            notes.focus()
            notes.wait_focused(timeout=3.0)
            if mode == "batched":
                sim.type_text(TEXT)
            else:
                _type_split(sim, TEXT)
            got = _settled(app, expected)
            if _norm(got) != expected:
                bad.append((i, got))
    finally:
        notes.set_value(ORIGINAL[APP])
    pytest.fail(f"EXPERIMENT {APP} mode={mode}: {len(bad)}/{reps} bad: {bad!r}")
