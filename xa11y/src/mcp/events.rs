//! Live event subscriptions, held across tool calls.
//!
//! `xa11y events` blocks for as long as you watch it. A `tools/call` that
//! never returns hangs the client, so the operation cannot be one tool: it is
//! a start / poll / stop trio, following the MCP specification's [Stateful
//! Tools] guidance — a creation tool returns an opaque handle, later calls
//! take it as an argument.
//!
//! [Stateful Tools]: https://modelcontextprotocol.io/specification/2026-07-28/server/tools#stateful-tools
//!
//! # What the registry has to solve
//!
//! A [`Subscription`] is a channel that fills whether or not anyone is
//! reading, and `Send` but not `Sync` — it cannot be shared between a tool
//! call and whatever delivers events. So each subscription gets one drainer
//! thread that owns it and moves events into a bounded ring buffer:
//!
//! - **Bounded**, because nothing else limits how far behind a model may fall.
//!   A chatty application emits thousands of events a minute, and holding all
//!   of them to hand over eventually is a memory leak with a delay on it.
//! - **Reported**, because an event stream that silently loses entries is
//!   worse than one that says it lost forty (tenet 1). Every poll carries
//!   `dropped` since the last poll and `dropped_total`, and each event carries
//!   a `sequence` so a gap is visible in the events themselves.
//! - **Oldest-first eviction**, because the newest events describe the UI as
//!   it is now, which is what the caller is polling for.
//!
//! The thread also gives the poll its long-poll: it signals a condition
//! variable, so a poll with a `timeout_ms` wakes on the first event rather
//! than sleeping out its whole timeout.
//!
//! # Lifetime
//!
//! Nothing in MCP tells a server that a client lost interest in a handle, so
//! subscriptions expire: [`EXPIRY`] without a poll and the next tool call
//! reclaims it. `events_start` states the window in its description and
//! reports it as `expires_after_ms`, and a reclaimed handle comes back as
//! [`CliError::NoSubscription`] with `expired: true`, which is a different
//! failure kind from an id that was never issued.
//!
//! Handles never outlive the process. stdio is one server per client, so the
//! session *is* the process: when it ends, [`Registry`] drops, every drainer
//! stops, and every platform subscription is cancelled.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::tools::element_data_json;
use crate::cli::{format_event_kind, format_state_flag, CliError, CliResult};
use crate::{Event, EventKind, RecvStatus, Subscription};

/// Events held per subscription before the oldest are evicted.
///
/// One `Event` carries an `ElementData` snapshot, so this is a few hundred
/// kilobytes for a subscription nobody is draining — bounded, and small
/// beside the tree a single `tree` call builds.
pub(super) const BUFFER_CAPACITY: usize = 1_024;

/// Idle time after which a subscription is reclaimed.
///
/// Long enough that a model can think between polls, short enough that a
/// client which walked away does not leave a platform subscription running
/// for the life of the process.
pub(super) const EXPIRY: Duration = Duration::from_secs(300);

/// How often the drainer re-checks its stop flag while waiting for an event.
const DRAIN_TICK: Duration = Duration::from_millis(100);

/// One buffered event, with the ordering data the poll result reports.
struct Buffered {
    /// Monotonic per subscription, assigned before any eviction, so a gap in
    /// the sequence is exactly the events that were dropped.
    sequence: u64,
    /// Milliseconds between the subscription starting and this event arriving.
    ///
    /// Relative because `Event::timestamp` is an [`Instant`], which has no
    /// wall-clock rendering — and relative is what a caller wants anyway:
    /// monotonic, immune to clock changes, and directly comparable between
    /// two events of one subscription.
    at_ms: u64,
    event: Event,
}

/// The buffer, shared between the drainer thread and the polling tool call.
struct Queue {
    events: VecDeque<Buffered>,
    /// Dropped since the last poll read it. Reset on each poll.
    dropped_since_poll: u64,
    /// Dropped over the life of the subscription.
    dropped_total: u64,
    /// Handed to the caller over the life of the subscription.
    delivered: u64,
    next_sequence: u64,
    /// False once the event source disconnected — no further event can
    /// arrive, so a caller can stop polling instead of burning calls.
    live: bool,
    /// Set by [`Entry::shut_down`] to end the drainer.
    stopping: bool,
}

/// Everything the drainer and the tool call share.
struct Shared {
    queue: Mutex<Queue>,
    /// Signalled when an event lands or the stream ends, so a long poll wakes
    /// on the first event rather than sleeping out its timeout.
    ready: Condvar,
}

impl Shared {
    /// Lock the queue, recovering from a poisoned mutex (tenet 4).
    ///
    /// A panic in the drainer would poison this, and the queue is a buffer:
    /// the worst case is that the events already in it are handed over after
    /// a panic that had nothing to do with them.
    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One open subscription.
struct Entry {
    id: String,
    app_name: String,
    app_pid: Option<u32>,
    last_poll: Mutex<Instant>,
    shared: Arc<Shared>,
    drainer: Mutex<Option<JoinHandle<()>>>,
}

impl Entry {
    fn touch(&self) {
        *self.last_poll.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_poll
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
    }

    /// Stop the drainer and wait for it to release the platform subscription.
    ///
    /// The join is bounded by [`DRAIN_TICK`]: the drainer blocks for at most
    /// that long between stop-flag checks.
    fn shut_down(&self) {
        {
            let mut queue = self.shared.queue();
            queue.stopping = true;
            queue.live = false;
        }
        self.shared.ready.notify_all();
        let handle = self
            .drainer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(handle) = handle {
            // A panicked drainer is already gone; there is nothing to report
            // to a caller who asked us to stop it.
            let _ = handle.join();
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Every open subscription in this session.
///
/// Held by the tool host, so it lives exactly as long as the process — which
/// on stdio is exactly as long as the client's session.
pub(super) struct Registry {
    subs: Mutex<Vec<Arc<Entry>>>,
    next_id: AtomicU64,
}

impl Registry {
    pub(super) fn new() -> Self {
        Self {
            subs: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn subs(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Entry>>> {
        self.subs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start draining `sub` and return the handle result.
    ///
    /// `kinds` filters at the drainer rather than at the poll, so a filtered
    /// subscription's buffer holds only what was asked for — which is the
    /// point of the filter on a noisy application.
    pub(super) fn start(
        &self,
        app_name: &str,
        app_pid: Option<u32>,
        sub: Subscription,
        kinds: Option<Vec<String>>,
    ) -> CliResult<Value> {
        self.sweep();
        let id = format!("sub_{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                events: VecDeque::with_capacity(64),
                dropped_since_poll: 0,
                dropped_total: 0,
                delivered: 0,
                next_sequence: 0,
                live: true,
                stopping: false,
            }),
            ready: Condvar::new(),
        });
        let started = Instant::now();
        let drainer = spawn_drainer(Arc::clone(&shared), sub, kinds.clone(), started)?;

        let entry = Arc::new(Entry {
            id: id.clone(),
            app_name: app_name.to_string(),
            app_pid,
            last_poll: Mutex::new(started),
            shared,
            drainer: Mutex::new(Some(drainer)),
        });
        self.subs().push(entry);

        Ok(json!({
            "subscription_id": id,
            "application": app_name,
            "pid": app_pid,
            "kinds": kinds,
            "buffer_capacity": BUFFER_CAPACITY,
            "expires_after_ms": EXPIRY.as_millis() as u64,
        }))
    }

    /// Hand over up to `max` buffered events, waiting up to `timeout` for the
    /// first one.
    pub(super) fn poll(&self, id: &str, max: usize, timeout: Duration) -> CliResult<Value> {
        self.sweep();
        let entry = self.lookup(id)?;
        entry.touch();

        let mut queue = entry.shared.queue();
        if queue.events.is_empty() && !timeout.is_zero() {
            let deadline = Instant::now() + timeout;
            // Condvar wakeups are not guaranteed to mean progress, so the
            // wait is a loop over the real condition and the real deadline.
            while queue.events.is_empty() && queue.live {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, _) = entry
                    .shared
                    .ready
                    .wait_timeout(queue, remaining)
                    .unwrap_or_else(|e| e.into_inner());
                queue = next;
            }
        }

        let take = queue.events.len().min(max);
        let taken: Vec<Buffered> = queue.events.drain(..take).collect();
        let dropped = std::mem::take(&mut queue.dropped_since_poll);
        queue.delivered += taken.len() as u64;
        let buffered = queue.events.len();
        let live = queue.live;
        let dropped_total = queue.dropped_total;
        drop(queue);

        // Refresh the idle clock *after* the wait, so a long poll is not
        // charged for the time it spent blocking.
        entry.touch();

        Ok(json!({
            "subscription_id": entry.id,
            "application": entry.app_name,
            "pid": entry.app_pid,
            "events": taken.iter().map(event_json).collect::<Vec<Value>>(),
            "count": taken.len(),
            "dropped": dropped,
            "dropped_total": dropped_total,
            "buffered": buffered,
            "truncated": buffered > 0,
            "live": live,
        }))
    }

    /// Close a subscription and report what it did.
    pub(super) fn stop(&self, id: &str) -> CliResult<Value> {
        self.sweep();
        let entry = self.take(id)?;
        entry.shut_down();
        let queue = entry.shared.queue();
        Ok(json!({
            "subscription_id": entry.id,
            "stopped": true,
            "delivered": queue.delivered,
            "dropped_total": queue.dropped_total,
            "discarded": queue.events.len(),
        }))
    }

    /// Remove a handle from the registry, or say why it did not resolve.
    ///
    /// One lock for the find and the removal, so two `events_stop` calls
    /// racing on one handle cannot both report having stopped it.
    fn take(&self, id: &str) -> CliResult<Arc<Entry>> {
        let mut subs = self.subs();
        match subs.iter().position(|e| e.id == id) {
            Some(position) => Ok(subs.remove(position)),
            None => Err(self.miss(id, &subs)),
        }
    }

    /// Resolve a handle, or say why it did not resolve.
    fn lookup(&self, id: &str) -> CliResult<Arc<Entry>> {
        let subs = self.subs();
        match subs.iter().find(|e| e.id == id) {
            Some(entry) => Ok(Arc::clone(entry)),
            None => Err(self.miss(id, &subs)),
        }
    }

    /// The error for a handle this registry does not hold.
    fn miss(&self, id: &str, subs: &[Arc<Entry>]) -> CliError {
        CliError::NoSubscription {
            id: id.to_string(),
            // An id this session handed out and no longer holds was either
            // reclaimed for idling or stopped; one it never handed out is a
            // different mistake, and the model's next move differs. `next_id`
            // is the only record of what was issued once the entry is gone.
            expired: was_issued(id, self.next_id.load(Ordering::Relaxed)),
            live: subs.iter().map(|e| e.id.clone()).collect(),
        }
    }

    /// Reclaim subscriptions nobody has polled for [`EXPIRY`].
    ///
    /// Lazy rather than timed: a sweeper thread would have to outlive every
    /// subscription to be useful, and every path into this registry is a tool
    /// call, so there is no state a caller can observe between sweeps.
    fn sweep(&self) {
        self.sweep_idle(EXPIRY);
    }

    /// [`sweep`](Self::sweep) with the threshold as an argument, so the tests
    /// can reclaim a subscription without waiting out [`EXPIRY`].
    fn sweep_idle(&self, expiry: Duration) {
        let mut subs = self.subs();
        subs.retain(|entry| entry.idle_for() < expiry);
    }

    /// Buffered count, dropped-since-poll, and dropped-total for one handle.
    ///
    /// Reading the buffer without draining it, which is the only way a test
    /// can watch the drainer fill it.
    #[cfg(test)]
    fn snapshot(&self, id: &str) -> Option<(usize, u64, u64)> {
        let subs = self.subs();
        let entry = subs.iter().find(|e| e.id == id)?;
        let queue = entry.shared.queue();
        Some((
            queue.events.len(),
            queue.dropped_since_poll,
            queue.dropped_total,
        ))
    }
}

/// Whether `id` looks like a handle this session issued and has since let go.
///
/// The registry keeps no tombstones — the alternative is a list that grows for
/// the life of the process — so this reads the one number that does record
/// what was issued: the next id counter.
fn was_issued(id: &str, next_id: u64) -> bool {
    id.strip_prefix("sub_")
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|n| n > 0 && n < next_id)
}

/// Move events from `sub` into `shared` until told to stop or the source ends.
///
/// `sub` is moved into the thread, so the platform subscription is released
/// when the loop returns — on the stop flag, on a disconnect, or when the
/// spawn itself fails and the closure is dropped un-run.
/// A thread the OS refused to create is reported rather than panicked on
/// (tenet 4): the subscription simply does not open, and the caller can act
/// on that.
fn spawn_drainer(
    shared: Arc<Shared>,
    sub: Subscription,
    kinds: Option<Vec<String>>,
    started: Instant,
) -> CliResult<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("xa11y-mcp-events".into())
        .spawn(move || {
            loop {
                if shared.queue().stopping {
                    return;
                }
                match sub.recv_status(DRAIN_TICK) {
                    RecvStatus::Event(event) => {
                        if let Some(kinds) = &kinds {
                            if !kinds.iter().any(|k| k == format_event_kind(&event.kind)) {
                                continue;
                            }
                        }
                        let at_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        let mut queue = shared.queue();
                        let sequence = queue.next_sequence;
                        queue.next_sequence += 1;
                        if queue.events.len() >= BUFFER_CAPACITY {
                            // Evict the oldest: the newest events describe the
                            // UI as it is now, which is what a caller polling
                            // a live application is asking for.
                            queue.events.pop_front();
                            queue.dropped_since_poll += 1;
                            queue.dropped_total += 1;
                        }
                        queue.events.push_back(Buffered {
                            sequence,
                            at_ms,
                            event: *event,
                        });
                        drop(queue);
                        shared.ready.notify_all();
                    }
                    RecvStatus::Timeout => continue,
                    RecvStatus::Disconnected => {
                        // The application exited or the platform dropped the
                        // subscription. Say so rather than leaving the caller
                        // polling an empty buffer forever (tenet 1).
                        shared.queue().live = false;
                        shared.ready.notify_all();
                        return;
                    }
                }
            }
        })
        .map_err(|e| {
            CliError::Xa11y(crate::Error::Platform {
                code: e.raw_os_error().unwrap_or(0).into(),
                message: format!("could not start the event drainer thread: {e}"),
            })
        })
}

/// One event, in the shape the other element-returning tools use.
fn event_json(buffered: &Buffered) -> Value {
    let event = &buffered.event;
    let mut out = Map::new();
    out.insert("sequence".into(), json!(buffered.sequence));
    out.insert("at_ms".into(), json!(buffered.at_ms));
    out.insert("kind".into(), json!(format_event_kind(&event.kind)));
    if let EventKind::StateChanged { flag, value } = event.kind {
        out.insert("state_flag".into(), json!(format_state_flag(flag)));
        out.insert("state_value".into(), json!(value));
    }
    out.insert("application".into(), json!(event.app_name));
    out.insert("pid".into(), json!(event.app_pid));
    if let Some(target) = &event.target {
        out.insert("target".into(), element_data_json(target));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancelHandle, ElementData, EventReceiver, Role, StateFlag};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, Sender};

    /// Longest any test here waits for the drainer thread to catch up. Every
    /// wait is on a condition, so this is a failure deadline rather than a
    /// sleep — a passing run never spends it.
    const DEADLINE: Duration = Duration::from_secs(5);

    /// A registry with one subscription fed by the returned sender.
    fn registry_with(kinds: Option<Vec<String>>) -> (Registry, Sender<Event>, String) {
        let (tx, rx) = mpsc::channel::<Event>();
        let sub = Subscription::new(EventReceiver::new(rx), CancelHandle::noop());
        let registry = Registry::new();
        let started = registry
            .start("Test App", Some(4321), sub, kinds)
            .expect("a drainer thread can be spawned");
        let id = started["subscription_id"]
            .as_str()
            .expect("start reports a handle")
            .to_string();
        (registry, tx, id)
    }

    fn event(kind: EventKind) -> Event {
        Event::new(kind, "Test App", 4321)
    }

    fn send(tx: &Sender<Event>, kind: EventKind) {
        tx.send(event(kind)).expect("drainer is receiving");
    }

    /// Block until `predicate` holds, or fail after [`DEADLINE`].
    fn until(what: &str, predicate: impl Fn() -> bool) {
        let start = Instant::now();
        while start.elapsed() < DEADLINE {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out after {DEADLINE:?} waiting for {what}");
    }

    /// Poll until `want` events have been collected across however many calls
    /// it takes. The drainer is a separate thread, so one poll is not
    /// guaranteed to see everything already sent.
    fn collect(registry: &Registry, id: &str, want: usize) -> Vec<Value> {
        let mut events = Vec::new();
        let start = Instant::now();
        while events.len() < want && start.elapsed() < DEADLINE {
            let result = registry
                .poll(id, 500, Duration::from_millis(100))
                .expect("handle is open");
            events.extend(
                result["events"]
                    .as_array()
                    .expect("events is an array")
                    .clone(),
            );
        }
        assert_eq!(events.len(), want, "collected {events:?}");
        events
    }

    #[test]
    fn a_new_subscription_reports_its_handle_and_its_limits() {
        // The buffer size and the retention window are what a caller needs to
        // decide how often to poll, so they come back with the handle rather
        // than only in the tool description.
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx);
        let registry = Registry::new();
        let started = registry
            .start(
                "Test App",
                Some(4321),
                Subscription::new(EventReceiver::new(rx), CancelHandle::noop()),
                None,
            )
            .expect("a drainer thread can be spawned");
        assert_eq!(started["subscription_id"], json!("sub_1"));
        assert_eq!(started["application"], json!("Test App"));
        assert_eq!(started["pid"], json!(4321));
        assert_eq!(started["kinds"], Value::Null, "no filter means every kind");
        assert_eq!(started["buffer_capacity"], json!(BUFFER_CAPACITY));
        assert_eq!(
            started["expires_after_ms"],
            json!(EXPIRY.as_millis() as u64)
        );
    }

    #[test]
    fn each_subscription_gets_its_own_handle() {
        let (registry, _tx, first) = registry_with(None);
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx);
        let second = registry
            .start(
                "Other App",
                None,
                Subscription::new(EventReceiver::new(rx), CancelHandle::noop()),
                None,
            )
            .expect("a drainer thread can be spawned");
        assert_ne!(json!(first), second["subscription_id"]);
        assert_eq!(
            second["pid"],
            Value::Null,
            "an application with no reported pid says so rather than omitting it"
        );
        registry
            .poll(&first, 1, Duration::ZERO)
            .expect("still open");
    }

    #[test]
    fn events_come_back_in_order_with_their_sequence_and_target() {
        let (registry, tx, id) = registry_with(None);
        let mut data = ElementData::for_role(Role::Button);
        data.name = Some("Submit".into());
        let mut with_target = event(EventKind::FocusChanged);
        with_target.target = Some(data);
        tx.send(with_target).expect("drainer is receiving");
        send(&tx, EventKind::ValueChanged);

        let events = collect(&registry, &id, 2);
        assert_eq!(events[0]["sequence"], json!(0));
        assert_eq!(events[0]["kind"], json!("focus_changed"));
        assert_eq!(
            events[0]["target"]["name"],
            json!("Submit"),
            "the target is the same shape `find` returns"
        );
        assert_eq!(events[1]["sequence"], json!(1));
        assert_eq!(events[1]["kind"], json!("value_changed"));
        assert!(
            events[1].get("target").is_none(),
            "an event with no target says so by omission, as element payloads do"
        );
    }

    #[test]
    fn a_state_change_carries_the_flag_and_the_value_as_fields() {
        // Not only in the kind string: a harness branching on which flag
        // changed should not have to parse prose (tenet 6).
        let (registry, tx, id) = registry_with(None);
        send(
            &tx,
            EventKind::StateChanged {
                flag: StateFlag::Checked,
                value: true,
            },
        );
        let events = collect(&registry, &id, 1);
        assert_eq!(events[0]["kind"], json!("state_changed"));
        assert_eq!(events[0]["state_flag"], json!("checked"));
        assert_eq!(events[0]["state_value"], json!(true));
    }

    #[test]
    fn a_full_buffer_evicts_the_oldest_and_reports_the_loss() {
        let (registry, tx, id) = registry_with(None);
        let overflow = 10;
        for _ in 0..BUFFER_CAPACITY + overflow {
            send(&tx, EventKind::FocusChanged);
        }
        until("the buffer to fill and overflow", || {
            registry.snapshot(&id) == Some((BUFFER_CAPACITY, overflow as u64, overflow as u64))
        });

        let result = registry
            .poll(&id, BUFFER_CAPACITY, Duration::ZERO)
            .expect("handle is open");
        assert_eq!(result["dropped"], json!(overflow));
        assert_eq!(result["dropped_total"], json!(overflow));
        assert_eq!(result["count"], json!(BUFFER_CAPACITY));
        let events = result["events"].as_array().expect("events is an array");
        assert_eq!(
            events[0]["sequence"],
            json!(overflow),
            "the oldest events go, so the surviving run starts at the gap"
        );
    }

    #[test]
    fn dropped_resets_between_polls_but_the_total_does_not() {
        let (registry, tx, id) = registry_with(None);
        for _ in 0..BUFFER_CAPACITY + 3 {
            send(&tx, EventKind::FocusChanged);
        }
        until("the buffer to overflow", || {
            matches!(registry.snapshot(&id), Some((_, 3, 3)))
        });

        let first = registry
            .poll(&id, BUFFER_CAPACITY, Duration::ZERO)
            .expect("handle is open");
        assert_eq!(first["dropped"], json!(3));

        let second = registry
            .poll(&id, 10, Duration::ZERO)
            .expect("handle is open");
        assert_eq!(
            second["dropped"],
            json!(0),
            "`dropped` counts loss since the last poll, so it does not re-report"
        );
        assert_eq!(
            second["dropped_total"],
            json!(3),
            "the running total still remembers it"
        );
    }

    #[test]
    fn a_poll_takes_at_most_max_and_says_what_is_left() {
        let (registry, tx, id) = registry_with(None);
        for _ in 0..5 {
            send(&tx, EventKind::FocusChanged);
        }
        until("all five events to be buffered", || {
            matches!(registry.snapshot(&id), Some((5, _, _)))
        });

        let result = registry
            .poll(&id, 2, Duration::ZERO)
            .expect("handle is open");
        assert_eq!(result["count"], json!(2));
        assert_eq!(result["buffered"], json!(3));
        assert_eq!(
            result["truncated"],
            json!(true),
            "a shortened result that does not say so reads as a complete one"
        );
    }

    #[test]
    fn an_empty_poll_is_not_a_failure() {
        let (registry, _tx, id) = registry_with(None);
        let result = registry
            .poll(&id, 10, Duration::ZERO)
            .expect("an idle subscription is not an error");
        assert_eq!(result["count"], json!(0));
        assert_eq!(result["truncated"], json!(false));
        assert_eq!(result["live"], json!(true));
    }

    #[test]
    fn a_blocking_poll_returns_on_the_first_event_not_at_its_timeout() {
        let (registry, tx, id) = registry_with(None);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            send(&tx, EventKind::WindowOpened);
            // Hold the sender until the poll has certainly returned, so this
            // tests the event path rather than the disconnect path.
            std::thread::sleep(Duration::from_secs(2));
        });

        let start = Instant::now();
        let result = registry
            .poll(&id, 10, Duration::from_secs(10))
            .expect("handle is open");
        assert_eq!(result["count"], json!(1));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the poll waited {:?}, so it slept out its timeout instead of \
             waking on the event",
            start.elapsed()
        );
    }

    #[test]
    fn a_blocking_poll_gives_up_at_its_timeout() {
        let (registry, _tx, id) = registry_with(None);
        let start = Instant::now();
        let result = registry
            .poll(&id, 10, Duration::from_millis(100))
            .expect("handle is open");
        assert_eq!(result["count"], json!(0));
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert!(
            start.elapsed() < DEADLINE,
            "a poll that times out must return at its timeout"
        );
    }

    #[test]
    fn a_disconnected_source_stops_being_live() {
        let (registry, tx, id) = registry_with(None);
        send(&tx, EventKind::WindowClosed);
        drop(tx);

        // The buffered event is still handed over: disconnect ends the stream,
        // it does not discard what already arrived.
        let result = registry
            .poll(&id, 10, Duration::from_secs(1))
            .expect("handle is open");
        assert_eq!(result["count"], json!(1));

        let after = registry
            .poll(&id, 10, Duration::from_secs(1))
            .expect("handle is open");
        assert_eq!(after["count"], json!(0));
        assert_eq!(
            after["live"],
            json!(false),
            "a caller polling a dead stream must be told, not left waiting"
        );
    }

    #[test]
    fn a_dead_stream_does_not_hold_a_blocking_poll_open() {
        let (registry, tx, id) = registry_with(None);
        drop(tx);
        until("the drainer to notice the disconnect", || {
            registry
                .poll(&id, 10, Duration::ZERO)
                .is_ok_and(|r| r["live"] == json!(false))
        });

        let start = Instant::now();
        let result = registry
            .poll(&id, 10, Duration::from_secs(10))
            .expect("handle is open");
        assert_eq!(result["live"], json!(false));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a poll on a finished stream waited {:?} for an event that cannot arrive",
            start.elapsed()
        );
    }

    #[test]
    fn a_kind_filter_keeps_only_what_was_asked_for() {
        // Filtering at the drainer rather than at the poll is what makes the
        // buffer hold what the caller wants on a chatty application.
        let (registry, tx, id) = registry_with(Some(vec!["value_changed".into()]));
        send(&tx, EventKind::FocusChanged);
        send(&tx, EventKind::ValueChanged);
        send(&tx, EventKind::StructureChanged);

        let events = collect(&registry, &id, 1);
        assert_eq!(events[0]["kind"], json!("value_changed"));
        assert_eq!(
            registry.snapshot(&id).map(|s| s.0),
            Some(0),
            "the filtered-out kinds never reached the buffer"
        );
    }

    #[test]
    fn an_unknown_handle_names_the_ones_that_are_open() {
        let (registry, _tx, id) = registry_with(None);
        let err = registry
            .poll("sub_999", 10, Duration::ZERO)
            .expect_err("an id this session never issued");
        match err {
            CliError::NoSubscription { expired, live, .. } => {
                assert!(!expired, "never issued is not the same as expired");
                assert_eq!(live, vec![id], "the way out is in the error");
            }
            other => panic!("expected NoSubscription, got {other:?}"),
        }
    }

    #[test]
    fn a_stopped_handle_reads_as_expired_rather_than_unknown() {
        // The recovery differs: an expired handle means "start another one",
        // an unknown one means the id itself is wrong.
        let (registry, _tx, id) = registry_with(None);
        registry.stop(&id).expect("handle is open");
        let err = registry
            .poll(&id, 10, Duration::ZERO)
            .expect_err("a stopped handle no longer resolves");
        match err {
            CliError::NoSubscription { expired, live, .. } => {
                assert!(expired);
                assert!(live.is_empty());
            }
            other => panic!("expected NoSubscription, got {other:?}"),
        }
    }

    #[test]
    fn stopping_reports_what_it_delivered_and_what_it_discarded() {
        let (registry, tx, id) = registry_with(None);
        for _ in 0..3 {
            send(&tx, EventKind::FocusChanged);
        }
        until("all three events to be buffered", || {
            matches!(registry.snapshot(&id), Some((3, _, _)))
        });
        let polled = registry
            .poll(&id, 1, Duration::ZERO)
            .expect("handle is open");
        assert_eq!(polled["count"], json!(1));

        let stopped = registry.stop(&id).expect("handle is open");
        assert_eq!(stopped["stopped"], json!(true));
        assert_eq!(stopped["delivered"], json!(1));
        assert_eq!(stopped["discarded"], json!(2));
    }

    #[test]
    fn an_idle_subscription_is_reclaimed() {
        let (registry, _tx, id) = registry_with(None);
        registry.sweep_idle(Duration::ZERO);
        let err = registry
            .poll(&id, 10, Duration::ZERO)
            .expect_err("a reclaimed handle no longer resolves");
        assert!(matches!(
            err,
            CliError::NoSubscription { expired: true, .. }
        ));
    }

    #[test]
    fn a_poll_keeps_the_subscription_alive() {
        let (registry, _tx, id) = registry_with(None);
        registry.poll(&id, 10, Duration::ZERO).expect("open");
        registry.sweep_idle(Duration::from_secs(60));
        registry
            .poll(&id, 10, Duration::ZERO)
            .expect("a polled subscription is not idle");
    }

    /// A subscription whose cancellation is observable, standing in for the
    /// platform subscription a real one holds.
    fn cancellable() -> (Subscription, Arc<AtomicBool>, Sender<Event>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let (tx, rx) = mpsc::channel::<Event>();
        let sub = Subscription::new(
            EventReceiver::new(rx),
            CancelHandle::new(move || flag.store(true, Ordering::SeqCst)),
        );
        (sub, cancelled, tx)
    }

    #[test]
    fn stopping_releases_the_platform_subscription() {
        let (sub, cancelled, _tx) = cancellable();
        let registry = Registry::new();
        let started = registry
            .start("Test App", Some(1), sub, None)
            .expect("a drainer thread can be spawned");
        let id = started["subscription_id"].as_str().unwrap().to_string();

        registry.stop(&id).expect("handle is open");
        assert!(
            cancelled.load(Ordering::SeqCst),
            "stop returned while the platform subscription was still running"
        );
    }

    #[test]
    fn dropping_the_registry_releases_every_subscription() {
        // stdio is one server per client, so the session ending is the only
        // shutdown there is: nothing else would ever unsubscribe.
        let (sub, cancelled, _tx) = cancellable();
        let registry = Registry::new();
        registry
            .start("Test App", Some(1), sub, None)
            .expect("a drainer thread can be spawned");
        drop(registry);
        assert!(
            cancelled.load(Ordering::SeqCst),
            "the session ended with a platform subscription still running"
        );
    }
}
