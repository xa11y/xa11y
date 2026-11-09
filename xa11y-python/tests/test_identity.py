"""Element identity and staleness (#481), against the mock provider.

An element is the reference to one node: an action reaches that node or
fails with ElementStaleError, never re-matching a similarly named one.
"""

import pytest
import xa11y
from xa11y import _native


def test_two_finds_of_one_node_are_same_as_but_not_equal():
    back, back_again = _native._make_test_stale_elements("Back", "none")
    forward, _ = _native._make_test_stale_elements("Forward", "none")
    assert back.same_as(back_again)
    back.check_alive()
    # A different node is not the same node, whatever the two share.
    assert not back.same_as(forward)
    # `==` and `hash` are Python object identity: no platform call, no raise.
    assert back == back
    assert back != back_again
    assert len({back, back_again}) == 2
    assert back != "Back"


def test_equality_and_hash_never_raise_on_a_stale_handle():
    back, _ = _native._make_test_stale_elements("Back", "removed")
    with pytest.raises(xa11y.ElementStaleError):
        back.check_alive()
    # A dead handle can still live in a list, set or dict.
    assert back in [back]
    assert {back: 1}[back] == 1
    assert hash(back) == hash(back)


def test_an_action_on_a_removed_node_raises_stale_and_does_nothing():
    back, _ = _native._make_test_stale_elements("Back", "removed")
    with pytest.raises(xa11y.ElementStaleError) as info:
        back.press()
    err = info.value
    assert err.reason == "removed"
    assert "Back" in err.element
    assert err.delivery == "not_delivered"
    assert str(err).startswith("Element stale (removed)")
    # Still a SelectorNotMatchedError, so existing handlers keep working.
    assert isinstance(err, xa11y.SelectorNotMatchedError)
    with pytest.raises(xa11y.ElementStaleError):
        back.check_alive()


def test_a_reused_id_with_another_role_is_identity_changed():
    back, _ = _native._make_test_stale_elements("Back", "replaced")
    for act in (back.press, back.focus, back.check_alive):
        with pytest.raises(xa11y.ElementStaleError) as info:
            act()
        assert info.value.reason == "identity_changed"


def test_change_argument_is_validated():
    with pytest.raises(ValueError):
        _native._make_test_stale_elements("Back", "gone")
