//! Unit tests for xa11y core types, selector engine, and locator.
//!
//! These tests exercise the public API without requiring platform accessibility
//! permissions, using a mock provider with in-memory element data.

use std::sync::Arc;
use std::time::Duration;
use xa11y::*;

// ── Mock Provider ──

/// In-memory tree node for the mock provider.
#[derive(Clone)]
struct MockNode {
    data: ElementData,
    children: Vec<usize>, // indices into MockProvider.nodes
    parent: Option<usize>,
}

/// Mock provider that serves an in-memory tree.
struct MockProvider {
    nodes: Vec<MockNode>,
    last_action: std::sync::Mutex<Option<(u64, String)>>,
}

impl Provider for MockProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Return root node as the only top-level element
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

    fn press(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "press".to_string()));
        Ok(())
    }
    fn focus(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "focus".to_string()));
        Ok(())
    }
    fn blur(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "blur".to_string()));
        Ok(())
    }
    fn toggle(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "toggle".to_string()));
        Ok(())
    }
    fn select(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "select".to_string()));
        Ok(())
    }
    fn expand(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "expand".to_string()));
        Ok(())
    }
    fn collapse(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "collapse".to_string()));
        Ok(())
    }
    fn show_menu(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "show_menu".to_string()));
        Ok(())
    }
    fn increment(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "increment".to_string()));
        Ok(())
    }
    fn decrement(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "decrement".to_string()));
        Ok(())
    }
    fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "scroll_into_view".to_string()));
        Ok(())
    }
    fn set_value(&self, element: &ElementData, _value: &str) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "set_value".to_string()));
        Ok(())
    }
    fn set_numeric_value(&self, element: &ElementData, _value: f64) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "set_numeric_value".to_string()));
        Ok(())
    }
    fn type_text(&self, element: &ElementData, _text: &str) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, "type_text".to_string()));
        Ok(())
    }
    fn set_text_selection(&self, element: &ElementData, _start: u32, _end: u32) -> Result<()> {
        *self.last_action.lock().unwrap() =
            Some((element.handle, "set_text_selection".to_string()));
        Ok(())
    }
    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, action.to_string()));
        Ok(())
    }

    fn subscribe(&self, _element: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "MockProvider does not support subscribe".to_string(),
        })
    }
}

/// Helper to build a sample accessibility tree for testing.
///
/// Structure:
/// [0] application "Test App" (pid=1234)
///   [1] window "My App"
///     [2] toolbar "Main Toolbar"
///       [3] button "Back" (description="Navigate back")
///       [4] text_field "Address Bar" (value="https://example.com", editable)
///     [5] web_area
///       [6] heading "Welcome"
///       [7] button "Submit"
///       [8] button "Cancel" (disabled)
///       [9] check_box "I agree to terms" (checked=off)
fn sample_provider() -> Arc<MockProvider> {
    let elements = vec![
        (
            Role::Application,
            Some("Test App"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
        ),
        (
            Role::Window,
            Some("My App"),
            None,
            None,
            Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
        ),
        (
            Role::Toolbar,
            Some("Main Toolbar"),
            None,
            None,
            Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 44,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
        ),
        (
            Role::Button,
            Some("Back"),
            None,
            Some("Navigate back"),
            Some(Rect {
                x: 10,
                y: 5,
                width: 60,
                height: 34,
            }),
            vec!["press".to_string(), "focus".to_string()],
            StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
        ),
        (
            Role::TextField,
            Some("Address Bar"),
            Some("https://example.com"),
            None,
            Some(Rect {
                x: 80,
                y: 5,
                width: 600,
                height: 34,
            }),
            vec!["focus".to_string(), "set_value".to_string()],
            StateSet {
                enabled: true,
                visible: true,
                editable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
        ),
        (
            Role::WebArea,
            None,
            None,
            None,
            Some(Rect {
                x: 0,
                y: 44,
                width: 1920,
                height: 1036,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
        ),
        (
            Role::Heading,
            Some("Welcome"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
        ),
        (
            Role::Button,
            Some("Submit"),
            None,
            None,
            None,
            vec!["press".to_string(), "focus".to_string()],
            StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
        ),
        (
            Role::Button,
            Some("Cancel"),
            None,
            None,
            None,
            vec!["press".to_string(), "focus".to_string()],
            StateSet {
                enabled: false,
                visible: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
        ),
        (
            Role::CheckBox,
            Some("I agree to terms"),
            None,
            None,
            None,
            vec!["press".to_string()],
            StateSet {
                enabled: true,
                visible: true,
                checked: Some(Toggled::Off),
                ..StateSet::default()
            },
            None,
            None,
            None,
        ),
    ];

    let children_map: Vec<Vec<usize>> = vec![
        vec![1],          // 0: application
        vec![2, 5],       // 1: window
        vec![3, 4],       // 2: toolbar
        vec![],           // 3: button Back
        vec![],           // 4: text_field
        vec![6, 7, 8, 9], // 5: web_area
        vec![],           // 6: heading
        vec![],           // 7: button Submit
        vec![],           // 8: button Cancel
        vec![],           // 9: check_box
    ];

    let parent_map: Vec<Option<usize>> = vec![
        None,    // 0: application
        Some(0), // 1: window
        Some(1), // 2: toolbar
        Some(2), // 3: button Back
        Some(2), // 4: text_field
        Some(1), // 5: web_area
        Some(5), // 6: heading
        Some(5), // 7: button Submit
        Some(5), // 8: button Cancel
        Some(5), // 9: check_box
    ];

    let mut nodes = Vec::new();
    for (i, (role, name, value, desc, bounds, actions, states, nv, minv, maxv)) in
        elements.into_iter().enumerate()
    {
        let data = ElementData {
            role,
            name: name.map(String::from),
            value: value.map(String::from),
            description: desc.map(String::from),
            bounds,
            actions,
            states,
            numeric_value: nv,
            min_value: minv,
            max_value: maxv,
            stable_id: None,
            pid: Some(1234),
            raw: std::collections::HashMap::new(),
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
        last_action: std::sync::Mutex::new(None),
    })
}

fn sample_app() -> App {
    let p = sample_provider();
    App::by_name_with(p as Arc<dyn Provider>, "Test App", Duration::ZERO).unwrap()
}

fn sample_root() -> Element {
    let app = sample_app();
    // The first child of the app is the window
    app.children().unwrap().into_iter().next().unwrap()
}

// ── Element basic operations ──

#[test]
fn element_root() {
    let root = sample_root();
    assert_eq!(root.role, Role::Window);
    assert_eq!(root.name.as_deref(), Some("My App"));
}

#[test]
fn element_children() {
    let root = sample_root();
    let children = root.children().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].role, Role::Toolbar);
    assert_eq!(children[1].role, Role::WebArea);
}

#[test]
fn element_nested_children() {
    let root = sample_root();
    let toolbar = &root.children().unwrap()[0];
    let children = toolbar.children().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].role, Role::Button);
    assert_eq!(children[1].role, Role::TextField);
}

#[test]
fn element_parent() {
    let root = sample_root();
    let toolbar = &root.children().unwrap()[0];
    let button = &toolbar.children().unwrap()[0];
    let parent = button.parent().unwrap().unwrap();
    assert_eq!(parent.role, Role::Toolbar);
    // root is the window, whose parent is the application
    let app_parent = root.parent().unwrap().unwrap();
    assert_eq!(app_parent.role, Role::Application);
}

#[test]
fn element_display() {
    let root = sample_root();
    let display = root.to_string();
    assert!(display.contains("window"));
    assert!(display.contains("My App"));
}

#[test]
fn app_locator() {
    let app = sample_app();
    let buttons = app.locator("button").elements().unwrap();
    // App subtree has 3 buttons: Back, Submit, Cancel
    assert_eq!(buttons.len(), 3);
}

// ── Role mapping ──

#[test]
fn role_snake_case_roundtrip() {
    let roles = vec![
        Role::Unknown,
        Role::Window,
        Role::Button,
        Role::TextField,
        Role::TextArea,
        Role::StaticText,
        Role::ComboBox,
        Role::ListItem,
        Role::MenuItem,
        Role::MenuBar,
        Role::TabGroup,
        Role::TableRow,
        Role::TableCell,
        Role::ScrollBar,
        Role::ScrollThumb,
        Role::ProgressBar,
        Role::TreeItem,
        Role::WebArea,
        Role::SplitGroup,
    ];
    for role in roles {
        let snake = role.to_snake_case();
        let parsed = Role::from_snake_case(snake).unwrap();
        assert_eq!(parsed, role, "roundtrip failed for {}", snake);
    }
}

#[test]
fn role_display() {
    assert_eq!(format!("{}", Role::Button), "button");
    assert_eq!(format!("{}", Role::TextField), "text_field");
    assert_eq!(format!("{}", Role::CheckBox), "check_box");
}

// ── StateSet ──

#[test]
fn stateset_default() {
    let states = StateSet::default();
    assert!(states.enabled);
    assert!(states.visible);
    assert!(!states.focused);
    assert!(states.checked.is_none());
    assert!(!states.selected);
    assert!(states.expanded.is_none());
    assert!(!states.editable);
    assert!(!states.required);
    assert!(!states.busy);
    assert!(!states.focusable);
    assert!(!states.modal);
}

#[test]
fn toggled_variants() {
    assert_ne!(Toggled::Off, Toggled::On);
    assert_ne!(Toggled::On, Toggled::Mixed);
    assert_ne!(Toggled::Off, Toggled::Mixed);
}

// ── Rect ──

#[test]
fn rect_negative_coords() {
    let rect = Rect {
        x: -1920,
        y: -500,
        width: 1920,
        height: 1080,
    };
    assert_eq!(rect.x, -1920);
    assert_eq!(rect.y, -500);
}

// ── Error ──

#[test]
fn error_display() {
    let err = Error::PermissionDenied {
        instructions: "Enable in System Preferences".to_string(),
    };
    assert!(format!("{}", err).contains("Permission denied"));

    let err = Error::SelectorNotMatched {
        selector: "button[name=\"Submit\"]".to_string(),
    };
    assert!(format!("{}", err).contains("Submit"));

    let err = Error::ElementStale {
        selector: "button".to_string(),
    };
    assert!(format!("{}", err).contains("stale"));

    let err = Error::ActionNotSupported {
        action: "toggle".to_string(),
        role: Role::StaticText,
    };
    assert!(format!("{}", err).contains("toggle"));

    let err = Error::InvalidSelector {
        selector: "bad".to_string(),
        message: "oops".to_string(),
    };
    assert!(format!("{}", err).contains("bad"));
}

// ── Serialization ──

#[test]
fn element_json_serialization() {
    let element = ElementData {
        role: Role::Button,
        name: Some("Submit".to_string()),
        value: None,
        description: None,
        bounds: Some(Rect {
            x: 100,
            y: 200,
            width: 80,
            height: 30,
        }),
        actions: vec!["press".to_string()],
        states: StateSet {
            enabled: true,
            visible: true,
            focused: true,
            ..StateSet::default()
        },
        pid: None,
        stable_id: None,
        numeric_value: None,
        min_value: None,
        max_value: None,
        raw: std::collections::HashMap::new(),
        handle: 0,
    };

    let json = serde_json::to_string_pretty(&element).unwrap();
    let deserialized: ElementData = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.role, Role::Button);
    assert_eq!(deserialized.name.as_deref(), Some("Submit"));
    assert!(deserialized.states.focused);
}

#[test]
fn raw_platform_data_serialization() {
    let mut raw: RawPlatformData = std::collections::HashMap::new();
    raw.insert(
        "ax_role".into(),
        serde_json::Value::String("AXButton".into()),
    );
    raw.insert(
        "ax_identifier".into(),
        serde_json::Value::String("submit-btn".into()),
    );
    let json = serde_json::to_string(&raw).unwrap();
    let deserialized: RawPlatformData = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized["ax_role"], "AXButton");
    assert_eq!(deserialized["ax_identifier"], "submit-btn");

    let mut raw_linux: RawPlatformData = std::collections::HashMap::new();
    raw_linux.insert(
        "atspi_role".into(),
        serde_json::Value::String("push button".into()),
    );
    raw_linux.insert("bus_name".into(), serde_json::Value::String(":1.42".into()));
    raw_linux.insert(
        "object_path".into(),
        serde_json::Value::String("/org/a11y/atspi/accessible/1234".into()),
    );
    let json = serde_json::to_string(&raw_linux).unwrap();
    assert!(json.contains("push button"));
}

// ── Provider trait ──

#[test]
fn platform_provider_creates_or_fails_gracefully() {
    let _result = xa11y::create_provider();
}

// ── Selector queries via Locator ──

#[test]
fn query_by_role() {
    let app = sample_app();
    let buttons = app
        .locator("window")
        .descendant("button")
        .elements()
        .unwrap();
    assert_eq!(buttons.len(), 3);
}

#[test]
fn query_by_exact_name() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant(r#"[name="Submit"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::Button);
}

#[test]
fn query_role_and_name() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant(r#"button[name="Submit"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_name_contains() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant(r#"[name*="addr"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_starts_with() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant(r#"[name^="addr"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_ends_with() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant(r#"[name$="bar"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 2); // "Main Toolbar" and "Address Bar"
}

#[test]
fn query_direct_child() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant("toolbar")
        .child("button")
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn query_descendant_buttons() {
    let app = sample_app();
    let results = app.locator("window button").elements().unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn query_nth() {
    let app = sample_app();
    let results = app.locator("window button:nth(2)").elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_nth_out_of_range() {
    let app = sample_app();
    let results = app.locator("window button:nth(99)").elements().unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_complex() {
    let app = sample_app();
    let results = app
        .locator(r#"window toolbar > text_field[name*="Address"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].value.as_deref(), Some("https://example.com"));
}

#[test]
fn query_no_match() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant("slider")
        .elements()
        .unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_invalid_selector() {
    let app = sample_app();
    // Invalid syntax (not just unknown role) should error
    let result = app.locator("").elements();
    assert!(result.is_err());
}

#[test]
fn query_unknown_platform_role_returns_empty() {
    let app = sample_app();
    // Unknown role names are valid (treated as platform roles) but match nothing
    let results = app.locator("foobar").elements().unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_check_box() {
    let app = sample_app();
    let results = app
        .locator("window")
        .descendant("check_box")
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].states.checked, Some(Toggled::Off));
}

#[test]
fn query_web_area_children() {
    let app = sample_app();
    let results = app.locator("window web_area > button").elements().unwrap();
    assert_eq!(results.len(), 2);
}

// ── Locator ──

#[test]
fn locator_basic_query() {
    let app = sample_app();
    let loc = app.locator(r#"window button[name="Submit"]"#);
    let element = loc.element().unwrap();
    assert_eq!(element.role, Role::Button);
    assert_eq!(element.name.as_deref(), Some("Submit"));
    assert!(loc.exists().unwrap());
}

#[test]
fn locator_press_dispatches_action() {
    let p = sample_provider();
    let app = App::by_name_with(
        Arc::clone(&p) as Arc<dyn Provider>,
        "Test App",
        Duration::ZERO,
    )
    .unwrap();
    let loc = app.locator(r#"window button[name="Submit"]"#);
    loc.press().unwrap();
    let (handle, action) = p.last_action.lock().unwrap().clone().unwrap();
    assert_eq!(action, "press");
    assert_eq!(handle, 7); // Submit button is handle 7
}

#[test]
fn locator_not_found() {
    let app = sample_app();
    let loc = app.locator(r#"button[name="NonExistent"]"#);
    assert!(!loc.exists().unwrap());
    assert!(loc.press().is_err());
}

#[test]
fn locator_nth() {
    let app = sample_app();
    // There are 3 buttons in window's subtree: Back(3), Submit(7), Cancel(8)
    // nth is 1-based: 1=Back, 2=Submit, 3=Cancel
    let loc = app.locator("window button").nth(2);
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Submit"));
}

#[test]
fn locator_count() {
    let app = sample_app();
    let loc = app.locator("window button");
    assert_eq!(loc.count().unwrap(), 3);
}

#[test]
fn locator_child() {
    let app = sample_app();
    let loc = app.locator("window toolbar").child("button");
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Back"));
}

#[test]
fn locator_states() {
    let app = sample_app();
    let loc = app.locator(r#"window button[name="Cancel"]"#);
    let element = loc.element().unwrap();
    assert!(!element.states.enabled);
    assert!(element.states.visible);
}

#[test]
fn locator_selector_getter() {
    let app = sample_app();
    let loc = app.locator("button").child("text_field");
    assert_eq!(loc.selector(), "button > text_field");
}

// Regression test for issue #168: a descendant-combinator selector must find
// elements nested inside virtual UIA fragment groups (e.g. Qt QFormLayout value
// columns) that `FindAllBuildCache(TreeScope_Subtree)` may miss because it
// returns nothing when rooted at a fragment element (not a fragment root).
//
// Tree: application > window > group["Outer"] > group["Inner"] > static_text["TestFarm"]
// Selector: group[name="Outer"] static_text[name="TestFarm"]
// The static_text is 2 hops deep from the outer group, so narrow_multi_segment
// must recurse into children rather than relying solely on a single flat scan.
#[test]
fn locator_descendant_through_nested_groups() {
    let children_map: Vec<Vec<usize>> = vec![
        vec![1], // 0: application
        vec![2], // 1: window
        vec![3], // 2: group "Outer"
        vec![4], // 3: group "Inner"
        vec![],  // 4: static_text "TestFarm"
    ];
    let parent_map: Vec<Option<usize>> = vec![None, Some(0), Some(1), Some(2), Some(3)];
    let roles = [
        Role::Application,
        Role::Window,
        Role::Group,
        Role::Group,
        Role::StaticText,
    ];
    let names = [
        Some("Test App"),
        Some("Window"),
        Some("Outer"),
        Some("Inner"),
        Some("TestFarm"),
    ];

    let nodes: Vec<MockNode> = (0..5usize)
        .map(|i| MockNode {
            data: ElementData {
                role: roles[i],
                name: names[i].map(String::from),
                value: None,
                description: None,
                bounds: None,
                actions: vec![],
                states: StateSet::default(),
                numeric_value: None,
                min_value: None,
                max_value: None,
                stable_id: None,
                pid: Some(1234),
                raw: std::collections::HashMap::new(),
                handle: i as u64,
            },
            children: children_map[i].clone(),
            parent: parent_map[i],
        })
        .collect();

    let provider = Arc::new(MockProvider {
        nodes,
        last_action: std::sync::Mutex::new(None),
    });
    let app = App::by_name_with(provider as Arc<dyn Provider>, "Test App", Duration::ZERO).unwrap();

    let loc = app.locator(r#"group[name="Outer"] static_text[name="TestFarm"]"#);
    assert!(
        loc.exists().unwrap(),
        "static_text nested inside two groups must be reachable via descendant combinator"
    );
    let el = loc.element().unwrap();
    assert_eq!(el.role, Role::StaticText);
    assert_eq!(el.name.as_deref(), Some("TestFarm"));
}

// ── Multi-app mock for system-root searches ──

/// Build a mock with multiple apps at the top level.
///
/// Structure (from system root):
///   [0] application "App1" (pid=100)
///     [1] window "Win1"
///       [2] button "Btn1"
///   [3] application "App2" (pid=200)
///     [4] window "Win2"
///       [5] button "Btn2"
///       [6] button "Btn3"
fn multi_app_provider() -> Arc<MultiAppMockProvider> {
    let defs: Vec<(Role, Option<&str>, Option<u32>)> = vec![
        (Role::Application, Some("App1"), Some(100)),
        (Role::Window, Some("Win1"), Some(100)),
        (Role::Button, Some("Btn1"), Some(100)),
        (Role::Application, Some("App2"), Some(200)),
        (Role::Window, Some("Win2"), Some(200)),
        (Role::Button, Some("Btn2"), Some(200)),
        (Role::Button, Some("Btn3"), Some(200)),
    ];

    let children_map: Vec<Vec<usize>> = vec![
        vec![1],    // 0: App1
        vec![2],    // 1: Win1
        vec![],     // 2: Btn1
        vec![4],    // 3: App2
        vec![5, 6], // 4: Win2
        vec![],     // 5: Btn2
        vec![],     // 6: Btn3
    ];

    let parent_map: Vec<Option<usize>> = vec![
        None,    // 0
        Some(0), // 1
        Some(1), // 2
        None,    // 3 (top-level app)
        Some(3), // 4
        Some(4), // 5
        Some(4), // 6
    ];

    let mut nodes = Vec::new();
    for (i, (role, name, pid)) in defs.into_iter().enumerate() {
        let data = ElementData {
            role,
            name: name.map(String::from),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid,
            raw: std::collections::HashMap::new(),
            handle: i as u64,
        };
        nodes.push(MockNode {
            data,
            children: children_map[i].clone(),
            parent: parent_map[i],
        });
    }

    // Override get_children(None) to return both apps
    Arc::new(MultiAppMockProvider { nodes })
}

/// Mock provider returning multiple top-level apps from get_children(None).
struct MultiAppMockProvider {
    nodes: Vec<MockNode>,
}

impl Provider for MultiAppMockProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Return all application-role nodes as top-level
                Ok(self
                    .nodes
                    .iter()
                    .filter(|n| n.data.role == Role::Application)
                    .map(|n| n.data.clone())
                    .collect())
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

    fn press(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn focus(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn blur(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn toggle(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn select(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn expand(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn collapse(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn show_menu(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn increment(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn decrement(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
        Ok(())
    }
    fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
        Ok(())
    }
    fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
        Ok(())
    }
    fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
        Ok(())
    }
    fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        Ok(())
    }
    fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
        Ok(())
    }

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "not supported".to_string(),
        })
    }
}

// ── find_elements / search behavior tests ──

#[test]
fn find_application_by_name_from_root() {
    let p = multi_app_provider();
    let app = App::by_name_with(p as Arc<dyn Provider>, "App2", Duration::ZERO).unwrap();
    assert_eq!(app.data.role, Role::Application);
    assert_eq!(app.name, "App2");
    assert_eq!(app.pid, Some(200));
}

#[test]
fn find_all_applications() {
    let p = multi_app_provider();
    let apps = App::list_with(p as Arc<dyn Provider>).unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0].name, "App1");
    assert_eq!(apps[1].name, "App2");
}

#[test]
fn find_application_only_checks_top_level() {
    // application search from root should NOT recurse into app subtrees
    let p = multi_app_provider();
    let apps = App::list_with(p as Arc<dyn Provider>).unwrap();
    // Should find exactly 2, not more (no nested apps)
    assert_eq!(apps.len(), 2);
}

#[test]
fn find_button_across_apps() {
    // Searching for buttons within each app
    let p = multi_app_provider();
    let apps = App::list_with(p as Arc<dyn Provider>).unwrap();
    let mut total_buttons = 0;
    for app in &apps {
        total_buttons += app.locator("button").count().unwrap();
    }
    assert_eq!(total_buttons, 3); // Btn1, Btn2, Btn3
}

#[test]
fn find_with_limit_stops_early() {
    let p = multi_app_provider();
    let app1 = App::by_name_with(p as Arc<dyn Provider>, "App1", Duration::ZERO).unwrap();
    let first = app1.locator("button").first().element().unwrap();
    assert_eq!(first.name.as_deref(), Some("Btn1"));
}

#[test]
fn find_multi_segment_across_apps() {
    // "window > button" — find buttons that are direct children of windows in App2
    let p = multi_app_provider();
    let app2 = App::by_name_with(p as Arc<dyn Provider>, "App2", Duration::ZERO).unwrap();
    let results = app2.locator("window > button").elements().unwrap();
    assert_eq!(results.len(), 2); // Btn2, Btn3
}

#[test]
fn app_locator_scopes_search() {
    let p = multi_app_provider();
    let app2 = App::by_name_with(p as Arc<dyn Provider>, "App2", Duration::ZERO).unwrap();
    // Scoped locator should only find buttons within App2
    let buttons = app2.locator("button").elements().unwrap();
    assert_eq!(buttons.len(), 2); // Btn2, Btn3
    assert_eq!(buttons[0].name.as_deref(), Some("Btn2"));
}

#[test]
fn app_locator_does_not_find_sibling_app_elements() {
    let p = multi_app_provider();
    let app1 = App::by_name_with(p as Arc<dyn Provider>, "App1", Duration::ZERO).unwrap();
    // App1 only has Btn1
    let buttons = app1.locator("button").elements().unwrap();
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0].name.as_deref(), Some("Btn1"));
}

#[test]
fn locator_count_matches_elements_len() {
    let p = multi_app_provider();
    let app1 = App::by_name_with(p as Arc<dyn Provider>, "App1", Duration::ZERO).unwrap();
    let loc = app1.locator("button");
    assert_eq!(loc.count().unwrap(), loc.elements().unwrap().len());
}

#[test]
fn app_by_name_not_found() {
    let p = multi_app_provider();
    let result = App::by_name_with(p as Arc<dyn Provider>, "NoSuchApp", Duration::ZERO);
    assert!(result.is_err());
}

#[test]
fn locator_nth_out_of_range() {
    let p = multi_app_provider();
    let apps = App::list_with(p as Arc<dyn Provider>).unwrap();
    // Use first app and request an out-of-range nth button
    let loc = apps[0].locator("button").nth(99);
    assert!(loc.element().is_err());
}

#[test]
fn element_children_of_leaf_is_empty() {
    let p = multi_app_provider();
    let app1 = App::by_name_with(p as Arc<dyn Provider>, "App1", Duration::ZERO).unwrap();
    let btn = app1.locator("button").first().element().unwrap();
    assert!(btn.children().unwrap().is_empty());
}

#[test]
fn element_parent_of_top_level_is_none() {
    let p = multi_app_provider();
    let app = App::list_with(Arc::clone(&p) as Arc<dyn Provider>)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    // The application element itself has no parent
    let app_element = Element::new(app.data.clone(), Arc::clone(&p) as Arc<dyn Provider>);
    assert!(app_element.parent().unwrap().is_none());
}

#[test]
fn element_parent_navigates_up() {
    let p = multi_app_provider();
    let app2 = App::by_name_with(p as Arc<dyn Provider>, "App2", Duration::ZERO).unwrap();
    let btn = app2.locator(r#"button[name="Btn2"]"#).element().unwrap();
    let parent = btn.parent().unwrap().unwrap();
    assert_eq!(parent.role, Role::Window);
    assert_eq!(parent.name.as_deref(), Some("Win2"));
}

#[test]
fn handle_preserved_through_find() {
    // Verify that handle IDs survive the find_elements pipeline
    let p = multi_app_provider();
    let app1 =
        App::by_name_with(Arc::clone(&p) as Arc<dyn Provider>, "App1", Duration::ZERO).unwrap();
    // handle should be non-default (we set it to the node index)
    assert_eq!(app1.data.handle, 0); // App1 is node index 0
    let app2 =
        App::by_name_with(Arc::clone(&p) as Arc<dyn Provider>, "App2", Duration::ZERO).unwrap();
    let btn = app2.locator(r#"button[name="Btn2"]"#).element().unwrap();
    assert_eq!(btn.handle, 5); // Btn2 is node index 5
}

// ── Timeout polling tests ──

/// Provider wrapper that delays root-level lookups (`get_children(None)`)
/// until `succeed_after` calls have been made. Earlier calls return an empty
/// Vec, simulating an app that hasn't registered with the accessibility API
/// yet. Non-root tree navigation and all other Provider methods forward to
/// `inner` unchanged.
struct DelayedProvider {
    inner: Arc<dyn Provider>,
    root_calls: std::sync::atomic::AtomicUsize,
    succeed_after: usize,
    always_fail_permission: bool,
}

impl DelayedProvider {
    fn new(inner: Arc<dyn Provider>, succeed_after: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            root_calls: std::sync::atomic::AtomicUsize::new(0),
            succeed_after,
            always_fail_permission: false,
        })
    }

    /// On every root-level lookup, return `PermissionDenied` (a non-retryable
    /// error). Used to verify that lookup polling short-circuits.
    fn always_fail_permission(inner: Arc<dyn Provider>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            root_calls: std::sync::atomic::AtomicUsize::new(0),
            succeed_after: 0,
            always_fail_permission: true,
        })
    }

    fn root_call_count(&self) -> usize {
        self.root_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Provider for DelayedProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        if element.is_none() {
            let n = self
                .root_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.always_fail_permission {
                return Err(Error::PermissionDenied {
                    instructions: "test".to_string(),
                });
            }
            if n < self.succeed_after {
                return Ok(vec![]);
            }
        }
        self.inner.get_children(element)
    }
    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        self.inner.get_parent(element)
    }
    fn press(&self, e: &ElementData) -> Result<()> {
        self.inner.press(e)
    }
    fn focus(&self, e: &ElementData) -> Result<()> {
        self.inner.focus(e)
    }
    fn blur(&self, e: &ElementData) -> Result<()> {
        self.inner.blur(e)
    }
    fn toggle(&self, e: &ElementData) -> Result<()> {
        self.inner.toggle(e)
    }
    fn select(&self, e: &ElementData) -> Result<()> {
        self.inner.select(e)
    }
    fn expand(&self, e: &ElementData) -> Result<()> {
        self.inner.expand(e)
    }
    fn collapse(&self, e: &ElementData) -> Result<()> {
        self.inner.collapse(e)
    }
    fn show_menu(&self, e: &ElementData) -> Result<()> {
        self.inner.show_menu(e)
    }
    fn increment(&self, e: &ElementData) -> Result<()> {
        self.inner.increment(e)
    }
    fn decrement(&self, e: &ElementData) -> Result<()> {
        self.inner.decrement(e)
    }
    fn scroll_into_view(&self, e: &ElementData) -> Result<()> {
        self.inner.scroll_into_view(e)
    }
    fn set_value(&self, e: &ElementData, v: &str) -> Result<()> {
        self.inner.set_value(e, v)
    }
    fn set_numeric_value(&self, e: &ElementData, v: f64) -> Result<()> {
        self.inner.set_numeric_value(e, v)
    }
    fn type_text(&self, e: &ElementData, t: &str) -> Result<()> {
        self.inner.type_text(e, t)
    }
    fn set_text_selection(&self, e: &ElementData, s: u32, en: u32) -> Result<()> {
        self.inner.set_text_selection(e, s, en)
    }
    fn perform_action(&self, e: &ElementData, a: &str) -> Result<()> {
        self.inner.perform_action(e, a)
    }
    fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
        self.inner.subscribe(e)
    }
}

#[test]
fn by_name_with_polls_until_app_appears() {
    let inner = multi_app_provider();
    // `by_name_with` issues two root lookups per attempt (application
    // selector, then window selector). Failing the first 3 root calls means
    // the first attempt fails entirely and the second attempt succeeds on
    // its first selector.
    let p = DelayedProvider::new(inner, 3);
    let app = App::by_name_with(
        Arc::clone(&p) as Arc<dyn Provider>,
        "App1",
        std::time::Duration::from_secs(2),
    )
    .expect("app should appear after a few polls");
    assert_eq!(app.name, "App1");
    assert!(
        p.root_call_count() > 2,
        "expected polling to retry, got {} root calls",
        p.root_call_count()
    );
}

#[test]
fn by_name_with_zero_timeout_is_single_attempt() {
    let inner = multi_app_provider();
    let p = DelayedProvider::new(inner, 100); // never succeeds during the test
    let result = App::by_name_with(
        Arc::clone(&p) as Arc<dyn Provider>,
        "App1",
        std::time::Duration::ZERO,
    );
    assert!(matches!(result, Err(Error::SelectorNotMatched { .. })));
    // One outer attempt = at most two selector lookups (application + window).
    assert!(
        p.root_call_count() <= 2,
        "ZERO timeout must not retry, got {} calls",
        p.root_call_count()
    );
}

#[test]
fn by_name_with_short_circuits_on_non_retryable_error() {
    let inner = multi_app_provider();
    let p = DelayedProvider::always_fail_permission(inner);
    let start = std::time::Instant::now();
    let result = App::by_name_with(
        Arc::clone(&p) as Arc<dyn Provider>,
        "App1",
        std::time::Duration::from_secs(10),
    );
    assert!(matches!(result, Err(Error::PermissionDenied { .. })));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "non-retryable error must fail fast, took {:?}",
        start.elapsed()
    );
}

#[test]
fn by_pid_with_polls_until_app_appears() {
    let inner = multi_app_provider();
    let p = DelayedProvider::new(inner, 3);
    // App1 has pid 100 in multi_app_provider.
    let app = App::by_pid_with(
        Arc::clone(&p) as Arc<dyn Provider>,
        100,
        std::time::Duration::from_secs(2),
    )
    .expect("app should appear after a few polls");
    assert_eq!(app.pid, Some(100));
    assert!(p.root_call_count() > 2);
}
