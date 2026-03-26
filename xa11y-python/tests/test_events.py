"""Tests for event provider Python bindings."""

import xa11y  # noqa: I001


# ── Event types are importable ──────────────────────────────────────────────


def test_event_class_exists():
    assert xa11y.Event is not None


def test_text_change_data_class_exists():
    assert xa11y.TextChangeData is not None


def test_subscription_class_exists():
    assert xa11y.Subscription is not None


# ── Functions are importable ────────────────────────────────────────────────


def test_subscribe_function_exists():
    assert callable(xa11y.subscribe)


def test_wait_for_event_function_exists():
    assert callable(xa11y.wait_for_event)


def test_wait_for_function_exists():
    assert callable(xa11y.wait_for)


# ── __all__ includes new exports ────────────────────────────────────────────


def test_all_includes_event():
    assert "Event" in xa11y.__all__


def test_all_includes_text_change_data():
    assert "TextChangeData" in xa11y.__all__


def test_all_includes_subscription():
    assert "Subscription" in xa11y.__all__


def test_all_includes_subscribe():
    assert "subscribe" in xa11y.__all__


def test_all_includes_wait_for_event():
    assert "wait_for_event" in xa11y.__all__


def test_all_includes_wait_for():
    assert "wait_for" in xa11y.__all__
