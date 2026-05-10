//! macOS-only event-subscription end-to-end tests.
//!
//! The parent `mod events_macos` declaration in `integ/mod.rs` is gated
//! with `#[cfg(target_os = "macos")]`, so tests in this file don't need
//! per-item `#[cfg]` attributes.
//!
//! These tests exercise the native event subscription API against the
//! AccessKit test app on macOS. Linux and Windows providers currently
//! ship with inert subscription stubs (no events are ever delivered)
//! per the events design doc — their backends will be implemented
//! separately.
//!
//! AccessKit's macOS bridge (accesskit_macos::EventGenerator) reliably
//! emits the following notifications, which the xa11y-macos provider
//! maps to EventKind variants:
//!
//!     Cocoa notification             → xa11y EventKind
//!     AXFocusedUIElementChanged      → FocusChanged
//!     AXValueChanged                 → ValueChanged (+ TextChanged for
//!                                       text roles, + StateChanged{Checked}
//!                                       for checkbox/radio roles)
//!     AXTitleChanged                 → NameChanged
//!     AXUIElementDestroyed           → StructureChanged
//!     AXSelectedTextChanged          → SelectionChanged
//!     AXAnnouncementRequested        → Announcement (live-region updates)
//!
//! AccessKit's macOS bridge does NOT emit the following — they require
//! native NSMenu/NSWindow behavior that the AccessKit adapter does not
//! synthesize. The macOS provider still subscribes to them, so they
//! will propagate correctly when a non-AccessKit app raises them, but
//! the AccessKit test app cannot drive e2e coverage for them today:
//!
//!     MenuOpened / MenuClosed              (NSMenu only)
//!     WindowOpened / WindowClosed          (multi-window required)
//!     WindowActivated / WindowDeactivated  (key-window change required)
//!     StateChanged { Busy } (and other flags not backed by value-changes)
//!
//! IMPORTANT: these tests MUST fail on timeout. A previous iteration
//! caught `Error::Timeout` and logged a "may depend on platform"
//! message while reporting success, hiding real regressions. Do not
//! reintroduce that pattern.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    fn find_name_field(app: &App) -> Element {
        app.locator(r#"[name="Name"]"#)
            .elements()
            .unwrap_or_default()
            .into_iter()
            .next()
            .expect("Name text field not found in test app")
    }

    fn ensure_checkbox(app: &App, want_on: bool) {
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found");
        let is_on = chk.states.checked == Some(Toggled::On);
        if is_on != want_on {
            chk.toggle().expect("toggle failed");
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    // ── Subscription mechanics ──

    #[test]
    #[ignore]
    fn event_try_recv_returns_none_when_idle() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        // Drain anything that may have been queued during subscription setup.
        std::thread::sleep(Duration::from_millis(100));
        while sub.try_recv().is_some() {}

        // After draining, try_recv must return None without blocking.
        assert!(sub.try_recv().is_none());
    }

    #[test]
    #[ignore]
    fn event_recv_times_out_when_idle() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        std::thread::sleep(Duration::from_millis(100));
        while sub.try_recv().is_some() {}

        let r = sub.recv(Duration::from_millis(300));
        assert!(
            matches!(r, Err(Error::Timeout { .. })),
            "expected Timeout, got {:?}",
            r
        );
    }

    #[test]
    #[ignore]
    fn event_drop_unsubscribes_cleanly() {
        let app = h::app_root();
        {
            let _sub = app.subscribe().expect("subscribe");
        }
        // Re-subscribing after drop must not hang or fail.
        let _sub2 = app.subscribe().expect("re-subscribe");
    }

    #[test]
    #[ignore]
    fn event_metadata_populated() {
        use std::time::Duration;
        let app = h::app_root();
        let expected_pid = app.pid;

        // Use the slider to deterministically drive an event — setting a
        // value different from its current one always fires AXValueChanged
        // (no dependence on prior test state).
        let slider = app.locator(r#"[role="slider"]"#).element().expect("slider");
        let target_val = if slider.numeric_value == Some(42.0) {
            17.0
        } else {
            42.0
        };

        let sub = app.subscribe().expect("subscribe");
        slider
            .provider()
            .set_numeric_value(&slider, target_val)
            .expect("set_numeric_value");

        let event = sub
            .wait_for(|_| true, Duration::from_secs(3))
            .expect("at least one event must arrive within 3s");

        if let Some(pid) = expected_pid {
            assert_eq!(event.app_pid, pid, "app_pid must match subscribed app");
        }
        assert!(!event.app_name.is_empty(), "app_name must be populated");
    }

    #[test]
    #[ignore]
    fn event_recv_delivers_across_threads() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        let handle =
            std::thread::spawn(move || -> Result<Event> { sub.recv(Duration::from_secs(5)) });

        // Let the thread block on recv before we trigger anything.
        std::thread::sleep(Duration::from_millis(100));

        let btn = h::named(&h::app_root(), "Submit");
        btn.focus().expect("focus failed");

        let event = handle
            .join()
            .expect("recv thread panicked")
            .expect("recv must yield an event");
        assert!(!event.app_name.is_empty());
    }

    // ── Per-EventKind end-to-end tests ──

    /// FocusChanged: focus a button other than the currently-focused one.
    /// AccessKit fires AXFocusedUIElementChanged on every focus move.
    #[test]
    #[ignore]
    fn event_focus_changed() {
        use std::time::Duration;
        let app = h::app_root();
        // Focus defaults to Submit. Focus Cancel to force a change.
        let target = app
            .locator(r#"button[name="Cancel"]"#)
            .element()
            .expect("Cancel button not found");

        let sub = app.subscribe().expect("subscribe");
        target.focus().expect("focus failed");

        let event = sub
            .wait_for(
                |e| e.kind == EventKind::FocusChanged,
                Duration::from_secs(3),
            )
            .expect("FocusChanged must be delivered within 3s");

        assert_eq!(event.kind, EventKind::FocusChanged);
        let tgt = event.target.expect("FocusChanged target must be populated");
        assert_eq!(
            tgt.role,
            Role::Button,
            "expected Button, got {:?}",
            tgt.role
        );
    }

    /// ValueChanged: set a slider to a new value. AccessKit fires
    /// AXValueChanged whenever node.raw_value() changes.
    #[test]
    #[ignore]
    fn event_value_changed() {
        use std::time::Duration;
        let app = h::app_root();
        let slider = app
            .locator(r#"[role="slider"]"#)
            .element()
            .expect("slider not found");

        let sub = app.subscribe().expect("subscribe");
        slider
            .provider()
            .set_numeric_value(&slider, 73.0)
            .expect("set_numeric_value failed");

        let event = sub
            .wait_for(
                |e| e.kind == EventKind::ValueChanged,
                Duration::from_secs(3),
            )
            .expect("ValueChanged must be delivered within 3s");
        assert_eq!(event.kind, EventKind::ValueChanged);
        let tgt = event.target.expect("target");
        assert_eq!(tgt.role, Role::Slider);
    }

    /// NameChanged: press Submit, which changes the status label text from
    /// "Status: Ready" to "Status: Please agree to terms" (checkbox off) or
    /// "Status: Submitted" (checkbox on). AccessKit fires AXTitleChanged.
    #[test]
    #[ignore]
    fn event_name_changed() {
        use std::time::Duration;

        // Step 1: prime status to "Please agree to terms" (checkbox off).
        let app = h::app_root();
        ensure_checkbox(&app, false);
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        submit
            .provider()
            .press(&submit)
            .expect("prime press failed");
        std::thread::sleep(Duration::from_millis(200));

        // Step 2: flip checkbox on so the next Submit press drives status
        // to "Submitted", guaranteed distinct from step 1.
        let app = h::app_root();
        ensure_checkbox(&app, true);
        std::thread::sleep(Duration::from_millis(200));

        // Step 3: subscribe, press Submit, assert NameChanged.
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");
        let submit = h::named(&app, "Submit");
        submit
            .provider()
            .press(&submit)
            .expect("trigger press failed");

        let event = sub
            .wait_for(|e| e.kind == EventKind::NameChanged, Duration::from_secs(3))
            .expect("NameChanged must be delivered within 3s");
        assert_eq!(event.kind, EventKind::NameChanged);
    }

    /// TextChanged: set a text field's value. AccessKit fires AXValueChanged
    /// on a text role; the macOS backend also synthesizes TextChanged.
    #[test]
    #[ignore]
    fn event_text_changed() {
        use std::time::Duration;
        let app = h::app_root();
        let text = find_name_field(&app);

        let sub = app.subscribe().expect("subscribe");
        text.provider()
            .set_value(&text, "Event E2E Text")
            .expect("set_value failed");

        let event = sub
            .wait_for(|e| e.kind == EventKind::TextChanged, Duration::from_secs(3))
            .expect("TextChanged must be delivered within 3s");
        assert_eq!(event.kind, EventKind::TextChanged);
        let tgt = event.target.expect("target");
        assert!(
            matches!(tgt.role, Role::TextField | Role::TextArea),
            "expected text role, got {:?}",
            tgt.role
        );
    }

    /// StateChanged { Checked }: toggling the checkbox fires AXValueChanged
    /// on a role of AXCheckBox; the macOS backend also emits StateChanged
    /// with the new checked flag.
    #[test]
    #[ignore]
    fn event_state_changed_checked() {
        use std::time::Duration;
        let app = h::app_root();
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found");
        let was_on = chk.states.checked == Some(Toggled::On);

        let sub = app.subscribe().expect("subscribe");
        chk.toggle().expect("toggle failed");

        let event = sub
            .wait_for(
                |e| {
                    matches!(
                        e.kind,
                        EventKind::StateChanged {
                            flag: StateFlag::Checked,
                            ..
                        }
                    )
                },
                Duration::from_secs(3),
            )
            .expect("StateChanged{Checked} must be delivered within 3s");

        match event.kind {
            EventKind::StateChanged {
                flag: StateFlag::Checked,
                value,
            } => {
                assert_eq!(value, !was_on, "checked flag must flip");
            }
            other => panic!("unexpected kind: {:?}", other),
        }
    }

    // StructureChanged: no end-to-end test yet.
    //
    // AccessKit's macOS bridge posts AXUIElementDestroyedNotification on the
    // element that is being torn down, but macOS does NOT propagate that
    // specific notification to observers registered on ancestor elements —
    // empirically confirmed by enabling the registration trace in our
    // subscribe path: all notifications register successfully, and other
    // kinds (focus, value, title, announcement) propagate to the app-level
    // observer as expected, but AXUIElementDestroyed fired by AccessKit's
    // subtree removal never reaches it.
    //
    // The provider is set up correctly to dispatch StructureChanged when
    // AXUIElementDestroyed does reach the callback (see
    // ax_observer_callback). Driving it requires either:
    //   - per-element observer registration (complex — requires tracking
    //     element lifetime), or
    //   - a non-AccessKit test app (Cocoa/AppKit) where NSWindow/NSView
    //     teardown fires the notification on the application element.
    //
    // Follow-up: add a Cocoa integration test harness or element-scoped
    // subscription support and cover StructureChanged there.

    // SelectionChanged: no end-to-end test yet.
    //
    // - AXSelectedTextChangedNotification requires `supports_text_ranges` on
    //   the text node, which means AccessKit text-run children. The test
    //   app's TextInput doesn't model runs, so set_text_selection lands at
    //   the AX level but AccessKit never synthesizes the notification.
    // - AXSelectedRowsChangedNotification / AXSelectedChildrenChanged are
    //   element-scoped on macOS; our app-level observer doesn't receive them
    //   for descendants by default, so driving via list-item selection
    //   doesn't surface them either.
    //
    // Follow-up: extend the AccessKit test app with text-run-backed
    // selectable text, or add per-container observer registration for
    // selection notifications.

    /// Announcement: press the test app's "Announce" button, which updates
    /// a Live::Polite region's value. AccessKit's macOS bridge fires
    /// AXAnnouncementRequested on the owning window.
    #[test]
    #[ignore]
    fn event_announcement() {
        use std::time::Duration;
        let app = h::app_root();
        let btn = app.locator(r#"button[name="Announce"]"#).element().expect(
            "Announce button not found in test app — \
                 required for Announcement e2e test",
        );

        let sub = app.subscribe().expect("subscribe");
        btn.press().expect("Announce press failed");

        let event = sub
            .wait_for(
                |e| e.kind == EventKind::Announcement,
                Duration::from_secs(3),
            )
            .expect("Announcement must be delivered within 3s");
        assert_eq!(event.kind, EventKind::Announcement);
    }
}
