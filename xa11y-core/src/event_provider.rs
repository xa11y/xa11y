use std::time::Duration;

use crate::error::Result;
use crate::event::{ElementState, Event, EventFilter};
use crate::node::NodeData;
use crate::provider::{AppTarget, Provider};

/// Optional trait for backends that support event subscriptions.
/// Extends Provider with reactive capabilities.
pub trait EventProvider: Provider {
    /// Subscribe to events matching the given filter.
    /// Returns a stream of events and a handle to manage the subscription.
    /// Dropping the `Subscription` unsubscribes automatically (RAII).
    fn subscribe(&self, target: &AppTarget, filter: EventFilter) -> Result<Subscription>;

    /// Wait for a single event matching the filter, with timeout.
    fn wait_for_event(
        &self,
        target: &AppTarget,
        filter: EventFilter,
        timeout: Duration,
    ) -> Result<Event>;

    /// Wait for an element matching the selector to reach the desired state.
    fn wait_for(
        &self,
        target: &AppTarget,
        selector: &str,
        state: ElementState,
        timeout: Duration,
    ) -> Result<Option<NodeData>>;
}

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

    /// Try to receive without blocking (returns None if no event ready).
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv()
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
    _cancel_fn: Option<Box<dyn FnOnce() + Send>>,
}

impl CancelHandle {
    /// Create a cancel handle with a cancellation callback.
    pub fn new(cancel_fn: impl FnOnce() + Send + 'static) -> Self {
        Self {
            _cancel_fn: Some(Box::new(cancel_fn)),
        }
    }

    /// Create a no-op cancel handle.
    pub fn noop() -> Self {
        Self { _cancel_fn: None }
    }
}

impl Drop for CancelHandle {
    fn drop(&mut self) {
        if let Some(cancel) = self._cancel_fn.take() {
            cancel();
        }
    }
}
