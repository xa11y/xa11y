//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app root element, retrying briefly for registration.
pub fn app_root() -> Element {
    let provider = xa11y::provider().unwrap();
    for attempt in 0..3 {
        let loc = locator(provider.clone(), r#"application[name*="xa11y"]"#);
        match loc.element() {
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
pub fn one(root: &Element, selector: &str) -> Element {
    let results = root
        .locator(selector)
        .elements()
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. Element: {}", selector, e, root));
    assert!(
        results.len() == 1,
        "Selector '{}' matched {} elements (expected 1). Element: {}",
        selector,
        results.len(),
        root
    );
    results.into_iter().next().unwrap()
}

/// Find first element whose name contains `substring` (case-insensitive).
pub fn named(root: &Element, substring: &str) -> Element {
    let selector = format!("[name*=\"{}\"]", substring);
    let results = root
        .locator(&selector)
        .elements()
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. Element: {}", selector, e, root));
    assert!(
        !results.is_empty(),
        "No element with name containing '{}'. Element: {}",
        substring,
        root
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
pub fn act(element: &Element, action: Action) -> Element {
    act_with(element, action, None)
}

/// Perform an action with data on an element, wait, then re-read the app root.
pub fn act_with(element: &Element, action: Action, data: Option<ActionData>) -> Element {
    try_act_with(element, action, data)
        .unwrap_or_else(|e| panic!("Action {:?} failed: {}", action, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_root()
}
