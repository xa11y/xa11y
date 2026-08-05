"""Integration tests for ``xa11y.screenshot()`` against the Tauri test app.

The screenshot pipeline needs pixel-capture permission on some platforms
(Screen Recording on macOS, a working X11 DISPLAY or Wayland portal on
Linux). Windows does not need a grant. Where the current session has no
capture path at all, the backend returns ``ActionNotSupportedError`` (mapped
from Rust's ``Error::Unsupported``); on headless CI (Xvfb without a
compositor) X11 ``GetImage`` can additionally reject a region capture whose
coordinates fall outside the root window's reported extents, surfacing as a
``PlatformError``. Both say the session has no usable capture path, not that
the binding is broken, so both skip rather than fail.

Deciding that is pytest-xa11y's job, not this file's: the module-level
``xa11y_requires("screenshot")`` marker probes the session once, and
``xa11y_capabilities.guard("screenshot")`` turns a capture that fails at the
call into a skip.
"""

from __future__ import annotations

import os

import pytest
import xa11y

pytestmark = [
    pytest.mark.skipif(
        os.environ.get("XA11Y_TEST_APP") not in ("tauri", None),
        reason="screenshot tests only run against Tauri (one-per-platform strategy)",
    ),
    pytest.mark.xa11y_requires("screenshot"),
]


def test_capture_full_display_returns_rgba_png(app, xa11y_capabilities):
    with xa11y_capabilities.guard("screenshot"):
        shot = xa11y.screenshot()

    assert shot.width > 0
    assert shot.height > 0
    assert shot.scale > 0.0
    assert len(shot.pixels) == shot.width * shot.height * 4

    png = shot.to_png()
    # PNG magic bytes.
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    assert len(png) > 100


def test_capture_region_matches_requested_size_at_scale(app, xa11y_capabilities):
    rect = (0, 0, 50, 40)
    with xa11y_capabilities.guard("screenshot"):
        shot = xa11y.screenshot(region=rect)

    # Physical pixels = logical * scale, within 1px of rounding.
    expected_w = round(rect[2] * shot.scale)
    expected_h = round(rect[3] * shot.scale)
    assert abs(shot.width - expected_w) <= 1
    assert abs(shot.height - expected_h) <= 1
    assert len(shot.pixels) == shot.width * shot.height * 4


def test_capture_element_uses_element_bounds(app, xa11y_capabilities):
    # Submit is the first button on the widgets page; it appears in the a11y
    # tree on all three platforms (macOS, Windows, Linux AT-SPI). Fall back
    # to any button with bounds if the widget set drifts, so the test stays
    # resilient to unrelated test-app changes.
    for selector in ['button[name="Submit"]', "button"]:
        candidates = app.locator(selector).elements()
        for candidate in candidates:
            if candidate.bounds and candidate.bounds.width > 0 and candidate.bounds.height > 0:
                el = candidate
                bounds = candidate.bounds
                break
        else:
            continue
        break
    else:
        pytest.skip("no button with on-screen bounds available")

    with xa11y_capabilities.guard("screenshot"):
        shot = xa11y.screenshot(element=el)

    expected_w = round(bounds.width * shot.scale)
    expected_h = round(bounds.height * shot.scale)
    assert abs(shot.width - expected_w) <= 1
    assert abs(shot.height - expected_h) <= 1


def test_save_png_writes_valid_file(app, tmp_path, xa11y_capabilities):
    with xa11y_capabilities.guard("screenshot"):
        shot = xa11y.screenshot(region=(0, 0, 20, 20))

    out = tmp_path / "shot.png"
    shot.save_png(out)
    data = out.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"


def test_passing_both_element_and_region_raises(app):
    el = app.locator("button").first().element()
    with pytest.raises(ValueError, match="element.*region"):
        xa11y.screenshot(element=el, region=(0, 0, 10, 10))
