//! Multi-window integration tests.
//!
//! The test app's main window carries an "Open Dialog" button that opens a
//! second top-level window (title "xa11y Test Dialog") with its own AccessKit
//! adapter. Opening it moves host focus onto the dialog and off the main
//! window; closing it reverses that. These tests exercise the scenarios the
//! #304/#305 changes exist for: two top-level windows of one process, and the
//! `active` / foreground flags tracking the focused window.
//!
//! Hygiene: the whole integration suite shares ONE app instance, so every test
//! here opens the dialog through a [`DialogGuard`] whose `Drop` closes it —
//! even on an assertion failure / panic — so a leaked-open dialog can't poison
//! the other tests.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use xa11y::*;

    /// Names the test app reports across platforms (process name on
    /// Linux/macOS, window title on Windows). The dialog's title
    /// ("xa11y Test Dialog") is deliberately NOT in this list, so
    /// [`current_app`] always resolves the *main* window.
    const TEST_APP_NAMES: [&str; 2] = ["xa11y-test-app", "xa11y Test App"];

    /// Non-panicking app lookup (usable from `Drop`, where panicking would
    /// abort the process). Mirrors `h::app_root` but returns a `Result`.
    fn current_app() -> Result<App> {
        App::find(Duration::from_secs(2), |d| {
            d.name
                .as_deref()
                .is_some_and(|n| TEST_APP_NAMES.contains(&n))
        })
    }

    /// The top-level windows belonging to the test-app process, as
    /// `(name, active)` pairs.
    ///
    /// `App::windows()` is the cross-platform enumeration: every platform
    /// reports one Application node per process, whose children are the
    /// process's `Role::Window` / `Role::Dialog` top-level windows in z-order
    /// (Windows synthesizes that node and queries by pid; macOS reads
    /// `AXWindows`; Linux filters the AT-SPI walk). When `pid` is given only
    /// that process is enumerated; otherwise the app owning the main window
    /// is inferred.
    fn app_windows(pid: Option<u32>) -> Result<Vec<(String, bool)>> {
        let apps = App::list()?;
        let pid = match pid {
            Some(pid) => Some(pid),
            None => apps
                .iter()
                .find(|a| TEST_APP_NAMES.contains(&a.name.as_str()))
                .and_then(|a| a.pid),
        };
        // Failed inference must not silently fall back to enumerating every
        // app on the system: the test's own app is identifiable by
        // construction, and window counts from unrelated processes would
        // make every assertion here meaningless.
        let Some(pid) = pid else {
            return Err(Error::selector_not_matched("xa11y test app"));
        };
        // A process can surface more than one Application entry for one pid
        // on Linux — the AT-SPI registry can register one process twice —
        // and each entry's `windows()` walks the same top-level windows.
        // Deduplicate by the platform's stable identity so each window is
        // listed once per enumeration whatever the entry count; without it a
        // duplicated window would show up twice as `active` and break the
        // "exactly one active window" assertions. macOS and Windows report
        // one Application entry per pid (macOS dedups `App::list` by pid), so
        // dedup never runs there.
        //
        // `ElementData.handle` is NOT that identity: both Linux and macOS
        // allocate a fresh monotonic handle per snapshot, so the same window
        // rebuilt through two entries gets two handles and would stay
        // duplicated. `stable_id` is the cross-snapshot correlation field:
        // Linux D-Bus object path (always present, and Linux is the only
        // multi-registering platform), Windows HWND for top-level windows.
        // The AT-SPI identity is really `(bus_name, object path)` — two bus
        // connections of one process can expose distinct windows under the
        // same path — so a Linux `stable_id` is scoped by the `bus_name` the
        // provider records in `raw`. Window roles carrying no stable_id
        // (macOS `AXIdentifier` is often absent) key on the handle instead —
        // unique per built node within one enumeration, so distinct windows
        // are never merged on shared title and bounds, which the old
        // name+bounds fallback could do.
        let mut out: Vec<(String, bool)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for app in &apps {
            if app.pid != Some(pid) {
                continue;
            }
            for w in app.windows()? {
                let data = w.data();
                let key = data
                    .stable_id
                    .clone()
                    .map(|sid| match data.raw.get("bus_name") {
                        Some(serde_json::Value::String(bus)) => format!("{bus}:{sid}"),
                        _ => sid,
                    })
                    .unwrap_or_else(|| {
                        // Handle, not (name, bounds): presentation data is
                        // not identity and could merge two distinct
                        // same-process windows (macOS AXIdentifier is often
                        // absent).
                        format!("h{}", data.handle)
                    });
                if seen.insert(key) {
                    out.push((w.name.clone().unwrap_or_default(), w.states.active));
                }
            }
        }
        Ok(out)
    }

    /// Poll `f` until it yields `Some`, or panic after `timeout`. The standard
    /// deadline-loop idiom for state transitions — no bare sleeps waiting for a
    /// fixed duration. A hard error inside `f` is expected to panic (surface),
    /// not be retried; only the "not ready yet" case returns `None`.
    fn wait_until<T>(timeout: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(v) = f() {
                return v;
            }
            if Instant::now() >= deadline {
                panic!("timed out after {timeout:?} waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Press the "Close Dialog" button through the accessibility API.
    /// Best-effort: returns `Ok(())` when no dialog is open (nothing to close).
    fn press_close_dialog() -> Result<()> {
        // The dialog is a top-level window of the process on every platform —
        // Windows includes it as a sibling top-level window of the synthesized
        // Application node, macOS reads it from AXWindows, Linux from the
        // AT-SPI walk — so search inside each of the app's windows.
        let app = current_app()?;
        let windows = app.windows()?;
        for window in &windows {
            let locator = Locator::new(
                Arc::clone(window.provider()),
                Some(window.data().clone()),
                r#"[name*="Close Dialog"]"#,
            );
            if let Some(btn) = locator.elements()?.first() {
                return btn.provider().perform_action(btn, "press");
            }
        }
        Ok(())
    }

    /// RAII guard: presses "Open Dialog", waits for the second window, and on
    /// `Drop` closes it again. Because the whole suite shares one app instance,
    /// a dialog left open would corrupt every subsequent test — the guard makes
    /// cleanup run on the normal path *and* on panic-unwind.
    struct DialogGuard {
        pid: Option<u32>,
    }

    impl DialogGuard {
        /// Press "Open Dialog" and block until the app exposes two windows.
        fn open() -> Self {
            let app = h::app_root();
            let pid = app.pid;
            let open_btn = h::named(&app, "Open Dialog");
            h::try_act(&open_btn, "press").expect("press 'Open Dialog'");
            wait_until(
                Duration::from_secs(5),
                "the dialog window to appear",
                || {
                    let windows = app_windows(pid).expect("enumerate windows");
                    (windows.len() >= 2).then_some(())
                },
            );
            DialogGuard { pid }
        }
    }

    impl Drop for DialogGuard {
        fn drop(&mut self) {
            // Best-effort, non-panicking cleanup — see the struct doc. The
            // result is deliberately ignored: `Drop` must not unwind, and a
            // dialog that is already gone is not an error. Real failures here
            // can only mean the app died, which the next test will surface.
            let _ = press_close_dialog();
            // Wait (bounded) for the window to actually go away so the next
            // test in this single-threaded, shared-app suite starts from a
            // clean single-window state.
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                // If the query fails we can't confirm closure; assume closed so
                // Drop returns promptly rather than spinning to the deadline.
                if app_windows(self.pid).map(|w| w.len()).unwrap_or(1) <= 1 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Multi-window tests
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn by_pid_windows_are_process_complete_on_linux() {
        // `App::by_pid` resolves one AT-SPI Application entry, while the Linux
        // registry can register several entries for one pid (a main app plus
        // a dialog surfaced as its own app node). `by_pid(pid).windows()` must
        // therefore list the whole process: `App::windows_with` merges the
        // same-pid entries (deduplicated by stable identity — D2). macOS and
        // Windows report one entry per pid, so the assertion holds there too,
        // but the multi-registration shape is the Linux one.
        if !cfg!(target_os = "linux") {
            return;
        }
        let app = h::app_root();
        let pid = app.pid.expect("the test app carries a pid");
        let by_pid = App::by_pid(pid, Duration::from_secs(2)).expect("by_pid must resolve");
        let _guard = DialogGuard::open();
        let names = wait_until(Duration::from_secs(5), "both windows via by_pid", || {
            let windows = by_pid.windows().expect("by_pid windows must enumerate");
            let names: Vec<String> = windows.iter().filter_map(|w| w.name.clone()).collect();
            (names.len() >= 2).then_some(names)
        });
        assert!(
            names.iter().any(|n| n.contains("xa11y Test App")),
            "main window missing from by_pid().windows(): {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("xa11y Test Dialog")),
            "dialog window missing from by_pid().windows(): {names:?}"
        );
    }

    #[test]
    #[ignore]
    fn second_window_appears() {
        // Pressing "Open Dialog" opens a second top-level window; the app then
        // exposes both the main window and the dialog, by title.
        let app = h::app_root();
        let pid = app.pid;
        let _guard = DialogGuard::open();

        let names = wait_until(Duration::from_secs(5), "two named windows", || {
            let windows = app_windows(pid).expect("enumerate windows");
            let names: Vec<String> = windows.into_iter().map(|(n, _)| n).collect();
            (names.len() >= 2).then_some(names)
        });
        assert!(
            names.iter().any(|n| n.contains("xa11y Test App")),
            "main window title missing from {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("xa11y Test Dialog")),
            "dialog window title missing from {names:?}"
        );
    }

    #[test]
    #[ignore]
    fn active_follows_window_focus() {
        // The `active` (foreground-window) flag tracks host focus: with the
        // dialog open exactly one window is active and it's the dialog; after
        // it closes, the main window is active again.
        let app = h::app_root();
        let pid = app.pid;

        {
            let _guard = DialogGuard::open();
            let active = wait_until(
                Duration::from_secs(5),
                "exactly one active window (the dialog)",
                || sole_active_window(pid),
            );
            assert!(
                active.contains("xa11y Test Dialog"),
                "the active window should be the dialog, got {active:?}"
            );
        }

        // Guard dropped → dialog closed → focus returns to the main window.
        let active = wait_until(
            Duration::from_secs(5),
            "the main window to become active again",
            || sole_active_window(pid),
        );
        assert!(
            active.contains("xa11y Test App"),
            "the main window should be active after closing the dialog, got {active:?}"
        );
    }

    /// Return the name of the single active window, or `None` if the count of
    /// active windows is not exactly one (still transitioning).
    fn sole_active_window(pid: Option<u32>) -> Option<String> {
        let windows = app_windows(pid).expect("enumerate windows");
        let mut active = windows.into_iter().filter(|(_, a)| *a).map(|(n, _)| n);
        let first = active.next()?;
        match active.next() {
            Some(_) => None, // more than one active — not settled yet
            None => Some(first),
        }
    }

    #[test]
    #[ignore]
    fn foreground_scenario_two_windows() {
        // The #304/#305 scenario: with a second window open, `App::foreground`
        // still resolves to the test-app process. Every pid-matching entry is
        // an Application node — the old Windows shape of one entry per
        // top-level window is gone, and macOS/Windows list one node per
        // pid/process (macOS synthesizes it per pid; Windows per process).
        let app = h::app_root();
        let pid = app.pid;
        let _guard = DialogGuard::open();

        let foreground = App::foreground(Duration::from_secs(2))
            .expect("App::foreground must resolve with the dialog open");
        assert_eq!(
            foreground.pid, pid,
            "the foreground app must be the test app, got {:?}",
            foreground.name
        );

        // The contract: every pid-matching entry is an Application node
        // (never a window), and the foreground process reports is_foreground()
        // on the entry the platform deems frontmost. The *count* is not part
        // of the contract — the Linux AT-SPI registry can surface more than
        // one Application entry for one pid (e.g. a toolkit that registers a
        // dialog as its own accessibility application), and tagging picks the
        // right one via the entry-level `active` flag rather than counting.
        let mine: Vec<App> = App::list()
            .expect("App::list must succeed")
            .into_iter()
            .filter(|a| a.pid == pid)
            .collect();
        assert!(
            !mine.is_empty(),
            "the process must appear in App::list(), got {:?}",
            mine.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert!(
            mine.iter().all(|a| a.data.role == Role::Application),
            "every pid-matching entry must be an Application node, not a window, got {:?}",
            mine.iter().map(|a| a.data.role).collect::<Vec<_>>()
        );
        assert!(
            mine.iter().any(|a| a.is_foreground()),
            "the pid-matching entries must report is_foreground() while the process holds the \
             foreground"
        );
    }

    #[test]
    #[ignore]
    fn app_node_contract_is_uniform_across_platforms() {
        // The synthetic-node contract, exercised end-to-end: the app entry is
        // an Application node whose children are the process's windows; each
        // window's parent is that Application node; and the app node itself is
        // not window-like — asking it to close must fail with
        // ActionNotSupported, never silently no-op or close a window.
        let app = h::app_root();
        assert_eq!(
            app.data.role,
            Role::Application,
            "the app entry must be an Application node on every platform"
        );

        let _guard = DialogGuard::open();
        let windows = app
            .windows()
            .expect("App::windows() must succeed with two windows open");
        let names: Vec<&str> = windows
            .iter()
            .map(|w| w.name.as_deref().unwrap_or_default())
            .collect();
        assert!(
            names.iter().any(|n| n.contains("xa11y Test App")),
            "main window missing from {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("xa11y Test Dialog")),
            "dialog window missing from {names:?}"
        );

        for window in &windows {
            let parent = window.parent().expect("parent must resolve");
            // The parent of a top-level window is the Application node on
            // macOS (AXParent) and on Windows (the synthesized node reports
            // the process as the parent of every top-level HWND). AT-SPI is
            // the exception: the spec puts top-level frames under the
            // registry root, and `LinuxProvider::get_parent` maps that to
            // "no parent" (see `tree.rs::element_parent_field`). The window
            // is still a child of the app for enumeration purposes — the
            // Parent attribute is what does not say so on Linux.
            #[cfg(not(target_os = "linux"))]
            {
                let parent = parent.expect("each top-level window must have a process parent");
                assert_eq!(
                    parent.data().role,
                    Role::Application,
                    "a window's parent must be the Application node"
                );
                assert_eq!(
                    parent.data().pid,
                    app.pid,
                    "the parent app must own the window"
                );
            }
            #[cfg(target_os = "linux")]
            {
                // AT-SPI quirk: a parent that resolves is still the app; a
                // registry-root parent is reported as None by the provider.
                if let Some(parent) = parent {
                    assert_eq!(
                        parent.data().role,
                        Role::Application,
                        "a window's parent must be the Application node"
                    );
                    assert_eq!(
                        parent.data().pid,
                        app.pid,
                        "the parent app must own the window"
                    );
                }
            }
        }

        // The app node is not a window: the close verb is rejected by role
        // before reaching any provider, on every platform.
        let err = app
            .as_element()
            .close()
            .expect_err("closing the app node itself must be refused");
        assert!(
            matches!(
                err,
                Error::ActionNotSupported { ref action, role: Role::Application } if action == "close"
            ),
            "expected ActionNotSupported naming the Application role, got {err:?}"
        );
    }
}
