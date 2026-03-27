//! Integration test helpers — minimize boilerplate for cross-platform tests.

use xa11y::action::{Action, ActionData};
use xa11y::*;

/// Create a provider for the current platform.
pub fn provider() -> Box<dyn Provider> {
    match create_provider() {
        Ok(p) => p,
        Err(e) => panic!("Provider unavailable: {}", e),
    }
}

/// Create an event provider for the current platform.
pub fn event_provider() -> Box<dyn EventProvider> {
    match create_event_provider() {
        Ok(p) => p,
        Err(e) => panic!("EventProvider unavailable: {}", e),
    }
}

/// Get the test app tree with default options, retrying briefly for registration.
pub fn app_tree(p: &dyn Provider) -> Tree {
    app_tree_with(p, &QueryOptions::default())
}

/// Get the test app tree with custom QueryOptions, retrying for registration.
pub fn app_tree_with(p: &dyn Provider, opts: &QueryOptions) -> Tree {
    for attempt in 0..3 {
        match p.get_app_tree(&AppTarget::ByName("xa11y".to_string()), opts) {
            Ok(tree) => return tree,
            Err(_) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => panic!("Could not find test app after retries: {}", e),
        }
    }
    unreachable!()
}

/// Find exactly one node by selector. Panics with tree dump on failure.
pub fn one<'a>(tree: &'a Tree, selector: &str) -> &'a Node {
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
pub fn named<'a>(tree: &'a Tree, substring: &str) -> &'a Node {
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

/// Perform an action on a node, wait briefly, then re-read the tree.
pub fn act(p: &dyn Provider, tree: &Tree, node: &Node, action: Action) -> Tree {
    act_with(p, tree, node, action, None)
}

/// Perform an action with data on a node, wait, then re-read the tree.
pub fn act_with(
    p: &dyn Provider,
    tree: &Tree,
    node: &Node,
    action: Action,
    data: Option<ActionData>,
) -> Tree {
    p.perform_action(tree, node, action, data)
        .unwrap_or_else(|e| panic!("Action {:?} failed: {}", action, e));
    std::thread::sleep(std::time::Duration::from_millis(100));
    app_tree(p)
}
