//! JS `_NativeSubscription` and `Event` classes.
//!
//! `_NativeSubscription` is the internal Rust-side subscription handle. The JS
//! wrapper in `index.js` wraps it into a public `Subscription` class that
//! extends `EventEmitter`, dispatching events to typed listeners.
//!
//! Architecture:
//!   1. `app.subscribe()` creates a `_NativeSubscription` (via AsyncTask).
//!   2. JS calls `_NativeSubscription.start(wakeup)` with a no-arg callback.
//!   3. A worker thread blocks on `xa11y::Subscription::recv` and, on each
//!      event, pushes it into a queue and calls `wakeup()` to notify JS.
//!   4. JS's wake-up callback calls `_NativeSubscription.drain()` to pull
//!      queued `Event` class instances, then emits them through EventEmitter.
//!   5. `close()` or `Drop` sets a cancellation flag; the worker exits on
//!      its next 100ms poll cycle.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};

use crate::element::Element;
use crate::types::event_type_to_str;

// ── Event ──────────────────────────────────────────────────────────────────

#[napi]
pub struct Event {
    kind: String,
    app_name: String,
    app_pid: u32,
    target_data: Option<xa11y::ElementData>,
    provider: Arc<dyn xa11y::Provider>,
}

impl Event {
    pub(crate) fn from_core(event: xa11y::Event, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            kind: event_type_to_str(event.event_type).to_string(),
            app_name: event.app_name,
            app_pid: event.app_pid,
            target_data: event.target,
            provider,
        }
    }
}

#[napi]
impl Event {
    /// Event kind, as a camelCase string (e.g. `"focusChanged"`, `"valueChanged"`).
    #[napi(getter, js_name = "type")]
    pub fn event_type(&self) -> String {
        self.kind.clone()
    }

    #[napi(getter)]
    pub fn app_name(&self) -> String {
        self.app_name.clone()
    }

    #[napi(getter)]
    pub fn app_pid(&self) -> u32 {
        self.app_pid
    }

    /// Snapshot of the element that triggered the event, if available.
    #[napi(getter)]
    pub fn target(&self) -> Option<Element> {
        self.target_data
            .as_ref()
            .map(|data| Element::new(data.clone(), self.provider.clone()))
    }
}

// ── _NativeSubscription ────────────────────────────────────────────────────

#[napi(js_name = "_NativeSubscription")]
pub struct NativeSubscription {
    queue: Arc<Mutex<VecDeque<xa11y::Event>>>,
    cancelled: Arc<AtomicBool>,
    provider: Arc<dyn xa11y::Provider>,
    /// Holds the xa11y::Subscription until `start()` moves it to the worker.
    pending_sub: Mutex<Option<xa11y::Subscription>>,
}

impl NativeSubscription {
    pub(crate) fn new(sub: xa11y::Subscription, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            cancelled: Arc::new(AtomicBool::new(false)),
            provider,
            pending_sub: Mutex::new(Some(sub)),
        }
    }
}

#[napi]
impl NativeSubscription {
    /// Start the background worker that reads events from the platform and
    /// calls `wakeup()` on the JS main thread whenever new events are queued.
    ///
    /// Must be called exactly once.
    #[napi]
    pub fn start(
        &self,
        #[napi(ts_arg_type = "() => void")] wakeup: ThreadsafeFunction<()>,
    ) -> napi::Result<()> {
        let sub =
            self.pending_sub.lock().unwrap().take().ok_or_else(|| {
                napi::Error::from_reason("Subscription already started or closed")
            })?;

        let queue = self.queue.clone();
        let cancelled = self.cancelled.clone();

        std::thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                match sub.recv(Duration::from_millis(100)) {
                    Ok(evt) => {
                        queue.lock().unwrap().push_back(evt);
                        wakeup.call(Ok(()), ThreadsafeFunctionCallMode::NonBlocking);
                    }
                    Err(xa11y::Error::Timeout { .. }) => continue,
                    Err(_) => break,
                }
            }
            // `sub` drops here, releasing the platform subscription.
        });

        Ok(())
    }

    /// Drain all queued events (called from the JS wake-up handler).
    #[napi]
    pub fn drain(&self) -> Vec<Event> {
        let drained: Vec<_> = self.queue.lock().unwrap().drain(..).collect();
        drained
            .into_iter()
            .map(|ev| Event::from_core(ev, self.provider.clone()))
            .collect()
    }

    /// Close the subscription and stop event delivery.
    #[napi]
    pub fn close(&self) {
        self.cancelled.store(true, Ordering::Release);
        // Drop the pending xa11y::Subscription if start() was never called.
        self.pending_sub.lock().unwrap().take();
    }
}

impl Drop for NativeSubscription {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
