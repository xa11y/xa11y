//! JS `Subscription` and `Event` classes.
//!
//! A subscription is a single-consumer event channel. The JS wrapper turns
//! `nextEvent()` into an async iterator so users can write:
//!
//! ```js
//! for await (const ev of sub) { ... }
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::element::Element;
use crate::map_err;
use crate::types::event_type_to_str;

#[napi]
pub struct Event {
    event_type: String,
    app_name: String,
    app_pid: u32,
    target_data: Option<xa11y::ElementData>,
    provider: Arc<dyn xa11y::Provider>,
}

impl Event {
    pub(crate) fn from_core(event: xa11y::Event, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            event_type: event_type_to_str(event.event_type).to_string(),
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
    #[napi(getter)]
    pub fn event_type(&self) -> String {
        self.event_type.clone()
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

#[napi]
pub struct Subscription {
    inner: Arc<Mutex<Option<xa11y::Subscription>>>,
    provider: Arc<dyn xa11y::Provider>,
}

impl Subscription {
    pub(crate) fn new(sub: xa11y::Subscription, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(sub))),
            provider,
        }
    }
}

#[napi]
impl Subscription {
    /// Try to receive an event without blocking. Returns `null` if none ready.
    #[napi]
    pub fn try_recv(&self) -> Option<Event> {
        let guard = self.inner.lock().unwrap();
        let sub = guard.as_ref()?;
        sub.try_recv()
            .map(|e| Event::from_core(e, self.provider.clone()))
    }

    /// Wait for the next event up to `timeoutSeconds` (default 5s).
    /// Rejects with a `TimeoutError` if no event arrives in time.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Event>"
    )]
    pub fn recv(&self, timeout_seconds: Option<f64>) -> AsyncTask<RecvTask> {
        AsyncTask::new(RecvTask {
            inner: self.inner.clone(),
            provider: self.provider.clone(),
            timeout: Duration::from_secs_f64(timeout_seconds.unwrap_or(5.0)),
        })
    }

    /// Close the subscription and release the underlying receiver.
    #[napi]
    pub fn close(&self) {
        self.inner.lock().unwrap().take();
    }

    /// Whether the subscription is still active (not closed).
    #[napi(getter)]
    pub fn active(&self) -> bool {
        self.inner.lock().unwrap().is_some()
    }
}

pub struct RecvTask {
    inner: Arc<Mutex<Option<xa11y::Subscription>>>,
    provider: Arc<dyn xa11y::Provider>,
    timeout: Duration,
}

impl Task for RecvTask {
    type Output = xa11y::Event;
    type JsValue = Event;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // Poll `try_recv` with short sleeps rather than holding the mutex
        // across a blocking `recv` — that way `close()` and `tryRecv()` from
        // other threads stay responsive while a recv is pending.
        let start = std::time::Instant::now();
        let poll = Duration::from_millis(20);
        loop {
            {
                let guard = self.inner.lock().unwrap();
                let sub = guard.as_ref().ok_or_else(|| {
                    napi::Error::from_reason(format!(
                        "{}: Subscription is closed",
                        crate::errors::codes::PLATFORM
                    ))
                })?;
                if let Some(event) = sub.try_recv() {
                    return Ok(event);
                }
            }
            if start.elapsed() >= self.timeout {
                return Err(map_err(xa11y::Error::Timeout {
                    elapsed: start.elapsed(),
                }));
            }
            std::thread::sleep(poll);
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Event::from_core(output, self.provider.clone()))
    }
}
