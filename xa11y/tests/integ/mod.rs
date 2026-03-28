//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app tree with default options, retrying briefly for registration.
pub fn app_tree() -> Node {
    app_tree_with(&QueryOptions::default())
}

/// Get the test app tree with custom QueryOptions, retrying for registration.
pub fn app_tree_with(opts: &QueryOptions) -> Node {
    for attempt in 0..3 {
        match xa11y::app(&AppTarget::ByName("xa11y".to_string()), opts) {
            Ok(root) => return root,
            Err(_) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => panic!("Could not find test app after retries: {}", e),
        }
    }
    unreachable!()
}

/// Find exactly one node by selector. Panics with tree dump on failure.
pub fn one(root: &Node, selector: &str) -> Node {
    let results = root.query(selector).unwrap_or_else(|e| {
        panic!(
            "Selector '{}' failed: {}. Tree:\n{}",
            selector,
            e,
            root.dump()
        )
    });
    assert!(
        results.len() == 1,
        "Selector '{}' matched {} nodes (expected 1). Tree:\n{}",
        selector,
        results.len(),
        root.dump()
    );
    results[0].clone()
}

/// Find first node whose name contains `substring` (case-insensitive).
pub fn named(root: &Node, substring: &str) -> Node {
    let selector = format!("[name*=\"{}\"]", substring);
    let results = root.query(&selector).unwrap_or_else(|e| {
        panic!(
            "Selector '{}' failed: {}. Tree:\n{}",
            selector,
            e,
            root.dump()
        )
    });
    assert!(
        !results.is_empty(),
        "No node with name containing '{}'. Tree:\n{}",
        substring,
        root.dump()
    );
    results[0].clone()
}

/// Try to perform an action on a node. Returns the result without panicking.
pub fn try_act(node: &Node, action: Action) -> Result<()> {
    try_act_with(node, action, None)
}

/// Try to perform an action with data on a node. Returns the result without panicking.
pub fn try_act_with(node: &Node, action: Action, data: Option<ActionData>) -> Result<()> {
    xa11y::perform_action(node, action, data)
}

/// Perform an action on a node, wait briefly, then re-read the tree.
pub fn act(node: &Node, action: Action) -> Node {
    act_with(node, action, None)
}

/// Perform an action with data on a node, wait, then re-read the tree.
pub fn act_with(node: &Node, action: Action, data: Option<ActionData>) -> Node {
    try_act_with(node, action, data)
        .unwrap_or_else(|e| panic!("Action {:?} failed: {}", action, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_tree()
}
