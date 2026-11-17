//! Confirms that each posted event got through the event taps of other programs.
//!
//! A filtering event tap (dictation, hotkey and scroll tools) holds or drops
//! the events it sees, and macOS times it from when an event joins its queue.
//! Events posted faster than such a tap releases them queue up, the OS turns
//! the tap off, and the next events race past the held ones out of order.
//! A listen-only tap at the tail of the annotated session location sees an
//! event only after every filtering tap released it, so the provider waits
//! for that before it posts the next event. A dropped event gives an error
//! that names the filtering taps.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource};

use xa11y_core::{Delivery, Diagnosis, Error, Result};

type CGEventRef = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

const K_CG_ANNOTATED_SESSION_EVENT_TAP: u32 = 2;
const K_CG_TAIL_APPEND_EVENT_TAP: u32 = 1;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;
const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;

/// The event types the provider posts.
const POSTED: [(u32, &str); 14] = [
    (1, "left mouse down"),
    (2, "left mouse up"),
    (3, "right mouse down"),
    (4, "right mouse up"),
    (5, "mouse moved"),
    (6, "left mouse dragged"),
    (7, "right mouse dragged"),
    (K_CG_EVENT_KEY_DOWN, "key down"),
    (K_CG_EVENT_KEY_UP, "key up"),
    (K_CG_EVENT_FLAGS_CHANGED, "flags changed"),
    (22, "scroll wheel"),
    (25, "other mouse down"),
    (26, "other mouse up"),
    (27, "other mouse dragged"),
];

/// The top 16 bits of the source user data of a posted event; the rest is its sequence number.
const MARK: i64 = 0x7A11 << 48;
const SEQUENCE: i64 = (1 << 48) - 1;

/// Longer than the time after which macOS turns off a tap that holds an
/// event, so a slow tap delays the post and only a dropped event fails it.
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Arrived sequence numbers that no post waits for any more are dropped below this distance.
const KEPT: u64 = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGEventTapInformation {
    event_tap_id: u32,
    tap_point: u32,
    options: u32,
    events_of_interest: u64,
    tapping_process: i32,
    process_being_tapped: i32,
    enabled: bool,
    min_usec_latency: f32,
    avg_usec_latency: f32,
    max_usec_latency: f32,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGGetEventTapList(max: u32, list: *mut CGEventTapInformation, count: *mut u32) -> i32;
    fn CGEventGetType(event: CGEventRef) -> u32;
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}

pub(crate) struct Arrivals {
    state: Mutex<ArrivalState>,
    changed: Condvar,
    next: AtomicU64,
    port: AtomicPtr<c_void>,
}

#[derive(Default)]
struct ArrivalState {
    arrived: BTreeSet<u64>,
    pending: BTreeMap<u64, (u32, u64)>,
}

static ARRIVALS: Mutex<Option<Arc<Arrivals>>> = Mutex::new(None);

// Serialize initialization, but cache only success: a denied permission can
// be granted while the embedding process stays alive.
fn cached<T: Clone>(
    cache: &Mutex<Option<T>>,
    start: impl FnOnce() -> std::result::Result<T, String>,
) -> std::result::Result<T, String> {
    let mut value = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(value) = value.as_ref() {
        return Ok(value.clone());
    }
    let initialized = start()?;
    *value = Some(initialized.clone());
    Ok(initialized)
}

/// The process-wide arrival monitor, started on first use.
pub(crate) fn arrivals() -> Result<Arc<Arrivals>> {
    cached(&ARRIVALS, start).map_err(|reason| Error::PermissionDenied {
        instructions: format!(
            "xa11y cannot see posted input arrive ({reason}). Grant Accessibility to this \
             process in System Settings > Privacy & Security > Accessibility; without it \
             macOS also drops the input that it posts"
        ),
    })
}

fn start() -> std::result::Result<Arc<Arrivals>, String> {
    let arrivals = Arc::new(Arrivals {
        state: Mutex::new(ArrivalState::default()),
        changed: Condvar::new(),
        next: AtomicU64::new(1),
        port: AtomicPtr::new(std::ptr::null_mut()),
    });
    let monitor_state = Arc::clone(&arrivals);
    let mask = POSTED.iter().fold(0u64, |mask, (ty, _)| mask | 1 << ty);
    let (ready, started) = mpsc::channel();
    std::thread::Builder::new()
        .name("xa11y-input-arrival".into())
        .spawn(move || {
            let monitor = listen(&monitor_state, mask);
            let source = monitor.as_ref().ok().cloned();
            // `start` waits on the receiver until this send, so the send cannot fail.
            let _ = ready.send(monitor.map(|_| ()));
            if let Some(source) = source {
                // SAFETY: kCFRunLoopCommonModes is a constant CFString.
                CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
                CFRunLoop::run_current();
            }
        })
        .map_err(|e| format!("cannot start the arrival thread: {e}"))?;
    started
        .recv()
        .map_err(|_| "the arrival thread ended before it started".to_string())??;
    Ok(arrivals)
}

fn listen(arrivals: &Arrivals, mask: u64) -> std::result::Result<CFRunLoopSource, String> {
    // SAFETY: the run-loop thread retains `arrivals` until its event source stops.
    let port = unsafe {
        CGEventTapCreate(
            K_CG_ANNOTATED_SESSION_EVENT_TAP,
            K_CG_TAIL_APPEND_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            mask,
            arrived,
            arrivals as *const Arrivals as *mut c_void,
        )
    };
    if port.is_null() {
        return Err("CGEventTapCreate returned NULL".to_string());
    }
    arrivals.port.store(port as *mut c_void, Ordering::SeqCst);
    // SAFETY: CGEventTapCreate follows the create rule.
    let port = unsafe { CFMachPort::wrap_under_create_rule(port) };
    let source = port
        .create_runloop_source(0)
        .map_err(|()| "CFMachPortCreateRunLoopSource failed".to_string())?;
    // The tap stays for the life of the process.
    std::mem::forget(port);
    Ok(source)
}

extern "C" fn arrived(_: *mut c_void, ty: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    // SAFETY: `info` is the thread-owned `Arrivals` given to CGEventTapCreate.
    let arrivals = unsafe { &*(info as *const Arrivals) };
    if ty == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT || ty == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT {
        // SAFETY: the port is stored before the run loop delivers any event.
        unsafe { CGEventTapEnable(arrivals.port.load(Ordering::SeqCst) as CFMachPortRef, true) };
        return event;
    }
    // SAFETY: the tap gives a live event.
    let data = unsafe { CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) };
    // SAFETY: the callback holds a live event.
    let timestamp = unsafe { CGEventGetTimestamp(event) };
    arrivals.record(ty, data, timestamp);

    event
}

impl Arrivals {
    fn record(&self, ty: u32, data: i64, timestamp: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let marked = (data & !SEQUENCE == MARK).then_some((data & SEQUENCE) as u64);
        // A filter may copy/repost an event and clear sourceUserData. Match
        // only a registered timestamp AND type, never the next arbitrary input.
        // Replacements that change both identifiers remain Unknown delivery.
        let seq = marked
            .filter(|seq| state.pending.contains_key(seq))
            .or_else(|| {
                state.pending.iter().find_map(|(seq, signature)| {
                    (timestamp != 0 && *signature == (ty, timestamp)).then_some(*seq)
                })
            });
        if let Some(seq) = seq {
            state.pending.remove(&seq);
            state.arrived.insert(seq);
            self.changed.notify_all();
        }
    }

    pub(crate) fn discard(&self, seq: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.remove(&seq);
        state.arrived.remove(&seq);
    }

    /// Marks `event` so that the monitor knows it, and gives the mark to wait for.
    pub(crate) fn mark(&self, event: CGEventRef) -> u64 {
        let seq = self.next.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `event` is a live event that the caller owns.
        unsafe {
            CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, MARK | seq as i64)
        };
        // SAFETY: the caller owns the live event.
        let signature = unsafe { (CGEventGetType(event), CGEventGetTimestamp(event)) };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.insert(seq, signature);
        state.arrived = state.arrived.split_off(&seq.saturating_sub(KEPT));
        seq
    }

    /// Waits until the event marked `seq` got through every tap, or fails with the taps that can hold it.
    pub(crate) fn wait(&self, seq: u64, ty: u32) -> Result<()> {
        let start = Instant::now();
        let timeout = match xa11y_core::call_timeout(ARRIVAL_TIMEOUT, || {
            "posted input acknowledgment".to_string()
        }) {
            Ok(timeout) => timeout,
            Err(error) => {
                self.discard(seq);
                return Err(error.diagnose(Diagnosis::new().delivery(Delivery::Unknown)));
            }
        };
        let deadline = start + timeout.duration;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while !state.arrived.remove(&seq) {
            let now = Instant::now();
            if now >= deadline {
                state.pending.remove(&seq);
                drop(state);
                // Record which native wait exhausted an enclosing operation.
                if timeout.is_deadline() {
                    let _ = timeout.expired("posted input acknowledgment".to_string());
                }
                return not_arrived(ty, start.elapsed());
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Ok(())
    }
}

/// The type of a posted event.
pub(crate) fn event_type(event: CGEventRef) -> u32 {
    // SAFETY: `event` is a live event that the caller owns.
    unsafe { CGEventGetType(event) }
}

/// Whether a listen-only tap cannot see events of type `ty` now.
///
/// While a password field has secure input on, macOS keeps key events from
/// every tap except the ones of the process that turned it on.
pub(crate) fn hidden_from_taps(ty: u32) -> bool {
    matches!(ty, K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP | K_CG_EVENT_FLAGS_CHANGED)
        // SAFETY: no arguments, no state.
        && unsafe { IsSecureEventInputEnabled() }
}

fn not_arrived(ty: u32, elapsed: Duration) -> Result<()> {
    if hidden_from_taps(ty) {
        log::info!(
            "secure input turned on while xa11y waited for a posted {} event",
            type_name(ty)
        );
        return Ok(());
    }
    let name = type_name(ty);
    let filters = filtering_taps(ty);
    let observed = if filters.is_empty() {
        format!(
            "no tap of another program filters {name} events; macOS drops input that a process \
             without the Accessibility grant posts"
        )
    } else {
        format!(
            "{} tap(s) of other programs can hold or drop {name} events",
            filters.len()
        )
    };
    Err(Error::timeout(elapsed).diagnose(
        Diagnosis::new()
            .condition(format!(
                "the posted {name} event to get through the event taps"
            ))
            .last_observed(observed)
            .candidates(filters)
            .delivery(Delivery::Unknown),
    ))
}

fn type_name(ty: u32) -> &'static str {
    POSTED
        .iter()
        .find(|(t, _)| *t == ty)
        .map_or("input", |(_, name)| name)
}

/// The enabled filtering taps of other processes that take events of type `ty`.
fn filtering_taps(ty: u32) -> Vec<String> {
    let mut count = 0u32;
    // SAFETY: a NULL list with max 0 asks only for the count.
    if unsafe { CGGetEventTapList(0, std::ptr::null_mut(), &mut count) } != 0 {
        return vec!["CGGetEventTapList failed".to_string()];
    }
    let mut list = vec![CGEventTapInformation::default(); count as usize];
    // SAFETY: `list` has room for `count` entries.
    if unsafe { CGGetEventTapList(count, list.as_mut_ptr(), &mut count) } != 0 {
        return vec!["CGGetEventTapList failed".to_string()];
    }
    list.truncate(count as usize);
    let own = std::process::id() as i32;
    list.iter()
        .filter(|t| {
            t.enabled
                && t.options == K_CG_EVENT_TAP_OPTION_DEFAULT
                && t.tapping_process != own
                && t.events_of_interest & (1 << ty) != 0
        })
        .map(|t| {
            format!(
                "{} (pid {}) at the {} location, latency average {:.0} µs, max {:.0} µs",
                program(t.tapping_process),
                t.tapping_process,
                location_name(t.tap_point),
                t.avg_usec_latency,
                t.max_usec_latency
            )
        })
        .collect()
}

fn program(pid: i32) -> String {
    let mut buffer = [0u8; 4096];
    // SAFETY: `buffer` has room for `buffer.len()` bytes.
    let len = unsafe { proc_pidpath(pid, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32) };
    if len <= 0 {
        return "a program that this process cannot name".to_string();
    }
    String::from_utf8_lossy(&buffer[..len as usize]).into_owned()
}

fn location_name(tap_point: u32) -> &'static str {
    match tap_point {
        0 => "HID",
        1 => "session",
        K_CG_ANNOTATED_SESSION_EVENT_TAP => "annotated session",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_monitor_start_can_be_retried_and_success_is_cached() {
        let cache = Mutex::new(None);
        assert!(cached::<u32>(&cache, || Err("permission denied".into())).is_err());
        assert_eq!(cached(&cache, || Ok(42)).unwrap(), 42);
        assert_eq!(
            cached(&cache, || panic!("must reuse the monitor")).unwrap(),
            42
        );
    }

    fn monitor() -> Arrivals {
        Arrivals {
            state: Mutex::new(ArrivalState::default()),
            changed: Condvar::new(),
            next: AtomicU64::new(1),
            port: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    #[test]
    fn stripped_marker_requires_the_registered_timestamp_and_type() {
        let monitor = monitor();
        monitor.state.lock().unwrap().pending.insert(1, (10, 1234));
        monitor.record(10, 0, 1235);
        monitor.record(11, 0, 1234);
        assert!(monitor.state.lock().unwrap().arrived.is_empty());
        monitor.record(10, 0, 1234);
        assert!(monitor.state.lock().unwrap().arrived.contains(&1));
        monitor.discard(1);
        monitor.record(10, 0, 1234);
        assert!(monitor.state.lock().unwrap().arrived.is_empty());
    }
}
