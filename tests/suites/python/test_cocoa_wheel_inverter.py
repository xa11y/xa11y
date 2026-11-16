"""``scroll`` goes the way the caller asked while a wheel inverter runs (macOS).

Scroll Reverser, UnnaturalScrollWheels and Mos flip line scroll events, which a
mouse wheel makes, and leave continuous ones, which a trackpad makes, alone. A
line event from xa11y therefore scrolled the other way on a Mac that runs one.
``test-apps/inverter`` is such a tool, and the Cocoa app's "Wheel Events" view
keeps one arrow per scroll event: "↓" toward the end of the content, "↑"
toward the start, "→" and "←" for the horizontal axis.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest

import xa11y

INVERTER = (
    Path(__file__).resolve().parents[3] / "test-apps" / "inverter" / "xa11y-inverter"
)
SETTLE_TIMEOUT = 5.0


@pytest.fixture(autouse=True)
def macos_cocoa_only(app_name):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the wheel-event fixture belongs to the macOS Cocoa cell")
    if not INVERTER.exists():
        pytest.fail(
            f"{INVERTER} is missing: run `make -C test-apps/inverter build` and grant it Accessibility"
        )


def wait_for(view, expected):
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = view.element().value or ""
        if value == expected:
            break
        time.sleep(0.05)
    return value


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_scroll_keeps_its_direction_while_a_wheel_inverter_runs(app):
    view = app.locator('[name="Wheel Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    target = view.element()
    inverter = subprocess.Popen(
        [str(INVERTER)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    line = inverter.stdout.readline().strip()
    if line != "ready":
        inverter.kill()
        pytest.fail(f"the inverter did not start: {line} {inverter.stderr.read()}")
    try:
        sim = xa11y.input_sim()
        sim.scroll(target, dx=0, dy=3)
        sim.scroll(target, dx=0, dy=-3)
        sim.scroll(target, dx=3, dy=0)
        value = wait_for(view, "↓↑→")
    finally:
        inverter.terminate()
        report, events = inverter.communicate(timeout=10)
    assert "enabled=true" in report, f"the inverter was not on: {report}"
    assert value == "↓↑→", (
        f"Wheel Events kept {value!r}, not '↓↑→'; the inverter reported {report.strip()}\n{events}"
    )
