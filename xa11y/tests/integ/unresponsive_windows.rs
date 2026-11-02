//! A hung application on the Windows desktop (issue #507).
//!
//! Before the hung check, one app that stopped pumping messages stalled every
//! desktop-wide UIA query for the transaction timeout. The fixture is a
//! WinForms window hosted by PowerShell:
//! - its thread is MTA (`-MTA`), and it blocks in `Thread.Sleep`, which on an
//!   MTA thread pumps nothing (an STA wait, and `Start-Sleep` on PowerShell's
//!   default STA thread, would keep answering sent messages, and the window
//!   would never count as hung);
//! - the form overrides `ShowWithoutActivation`, so it does not take the
//!   foreground from the test app;
//! - it prints a ready line once the form is up, and the test then waits past
//!   Windows' 5 s threshold, so the first listing already sees it hung.
//!
//! It is killed on drop, so a failed assertion cannot leave it on the desktop
//! for the tests after it.
//!
//! Windows-only; the module is gated with `#[cfg(target_os = "windows")]` in
//! `mod.rs`.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use xa11y::*;

    const FIXTURE_TITLE: &str = "xa11y-hung-fixture";
    const READY: &str = "xa11y-hung-fixture-ready";

    /// The hung window's host process, killed on drop.
    struct HungFixture(Child);

    impl HungFixture {
        /// Start the fixture and return once its window is shown and its UI
        /// thread has stopped pumping.
        fn start() -> Self {
            let script = format!(
                r#"
$ErrorActionPreference = 'Stop'
if ([System.Threading.Thread]::CurrentThread.GetApartmentState() -ne 'MTA') {{
    throw 'the hung-window fixture must run on an MTA thread, which does not pump while it sleeps'
}}
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
public class NoActivateForm : System.Windows.Forms.Form {{
    protected override bool ShowWithoutActivation {{ get {{ return true; }} }}
}}
public static class Ghosting {{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    public static extern void DisableProcessWindowsGhosting();
}}
'@
Add-Type -AssemblyName System.Windows.Forms
# Without this, Windows swaps a hung window for a "ghost" window owned by
# dwm, and the hung HWND stops being the one on screen.
[Ghosting]::DisableProcessWindowsGhosting()
$f = New-Object NoActivateForm
$f.Text = '{FIXTURE_TITLE}'
$f.Show()
[System.Windows.Forms.Application]::DoEvents()
if (-not ($f.IsHandleCreated -and $f.Visible)) {{
    throw 'the fixture form is not shown'
}}
[Console]::Out.WriteLine('{READY}')
[Console]::Out.Flush()
[System.Threading.Thread]::Sleep(120000)
"#
            );
            let mut child = Command::new("powershell.exe")
                .args(["-NoProfile", "-MTA", "-Command", &script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start the hung-window fixture");
            let stdout = child.stdout.take().expect("piped stdout");
            let mut stderr = child.stderr.take().expect("piped stderr");
            let mut fixture = HungFixture(child);

            // Read on a helper thread so a fixture that never prints READY
            // cannot block the test: `Some(true)` is READY, `None` is EOF.
            let (tx, rx) = mpsc::channel::<Option<bool>>();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim() == READY {
                        // A send fails only once the test stopped listening.
                        let _ = tx.send(Some(true));
                        return;
                    }
                }
                // As above: a closed receiver means nobody needs the EOF.
                let _ = tx.send(None);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(true)) => {}
                Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = fixture.0.wait().expect("fixture exit status");
                    let mut err = String::new();
                    // Diagnostic only: an unreadable stderr leaves it empty.
                    let _ = stderr.read_to_string(&mut err);
                    panic!(
                        "the hung-window fixture exited before it was ready ({status}); \
                         stderr:\n{err}"
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Kill it first so stderr reaches EOF and can be read.
                    // Best effort: it may have exited on its own meanwhile.
                    let _ = fixture.0.kill();
                    let status = fixture.0.wait().expect("fixture exit status");
                    let mut err = String::new();
                    // Diagnostic only: an unreadable stderr leaves it empty.
                    let _ = stderr.read_to_string(&mut err);
                    panic!(
                        "the hung-window fixture printed no ready line within 10 s ({status}); \
                         PowerShell or WinForms did not start. stderr:\n{err}"
                    );
                }
            }
            // The UI thread is now blocked; Windows calls it hung after 5 s.
            std::thread::sleep(Duration::from_secs(7));
            fixture
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for HungFixture {
        fn drop(&mut self) {
            if let Err(e) = self.0.kill() {
                eprintln!("killing the hung-window fixture failed: {e}");
            }
            // Reap it; the exit status is irrelevant once it was killed.
            let _ = self.0.wait();
        }
    }

    /// Assert `err` is the hung-window Timeout naming the fixture.
    #[track_caller]
    fn assert_names_fixture(err: &Error, pid: u32, what: &str) {
        assert!(
            matches!(err, Error::Timeout { .. }),
            "{what}: expected Error::Timeout for the hung window, got: {err}"
        );
        let observed = err
            .diagnosis()
            .and_then(|d| d.last_observed.clone())
            .unwrap_or_default();
        assert!(
            observed.contains("IsHungAppWindow")
                && observed.contains(&format!("pid {pid}"))
                && observed.contains(FIXTURE_TITLE),
            "{what}: the diagnosis should name the hung window (pid {pid}, \
             {FIXTURE_TITLE:?}): {err}"
        );
    }

    #[test]
    #[ignore]
    fn hung_window_fails_list_and_by_pid_fast_and_is_named() {
        let fixture = HungFixture::start();
        let pid = fixture.pid();

        // Far from the 20 s UIA transaction timeout: the hung check runs first.
        let start = Instant::now();
        let listed = App::list();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "App::list took {elapsed:?} with a hung window on the desktop"
        );
        match listed {
            Err(e) => assert_names_fixture(&e, pid, "App::list"),
            Ok(apps) => panic!(
                "App::list succeeded with a hung window on the desktop; it must report it. \
                 Listed: {:?}",
                apps.iter().map(|a| a.name.clone()).collect::<Vec<_>>()
            ),
        }

        let start = Instant::now();
        let by_pid = App::by_pid(pid, Duration::from_secs(2));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "App::by_pid took {elapsed:?} for a hung app"
        );
        match by_pid {
            Err(e) => assert_names_fixture(&e, pid, "App::by_pid"),
            Ok(_) => panic!("App::by_pid resolved a hung app without reporting it"),
        }

        // Once the hung app is gone, enumeration works again.
        drop(fixture);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match App::list() {
                Ok(_) => break,
                other if Instant::now() >= deadline => {
                    panic!("App::list did not recover after the hung app exited: {other:?}")
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}
