//! Shared mock Provider infrastructure for fuzz targets.
//!
//! Included via `#[path = "mock.rs"] mod mock;` in each fuzz target.
//! The tree topology, element roles, names, and all fields are fully
//! fuzz-driven so selectors are exercised against varied accessibility trees.

use arbitrary::Arbitrary;
use std::sync::Arc;
use xa11y::{ElementData, Error, Provider, Rect, Result, Role, StateSet, Subscription, Toggled};

// ── Role and Action tables ────────────────────────────────────────────────────

pub const ROLES: [Role; 33] = [
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

pub const ALL_ACTIONS: &[&str] = &[
    "press",
    "focus",
    "set_value",
    "toggle",
    "expand",
    "collapse",
    "select",
    "show_menu",
    "scroll_into_view",
    "increment",
    "decrement",
    "blur",
    "set_text_selection",
    "type_text",
];

// ── Fuzz element types ────────────────────────────────────────────────────────

#[derive(Arbitrary, Debug)]
pub struct FuzzRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Arbitrary, Debug)]
pub struct FuzzStateSet {
    pub enabled: bool,
    pub visible: bool,
    pub focused: bool,
    /// None = not checkable; Some(v % 3) → Off / On / Mixed
    pub checked: Option<u8>,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub editable: bool,
    pub focusable: bool,
    pub modal: bool,
    pub required: bool,
    pub busy: bool,
}

#[derive(Arbitrary, Debug)]
pub struct FuzzRawPlatform {
    pub key: String,
    pub value: Option<String>,
}

/// Full ElementData shape exposed to the fuzzer.
#[derive(Arbitrary, Debug)]
pub struct FuzzElement {
    pub role_idx: u8,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub bounds: Option<FuzzRect>,
    pub states: FuzzStateSet,
    pub stable_id: Option<String>,
    pub numeric_value: Option<f64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pid: Option<u32>,
    pub raw: FuzzRawPlatform,
    pub child_count: u8,
    pub action_idxs: Vec<u8>,
}

// ── Mock Provider ─────────────────────────────────────────────────────────────

pub struct FuzzNode {
    pub data: ElementData,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

pub struct FuzzProvider {
    pub nodes: Vec<FuzzNode>,
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

    fn list_apps(&self) -> Result<Vec<ElementData>> {
        if self.nodes.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![self.nodes[0].data.clone()])
    }

    fn press(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn focus(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn blur(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn toggle(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn select(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn expand(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn collapse(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn show_menu(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn increment(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn decrement(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn scroll_into_view(&self, _: &ElementData) -> Result<()> { Ok(()) }
    fn set_value(&self, _: &ElementData, _: &str) -> Result<()> { Ok(()) }
    fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> { Ok(()) }
    fn type_text(&self, _: &ElementData, _: &str) -> Result<()> { Ok(()) }
    fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> { Ok(()) }
    fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> { Ok(()) }

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "not supported".to_string(),
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn make_state(s: &FuzzStateSet) -> StateSet {
    StateSet {
        enabled: s.enabled,
        visible: s.visible,
        focused: s.focused,
        checked: s.checked.map(|v| match v % 3 {
            0 => Toggled::Off,
            1 => Toggled::On,
            _ => Toggled::Mixed,
        }),
        selected: s.selected,
        expanded: s.expanded,
        editable: s.editable,
        focusable: s.focusable,
        modal: s.modal,
        required: s.required,
        busy: s.busy,
    }
}

pub fn make_raw(r: &FuzzRawPlatform) -> std::collections::HashMap<String, serde_json::Value> {
    let mut map = std::collections::HashMap::new();
    if !r.key.is_empty() {
        let val = match &r.value {
            Some(v) => serde_json::Value::String(v.clone()),
            None => serde_json::Value::Null,
        };
        map.insert(r.key.clone(), val);
    }
    map
}

/// Build a random mock provider from fuzz-driven elements.
///
/// Node 0 is forced to `Role::Application` so `App::list_with` always
/// finds at least one app, ensuring Locator code paths are exercised.
pub fn build_provider(elements: &[FuzzElement]) -> Option<Arc<FuzzProvider>> {
    const MAX_ELEMENTS: usize = 256;
    let element_count = elements.len().min(MAX_ELEMENTS);
    if element_count == 0 {
        return None;
    }

    let mut nodes: Vec<FuzzNode> = Vec::with_capacity(element_count);
    for i in 0..element_count {
        let fuzz = &elements[i];
        let role = if i == 0 {
            Role::Application
        } else {
            ROLES[fuzz.role_idx as usize % ROLES.len()]
        };
        let actions: Vec<String> = fuzz
            .action_idxs
            .iter()
            .map(|&idx| ALL_ACTIONS[idx as usize % ALL_ACTIONS.len()].to_string())
            .collect();
        nodes.push(FuzzNode {
            data: ElementData {
                role,
                name: fuzz.name.clone(),
                value: fuzz.value.clone(),
                description: fuzz.description.clone(),
                bounds: fuzz.bounds.as_ref().map(|b| Rect {
                    x: b.x,
                    y: b.y,
                    width: b.width,
                    height: b.height,
                }),
                actions,
                states: make_state(&fuzz.states),
                stable_id: fuzz.stable_id.clone(),
                numeric_value: fuzz.numeric_value,
                min_value: fuzz.min_value,
                max_value: fuzz.max_value,
                pid: fuzz.pid,
                raw: make_raw(&fuzz.raw),
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
        let desired = (elements[parent_idx].child_count as usize).min(8);
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
    while next_child < element_count {
        nodes[next_child].parent = Some(0);
        nodes[0].children.push(next_child);
        next_child += 1;
    }

    Some(Arc::new(FuzzProvider { nodes }))
}
