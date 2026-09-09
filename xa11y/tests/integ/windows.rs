//! Window-management integration tests.
//!
//! Runs against the AccessKit test app on macOS, Windows, and Linux. Where a
//! verb has no platform API (Linux cannot minimize / maximize / restore /
//! close / move / resize a window) the test asserts the surfaceable
//! `Unsupported` error instead of the effect — these verbs must never fall
//! back to input simulation (tenet 2).
//!
//! Success-path coverage per platform: minimize/restore round-trip and close
//! run on macOS and Windows; raise runs everywhere; geometry (`move_to` /
//! `resize_to`) and maximize success are macOS-only today — Windows
//! TransformPattern move/resize is not exercised by this suite, and that gap
//! is tracked in `tests/matrix.yaml` as coverage to add on the Windows side.
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
    /// On Linux the window verbs fail with `Unsupported` (asserted inline in
    /// the tests), so the dialog / guard / poll helpers below are never
    /// constructed there and are gated `#[cfg(not(target_os = "linux"))]`
    /// item by item, so the Linux build does not trip `dead_code` under
    /// `-Dwarnings`.
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

    /// RAII guard for [`minimize_restore_roundtrip`]: on unwind (a polling
    /// timeout, a failed `minimize`) it best-effort `restore()`s the main
    /// window, so a failure here cannot leave the shared app instance
    /// minimized for every subsequent test. Same convention as
    /// `WindowBoundsGuard` in [`move_and_resize_window`].
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
            // The guard restores on unwind: a failed minimize or a polling
            // timeout must not leave the shared app instance minimized.
            let _restore_guard = RestoreGuard { win: win.clone() };
            win.minimize().expect("minimize must succeed");
            // The state read must reflect the minimized window; poll, since
            // the notification can lag the call.
            wait_until(Duration::from_secs(5), "window to report minimized", || {
                let w = h::one(&app, "window");
                (w.states.minimized == Some(true)).then_some(())
            });
            h::one(&app, "window")
                .restore()
                .expect("restore must succeed");
            wait_until(Duration::from_secs(5), "window to report restored", || {
                let w = h::one(&app, "window");
                (w.states.minimized == Some(false)).then_some(())
            });
        }
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn linux_window_verbs_are_unsupported_not_faked() {
        // AT-SPI has no API to alter window state or geometry. Every window
        // verb must fail surfaceably with the same `Unsupported` error rather
        // than falling back to input simulation (tenet 2) — including the
        // verbs a previous revision omitted from the assertion set.
        let app = h::app_root();
        let win = h::one(&app, "window");
        for (label, result) in [
            ("minimize", win.minimize()),
            ("maximize", win.maximize()),
            ("restore", win.restore()),
            ("close", win.close()),
            ("move_to", win.move_to(0, 0)),
            ("resize_to", win.resize_to(100, 100)),
        ] {
            let err = result.expect_err(&format!("{label} must be unsupported on Linux"));
            assert!(
                matches!(err, Error::Unsupported { .. }),
                "{label}: expected Unsupported, got {err:?}"
            );
        }
    }

    #[test]
    #[ignore]
    fn raise_brings_window_to_foreground() {
        // Every platform has a raise: activate-then-AXRaise on macOS,
        // SetForegroundWindow + SetFocus on Windows, Component.GrabFocus on
        // Linux. The assertion is that the call succeeds — foreground
        // verification is intentionally not attempted on Windows/Linux, since
        // a headless CI session cannot reliably observe the system foreground.
        let app = h::app_root();
        let win = h::one(&app, "window");
        win.raise().expect("raise must succeed");
        // And the window must still be reported active (no error, tree alive).
        h::one(&app, "window");

        // macOS: AXRaise alone answers success with the app still in the
        // background (it only re-raises within the app's own window list), so
        // "the call succeeded" cannot distinguish a real raise from a silent
        // success. The verb must activate the app (AXFrontmost), so the app
        // ends up foreground; poll, since the round-trip can lag the call.
        #[cfg(target_os = "macos")]
        wait_until(
            Duration::from_secs(5),
            "test app to be foreground after raise",
            || {
                let root = h::app_root();
                root.is_foreground().then_some(())
            },
        );
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn maximize_restore_roundtrip() {
        // macOS: maximize presses the window's zoom button (there is no
        // zoom-state AX attribute; see `WindowZoom` in xa11y-macos), and the
        // state the press leaves behind is AXFullScreen. restore() undoes it.
        // The read-back is polled because the bridge can round-trip
        // asynchronously. Windows maximize is not asserted here — the winit
        // window's TransformPattern coverage is tracked as a gap in
        // tests/matrix.yaml.
        struct MaximizeGuard {
            win: Element,
        }
        impl Drop for MaximizeGuard {
            fn drop(&mut self) {
                // Best-effort cleanup; `Drop` must not unwind, and a failed
                // restore only means the app died, which the next test
                // surfaces.
                let _ = self.win.restore();
            }
        }

        let app = h::app_root();
        let win = h::one(&app, "window");
        let _guard = MaximizeGuard { win: win.clone() };
        win.maximize().expect("maximize must succeed");
        wait_until(Duration::from_secs(5), "window to report maximized", || {
            let w = h::one(&app, "window");
            // macOS reports the zoomed state as `fullscreen` (AXFullScreen).
            (w.states.maximized == Some(true) || w.states.fullscreen == Some(true)).then_some(())
        });
        win.restore().expect("restore must succeed");
        wait_until(Duration::from_secs(5), "window to report restored", || {
            let w = h::one(&app, "window");
            let still_zoomed =
                w.states.maximized == Some(true) || w.states.fullscreen == Some(true);
            (!still_zoomed).then_some(())
        });
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
        // macOS: AXPosition / AXSize are settable on the winit window. The
        // read-back is polled because the bridge can round-trip asynchronously.
        // On Windows the winit window does not reliably expose
        // TransformPattern, so TransformPattern move/resize coverage lives in
        // the CLI suite's test_window.py (driven on the windows-latest
        // winforms cell) and the python-window / js-window mutating suites —
        // the same coverage tests/matrix.yaml records. Only the macOS
        // geometry path is exercised here.
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
        // deterministic regardless of where the app was placed.
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
        win.move_to(from_x - 40, from_y - 40)
            .expect("move_to must succeed");
        wait_until(Duration::from_secs(5), "the window to move", || {
            let w = h::one(&app, "window");
            // Poll with a small tolerance: macOS converts logical→physical→
            // logical, and fractional-scale rounding can shift the read-back
            // by a few points. Off-by-40 is still off-by-40, so the delta
            // cannot be masked by the tolerance.
            w.bounds
                .map(|b| (b.x - (from_x - 40)).abs() <= 2 && (b.y - (from_y - 40)).abs() <= 2)
                .and_then(|moved| moved.then_some(()))
        });

        win.resize_to(w + 10, hgt + 10)
            .expect("resize_to must succeed");
        wait_until(Duration::from_secs(5), "the window to resize", || {
            let w2 = h::one(&app, "window");
            w2.bounds
                .map(|b| {
                    (i64::from(b.width) - i64::from(w + 10)).abs() <= 2
                        && (i64::from(b.height) - i64::from(hgt + 10)).abs() <= 2
                })
                .and_then(|resized| resized.then_some(()))
        });
    }
}
