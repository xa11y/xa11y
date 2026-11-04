"""Input geometry through the real binding, without a desktop or input events."""

import pytest
import xa11y
from xa11y._native import _make_test_app, _make_test_input


def test_literal_target_has_plain_structured_geometry():
    with pytest.raises(xa11y.SelectorNotMatchedError) as caught:
        _make_test_input().click((500, 500))
    error = caught.value
    assert error.delivery == "not_delivered"
    geometry = error.pointer_target
    assert geometry["point"] == {"x": 500, "y": 500}
    assert geometry["checked_point"] == geometry["point"]
    assert geometry["element_bounds"] is None
    assert geometry["anchor"] is None
    assert geometry["coordinate_space"] == "mock desktop pixels"
    assert geometry["displays"][0] == {"x": 0, "y": 0, "width": 100, "height": 100}
    assert geometry["omitted_displays"] == 0


def test_element_anchor_survives_lowering_and_error_projection():
    element = _make_test_app().locator('button[name="Back"]').element()
    bounds = element.bounds
    with pytest.raises(xa11y.SelectorNotMatchedError) as caught:
        _make_test_input().click(element, anchor=(3, 4), held=["Shift"])
    geometry = caught.value.pointer_target
    assert geometry["element_bounds"] == {
        "x": bounds.x,
        "y": bounds.y,
        "width": bounds.width,
        "height": bounds.height,
    }
    assert geometry["point"] == {"x": bounds.x + 3, "y": bounds.y + 4}
    assert geometry["anchor"] == {"offset": {"dx": 3, "dy": 4}}
    assert caught.value.delivery == "not_delivered"


def test_drag_reports_the_invalid_end_element_not_the_raw_start():
    element = _make_test_app().locator('button[name="Back"]').element()
    with pytest.raises(xa11y.SelectorNotMatchedError) as caught:
        _make_test_input().drag((5, 5), element, held=["Shift"])
    geometry = caught.value.pointer_target
    assert geometry["element_bounds"]["x"] == element.bounds.x
    assert geometry["anchor"] == "center"
    assert geometry["point"]["x"] == element.bounds.x + element.bounds.width // 2


@pytest.mark.parametrize("method", ["double_click", "right_click", "move_to", "scroll"])
def test_all_point_paths_refuse_an_off_display_target(method):
    with pytest.raises(xa11y.SelectorNotMatchedError) as caught:
        getattr(_make_test_input(), method)((500, 500))
    assert caught.value.pointer_target["point"] == {"x": 500, "y": 500}


def test_captured_move_and_release_remain_possible():
    simulator = _make_test_input()
    simulator.mouse_down()
    try:
        simulator.move_to((500, 500))
        with pytest.raises(xa11y.SelectorNotMatchedError):
            simulator.click((500, 500))
    finally:
        simulator.mouse_up()
    with pytest.raises(xa11y.SelectorNotMatchedError):
        simulator.move_to((500, 500))


def test_zero_click_count_is_a_noop_and_negative_monitor_is_valid():
    simulator = _make_test_input()
    simulator.click((500, 500), count=0, held=["Shift"])
    simulator.click((-50, -50))
