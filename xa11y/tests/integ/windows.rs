//! Window-management integration tests.
//!
//! Runs against the AccessKit test app on macOS, Windows, and Linux. Where a
//! verb has no platform API (Linux cannot minimize / maximize /
//! enter_fullscreen / restore / close a window) the test asserts the
//! surfaceable `Unsupported` error instead of the effect — these verbs must
//! never fall back to input simulation (tenet 2).
//!
//! Success-path coverage per platform: minimize/restore round-trip and close
//! run on macOS and Windows; activate runs everywhere; geometry (`move_to` /
//! `resize_to`) and enter-fullscreen success are macOS-only for the AccessKit
//! app — its Linux adapter omits the AT-SPI Component geometry setters.
//! Windows TransformPattern move/resize is not exercised by this suite, and
//! that gap is tracked in `tests/matrix.yaml` as coverage to add on the
//! Windows side.
//!
//! Window-state verbs are asserted with a single read wherever the provider
//! commits the state before the call returns: macOS settles
//! `enter_fullscreen` / `restore` (and the minimize that follows a fullscreen
//! exit) by polling the platform state, so a poll in the test would mask a
//! provider that stopped settling. Windows drives `WindowVisualState`
//! directly and promises only that the set call succeeded, so its
//! minimize/restore assertions keep a short poll. Waits are otherwise
//! reserved for observations outside the verbs' promises (a dialog appearing,
//! foreground activation, geometry read-back).
//!
//! Hygiene follows `multi_window.rs`: the suite shares one app instance, so
//! any test that opens the dialog closes it again before returning, and the
//! minimize/restore round-trip always restores the main window.

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use std::time::{Duration, Instant};

    use crate::integ as h;
    use xa11y::*;

    /// Poll `f` until it yields `Some`, or panic after `timeout`.
    ///
    /// Helpers that are platform-specific are gated item by item so the
    /// Linux build does not trip `dead_code` under `-Dwarnings`.
    #[cfg(not(target_os = "linux"))]
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

    /// Assert the minimized flag after a state verb.
    ///
    /// macOS settles the iconify before `minimize` returns, so one fresh read
    /// proves the promise and a poll would mask a provider that stopped
    /// settling — the regression issue #399 is about. Windows drives
    /// `WindowVisualState` directly and promises only that the set call
    /// succeeded, so it keeps the short poll the suite used before the macOS
    /// settle made it unnecessary.
    #[cfg(not(target_os = "linux"))]
    fn assert_minimized(app: &App, want: bool, what: &str) {
        let matches = |win: &Element| win.states.minimized == Some(want);
        if cfg!(target_os = "macos") {
            let win = h::one(app, "window");
            assert!(
                matches(&win),
                "{what}: minimized is {:?}, expected {want:?}",
                win.states.minimized
            );
        } else {
            wait_until(Duration::from_secs(5), what, || {
                let win = h::one(app, "window");
                matches(&win).then_some(())
            });
        }
    }

    /// The dialog window of the test app, via `App::windows()` — the
    /// cross-platform enumeration (the app's top-level window children;
    /// Windows answers the same question on the synthesized Application
    /// node).
    ///
    /// Strict: an enumeration failure panics the test. The assertion paths
    /// (appear/disappear polls) must never read a transient `windows()` error
    /// as "the dialog is gone" — that would let `close_dialog_via_window_verb`
    /// pass while the dialog is still open. The `Drop` paths use
    /// [`dialog_in`], which cannot panic.
    #[cfg(not(target_os = "linux"))]
    fn dialog_window() -> Option<Element> {
        let app = h::app_root();
        dialog_window_result(&app).expect("App::windows() enumeration must succeed")
    }

    /// Strict lookup, as a `Result`: `Ok(None)` means the app really has no
    /// dialog; an enumeration failure is `Err` and must not masquerade as an
    /// absent dialog.
    #[cfg(not(target_os = "linux"))]
    fn dialog_window_result(app: &App) -> Result<Option<Element>> {
        Ok(app.windows()?.into_iter().find(|w| {
            w.name
                .as_deref()
                .is_some_and(|n| n.contains("xa11y Test Dialog"))
        }))
    }

    /// Like [`dialog_window_result`], but lossy and cannot panic — usable
    /// from the `Drop` paths below, where a panic would abort the process and
    /// an enumeration failure is indistinguishable from "no dialog".
    #[cfg(not(target_os = "linux"))]
    fn dialog_in(app: &App) -> Option<Element> {
        app.windows().ok()?.into_iter().find(|w| {
            w.name
                .as_deref()
                .is_some_and(|n| n.contains("xa11y Test Dialog"))
        })
    }

    /// Best-effort close of the test-app dialog, if one is open. Returns
    /// `Ok(())` when there is nothing to close. Cannot panic: used from
    /// `Drop`, where a panic would abort the process.
    #[cfg(not(target_os = "linux"))]
    fn close_dialog_best_effort() -> Result<()> {
        let names = ["xa11y-test-app", "xa11y Test App"];
        let Ok(app) = App::find(Duration::from_secs(2), |d| {
            d.name.as_deref().is_some_and(|n| names.contains(&n))
        }) else {
            // App gone → nothing to close.
            return Ok(());
        };
        match dialog_in(&app) {
            Some(dialog) => dialog.close(),
            None => Ok(()),
        }
    }

    /// RAII guard for the dialog opened by [`close_dialog_via_window_verb`]:
    /// on unwind (a `close()` failure or a disappearance timeout) it
    /// best-effort closes the dialog, so a panic here cannot poison the
    /// suite's shared app instance. Same convention as `DialogGuard` in
    /// `multi_window.rs` — this file opens the dialog inline, so the guard
    /// owns only the cleanup.
    #[cfg(not(target_os = "linux"))]
    struct DialogCloseGuard;

    #[cfg(not(target_os = "linux"))]
    impl Drop for DialogCloseGuard {
        fn drop(&mut self) {
            // Best-effort, non-panicking: `Drop` must not unwind, and a
            // dialog that is already gone (the test passed) is not an error.
            let _ = close_dialog_best_effort();
        }
    }

    /// RAII guard for [`minimize_restore_roundtrip`]: on unwind (a failed
    /// `minimize`/`restore`, or a state assertion) it best-effort
    /// `restore()`s the main window, so a failure here cannot leave the
    /// shared app instance minimized for every subsequent test. Same
    /// convention as `WindowBoundsGuard` in [`move_and_resize_window`].
    #[cfg(not(target_os = "linux"))]
    struct RestoreGuard {
        win: Element,
    }

    #[cfg(not(target_os = "linux"))]
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            // Best-effort: `Drop` must not unwind, and a failed restore only
            // means the app died or the window is already restored, which the
            // next test surfaces.
            let _ = self.win.restore();
        }
    }

    #[test]
    #[ignore]
    fn windows_lists_test_app_windows() {
        // `App::windows()` must surface the main window on every platform —
        // the Application node's `Window`/`Dialog` children, uniformly (the
        // synthesized Application node on Windows answers with the process's
        // top-level windows).
        let app = h::app_root();
        let windows = app.windows().expect("App::windows must succeed");
        assert!(
            windows.iter().any(|w| {
                w.name
                    .as_deref()
                    .is_some_and(|n| n.contains("xa11y Test App"))
            }),
            "the main window must be listed: {:?}",
            windows
                .iter()
                .map(|w| w.name.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[ignore]
    fn minimize_restore_roundtrip() {
        #[cfg(target_os = "linux")]
        {
            // AT-SPI has no API to alter window state; the verb must fail
            // surfaceably, not silently no-op.
            let app = h::app_root();
            let win = h::one(&app, "window");
            let err = win
                .minimize()
                .expect_err("minimize must be unsupported on Linux");
            assert!(matches!(err, Error::Unsupported { .. }), "got {err:?}");
            assert!(
                matches!(win.restore(), Err(Error::Unsupported { .. })),
                "restore must also be unsupported on Linux"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            let app = h::app_root();
            let win = h::one(&app, "window");
            // The guard restores on unwind: a failed minimize must not leave
            // the shared app instance minimized.
            let _restore_guard = RestoreGuard { win: win.clone() };
            win.minimize().expect("minimize must succeed");
            assert_minimized(&app, true, "the window to report minimized after minimize");
            h::one(&app, "window")
                .restore()
                .expect("restore must succeed");
            assert_minimized(&app, false, "the window to report restored after restore");
        }
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn linux_window_verbs_are_unsupported_not_faked() {
        // AT-SPI has no API to alter window state or close a window. Every
        // verb must fail surfaceably with the same `Unsupported` error rather
        // than falling back to input simulation (tenet 2) — including the
        // verbs a previous revision omitted from the assertion set.
        let app = h::app_root();
        let win = h::one(&app, "window");
        for (label, result) in [
            ("minimize", win.minimize()),
            ("maximize", win.maximize()),
            ("enter_fullscreen", win.enter_fullscreen()),
            ("restore", win.restore()),
            ("close", win.close()),
        ] {
            let err = result.expect_err(&format!("{label} must be unsupported on Linux"));
            assert!(
                matches!(err, Error::Unsupported { .. }),
                "{label}: expected Unsupported, got {err:?}"
            );
        }
        for (label, result) in [
            ("move_to", win.move_to(0, 0)),
            ("resize_to", win.resize_to(100, 100)),
        ] {
            let err = result.expect_err(&format!(
                "{label} must report that AccessKit omits the AT-SPI setter"
            ));
            assert!(
                matches!(err, Error::ActionNotSupported { .. }),
                "{label}: expected ActionNotSupported, got {err:?}"
            );
        }
    }

    #[test]
    #[ignore]
    fn activate_brings_window_to_foreground() {
        // Every platform can activate: activate-then-AXRaise on macOS,
        // SetForegroundWindow + SetFocus on Windows, Component.GrabFocus on
        // Linux. The assertion is that the call succeeds — foreground
        // verification is intentionally not attempted on Windows/Linux, since
        // a headless CI session cannot reliably observe the system foreground.
        let app = h::app_root();
        let win = h::one(&app, "window");
        win.activate().expect("activate must succeed");
        // And the window must still be reported active (no error, tree alive).
        h::one(&app, "window");

        // macOS: AXRaise alone answers success with the app still in the
        // background (it only re-raises within the app's own window list), so
        // "the call succeeded" cannot distinguish a real activate from a silent
        // success. The verb must activate the app (AXFrontmost), so the app
        // ends up foreground; poll, since the round-trip can lag the call.
        #[cfg(target_os = "macos")]
        wait_until(
            Duration::from_secs(5),
            "test app to be foreground after activate",
            || {
                let root = h::app_root();
                root.is_foreground().then_some(())
            },
        );
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn enter_fullscreen_minimize_roundtrip() {
        // macOS has no accessible maximize: the classic zoom state has no
        // readable or writable attribute (`AXZoomed` is unsupported and the
        // green button's `AXPress` / `AXZoomWindow` are toggles), so the
        // provider refuses `maximize` instead of substituting another
        // operation. The native fullscreen state (`AXFullScreen`) is the only
        // window state that can be both read and written, and it is exposed
        // as its own verb, `enter_fullscreen`.
        //
        // Every verb settles before it returns: `enter_fullscreen`/`restore`
        // hold the state through the asynchronous transition, and `minimize`
        // leaves fullscreen first because AppKit ignores a minimized set on a
        // fullscreen window. The test therefore never polls — each step
        // re-enumerates once and asserts the state it just asked for — and the
        // sequence runs back-to-back (enter_fullscreen, minimize,
        // enter_fullscreen, minimize, restore), so a verb that left a
        // transition half-applied fails the next step's immediate read.
        struct RestoreOnDrop<'a> {
            app: &'a App,
        }
        impl Drop for RestoreOnDrop<'_> {
            fn drop(&mut self) {
                // Best-effort and non-panicking (`Drop` must not unwind), and
                // re-resolved rather than restoring the element the test
                // captured: a transition can have recreated the window. Poll
                // briefly rather than enumerating once — a failure
                // mid-transition can leave the real window out of
                // `App::windows()` for a moment (the fullscreen settle measures
                // an absence of 450-650 ms), and the cleanup must still find
                // something to restore, the same retry the binding suites'
                // cleanup rails make.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Ok(windows) = self.app.windows() {
                        if let Some(win) = windows
                            .into_iter()
                            .find(|w| w.actions.iter().any(|a| a == "restore"))
                        {
                            let _ = win.restore();
                            return;
                        }
                    }
                    if Instant::now() >= deadline {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }

        /// The real test-app window, from one enumeration. It advertises
        /// `enter_fullscreen` in every state this test visits (fullscreen,
        /// minimized, restored), because `AXFullScreen` stays settable.
        fn fullscreen_window(app: &App) -> Element {
            app.windows()
                .expect("App::windows() enumeration must succeed")
                .into_iter()
                .find(|w| w.actions.iter().any(|a| a == "enter_fullscreen"))
                .expect("the test app window must advertise enter_fullscreen")
        }

        /// Assert the settled state with one read. `maximized` must stay
        /// `None` on macOS — there is no readable zoom state — which is what
        /// keeps the two operations distinct (see the module docs).
        fn assert_states(app: &App, fullscreen: Option<bool>, minimized: Option<bool>, what: &str) {
            let w = fullscreen_window(app);
            assert_eq!(w.states.fullscreen, fullscreen, "{what}: fullscreen");
            assert_eq!(w.states.minimized, minimized, "{what}: minimized");
            assert_eq!(
                w.states.maximized, None,
                "{what}: maximized stays unknown on macOS"
            );
        }

        let app = h::app_root();
        let _guard = RestoreOnDrop { app: &app };

        // Baseline: the shared app must start restored; if an earlier test
        // left it fullscreen or minimized, restore first.
        let state = fullscreen_window(&app);
        if state.states.fullscreen == Some(true) || state.states.minimized == Some(true) {
            state.restore().expect("baseline restore must succeed");
        }

        // `maximize` is not a macOS operation: the verb must not be
        // advertised, and calling it must fail surfaceably rather than
        // substituting fullscreen (tenet 3).
        assert!(
            app.windows()
                .expect("App::windows() enumeration must succeed")
                .iter()
                .all(|w| !w.actions.iter().any(|a| a == "maximize")),
            "macOS windows must not advertise maximize"
        );
        let err = fullscreen_window(&app)
            .maximize()
            .expect_err("maximize must be unsupported on macOS");
        assert!(matches!(err, Error::Unsupported { .. }), "got {err:?}");

        // enter_fullscreen commits.
        fullscreen_window(&app)
            .enter_fullscreen()
            .expect("enter_fullscreen must succeed");
        assert_states(&app, Some(true), Some(false), "after enter_fullscreen");

        // A repeated call is a no-op, not a toggle.
        fullscreen_window(&app)
            .enter_fullscreen()
            .expect("repeated enter_fullscreen must succeed");
        assert_states(
            &app,
            Some(true),
            Some(false),
            "after repeated enter_fullscreen",
        );

        // minimize leaves fullscreen first, then iconifies.
        fullscreen_window(&app)
            .minimize()
            .expect("minimize must succeed on a fullscreen window");
        assert_states(
            &app,
            Some(false),
            Some(true),
            "after minimize from fullscreen",
        );

        // A minimized window is still reachable by the next back-to-back call.
        fullscreen_window(&app)
            .enter_fullscreen()
            .expect("enter_fullscreen must succeed on a minimized window");
        assert_states(
            &app,
            Some(true),
            Some(false),
            "after re-entering fullscreen",
        );

        fullscreen_window(&app)
            .minimize()
            .expect("minimize must succeed again");
        assert_states(&app, Some(false), Some(true), "after the second minimize");

        // restore clears both states and is the last step.
        fullscreen_window(&app)
            .restore()
            .expect("restore must succeed");
        assert_states(&app, Some(false), Some(false), "after restore");
    }

    #[test]
    #[ignore]
    fn close_dialog_via_window_verb() {
        #[cfg(target_os = "linux")]
        {
            let app = h::app_root();
            let win = h::one(&app, "window");
            let err = win.close().expect_err("close must be unsupported on Linux");
            assert!(matches!(err, Error::Unsupported { .. }), "got {err:?}");
            // No `return;` here: on Linux the cfg below removes the
            // non-Linux block, so this is already the last statement.
            // A trailing `return;` is `needless_return` under `-D warnings`.
        }
        #[cfg(not(target_os = "linux"))]
        {
            let app = h::app_root();
            // The guard exists before the dialog opens: a failed press or an
            // appearance timeout must still get the cleanup.
            let _guard = DialogCloseGuard;
            let open_btn = h::named(&app, "Open Dialog");
            h::try_act(&open_btn, "press").expect("press 'Open Dialog'");
            wait_until(Duration::from_secs(5), "the dialog to appear", || {
                dialog_window().map(|_| ())
            });

            let dialog = dialog_window().expect("dialog element must resolve");
            dialog.close().expect("close() must succeed on the dialog");

            wait_until(Duration::from_secs(5), "the dialog to disappear", || {
                dialog_window().is_none().then_some(())
            });
        }
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn move_and_resize_window() {
        // macOS AXPosition / AXSize are settable on the winit window. The
        // read-back is polled because the bridge can round-trip asynchronously.
        // On Windows the winit window does not reliably expose
        // TransformPattern, so TransformPattern move/resize coverage lives in
        // the CLI suite's test_window.py (driven on the windows-latest
        // winforms cell) and the python-window / js-window mutating suites —
        // the same coverage tests/matrix.yaml records.
        //
        // The suite shares one app instance, so the move and the grow must be
        // undone before the test returns — on the success path AND on panic
        // (same RAII convention as `DialogGuard` in multi_window.rs).
        struct WindowBoundsGuard {
            win: Element,
            x: i32,
            y: i32,
            w: u32,
            h: u32,
        }
        impl Drop for WindowBoundsGuard {
            fn drop(&mut self) {
                // Best-effort cleanup; `Drop` must not unwind, and a failed
                // restore only means the app died, which the next test
                // surfaces.
                let _ = self.win.move_to(self.x, self.y);
                let _ = self.win.resize_to(self.w, self.h);
            }
        }

        let app = h::app_root();
        let win = h::one(&app, "window");

        // Move by the window's own bounds origin delta so the test is
        // deterministic regardless of where the app was placed — but move
        // horizontally. macOS clamps a window's y to keep its title bar below
        // the menu bar, so a window that opens near the top (the test app does
        // under a VM's placement) cannot reach an up-and-left target and the
        // read-back would never match; x has no such clamp.
        let (from_x, from_y, w, hgt) = win
            .bounds
            .map(|b| (b.x, b.y, b.width, b.height))
            .expect("the window must have bounds");
        let _guard = WindowBoundsGuard {
            win: win.clone(),
            x: from_x,
            y: from_y,
            w,
            h: hgt,
        };
        win.move_to(from_x + 40, from_y)
            .expect("move_to must succeed");
        wait_until(Duration::from_secs(5), "the window to move", || {
            let w = h::one(&app, "window");
            // Poll with a small tolerance: macOS converts logical→physical→
            // logical, and fractional-scale rounding can shift the read-back
            // by a few points. Off-by-40 is still off-by-40, so the delta
            // cannot be masked by the tolerance.
            w.bounds
                .map(|b| (b.x - (from_x + 40)).abs() <= 2 && (b.y - from_y).abs() <= 2)
                .and_then(|moved| moved.then_some(()))
        });

        // Grow the width and shrink the height: growing the height can be
        // clamped by the visible frame (the dock) when the window sits low on
        // a small screen, and shrinking never is.
        win.resize_to(w + 10, hgt - 10)
            .expect("resize_to must succeed");
        wait_until(Duration::from_secs(5), "the window to resize", || {
            let w2 = h::one(&app, "window");
            w2.bounds
                .map(|b| {
                    (i64::from(b.width) - i64::from(w + 10)).abs() <= 2
                        && (i64::from(b.height) - i64::from(hgt - 10)).abs() <= 2
                })
                .and_then(|resized| resized.then_some(()))
        });
    }
}
