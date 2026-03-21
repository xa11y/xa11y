//! Fuzz target for xa11y-core tree operations (NOT platform providers).
//! Builds random trees and exercises Tree methods: get, root, iter, children,
//! subtree, find_by_role, find_by_name, dump, query, len, is_empty.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use xa11y_core::{Node, QueryOptions, Role, StateSet, Tree};

/// Roles indexed by u8 for fuzzer-driven selection.
const ROLES: [Role; 33] = [
    Role::Unknown,
    Role::Window,
    Role::Application,
    Role::Button,
    Role::CheckBox,
    Role::RadioButton,
    Role::TextField,
    Role::TextArea,
    Role::StaticText,
    Role::ComboBox,
    Role::List,
    Role::ListItem,
    Role::Menu,
    Role::MenuItem,
    Role::MenuBar,
    Role::Tab,
    Role::TabGroup,
    Role::Table,
    Role::TableRow,
    Role::TableCell,
    Role::Toolbar,
    Role::ScrollBar,
    Role::Slider,
    Role::Image,
    Role::Link,
    Role::Group,
    Role::Dialog,
    Role::Alert,
    Role::ProgressBar,
    Role::TreeItem,
    Role::WebArea,
    Role::Heading,
    Role::Separator,
];

#[derive(Arbitrary, Debug)]
struct FuzzNode {
    role_idx: u8,
    name: Option<String>,
    value: Option<String>,
    /// How many children this node should have (clamped later).
    child_count: u8,
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    app_name: String,
    pid: Option<u32>,
    screen_w: u32,
    screen_h: u32,
    fuzz_nodes: Vec<FuzzNode>,
    /// A selector string to try querying with.
    selector: String,
    /// A name pattern to search for.
    name_pattern: String,
    /// Role index for find_by_role.
    role_idx: u8,
}

/// Build a valid tree from fuzzer-supplied nodes.
fn build_tree(input: &FuzzInput) -> Tree {
    let max_nodes = 256usize;
    let node_count = input.fuzz_nodes.len().min(max_nodes);

    if node_count == 0 {
        return Tree::new(
            input.app_name.clone(),
            input.pid,
            (input.screen_w.max(1), input.screen_h.max(1)),
            vec![],
            QueryOptions::default(),
        );
    }

    let mut nodes: Vec<Node> = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let fuzz = &input.fuzz_nodes[i];
        let role = ROLES[fuzz.role_idx as usize % ROLES.len()];
        nodes.push(Node {
            role,
            name: fuzz.name.clone(),
            value: fuzz.value.clone(),
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            depth: 0,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: None,
            index: i as u32,
            children_indices: vec![],
            parent_index: None,
        });
    }

    // Assign parent/child relationships via BFS.
    let mut next_child: usize = 1;
    let mut queue: Vec<usize> = vec![0];

    while let Some(parent_idx) = queue.first().copied() {
        queue.remove(0);
        if next_child >= node_count {
            break;
        }
        let desired = (input.fuzz_nodes[parent_idx].child_count as usize).min(8);
        let actual = desired.min(node_count - next_child);
        let parent_depth = nodes[parent_idx].depth;

        for _ in 0..actual {
            let child_idx = next_child;
            next_child += 1;
            nodes[child_idx].parent_index = Some(parent_idx as u32);
            nodes[child_idx].depth = parent_depth + 1;
            nodes[parent_idx].children_indices.push(child_idx as u32);
            queue.push(child_idx);
        }

        if next_child >= node_count {
            break;
        }
    }

    // Any remaining unassigned nodes become children of the root.
    while next_child < node_count {
        let root_depth = nodes[0].depth;
        nodes[next_child].parent_index = Some(0);
        nodes[next_child].depth = root_depth + 1;
        nodes[0].children_indices.push(next_child as u32);
        next_child += 1;
    }

    Tree::new(
        input.app_name.clone(),
        input.pid,
        (input.screen_w.max(1), input.screen_h.max(1)),
        nodes,
        QueryOptions::default(),
    )
}

fuzz_target!(|input: FuzzInput| {
    let tree = build_tree(&input);

    // Exercise len / is_empty
    let _ = tree.len();
    let _ = tree.is_empty();

    if tree.is_empty() {
        return;
    }

    // Exercise root (safe because tree is non-empty)
    let root = tree.root();
    let _ = root.role;

    // Exercise get with valid and invalid indices
    for i in 0..tree.len() as u32 + 2 {
        let _ = tree.get(i);
    }

    // Exercise iter
    let count = tree.iter().count();
    assert_eq!(count, tree.len());

    // Exercise children
    for node in tree.iter() {
        let _ = tree.children(node);
    }

    // Exercise subtree on root and a few nodes
    let _ = tree.subtree(root);
    if tree.len() > 1 {
        if let Some(node) = tree.get(1) {
            let _ = tree.subtree(node);
        }
    }

    // Exercise find_by_role
    let role = ROLES[input.role_idx as usize % ROLES.len()];
    let _ = tree.find_by_role(role);

    // Exercise find_by_name
    let _ = tree.find_by_name(&input.name_pattern);

    // Exercise dump
    let _ = tree.dump();

    // Exercise query (selector may be invalid, that's fine)
    let _ = tree.query(&input.selector);
});
