//! Error-path integration tests for the public query/wait API.
//!
//! Complements the error-path tests in `integ::actions` (app not found,
//! invalid selectors via `elements()`, press on non-interactive elements,
//! expand/collapse on non-expandables) with the surfaces those tests do not
//! touch: single-element resolution (`element()`), the `exists()`/`count()`
//! miss contract, the `wait_*` family's timeout behaviour, the action
//! auto-wait timeout, unknown action names, and up-front action-data
//! validation on the `Locator` shape.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use std::time::Duration;
    use xa11y::*;

    /// Syntactically valid selector that never matches anything in the test
    /// app. Lookups with it must fail fast or honour an explicit timeout.
    const NO_MATCH: &str = r#"button[name="xa11y-no-such-element-3f9a"]"#;

    /// Generous ceiling for operations given a short (500 ms) explicit
    /// timeout — a short timeout must never degrade into the 5 s default.
    const SHORT_TIMEOUT_CEILING: Duration = Duration::from_secs(4);

    #[test]
    #[ignore]
    fn error_element_on_no_match_is_selector_not_matched() {
        let app = h::app_root();
        match app.locator(NO_MATCH).element() {
            Err(Error::SelectorNotMatched { .. }) => {}
            Err(e) => panic!("expected SelectorNotMatched, got: {e}"),
            Ok(_) => panic!("expected SelectorNotMatched, but the selector matched"),
        }
    }

    #[test]
    #[ignore]
    fn error_no_match_exists_false_count_zero() {
        // A selector miss is an *answer* for the multi-result queries, not
        // an error: exists() → false, count() → 0.
        let app = h::app_root();
        let exists = app
            .locator(NO_MATCH)
            .exists()
            .expect("exists() should not error on a selector miss");
        assert!(
            !exists,
            "exists() should be false for a never-matching selector"
        );
        let count = app
            .locator(NO_MATCH)
            .count()
            .expect("count() should not error on a selector miss");
        assert_eq!(
            count, 0,
            "count() should be 0 for a never-matching selector"
        );
    }

    #[test]
    #[ignore]
    fn error_invalid_selector_propagates_through_exists() {
        // exists() swallows only SelectorNotMatched; a parse error must
        // surface as InvalidSelector (tenet 1: no silent fallbacks).
        let app = h::app_root();
        match app.locator("$$$invalid!!!").exists() {
            Err(Error::InvalidSelector { .. }) => {}
            Err(e) => panic!("expected InvalidSelector, got: {e}"),
            Ok(v) => panic!("expected InvalidSelector, got Ok({v})"),
        }
    }

    #[test]
    #[ignore]
    fn error_wait_attached_times_out() {
        let app = h::app_root();
        let start = std::time::Instant::now();
        match app
            .locator(NO_MATCH)
            .wait_attached(Duration::from_millis(500))
        {
            Err(err @ Error::Timeout { .. }) => {
                let Error::Timeout { elapsed, .. } = &err else {
                    unreachable!()
                };
                assert!(
                    *elapsed >= Duration::from_millis(500),
                    "Timeout::elapsed should cover the full wait, got {elapsed:?}"
                );
                // Tenet 6: the timeout must say what it was waiting for and
                // what it observed, on every platform provider.
                let d = err
                    .diagnosis()
                    .expect("wait timeout must carry a diagnosis");
                assert_eq!(d.condition.as_deref(), Some("attached"));
                assert_eq!(d.selector.as_deref(), Some(NO_MATCH));
                // A prefix, not equality: the miss context is read on a short
                // budget after the deadline, and a check it cuts short is
                // reported after this text (tenet 1) rather than dropped.
                let observed = d.last_observed.as_deref().unwrap_or_default();
                assert!(
                    observed.starts_with("selector never matched"),
                    "unexpected last_observed: {observed:?}"
                );
                assert!(
                    d.scope.is_some(),
                    "a never-matched wait should include a bounded scope snapshot"
                );
            }
            Err(e) => panic!("expected Timeout, got: {e}"),
            Ok(_) => panic!("expected Timeout, but the selector matched"),
        }
        assert!(
            start.elapsed() < SHORT_TIMEOUT_CEILING,
            "wait_attached(500ms) took {:?} — the explicit short timeout was not honoured",
            start.elapsed()
        );
    }

    #[test]
    #[ignore]
    fn error_wait_detached_times_out_for_persistent_element() {
        // Submit never detaches, so a short wait_detached must time out.
        let app = h::app_root();
        match app
            .locator(r#"[name*="Submit"]"#)
            .wait_detached(Duration::from_millis(500))
        {
            Err(Error::Timeout { .. }) => {}
            Err(e) => panic!("expected Timeout, got: {e}"),
            Ok(()) => panic!("expected Timeout, but Submit detached"),
        }
    }

    #[test]
    #[ignore]
    fn error_action_auto_wait_times_out() {
        // Action methods auto-wait on the selector; a never-matching
        // selector must surface Timeout after the configured window.
        let app = h::app_root();
        let start = std::time::Instant::now();
        let result = app
            .locator(NO_MATCH)
            .with_timeout(Duration::from_millis(500))
            .press();
        assert!(
            matches!(result, Err(Error::Timeout { .. })),
            "press() on a never-matching selector should be Timeout, got: {result:?}"
        );
        assert!(
            start.elapsed() < SHORT_TIMEOUT_CEILING,
            "press() with a 500ms auto-wait took {:?}",
            start.elapsed()
        );
    }

    #[test]
    #[ignore]
    fn error_unknown_action_is_action_not_supported() {
        // All providers reject unknown action names with ActionNotSupported
        // before touching the platform.
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        let result = h::try_act(&submit, "frobnicate");
        assert!(
            matches!(result, Err(Error::ActionNotSupported { .. })),
            "unknown action name should be ActionNotSupported, got: {result:?}"
        );
    }

    #[test]
    #[ignore]
    fn error_select_text_inverted_range_is_invalid_text_range() {
        // Range validation happens in the Locator before selector
        // resolution, so this fails fast regardless of the target.
        let app = h::app_root();
        let result = app.locator(r#"[name*="Submit"]"#).select_text(5, 2);
        assert!(
            matches!(result, Err(Error::InvalidTextRange { .. })),
            "select_text(5, 2) should be InvalidTextRange, got: {result:?}"
        );
    }

    #[test]
    #[ignore]
    fn error_set_numeric_value_nan_via_locator_is_invalid_action_data() {
        // The Locator shape validates NaN up-front without burning the
        // auto-wait window (the Element shape is covered in integ::actions).
        let app = h::app_root();
        let start = std::time::Instant::now();
        let result = app.locator("slider").set_numeric_value(f64::NAN);
        assert!(
            matches!(result, Err(Error::InvalidActionData { .. })),
            "set_numeric_value(NaN) should be InvalidActionData, got: {result:?}"
        );
        assert!(
            start.elapsed() < SHORT_TIMEOUT_CEILING,
            "NaN validation should fail fast, took {:?}",
            start.elapsed()
        );
    }

    /// `enable_web_accessibility` on an app with no web content. On Linux and
    /// windows the switch is not a client request at all, so the answer is
    /// `Unsupported` naming what to do instead, without touching the app. On
    /// macOS the request is made, and exactly two outcomes are accepted, each
    /// for a stated reason:
    ///
    /// - `AccessibilityNotEnabled`: the app neither accepted the
    ///   `AXManualAccessibility` write nor reads it back as set. The error must name the attribute and the relaunch flag.
    /// - `Timeout`: the app kept the value, so the request counts as taken,
    ///   but a native app has no `web_area`, so the wait ends with the
    ///   diagnosis "no web_area in the app's tree".
    ///
    /// Which one a given AppKit-style app produces depends on whether it
    /// retains `AXManualAccessibility`; the test does not assume either, but
    /// it does not accept a success or any other error.
    #[test]
    #[ignore]
    fn error_enable_web_accessibility_on_a_native_app() {
        let app = h::app_root();
        let start = std::time::Instant::now();
        let err = app
            .enable_web_accessibility(Duration::from_millis(500))
            .expect_err("a native app has no web content to enable");
        assert!(start.elapsed() < SHORT_TIMEOUT_CEILING, "{err}");
        match err {
            #[cfg(target_os = "linux")]
            Error::Unsupported { feature } => {
                assert!(
                    feature.contains("--force-renderer-accessibility"),
                    "{feature}"
                );
                assert!(feature.contains("ACCESSIBILITY_ENABLED=1"), "{feature}");
            }
            #[cfg(target_os = "windows")]
            Error::Unsupported { feature } => {
                assert!(feature.contains("UI Automation"), "{feature}");
            }
            // Outcome 1: the app refused the switch.
            #[cfg(target_os = "macos")]
            Error::AccessibilityNotEnabled { instructions, .. } => {
                assert!(
                    instructions.contains("AXManualAccessibility"),
                    "{instructions}"
                );
                assert!(
                    instructions.contains("--force-renderer-accessibility"),
                    "{instructions}"
                );
            }
            // Outcome 2: the switch was retained, and the app has no web area.
            #[cfg(target_os = "macos")]
            e @ Error::Timeout { .. } => {
                let observed = e.diagnosis().and_then(|d| d.last_observed.clone());
                assert_eq!(
                    observed.as_deref(),
                    Some("no web_area in the app's tree"),
                    "{e}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
