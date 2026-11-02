//! Native AT-SPI2 event subscription (push-based, via D-Bus signals).
//!
//! This replaces the prior inert subscription stub. AT-SPI2 delivers
//! accessibility events as D-Bus signals on four interfaces:
//!
//! - `org.a11y.atspi.Event.Object` — StateChanged, ChildrenChanged,
//!   PropertyChange, TextChanged, SelectionChanged, ValueChanged,
//!   Announcement, BoundsChanged, VisibleDataChanged, etc.
//! - `org.a11y.atspi.Event.Window` — Create/Destroy/Activate/Deactivate/…
//! - `org.a11y.atspi.Event.Focus` — Focus
//! - `org.a11y.atspi.Event.Document` — LoadComplete (not currently modelled)
//!
//! Each signal body follows the AT-SPI2 standard signature `(siiva{sv})`:
//! `(detail_kind, detail1, detail2, any_data, properties)`. The source
//! element is identified by the D-Bus message header's `sender` (bus name)
//! and `path` (object path).
//!
//! Subscriptions are scoped to a single application: we resolve the app's
//! D-Bus unique name via the registry, then install match rules filtered
//! by `sender`. A dedicated background thread iterates the message stream
//! and fans received signals out to an mpsc channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::OwnedValue;
use zbus::MatchRule;

use xa11y_core::{
    CancelHandle, ElementData, ElementParts, Error, Event, EventKind, EventParts, EventReceiver,
    Result, Role, StateFlag, StateParts, StateSet, Subscription, Toggled,
};

use crate::atspi::{
    is_listable_top_level, is_password_role, map_atspi_role, map_atspi_role_number,
    read_interfaces, AccessibleRef, Interfaces, LinuxProvider,
};

/// AT-SPI2 event-interface names we subscribe to via match rules.
///
/// Kept as a shared constant so registration and the interface filter in
/// `signal_to_kinds` iterate the same list.
pub(crate) const EVENT_INTERFACES: &[&str] = &[
    "org.a11y.atspi.Event.Object",
    "org.a11y.atspi.Event.Window",
    "org.a11y.atspi.Event.Focus",
    "org.a11y.atspi.Event.Document",
];

// ── Entry point ─────────────────────────────────────────────────────────────

/// Subscribe to AT-SPI2 events for the application identified by `pid`.
///
/// Returns a `Subscription` whose receiver yields `Event`s for every signal
/// emitted on the four AT-SPI event interfaces (scoped by the app's D-Bus
/// sender name). Drops and re-subscribes are clean: the cancel closure
/// removes every match rule before the thread joins.
pub(crate) fn subscribe_for_pid(
    provider: &LinuxProvider,
    pid: u32,
    app_name: String,
) -> Result<Subscription> {
    let sender_buses: Vec<String> = provider
        .find_apps_by_pid(pid)?
        .into_iter()
        .map(|app| app.bus_name)
        .collect();
    if sender_buses.is_empty() {
        return Err(Error::Platform {
            code: -1,
            message: format!("No application found with PID {pid}"),
        });
    }

    // Dedicated Connection per subscription. Dropping it (after we've
    // removed every match rule) cleanly tears the subscription down and
    // forbids any stale signal from arriving at the previous owner.
    let conn = LinuxProvider::connect_a11y_bus()?;

    // Match-rule management goes through the DBusProxy — the blocking
    // Connection itself doesn't expose AddMatch/RemoveMatch.
    let dbus = DBusProxy::new(&conn).map_err(|e| Error::Platform {
        code: -1,
        message: format!("DBusProxy: {e}"),
    })?;

    let mut rules: Vec<MatchRule<'static>> =
        Vec::with_capacity(EVENT_INTERFACES.len() * sender_buses.len());
    for sender_bus in &sender_buses {
        for iface in EVENT_INTERFACES {
            // Pass owned Strings so the produced MatchRule is `'static` and
            // can be moved into the cancel closure alongside other owned state.
            let rule = MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(sender_bus.clone())
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("sender match rule: {e}"),
                })?
                .interface((*iface).to_string())
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("interface match rule: {e}"),
                })?
                .build();
            dbus.add_match_rule(rule.clone())
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("add_match_rule({iface}): {e}"),
                })?;
            rules.push(rule);
        }
    }

    // Tell the registry which events we listen to. Toolkits that consult the
    // registered-listener list (Qt, GTK 3's atk-bridge) emit nothing at all
    // for an unregistered event, so without this a Qt app's subscription
    // was silent (issue #488). Registration is tied to this connection's
    // bus name; the cancel path deregisters each one in the form the
    // registry declares for `DeregisterEvent`, and the registry also drops
    // them when the connection closes.
    let registered = register_events(&conn, &sender_buses)?;

    // Seed the top-level window states. This is also the barrier that makes
    // registration effective before `subscribe` returns: the application
    // answers these reads on the same queue that delivered the registry's
    // `EventListenerRegistered` signal, so by the time they return it has
    // switched its emission on.
    let mut windows = HashMap::new();
    for bus in &sender_buses {
        seed_top_level_windows(&conn, bus, &sender_buses, &mut windows)?;
    }
    // Windows already open get a snapshot now, so their eventual
    // `WindowClosed` names them even when a destroyed window no longer
    // answers by then.
    let window_snapshots: HashMap<String, ElementData> = windows
        .iter()
        .filter(|(_, window)| window.listed)
        .filter_map(|(key, _)| {
            let aref = window_ref(key, &sender_buses)?;
            build_event_snapshot(&conn, &aref, Some(pid))
                .filter(|t| is_window_role(t.role))
                .map(|t| (key.clone(), t))
        })
        .collect();

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let ctx = Arc::new(EventContext {
        sender_buses: sender_buses.clone(),
        app_name,
        app_pid: pid,
        tx: Mutex::new(tx),
        conn: conn.clone(),
        windows: Mutex::new(windows),
        window_snapshots: Mutex::new(window_snapshots),
    });

    // Activate the broadcast receiver *before* spawning the worker. If we
    // waited until inside the spawned thread to construct the iterator,
    // there would be a window between `add_match_rule` returning above and
    // the thread scheduling its first statement during which the daemon
    // could deliver matching signals to our Connection — where they would
    // fan out to zero subscribers and be dropped. Creating the iterator
    // here makes the subscription atomic from the caller's point of view:
    // any signal that arrives after every `add_match_rule` call has
    // returned is guaranteed to land in our queue.
    let iter = MessageIterator::from(conn.clone());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let ctx_for_thread = ctx.clone();
    let handle = thread::spawn(move || {
        for msg in iter {
            if stop_for_thread.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(msg) = msg {
                ctx_for_thread.dispatch(&msg);
            }
        }
    });

    // `CancelHandle` captures the connection and rules. On drop:
    // 1. Flip the stop flag so the thread exits on its next wake-up.
    // 2. Remove every match rule so the D-Bus daemon stops routing
    //    matching signals to us.
    // 3. Issue a Ping to the daemon — its reply travels back through our
    //    MessageIterator, forcing `.next()` to return and letting the
    //    thread observe the stop flag.
    // 4. Join the worker thread.
    let cancel_conn = conn.clone();
    let cancel = CancelHandle::new(move || {
        stop.store(true, Ordering::Relaxed);
        // Best-effort teardown: a drop cannot report failure, and the
        // registry drops a closed connection's registrations anyway.
        deregister_events(&cancel_conn, &registered);
        if let Ok(dbus) = DBusProxy::new(&cancel_conn) {
            for rule in &rules {
                let _ = dbus.remove_match_rule(rule.clone());
            }
        }
        // Wake the blocked MessageIterator by forcing a round-trip: the
        // daemon's Ping reply is broadcast through our connection's
        // message stream, so `.next()` returns and the thread observes
        // the stop flag on its next iteration.
        let _ = cancel_conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus.Peer"),
            "Ping",
            &(),
        );
        let _ = handle.join();
    });

    Ok(Subscription::new(EventReceiver::new(rx), cancel))
}

// ── Event context + dispatch ────────────────────────────────────────────────

/// Shared state between the subscription thread and the cancel path.
///
/// `tx` is wrapped in `Mutex` because `std::sync::mpsc::Sender` is `!Sync`;
/// contention is trivial (one lock per emitted event).
struct EventContext {
    sender_buses: Vec<String>,
    app_name: String,
    app_pid: u32,
    tx: Mutex<std::sync::mpsc::Sender<Event>>,
    /// Used for D-Bus property/method calls to build target snapshots.
    /// Cloned from the subscription's owning Connection, so it stays alive
    /// until every `Arc<EventContext>` (including the one held by the
    /// spawned thread) is dropped.
    conn: Connection,
    /// Each tracked top-level window (keyed `bus` + `path`): whether it is
    /// currently listed in `App::windows()`, so `WindowOpened` /
    /// `WindowClosed` fire once per transition however many signals report
    /// it, and which application bus it belongs to.
    windows: Mutex<HashMap<String, TrackedWindow>>,
    /// The last window-role snapshot of each tracked window, for the
    /// `WindowClosed` target: a destroyed GTK 4 window no longer answers
    /// its role or name by the time `defunct` arrives.
    window_snapshots: Mutex<HashMap<String, ElementData>>,
}

impl EventContext {
    fn emit(&self, kind: EventKind, target: Option<ElementData>) {
        let event: Event = EventParts {
            kind,
            target,
            app_name: self.app_name.clone(),
            app_pid: self.app_pid,
            timestamp: std::time::Instant::now(),
        }
        .into();
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(event);
        }
    }

    fn dispatch(&self, msg: &zbus::Message) {
        let header = msg.header();

        // Match rules already filter by sender, but we stay defensive: the
        // daemon's Ping reply (used to wake the iterator on cancel) arrives
        // here too and must be skipped.
        let Some(sender) = header.sender() else {
            return;
        };
        if !self.sender_buses.iter().any(|bus| bus == sender.as_str()) {
            return;
        }

        let Some(iface) = header.interface() else {
            return;
        };
        let Some(member) = header.member() else {
            return;
        };
        let Some(path) = header.path() else {
            return;
        };

        let Some((detail, detail1)) = event_body_fields(msg) else {
            return;
        };

        let target_ref = AccessibleRef {
            bus_name: sender.as_str().to_string(),
            path: path.to_string(),
        };
        // Snapshot the source element synchronously so consumers receive a
        // durable `ElementData` even if the element is destroyed before
        // they read the event.
        let target = build_event_snapshot(&self.conn, &target_ref, Some(self.app_pid));
        let target_role = target.as_ref().map(|t| t.role);

        let kinds = signal_to_kinds(
            iface.as_str(),
            member.as_str(),
            &detail,
            detail1,
            target_role,
        );

        for kind in kinds {
            match kind {
                // `Window:Create` / `Window:Destroy`: deduplicated against
                // the derived lifecycle below.
                EventKind::WindowOpened => self.window_listed(&target_ref, true, target.clone()),
                EventKind::WindowClosed => self.window_listed(&target_ref, false, target.clone()),
                other => self.emit(other, target.clone()),
            }
        }

        self.derive_window_lifecycle(
            iface.as_str(),
            member.as_str(),
            &detail,
            detail1,
            &target_ref,
            target,
        );
    }

    /// Re-read `bus`'s top-level windows after its application object's
    /// children changed: a new listable window is opened, a known window no
    /// longer among the children is closed. A failed read emits nothing;
    /// the subscription has no error channel, and a later change rescans.
    fn rescan_top_level_windows(&self, bus: &str) {
        let mut current = HashMap::new();
        if seed_top_level_windows(&self.conn, bus, &self.sender_buses, &mut current).is_err() {
            return;
        }
        let plan = {
            let windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            plan_rescan(&windows, current, bus, &self.sender_buses)
        };
        // A seeded hidden window that has since been destroyed was never
        // listed, so it leaves without an event, but must not linger.
        if !plan.vanished_hidden.is_empty() {
            let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            for key in &plan.vanished_hidden {
                windows.remove(key);
            }
        }
        for (aref, listed) in plan.current {
            // Snapshot only on a real transition: a rescan revisits every
            // window, and most have not changed.
            self.window_listed_with(&aref, bus, listed, || {
                build_event_snapshot(&self.conn, &aref, Some(self.app_pid))
            });
        }
        for aref in plan.gone {
            self.window_listed(&aref, false, None);
        }
    }

    /// Record that a top-level window entered (`listed`) or left the
    /// window list, emitting `WindowOpened` / `WindowClosed` only on a
    /// change (see [`note_window`]). For a window named by an event's
    /// sender, whose bus is the application's own.
    fn window_listed(&self, aref: &AccessibleRef, listed: bool, target: Option<ElementData>) {
        self.window_listed_with(aref, &aref.bus_name, listed, || target);
    }

    /// [`Self::window_listed`] for a window of the application on
    /// `app_bus`, with the target built only when the transition is real.
    fn window_listed_with(
        &self,
        aref: &AccessibleRef,
        app_bus: &str,
        listed: bool,
        target: impl FnOnce() -> Option<ElementData>,
    ) {
        let key = window_key(aref);
        let changed = note_window(
            &mut self.windows.lock().unwrap_or_else(|e| e.into_inner()),
            key.clone(),
            app_bus,
            listed,
        );
        if !changed {
            return;
        }
        let target = {
            let mut snapshots = self
                .window_snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let target = match target() {
                Some(t) if is_window_role(t.role) => Some(t),
                other => snapshots.get(&key).cloned().or(other),
            };
            // A closed window's entry goes with it, so neither map grows
            // with every window the application ever opened.
            if listed {
                if let Some(t) = &target {
                    snapshots.insert(key, t.clone());
                }
            } else {
                snapshots.remove(&key);
            }
            target
        };
        let kind = if listed {
            EventKind::WindowOpened
        } else {
            EventKind::WindowClosed
        };
        self.emit(kind, target);
    }

    /// Derive window open/close for toolkits that never emit `Window:Create`
    /// / `Window:Destroy` (issue #488). Observed on X11:
    ///
    /// - GTK 4 adds a new window as a child of the application object
    ///   (`ChildrenChanged:add` on `/org/a11y/atspi/accessible/root`) and
    ///   marks a closed one `defunct`.
    /// - Qt keeps a hidden window in the tree and only toggles its states
    ///   (`showing`), so open/close is the window entering or leaving the
    ///   set `App::windows()` lists: `VISIBLE` or `ICONIFIED` (#492).
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site, passing the signal's fields through unchanged"
    )]
    fn derive_window_lifecycle(
        &self,
        iface: &str,
        member: &str,
        detail: &str,
        detail1: i32,
        source: &AccessibleRef,
        target: Option<ElementData>,
    ) {
        if iface != "org.a11y.atspi.Event.Object" {
            return;
        }
        match (member, detail.to_ascii_lowercase().as_str()) {
            ("ChildrenChanged", "add" | "remove") if source.path == APP_ROOT_PATH => {
                // The signal's child reference cannot be trusted here: GTK 4
                // names the application object itself as the added child.
                // Re-read the application's top-level windows instead and
                // diff them against what the subscription knows.
                self.rescan_top_level_windows(&source.bus_name);
            }
            ("StateChanged", "defunct") if detail1 != 0 => {
                // A defunct object may no longer answer its role, so a window
                // the subscription already tracks counts by membership.
                let known = self
                    .windows
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&window_key(source));
                if known || target.as_ref().is_some_and(|t| is_window_role(t.role)) {
                    self.window_listed(source, false, target);
                }
            }
            ("StateChanged", "showing" | "visible" | "iconified") => {
                if !target.as_ref().is_some_and(|t| is_window_role(t.role))
                    || !is_top_level(&self.conn, source)
                {
                    return;
                }
                // Decide on the state as it is now, not on the signal's
                // value: GTK 4 reports `showing`/`visible` false on a window
                // it is in the middle of mapping. The signal and the fresh
                // read must agree before a transition counts.
                let Some(bits) = get_state(&self.conn, source).map(|b| bits_from_u32s(&b)) else {
                    return;
                };
                let listed = is_listable_top_level(bits);
                if listed == (detail1 != 0) {
                    self.window_listed(source, listed, target);
                }
            }
            _ => {}
        }
    }
}

/// The `(detail, detail1)` fields of an AT-SPI event signal.
///
/// The current body is `(siiva{sv})`; GTK 4 still sends some events,
/// including `ChildrenChanged` on the application object, in the legacy
/// `(siiv(so))` form, whose trailer is the application reference. Both are
/// read by their signature. Any other body is not an AT-SPI event and is
/// dropped, as before.
fn event_body_fields(msg: &zbus::Message) -> Option<(String, i32)> {
    let body = msg.body();
    let signature = body.signature().to_string_no_parens();
    let (detail, detail1) = match signature.as_str() {
        "siiva{sv}" => {
            let (d, d1, _d2, _any, _props): (
                String,
                i32,
                i32,
                OwnedValue,
                HashMap<String, OwnedValue>,
            ) = body.deserialize().ok()?;
            (d, d1)
        }
        "siiv(so)" => {
            let (d, d1, _d2, _any, _app): (
                String,
                i32,
                i32,
                OwnedValue,
                (String, zbus::zvariant::OwnedObjectPath),
            ) = body.deserialize().ok()?;
            (d, d1)
        }
        _ => return None,
    };
    Some((detail, detail1))
}

/// The application object's path on every AT-SPI application connection.
const APP_ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

/// Record `listed` for `key`; whether that is a change worth an event.
///
/// Opening counts unless the window is already listed. Closing counts only
/// for a window known to be listed, and drops its entry, so the map holds
/// the open windows (plus seeded hidden ones) rather than every window the
/// application ever had; a second close signal for the same window (GTK 4's
/// `defunct` then `Window:Destroy`) finds no entry and stays silent.
///
/// A newly tracked window belongs to `app_bus`; one already tracked keeps
/// the application it was first recorded under.
fn note_window(
    windows: &mut HashMap<String, TrackedWindow>,
    key: String,
    app_bus: &str,
    listed: bool,
) -> bool {
    if listed {
        match windows.get_mut(&key) {
            Some(window) => !std::mem::replace(&mut window.listed, true),
            None => {
                windows.insert(key, TrackedWindow::new(true, app_bus));
                true
            }
        }
    } else {
        windows.remove(&key).is_some_and(|window| window.listed)
    }
}

/// One top-level window the subscription tracks.
#[derive(Debug, Clone, PartialEq)]
struct TrackedWindow {
    /// Whether `App::windows()` lists it.
    listed: bool,
    /// The application bus whose children it was read from. A window's own
    /// key can name another bus (a child reports its own), so a rescan of
    /// an application diffs on this rather than on the key's prefix.
    app_bus: String,
}

impl TrackedWindow {
    fn new(listed: bool, app_bus: &str) -> Self {
        Self {
            listed,
            app_bus: app_bus.to_owned(),
        }
    }
}

/// Split a [`window_key`] back into bus name and path, given the buses it
/// can belong to.
fn split_window_key<'k>(key: &'k str, buses: &[String]) -> Option<(&'k str, &'k str)> {
    buses.iter().find_map(|bus| {
        let path = key.strip_prefix(bus.as_str())?;
        path.starts_with('/').then(|| (&key[..bus.len()], path))
    })
}

/// Rebuild the [`AccessibleRef`] a [`window_key`] was made from. `None` for
/// a key on none of `buses`.
fn window_ref(key: &str, buses: &[String]) -> Option<AccessibleRef> {
    let (bus, path) = split_window_key(key, buses)?;
    Some(AccessibleRef {
        bus_name: bus.to_owned(),
        path: path.to_owned(),
    })
}

/// What a rescan of `bus` changes, given the windows already `known` and
/// the `current` ones just read.
struct RescanPlan {
    /// Every current window, with whether it is listed.
    current: Vec<(AccessibleRef, bool)>,
    /// Known listed windows of `bus`'s application that are no longer
    /// current.
    gone: Vec<AccessibleRef>,
    /// Known hidden windows of `bus`'s application that are no longer
    /// current.
    vanished_hidden: Vec<String>,
}

/// Diff a rescan of the application on `bus` against the `known` windows.
///
/// A window's key can name a bus other than `bus`, because
/// [`seed_top_level_windows`] keys a child under the bus the child reports.
/// So the windows a rescan can close are the ones recorded under `bus` as
/// their application ([`TrackedWindow::app_bus`]), not the ones whose key
/// starts with it, and each key is split against all of `buses` rather
/// than cut at `bus`'s length.
///
/// Every key here splits: [`seed_top_level_windows`] records only windows
/// on one of `buses`, and the dispatcher drops any signal whose sender is
/// not one of them, so no other key is ever tracked. The `filter_map`s
/// below therefore drop nothing.
fn plan_rescan(
    known: &HashMap<String, TrackedWindow>,
    current: HashMap<String, TrackedWindow>,
    bus: &str,
    buses: &[String],
) -> RescanPlan {
    let mut gone = Vec::new();
    let mut vanished_hidden = Vec::new();
    for (key, window) in known {
        if window.app_bus == bus && !current.contains_key(key) {
            if window.listed {
                gone.extend(window_ref(key, buses));
            } else {
                vanished_hidden.push(key.clone());
            }
        }
    }
    let current = current
        .into_iter()
        .filter_map(|(key, window)| Some((window_ref(&key, buses)?, window.listed)))
        .collect();
    RescanPlan {
        current,
        gone,
        vanished_hidden,
    }
}

fn window_key(aref: &AccessibleRef) -> String {
    format!("{}{}", aref.bus_name, aref.path)
}

fn is_window_role(role: Role) -> bool {
    matches!(role, Role::Window | Role::Dialog)
}

/// Whether an AT-SPI role *number* is a window or dialog (frame, window,
/// dialog, file chooser). Keyed on the number: GTK 4 names its window
/// `application` (#485).
fn is_window_role_number(role: u32) -> bool {
    matches!(role, 16 | 19 | 23 | 69)
}

/// Whether the object's parent is its application object.
///
/// Any failed read is "not top-level": this only gates deriving a window
/// event from a state change, and the subscription has no error channel to
/// report it on; the window's own `ChildrenChanged` / `defunct` still apply.
fn is_top_level(conn: &Connection, aref: &AccessibleRef) -> bool {
    let Some(proxy) = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.freedesktop.DBus.Properties",
    ) else {
        return false;
    };
    let Ok(reply) = proxy.call_method("Get", &("org.a11y.atspi.Accessible", "Parent")) else {
        return false;
    };
    let Ok(variant) = reply.body().deserialize::<OwnedValue>() else {
        return false;
    };
    let parsed: std::result::Result<(String, zbus::zvariant::OwnedObjectPath), _> =
        zbus::zvariant::Value::from(variant).try_into();
    parsed.is_ok_and(|(_, path)| path.as_str() == APP_ROOT_PATH)
}

/// Record each current top-level window of the application on `bus` as
/// listed or not, so the first transition after `subscribe` is a real one.
///
/// A peer that left, or a window destroyed mid-read, is absence; any other
/// failure fails the subscription.
///
/// A child that reports a bus outside `buses` is skipped: the subscription
/// does not listen to that bus, and the dispatcher drops its senders, so no
/// event could ever move that window, and tracking it would only leave an
/// entry that can be neither updated nor turned back into a reference.
fn seed_top_level_windows(
    conn: &Connection,
    bus: &str,
    buses: &[String],
    windows: &mut HashMap<String, TrackedWindow>,
) -> Result<()> {
    let call = |bus: &str, path: &str, method: &str| -> Result<Option<zbus::Message>> {
        let proxy = zbus::blocking::Proxy::new(
            conn,
            bus.to_owned(),
            path.to_owned(),
            "org.a11y.atspi.Accessible",
        )
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("proxy for {bus}{path}: {e}"),
        })?;
        match proxy.call_method(method, &()) {
            Ok(reply) => Ok(Some(reply)),
            Err(e) if crate::atspi::is_gone(&e) => Ok(None),
            Err(e) => Err(Error::Platform {
                code: -1,
                message: format!("{method} on {bus}{path} while seeding window states: {e}"),
            }),
        }
    };
    let Some(reply) = call(bus, APP_ROOT_PATH, "GetChildren")? else {
        return Ok(());
    };
    let children: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
        reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("GetChildren deserialize for {bus}: {e}"),
        })?;
    for (child_bus, path) in children {
        let aref = AccessibleRef {
            bus_name: if child_bus.is_empty() {
                bus.to_owned()
            } else {
                child_bus
            },
            path: path.to_string(),
        };
        if !buses.contains(&aref.bus_name) {
            log::debug!(
                "not tracking window {}: its bus is outside the subscription",
                window_key(&aref)
            );
            continue;
        }
        let Some(role) = call(&aref.bus_name, &aref.path, "GetRole")? else {
            continue;
        };
        let role: u32 = role.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("GetRole deserialize for {}: {e}", window_key(&aref)),
        })?;
        if !is_window_role_number(role) {
            continue;
        }
        let Some(state) = call(&aref.bus_name, &aref.path, "GetState")? else {
            continue;
        };
        let words: Vec<u32> = state.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("GetState deserialize for {}: {e}", window_key(&aref)),
        })?;
        windows.insert(
            window_key(&aref),
            TrackedWindow::new(is_listable_top_level(bits_from_u32s(&words)), bus),
        );
    }
    Ok(())
}

// ── Registry event registration ─────────────────────────────────────────────

/// Event classes registered with the AT-SPI registry: everything the
/// subscription maps (object, window and focus events).
const REGISTERED_EVENTS: &[&str] = &["object:", "window:", "focus:"];

/// Which `RegisterEvent` signature the running registry has.
#[derive(Clone, Copy)]
enum RegisterSignature {
    /// at-spi2-core before 2.46: `RegisterEvent(s event)`.
    EventOnly,
    /// 2.46 and later: `RegisterEvent(s event, as properties, s app_bus_name)`.
    WithProperties,
}

fn registry_proxy(conn: &Connection, iface: &str) -> Result<Proxy<'static>> {
    zbus::blocking::Proxy::new(
        conn,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/registry",
        iface.to_owned(),
    )
    .map_err(|e| Error::Platform {
        code: -1,
        message: format!("AT-SPI registry proxy: {e}"),
    })
}

/// The registry's `RegisterEvent` and `DeregisterEvent` signatures.
struct RegistrySignature {
    register: RegisterSignature,
    /// `DeregisterEvent` declares an `app_bus_name` argument after the
    /// event. at-spi2-core 2.52 declares only the event: libdbus there
    /// ignores an extra argument, but a registry that checks signatures
    /// (GDBus) rejects one, so `(ss)` is sent only when it is declared.
    deregister_with_app: bool,
}

/// Read the registry's `RegisterEvent` and `DeregisterEvent` signatures
/// from its introspection data, so each call is made in the one form the
/// registry declares rather than tried in one form and retried in the other.
fn register_signature(conn: &Connection) -> Result<RegistrySignature> {
    let xml: String = registry_proxy(conn, "org.freedesktop.DBus.Introspectable")?
        .call("Introspect", &())
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("introspecting the AT-SPI registry: {e}"),
        })?;
    register_signature_from_xml(&xml)
}

fn register_signature_from_xml(xml: &str) -> Result<RegistrySignature> {
    let register_args = in_arg_count(xml, "RegisterEvent").ok_or_else(|| Error::Platform {
        code: -1,
        message: "the AT-SPI registry has no RegisterEvent method, so toolkits that \
                  emit only for registered listeners (Qt, GTK 3) would stay silent"
            .to_string(),
    })?;
    Ok(RegistrySignature {
        register: if register_args >= 3 {
            RegisterSignature::WithProperties
        } else {
            RegisterSignature::EventOnly
        },
        // An undeclared DeregisterEvent gets the one-argument form. Every
        // registry version accepts it, but one that matches deregistrations
        // by app removes nothing with it; closing the connection is what
        // cleans those registrations up.
        deregister_with_app: in_arg_count(xml, "DeregisterEvent").is_some_and(|n| n >= 2),
    })
}

/// The number of input arguments introspection `xml` declares for
/// `method`, or `None` when it declares no such method.
fn in_arg_count(xml: &str, method: &str) -> Option<usize> {
    let start = xml.find(&format!("<method name=\"{method}\""))?;
    let body = &xml[start..];
    let declaration = &body[..body.find("</method>").unwrap_or(body.len())];
    // Method arguments are inputs unless marked `direction="out"`.
    Some(
        declaration
            .matches("<arg ")
            .count()
            .saturating_sub(declaration.matches("direction=\"out\"").count()),
    )
}

/// One registration made by [`register_events`], kept so
/// [`deregister_events`] can undo exactly it.
struct Registration {
    event: &'static str,
    /// The `app_bus_name` it was registered for (2.46+), or `None` for the
    /// one-argument form.
    app: Option<String>,
}

/// What [`register_events`] registered, and how [`deregister_events`]
/// undoes it.
struct Registrations {
    entries: Vec<Registration>,
    /// See [`RegistrySignature::deregister_with_app`].
    deregister_with_app: bool,
}

/// Register [`REGISTERED_EVENTS`] for this connection. Returns what was
/// registered, for [`deregister_events`].
///
/// On the 2.46+ signature each registration names the application's bus
/// (`app_bus_name`) rather than empty. That scoping is best-effort and up to
/// the registry: at-spi2-core still broadcasts `EventListenerRegistered` to
/// every application, and scopes only what `GetRegisteredEvents` returns,
/// so other applications may still switch their emission on.
fn register_events(conn: &Connection, app_buses: &[String]) -> Result<Registrations> {
    let signature = register_signature(conn)?;
    let registry = registry_proxy(conn, "org.a11y.atspi.Registry")?;
    let apps: Vec<Option<&str>> = match signature.register {
        RegisterSignature::EventOnly => vec![None],
        RegisterSignature::WithProperties => app_buses.iter().map(|b| Some(b.as_str())).collect(),
    };
    let mut done = Registrations {
        entries: Vec::new(),
        deregister_with_app: signature.deregister_with_app,
    };
    for app in apps {
        for event in REGISTERED_EVENTS {
            let reply = match app {
                None => registry.call_method("RegisterEvent", &(*event,)),
                Some(app) => {
                    registry.call_method("RegisterEvent", &(*event, Vec::<String>::new(), app))
                }
            };
            if let Err(e) = reply {
                deregister_events(conn, &done);
                return Err(Error::Platform {
                    code: -1,
                    message: format!(
                        "RegisterEvent({event}) for {app:?} with the AT-SPI registry: {e}"
                    ),
                });
            }
            done.entries.push(Registration {
                event,
                app: app.map(str::to_owned),
            });
        }
    }
    Ok(done)
}

/// Undo [`register_events`]. Where the registry declares
/// `DeregisterEvent(s event, s app_bus_name)`, an app-scoped registration
/// is undone with the same app, so a registry that matches by app removes
/// exactly what was registered; otherwise `DeregisterEvent(s)`, which
/// at-spi2-core 2.52 matches by sender and event alone.
fn deregister_events(conn: &Connection, registrations: &Registrations) {
    let Ok(registry) = registry_proxy(conn, "org.a11y.atspi.Registry") else {
        return;
    };
    for registration in &registrations.entries {
        // Teardown cannot report a failure, and the registry forgets a
        // closed connection's registrations on its own.
        let _ = match deregister_args(registration, registrations.deregister_with_app) {
            (event, None) => registry.call_method("DeregisterEvent", &(event,)),
            (event, Some(app)) => registry.call_method("DeregisterEvent", &(event, app)),
        };
    }
}

/// The `DeregisterEvent` arguments for `registration`: the app as well only
/// when it was registered with one and the registry declares the argument.
fn deregister_args(registration: &Registration, with_app: bool) -> (&'static str, Option<&str>) {
    let app = registration.app.as_deref().filter(|_| with_app);
    (registration.event, app)
}

// ── Signal → EventKind mapping ──────────────────────────────────────────────

/// Map an AT-SPI2 signal tuple to zero-or-more xa11y `EventKind`s.
///
/// Returns `Vec` because some AT-SPI2 signals map to multiple cross-platform
/// kinds — e.g. `Object:StateChanged(focused, true)` emits both
/// `FocusChanged` and `StateChanged{Focused, true}` (matching the Windows
/// UIA behaviour where focus changes arrive as both a dedicated focus event
/// and a property change), and `Object:ValueChanged` on a text role emits
/// `ValueChanged` + `TextChanged` (matching macOS).
pub(crate) fn signal_to_kinds(
    iface: &str,
    member: &str,
    detail: &str,
    detail1: i32,
    target_role: Option<Role>,
) -> Vec<EventKind> {
    match (iface, member) {
        // Focus interface — keyboard focus moved.
        ("org.a11y.atspi.Event.Focus", "Focus") => vec![EventKind::FocusChanged],

        // Object.StateChanged — one signal per state bit. `detail` is the
        // state name, `detail1` is 0/1.
        ("org.a11y.atspi.Event.Object", "StateChanged") => {
            let value = detail1 != 0;
            let lower = detail.to_ascii_lowercase();
            match lower.as_str() {
                "focused" => {
                    // Some toolkits (notably GTK4) skip `Focus:Focus` and
                    // only emit StateChanged(focused). Raise both so
                    // consumers can filter on either.
                    if value {
                        vec![
                            EventKind::FocusChanged,
                            EventKind::StateChanged {
                                flag: StateFlag::Focused,
                                value,
                            },
                        ]
                    } else {
                        vec![EventKind::StateChanged {
                            flag: StateFlag::Focused,
                            value,
                        }]
                    }
                }
                "checked" => vec![EventKind::StateChanged {
                    flag: StateFlag::Checked,
                    value,
                }],
                // AT-SPI exposes both `enabled` and `sensitive`; xa11y
                // collapses them onto `Enabled` so consumers don't have to
                // deal with the (historical) distinction.
                "enabled" | "sensitive" => vec![EventKind::StateChanged {
                    flag: StateFlag::Enabled,
                    value,
                }],
                "visible" | "showing" => vec![EventKind::StateChanged {
                    flag: StateFlag::Visible,
                    value,
                }],
                "expanded" => vec![EventKind::StateChanged {
                    flag: StateFlag::Expanded,
                    value,
                }],
                "selected" => vec![EventKind::StateChanged {
                    flag: StateFlag::Selected,
                    value,
                }],
                "busy" => vec![EventKind::StateChanged {
                    flag: StateFlag::Busy,
                    value,
                }],
                "editable" => vec![EventKind::StateChanged {
                    flag: StateFlag::Editable,
                    value,
                }],
                "focusable" => vec![EventKind::StateChanged {
                    flag: StateFlag::Focusable,
                    value,
                }],
                "modal" => vec![EventKind::StateChanged {
                    flag: StateFlag::Modal,
                    value,
                }],
                "required" => vec![EventKind::StateChanged {
                    flag: StateFlag::Required,
                    value,
                }],
                // The AT-SPI state for "this window was iconified/restored".
                // Not emitted by every toolkit, but wiring it here is the only
                // way a subscriber learns of a window minimization — without
                // it the public StateFlag::Minimized is dead on Linux.
                "iconified" => vec![EventKind::StateChanged {
                    flag: StateFlag::Minimized,
                    value,
                }],
                _ => vec![],
            }
        }

        // Object.PropertyChange — AccessKit's AT-SPI bridge publishes both
        // name changes and numeric value changes through this signal with
        // `detail` set to the AT-SPI property id (`accessible-name`,
        // `accessible-value`, `accessible-description`, `accessible-parent`,
        // `accessible-role`). The design doc maps:
        // - name  → NameChanged
        // - value → ValueChanged (+ TextChanged on text roles, mirroring macOS)
        // - description/parent/role → no cross-platform EventKind; drop.
        ("org.a11y.atspi.Event.Object", "PropertyChange") => {
            let d = detail.to_ascii_lowercase();
            if d == "accessible-name" || d == "name" {
                vec![EventKind::NameChanged]
            } else if d == "accessible-value" || d == "value" {
                let mut kinds = vec![EventKind::ValueChanged];
                if matches!(target_role, Some(Role::TextField | Role::TextArea)) {
                    kinds.push(EventKind::TextChanged);
                }
                kinds
            } else {
                vec![]
            }
        }

        ("org.a11y.atspi.Event.Object", "ChildrenChanged") => vec![EventKind::StructureChanged],

        // Text changes. AT-SPI2 publishes text-change events on the Object
        // event bus (both "TextChanged" in newer specs and historical
        // "TextChanged" via an application-specific signal). `detail` is
        // "insert" or "delete".
        ("org.a11y.atspi.Event.Object", "TextChanged") => vec![EventKind::TextChanged],
        ("org.a11y.atspi.Event.Object", "TextSelectionChanged") => {
            vec![EventKind::SelectionChanged]
        }
        ("org.a11y.atspi.Event.Object", "TextAttributesChanged") => vec![],

        ("org.a11y.atspi.Event.Object", "SelectionChanged") => vec![EventKind::SelectionChanged],
        ("org.a11y.atspi.Event.Object", "ActiveDescendantChanged") => {
            vec![EventKind::SelectionChanged]
        }

        // Object.ValueChanged: AccessKit's AT-SPI bridge publishes numeric
        // value changes here (the spec moved this to Value:ValueChanged
        // eventually, but both shapes appear in the wild). Treat as
        // ValueChanged, and also emit TextChanged when the role is a text
        // role so the macOS/Windows text-field pattern survives.
        ("org.a11y.atspi.Event.Object", "ValueChanged") => {
            let mut kinds = vec![EventKind::ValueChanged];
            if matches!(target_role, Some(Role::TextField | Role::TextArea)) {
                kinds.push(EventKind::TextChanged);
            }
            kinds
        }

        ("org.a11y.atspi.Event.Object", "Announcement") => vec![EventKind::Announcement],

        // BoundsChanged / VisibleDataChanged / AttributesChanged — no cross
        // platform EventKind; skip.
        (
            "org.a11y.atspi.Event.Object",
            "BoundsChanged" | "VisibleDataChanged" | "AttributesChanged" | "ModelChanged"
            | "ColumnReordered" | "RowReordered" | "ColumnInserted" | "RowInserted"
            | "ColumnDeleted" | "RowDeleted",
        ) => vec![],

        // Window interface.
        ("org.a11y.atspi.Event.Window", "Create") => vec![EventKind::WindowOpened],
        ("org.a11y.atspi.Event.Window", "Destroy") => vec![EventKind::WindowClosed],
        ("org.a11y.atspi.Event.Window", "Activate") => vec![EventKind::WindowActivated],
        ("org.a11y.atspi.Event.Window", "Deactivate") => vec![EventKind::WindowDeactivated],
        ("org.a11y.atspi.Event.Window", "Minimize") => vec![EventKind::WindowDeactivated],
        ("org.a11y.atspi.Event.Window", "Restore") => vec![EventKind::WindowActivated],
        // Move/Resize/Maximize/Raise/Lower have no cross-platform kind.
        ("org.a11y.atspi.Event.Window", _) => vec![],

        // Document load events — not modelled cross-platform.
        ("org.a11y.atspi.Event.Document", _) => vec![],

        _ => vec![],
    }
}

// ── Snapshot helpers (free functions, independent of LinuxProvider) ─────────

/// Build a best-effort `ElementData` snapshot for an event's source element.
///
/// Returns `None` only on a catastrophic D-Bus failure — role/name/value
/// fetches are individually fallible and the snapshot survives their
/// absence (the element may have been destroyed just before we queried it).
fn build_event_snapshot(
    conn: &Connection,
    aref: &AccessibleRef,
    pid: Option<u32>,
) -> Option<ElementData> {
    let role_name = get_role_name(conn, aref).unwrap_or_default();
    let role_num = if role_name.is_empty() {
        get_role_number(conn, aref).unwrap_or(0)
    } else {
        0
    };
    let mut role = if !role_name.is_empty() {
        let r = map_atspi_role(&role_name);
        if r == Role::Unknown {
            let n = get_role_number(conn, aref).unwrap_or(0);
            map_atspi_role_number(n)
        } else {
            r
        }
    } else {
        map_atspi_role_number(role_num)
    };
    // `role_num` is read only when the name is empty; a named role is
    // decided by its name, which `is_password_role` also accepts.
    let password = is_password_role(&role_name, role_num);

    // State bits (used both for role refinement and for StateSet).
    // A failed GetState is `None` — preserve that as `state_read_ok = false`
    // rather than collapsing it into an empty bitset: an unread state is not
    // a state read as absent (same rule `LinuxProvider::parse_states_with_bits`
    // applies via `state_bits`), and `states_from_bits` needs it to leave
    // `minimized` unknown instead of reporting `Some(false)`.
    let (bits, state_read_ok) = match get_state(conn, aref) {
        Some(state_bits) => (bits_from_u32s(&state_bits), true),
        None => (0, false),
    };

    // Refine TextArea → TextField for single-line text widgets (mirrors
    // the role-resolution logic on the main query path).
    if role == Role::TextArea {
        const MULTI_LINE: u64 = 1 << 17;
        if (bits & MULTI_LINE) == 0 {
            role = Role::TextField;
        }
    }

    // Gates the Text / Value reads below; see `Interfaces` for why an
    // unreported interface must not be read. A failed read gates them off,
    // like every other best-effort field of this snapshot.
    let ifaces = get_interfaces(conn, aref).unwrap_or_default();

    let name = get_name(conn, aref).filter(|s| !s.is_empty());
    let value = get_value(conn, aref, role, ifaces);

    let (numeric_value, min_value, max_value) = if ifaces.value
        && matches!(
            role,
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::SpinButton
        ) {
        get_numeric(conn, aref)
    } else {
        (None, None, None)
    };

    let states = states_from_bits(bits, role, state_read_ok, password);

    let raw = {
        let raw_role = if role_name.is_empty() {
            format!("role_num:{}", role_num)
        } else {
            role_name
        };
        let mut raw = HashMap::new();
        raw.insert("atspi_role".into(), serde_json::Value::String(raw_role));
        raw.insert(
            "bus_name".into(),
            serde_json::Value::String(aref.bus_name.clone()),
        );
        raw.insert(
            "object_path".into(),
            serde_json::Value::String(aref.path.clone()),
        );
        raw
    };

    Some(
        ElementParts {
            role,
            name,
            value,
            description: None,
            bounds: None,
            actions: vec![],
            states,
            numeric_value,
            min_value,
            max_value,
            stable_id: Some(aref.path.clone()),
            pid,
            raw,
            // Snapshots are read-only targets, not live handles into the main
            // provider's cache. Consumers wanting to drive actions must
            // re-resolve through the regular locator path.
            handle: 0,
        }
        .into(),
    )
}

fn make_proxy<'a>(conn: &'a Connection, bus: &str, path: &str, iface: &str) -> Option<Proxy<'a>> {
    zbus::blocking::proxy::Builder::<Proxy>::new(conn)
        .destination(bus.to_owned())
        .ok()?
        .path(path.to_owned())
        .ok()?
        .interface(iface.to_owned())
        .ok()?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .ok()
}

fn get_role_name(conn: &Connection, aref: &AccessibleRef) -> Option<String> {
    let proxy = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.a11y.atspi.Accessible",
    )?;
    let reply = proxy.call_method("GetRoleName", &()).ok()?;
    reply.body().deserialize::<String>().ok()
}

fn get_role_number(conn: &Connection, aref: &AccessibleRef) -> Option<u32> {
    let proxy = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.a11y.atspi.Accessible",
    )?;
    let reply = proxy.call_method("GetRole", &()).ok()?;
    reply.body().deserialize::<u32>().ok()
}

fn get_name(conn: &Connection, aref: &AccessibleRef) -> Option<String> {
    let proxy = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.a11y.atspi.Accessible",
    )?;
    proxy.get_property::<String>("Name").ok()
}

fn get_state(conn: &Connection, aref: &AccessibleRef) -> Option<Vec<u32>> {
    let proxy = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.a11y.atspi.Accessible",
    )?;
    let reply = proxy.call_method("GetState", &()).ok()?;
    reply.body().deserialize::<Vec<u32>>().ok()
}

fn get_interfaces(conn: &Connection, aref: &AccessibleRef) -> Option<Interfaces> {
    let proxy = make_proxy(
        conn,
        &aref.bus_name,
        &aref.path,
        "org.a11y.atspi.Accessible",
    )?;
    read_interfaces(&proxy).ok()
}

fn get_value(
    conn: &Connection,
    aref: &AccessibleRef,
    role: Role,
    ifaces: Interfaces,
) -> Option<String> {
    if matches!(
        role,
        Role::Application
            | Role::Window
            | Role::Dialog
            | Role::Group
            | Role::MenuBar
            | Role::Toolbar
            | Role::TabGroup
            | Role::SplitGroup
            | Role::Table
            | Role::TableRow
            | Role::Separator
    ) {
        return None;
    }

    // Prefer Text.GetText for roles that carry text (text fields, labels,
    // combo boxes) — Value.CurrentValue returns 0.0 for text elements on
    // some AT-SPI adapters. Only interfaces the node reports are read.
    let proxy_for = |reported: bool, iface: &str| {
        reported
            .then(|| make_proxy(conn, &aref.bus_name, &aref.path, iface))
            .flatten()
    };
    if let Some(proxy) = proxy_for(ifaces.text, "org.a11y.atspi.Text") {
        if let Ok(char_count) = proxy.get_property::<i32>("CharacterCount") {
            if char_count > 0 {
                if let Ok(reply) = proxy.call_method("GetText", &(0i32, char_count)) {
                    if let Ok(text) = reply.body().deserialize::<String>() {
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
        }
    }

    if let Some(proxy) = proxy_for(ifaces.value, "org.a11y.atspi.Value") {
        if let Ok(v) = proxy.get_property::<f64>("CurrentValue") {
            return Some(v.to_string());
        }
    }

    None
}

fn get_numeric(conn: &Connection, aref: &AccessibleRef) -> (Option<f64>, Option<f64>, Option<f64>) {
    let Some(proxy) = make_proxy(conn, &aref.bus_name, &aref.path, "org.a11y.atspi.Value") else {
        return (None, None, None);
    };
    (
        proxy.get_property::<f64>("CurrentValue").ok(),
        proxy.get_property::<f64>("MinimumValue").ok(),
        proxy.get_property::<f64>("MaximumValue").ok(),
    )
}

/// AT-SPI2 packs its state bitfield into a `Vec<u32>` of length 2 (low and
/// high halves). Collapse into a single u64 for easier bit-testing.
fn bits_from_u32s(state_bits: &[u32]) -> u64 {
    match state_bits.len() {
        0 => 0,
        1 => state_bits[0] as u64,
        _ => (state_bits[0] as u64) | ((state_bits[1] as u64) << 32),
    }
}

/// Convert an AT-SPI2 state bitfield into a [`StateSet`].
///
/// `state_read_ok` reports whether the bits came from a successful `GetState`
/// read. When it is `false` the read produced no evidence, and the window
/// flags must stay tri-state-unknown (`None`) rather than read-as-absent —
/// the same rule `LinuxProvider::parse_states_with_bits` applies through
/// `state_bits`, kept in sync here.
///
/// `password` comes from the raw role, not the bits (see
/// `is_password_role`).
pub(crate) fn states_from_bits(
    bits: u64,
    role: Role,
    state_read_ok: bool,
    password: bool,
) -> StateSet {
    // Same bit positions as `LinuxProvider::parse_states` — kept in sync.
    // Each constant is `1 << ordinal` of the `AtspiStateType` enum in
    // at-spi2-core's `atspi/atspi-constants.h` (e.g. ICONIFIED=15,
    // MODAL=16, VISIBLE=30); the state set arrives packed as one bit per
    // state ordinal.
    const ACTIVE: u64 = 1 << 1;
    const BUSY: u64 = 1 << 3;
    const CHECKED: u64 = 1 << 4;
    const EDITABLE: u64 = 1 << 7;
    const ENABLED: u64 = 1 << 8;
    const EXPANDABLE: u64 = 1 << 9;
    const EXPANDED: u64 = 1 << 10;
    const FOCUSABLE: u64 = 1 << 11;
    const FOCUSED: u64 = 1 << 12;
    const ICONIFIED: u64 = 1 << 15;
    const MODAL: u64 = 1 << 16;
    const SELECTED: u64 = 1 << 23;
    const SENSITIVE: u64 = 1 << 24;
    const INDETERMINATE: u64 = 1 << 32;
    const REQUIRED: u64 = 1 << 33;

    let enabled = (bits & ENABLED) != 0 || (bits & SENSITIVE) != 0;
    let visible = crate::atspi::atspi_visible(bits);

    let checked = match role {
        Role::CheckBox | Role::RadioButton | Role::MenuItem | Role::Switch => {
            if (bits & INDETERMINATE) != 0 {
                Some(Toggled::Mixed)
            } else if (bits & CHECKED) != 0 {
                Some(Toggled::On)
            } else {
                Some(Toggled::Off)
            }
        }
        _ => None,
    };

    let expanded = if (bits & EXPANDABLE) != 0 {
        Some((bits & EXPANDED) != 0)
    } else {
        None
    };

    StateParts {
        enabled,
        visible,
        focused: (bits & FOCUSED) != 0,
        // AT-SPI `ACTIVE` = foreground window/frame; kept in sync with
        // `LinuxProvider::parse_states`.
        active: (bits & ACTIVE) != 0,
        checked,
        selected: (bits & SELECTED) != 0,
        expanded,
        editable: (bits & EDITABLE) != 0,
        focusable: (bits & FOCUSABLE) != 0,
        modal: (bits & MODAL) != 0,
        required: (bits & REQUIRED) != 0,
        busy: (bits & BUSY) != 0,
        minimized: if matches!(role, Role::Window | Role::Dialog) {
            if state_read_ok {
                Some((bits & ICONIFIED) != 0)
            } else {
                // Could not read the state: say so, don't misreport the
                // window as not-minimized (mirrors `parse_states_with_bits`).
                None
            }
        } else {
            None
        },
        maximized: None,
        fullscreen: None,
        password,
    }
    .into()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_signal_emits_focus_changed() {
        let kinds = signal_to_kinds("org.a11y.atspi.Event.Focus", "Focus", "", 0, None);
        assert_eq!(kinds, vec![EventKind::FocusChanged]);
    }

    #[test]
    fn state_changed_focused_true_emits_both_kinds() {
        // GTK4 skips Focus:Focus sometimes — `focused=true` StateChanged
        // must still raise FocusChanged so consumers don't miss it.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "StateChanged",
            "focused",
            1,
            None,
        );
        assert!(kinds.contains(&EventKind::FocusChanged));
        assert!(kinds.iter().any(|k| matches!(
            k,
            EventKind::StateChanged {
                flag: StateFlag::Focused,
                value: true
            }
        )));
    }

    #[test]
    fn state_changed_focused_false_is_state_only() {
        // Losing focus shouldn't fire FocusChanged — that's reserved for
        // the element that gained focus.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "StateChanged",
            "focused",
            0,
            None,
        );
        assert_eq!(
            kinds,
            vec![EventKind::StateChanged {
                flag: StateFlag::Focused,
                value: false
            }]
        );
    }

    #[test]
    fn state_changed_checked_maps_value() {
        let on = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "StateChanged",
            "checked",
            1,
            None,
        );
        assert_eq!(
            on,
            vec![EventKind::StateChanged {
                flag: StateFlag::Checked,
                value: true
            }]
        );
        let off = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "StateChanged",
            "checked",
            0,
            None,
        );
        assert_eq!(
            off,
            vec![EventKind::StateChanged {
                flag: StateFlag::Checked,
                value: false
            }]
        );
    }

    #[test]
    fn state_changed_enabled_and_sensitive_collapse() {
        // AT-SPI2 exposes both `enabled` and `sensitive`; xa11y's cross
        // platform model collapses them onto `Enabled`.
        for name in ["enabled", "sensitive"] {
            let kinds =
                signal_to_kinds("org.a11y.atspi.Event.Object", "StateChanged", name, 1, None);
            assert_eq!(
                kinds,
                vec![EventKind::StateChanged {
                    flag: StateFlag::Enabled,
                    value: true
                }],
                "state '{name}' should map to Enabled"
            );
        }
    }

    #[test]
    fn state_changed_iconified_maps_to_minimized() {
        // Wiring Object:StateChanged(iconified) is the only thing that makes
        // the public StateFlag::Minimized observable on Linux — without it
        // the flag would only ever surface as a snapshot, never as an event.
        for value in [0, 1] {
            let kinds = signal_to_kinds(
                "org.a11y.atspi.Event.Object",
                "StateChanged",
                "iconified",
                value,
                None,
            );
            assert_eq!(
                kinds,
                vec![EventKind::StateChanged {
                    flag: StateFlag::Minimized,
                    value: value != 0,
                }],
                "iconified={value}"
            );
        }
    }

    #[test]
    fn state_changed_unknown_state_is_dropped() {
        // Unknown AT-SPI state names must not synthesize events.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "StateChanged",
            "expandable",
            1,
            None,
        );
        assert!(kinds.is_empty());
    }

    #[test]
    fn children_changed_emits_structure_changed() {
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "ChildrenChanged",
            "add",
            0,
            None,
        );
        assert_eq!(kinds, vec![EventKind::StructureChanged]);
    }

    #[test]
    fn property_change_name_emits_name_changed() {
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "PropertyChange",
            "accessible-name",
            0,
            None,
        );
        assert_eq!(kinds, vec![EventKind::NameChanged]);
    }

    #[test]
    fn property_change_description_is_dropped() {
        // Description changes don't map to any cross-platform event kind.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "PropertyChange",
            "accessible-description",
            0,
            None,
        );
        assert!(kinds.is_empty());
    }

    #[test]
    fn property_change_value_emits_value_changed() {
        // AccessKit's AT-SPI bridge emits Object:PropertyChange with
        // detail="accessible-value" for slider/range value changes.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "PropertyChange",
            "accessible-value",
            0,
            Some(Role::Slider),
        );
        assert_eq!(kinds, vec![EventKind::ValueChanged]);
    }

    #[test]
    fn property_change_value_on_text_role_also_emits_text_changed() {
        // If a text-role element's value changes via PropertyChange, the
        // provider synthesizes TextChanged too so the macOS/Windows text
        // pattern carries over to Linux.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "PropertyChange",
            "accessible-value",
            0,
            Some(Role::TextField),
        );
        assert!(kinds.contains(&EventKind::ValueChanged));
        assert!(kinds.contains(&EventKind::TextChanged));
    }

    #[test]
    fn text_changed_maps_directly() {
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "TextChanged",
            "insert",
            0,
            None,
        );
        assert_eq!(kinds, vec![EventKind::TextChanged]);
    }

    #[test]
    fn value_changed_on_text_role_also_emits_text_changed() {
        // AccessKit's AT-SPI bridge publishes text mutations via
        // Object:ValueChanged; the events module must synthesize the
        // cross-platform TextChanged signal too so consumers match.
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "ValueChanged",
            "",
            0,
            Some(Role::TextField),
        );
        assert!(kinds.contains(&EventKind::ValueChanged));
        assert!(kinds.contains(&EventKind::TextChanged));

        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "ValueChanged",
            "",
            0,
            Some(Role::TextArea),
        );
        assert!(kinds.contains(&EventKind::TextChanged));
    }

    #[test]
    fn value_changed_on_slider_is_value_only() {
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "ValueChanged",
            "",
            0,
            Some(Role::Slider),
        );
        assert_eq!(kinds, vec![EventKind::ValueChanged]);
    }

    // ── #488: registration and derived window lifecycle ──────────────

    #[test]
    fn register_signature_follows_the_registry_introspection() {
        let old = r#"<node><interface name="org.a11y.atspi.Registry">
            <method name="RegisterEvent"><arg direction="in" name="event" type="s"/></method>
            <method name="DeregisterEvent"><arg direction="in" name="event" type="s"/></method>
            </interface></node>"#;
        assert!(matches!(
            register_signature_from_xml(old),
            Ok(RegistrySignature {
                register: RegisterSignature::EventOnly,
                deregister_with_app: false,
            })
        ));
        let new = r#"<node><interface name="org.a11y.atspi.Registry">
            <method name="RegisterEvent">
              <arg direction="in" name="event" type="s"/>
              <arg direction="in" name="properties" type="as"/>
              <arg direction="in" name="app_bus_name" type="s"/>
            </method>
            <method name="DeregisterEvent"><arg direction="in" name="event" type="s"/></method>
            <method name="GetRegisteredEvents"><arg direction="out" type="a(ss)"/></method>
            </interface></node>"#;
        // at-spi2-core 2.52: RegisterEvent takes the app, DeregisterEvent
        // declares only the event, so deregistration sends `(s)`.
        assert!(matches!(
            register_signature_from_xml(new),
            Ok(RegistrySignature {
                register: RegisterSignature::WithProperties,
                deregister_with_app: false,
            })
        ));
        // A registry that declares DeregisterEvent's app argument gets it.
        let with_app = new.replace(
            r#"<method name="DeregisterEvent"><arg direction="in" name="event" type="s"/></method>"#,
            r#"<method name="DeregisterEvent">
              <arg direction="in" name="event" type="s"/>
              <arg direction="in" name="app_bus_name" type="s"/>
            </method>"#,
        );
        assert!(matches!(
            register_signature_from_xml(&with_app),
            Ok(RegistrySignature {
                register: RegisterSignature::WithProperties,
                deregister_with_app: true,
            })
        ));
        // No DeregisterEvent declared: the one-argument form.
        let undeclared = new.replace(
            r#"<method name="DeregisterEvent"><arg direction="in" name="event" type="s"/></method>"#,
            "",
        );
        assert!(matches!(
            register_signature_from_xml(&undeclared),
            Ok(RegistrySignature {
                deregister_with_app: false,
                ..
            })
        ));
        // No RegisterEvent at all is an error naming the consequence, not a
        // subscription that silently hears nothing from Qt.
        assert!(matches!(
            register_signature_from_xml("<node/>"),
            Err(Error::Platform { message, .. }) if message.contains("RegisterEvent")
        ));
    }

    #[test]
    fn window_transitions_fire_once_per_change() {
        let mut windows = HashMap::new();
        // A new window opens once.
        assert!(note_window(&mut windows, "a".into(), ":1.5", true));
        // GTK 4 ChildrenChanged:add and a later showing re-read agree: once.
        assert!(!note_window(&mut windows, "a".into(), ":1.5", true));
        assert!(note_window(&mut windows, "a".into(), ":1.5", false));
        // Defunct then Window:Destroy: no second close, and no entry left.
        assert!(!note_window(&mut windows, "a".into(), ":1.5", false));
        assert!(windows.is_empty());
        // A close for a window never seen open says nothing.
        assert!(!note_window(&mut windows, "c".into(), ":1.5", false));
        // A seeded hidden window (Qt's dialog before it is first shown)
        // keeps the application it was seeded under when it opens.
        windows.insert("b".into(), TrackedWindow::new(false, ":1.5"));
        assert!(note_window(&mut windows, "b".into(), ":1.12", true));
        assert_eq!(windows["b"], TrackedWindow::new(true, ":1.5"));
    }

    #[test]
    fn window_keys_split_back_into_bus_and_path() {
        let buses = vec![":1.1".to_string(), ":1.12".to_string()];
        assert_eq!(
            split_window_key(":1.12/org/a11y/atspi/accessible/3", &buses),
            Some((":1.12", "/org/a11y/atspi/accessible/3"))
        );
        assert_eq!(split_window_key(":1.1/x", &buses), Some((":1.1", "/x")));
        assert_eq!(split_window_key(":1.9/x", &buses), None);
    }

    fn tracked(entries: &[(&str, bool, &str)]) -> HashMap<String, TrackedWindow> {
        entries
            .iter()
            .map(|(key, listed, app)| (key.to_string(), TrackedWindow::new(*listed, app)))
            .collect()
    }

    fn current_refs(plan: &RescanPlan) -> Vec<(&str, &str, bool)> {
        let mut current: Vec<_> = plan
            .current
            .iter()
            .map(|(r, listed)| (r.bus_name.as_str(), r.path.as_str(), *listed))
            .collect();
        current.sort();
        current
    }

    fn gone_refs(plan: &RescanPlan) -> Vec<(&str, &str)> {
        let mut gone: Vec<_> = plan
            .gone
            .iter()
            .map(|r| (r.bus_name.as_str(), r.path.as_str()))
            .collect();
        gone.sort();
        gone
    }

    #[test]
    fn rescan_keeps_each_window_on_its_own_bus() {
        let buses = vec![":1.5".to_string(), ":1.12".to_string()];
        let known = tracked(&[
            (":1.5/w/listed", true, ":1.5"),
            (":1.5/w/hidden", false, ":1.5"),
            (":1.5/w/still", true, ":1.5"),
            // Another application's window is not this rescan's to close.
            (":1.12/w/other", true, ":1.12"),
        ]);
        let current = tracked(&[
            (":1.5/w/still", true, ":1.5"),
            // Read from :1.5 but keyed under the bus the child reported.
            // Cutting at ":1.5".len() would give the path "2/w/child".
            (":1.12/w/child", false, ":1.5"),
        ]);
        let plan = plan_rescan(&known, current, ":1.5", &buses);
        assert_eq!(
            current_refs(&plan),
            vec![(":1.12", "/w/child", false), (":1.5", "/w/still", true)]
        );
        assert_eq!(gone_refs(&plan), vec![(":1.5", "/w/listed")]);
        assert_eq!(plan.vanished_hidden, vec![":1.5/w/hidden".to_string()]);
    }

    #[test]
    fn rescan_closes_a_cross_bus_window_from_its_own_application_only() {
        let buses = vec![":1.5".to_string(), ":1.12".to_string()];
        // :1.5's window keyed under :1.12, alongside :1.12's own window.
        let known = tracked(&[
            (":1.12/w/child", true, ":1.5"),
            (":1.12/w/own", true, ":1.12"),
        ]);

        // :1.12's rescan, still listing its own window, must not close
        // :1.5's (it used to, and :1.5's next rescan reopened it).
        let plan = plan_rescan(
            &known,
            tracked(&[(":1.12/w/own", true, ":1.12")]),
            ":1.12",
            &buses,
        );
        assert_eq!(gone_refs(&plan), Vec::<(&str, &str)>::new());

        // :1.5's rescan, without the child, does close it (it used to stay
        // listed, because its key does not start with ":1.5").
        let plan = plan_rescan(&known, HashMap::new(), ":1.5", &buses);
        assert_eq!(gone_refs(&plan), vec![(":1.12", "/w/child")]);
    }

    #[test]
    fn deregistration_names_the_app_only_where_declared() {
        let scoped = Registration {
            event: "object:",
            app: Some(":1.5".to_string()),
        };
        let unscoped = Registration {
            event: "window:",
            app: None,
        };
        assert_eq!(deregister_args(&scoped, true), ("object:", Some(":1.5")));
        // at-spi2-core 2.52: registered with the app, deregistered with `(s)`.
        assert_eq!(deregister_args(&scoped, false), ("object:", None));
        assert_eq!(deregister_args(&unscoped, true), ("window:", None));
        assert_eq!(deregister_args(&unscoped, false), ("window:", None));
    }

    #[test]
    fn window_role_numbers_are_frame_window_dialog_and_file_chooser() {
        for n in [16, 19, 23, 69] {
            assert!(is_window_role_number(n), "{n}");
        }
        // The application object (GTK 4 *names* its window "application").
        assert!(!is_window_role_number(75));
        assert!(!is_window_role_number(43));
    }

    #[test]
    fn window_signals_map_to_window_kinds() {
        let cases = [
            ("Create", EventKind::WindowOpened),
            ("Destroy", EventKind::WindowClosed),
            ("Activate", EventKind::WindowActivated),
            ("Deactivate", EventKind::WindowDeactivated),
            ("Minimize", EventKind::WindowDeactivated),
            ("Restore", EventKind::WindowActivated),
        ];
        for (member, expected) in cases {
            let kinds = signal_to_kinds("org.a11y.atspi.Event.Window", member, "", 0, None);
            assert_eq!(
                kinds,
                vec![expected.clone()],
                "window member '{member}' should map to {expected:?}",
            );
        }
    }

    #[test]
    fn selection_changed_maps_to_selection() {
        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "SelectionChanged",
            "",
            0,
            None,
        );
        assert_eq!(kinds, vec![EventKind::SelectionChanged]);

        let kinds = signal_to_kinds(
            "org.a11y.atspi.Event.Object",
            "TextSelectionChanged",
            "",
            0,
            None,
        );
        assert_eq!(kinds, vec![EventKind::SelectionChanged]);
    }

    #[test]
    fn announcement_maps_to_announcement() {
        let kinds = signal_to_kinds("org.a11y.atspi.Event.Object", "Announcement", "", 0, None);
        assert_eq!(kinds, vec![EventKind::Announcement]);
    }

    #[test]
    fn unrecognised_signals_are_dropped() {
        // Interfaces/members that don't map to any EventKind must yield an
        // empty vec so we don't flood consumers with no-op events.
        assert!(signal_to_kinds("org.a11y.atspi.Event.Window", "Move", "", 0, None).is_empty());
        assert!(
            signal_to_kinds("org.a11y.atspi.Event.Object", "BoundsChanged", "", 0, None).is_empty()
        );
        assert!(signal_to_kinds("com.example.OtherBus", "Whatever", "", 0, None).is_empty());
    }

    #[test]
    fn event_interfaces_covers_design_doc() {
        // The design doc mandates that we subscribe to exactly these four
        // AT-SPI2 event interfaces — a future refactor that drops one
        // silently should fail this test.
        assert!(EVENT_INTERFACES.contains(&"org.a11y.atspi.Event.Object"));
        assert!(EVENT_INTERFACES.contains(&"org.a11y.atspi.Event.Window"));
        assert!(EVENT_INTERFACES.contains(&"org.a11y.atspi.Event.Focus"));
        assert!(EVENT_INTERFACES.contains(&"org.a11y.atspi.Event.Document"));
    }

    #[test]
    fn bits_from_u32s_handles_short_and_long_arrays() {
        assert_eq!(bits_from_u32s(&[]), 0);
        assert_eq!(bits_from_u32s(&[0x1234]), 0x1234);
        // The u32[1] is the high 32 bits.
        assert_eq!(bits_from_u32s(&[0x11, 0x22]), 0x22_0000_0011u64);
    }

    #[test]
    fn states_from_bits_checked_requires_toggleable_role() {
        // `Checked` only makes sense on checkbox-like roles — other roles
        // should leave it as None even when the bit is set.
        const CHECKED: u64 = 1 << 4;
        let s = states_from_bits(CHECKED, Role::Button, true, false);
        assert!(s.checked.is_none());
        let s = states_from_bits(CHECKED, Role::CheckBox, true, false);
        assert_eq!(s.checked, Some(Toggled::On));
    }

    #[test]
    fn states_from_bits_collapses_enabled_sensitive() {
        const ENABLED: u64 = 1 << 8;
        const SENSITIVE: u64 = 1 << 24;
        // Either bit on its own should flip the enabled flag.
        assert!(states_from_bits(ENABLED, Role::Button, true, false).enabled);
        assert!(states_from_bits(SENSITIVE, Role::Button, true, false).enabled);
        assert!(!states_from_bits(0, Role::Button, true, false).enabled);
    }

    #[test]
    fn states_from_bits_leaves_minimized_unknown_when_read_failed() {
        // A failed GetState is not evidence the window is not minimized:
        // `state_read_ok = false` must leave `minimized` as None (tri-state
        // contract, mirrored with `parse_states_with_bits`), not report
        // `Some(false)` from the defaulted zero bitset.
        const ICONIFIED: u64 = 1 << 15;
        let read_ok = states_from_bits(ICONIFIED, Role::Window, true, false);
        assert_eq!(read_ok.minimized, Some(true));
        let read_ok = states_from_bits(0, Role::Window, true, false);
        assert_eq!(read_ok.minimized, Some(false));
        let failed = states_from_bits(0, Role::Window, false, false);
        assert_eq!(failed.minimized, None, "unread state must stay unknown");
        // Role guard is unchanged: a non-window role stays None regardless.
        let failed = states_from_bits(0, Role::Group, false, false);
        assert_eq!(failed.minimized, None);
    }
}
