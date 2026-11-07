"""Text events: ``Event.text_change`` and ``EventType.TEXT_SELECTION_CHANGED``.

Driven by a scripted subscription that yields real core events
(``xa11y::mock::text_edit_events``), so the conversion under test is the one a
live subscription uses.
"""

import pytest
import xa11y
from xa11y._native import _make_test_text_event_subscription


def _events():
    with _make_test_text_event_subscription() as sub:
        return list(sub)


def test_text_events_cross_with_their_payloads():
    inserted, deleted, bare, caret = _events()

    assert inserted.event_type == xa11y.EventType.TEXT_CHANGED == "text_changed"
    change = inserted.text_change
    assert isinstance(change, xa11y.TextChange)
    assert change.kind == "inserted"
    assert change.range == xa11y.TextRange(3, 7)
    assert change.text == "hé😀"
    assert "inserted" in repr(change)

    assert deleted.text_change.kind == "deleted"
    assert deleted.text_change.range is None
    assert deleted.text_change.text == "x"

    # A TextChanged whose platform supplies no delta.
    assert bare.event_type == "text_changed"
    assert bare.text_change is None

    assert caret.event_type == xa11y.EventType.TEXT_SELECTION_CHANGED
    assert caret.event_type == "text_selection_changed"
    assert caret.text_change is None
    # Not the item-selection kind.
    assert caret.event_type != xa11y.EventType.SELECTION_CHANGED


def test_text_change_is_frozen():
    inserted = _events()[0]
    with pytest.raises(AttributeError):
        inserted.text_change.kind = "deleted"
