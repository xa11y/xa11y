//! Fuzz target for xa11y-core tree operations (NOT platform providers).
//! Builds random trees via root_element and exercises Element methods:
//! children, parent, subtree, display.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use xa11y_core::{root_element, ElementData, RawPlatformData, Role, StateSet};

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
struct FuzzElement {
    role_idx: u8,
    name: Option<String>,
    value: Option<String>,
    /// How many children this element should have (clamped later).
    child_count: u8,
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    app_name: String,
    pid: Option<u32>,
    screen_w: u32,
    screen_h: u32,
    fuzz_elements: Vec<FuzzElement>,
}

/// Build a valid tree from fuzzer-supplied elements, returning the root Element.
fn build_root(input: &FuzzInput) -> Option<xa11y_core::Element> {
    let max_elements = 256usize;
    let element_count = input.fuzz_elements.len().min(max_elements);

    if element_count == 0 {
        return None;
    }

    let mut elements: Vec<ElementData> = Vec::with_capacity(element_count);
    for i in 0..element_count {
        let fuzz = &input.fuzz_elements[i];
        let role = ROLES[fuzz.role_idx as usize % ROLES.len()];
        elements.push(ElementData {
            role,
            name: fuzz.name.clone(),
            value: fuzz.value.clone(),
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            pid: None,
            raw: RawPlatformData::Synthetic,
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
        if next_child >= element_count {
            break;
        }
        let desired = (input.fuzz_elements[parent_idx].child_count as usize).min(8);
        let actual = desired.min(element_count - next_child);
        for _ in 0..actual {
            let child_idx = next_child;
            next_child += 1;
            elements[child_idx].parent_index = Some(parent_idx as u32);
            elements[parent_idx].children_indices.push(child_idx as u32);
            queue.push(child_idx);
        }

        if next_child >= element_count {
            break;
        }
    }

    // Any remaining unassigned elements become children of the root.
    while next_child < element_count {
        elements[next_child].parent_index = Some(0);
        elements[0].children_indices.push(next_child as u32);
        next_child += 1;
    }

    Some(root_element(
        input.app_name.clone(),
        input.pid,
        (input.screen_w.max(1), input.screen_h.max(1)),
        elements,
    ))
}

fuzz_target!(|input: FuzzInput| {
    let root = match build_root(&input) {
        Some(r) => r,
        None => return,
    };

    // Exercise subtree
    let subtree = root.subtree();
    assert!(!subtree.is_empty());

    // Exercise children on root
    let children = root.children();
    for child in &children {
        // Exercise parent
        let parent = child.parent();
        assert!(parent.is_some());
    }

    // Exercise subtree on a non-root element
    if subtree.len() > 1 {
        let second = &subtree[1];
        let _ = second.subtree();
        let _ = second.children();
        let _ = second.parent();
    }

    // Exercise Display
    let _ = root.to_string();
});
