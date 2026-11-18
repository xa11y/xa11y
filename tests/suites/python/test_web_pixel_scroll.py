"""Continuous macOS input moves a real Chromium overflow scroller."""

import sys
import time

import pytest

import xa11y


@pytest.fixture(autouse=True)
def macos_electron_only(app_name):
    if sys.platform != "darwin" or app_name != "electron":
        pytest.skip("pixel distance is the macOS input contract")


@pytest.mark.parametrize("dx,dy", [(0, 3), (0, -3), (3, 0), (-3, 0)])
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_pixel_scroll_distance_in_chromium(app, dx, dy):
    target = app.locator('[name="Pixel Scroll Area"]').element()
    output = app.locator('[name="Pixel Scroll Offset"]')

    def offset():
        node = output.element()
        text = node.value or "".join(c.value or c.name or "" for c in node.children())
        return tuple(float(n) for n in text.split(","))

    before = offset()
    xa11y.input_sim().scroll(target, dx=dx, dy=dy)
    expected = (before[0] + 10 * dx, before[1] + 10 * dy)
    deadline = time.monotonic() + 5
    after = offset()
    while time.monotonic() < deadline and after != pytest.approx(expected, abs=1):
        time.sleep(0.05)
        after = offset()
    assert after == pytest.approx(expected, abs=1), (before, after, expected)
