"""Tests for Subscription iteration + disconnect handling."""

import pytest
from xa11y._native import (
    XA11yError,
    _make_disconnected_subscription,
    _make_test_locator,
)


def test_subscription_iter_raises_stop_iteration_on_disconnect():
    """When the event source drops (all senders gone), iterating the
    Subscription must raise StopIteration rather than hanging or silently
    returning None. Regression test for the `.ok()`-masking bug in __next__."""
    sub = _make_disconnected_subscription()
    # The iterator must terminate promptly — no hang waiting for events
    # that will never come.
    events = list(sub)
    assert events == []


def test_subscription_next_raises_stop_iteration_on_disconnect():
    """Direct __next__() call on a disconnected subscription must raise
    StopIteration (not hang, not return None, not raise another error)."""
    sub = _make_disconnected_subscription()
    with pytest.raises(StopIteration):
        next(iter(sub))


def test_subscription_recv_on_disconnected_raises_timeout():
    """`recv()` with a short timeout on an already-disconnected subscription
    should raise the xa11y TimeoutError — the current `recv` semantics treat
    disconnect as "no event within timeout", which is consistent with the
    existing public API (`recv` returns the timeout error on no-event).

    This test pins that behaviour so future changes are deliberate."""
    sub = _make_disconnected_subscription()
    # recv maps both timeout and disconnect to the timeout error via
    # EventReceiver::recv_timeout. That's the existing contract; __next__ is
    # what distinguishes them via recv_status.
    with pytest.raises(XA11yError):
        sub.recv(timeout=0.05)


def test_mock_provider_still_works_after_refactor():
    """Smoke test that the shared xa11y-core::mock provider still produces
    the expected test tree after the python/js mocks were consolidated."""
    locator = _make_test_locator()
    app = locator.element()
    assert app.role == "application"
    assert app.name == "TestApp"
    assert app.stable_id == "app-root"
