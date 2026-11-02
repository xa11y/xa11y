//! A stopped application on the AT-SPI bus (issue #484).
//!
//! Enumeration reads every registered application's name, so before the
//! connection carried a method timeout one `SIGSTOP`ped app hung `App::list`
//! for every caller, forever. These tests stop the test app itself — the only
//! fixture the harness guarantees — and always resume it, even on panic, so
//! the tests after them see a live app again.
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
}
