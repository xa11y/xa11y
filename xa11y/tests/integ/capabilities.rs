//! The capability table against the backend that is actually running.
//!
//! `xa11y::capabilities()` is static data, so nothing stops it from claiming
//! something the backend does not do. This test walks every row of the
//! running platform's table and either probes it against the AccessKit test
//! app or names it in [`NOT_PROBED`] with the reason. A new row fails the
//! test until it is placed in one of the two, so the table cannot grow
//! claims that no platform cell checks.
//!
//! What a probe asserts depends on the answer:
//!
//! - `unsupported`: the call fails with an unsupported-class error, or the
//!   state reads unknown. This is the claim a consumer acts on ("do not wait
//!   for this"), so it is checked wherever the call is harmless.
//! - `supported`: the call does not fail as unsupported, or the state is
//!   known. Destructive window verbs are not called for this; `windows.rs`
//!   drives them.
//! - `partial`: depends on the toolkit or display server, so no assertion;
//!   the row's note is what the docs promise.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    /// Rows with no probe here, and why. Every other row must have one.
    const NOT_PROBED: &[(&str, &str)] = &[
        ("event:", "an event that never arrives cannot be shown absent by waiting; the supported kinds are driven by events_<platform>.rs"),
        ("table:", "the AccessKit app has no table; rich.rs and the Python rich suites cover tables per platform"),
        ("text:attribute:script", "the AccessKit text entry carries no superscript; rich.rs covers runs"),
        ("property:", "presence depends on what the page or app published, so an absent value proves nothing; semantics.rs reads each property from the AccessKit document fixture"),
        ("window:activate", "not harmful to call, but windows.rs and multi_window.rs already drive it on every platform"),
    ];

    /// Destructive window verbs: only probed where the table says unsupported,
    /// so the call is refused before it reaches the window.
    const WINDOW_VERBS: &[&str] = &[
        "minimize",
        "maximize",
        "enter_fullscreen",
        "restore",
        "close",
        "move_to",
        "resize_to",
    ];

    fn is_unsupported(err: &Error) -> bool {
        matches!(
            err,
            Error::Unsupported { .. } | Error::ActionNotSupported { .. }
        )
    }

    fn check_call(name: &str, cap: &Capability, result: Result<()>) {
        match cap.support {
            Support::Unsupported => {
                let err = result.expect_err(&format!(
                    "{name} is listed unsupported ({:?}) but the call succeeded",
                    cap.note
                ));
                assert!(
                    is_unsupported(&err),
                    "{name} is listed unsupported; the call failed with something else: {err}"
                );
            }
            Support::Supported => {
                if let Err(err) = result {
                    assert!(
                        !is_unsupported(&err),
                        "{name} is listed supported but the call failed as unsupported: {err}"
                    );
                }
            }
            _ => {}
        }
    }

    fn check_state(name: &str, cap: &Capability, value: Option<bool>) {
        match cap.support {
            Support::Unsupported => assert!(
                value.is_none(),
                "{name} is listed unsupported but the window reports {value:?}"
            ),
            Support::Supported => assert!(
                value.is_some(),
                "{name} is listed supported but the window reports it unknown"
            ),
            _ => {}
        }
    }

    #[test]
    #[ignore]
    fn capability_table_matches_the_running_backend() {
        let caps = capabilities().expect("this platform has a table");
        let app = h::app_root();
        let window = app
            .windows()
            .expect("list windows")
            .into_iter()
            .next()
            .expect("the test app has a window");
        let entry = app
            .locator("text_field")
            .elements()
            .expect("find text fields")
            .into_iter()
            .next()
            .expect("the test app has a text field");

        let mut probed = Vec::new();
        for cap in caps.entries() {
            let name = cap.name;
            if NOT_PROBED.iter().any(|(prefix, _)| {
                name == *prefix || (prefix.ends_with(':') && name.starts_with(prefix))
            }) {
                continue;
            }
            match name {
                "action:blur" => {
                    entry
                        .focus()
                        .expect("focus the text field before blurring it");
                    check_call(name, cap, entry.blur());
                }
                "app:enable_web_accessibility" => check_call(
                    name,
                    cap,
                    app.enable_web_accessibility(std::time::Duration::from_millis(500)),
                ),
                "state:minimized" => check_state(name, cap, window.states.minimized),
                "state:maximized" => check_state(name, cap, window.states.maximized),
                "state:fullscreen" => check_state(name, cap, window.states.fullscreen),
                "text:object_at" => check_call(
                    name,
                    cap,
                    entry.provider().text_object_at(entry.data(), 0).map(|_| ()),
                ),
                _ if name.starts_with("text:range_at:") => {
                    // The AccessKit text field has no text interface here:
                    // accesskit_unix implements no AT-SPI Text, and
                    // accesskit_windows exposes TextPattern only for nodes
                    // with TextRun children, which the test app has none of.
                    // These rows are covered by rich.rs (GTK 3) on Linux and
                    // rich_windows.rs (WPF) on Windows instead.
                    if cfg!(any(target_os = "linux", target_os = "windows")) {
                        continue;
                    }
                    let unit = TextUnit::parse(name.trim_start_matches("text:range_at:"))
                        .expect("the row names a text unit");
                    let text = entry.text();
                    check_call(name, cap, text.range_at(0, unit).map(|_| ()));
                }
                _ if name.starts_with("relation:") => {
                    let kind = RelationKind::parse(name.trim_start_matches("relation:"))
                        .expect("the row names a relation kind");
                    check_call(name, cap, entry.relations(&[kind]).map(|_| ()));
                }
                _ if name.starts_with("window:") => {
                    let verb = name.trim_start_matches("window:");
                    assert!(WINDOW_VERBS.contains(&verb), "{name} has no probe");
                    if cap.support != Support::Unsupported {
                        continue;
                    }
                    let result = match verb {
                        "minimize" => window.minimize(),
                        "maximize" => window.maximize(),
                        "enter_fullscreen" => window.enter_fullscreen(),
                        "restore" => window.restore(),
                        "close" => window.close(),
                        "move_to" => window.move_to(10, 10),
                        "resize_to" => window.resize_to(400, 300),
                        other => panic!("{other}: add it to WINDOW_VERBS and here"),
                    };
                    check_call(name, cap, result);
                }
                other => panic!(
                    "capability {other} has no probe in tests/integ/capabilities.rs and is not \
                     listed in NOT_PROBED; add one or the other"
                ),
            }
            probed.push(name);
        }
        assert!(
            !probed.is_empty(),
            "nothing was probed, so the test proves nothing about {}",
            caps.platform
        );
        eprintln!("probed on {}: {probed:?}", caps.platform);
    }
}
