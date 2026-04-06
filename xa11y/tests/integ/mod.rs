//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app as an `App`, retrying briefly for registration.
pub fn app_root() -> App {
    for attempt in 0..3 {
        match App::by_name("xa11y-test-app") {
            Ok(app) => return app,
            Err(_) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => panic!("Could not find test app after retries: {}", e),
        }
    }
    unreachable!()
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

/// Try to perform an action on an element. Returns the result without panicking.
pub fn try_act(element: &Element, action: Action) -> Result<()> {
    try_act_with(element, action, None)
}

/// Try to perform an action with data on an element.
pub fn try_act_with(element: &Element, action: Action, data: Option<ActionData>) -> Result<()> {
    element.provider().perform_action(element, action, data)
}

/// Perform an action on an element, wait briefly, then re-read the app root.
pub fn act(element: &Element, action: Action) -> App {
    act_with(element, action, None)
}

/// Perform an action with data on an element, wait, then re-read the app root.
pub fn act_with(element: &Element, action: Action, data: Option<ActionData>) -> App {
    let action_dbg = format!("{:?}", action);
    try_act_with(element, action, data)
        .unwrap_or_else(|e| panic!("Action {} failed: {}", action_dbg, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_root()
}
