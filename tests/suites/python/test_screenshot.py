"""Integration tests for ``xa11y.screenshot()`` against the Tauri test app.

The screenshot pipeline needs pixel-capture permission on some platforms
(Screen Recording on macOS, a working X11 DISPLAY or Wayland portal on
Linux). Windows does not need a grant. Where the current session has no
capture path at all, the backend returns ``ActionNotSupportedError`` (mapped
from Rust's ``Error::Unsupported``); the tests treat that as a skip rather
than a failure so they stay useful on headless CI runners.
"""

from __future__ import annotations

import os

import pytest
import xa11y

pytestmark = pytest.mark.skipif(
    os.environ.get("XA11Y_TEST_APP") not in ("tauri", None),
    reason="screenshot tests only run against Tauri (one-per-platform strategy)",
)


def _capture_or_skip(fn):
    """Run a capture call, skipping on Unsupported/PermissionDenied.

    On headless CI (Xvfb without a compositor), X11 `GetImage` can reject
    region captures whose coordinates fall outside the root window's
    reported extents — this surfaces as ``PlatformError`` with a
    ``GetImage`` / ``Match`` message. Treat that as a skip, not a failure:
    it signals the session has no usable capture path for that region, not
    that the binding is broken.
    """
    try:
        return fn()
    except xa11y.ActionNotSupportedError as e:
        pytest.skip(f"capture unsupported in this session: {e}")
    except xa11y.PermissionDeniedError as e:
        pytest.skip(f"screen capture permission not granted: {e}")
    except xa11y.PlatformError as e:
        msg = str(e)
        if "GetImage" in msg or "BadMatch" in msg or "SCScreenshotManager" in msg:
            pytest.skip(f"screen capture not available in this session: {e}")
        raise


def test_capture_full_display_returns_rgba_png(app):
    shot = _capture_or_skip(xa11y.screenshot)

    assert shot.width > 0
    assert shot.height > 0
    assert shot.scale > 0.0
    assert len(shot.pixels) == shot.width * shot.height * 4

    png = shot.to_png()
    # PNG magic bytes.
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    assert len(png) > 100


def test_capture_region_matches_requested_size_at_scale(app):
    rect = (0, 0, 50, 40)
    shot = _capture_or_skip(lambda: xa11y.screenshot(region=rect))

    # Physical pixels = logical * scale, within 1px of rounding.
    expected_w = round(rect[2] * shot.scale)
    expected_h = round(rect[3] * shot.scale)
    assert abs(shot.width - expected_w) <= 1
    assert abs(shot.height - expected_h) <= 1
    assert len(shot.pixels) == shot.width * shot.height * 4


def test_capture_element_uses_element_bounds(app):
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

    shot = _capture_or_skip(lambda: xa11y.screenshot(element=el))

    expected_w = round(bounds.width * shot.scale)
    expected_h = round(bounds.height * shot.scale)
    assert abs(shot.width - expected_w) <= 1
    assert abs(shot.height - expected_h) <= 1


def test_save_png_writes_valid_file(app, tmp_path):
    shot = _capture_or_skip(lambda: xa11y.screenshot(region=(0, 0, 20, 20)))

    out = tmp_path / "shot.png"
    shot.save_png(out)
    data = out.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"


def test_passing_both_element_and_region_raises(app):
    el = app.locator("button").first().element()
    with pytest.raises(ValueError, match="element.*region"):
        xa11y.screenshot(element=el, region=(0, 0, 10, 10))
