"""Recording accessibility events across a block of test code."""

from __future__ import annotations

import time
from collections import deque
from collections.abc import Sequence
from typing import Callable, Union

import pytest
import xa11y

# Events retained for the failure report. Bounded: a structure-heavy app can
# emit thousands, and only the tail is diagnostically useful.
_KEEP = 200

_IDLE_POLL = 0.05


# One event type, or several. Platforms genuinely disagree about which
# event a given interaction emits — toggling a checkbox is StateChanged on
# one bridge and ValueChanged on another — so a test that must run everywhere
# needs to accept either without dropping to a hand-written predicate.
EventTypes = Union[str, Sequence[str]]


class EventRecorder:
    """A subscription plus the events it has seen.

    Obtained from the ``xa11y_events`` fixture::

        def test_focus_moves(xa11y_app, xa11y_events):
            with xa11y_events(xa11y_app) as events:
                xa11y_app.locator('button[name="OK"]').focus()
                events.expect("focus_changed", timeout=2.0)

    Whatever the recorder has seen is attached to the failure report, so a
    test that expected an event it never got shows what did arrive instead.
    """

    def __init__(self, app: xa11y.App, *, keep: int = _KEEP) -> None:
        self._app = app
        self._subscription: xa11y.Subscription | None = None
        self._seen: deque[xa11y.Event] = deque(maxlen=keep)

    # -- lifecycle ---------------------------------------------------------

    def open(self) -> EventRecorder:
        if self._subscription is None:
            self._subscription = self._app.subscribe()
        return self

    def close(self) -> None:
        if self._subscription is not None:
            self._subscription.close()
            self._subscription = None

    def __enter__(self) -> EventRecorder:
        return self.open()

    def __exit__(self, exc_type, exc_val, exc_tb) -> bool:
        self.close()
        return False

    @property
    def subscription(self) -> xa11y.Subscription:
        if self._subscription is None:
            raise RuntimeError(
                "EventRecorder is not open — use it as a context manager, or call open()."
            )
        return self._subscription

    # -- collection --------------------------------------------------------

    def drain(self, duration: float = 0.3) -> list[xa11y.Event]:
        """Collect every event delivered over the next ``duration`` seconds."""
        subscription = self.subscription
        collected: list[xa11y.Event] = []
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            event = subscription.try_recv()
            if event is None:
                time.sleep(_IDLE_POLL)
                continue
            collected.append(event)
            self._seen.append(event)
        return collected

    def expect(
        self,
        event_type: EventTypes | None = None,
        *,
        name: str | None = None,
        predicate: Callable[[xa11y.Event], bool] | None = None,
        timeout: float = 5.0,
    ) -> xa11y.Event:
        """Wait for a matching event, or fail the test.

        ``event_type`` accepts one type or several; several means "any of
        these". At least one of ``event_type``, ``name`` or ``predicate`` is
        required — "wait for any event at all" is deliberately not spelled
        ``expect()``, because a filter accidentally left off would then pass
        against whatever arrived. Use ``Subscription.recv`` for that.

        Delegates the wait to the subscription so the GIL is released while
        blocking, then reports what *did* arrive when nothing matched.
        """
        matcher = self._matcher(event_type, name, predicate)

        def record_and_match(event: xa11y.Event) -> bool:
            self._seen.append(event)
            return matcher(event)

        try:
            return self.subscription.wait_for(record_and_match, timeout)
        except xa11y.TimeoutError:
            pytest.fail(
                f"No {self._describe(event_type, name, predicate)} within {timeout:.1f}s.\n"
                f"{self.render(indent='  ')}"
            )

    def seen(
        self,
        event_type: EventTypes | None = None,
        *,
        name: str | None = None,
        predicate: Callable[[xa11y.Event], bool] | None = None,
    ) -> list[xa11y.Event]:
        """Already-recorded events matching the filter (does not wait)."""
        matcher = self._matcher(event_type, name, predicate)
        return [event for event in self._seen if matcher(event)]

    @property
    def recorded(self) -> list[xa11y.Event]:
        """Every event this recorder has seen, oldest first (bounded)."""
        return list(self._seen)

    # -- reporting ---------------------------------------------------------

    def render(self, *, indent: str = "", limit: int = 25) -> str:
        """Bounded, human-readable rendering of what was recorded."""
        events = self.recorded
        if not events:
            return f"{indent}(no events recorded)"
        shown = events[-limit:]
        lines = [f"{indent}events recorded ({len(events)}):"]
        if len(events) > len(shown):
            lines.append(f"{indent}  ... {len(events) - len(shown)} earlier events omitted")
        for event in shown:
            target = event.target
            label = f" target={target.role}" if target is not None else ""
            if target is not None and target.name:
                label += f' name="{target.name}"'
            lines.append(f"{indent}  {event.event_type}{label}")
        return "\n".join(lines)

    # -- internals ---------------------------------------------------------

    @staticmethod
    def _types(event_type: EventTypes | None) -> tuple[str, ...] | None:
        if event_type is None:
            return None
        if isinstance(event_type, str):
            return (event_type,)
        types = tuple(event_type)
        if not types:
            raise ValueError("event_type= was an empty sequence; pass at least one type.")
        return types

    @staticmethod
    def _matcher(
        event_type: EventTypes | None,
        name: str | None,
        predicate: Callable[[xa11y.Event], bool] | None,
    ) -> Callable[[xa11y.Event], bool]:
        if event_type is None and name is None and predicate is None:
            raise ValueError("Pass at least one of event_type, name= or predicate=.")
        types = EventRecorder._types(event_type)

        def matches(event: xa11y.Event) -> bool:
            if types is not None and event.event_type not in types:
                return False
            if name is not None:
                target = event.target
                if target is None or target.name != name:
                    return False
            return not (predicate is not None and not predicate(event))

        return matches

    @staticmethod
    def _describe(
        event_type: EventTypes | None,
        name: str | None,
        predicate: Callable[[xa11y.Event], bool] | None,
    ) -> str:
        types = EventRecorder._types(event_type)
        parts = []
        if types:
            parts.append(f"{' or '.join(types)} event")
        else:
            parts.append("event")
        if name is not None:
            parts.append(f'targeting "{name}"')
        if predicate is not None:
            parts.append("matching predicate")
        return " ".join(parts)
