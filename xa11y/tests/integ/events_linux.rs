//! Linux-only event-subscription end-to-end tests.
//!
//! The parent `mod events_linux` declaration in `integ/mod.rs` is gated
//! with `#[cfg(target_os = "linux")]`, so tests in this file don't need
//! per-item `#[cfg]` attributes.
//!
//! These tests exercise the native AT-SPI2 event subscription backend
//! against the AccessKit test app. They are `#[ignore]` like all the
//! other integration tests; CI runs them via `scripts/run_integ_tests.sh`
//! (Xvfb + dbus-run-session + at-spi2-registryd).
//!
//! AccessKit's Linux bridge (accesskit_unix) publishes AT-SPI2 signals
//! whenever a tree update touches the relevant properties:
//!
//!     AT-SPI2 signal                        → xa11y EventKind
//!     Focus:Focus                           → FocusChanged
//!     Object:StateChanged(focused,true)     → FocusChanged + StateChanged{Focused}
//!     Object:StateChanged(checked,_)        → StateChanged{Checked}
//!     Object:StateChanged(enabled,_)        → StateChanged{Enabled}
//!     Object:PropertyChange(accessible-name) → NameChanged
//!     Object:ValueChanged (slider/range)    → ValueChanged
//!     Object:ValueChanged (text role)       → ValueChanged + TextChanged
//!     Object:TextChanged                    → TextChanged
//!     Object:ChildrenChanged                → StructureChanged
//!     Object:SelectionChanged               → SelectionChanged
//!     Object:Announcement                   → Announcement
//!     Window:Create / Activate              → WindowOpened / WindowActivated
//!     Window:Destroy / Deactivate           → WindowClosed / WindowDeactivated
//!
//! AT-SPI2 has no menu open/close signal, so MenuOpened/MenuClosed
//! never fire on Linux — the design doc calls this out explicitly.
//!
//! Unlike the macOS tests, these tests MUST NOT catch Error::Timeout
//! and pass silently — a hard panic on timeout is the only way to
//! surface real regressions.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    fn ensure_checkbox_linux(app: &App, want_on: bool) {
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found");
        let is_on = chk.states.checked == Some(Toggled::On);
        if is_on != want_on {
            // AccessKit's AT-SPI bridge exposes checkbox toggling as the
            // generic "click" action, not a separate "toggle" — so go
            // through press() to match the existing action_toggle_checkbox
            // integration test.
            chk.provider().press(&chk).expect("press failed");
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    // ── Subscription mechanics ──

    #[test]
    #[ignore]
    fn event_try_recv_returns_none_when_idle_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        // Drain any signals that may have been queued during subscription setup
        // (AT-SPI2 often replays focus/state bits to new subscribers).
        std::thread::sleep(Duration::from_millis(300));
        while sub.try_recv().is_some() {}

        assert!(sub.try_recv().is_none());
    }

    #[test]
    #[ignore]
    fn event_recv_times_out_when_idle_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        std::thread::sleep(Duration::from_millis(300));
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
    fn event_drop_unsubscribes_cleanly_linux() {
        let app = h::app_root();
        {
            let _sub = app.subscribe().expect("subscribe");
        }
        // Re-subscribing after drop must not hang or fail: the cancel
        // closure removes every match rule and joins the iterator thread
        // before returning, so the second subscribe starts clean.
        let _sub2 = app.subscribe().expect("re-subscribe");
    }

    #[test]
    #[ignore]
    fn event_metadata_populated_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let expected_pid = app.pid;

        // Drive a deterministic event via the slider — setting a distinct
        // value always fires Object:ValueChanged, independent of prior
        // test state.
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

    // ── Per-EventKind end-to-end tests ──

    /// FocusChanged: focus a non-default *enabled* button to force a real
    /// focus move. AccessKit's AT-SPI bridge fires `StateChanged(focused,
    /// true)` from `focus_moved()`, which we map to `FocusChanged`.
    ///
    /// "Enabled" matters: the test-app's initial focus is Submit; focusing
    /// the Cancel button (disabled by default until the Terms checkbox is
    /// ticked) produces no AT-SPI signal because AccessKit skips focus
    /// diffs involving disabled nodes. Using the always-enabled "New"
    /// toolbar button gives a deterministic focus transition.
    #[test]
    #[ignore]
    fn event_focus_changed_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let target = app
            .locator(r#"button[name="New"]"#)
            .element()
            .expect("New button not found");

        let sub = app.subscribe().expect("subscribe");
        target.provider().focus(&target).expect("focus failed");

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

    /// ValueChanged: setting the slider fires Object:ValueChanged on the
    /// slider element, which the Linux backend maps to ValueChanged.
    #[test]
    #[ignore]
    fn event_value_changed_linux() {
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

    /// NameChanged: pressing Submit mutates the status label's name,
    /// which the AccessKit bridge publishes as Object:PropertyChange
    /// with detail="accessible-name".
    #[test]
    #[ignore]
    fn event_name_changed_linux() {
        use std::time::Duration;

        // Step 1: prime status to "Please agree to terms" (checkbox off).
        let app = h::app_root();
        ensure_checkbox_linux(&app, false);
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
        ensure_checkbox_linux(&app, true);
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

        // Cleanup: restore checkbox to Off so later alphabetically-sorted
        // tests (e.g. `state_checked_off_on_checkbox`,
        // `thrash_toggle_checkbox_*`) see the same initial state. The
        // integ suite runs `--test-threads=1` against a single long-lived
        // test-app process, so sticky widget state carries between tests.
        drop(sub);
        let app = h::app_root();
        ensure_checkbox_linux(&app, false);
    }

    // TextChanged: no end-to-end test yet on Linux.
    //
    // AccessKit's AT-SPI bridge only emits Object:TextChanged when the
    // AccessKit tree's text content changes (via TextInserted / TextRemoved
    // diffs), but the xa11y `set_value` path goes through AT-SPI's
    // EditableText interface — which AccessKit's bridge does not implement
    // — so the tree never observes a mutation and no signal fires. Driving
    // real text changes on Linux requires either a GTK/Qt test harness or
    // AccessKit test-app enhancements (e.g. exposing the text field via a
    // bridged EditableText provider). The mapping itself is covered by
    // the `signal_to_kinds` unit tests in xa11y-linux.

    /// Announcement: pressing the "Announce" button mutates the live
    /// region's value. AccessKit's AT-SPI bridge emits
    /// `Object:Announcement` for `Live::Polite` nodes whenever their name
    /// (which for `Role::Label` is derived from `value()`) changes; the
    /// Linux backend maps that to `Announcement`.
    #[test]
    #[ignore]
    fn event_announcement_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let announce = app
            .locator(r#"button[name="Announce"]"#)
            .element()
            .expect("Announce button not found");

        let sub = app.subscribe().expect("subscribe");
        announce.provider().press(&announce).expect("press failed");

        let event = sub
            .wait_for(
                |e| e.kind == EventKind::Announcement,
                Duration::from_secs(3),
            )
            .expect("Announcement must be delivered within 3s");
        assert_eq!(event.kind, EventKind::Announcement);
    }

    /// StateChanged{Checked}: pressing the checkbox fires
    /// Object:StateChanged(checked, new_value), which the Linux backend
    /// maps to StateChanged{Checked}. AccessKit doesn't expose a distinct
    /// "toggle" action on Linux — `press` is the canonical way to flip a
    /// checkbox (see the pre-existing `action_toggle_checkbox` integration
    /// test, which also uses press).
    #[test]
    #[ignore]
    fn event_state_changed_checked_linux() {
        use std::time::Duration;
        let app = h::app_root();
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found");
        let was_on = chk.states.checked == Some(Toggled::On);

        let sub = app.subscribe().expect("subscribe");
        chk.provider().press(&chk).expect("press failed");

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

        // Restore the checkbox to its pre-test state so later tests in the
        // same `cargo test --test-threads=1` run (e.g. `state_checked_off_
        // on_checkbox`, `thrash_toggle_checkbox_5_times`) see the app in
        // its expected initial state. The macOS/Windows counterparts of
        // this test run against isolated processes in their respective
        // harnesses, so they don't need this cleanup.
        let app = h::app_root();
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found (post-test)");
        if (chk.states.checked == Some(Toggled::On)) != was_on {
            chk.provider()
                .press(&chk)
                .expect("post-test press to restore checkbox");
        }
    }

    // Announcement, StructureChanged, SelectionChanged, Window* —
    // AccessKit's AT-SPI bridge may or may not synthesize these for the
    // specific widgets in the test app. They're covered by the dispatch
    // table's unit tests (xa11y-linux::events::tests); when the bridge
    // does emit them, the test-app-independent pathway forwards them
    // correctly. A dedicated Linux-specific test harness (or AccessKit
    // bridge widgets that reliably emit these signals) would be the right
    // place to add end-to-end coverage later.
}
