//! In-memory mock provider so the JS unit tests can exercise the bindings
//! without a live accessibility backend. Mirrors the mock used in
//! `xa11y-python`.
//!
//! The items here are exposed to JavaScript via the napi entry point at the
//! bottom of the file, so cargo's default `dead_code` analysis (which runs
//! in test builds without the napi macro hooks) flags them as unused. Suppress
//! the warning at the module level with a comment explaining why.

#![allow(
    dead_code,
    reason = "items are referenced from JS via the napi-generated entrypoint, \
              not from Rust callers"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::locator::Locator;

struct MockProvider {
    nodes: Vec<MockNode>,
    actions: Mutex<Vec<(u64, String, Option<String>)>>,
}

struct MockNode {
    data: xa11y::ElementData,
    children: Vec<usize>,
    parent: Option<usize>,
}

impl xa11y::Provider for MockProvider {
    fn get_children(
        &self,
        element: Option<&xa11y::ElementData>,
    ) -> xa11y::Result<Vec<xa11y::ElementData>> {
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

    fn get_parent(
        &self,
        element: &xa11y::ElementData,
    ) -> xa11y::Result<Option<xa11y::ElementData>> {
        let idx = element.handle as usize;
        if idx >= self.nodes.len() {
            return Ok(None);
        }
        Ok(self.nodes[idx].parent.map(|i| self.nodes[i].data.clone()))
    }

    fn press(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "press", None)
    }
    fn focus(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "focus", None)
    }
    fn blur(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "blur", None)
    }
    fn toggle(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "toggle", None)
    }
    fn select(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "select", None)
    }
    fn expand(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "expand", None)
    }
    fn collapse(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "collapse", None)
    }
    fn show_menu(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "show_menu", None)
    }
    fn increment(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "increment", None)
    }
    fn decrement(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "decrement", None)
    }
    fn scroll_into_view(&self, el: &xa11y::ElementData) -> xa11y::Result<()> {
        self.record(el, "scroll_into_view", None)
    }
    fn set_value(&self, el: &xa11y::ElementData, value: &str) -> xa11y::Result<()> {
        self.record(el, "set_value", Some(value.to_string()))
    }
    fn set_numeric_value(&self, el: &xa11y::ElementData, v: f64) -> xa11y::Result<()> {
        self.record(el, "set_numeric_value", Some(format!("{v}")))
    }
    fn type_text(&self, el: &xa11y::ElementData, text: &str) -> xa11y::Result<()> {
        self.record(el, "type_text", Some(text.to_string()))
    }
    fn set_text_selection(
        &self,
        el: &xa11y::ElementData,
        start: u32,
        end: u32,
    ) -> xa11y::Result<()> {
        self.record(el, "set_text_selection", Some(format!("{start}..{end}")))
    }
    fn perform_action(&self, el: &xa11y::ElementData, action: &str) -> xa11y::Result<()> {
        self.record(el, action, None)
    }
    fn subscribe(&self, _el: &xa11y::ElementData) -> xa11y::Result<xa11y::Subscription> {
        Err(xa11y::Error::Platform {
            code: -1,
            message: "MockProvider does not support subscribe".to_string(),
        })
    }
}

impl MockProvider {
    fn record(
        &self,
        el: &xa11y::ElementData,
        action: &str,
        data: Option<String>,
    ) -> xa11y::Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push((el.handle, action.to_string(), data));
        Ok(())
    }
}

/// One row of the mock element table — broken out into a type alias so
/// clippy's `type_complexity` lint stays happy.
type MockElementSpec<'a> = (
    xa11y::Role,
    Option<&'a str>,
    Option<&'a str>,
    Vec<&'a str>,
    xa11y::StateSet,
);

fn build_tree() -> Arc<MockProvider> {
    use xa11y::*;

    let elements: Vec<MockElementSpec> = vec![
        (
            Role::Application,
            Some("MockApp"),
            None,
            vec![],
            StateSet::default(),
        ),
        (
            Role::Window,
            Some("Main Window"),
            None,
            vec![],
            StateSet {
                focused: true,
                ..StateSet::default()
            },
        ),
        (
            Role::Button,
            Some("OK"),
            None,
            vec!["press", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
        ),
        (
            Role::Button,
            Some("Cancel"),
            None,
            vec!["press", "focus"],
            StateSet {
                enabled: false,
                focusable: true,
                ..StateSet::default()
            },
        ),
        (
            Role::TextField,
            Some("Search"),
            Some("hello"),
            vec!["focus", "set_value", "type_text"],
            StateSet {
                editable: true,
                focusable: true,
                ..StateSet::default()
            },
        ),
        (
            Role::CheckBox,
            Some("Agree"),
            None,
            vec!["press", "focus"],
            StateSet {
                checked: Some(Toggled::On),
                focusable: true,
                ..StateSet::default()
            },
        ),
    ];

    let children_map: Vec<Vec<usize>> =
        vec![vec![1], vec![2, 3, 4, 5], vec![], vec![], vec![], vec![]];
    let parent_map: Vec<Option<usize>> = vec![None, Some(0), Some(1), Some(1), Some(1), Some(1)];

    let mut nodes = Vec::new();
    for (i, (role, name, value, actions, states)) in elements.into_iter().enumerate() {
        let data = ElementData {
            role,
            name: name.map(String::from),
            value: value.map(String::from),
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 20,
            }),
            actions: actions.iter().map(|s| s.to_string()).collect(),
            states,
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: Some(4242),
            raw: HashMap::new(),
            handle: i as u64,
        };
        nodes.push(MockNode {
            data,
            children: children_map[i].clone(),
            parent: parent_map[i],
        });
    }

    Arc::new(MockProvider {
        nodes,
        actions: Mutex::new(Vec::new()),
    })
}

/// Create a mock `Locator` rooted at the synthetic tree. Used only from
/// the JS unit tests — not part of the public API.
#[napi(js_name = "_makeTestLocator")]
pub fn make_test_locator() -> Locator {
    let provider = build_tree();
    Locator::from_inner(xa11y::Locator::new(
        provider as Arc<dyn xa11y::Provider>,
        None,
        "application",
    ))
}
