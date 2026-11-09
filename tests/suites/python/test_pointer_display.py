"""Native display rejection must happen before any pointer or modifier input."""

import ctypes
import os
import sys
from ctypes import wintypes

import pytest

import xa11y

pytestmark = pytest.mark.skipif(
    os.environ.get("XA11Y_TEST_APP") not in ("tauri", None),
    reason="Native pointer geometry is exercised once per platform with Tauri",
)


def _windows_cursor():
    if sys.platform != "win32":
        return None
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.GetPhysicalCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
    user32.GetPhysicalCursorPos.restype = wintypes.BOOL
    point = wintypes.POINT()
    if not user32.GetPhysicalCursorPos(ctypes.byref(point)):
        raise ctypes.WinError(ctypes.get_last_error())
    return point.x, point.y


@pytest.mark.parametrize(
    "method", ["click", "double_click", "right_click", "move_to", "scroll"]
)
def test_native_off_display_point_is_not_delivered(method):
    # Outside ordinary desktop extents, while representable in every public
    # point type. No input permission is required: rejection precedes dispatch.
    target = (1_000_000, 1_000_000)
    before = _windows_cursor()
    with pytest.raises(xa11y.InvalidActionDataError) as caught:
        simulator = xa11y.input_sim()
        if method == "click":
            simulator.click(target, held=["Shift"])
        else:
            getattr(simulator, method)(target)
    geometry = caught.value.pointer_target
    assert caught.value.delivery == "not_delivered"
    assert geometry["point"] == {"x": target[0], "y": target[1]}
    assert geometry["coordinate_space"]
    assert geometry["displays"]
    checked = geometry["checked_point"]
    for region in geometry["displays"]:
        assert not (
            region["x"] <= checked["x"] < region["x"] + region["width"]
            and region["y"] <= checked["y"] < region["y"] + region["height"]
        )
    assert _windows_cursor() == before


@pytest.mark.skipif(
    sys.platform != "win32", reason="Native Windows Element anchor regression"
)
def test_native_off_display_element_anchor_retains_context(app):
    element = app.locator('button[name="OK"]').element()
    bounds = element.bounds
    assert bounds is not None
    before = _windows_cursor()
    with pytest.raises(xa11y.InvalidActionDataError) as caught:
        xa11y.input_sim().click(element, anchor=(1_000_000, 1_000_000), held=["Shift"])
    geometry = caught.value.pointer_target
    assert caught.value.delivery == "not_delivered"
    assert geometry["point"] == {"x": bounds.x + 1_000_000, "y": bounds.y + 1_000_000}
    assert geometry["element_bounds"] == {
        "x": bounds.x,
        "y": bounds.y,
        "width": bounds.width,
        "height": bounds.height,
    }
    assert geometry["anchor"] == {"offset": {"dx": 1_000_000, "dy": 1_000_000}}
    assert _windows_cursor() == before
