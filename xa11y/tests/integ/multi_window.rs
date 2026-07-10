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
    /// On Windows each top-level window is its own `App::list()` entry (the
    /// dialog is a sibling of the main window, not a descendant), so we
    /// enumerate by pid. Elsewhere the windows are descendants of the app
    /// root, so a `window` locator finds them.
    fn app_windows(pid: Option<u32>) -> Result<Vec<(String, bool)>> {
        #[cfg(target_os = "windows")]
        {
            Ok(App::list()?
                .into_iter()
                .filter(|a| a.pid == pid)
                .map(|a| {
                    let el = a.as_element();
                    (a.name.clone(), el.states.active)
                })
                .collect())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = pid;
            let app = current_app()?;
            Ok(app
                .locator("window")
                .elements()?
                .into_iter()
                .map(|w| (w.name.clone().unwrap_or_default(), w.states.active))
                .collect())
        }
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
        // On Windows the dialog is a sibling top-level window, so search each
        // of the process's windows; elsewhere it's a descendant of the app
        // root and one locator suffices.
        #[cfg(target_os = "windows")]
        let roots: Vec<App> = {
            let app = current_app()?;
            App::list()?
                .into_iter()
                .filter(|a| a.pid == app.pid)
                .collect()
        };
        #[cfg(not(target_os = "windows"))]
        let roots: Vec<App> = vec![current_app()?];

        for root in &roots {
            let buttons = root.locator(r#"[name*="Close Dialog"]"#).elements()?;
            if let Some(btn) = buttons.first() {
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
        // still resolves to the test-app process, and enumeration/tagging
        // behaves per-platform.
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

        #[cfg(target_os = "windows")]
        {
            // #304: the process owns two top-level windows and BOTH must appear
            // in `App::list()` (the modal case that previously dropped the
            // second window when it shared a pid).
            let mine: Vec<App> = App::list()
                .expect("App::list must succeed")
                .into_iter()
                .filter(|a| a.pid == pid)
                .collect();
            assert_eq!(
                mine.len(),
                2,
                "both top-level windows of the process must appear in App::list(), got {:?}",
                mine.iter().map(|a| &a.name).collect::<Vec<_>>()
            );
            // #305: window-precise tagging — exactly one of the two windows may
            // report `is_foreground()`.
            let foreground_count = mine.iter().filter(|a| a.is_foreground()).count();
            assert_eq!(
                foreground_count, 1,
                "exactly one window may report is_foreground(), got {foreground_count}"
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            // On macOS/Linux apps are processes, so the two windows collapse to
            // a single `App::list()` entry for the pid, and it reports
            // is_foreground().
            let mine: Vec<App> = App::list()
                .expect("App::list must succeed")
                .into_iter()
                .filter(|a| a.pid == pid)
                .collect();
            assert_eq!(
                mine.len(),
                1,
                "the process should be a single App::list() entry on macOS/Linux, got {:?}",
                mine.iter().map(|a| &a.name).collect::<Vec<_>>()
            );
            assert!(
                mine[0].is_foreground(),
                "the test-app entry must report is_foreground() while it holds the foreground"
            );
        }
    }
}
