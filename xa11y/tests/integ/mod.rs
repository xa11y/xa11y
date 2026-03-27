//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::*;

/// Get the test app tree with default options, retrying briefly for registration.
pub fn app_tree() -> Tree {
    app_tree_with(&QueryOptions::default())
}

/// Get the test app tree with custom QueryOptions, retrying for registration.
pub fn app_tree_with(opts: &QueryOptions) -> Tree {
    for attempt in 0..3 {
        match provider().and_then(|p| p.get_app_tree(&AppTarget::ByName("xa11y".to_string()), opts))
        {
            Ok(tree) => return tree,
            Err(_) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => panic!("Could not find test app after retries: {}", e),
        }
    }
    unreachable!()
}

/// Create a platform provider for direct use.
pub fn provider() -> Result<std::sync::Arc<dyn Provider>> {
    xa11y::create_provider()
}

/// Find exactly one node by selector. Panics with tree dump on failure.
pub fn one<'a>(tree: &'a Tree, selector: &str) -> &'a RawNode {
    let results = tree.query(selector).unwrap_or_else(|e| {
        panic!(
            "Selector '{}' failed: {}. Tree:\n{}",
            selector,
            e,
            tree.dump()
        )
    });
    assert!(
        results.len() == 1,
        "Selector '{}' matched {} nodes (expected 1). Tree:\n{}",
        selector,
        results.len(),
        tree.dump()
    );
    results[0]
}

/// Find first node whose name contains `substring` (case-insensitive).
pub fn named<'a>(tree: &'a Tree, substring: &str) -> &'a RawNode {
    let selector = format!("[name*=\"{}\"]", substring);
    let results = tree.query(&selector).unwrap_or_else(|e| {
        panic!(
            "Selector '{}' failed: {}. Tree:\n{}",
            selector,
            e,
            tree.dump()
        )
    });
    assert!(
        !results.is_empty(),
        "No node with name containing '{}'. Tree:\n{}",
        substring,
        tree.dump()
    );
    results[0]
}

/// Try to perform an action on a node. Returns the result without panicking.
pub fn try_act(tree: &Tree, node: &RawNode, action: Action) -> Result<()> {
    try_act_with(tree, node, action, None)
}

/// Try to perform an action with data on a node. Returns the result without panicking.
pub fn try_act_with(
    tree: &Tree,
    node: &RawNode,
    action: Action,
    data: Option<ActionData>,
) -> Result<()> {
    xa11y::perform_action(tree, node, action, data)
}

/// Perform an action on a node, wait briefly, then re-read the tree.
pub fn act(tree: &Tree, node: &RawNode, action: Action) -> Tree {
    act_with(tree, node, action, None)
}

/// Perform an action with data on a node, wait, then re-read the tree.
pub fn act_with(tree: &Tree, node: &RawNode, action: Action, data: Option<ActionData>) -> Tree {
    try_act_with(tree, node, action, data)
        .unwrap_or_else(|e| panic!("Action {:?} failed: {}", action, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_tree()
}
