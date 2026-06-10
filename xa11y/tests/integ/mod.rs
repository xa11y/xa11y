//! Integration test helpers — minimize boilerplate for cross-platform tests.
//!
//! Test bodies live in thematic submodules so this crate stays navigable:
//!
//! * `tree` — tree structure, role coverage, tree methods, element fields,
//!   stateset fields, selector queries, serialization, provider operations.
//! * `actions` — action dispatch, new actions (Blur/SetTextSelection/TypeText),
//!   complex/stress scenarios, action error paths.
//! * `errors` — error paths on the query/wait surface: selector misses,
//!   invalid selectors, `wait_*` timeouts, auto-wait timeouts, unknown
//!   actions, invalid action data.
//! * `events_{macos,windows,linux}` — platform-specific event subscription
//!   end-to-end tests. The `#[cfg(target_os = "…")]` gate lives on the
//!   module declaration below so individual tests don't need it.
//!
//! The helpers in this file (`app_root`, `one`, `named`, `act`, `try_act`)
//! are reached from submodule tests via `use crate::integ as h;`.

pub mod actions;
pub mod errors;
pub mod screenshot;
pub mod tree;

#[cfg(target_os = "macos")]
pub mod events_macos;

#[cfg(target_os = "windows")]
pub mod events_windows;

#[cfg(target_os = "linux")]
pub mod events_linux;

use xa11y::*;

/// Get the test app as an `App`, retrying briefly for registration.
pub fn app_root() -> App {
    // On Linux/macOS, AT-SPI and AX report the process name ("xa11y-test-app").
    // On Windows, UIA reports the window title ("xa11y Test App"). Match either
    // candidate name in a single waited call — `App::find` polls internally, so
    // we no longer interleave names by hand.
    let names = ["xa11y-test-app", "xa11y Test App"];
    App::find(std::time::Duration::from_secs(2), |d| {
        d.name.as_deref().is_some_and(|n| names.contains(&n))
    })
    .unwrap_or_else(|e| panic!("Could not find test app (tried {:?}): {e}", names))
}

/// Find exactly one element by selector within the app. Panics on failure.
pub fn one(app: &App, selector: &str) -> Element {
    let results = app
        .locator(selector)
        .elements()
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. App: {}", selector, e, app));
    assert!(
        results.len() == 1,
        "Selector '{}' matched {} elements (expected 1). App: {}",
        selector,
        results.len(),
        app
    );
    results.into_iter().next().unwrap()
}

/// Find first element whose name contains `substring` (case-insensitive).
pub fn named(app: &App, substring: &str) -> Element {
    let selector = format!("[name*=\"{}\"]", substring);
    let results = app
        .locator(&selector)
        .elements()
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. App: {}", selector, e, app));
    assert!(
        !results.is_empty(),
        "No element with name containing '{}'. App: {}",
        substring,
        app
    );
    results.into_iter().next().unwrap()
}

/// Try to perform an action on an element by name. Returns the result without panicking.
pub fn try_act(element: &Element, action: &str) -> Result<()> {
    element.provider().perform_action(element, action)
}

/// Post-action settle time, read from `XA11Y_TEST_SETTLE_MS` (default 100 ms).
///
/// Panics on an unparsable value rather than silently falling back — a typo'd
/// override should fail the run, not quietly change timing behaviour.
fn settle_duration() -> std::time::Duration {
    let ms = match std::env::var("XA11Y_TEST_SETTLE_MS") {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|e| panic!("XA11Y_TEST_SETTLE_MS={v:?} is not a valid u64: {e}")),
        Err(std::env::VarError::NotPresent) => 100,
        Err(e) => panic!("XA11Y_TEST_SETTLE_MS is not valid unicode: {e}"),
    };
    std::time::Duration::from_millis(ms)
}

/// Perform an action on an element, wait briefly for the app to settle, then
/// re-read the app root.
///
/// The settle time defaults to 100 ms and can be overridden via the
/// `XA11Y_TEST_SETTLE_MS` environment variable (in milliseconds) — raise it
/// on slow machines/CI runners where the action isn't reflected in the
/// re-read tree within the default window.
pub fn act(element: &Element, action: &str) -> App {
    try_act(element, action).unwrap_or_else(|e| panic!("Action '{}' failed: {}", action, e));
    std::thread::sleep(settle_duration());
    app_root()
}
