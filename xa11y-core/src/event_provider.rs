use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::error::{Diagnosis, Error, Result};
use crate::event::Event;

/// The buffer every subscription starts with: 4,096 events.
pub const DEFAULT_EVENT_BUFFER: usize = 4096;

/// A live event subscription. Drop to unsubscribe.
///
/// `Subscription` is `Send` but not `Clone`. It can be moved to another
/// thread but not shared.
///
/// Events wait in a bounded buffer ([`DEFAULT_EVENT_BUFFER`] unless
/// [`SubscribeOptions::buffer`] says otherwise). When it is full the oldest
/// event is dropped, counted in [`dropped`](Self::dropped), and its
/// [`sequence`](crate::Event::sequence) number left as a gap.
pub struct Subscription {
    rx: EventReceiver,
    _cancel: CancelHandle,
}

impl Subscription {
    /// Create a new subscription from its components.
    pub fn new(rx: EventReceiver, cancel: CancelHandle) -> Self {
        Self {
            rx,
            _cancel: cancel,
        }
    }

    /// How many events the buffer has dropped, in total, because it was
    /// full. Always 0 for a subscription not built on [`event_channel`].
    pub fn dropped(&self) -> u64 {
        match &self.rx.source {
            Source::Ring(ring) => ring.lock().dropped,
            Source::Channel(_) => 0,
        }
    }

    /// Keep only `kinds` (by [`EventKind::to_snake_case`](crate::EventKind::to_snake_case))
    /// and bound the buffer to `buffer` events. Events of other kinds
    /// already buffered are discarded.
    fn restrict(&self, kinds: Option<Vec<&'static str>>, buffer: usize) {
        match &self.rx.source {
            Source::Ring(ring) => {
                let mut state = ring.lock();
                if let Some(kinds) = &kinds {
                    state
                        .queue
                        .retain(|e| kinds.contains(&e.kind.to_snake_case()));
                }
                state.kinds = kinds;
                state.capacity = buffer;
                while state.queue.len() > buffer {
                    state.queue.pop_front();
                    state.dropped += 1;
                }
            }
            // A plain channel cannot be bounded after the fact; its kinds
            // are filtered as they are received.
            Source::Channel(_) => {
                *self
                    .rx
                    .channel_kinds
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = kinds;
            }
        }
    }

    /// Try to receive without blocking (returns `None` if no event ready).
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv()
    }

    /// Block until an event arrives or the timeout expires.
    pub fn recv(&self, timeout: Duration) -> Result<Event> {
        self.rx.recv_timeout(timeout).ok_or_else(|| {
            Error::timeout(timeout).diagnose(Diagnosis {
                condition: Some("any accessibility event".to_string()),
                delivery: Some(crate::error::Delivery::NotDelivered),
                ..Diagnosis::default()
            })
        })
    }

    /// Block until an event arrives, the timeout expires, or the event source
    /// disconnects. Unlike [`recv`](Self::recv), this preserves the distinction
    /// between "no event yet" and "no events will ever arrive" so bindings can
    /// terminate their poll loops cleanly when the subscription shuts down.
    pub fn recv_status(&self, timeout: Duration) -> RecvStatus {
        self.rx.recv_timeout_status(timeout)
    }

    /// Block until an event matching `predicate` arrives or the timeout expires.
    ///
    /// If the event stream disconnects (all senders dropped) before a match
    /// arrives, `wait_for` short-circuits with `Error::Timeout` carrying the
    /// actual elapsed time — no further matching event can ever arrive, so
    /// burning the remainder of the timeout in a dead poll loop is pointless.
    pub fn wait_for(&self, predicate: impl Fn(&Event) -> bool, timeout: Duration) -> Result<Event> {
        let start = std::time::Instant::now();
        let mut seen: usize = 0;
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(Error::timeout(start.elapsed()).diagnose(Diagnosis {
                    condition: Some("event matching predicate".to_string()),
                    last_observed: Some(format!("{seen} event(s) received, none matched")),
                    delivery: Some(crate::error::Delivery::NotDelivered),
                    ..Diagnosis::default()
                }));
            }
            // Poll with short recv timeouts so we can re-check the deadline,
            // and break out early on disconnect instead of polling a dead
            // channel until the outer timeout fires.
            let poll = remaining.min(Duration::from_millis(10));
            match self.rx.recv_timeout_status(poll) {
                RecvStatus::Event(event) => {
                    if predicate(&event) {
                        return Ok(*event);
                    }
                    seen += 1;
                }
                RecvStatus::Timeout => continue,
                RecvStatus::Disconnected => {
                    return Err(Error::timeout(start.elapsed()).diagnose(Diagnosis {
                        condition: Some("event matching predicate".to_string()),
                        last_observed: Some(format!(
                            "event source disconnected after {seen} non-matching event(s) — \
                             no further events will arrive"
                        )),
                        delivery: Some(crate::error::Delivery::NotDelivered),
                        ..Diagnosis::default()
                    }));
                }
            }
        }
    }

    /// Return a blocking iterator over incoming events.
    ///
    /// The iterator yields events until the subscription is dropped or the
    /// event source disconnects (all senders dropped).
    pub fn iter(&self) -> SubscriptionIter<'_> {
        SubscriptionIter { sub: self }
    }
}

/// Status returned by [`EventReceiver::recv_timeout_status`].
///
/// Unlike the collapsed `Option<Event>` returned by
/// [`EventReceiver::recv_timeout`], this distinguishes a timeout (keep
/// polling) from a disconnect (senders gone — the stream is finished).
///
/// `Event` is boxed to keep the enum small — `Event` carries an
/// `ElementData` snapshot and is ~360 bytes, while the other two variants
/// are empty.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a bounded receive on a channel either yields a \
              value, runs out of time, or finds the senders gone. This \
              mirrors std's RecvTimeoutError, which has the same three \
              outcomes and is likewise complete."
)]
pub enum RecvStatus {
    /// An event was received.
    Event(Box<Event>),
    /// The timeout elapsed with no event available. The channel is still live.
    Timeout,
    /// All senders have been dropped. No more events will ever arrive.
    Disconnected,
}

/// Blocking iterator over events from a [`Subscription`].
///
/// Yields events until the subscription is dropped or the event source
/// disconnects. In the disconnect case (all senders dropped) the iterator
/// returns `None` rather than spinning forever.
pub struct SubscriptionIter<'a> {
    sub: &'a Subscription,
}

impl<'a> Iterator for SubscriptionIter<'a> {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        // Block in short intervals so the iterator stays responsive to drop,
        // but terminate cleanly if the underlying channel disconnects.
        loop {
            match self.sub.rx.recv_timeout_status(Duration::from_millis(100)) {
                RecvStatus::Event(event) => return Some(*event),
                RecvStatus::Timeout => continue,
                RecvStatus::Disconnected => return None,
            }
        }
    }
}

/// Platform-agnostic event receiver.
pub struct EventReceiver {
    source: Source,
    /// The kinds filter of a [`Source::Channel`], applied on receive.
    channel_kinds: Mutex<Option<Vec<&'static str>>>,
}

enum Source {
    /// The bounded buffer of [`event_channel`].
    Ring(Arc<Ring>),
    /// A caller's own unbounded channel (tests, adapters).
    Channel(std::sync::mpsc::Receiver<Event>),
}

impl EventReceiver {
    /// Wrap a standard channel. It is unbounded and assigns no sequence
    /// numbers; backends use [`event_channel`] instead.
    pub fn new(rx: std::sync::mpsc::Receiver<Event>) -> Self {
        Self {
            source: Source::Channel(rx),
            channel_kinds: Mutex::new(None),
        }
    }

    /// Try to receive without blocking.
    pub fn try_recv(&self) -> Option<Event> {
        match self.recv_timeout_status(Duration::ZERO) {
            RecvStatus::Event(event) => Some(*event),
            RecvStatus::Timeout | RecvStatus::Disconnected => None,
        }
    }

    /// Receive with timeout. Returns `None` for both timeout and disconnect —
    /// use [`recv_timeout_status`](Self::recv_timeout_status) when the caller
    /// needs to distinguish "no event yet" from "stream finished".
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        match self.recv_timeout_status(timeout) {
            RecvStatus::Event(event) => Some(*event),
            RecvStatus::Timeout | RecvStatus::Disconnected => None,
        }
    }

    /// Receive with timeout, preserving the distinction between timeout and
    /// sender disconnect. Used by [`SubscriptionIter`] to terminate cleanly
    /// when the event source goes away, rather than spinning forever.
    pub fn recv_timeout_status(&self, timeout: Duration) -> RecvStatus {
        match &self.source {
            Source::Ring(ring) => ring.recv(timeout),
            Source::Channel(rx) => {
                use std::sync::mpsc::RecvTimeoutError;
                // A timeout too large for an `Instant` (`Duration::MAX`)
                // means no deadline, as in `Deadline::after`.
                let deadline = std::time::Instant::now().checked_add(timeout);
                loop {
                    let received = match deadline {
                        Some(d) => {
                            rx.recv_timeout(d.saturating_duration_since(std::time::Instant::now()))
                        }
                        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    };
                    let event = match received {
                        Ok(event) => event,
                        Err(RecvTimeoutError::Timeout) => return RecvStatus::Timeout,
                        Err(RecvTimeoutError::Disconnected) => return RecvStatus::Disconnected,
                    };
                    let kinds = self.channel_kinds.lock().unwrap_or_else(|e| e.into_inner());
                    if kinds
                        .as_ref()
                        .is_none_or(|k| k.contains(&event.kind.to_snake_case()))
                    {
                        return RecvStatus::Event(Box::new(event));
                    }
                }
            }
        }
    }
}

/// Create a bounded event channel: a sender for the backend, a receiver for
/// the [`Subscription`]. The buffer holds [`DEFAULT_EVENT_BUFFER`] events and
/// drops the oldest when full; each queued event gets the next
/// [`sequence`](crate::Event::sequence) number.
pub fn event_channel() -> (EventSender, EventReceiver) {
    let ring = Arc::new(Ring {
        state: Mutex::new(RingState {
            queue: VecDeque::new(),
            capacity: DEFAULT_EVENT_BUFFER,
            dropped: 0,
            next_sequence: 0,
            senders: 1,
            receiver_alive: true,
            kinds: None,
        }),
        ready: Condvar::new(),
    });
    (
        EventSender {
            ring: Arc::clone(&ring),
        },
        EventReceiver {
            source: Source::Ring(ring),
            channel_kinds: Mutex::new(None),
        },
    )
}

/// The backend's half of an [`event_channel`]. Clone it for each source;
/// the stream disconnects when every clone is dropped.
pub struct EventSender {
    ring: Arc<Ring>,
}

/// Returned by [`EventSender::send`] once the [`Subscription`] is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventSendError;

impl std::fmt::Display for EventSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the subscription was dropped")
    }
}

impl std::error::Error for EventSendError {}

impl EventSender {
    /// Queue `event`, dropping the oldest when the buffer is full. An event
    /// of a kind the subscription did not ask for is discarded and not
    /// counted. Fails only once the subscription is gone, so a backend can
    /// stop listening.
    pub fn send(&self, mut event: Event) -> std::result::Result<(), EventSendError> {
        let mut state = self.ring.lock();
        if !state.receiver_alive {
            return Err(EventSendError);
        }
        if let Some(kinds) = &state.kinds {
            if !kinds.contains(&event.kind.to_snake_case()) {
                return Ok(());
            }
        }
        event.sequence = state.next_sequence;
        state.next_sequence += 1;
        state.queue.push_back(event);
        while state.queue.len() > state.capacity {
            state.queue.pop_front();
            state.dropped += 1;
        }
        drop(state);
        self.ring.ready.notify_one();
        Ok(())
    }
}

impl Clone for EventSender {
    fn clone(&self) -> Self {
        self.ring.lock().senders += 1;
        Self {
            ring: Arc::clone(&self.ring),
        }
    }
}

impl Drop for EventSender {
    fn drop(&mut self) {
        self.ring.lock().senders -= 1;
        self.ring.ready.notify_all();
    }
}

impl std::fmt::Debug for EventSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSender").finish_non_exhaustive()
    }
}

struct Ring {
    state: Mutex<RingState>,
    ready: Condvar,
}

struct RingState {
    queue: VecDeque<Event>,
    capacity: usize,
    dropped: u64,
    next_sequence: u64,
    senders: usize,
    receiver_alive: bool,
    kinds: Option<Vec<&'static str>>,
}

impl Ring {
    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        // The state is counters and a queue; a panic mid-update leaves it
        // usable, so poisoning is recovered rather than propagated.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recv(&self, timeout: Duration) -> RecvStatus {
        // A timeout too large for an `Instant` (`Duration::MAX`) means no
        // deadline, as in `Deadline::after`.
        let deadline = std::time::Instant::now().checked_add(timeout);
        let mut state = self.lock();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return RecvStatus::Event(Box::new(event));
            }
            if state.senders == 0 {
                return RecvStatus::Disconnected;
            }
            state = match deadline {
                Some(d) => {
                    let left = d.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return RecvStatus::Timeout;
                    }
                    self.ready
                        .wait_timeout(state, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => self.ready.wait(state).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        if let Source::Ring(ring) = &self.source {
            let mut state = ring.lock();
            state.receiver_alive = false;
            state.queue.clear();
        }
    }
}

/// Options for [`App::subscribe_with`](crate::App::subscribe_with).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubscribeOptions {
    /// Event kinds to deliver, by [`EventKind::to_snake_case`](crate::EventKind::to_snake_case).
    /// Empty means every kind the backend reports.
    pub kinds: Vec<String>,
    /// Buffer size; `None` is [`DEFAULT_EVENT_BUFFER`].
    pub buffer: Option<usize>,
}

impl SubscribeOptions {
    /// Every kind, the default buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver only these kinds (`"announcement"`, `"focus_changed"`, ...).
    pub fn kinds<S: Into<String>>(mut self, kinds: impl IntoIterator<Item = S>) -> Self {
        self.kinds = kinds.into_iter().map(Into::into).collect();
        self
    }

    /// Hold at most `events` events.
    pub fn buffer(mut self, events: usize) -> Self {
        self.buffer = Some(events);
        self
    }
}

/// Every event kind's name, in declaration order, for surfaces that
/// validate a name before subscribing. Not public API.
#[doc(hidden)]
pub fn event_kind_names() -> &'static [&'static str] {
    KIND_NAMES
}

/// Every event kind's name, in declaration order: [`EventKind::to_snake_case`]
/// over [`crate::EventKind::EVERY`], so the two cannot spell a kind differently.
pub(crate) const KIND_NAMES: &[&str] = &{
    let mut names = [""; crate::EventKind::EVERY.len()];
    let mut i = 0;
    while i < names.len() {
        names[i] = crate::EventKind::EVERY[i].to_snake_case();
        i += 1;
    }
    names
};

/// Subscribe `element` (an application) through `provider` with `options`.
///
/// A kind with no name fails [`Error::InvalidActionData`] listing the names;
/// a kind the capability table marks unsupported on this platform fails
/// [`Error::Unsupported`] with the table's reason, since its events would
/// never arrive. Both are checked before the platform is asked to subscribe.
#[doc(hidden)]
pub fn subscribe_with(
    provider: &dyn crate::Provider,
    element: &crate::ElementData,
    options: &SubscribeOptions,
) -> Result<Subscription> {
    let buffer = options.buffer.unwrap_or(DEFAULT_EVENT_BUFFER);
    if buffer == 0 {
        return Err(Error::invalid_action_data(
            "buffer must hold at least 1 event",
        ));
    }
    let kinds = if options.kinds.is_empty() {
        None
    } else {
        let caps = provider.capabilities()?;
        let mut names = Vec::with_capacity(options.kinds.len());
        for kind in &options.kinds {
            let Some(name) = KIND_NAMES.iter().find(|n| **n == kind.as_str()) else {
                return Err(Error::unknown_name(
                    "event kind",
                    kind,
                    KIND_NAMES.iter().copied(),
                ));
            };
            let feature = format!("event:{name}");
            if caps.support(&feature)? == crate::Support::Unsupported {
                return Err(Error::Unsupported {
                    feature: format!(
                        "{name} events on {}: {}",
                        caps.platform,
                        caps.note(&feature)?
                            .unwrap_or("this backend never sends them")
                    ),
                });
            }
            names.push(*name);
        }
        Some(names)
    };
    let sub = provider.subscribe_kinds(element, kinds.as_deref())?;
    sub.restrict(kinds, buffer);
    Ok(sub)
}

/// Handle to cancel a subscription. Dropping this stops event delivery.
pub struct CancelHandle {
    cancel_fn: Option<Box<dyn FnOnce() + Send>>,
}

impl CancelHandle {
    /// Create a cancel handle with a cancellation callback.
    pub fn new(cancel_fn: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cancel_fn: Some(Box::new(cancel_fn)),
        }
    }

    /// Create a no-op cancel handle.
    pub fn noop() -> Self {
        Self { cancel_fn: None }
    }
}

impl Drop for CancelHandle {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel_fn.take() {
            cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind};
    use std::sync::mpsc;

    fn make_event() -> Event {
        Event {
            kind: EventKind::FocusChanged,
            target: None,
            app_name: "test".into(),
            app_pid: 0,
            timestamp: std::time::Instant::now(),
            text_change: None,
            announcement: None,
            sequence: 0,
        }
    }

    fn named(kind: EventKind) -> Event {
        Event::new(kind, "test", 0)
    }

    #[test]
    fn an_unrepresentable_timeout_waits_without_a_deadline() {
        // `Instant::now() + Duration::MAX` overflows; both sources must
        // treat it as "no deadline" rather than panic.
        let (tx, rx) = event_channel();
        let sub = Subscription::new(rx, CancelHandle::noop());
        tx.send(make_event()).expect("subscribed");
        assert!(sub.recv(Duration::MAX).is_ok());
        let waiter = std::thread::spawn(move || sub.recv_status(Duration::MAX));
        std::thread::sleep(Duration::from_millis(20));
        drop(tx);
        assert!(matches!(
            waiter.join().expect("no panic"),
            RecvStatus::Disconnected
        ));

        let (tx, rx) = mpsc::channel::<Event>();
        let rx = EventReceiver::new(rx);
        tx.send(make_event()).expect("open");
        assert!(rx.recv_timeout(Duration::MAX).is_some());
        drop(tx);
        assert!(matches!(
            rx.recv_timeout_status(Duration::MAX),
            RecvStatus::Disconnected
        ));
    }

    #[test]
    fn the_ring_numbers_events_and_drops_the_oldest_when_full() {
        let (tx, rx) = event_channel();
        let sub = Subscription::new(rx, CancelHandle::noop());
        sub.restrict(None, 3);
        for _ in 0..5 {
            tx.send(make_event()).expect("subscribed");
        }
        assert_eq!(sub.dropped(), 2);
        let seqs: Vec<u64> = std::iter::from_fn(|| sub.try_recv())
            .map(|e| e.sequence)
            .collect();
        assert_eq!(seqs, [2, 3, 4], "the oldest went, and the gap shows it");
    }

    #[test]
    fn the_ring_filters_kinds_without_counting_them_as_dropped() {
        let (tx, rx) = event_channel();
        tx.send(named(EventKind::FocusChanged)).expect("subscribed");
        let sub = Subscription::new(rx, CancelHandle::noop());
        sub.restrict(Some(vec!["announcement"]), DEFAULT_EVENT_BUFFER);
        tx.send(named(EventKind::Announcement)).expect("subscribed");
        tx.send(named(EventKind::ValueChanged)).expect("subscribed");
        let got: Vec<&str> = std::iter::from_fn(|| sub.try_recv())
            .map(|e| e.kind.to_snake_case())
            .collect();
        assert_eq!(
            got,
            ["announcement"],
            "the earlier focus event was purged too"
        );
        assert_eq!(sub.dropped(), 0);
    }

    #[test]
    fn the_ring_disconnects_when_every_sender_is_gone_and_refuses_after_drop() {
        let (tx, rx) = event_channel();
        let tx2 = tx.clone();
        let sub = Subscription::new(rx, CancelHandle::noop());
        drop(tx);
        assert!(matches!(
            sub.recv_status(Duration::from_millis(5)),
            RecvStatus::Timeout
        ));
        tx2.send(make_event()).expect("still subscribed");
        assert!(matches!(
            sub.recv_status(Duration::from_millis(5)),
            RecvStatus::Event(_)
        ));
        let (tx3, rx3) = event_channel();
        drop(tx2);
        assert!(matches!(
            sub.recv_status(Duration::from_millis(5)),
            RecvStatus::Disconnected
        ));
        drop(rx3);
        assert_eq!(tx3.send(make_event()), Err(EventSendError));
    }

    #[test]
    fn a_blocked_receive_wakes_on_send() {
        let (tx, rx) = event_channel();
        let sub = Subscription::new(rx, CancelHandle::noop());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(make_event()).expect("subscribed");
        });
        assert!(matches!(
            sub.recv_status(Duration::from_secs(2)),
            RecvStatus::Event(_)
        ));
    }

    #[test]
    fn subscribe_with_refuses_unknown_and_undeliverable_kinds() {
        let provider = crate::mock::build_provider();
        provider.enable_events();
        let app = crate::ElementData::for_role(crate::Role::Application);
        let err = subscribe_with(&*provider, &app, &SubscribeOptions::new().kinds(["focus"]))
            .err()
            .expect("an unknown name is refused");
        assert!(matches!(err, Error::InvalidActionData { .. }), "{err}");
        assert!(err.to_string().contains("focus_changed"), "{err}");
        let err = subscribe_with(&*provider, &app, &SubscribeOptions::new().buffer(0))
            .err()
            .expect("a zero buffer is refused");
        assert!(matches!(err, Error::InvalidActionData { .. }), "{err}");
        // A kind the running platform's table marks unsupported.
        let caps = crate::Capabilities::current().expect("a table");
        if let Some(cap) = caps
            .entries()
            .iter()
            .find(|c| c.name.starts_with("event:") && c.support == crate::Support::Unsupported)
        {
            let name = cap.name.trim_start_matches("event:");
            if KIND_NAMES.contains(&name) {
                let err = subscribe_with(&*provider, &app, &SubscribeOptions::new().kinds([name]))
                    .err()
                    .expect("an undeliverable kind is refused");
                assert!(matches!(err, Error::Unsupported { .. }), "{err}");
            }
        }
    }

    #[test]
    fn recv_timeout_status_distinguishes_timeout_and_disconnect() {
        // Connected but empty → Timeout.
        let (tx, rx) = mpsc::channel::<Event>();
        let receiver = EventReceiver::new(rx);
        match receiver.recv_timeout_status(Duration::from_millis(10)) {
            RecvStatus::Timeout => {}
            RecvStatus::Event(_) => panic!("unexpected event"),
            RecvStatus::Disconnected => panic!("should not be disconnected yet"),
        }

        // Deliver an event → Event.
        tx.send(make_event()).unwrap();
        match receiver.recv_timeout_status(Duration::from_millis(10)) {
            RecvStatus::Event(_) => {}
            RecvStatus::Timeout => panic!("expected Event, got Timeout"),
            RecvStatus::Disconnected => panic!("expected Event, got Disconnected"),
        }

        // Drop the sender → Disconnected.
        drop(tx);
        match receiver.recv_timeout_status(Duration::from_millis(10)) {
            RecvStatus::Disconnected => {}
            RecvStatus::Timeout => panic!("expected Disconnected, got Timeout"),
            RecvStatus::Event(_) => panic!("unexpected event"),
        }
    }

    #[test]
    fn wait_for_short_circuits_on_disconnect() {
        // wait_for used to poll until its outer timeout even after the sender
        // was dropped, because it used recv_timeout (no disconnect signal).
        // It must now return as soon as the channel disconnects.
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx); // sender gone — no event will ever match.

        let sub = Subscription::new(EventReceiver::new(rx), CancelHandle::noop());

        let start = std::time::Instant::now();
        let err = sub
            .wait_for(|_| true, Duration::from_secs(5))
            .expect_err("wait_for must not return an event on a disconnected stream");
        let elapsed = start.elapsed();

        match err {
            Error::Timeout { .. } => {}
            other => panic!("expected Timeout error, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(1),
            "wait_for blocked for {elapsed:?} — should short-circuit on disconnect"
        );
    }

    #[test]
    fn subscription_iter_terminates_on_disconnect() {
        // Regression test for a hang: SubscriptionIter::next used to loop
        // forever because EventReceiver::recv_timeout collapsed timeout and
        // disconnect into the same None. Now that recv_timeout_status
        // distinguishes them, a dropped sender ends the iteration.
        let (tx, rx) = mpsc::channel::<Event>();
        tx.send(make_event()).unwrap();
        drop(tx); // sender gone — after the one buffered event, iterator must end.

        let sub = Subscription::new(EventReceiver::new(rx), CancelHandle::noop());

        // Run iteration on a worker thread with a wall-clock timeout so a
        // regression manifests as a test failure rather than a hang.
        let (done_tx, done_rx) = mpsc::channel::<Vec<Event>>();
        std::thread::spawn(move || {
            let collected: Vec<Event> = sub.iter().collect();
            let _ = done_tx.send(collected);
        });

        let collected = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("SubscriptionIter did not terminate after sender was dropped");
        assert_eq!(collected.len(), 1, "expected the one buffered event");
    }
}
