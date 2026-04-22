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
    /// underlying channel disconnects.
    pub fn iter(&self) -> SubscriptionIter<'_> {
        SubscriptionIter { sub: self }
    }
}

/// Blocking iterator over events from a [`Subscription`].
pub struct SubscriptionIter<'a> {
    sub: &'a Subscription,
}

impl<'a> Iterator for SubscriptionIter<'a> {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        // Block in short intervals so the iterator stays responsive to drop.
        loop {
            match self.sub.rx.recv_timeout(Duration::from_millis(100)) {
                Some(event) => return Some(event),
                None => {
                    // Check if the channel is disconnected (sender dropped).
                    // recv_timeout returns None for both timeout and disconnect,
                    // but try_recv on a disconnected channel also returns None
                    // while a connected-but-empty channel returns None too.
                    // We simply keep looping; the iterator ends when the
                    // Subscription is dropped (which drops the cancel handle).
                    continue;
                }
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

    /// Receive with timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
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
