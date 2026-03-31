//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app tree, retrying briefly for registration.
pub fn app_tree() -> Element {
    for attempt in 0..6 {
        match App::from_name(xa11y::provider().unwrap(), "xa11y") {
            Ok(app) => return app.elements().expect("Failed to snapshot app tree"),
            Err(_) if attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("Could not find test app after retries: {}", e),
        }
    }
    unreachable!()
}

/// Query elements matching a CSS-like selector within a snapshot.
pub fn query(root: &Element, selector: &str) -> Result<Vec<Element>> {
    root.query_selector(selector)
}

/// Find exactly one element by selector. Panics with tree dump on failure.
pub fn one(root: &Element, selector: &str) -> Element {
    let results = query(root, selector)
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. Tree:\n{}", selector, e, root));
    assert!(
        results.len() == 1,
        "Selector '{}' matched {} elements (expected 1). Tree:\n{}",
        selector,
        results.len(),
        root
    );
    results[0].clone()
}

/// Find first element whose name contains `substring` (case-insensitive).
pub fn named(root: &Element, substring: &str) -> Element {
    let selector = format!("[name*=\"{}\"]", substring);
    let results = query(root, &selector)
        .unwrap_or_else(|e| panic!("Selector '{}' failed: {}. Tree:\n{}", selector, e, root));
    assert!(
        !results.is_empty(),
        "No element with name containing '{}'. Tree:\n{}",
        substring,
        root
    );
    results[0].clone()
}

/// Try to perform an action on an element. Returns the result without panicking.
pub fn try_act(element: &Element, action: Action) -> Result<()> {
    try_act_with(element, action, None)
}

/// Try to perform an action with data on an element. Returns the result without panicking.
pub fn try_act_with(element: &Element, action: Action, data: Option<ActionData>) -> Result<()> {
    xa11y::perform_action(element, action, data)
}

/// Perform an action on an element, wait briefly, then re-read the tree.
pub fn act(element: &Element, action: Action) -> Element {
    act_with(element, action, None)
}

/// Perform an action with data on an element, then re-read the tree.
/// The provider already pauses briefly after each action for state to settle.
pub fn act_with(element: &Element, action: Action, data: Option<ActionData>) -> Element {
    try_act_with(element, action, data)
        .unwrap_or_else(|e| panic!("Action {:?} failed: {}", action, e));
    app_tree()
}
