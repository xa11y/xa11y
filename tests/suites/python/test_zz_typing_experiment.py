"""THROWAWAY experiment for the WPF type_text flake. Not for merge.

Variants (150 reps each):
  batched  - current type_text on text with a line break
  nobreak  - current type_text on text of the same length without one
  perkey   - one press() per character (pure measurement, no library change)
The experiment WPF test app logs key / text-input / text-changed events to a
temp file; the first few corrupted iterations dump their slice of that log.
"""

from __future__ import annotations

import os
import sys
import tempfile
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")
NOTES = 'text_field[name="Notes"]'
ORIGINAL = {"winforms": "Line 1\r\nLine 2\r\nLine 3", "wpf": "Line 1\nLine 2\nLine 3"}
BREAK = "abcdefghijklm\rnopqrstuvwxyz0123456789"
NOBREAK = "abcdefghijklmnopqrstuvwxyz0123456789"
LOG = os.path.join(tempfile.gettempdir(), "xa11y-wpf-textlog.txt")

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


def _log_size():
    try:
        return os.path.getsize(LOG)
    except OSError:
        return 0


def _log_since(offset):
    try:
        with open(LOG, "rb") as f:
            f.seek(offset)
            return f.read().decode("utf-8", "replace")
    except OSError as e:
        return f"<no log: {e}>"


def _type_perkey(sim, text):
    for ch in text:
        sim.press("Enter" if ch in "\r\n" else ch)


@pytest.mark.parametrize("mode", ["batched", "nobreak", "perkey"])
def test_experiment(app, mode):
    # 150 reps x 3 variants overran the harness's 900 s suite timeout on WPF
    # (about 2.8 s per repetition there), which killed the run before pytest
    # printed any failure detail.
    reps = 60 if APP == "wpf" else 150
    bad = []
    dumps = []
    text = NOBREAK if mode == "nobreak" else BREAK
    expected = _norm(text)
    notes = app.locator(NOTES)
    sim = xa11y.input_sim()
    try:
        for i in range(reps):
            notes.set_value("")
            _settled(app, "")
            notes.focus()
            notes.wait_focused(timeout=3.0)
            mark = _log_size()
            if mode == "perkey":
                _type_perkey(sim, text)
            else:
                sim.type_text(text)
            got = _settled(app, expected)
            if _norm(got) != expected:
                bad.append((i, got))
                if len(dumps) < 3:
                    dumps.append((i, _log_since(mark)))
    finally:
        notes.set_value(ORIGINAL[APP])
    for i, d in dumps:
        print(f"EXPLOG {APP} {mode} iteration {i}\n{d}\nEXPLOG-END", flush=True)
    pytest.fail(f"EXPERIMENT {APP} mode={mode}: {len(bad)}/{reps} bad: {bad!r}")
