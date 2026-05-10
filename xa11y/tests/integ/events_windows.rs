//! Windows-only event-subscription end-to-end tests.
//!
//! The parent `mod events_windows` declaration in `integ/mod.rs` is gated
//! with `#[cfg(target_os = "windows")]`, so tests in this file don't need
//! per-item `#[cfg]` attributes.
//!
//! These tests exercise the native UIA event subscription backend
//! against the AccessKit test app. They are `#[ignore]` like all the
//! other integration tests. CI runs them on windows-latest via
//! `scripts/run_integ_tests_windows.ps1`; the same runner class already
//! executes the Qt UIA tests, so the "no interactive desktop" objection
//! that earlier versions of this doc raised is factually wrong.
//!
//! AccessKit's Windows bridge (accesskit_windows) emits UIA events via
//! `UiaRaiseAutomationEvent`, `UiaRaiseAutomationPropertyChangedEvent`,
//! and `UiaRaiseStructureChangedEvent` whenever a tree update touches
//! the relevant properties:
//!
//!     UIA notification                     → xa11y EventKind
//!     AddFocusChangedEventHandler          → FocusChanged
//!     PropertyChanged(Value_Value)         → ValueChanged
//!     PropertyChanged(RangeValue_Value)    → ValueChanged
//!     PropertyChanged(ToggleState)         → ValueChanged + StateChanged{Checked}
//!     PropertyChanged(Name)                → NameChanged
//!     PropertyChanged(IsEnabled)           → StateChanged{Enabled}
//!     PropertyChanged(ExpandCollapseState) → StateChanged{Expanded}
//!     UIA_Text_TextChangedEventId          → TextChanged
//!     UIA_NotificationEventId              → Announcement
//!     UIA_LiveRegionChangedEventId         → Announcement
//!     UIA_Window_WindowOpenedEventId       → WindowOpened
//!     StructureChangedEventHandler         → StructureChanged
//!
//! AccessKit's Windows bridge does NOT synthesize certain events that
//! only native Win32/WPF controls would raise — notably the menu
//! open/close pair (UIA_MenuOpenedEventId / UIA_MenuClosedEventId).
//! Our provider still registers them so non-AccessKit apps get them.
//!
//! Unlike the macOS tests, these tests MUST NOT catch Error::Timeout
//! and pass silently — a hard panic on timeout is the only way to
//! surface real regressions.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    fn find_name_field_win(app: &App) -> Element {
        app.locator(r#"[name="Name"]"#)
            .elements()
            .unwrap_or_default()
            .into_iter()
            .next()
            .expect("Name text field not found in test app")
    }

    fn ensure_checkbox_win(app: &App, want_on: bool) {
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
    fn event_try_recv_returns_none_when_idle_win() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        // Drain anything from subscription setup (UIA often replays focus).
        std::thread::sleep(Duration::from_millis(200));
        while sub.try_recv().is_some() {}

        assert!(sub.try_recv().is_none());
    }

    #[test]
    #[ignore]
    fn event_recv_times_out_when_idle_win() {
        use std::time::Duration;
        let app = h::app_root();
        let sub = app.subscribe().expect("subscribe");

        std::thread::sleep(Duration::from_millis(200));
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
    fn event_drop_unsubscribes_cleanly_win() {
        let app = h::app_root();
        {
            let _sub = app.subscribe().expect("subscribe");
        }
        // Re-subscribing after drop must not hang or fail: RemoveXxx runs
        // synchronously and per-handler, so the second subscribe starts
        // with a clean slate.
        let _sub2 = app.subscribe().expect("re-subscribe");
    }

    #[test]
    #[ignore]
    fn event_metadata_populated_win() {
        use std::time::Duration;
        let app = h::app_root();
        let expected_pid = app.pid;

        // Drive a deterministic event via the slider so we don't depend on
        // prior test state — setting a distinct value always fires
        // PropertyChanged(RangeValue_Value).
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

    /// FocusChanged: focus a non-default button to force a focus move.
    /// Targets the always-enabled "New" toolbar button rather than
    /// "Cancel" — Cancel starts disabled, and the Windows UIA SetFocus
    /// call fails (`E_INVALIDARG`, HRESULT 0x80070057 when the provider
    /// reports IsEnabled=false) against disabled elements. The earlier
    /// test ran only locally (via `run_integ_tests_windows.ps1`, where
    /// prior tests may have enabled Cancel by toggling the checkbox);
    /// now that CI runs the suite cold it needs a target that's
    /// guaranteed focusable from a fresh app launch.
    #[test]
    #[ignore]
    fn event_focus_changed_win() {
        use std::time::Duration;
        let app = h::app_root();
        let target = app
            .locator(r#"button[name="New"]"#)
            .element()
            .expect("New toolbar button not found");

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

    /// ValueChanged: setting the slider fires PropertyChanged(RangeValue_Value).
    #[test]
    #[ignore]
    fn event_value_changed_win() {
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
    /// which AccessKit translates into PropertyChanged(Name).
    #[test]
    #[ignore]
    fn event_name_changed_win() {
        use std::time::Duration;

        // Step 1: prime status to "Please agree to terms" (checkbox off).
        let app = h::app_root();
        ensure_checkbox_win(&app, false);
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
        ensure_checkbox_win(&app, true);
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

    /// StateChanged{Checked}: toggling the checkbox fires
    /// PropertyChanged(ToggleState) with the new boolean value.
    /// The Windows backend also emits ValueChanged alongside — this test
    /// only asserts the StateChanged variant is delivered.
    #[test]
    #[ignore]
    fn event_state_changed_checked_win() {
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

        // Restore the checkbox to its pre-test state so later tests in the
        // same `cargo test --test-threads=1` run (state_checked_off_on_
        // checkbox, state_disabled_on_cancel, thrash_toggle_checkbox_5_
        // times) see the app in its expected initial state. Toggling the
        // checkbox also drives `state.cancel_enabled`, so a dirty checkbox
        // leaks into multiple fixtures. Matches the Linux test's cleanup.
        let app = h::app_root();
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found (post-test)");
        if (chk.states.checked == Some(Toggled::On)) != was_on {
            chk.provider()
                .toggle(&chk)
                .expect("post-test toggle to restore checkbox");
        }
    }

    /// ToggleState also produces a ValueChanged event (the design doc
    /// says the ToggleState path emits both). This test asserts the
    /// ValueChanged half of the same interaction.
    #[test]
    #[ignore]
    fn event_toggle_emits_value_changed_win() {
        use std::time::Duration;
        let app = h::app_root();
        let chk = app
            .locator("check_box")
            .element()
            .expect("check_box not found");

        let sub = app.subscribe().expect("subscribe");
        chk.toggle().expect("toggle failed");

        let event = sub
            .wait_for(
                |e| e.kind == EventKind::ValueChanged,
                Duration::from_secs(3),
            )
            .expect("ValueChanged must be delivered alongside ToggleState change");
        assert_eq!(event.kind, EventKind::ValueChanged);
    }

    /// TextChanged: setting a text field's value fires
    /// UIA_Text_TextChangedEventId when the text provider is available.
    /// If AccessKit's TextInput backend doesn't implement the text pattern
    /// we fall through to ValueChanged, so accept either signal to guard
    /// against a future AccessKit regression.
    #[test]
    #[ignore]
    fn event_text_changed_or_value_changed_win() {
        use std::time::Duration;
        let app = h::app_root();
        let text = find_name_field_win(&app);

        let sub = app.subscribe().expect("subscribe");
        text.provider()
            .set_value(&text, "Event E2E Text")
            .expect("set_value failed");

        let event = sub
            .wait_for(
                |e| matches!(e.kind, EventKind::TextChanged | EventKind::ValueChanged),
                Duration::from_secs(3),
            )
            .expect("TextChanged or ValueChanged must be delivered within 3s");
        assert!(matches!(
            event.kind,
            EventKind::TextChanged | EventKind::ValueChanged
        ));
    }

    /// Announcement: pressing the Announce button updates the live
    /// region's value, which AccessKit's Windows bridge forwards as
    /// `UIA_LiveRegionChangedEventId`. Our handler maps any of
    /// `UIA_LiveRegionChangedEventId`, `UIA_NotificationEventId`, or
    /// `UIA_SystemAlertEventId` to `EventKind::Announcement`, so we accept
    /// all three.
    #[test]
    #[ignore]
    fn event_announcement_win() {
        use std::time::Duration;
        let app = h::app_root();
        let btn = app
            .locator(r#"button[name="Announce"]"#)
            .element()
            .expect("Announce button not found");

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

    // StructureChanged, SelectionChanged, WindowOpened / WindowClosed,
    // WindowActivated / WindowDeactivated, MenuOpened / MenuClosed,
    // StateChanged{Enabled|Expanded|Busy} — no end-to-end test.
    //
    // WindowActivated / WindowDeactivated are not emitted on Windows at
    // all: UIA has no first-class event for them, and the design doc
    // principle is to not model what at least two platforms don't deliver
    // natively. (Earlier iterations of this provider inferred them from
    // focus changes; the inference was lossy enough — false negatives on
    // windows that open without taking focus, spurious emissions for
    // in-app focus moves across multiple windows — to be misleading, so
    // it was removed.)
    //
    // The other kinds: AccessKit's Windows bridge does not synthesize
    // them for the widgets in our test app (single-window, no menus, no
    // disabled/busy cycle), and some require native Win32/WPF constructs
    // (NSMenu-style menus, live regions backed by real UIA providers)
    // that AccessKit doesn't model. Covering them requires either a
    // dedicated Win32 test app (future `test-apps/win32/`) or widget
    // additions to the AccessKit app that exercise the relevant bridge
    // code paths.
}
