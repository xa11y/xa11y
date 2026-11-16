"""Raw input gets through the filtering event taps of other programs (macOS).

Dictation, hotkey and scroll tools install a filtering event tap, which holds
each event it sees until its callback returns. macOS times such a tap from when
an event joins its queue, so events posted back to back queue up behind a slow
tap, the OS turns the tap off, and later events overtake the held ones: a
chord loses its Command key and keys arrive out of order. ``test-apps/taps``
is such a tap, and the Cocoa app's "Key Events" view keeps one character per
key event, with "⌘" in front of a key that has Command held.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest

import xa11y

TAPS = Path(__file__).resolve().parents[3] / "test-apps" / "taps" / "xa11y-taps"
SETTLE_TIMEOUT = 5.0


@pytest.fixture(autouse=True)
def macos_cocoa_only(app_name):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")
    if not TAPS.exists():
        pytest.fail(
            f"{TAPS} is missing: run `make -C test-apps/taps build` and grant it Accessibility"
        )


class Tap:
    def __init__(self, *args: str) -> None:
        self.process = subprocess.Popen(
            [str(TAPS), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        line = self.process.stdout.readline().strip()
        if line != "ready":
            self.process.kill()
            raise RuntimeError(
                f"the fixture tap did not start: {line} {self.process.stderr.read()}"
            )

    def stop(self) -> dict[str, str]:
        self.process.terminate()
        out, _ = self.process.communicate(timeout=10)
        return dict(field.split("=") for field in out.split())


def key_events(app):
    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    return view


def wait_for(view, expected):
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = view.element().value or ""
        if value == expected:
            break
        time.sleep(0.05)
    return value


@pytest.mark.parametrize("location", ["hid", "session"])
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_keys_and_chords_keep_their_order_through_a_slow_filtering_tap(app, location):
    view = key_events(app)
    tap = Tap("--location", location, "--events", "keys,flags", "--delay-ms", "200")
    try:
        sim = xa11y.input_sim()
        sim.press("i")
        sim.chord("u", ["Meta"])
        sim.press("i")
        sim.type_text("ab")
        value = wait_for(view, "i⌘uiab")
    finally:
        report = tap.stop()
    # 12 events: two keys for each of i, u, i, a and b, and the Command down and up.
    held_all = (
        report["enabled"] == "true"
        and report["reenabled"] == "0"
        and int(report["held"]) >= 12
    )
    assert held_all, f"the fixture tap did not hold every event: {report}"
    assert value == "i⌘uiab", f"Key Events kept {value!r}, not 'i⌘uiab'"


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_a_tap_that_drops_keys_gives_an_error_that_names_its_program(app):
    view = key_events(app)
    tap = Tap("--location", "session", "--events", "keys", "--drop")
    try:
        with pytest.raises(xa11y.TimeoutError) as raised:
            xa11y.input_sim().press("x")
    finally:
        tap.stop()
    assert any(str(TAPS) in c for c in raised.value.candidates), raised.value.candidates
    xa11y.input_sim().press("y")
    assert wait_for(view, "y") == "y"
