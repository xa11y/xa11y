use std::time::Duration;

use crate::error::{Error, Result};
use crate::event::Event;

/// A live event subscription. Drop to unsubscribe.
///
/// `Subscription` is `Send` but not `Clone`. It can be moved to another
/// thread but not shared.
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

    /// Try to receive without blocking (returns `None` if no event ready).
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv()
    }

    /// Block until an event arrives or the timeout expires.
    pub fn recv(&self, timeout: Duration) -> Result<Event> {
        self.rx
            .recv_timeout(timeout)
            .ok_or(Error::Timeout { elapsed: timeout })
    }

    /// Block until an event arrives, the timeout expires, or the event source
    /// disconnects. Unlike [`recv`](Self::recv), this preserves the distinction
    /// between "no event yet" and "no events will ever arrive" so bindings can
    /// terminate their poll loops cleanly when the subscription shuts down.
    pub fn recv_status(&self, timeout: Duration) -> RecvStatus {
        self.rx.recv_timeout_status(timeout)
    }

    /// Block until an event matching `predicate` arrives or the timeout expires.
    pub fn wait_for(&self, predicate: impl Fn(&Event) -> bool, timeout: Duration) -> Result<Event> {
        let start = std::time::Instant::now();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(Error::Timeout {
                    elapsed: start.elapsed(),
                });
            }
            // Poll with short recv timeouts so we can re-check the deadline.
            let poll = remaining.min(Duration::from_millis(10));
            if let Some(event) = self.rx.recv_timeout(poll) {
                if predicate(&event) {
                    return Ok(event);
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
    rx: std::sync::mpsc::Receiver<Event>,
}

impl EventReceiver {
    /// Create a new event receiver wrapping a standard channel.
    pub fn new(rx: std::sync::mpsc::Receiver<Event>) -> Self {
        Self { rx }
    }

    /// Try to receive without blocking.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Receive with timeout. Returns `None` for both timeout and disconnect —
    /// use [`recv_timeout_status`](Self::recv_timeout_status) when the caller
    /// needs to distinguish "no event yet" from "stream finished".
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Receive with timeout, preserving the distinction between timeout and
    /// sender disconnect. Used by [`SubscriptionIter`] to terminate cleanly
    /// when the event source goes away, rather than spinning forever.
    pub fn recv_timeout_status(&self, timeout: Duration) -> RecvStatus {
        use std::sync::mpsc::RecvTimeoutError;
        match self.rx.recv_timeout(timeout) {
            Ok(event) => RecvStatus::Event(Box::new(event)),
            Err(RecvTimeoutError::Timeout) => RecvStatus::Timeout,
            Err(RecvTimeoutError::Disconnected) => RecvStatus::Disconnected,
        }
    }
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
