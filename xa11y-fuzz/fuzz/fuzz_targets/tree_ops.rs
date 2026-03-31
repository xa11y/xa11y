//! Fuzz target for xa11y-core element operations (NOT platform providers).
//! Builds random elements via a mock provider and exercises Element methods:
//! children, parent, display, locator.
#![no_main]

use std::sync::Arc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use xa11y_core::{
    Action, ActionData, ElementData, Error, PermissionStatus, Provider, RawPlatformData, Result,
    Role, StateSet, Subscription,
};

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
    child_count: u8,
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    fuzz_elements: Vec<FuzzElement>,
}

struct FuzzNode {
    data: ElementData,
    children: Vec<usize>,
    parent: Option<usize>,
}

struct FuzzProvider {
    nodes: Vec<FuzzNode>,
}

impl Provider for FuzzProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                if self.nodes.is_empty() {
                    return Ok(vec![]);
                }
                Ok(vec![self.nodes[0].data.clone()])
            }
            Some(el) => {
                let idx = el.handle as usize;
                if idx >= self.nodes.len() {
                    return Ok(vec![]);
                }
                Ok(self.nodes[idx]
                    .children
                    .iter()
                    .map(|&i| self.nodes[i].data.clone())
                    .collect())
            }
        }
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let idx = element.handle as usize;
        if idx >= self.nodes.len() {
            return Ok(None);
        }
        Ok(self.nodes[idx].parent.map(|i| self.nodes[i].data.clone()))
    }

    fn perform_action(
        &self,
        _: &ElementData,
        _: Action,
        _: Option<ActionData>,
    ) -> Result<()> {
        Ok(())
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        Ok(PermissionStatus::Granted)
    }

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "not supported".to_string(),
        })
    }
}

fn build_provider(input: &FuzzInput) -> Option<Arc<FuzzProvider>> {
    let max_elements = 256usize;
    let element_count = input.fuzz_elements.len().min(max_elements);

    if element_count == 0 {
        return None;
    }

    let mut nodes: Vec<FuzzNode> = Vec::with_capacity(element_count);
    for i in 0..element_count {
        let fuzz = &input.fuzz_elements[i];
        let role = ROLES[fuzz.role_idx as usize % ROLES.len()];
        nodes.push(FuzzNode {
            data: ElementData {
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
                handle: i as u64,
            },
            children: vec![],
            parent: None,
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
            nodes[child_idx].parent = Some(parent_idx);
            nodes[parent_idx].children.push(child_idx);
            queue.push(child_idx);
        }
        if next_child >= element_count {
            break;
        }
    }

    // Remaining unassigned elements become children of root.
    while next_child < element_count {
        nodes[next_child].parent = Some(0);
        nodes[0].children.push(next_child);
        next_child += 1;
    }

    Some(Arc::new(FuzzProvider { nodes }))
}

fuzz_target!(|input: FuzzInput| {
    let provider = match build_provider(&input) {
        Some(p) => p,
        None => return,
    };

    // Get root element
    let children = match provider.get_children(None) {
        Ok(c) => c,
        Err(_) => return,
    };
    let root_data = match children.into_iter().next() {
        Some(d) => d,
        None => return,
    };
    let root =
        xa11y_core::Element::new(root_data, Arc::clone(&provider) as Arc<dyn Provider>);

    // Exercise children on root
    if let Ok(children) = root.children() {
        for child in &children {
            // Exercise parent
            if let Ok(Some(_parent)) = child.parent() {
                // parent exists
            }
        }
    }

    // Exercise Display
    let _ = root.to_string();

    // Exercise locator
    let loc = root.locator("button");
    let _ = loc.exists();
    let _ = loc.elements();
});
