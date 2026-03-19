//! Fuzz target for xa11y-core query engine (NOT platform providers).
//! Builds random trees and queries them with random selectors.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use xa11y_core::{Node, NodeId, QueryOptions, Role, StateSet, Tree};

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
    child_count: u8,
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    nodes: Vec<FuzzNode>,
    selector: String,
}

fn build_tree(fuzz_nodes: &[FuzzNode]) -> Tree {
    let max_nodes = 128usize;
    let node_count = fuzz_nodes.len().min(max_nodes);

    if node_count == 0 {
        return Tree::new(
            0,
            "fuzz-app".to_string(),
            None,
            (1920, 1080),
            vec![],
            QueryOptions::default(),
        );
    }

    let mut nodes: Vec<Node> = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let fuzz = &fuzz_nodes[i];
        let role = ROLES[fuzz.role_idx as usize % ROLES.len()];
        nodes.push(Node {
            id: i as NodeId,
            role,
            name: fuzz.name.clone(),
            value: fuzz.value.clone(),
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            children: vec![],
            parent: None,
            depth: 0,
            app_name: None,
            raw: None,
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
        let desired = (fuzz_nodes[parent_idx].child_count as usize).min(8);
        let actual = desired.min(node_count - next_child);
        let parent_depth = nodes[parent_idx].depth;
        let parent_id = nodes[parent_idx].id;

        for _ in 0..actual {
            let child_idx = next_child;
            next_child += 1;
            nodes[child_idx].parent = Some(parent_id);
            nodes[child_idx].depth = parent_depth + 1;
            nodes[parent_idx].children.push(child_idx as NodeId);
            queue.push(child_idx);
        }
        if next_child >= node_count {
            break;
        }
    }

    // Remaining unassigned nodes become children of root.
    while next_child < node_count {
        nodes[next_child].parent = Some(0);
        nodes[next_child].depth = 1;
        nodes[0].children.push(next_child as NodeId);
        next_child += 1;
    }

    Tree::new(
        0,
        "fuzz-app".to_string(),
        None,
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    )
}

fuzz_target!(|input: FuzzInput| {
    let tree = build_tree(&input.nodes);

    // Query with the fuzz-generated selector. Both parse errors and
    // successful matches are valid outcomes; only panics are bugs.
    let _ = tree.query(&input.selector);

    // Also try querying with some well-known valid selectors against
    // the random tree to exercise the matching logic.
    let _ = tree.query("button");
    let _ = tree.query("[name*=\"test\"]");
    let _ = tree.query("group > button");
    let _ = tree.query("window button:nth(1)");
});
