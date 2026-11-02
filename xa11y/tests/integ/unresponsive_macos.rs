//! A stopped application on macOS (issue #512).
//!
//! Before the provider bounded AX messaging, every message to a hung app
//! waited out the system default, and the failure said only "AXError -25204".
//! These tests `SIGSTOP` the test app itself, the only fixture the harness
//! guarantees, and always resume it, even on panic, so the tests after them
//! see a live app again.
//!
//! macOS-only; the module is gated with `#[cfg(target_os = "macos")]` in
//! `mod.rs`.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use std::process::Command;
    use std::time::{Duration, Instant};
    use xa11y::*;

    /// Sends `SIGSTOP` on creation and `SIGCONT` on drop, so a failed
    /// assertion cannot leave the shared test app frozen for the rest of the
    /// run.
    struct Stopped(u32);

    impl Stopped {
        fn new(pid: u32) -> Self {
            let status = Command::new("kill")
                .args(["-STOP", &pid.to_string()])
                .status()
                .expect("run kill -STOP");
            assert!(status.success(), "kill -STOP {pid} failed: {status}");
            Stopped(pid)
        }
    }

    impl Drop for Stopped {
        fn drop(&mut self) {
            let status = Command::new("kill")
                .args(["-CONT", &self.0.to_string()])
                .status();
            match status {
                Ok(s) if s.success() => {}
                other => eprintln!("kill -CONT {} failed: {other:?}", self.0),
            }
        }
    }

    #[test]
    #[ignore]
    fn stopped_app_fails_press_within_the_bound_and_is_named() {
        let app = h::app_root();
        let pid = app.pid.expect("test app reports a pid");
        // The bidi button's click is a no-op in the test app, so the press
        // delivered once the app resumes changes nothing other tests read.
        let target = h::named(&app, "Bidi");

        let start;
        let result;
        {
            let _stopped = Stopped::new(pid);
            start = Instant::now();
            result = target.press();
        }
        let elapsed = start.elapsed();

        // Bounded by the provider's 2 s messaging timeout plus slack, not by
        // the system default or the stopped app ever answering.
        assert!(
            elapsed < Duration::from_secs(3),
            "press took {elapsed:?} with the app stopped"
        );
        let err = result.expect_err("press must fail while the app is stopped");
        let Error::Platform { code, message } = &err else {
            panic!("expected Error::Platform with kAXErrorCannotComplete, got: {err}");
        };
        assert_eq!(*code, -25204, "expected kAXErrorCannotComplete: {err}");
        assert!(
            message.contains(&format!("pid {pid}")),
            "error should name the stopped pid {pid}: {message}"
        );
        assert!(
            message.contains(&format!("{:?}", app.name)),
            "error should name the app {:?}: {message}",
            app.name
        );
        assert!(
            message.contains("did not respond within"),
            "a full-length wait is a timeout and should say so: {message}"
        );

        // And once resumed, the app answers again.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match h::app_root().locator("button").count() {
                Ok(n) if n > 0 => break,
                other if Instant::now() >= deadline => {
                    panic!("app did not recover after SIGCONT: {other:?}")
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}
