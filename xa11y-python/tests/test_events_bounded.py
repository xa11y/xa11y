"""Event announcements, sequence numbers, kinds and the bounded buffer (#480),
against the mock subscription."""

from __future__ import annotations

import threading
import time

import pytest
import xa11y
from xa11y._native import _make_test_app, _make_test_open_subscription


def _drain(sub):
    out = []
    while (event := sub.try_recv()) is not None:
        out.append(event)
    return out


def test_events_are_numbered_and_announcements_carry_their_payload():
    with _make_test_app(False, events=True).subscribe() as sub:
        events = _drain(sub)
        assert [e.sequence for e in events] == list(range(len(events)))
        assert sub.dropped == 0
    times = [e.time for e in events]
    assert times == sorted(times)
    assert all(abs(t - time.time()) < 60 for t in times)
    kinds = [e.event_type for e in events]
    assert kinds == ["focus_changed", "announcement", "announcement", "announcement"]
    assert events[0].announcement is None
    request, region, alert = (e.announcement for e in events[1:])
    assert isinstance(request, xa11y.AnnouncementInfo)
    assert (request.origin, request.text, request.politeness) == ("request", "Saved", "polite")
    assert request.platform_priority == "MostRecent"
    assert request.activity_id == "save"
    assert (region.origin, region.text) == ("live_region", None)
    assert (alert.origin, alert.politeness) == ("alert", "assertive")


def test_kinds_keep_only_what_was_asked_for():
    with _make_test_app(False, events=True).subscribe(kinds=[xa11y.EventType.ANNOUNCEMENT]) as sub:
        events = _drain(sub)
    assert [e.event_type for e in events] == ["announcement"] * 3
    # Numbered before filtering: the purged focus event left a gap.
    assert [e.sequence for e in events] == [1, 2, 3]


def test_a_small_buffer_drops_the_oldest_and_counts_them():
    with _make_test_app(False, events=True).subscribe(buffer=2) as sub:
        events = _drain(sub)
        assert sub.dropped == 2
    assert [e.sequence for e in events] == [2, 3]


@pytest.mark.parametrize(
    "kwargs, exc",
    [
        ({"kinds": ["focus"]}, xa11y.InvalidActionDataError),
        ({"buffer": 0}, xa11y.InvalidActionDataError),
    ],
)
def test_bad_subscribe_options_are_refused(kwargs, exc):
    with pytest.raises(exc):
        _make_test_app(False, events=True).subscribe(**kwargs)


def test_dropped_does_not_wait_for_a_blocked_recv():
    sub = _make_test_open_subscription()
    started = threading.Event()
    outcome = []

    def wait():
        started.set()
        began = time.monotonic()
        with pytest.raises(xa11y.TimeoutError):
            sub.recv(timeout=1.5)
        outcome.append(time.monotonic() - began)

    waiter = threading.Thread(target=wait)
    waiter.start()
    started.wait()
    time.sleep(0.2)
    # The waiter holds the subscription for its whole 1.5 s timeout; the
    # count must not wait for it.
    begin = time.monotonic()
    assert sub.dropped == 0
    assert time.monotonic() - begin < 0.5
    waiter.join()
    # The recv really did block, so the read above overlapped it.
    assert outcome and outcome[0] >= 1.0
    sub.close()


def test_dropped_keeps_its_final_count_after_close():
    sub = _make_test_app(False, events=True).subscribe(buffer=2)
    sub.close()
    assert sub.dropped == 2
    with sub:
        pass
    assert sub.dropped == 2
