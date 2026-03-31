//! Unit tests for xa11y core types, selector engine, and locator.
//!
//! These tests exercise the public API without requiring platform accessibility
//! permissions, using a mock provider with in-memory element data.

use std::sync::Arc;
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
    last_action: std::sync::Mutex<Option<(u64, Action)>>,
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

    fn perform_action(
        &self,
        element: &ElementData,
        action: Action,
        _data: Option<ActionData>,
    ) -> Result<()> {
        *self.last_action.lock().unwrap() = Some((element.handle, action));
        Ok(())
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        Ok(PermissionStatus::Granted)
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
/// [0] window "My App"
///   [1] toolbar "Main Toolbar"
///     [2] button "Back" (description="Navigate back")
///     [3] text_field "Address Bar" (value="https://example.com", editable)
///   [4] web_area
///     [5] heading "Welcome"
///     [6] button "Submit"
///     [7] button "Cancel" (disabled)
///     [8] check_box "I agree to terms" (checked=off)
fn sample_provider() -> Arc<MockProvider> {
    let elements = vec![
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
            vec![Action::Press, Action::Focus],
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
            vec![Action::Focus, Action::SetValue],
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
            vec![Action::Press, Action::Focus],
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
            vec![Action::Press, Action::Focus],
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
            vec![Action::Press, Action::Toggle],
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
        vec![1, 4],       // 0: window
        vec![2, 3],       // 1: toolbar
        vec![],           // 2: button Back
        vec![],           // 3: text_field
        vec![5, 6, 7, 8], // 4: web_area
        vec![],           // 5: heading
        vec![],           // 6: button Submit
        vec![],           // 7: button Cancel
        vec![],           // 8: check_box
    ];

    let parent_map: Vec<Option<usize>> = vec![
        None,    // 0
        Some(0), // 1
        Some(1), // 2
        Some(1), // 3
        Some(0), // 4
        Some(4), // 5
        Some(4), // 6
        Some(4), // 7
        Some(4), // 8
    ];

    let mut nodes = Vec::new();
    for (i, (role, name, value, desc, bounds, actions, states, nv, minv, maxv)) in
        elements.into_iter().enumerate()
    {
        nodes.push(MockNode {
            data: ElementData {
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
                raw: RawPlatformData::Synthetic,
                handle: i as u64,
            },
            children: children_map[i].clone(),
            parent: parent_map[i],
        });
    }

    Arc::new(MockProvider {
        nodes,
        last_action: std::sync::Mutex::new(None),
    })
}

fn sample_root() -> Element {
    let p = sample_provider();
    let children = p.get_children(None).unwrap();
    Element::new(children.into_iter().next().unwrap(), p as Arc<dyn Provider>)
}

fn sample_locator() -> (Arc<MockProvider>, Locator) {
    let p = sample_provider();
    let loc = locator(Arc::clone(&p) as Arc<dyn Provider>, "window");
    (p, loc)
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
    assert!(root.parent().unwrap().is_none());
}

#[test]
fn element_display() {
    let root = sample_root();
    let display = root.to_string();
    assert!(display.contains("window"));
    assert!(display.contains("My App"));
}

#[test]
fn element_locator() {
    let root = sample_root();
    let loc = root.locator("button");
    let buttons = loc.elements().unwrap();
    // Root is the window — its subtree has 3 buttons: Back, Submit, Cancel
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

// ── Action ──

#[test]
fn action_display() {
    assert_eq!(format!("{}", Action::Press), "Press");
    assert_eq!(format!("{}", Action::SetValue), "SetValue");
    assert_eq!(format!("{}", Action::ScrollIntoView), "ScrollIntoView");
}

#[test]
fn validate_text_selection_start_must_be_lte_end() {
    let valid = ActionData::TextSelection { start: 0, end: 5 };
    assert!(valid.validate(Action::SetTextSelection).is_ok());

    let equal = ActionData::TextSelection { start: 3, end: 3 };
    assert!(equal.validate(Action::SetTextSelection).is_ok());

    let reversed = ActionData::TextSelection { start: 5, end: 2 };
    assert!(matches!(
        reversed.validate(Action::SetTextSelection),
        Err(Error::InvalidActionData { .. })
    ));
}

#[test]
fn validate_numeric_value_must_be_finite() {
    let valid = ActionData::NumericValue(42.0);
    assert!(valid.validate(Action::SetValue).is_ok());

    let nan = ActionData::NumericValue(f64::NAN);
    assert!(matches!(
        nan.validate(Action::SetValue),
        Err(Error::InvalidActionData { .. })
    ));

    let inf = ActionData::NumericValue(f64::INFINITY);
    assert!(matches!(
        inf.validate(Action::SetValue),
        Err(Error::InvalidActionData { .. })
    ));
}

#[test]
fn validate_other_action_data_always_ok() {
    let text = ActionData::Value("hello".to_string());
    assert!(text.validate(Action::TypeText).is_ok());

    let scroll = ActionData::ScrollAmount(0.0);
    assert!(scroll.validate(Action::ScrollDown).is_ok());
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
        action: Action::Toggle,
        role: Role::StaticText,
    };
    assert!(format!("{}", err).contains("Toggle"));

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
        actions: vec![Action::Press],
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
        raw: RawPlatformData::Synthetic,
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
    let raw_mac = RawPlatformData::MacOS {
        ax_role: "AXButton".to_string(),
        ax_subrole: None,
        ax_identifier: Some("submit-btn".to_string()),
    };
    let json = serde_json::to_string(&raw_mac).unwrap();
    let deserialized: RawPlatformData = serde_json::from_str(&json).unwrap();
    match deserialized {
        RawPlatformData::MacOS {
            ax_role,
            ax_identifier,
            ..
        } => {
            assert_eq!(ax_role, "AXButton");
            assert_eq!(ax_identifier.as_deref(), Some("submit-btn"));
        }
        _ => panic!("expected MacOS variant"),
    }

    let raw_linux = RawPlatformData::Linux {
        atspi_role: "push button".to_string(),
        bus_name: ":1.42".to_string(),
        object_path: "/org/a11y/atspi/accessible/1234".to_string(),
    };
    let json = serde_json::to_string(&raw_linux).unwrap();
    assert!(json.contains("push button"));
}

// ── Provider trait ──

#[test]
fn permission_status_variants() {
    let granted = PermissionStatus::Granted;
    let denied = PermissionStatus::Denied {
        instructions: "Enable accessibility".to_string(),
    };

    let json = serde_json::to_string(&granted).unwrap();
    assert!(json.contains("Granted"));

    let json = serde_json::to_string(&denied).unwrap();
    assert!(json.contains("Enable accessibility"));
}

#[test]
fn platform_provider_creates_or_fails_gracefully() {
    let _result = xa11y::create_provider();
}

#[test]
fn platform_provider_check_permissions() {
    let provider = match xa11y::create_provider() {
        Ok(p) => p,
        Err(_) => return,
    };
    let status = provider.check_permissions().unwrap();
    match status {
        PermissionStatus::Granted | PermissionStatus::Denied { .. } => {}
    }
}

// ── Selector queries via Locator ──

#[test]
fn query_by_role() {
    let (_, loc) = sample_locator();
    let buttons = loc.descendant("button").elements().unwrap();
    assert_eq!(buttons.len(), 3);
}

#[test]
fn query_by_exact_name() {
    let (_, loc) = sample_locator();
    let results = loc.descendant(r#"[name="Submit"]"#).elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::Button);
}

#[test]
fn query_role_and_name() {
    let (_, loc) = sample_locator();
    let results = loc
        .descendant(r#"button[name="Submit"]"#)
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_name_contains() {
    let (_, loc) = sample_locator();
    let results = loc.descendant(r#"[name*="addr"]"#).elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_starts_with() {
    let (_, loc) = sample_locator();
    let results = loc.descendant(r#"[name^="addr"]"#).elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_ends_with() {
    let (_, loc) = sample_locator();
    let results = loc.descendant(r#"[name$="bar"]"#).elements().unwrap();
    assert_eq!(results.len(), 2); // "Main Toolbar" and "Address Bar"
}

#[test]
fn query_direct_child() {
    let (_, loc) = sample_locator();
    let results = loc
        .descendant("toolbar")
        .child("button")
        .elements()
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn query_descendant_buttons() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window button");
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn query_nth() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window button:nth(2)");
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_nth_out_of_range() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window button:nth(99)");
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_complex() {
    let p = sample_provider();
    let loc = locator(
        p as Arc<dyn Provider>,
        r#"window toolbar > text_field[name*="Address"]"#,
    );
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].value.as_deref(), Some("https://example.com"));
}

#[test]
fn query_no_match() {
    let (_, loc) = sample_locator();
    let results = loc.descendant("slider").elements().unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_invalid_selector() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "foobar");
    let result = loc.elements();
    assert!(result.is_err());
}

#[test]
fn query_check_box() {
    let (_, loc) = sample_locator();
    let results = loc.descendant("check_box").elements().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].states.checked, Some(Toggled::Off));
}

#[test]
fn query_web_area_children() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window web_area > button");
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 2);
}

// ── Locator ──

#[test]
fn locator_basic_query() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, r#"window button[name="Submit"]"#);
    let element = loc.element().unwrap();
    assert_eq!(element.role, Role::Button);
    assert_eq!(element.name.as_deref(), Some("Submit"));
    assert!(loc.exists().unwrap());
}

#[test]
fn locator_press_dispatches_action() {
    let p = sample_provider();
    let loc = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"window button[name="Submit"]"#,
    );
    loc.press().unwrap();
    let (handle, action) = p.last_action.lock().unwrap().unwrap();
    assert_eq!(action, Action::Press);
    assert_eq!(handle, 6); // Submit button is handle 6
}

#[test]
fn locator_not_found() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, r#"button[name="NonExistent"]"#);
    assert!(!loc.exists().unwrap());
    assert!(loc.press().is_err());
}

#[test]
fn locator_nth() {
    let p = sample_provider();
    // There are 3 buttons in window's subtree: Back(2), Submit(6), Cancel(7)
    let loc = locator(p as Arc<dyn Provider>, "window button").nth(1);
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Submit"));
}

#[test]
fn locator_count() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window button");
    assert_eq!(loc.count().unwrap(), 3);
}

#[test]
fn locator_child() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "window toolbar").child("button");
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Back"));
}

#[test]
fn locator_states() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, r#"window button[name="Cancel"]"#);
    let element = loc.element().unwrap();
    assert!(!element.states.enabled);
    assert!(element.states.visible);
}

#[test]
fn locator_selector_getter() {
    let p = sample_provider();
    let loc = locator(p as Arc<dyn Provider>, "button").child("text_field");
    assert_eq!(loc.selector(), "button > text_field");
}

// ── ActionData serialization ──

#[test]
fn action_data_variants() {
    let text = ActionData::Value("hello".to_string());
    let json = serde_json::to_string(&text).unwrap();
    assert!(json.contains("hello"));

    let numeric = ActionData::NumericValue(42.5);
    let json = serde_json::to_string(&numeric).unwrap();
    assert!(json.contains("42.5"));

    let scroll = ActionData::ScrollAmount(100.0);
    let json = serde_json::to_string(&scroll).unwrap();
    assert!(json.contains("100"));
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
        nodes.push(MockNode {
            data: ElementData {
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
                raw: RawPlatformData::Synthetic,
                handle: i as u64,
            },
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

    fn perform_action(&self, _: &ElementData, _: Action, _: Option<ActionData>) -> Result<()> {
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

// ── find_elements / search behavior tests ──

#[test]
fn find_application_by_name_from_root() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, r#"application[name="App2"]"#);
    let app = loc.element().unwrap();
    assert_eq!(app.role, Role::Application);
    assert_eq!(app.name.as_deref(), Some("App2"));
    assert_eq!(app.pid, Some(200));
}

#[test]
fn find_all_applications() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "application");
    let apps = loc.elements().unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0].name.as_deref(), Some("App1"));
    assert_eq!(apps[1].name.as_deref(), Some("App2"));
}

#[test]
fn find_application_only_checks_top_level() {
    // application search from root should NOT recurse into app subtrees
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "application");
    let apps = loc.elements().unwrap();
    // Should find exactly 2, not more (no nested apps)
    assert_eq!(apps.len(), 2);
}

#[test]
fn find_button_across_apps() {
    // Searching for buttons from root traverses all apps
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "button");
    let buttons = loc.elements().unwrap();
    assert_eq!(buttons.len(), 3); // Btn1, Btn2, Btn3
}

#[test]
fn find_with_limit_stops_early() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "button");
    // nth(0) means "first match" — internally uses limit=1
    let first = loc.first().element().unwrap();
    assert_eq!(first.name.as_deref(), Some("Btn1"));
}

#[test]
fn find_multi_segment_across_apps() {
    // "application window > button" — find buttons that are direct children of windows
    let p = multi_app_provider();
    let loc = locator(
        p as Arc<dyn Provider>,
        r#"application[name="App2"] window > button"#,
    );
    let results = loc.elements().unwrap();
    assert_eq!(results.len(), 2); // Btn2, Btn3
}

#[test]
fn element_locator_scopes_search() {
    let p = multi_app_provider();
    let app2 = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"application[name="App2"]"#,
    )
    .element()
    .unwrap();
    // Scoped locator should only find buttons within App2
    let buttons = app2.locator("button").elements().unwrap();
    assert_eq!(buttons.len(), 2); // Btn2, Btn3
    assert_eq!(buttons[0].name.as_deref(), Some("Btn2"));
}

#[test]
fn element_locator_does_not_find_sibling_app_elements() {
    let p = multi_app_provider();
    let app1 = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"application[name="App1"]"#,
    )
    .element()
    .unwrap();
    // App1 only has Btn1
    let buttons = app1.locator("button").elements().unwrap();
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0].name.as_deref(), Some("Btn1"));
}

#[test]
fn locator_count_matches_elements_len() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "button");
    assert_eq!(loc.count().unwrap(), loc.elements().unwrap().len());
}

#[test]
fn locator_exists_false_for_no_match() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, r#"application[name="NoSuchApp"]"#);
    assert!(!loc.exists().unwrap());
}

#[test]
fn locator_nth_out_of_range() {
    let p = multi_app_provider();
    let loc = locator(p as Arc<dyn Provider>, "application").nth(99);
    assert!(loc.element().is_err());
}

#[test]
fn element_children_of_leaf_is_empty() {
    let p = multi_app_provider();
    let btn = locator(Arc::clone(&p) as Arc<dyn Provider>, "button")
        .first()
        .element()
        .unwrap();
    assert!(btn.children().unwrap().is_empty());
}

#[test]
fn element_parent_of_top_level_is_none() {
    let p = multi_app_provider();
    let app = locator(Arc::clone(&p) as Arc<dyn Provider>, "application")
        .first()
        .element()
        .unwrap();
    assert!(app.parent().unwrap().is_none());
}

#[test]
fn element_parent_navigates_up() {
    let p = multi_app_provider();
    let btn = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"button[name="Btn2"]"#,
    )
    .element()
    .unwrap();
    let parent = btn.parent().unwrap().unwrap();
    assert_eq!(parent.role, Role::Window);
    assert_eq!(parent.name.as_deref(), Some("Win2"));
}

#[test]
fn handle_preserved_through_find() {
    // Verify that handle IDs survive the find_elements pipeline
    let p = multi_app_provider();
    let app = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"application[name="App1"]"#,
    )
    .element()
    .unwrap();
    // handle should be non-default (we set it to the node index)
    assert_eq!(app.handle, 0); // App1 is node index 0
    let btn = locator(
        Arc::clone(&p) as Arc<dyn Provider>,
        r#"button[name="Btn2"]"#,
    )
    .element()
    .unwrap();
    assert_eq!(btn.handle, 5); // Btn2 is node index 5
}
