//! End-to-end xa11y example: drive the AccessKit test app from launch to teardown.
//!
//! This binary is a complete, copy-pasteable starting point for writing your
//! first xa11y program in Rust. It targets the AccessKit test app shipped
//! with this repo (`test-apps/accesskit`) so it runs identically on Linux,
//! macOS, and Windows.
//!
//! What it demonstrates:
//!
//! * Launching a test app and polling the accessibility API until the OS
//!   registers it (`App::by_pid` with a `Duration` timeout).
//! * Dumping the tree (`App::dump`) to discover the role and name of every
//!   element before writing selectors.
//! * The `Locator` pattern with auto-waiting actions (`press`, `set_value`).
//! * Wait helpers: `wait_visible`, `wait_until`.
//! * Reading element fields (`role`, `name`, `actions`, `states.checked`).
//! * Subscribing to events with `App::subscribe` and `Subscription::wait_for`.
//! * Tearing the child process down cleanly with a panic-safe guard.
//!
//! Run from the repo root, after building the test app:
//!
//! ```bash
//! cargo build -p xa11y-test-app
//! cargo run -p xa11y-example-end-to-end
//! ```

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use xa11y::{App, AppExt, Error};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

fn binary_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut p = PathBuf::from(manifest_dir);
    p.pop(); // examples/
    p.pop(); // repo root
    p.push("target");
    p.push("debug");
    p.push(if cfg!(windows) {
        "xa11y-test-app.exe"
    } else {
        "xa11y-test-app"
    });
    p
}

/// Panic-safe child guard: kills the child when dropped.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_registration(pid: u32) -> Result<App, Error> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut last = None;
    while Instant::now() < deadline {
        match App::by_pid(pid, Duration::from_secs(1)) {
            Ok(app) => return Ok(app),
            Err(err) => {
                last = Some(err);
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
    Err(last.unwrap_or(Error::Timeout {
        elapsed: STARTUP_TIMEOUT,
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = binary_path();
    if !binary.exists() {
        return Err(format!(
            "Build the test app first: cargo build -p xa11y-test-app (looked at {})",
            binary.display()
        )
        .into());
    }

    // 1. Launch the test app. We wrap the child in a guard so a panic later
    //    in the example still terminates the subprocess.
    let child = Command::new(&binary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id();
    let _guard = ChildGuard(child);

    // 2. Poll the accessibility API until the OS registers the new process.
    let app = wait_for_registration(pid)?;
    println!("App registered: {} (pid={:?})", app.name, app.pid);

    // 3. Dump the tree once to discover the role/name of every element. Copy
    //    selectors out of this output instead of guessing.
    println!("\n--- Tree (depth 4) ---");
    println!("{}", app.dump(Some(4))?);

    // 4. Locators auto-wait and re-resolve on every call, so they stay
    //    correct even if the UI mutates between operations.
    let submit = app.locator(r#"button[name="Submit"]"#);
    submit.wait_visible(Duration::from_secs(5))?;

    // 5. Read element fields via `.element()`.
    let button = submit.element()?;
    assert_eq!(button.role, xa11y::Role::Button);
    assert!(button.states.enabled, "Submit should be enabled at startup");
    assert!(button.actions.iter().any(|a| a == "press"));

    // 6. Press the primary button.
    submit.press()?;

    // 7. Drive a text input. `wait_until` polls until the predicate is
    //    true — preferable to a fixed `thread::sleep`.
    //
    //    Some platform providers don't implement editable-text writes for
    //    every widget (e.g. Linux AT-SPI's AccessKit bridge doesn't expose
    //    `EditableText`). Real apps usually expose it via Qt/GTK; the test
    //    app here is pure AccessKit, so we tolerate the error explicitly
    //    rather than swallowing it silently.
    let name_field = app.locator(r#"text_field[name="Name"]"#);
    match name_field.set_value("Ada Lovelace") {
        Ok(()) => {
            let wait_result = name_field.wait_until(
                |el| el.and_then(|d| d.value.as_deref()) == Some("Ada Lovelace"),
                Duration::from_secs(2),
            );
            if let Err(Error::Timeout { .. }) = wait_result {
                println!("note: text value not echoed back via accessibility (adapter quirk)");
            } else {
                wait_result?;
            }
        }
        Err(Error::TextValueNotSupported) => {
            println!("note: set_value not supported by this provider (e.g. Linux AT-SPI on AccessKit)");
        }
        Err(e) => return Err(e.into()),
    }

    // 8. Toggle the checkbox via the `press` semantic verb and confirm the
    //    new state with `wait_until`.
    let checkbox = app.locator(r#"check_box[name="I agree to terms"]"#);
    let before = checkbox.element()?.states.checked;
    checkbox.press()?;
    checkbox.wait_until(
        |el| el.map(|d| d.states.checked) != Some(before),
        Duration::from_secs(2),
    )?;
    let after = checkbox.element()?.states.checked;
    println!("checkbox toggled: {:?} -> {:?}", before, after);
    assert_ne!(before, after);

    // 9. Iterate matching elements with `.elements()`.
    let buttons = app.locator("button").elements()?;
    println!("discovered {} buttons total", buttons.len());
    assert!(buttons.len() >= 2);

    // 10. Subscribe to events, trigger a press, and wait for the next event.
    //     In real code you'd filter the predicate by `e.kind` (FocusChanged,
    //     ValueChanged, StateChanged, ...) and/or by `e.target` fields. Here
    //     we just demonstrate the API by waiting for any event — pressing
    //     Submit on the test app mutates `status_text`, so an event is
    //     guaranteed to fire shortly after.
    let sub = app.subscribe()?;
    submit.press()?;
    let event = sub.wait_for(|_| true, Duration::from_secs(5))?;
    println!(
        "observed event: {:?} on {:?}",
        event.kind,
        event.target.as_ref().and_then(|t| t.name.as_deref())
    );

    println!("\nOK — example completed successfully.");
    Ok(())
}
