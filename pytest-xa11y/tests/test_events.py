"""EventRecorder matching, bounded retention, and its failure message."""

from __future__ import annotations

import pytest
import xa11y
from _pytest.outcomes import Failed

from pytest_xa11y import EventRecorder


class FakeElement:
    def __init__(self, role="button", name=None):
        self.role = role
        self.name = name


class FakeEvent:
    def __init__(self, event_type, target=None):
        self.event_type = event_type
        self.target = target


class FakeSubscription:
    """Stands in for xa11y.Subscription; no accessibility bus required."""

    def __init__(self, queue=()):
        self.queue = list(queue)
        self.closed = False

    def try_recv(self):
        return self.queue.pop(0) if self.queue else None

    def wait_for(self, predicate, timeout):
        while self.queue:
            event = self.queue.pop(0)
            if predicate(event):
                return event
        raise xa11y.TimeoutError(f"Timeout after {timeout}s; waiting for: event matching predicate")

    def close(self):
        self.closed = True


class FakeApp:
    def __init__(self, queue=()):
        self.subscription = FakeSubscription(queue)

    def subscribe(self):
        return self.subscription


def test_recorder_requires_opening():
    recorder = EventRecorder(FakeApp())
    with pytest.raises(RuntimeError, match="not open"):
        recorder.drain(0.0)


def test_context_manager_closes_the_subscription():
    app = FakeApp()
    with EventRecorder(app):
        pass
    assert app.subscription.closed is True


def test_drain_collects_and_records():
    app = FakeApp([FakeEvent("focus_changed"), FakeEvent("value_changed")])
    with EventRecorder(app) as recorder:
        collected = recorder.drain(0.05)
    assert [event.event_type for event in collected] == ["focus_changed", "value_changed"]
    assert len(recorder.recorded) == 2


def test_expect_matches_on_type():
    app = FakeApp([FakeEvent("value_changed"), FakeEvent("focus_changed")])
    with EventRecorder(app) as recorder:
        event = recorder.expect("focus_changed", timeout=0.1)
    assert event.event_type == "focus_changed"


def test_expect_matches_on_target_name():
    app = FakeApp(
        [
            FakeEvent("focus_changed", FakeElement(name="Cancel")),
            FakeEvent("focus_changed", FakeElement(name="OK")),
        ]
    )
    with EventRecorder(app) as recorder:
        event = recorder.expect("focus_changed", name="OK", timeout=0.1)
    assert event.target.name == "OK"


def test_expect_failure_reports_what_did_arrive():
    app = FakeApp([FakeEvent("value_changed", FakeElement(name="Volume"))])
    with EventRecorder(app) as recorder, pytest.raises(Failed) as excinfo:
        recorder.expect("focus_changed", timeout=0.1)
    message = str(excinfo.value)
    assert "No focus_changed event within 0.1s" in message
    # The point of the recorder: the report names the events that did arrive.
    assert "value_changed" in message
    assert 'name="Volume"' in message


def test_expect_requires_a_filter():
    with EventRecorder(FakeApp()) as recorder, pytest.raises(ValueError, match="at least one"):
        recorder.expect()


def test_seen_filters_without_waiting():
    app = FakeApp([FakeEvent("focus_changed"), FakeEvent("value_changed")])
    with EventRecorder(app) as recorder:
        recorder.drain(0.05)
        assert len(recorder.seen("focus_changed")) == 1
        assert len(recorder.seen(predicate=lambda e: True)) == 2


def test_retention_is_bounded():
    app = FakeApp([FakeEvent("focus_changed") for _ in range(50)])
    with EventRecorder(app, keep=10) as recorder:
        recorder.drain(0.05)
        assert len(recorder.recorded) == 10
        rendered = recorder.render()
    # Rendering says how many it saw, not just how many it kept.
    assert "events recorded (10)" in rendered


def test_render_is_truncated_and_says_so():
    app = FakeApp([FakeEvent("focus_changed") for _ in range(40)])
    with EventRecorder(app) as recorder:
        recorder.drain(0.05)
        rendered = recorder.render(limit=5)
    assert "earlier events omitted" in rendered
    assert rendered.count("focus_changed") == 5


def test_render_with_nothing_recorded():
    with EventRecorder(FakeApp()) as recorder:
        assert "no events recorded" in recorder.render()


def test_expect_accepts_several_types():
    # Platforms disagree about which event a toggle emits, so a cross-platform
    # test needs "either of these" without dropping to a predicate.
    app = FakeApp([FakeEvent("value_changed")])
    with EventRecorder(app) as recorder:
        event = recorder.expect(("state_changed", "value_changed"), timeout=0.1)
    assert event.event_type == "value_changed"


def test_expect_failure_names_every_accepted_type():
    app = FakeApp([FakeEvent("name_changed")])
    with EventRecorder(app) as recorder, pytest.raises(Failed) as excinfo:
        recorder.expect(["state_changed", "value_changed"], timeout=0.1)
    assert "state_changed or value_changed event" in str(excinfo.value)


def test_expect_rejects_an_empty_type_sequence():
    with EventRecorder(FakeApp()) as recorder, pytest.raises(ValueError, match="empty sequence"):
        recorder.expect([])


def test_seen_accepts_several_types():
    app = FakeApp([FakeEvent("focus_changed"), FakeEvent("value_changed")])
    with EventRecorder(app) as recorder:
        recorder.drain(0.05)
        assert len(recorder.seen(("focus_changed", "value_changed"))) == 2
