//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app as an `App`, retrying briefly for registration.
pub fn app_root() -> App {
    // On Linux/macOS, AT-SPI and AX report the process name ("xa11y-test-app").
    // On Windows, UIA reports the window title ("xa11y Test App").
    // Two candidate names to interleave, so we keep the manual loop (a
    // single `by_name_timeout` per name would block on the wrong name for
    // its full timeout before trying the right one).
    let names = ["xa11y-test-app", "xa11y Test App"];
    for attempt in 0..3 {
        for name in &names {
            if let Ok(app) = App::by_name(name) {
                return app;
            }
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    panic!("Could not find test app after retries (tried {:?})", names);
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

/// Perform an action on an element, wait briefly, then re-read the app root.
pub fn act(element: &Element, action: &str) -> App {
    try_act(element, action).unwrap_or_else(|e| panic!("Action '{}' failed: {}", action, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_root()
}
