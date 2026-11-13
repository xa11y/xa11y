"""`type_text` refuses while `key_down` holds a modifier, through the real binding."""

import pytest
import xa11y
from xa11y._native import _make_test_input


def test_type_text_refuses_while_a_modifier_is_held():
    sim = _make_test_input()
    sim.key_down("Shift")
    try:
        with pytest.raises(xa11y.InvalidActionDataError) as caught:
            sim.type_text("hello")
        assert "Shift held with key_down" in str(caught.value)
        assert "key_up" in str(caught.value)
    finally:
        sim.key_up("Shift")
    sim.type_text("hello")


def test_held_non_modifier_and_chord_do_not_block_type_text():
    sim = _make_test_input()
    sim.chord("a", ["Ctrl"])
    sim.key_down("a")
    try:
        sim.type_text("hello")
    finally:
        sim.key_up("a")
