//! A stopped application on the AT-SPI bus (issue #484).
//!
//! Enumeration reads every registered application's name, so before the
//! connection carried a method timeout one `SIGSTOP`ped app hung `App::list`
//! for every caller, forever. A locator wait against a stopped app is
//! bounded more tightly still: by its own timeout, not the connection's 5 s
//! one (#531). These tests stop the test app itself — the only fixture the
//! harness guarantees — and always resume it, even on panic, so the tests
//! after them see a live app again.
//!
//! Linux-only; the module is gated with `#[cfg(target_os = "linux")]` in
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
    fn stopped_app_fails_list_fast_and_is_named() {
        let pid = h::app_root().pid.expect("test app reports a pid");
        let start;
        let result;
        {
            let _stopped = Stopped::new(pid);
            start = Instant::now();
            result = App::list();
        }
        let elapsed = start.elapsed();

        // Bounded by the connection's 5 s method timeout plus slack, not by
        // the stopped app ever answering.
        assert!(
            elapsed < Duration::from_secs(15),
            "App::list took {elapsed:?} with a stopped app on the bus"
        );
        let err = match result {
            Err(e) => e,
            Ok(apps) => panic!(
                "App::list succeeded with the test app stopped; it must report the \
                 unresponsive app, not drop it. Listed: {:?}",
                apps.iter().map(|a| a.name.clone()).collect::<Vec<_>>()
            ),
        };
        let Error::Timeout { diagnosis, .. } = &err else {
            panic!("expected Error::Timeout naming the stopped app, got: {err}");
        };
        let d = diagnosis.as_deref().expect("timeout carries a diagnosis");
        let condition = d.condition.as_deref().unwrap_or_default();
        assert!(
            condition.contains(&format!("pid {pid}")),
            "diagnosis should name the stopped pid {pid}: {err}"
        );
        let observed = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            observed.contains("stopped"),
            "diagnosis should report the process state: {err}"
        );

        // And once resumed, enumeration works again on the same provider.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match App::list() {
                Ok(apps) if apps.iter().any(|a| a.pid == Some(pid)) => break,
                other if Instant::now() >= deadline => {
                    panic!(
                        "App::list did not recover after SIGCONT: {:?}",
                        other.map(|apps| apps.len())
                    )
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    #[test]
    #[ignore]
    fn stopped_app_fails_a_locator_wait_within_its_timeout_and_names_the_call() {
        let app = h::app_root();
        let pid = app.pid.expect("test app reports a pid");
        // A read-only wait, so nothing is left queued for the app to act on
        // once it resumes. Submit is visible: with the app running this wait
        // returns at once, so a Timeout below can only come from the stop.
        let target = app.locator(r#"button[name="Submit"]"#);
        target
            .wait_visible(Duration::from_secs(5))
            .expect("Submit is visible while the app runs");

        let start;
        let result;
        {
            let _stopped = Stopped::new(pid);
            start = Instant::now();
            result = target.wait_visible(Duration::from_secs(1));
        }
        let elapsed = start.elapsed();

        // Bounded by the locator's 1 s timeout plus the short pid/state
        // lookup the diagnosis makes, well under the connection's 5 s method
        // timeout that a call ignoring the deadline would wait out.
        assert!(
            elapsed < Duration::from_millis(3500),
            "a 1 s locator wait took {elapsed:?} with the app stopped"
        );
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("wait_visible succeeded with the test app stopped"),
        };
        let Error::Timeout { diagnosis, .. } = &err else {
            panic!("expected Error::Timeout naming the pending call, got: {err}");
        };
        let d = diagnosis.as_deref().expect("timeout carries a diagnosis");
        let pending = d.pending.as_deref().unwrap_or_else(|| {
            panic!("the timeout must name the D-Bus call it was waiting on: {err}")
        });
        // `Interface.Member on /object/path (:bus.name)`, as the deadline
        // recorded it when it cut the call short.
        assert!(
            pending.contains(" on /") && pending.contains("(:"),
            "pending should name the call, object and bus: {pending}"
        );
        assert!(
            err.to_string().contains("still pending"),
            "the message should carry the pending call too: {err}"
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
