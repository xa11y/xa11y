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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};

use crate::element::Element;
use crate::types::{event_kind_to_str, state_flag_to_str, TextChange};

// ── Event ──────────────────────────────────────────────────────────────────

/// An accessibility event delivered to a `Subscription`.
///
/// Events are emitted from the source application — focus changes, value
/// edits, window lifecycle, structural updates. Attach a listener via
/// `subscription.on(type, handler)` or await one with
/// `subscription.waitForEvent(type, opts)`.
#[napi]
pub struct Event {
    kind: String,
    /// For `stateChanged` events: the flag that changed (e.g. `"checked"`).
    state_flag: Option<String>,
    /// For `stateChanged` events: the new boolean value of the flag.
    state_value: Option<bool>,
    text_change: Option<TextChange>,
    app_name: String,
    app_pid: u32,
    target_data: Option<xa11y::ElementData>,
    provider: Arc<dyn xa11y::Provider>,
    sequence: u64,
    announcement: Option<AnnouncementInfo>,
    time: f64,
}

/// What an `announcement` event carries (`Event.announcement`).
#[napi(object)]
#[derive(Clone)]
pub struct AnnouncementInfo {
    /// `'request'` (an app asked for text to be spoken), `'live_region'` or
    /// `'alert'` (a region changed: read the target).
    pub origin: String,
    /// The text to announce, for a request where the platform sends it.
    pub text: Option<String>,
    /// `'polite'` or `'assertive'`, normalised across platforms.
    pub politeness: Option<String>,
    /// The platform's own priority, as it spells it.
    pub platform_priority: Option<String>,
    /// UIA's `activityId` (Windows only).
    pub activity_id: Option<String>,
}

impl From<xa11y::AnnouncementInfo> for AnnouncementInfo {
    fn from(a: xa11y::AnnouncementInfo) -> Self {
        Self {
            origin: match a.origin {
                xa11y::AnnouncementOrigin::Request => "request",
                xa11y::AnnouncementOrigin::LiveRegion => "live_region",
                xa11y::AnnouncementOrigin::Alert => "alert",
            }
            .to_string(),
            text: a.text,
            politeness: a
                .politeness
                .map(|p| crate::types::politeness_name(p).to_string()),
            platform_priority: a.platform_priority,
            activity_id: a.activity_id,
        }
    }
}

/// Turn the JS event type names `kinds` takes (`'focusChanged'`, the same
/// spelling as `Event.type`) into the core's snake_case names. Unknown names
/// pass through for core to reject with the list.
pub(crate) fn subscribe_options(
    kinds: Option<Vec<String>>,
    buffer: Option<u32>,
) -> xa11y::SubscribeOptions {
    let snake = |name: &str| {
        let mut out = String::with_capacity(name.len() + 4);
        for c in name.chars() {
            if c.is_ascii_uppercase() {
                out.push('_');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    };
    let mut options =
        xa11y::SubscribeOptions::new().kinds(kinds.unwrap_or_default().iter().map(|k| snake(k)));
    if let Some(buffer) = buffer {
        options = options.buffer(buffer as usize);
    }
    options
}

impl Event {
    pub(crate) fn from_core(event: xa11y::Event, provider: Arc<dyn xa11y::Provider>) -> Self {
        let kind_str = event_kind_to_str(&event.kind).to_string();
        let time = crate::capture::epoch_ms(event.time());
        let (state_flag, state_value) =
            if let xa11y::EventKind::StateChanged { flag, value } = event.kind {
                (Some(state_flag_to_str(flag).to_string()), Some(value))
            } else {
                (None, None)
            };
        Self {
            kind: kind_str,
            state_flag,
            state_value,
            text_change: event.text_change.map(TextChange::from),
            app_name: event.app_name,
            app_pid: event.app_pid,
            target_data: event.target,
            provider,
            sequence: event.sequence,
            time,
            announcement: event.announcement.map(Into::into),
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

    /// For `stateChanged` events: the flag that changed (e.g. `"checked"`, `"busy"`).
    /// `null` for all other event kinds.
    #[napi(getter)]
    pub fn state_flag(&self) -> Option<String> {
        self.state_flag.clone()
    }

    /// For `stateChanged` events: the new boolean value of the flag.
    /// `null` for all other event kinds.
    #[napi(getter)]
    pub fn state_value(&self) -> Option<bool> {
        self.state_value
    }

    /// For `textChanged` events: what was inserted or deleted, when the
    /// platform reports it (Linux only for now). `null` for other event
    /// kinds and for a `textChanged` with no delta. A `textSelectionChanged`
    /// event carries no offsets: read `text().caret()` / `selections()` on
    /// the target.
    #[napi(getter)]
    pub fn text_change(&self) -> Option<TextChange> {
        self.text_change.clone()
    }

    /// Name of the application that emitted this event.
    #[napi(getter)]
    pub fn app_name(&self) -> String {
        self.app_name.clone()
    }

    /// Process ID of the application that emitted this event.
    #[napi(getter)]
    pub fn app_pid(&self) -> u32 {
        self.app_pid
    }

    /// Position in this subscription's stream, from 0. A gap means events
    /// were dropped because the buffer was full (see `Subscription.dropped`).
    #[napi(getter)]
    pub fn sequence(&self) -> i64 {
        i64::try_from(self.sequence).unwrap_or(i64::MAX)
    }

    /// When the event was received, in milliseconds since the epoch (the
    /// unit `Date` takes).
    #[napi(getter)]
    pub fn time(&self) -> f64 {
        self.time
    }

    /// For announcement events: the origin and, where sent, the text.
    #[napi(getter)]
    pub fn announcement(&self) -> Option<AnnouncementInfo> {
        self.announcement.clone()
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
    /// The core subscription's dropped count, refreshed by the worker after
    /// each receive (the subscription itself lives on the worker thread).
    dropped: Arc<AtomicU64>,
    provider: Arc<dyn xa11y::Provider>,
    /// Holds the xa11y::Subscription until `start()` moves it to the worker.
    pending_sub: Mutex<Option<xa11y::Subscription>>,
}

impl NativeSubscription {
    pub(crate) fn new(sub: xa11y::Subscription, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            dropped: Arc::new(AtomicU64::new(sub.dropped())),
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
        let dropped = self.dropped.clone();

        std::thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                let status = sub.recv_status(Duration::from_millis(100));
                dropped.store(sub.dropped(), Ordering::Release);
                match status {
                    xa11y::RecvStatus::Event(evt) => {
                        queue.lock().unwrap().push_back(*evt);
                        wakeup.call(Ok(()), ThreadsafeFunctionCallMode::NonBlocking);
                    }
                    xa11y::RecvStatus::Timeout => continue,
                    // All senders dropped — the native event source has shut
                    // down, so the stream is legitimately finished. Exit the
                    // worker; JS sees no further events but no error either
                    // (mirrors how Subscription::iter terminates on disconnect).
                    xa11y::RecvStatus::Disconnected => break,
                }
            }
            // `sub` drops here, releasing the platform subscription.
        });

        Ok(())
    }

    /// How many events the buffer has dropped, in total, because it was full.
    #[napi(getter)]
    pub fn dropped(&self) -> i64 {
        i64::try_from(self.dropped.load(Ordering::Acquire)).unwrap_or(i64::MAX)
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
