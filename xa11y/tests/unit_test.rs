//! Unit tests for xa11y core types, tree operations, and selector engine.
//!
//! These tests exercise the public API without requiring platform accessibility
//! permissions, using manually constructed trees. No real accessibility backend
//! or running applications are needed.

use xa11y::*;

/// Helper to build a sample accessibility tree for testing.
fn sample_tree() -> Element {
    let elements = vec![
        ElementData {
            role: Role::Window,
            name: Some("My App".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),

            actions: vec![],
            states: StateSet::default(),

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![1, 4],
            parent_index: None,
        },
        ElementData {
            role: Role::Toolbar,
            name: Some("Main Toolbar".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 44,
            }),

            actions: vec![],
            states: StateSet::default(),

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 1,
            children_indices: vec![2, 3],
            parent_index: Some(0),
        },
        ElementData {
            role: Role::Button,
            name: Some("Back".to_string()),
            value: None,
            description: Some("Navigate back".to_string()),
            bounds: Some(Rect {
                x: 10,
                y: 5,
                width: 60,
                height: 34,
            }),

            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 2,
            children_indices: vec![],
            parent_index: Some(1),
        },
        ElementData {
            role: Role::TextField,
            name: Some("Address Bar".to_string()),
            value: Some("https://example.com".to_string()),
            description: None,
            bounds: Some(Rect {
                x: 80,
                y: 5,
                width: 600,
                height: 34,
            }),

            actions: vec![Action::Focus, Action::SetValue],
            states: StateSet {
                enabled: true,
                visible: true,
                editable: true,
                ..StateSet::default()
            },

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 3,
            children_indices: vec![],
            parent_index: Some(1),
        },
        ElementData {
            role: Role::WebArea,
            name: None,
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 44,
                width: 1920,
                height: 1036,
            }),

            actions: vec![],
            states: StateSet::default(),

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 4,
            children_indices: vec![5, 6, 7, 8],
            parent_index: Some(0),
        },
        ElementData {
            role: Role::Heading,
            name: Some("Welcome".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![],
            states: StateSet::default(),

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 5,
            children_indices: vec![],
            parent_index: Some(4),
        },
        ElementData {
            role: Role::Button,
            name: Some("Submit".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 6,
            children_indices: vec![],
            parent_index: Some(4),
        },
        ElementData {
            role: Role::Button,
            name: Some("Cancel".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: false,
                visible: true,
                ..StateSet::default()
            },

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 7,
            children_indices: vec![],
            parent_index: Some(4),
        },
        ElementData {
            role: Role::CheckBox,
            name: Some("I agree to terms".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Press, Action::Toggle],
            states: StateSet {
                enabled: true,
                visible: true,
                checked: Some(Toggled::Off),
                ..StateSet::default()
            },

            pid: None,
            stable_id: None,
            numeric_value: None,
            min_value: None,
            max_value: None,
            raw: RawPlatformData::Synthetic,
            index: 8,
            children_indices: vec![],
            parent_index: Some(4),
        },
    ];

    root_element("My App".to_string(), Some(1234), (1920, 1080), elements)
}

// ── Tree basic operations ──

#[test]
fn tree_root() {
    let root = sample_tree();
    assert_eq!(root.role, Role::Window);
    assert_eq!(root.name.as_deref(), Some("My App"));
}

#[test]
fn tree_get_by_index() {
    let root = sample_tree();
    let button = root.subtree().into_iter().find(|e| e.index == 2).unwrap();
    assert_eq!(button.role, Role::Button);
    assert_eq!(button.name.as_deref(), Some("Back"));
}

#[test]
fn tree_get_nonexistent() {
    let root = sample_tree();
    assert!(root
        .subtree()
        .into_iter()
        .find(|e| e.index == 999)
        .is_none());
}

#[test]
fn tree_len() {
    let root = sample_tree();
    assert_eq!(root.subtree().len(), 9);
    assert!(!root.subtree().is_empty());
}

#[test]
fn tree_children() {
    let root = sample_tree();
    let toolbar = root.subtree().into_iter().find(|e| e.index == 1).unwrap();
    let children = toolbar.children();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].role, Role::Button);
    assert_eq!(children[1].role, Role::TextField);
}

#[test]
fn tree_parent() {
    let root = sample_tree();
    let button = root.subtree().into_iter().find(|e| e.index == 2).unwrap();
    let parent = button.parent().unwrap();
    assert_eq!(parent.role, Role::Toolbar);
    assert!(root.parent().is_none());
}

#[test]
fn tree_subtree() {
    let root = sample_tree();
    let toolbar = root.subtree().into_iter().find(|e| e.index == 1).unwrap();
    let subtree = toolbar.subtree();
    assert_eq!(subtree.len(), 3);
    assert_eq!(subtree[0].role, Role::Toolbar);
    assert_eq!(subtree[1].role, Role::Button);
    assert_eq!(subtree[2].role, Role::TextField);
}

#[test]
fn tree_display() {
    let root = sample_tree();
    let display = root.to_string();
    assert!(display.contains("[0] window \"My App\""));
    assert!(display.contains("  [1] toolbar \"Main Toolbar\""));
    assert!(display.contains("    [2] button \"Back\""));
    assert!(display.contains("    [3] text_field \"Address Bar\""));
    assert!(display.contains("value=\"https://example.com\""));
}

#[test]
fn tree_iter() {
    let root = sample_tree();
    let count = root.subtree().into_iter().count();
    assert_eq!(count, 9);
}

// ── Selector queries ──

#[test]
fn query_by_role() {
    let root = sample_tree();
    let buttons = root.query_selector("button").unwrap();
    assert_eq!(buttons.len(), 3);
}

#[test]
fn query_by_exact_name() {
    let root = sample_tree();
    let results = root.query_selector(r#"[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::Button);
}

#[test]
fn query_role_and_name() {
    let root = sample_tree();
    let results = root.query_selector(r#"button[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_name_contains() {
    let root = sample_tree();
    let results = root.query_selector(r#"[name*="addr"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_starts_with() {
    let root = sample_tree();
    let results = root.query_selector(r#"[name^="addr"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_ends_with() {
    let root = sample_tree();
    let results = root.query_selector(r#"[name$="bar"]"#).unwrap();
    assert_eq!(results.len(), 2); // "Main Toolbar" and "Address Bar"
}

#[test]
fn query_direct_child() {
    let root = sample_tree();
    let results = root.query_selector("toolbar > button").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn query_descendant() {
    let root = sample_tree();
    let results = root.query_selector("window button").unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn query_nth() {
    let root = sample_tree();
    let results = root.query_selector("button:nth(2)").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_nth_out_of_range() {
    let root = sample_tree();
    let results = root.query_selector("button:nth(99)").unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_complex() {
    let root = sample_tree();
    let results = root
        .query_selector(r#"toolbar > text_field[name*="Address"]"#)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].value.as_deref(), Some("https://example.com"));
}

#[test]
fn query_by_value() {
    let root = sample_tree();
    let results = root.query_selector(r#"[value*="example"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::TextField);
}

#[test]
fn query_no_match() {
    let root = sample_tree();
    let results = root.query_selector("slider").unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_invalid_selector() {
    let root = sample_tree();
    let result = root.query_selector("foobar");
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidSelector { selector, message } => {
            assert_eq!(selector, "foobar");
            assert!(message.contains("unknown role"));
        }
        _ => panic!("expected InvalidSelector error"),
    }
}

#[test]
fn query_empty_selector() {
    let root = sample_tree();
    assert!(root.query_selector("").is_err());
}

#[test]
fn query_check_box() {
    let root = sample_tree();
    let results = root.query_selector("check_box").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].states.checked, Some(Toggled::Off));
}

#[test]
fn query_web_area_children() {
    let root = sample_tree();
    let results = root.query_selector("web_area > button").unwrap();
    assert_eq!(results.len(), 2);
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

    let zero = ActionData::NumericValue(0.0);
    assert!(zero.validate(Action::SetValue).is_ok());

    let negative = ActionData::NumericValue(-10.0);
    assert!(negative.validate(Action::SetValue).is_ok());

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

    let neg_inf = ActionData::NumericValue(f64::NEG_INFINITY);
    assert!(matches!(
        neg_inf.validate(Action::SetValue),
        Err(Error::InvalidActionData { .. })
    ));
}

#[test]
fn validate_other_action_data_always_ok() {
    let text = ActionData::Value("hello".to_string());
    assert!(text.validate(Action::TypeText).is_ok());

    let empty = ActionData::Value(String::new());
    assert!(empty.validate(Action::TypeText).is_ok());

    let scroll = ActionData::ScrollAmount(0.0);
    assert!(scroll.validate(Action::ScrollDown).is_ok());

    let neg_scroll = ActionData::ScrollAmount(-3.0);
    assert!(neg_scroll.validate(Action::ScrollDown).is_ok());
}

// ── Error ──

#[test]
fn error_display() {
    let err = Error::PermissionDenied {
        instructions: "Enable in System Preferences".to_string(),
    };
    assert!(format!("{}", err).contains("Permission denied"));

    let err = Error::AppNotFound {
        target: "Safari".to_string(),
    };
    assert!(format!("{}", err).contains("Safari"));

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
        index: 0,
        children_indices: vec![],
        parent_index: None,
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

    let raw_win = RawPlatformData::Windows {
        control_type_id: 50000,
        automation_id: Some("SubmitButton".to_string()),
        class_name: Some("Button".to_string()),
    };
    let json = serde_json::to_string(&raw_win).unwrap();
    assert!(json.contains("50000"));

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

// ── Platform backend ──
// These tests use create_provider() to get a fresh instance rather than the
// global singleton. On Windows, initializing the COM singleton in unit tests
// causes STATUS_ACCESS_VIOLATION during process exit.

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

#[test]
fn platform_provider_operations_return_errors() {
    let provider = match xa11y::create_provider() {
        Ok(p) => p,
        Err(_) => return,
    };

    let result = provider.resolve_pid_by_name("NonexistentApp12345");
    assert!(result.is_err());
}

// ── Selector edge cases ──

#[test]
fn selector_multiple_attr_filters() {
    let root = sample_tree();
    let results = root
        .query_selector(r#"[name*="address"][role="text_field"]"#)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::TextField);
}

#[test]
fn selector_descendant_chain() {
    let root = sample_tree();
    let results = root.query_selector("window toolbar button").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn selector_mixed_combinators() {
    let root = sample_tree();
    let results = root.query_selector("window > web_area button").unwrap();
    assert_eq!(results.len(), 2);
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

// ── Locator ──

/// Mock provider that returns a fixed tree for Locator tests.
struct MockProvider {
    root: Element,
    last_action: std::sync::Mutex<Option<(u32, Action)>>,
}

impl MockProvider {
    fn new() -> Self {
        Self {
            root: sample_tree(),
            last_action: std::sync::Mutex::new(None),
        }
    }
}

fn mock_app() -> (Arc<MockProvider>, App) {
    let p = Arc::new(MockProvider::new());
    let app = App::from_name(Arc::clone(&p) as Arc<dyn Provider>, "My App").unwrap();
    (p, app)
}

use std::sync::Arc;

impl Provider for MockProvider {
    fn resolve_pid_by_name(&self, _name: &str) -> xa11y::Result<u32> {
        Ok(1)
    }

    fn get_elements(&self, _pid: u32) -> xa11y::Result<Element> {
        Ok(self.root.clone())
    }

    fn get_apps(&self) -> xa11y::Result<Element> {
        Ok(self.root.clone())
    }

    fn perform_action(
        &self,
        element: &Element,
        action: Action,
        _data: Option<ActionData>,
    ) -> xa11y::Result<()> {
        *self.last_action.lock().unwrap() = Some((element.index, action));
        Ok(())
    }

    fn check_permissions(&self) -> xa11y::Result<PermissionStatus> {
        Ok(PermissionStatus::Granted)
    }

    fn subscribe(&self, _pid: u32) -> xa11y::Result<xa11y::Subscription> {
        Err(xa11y::Error::Platform {
            code: -1,
            message: "MockProvider does not support subscribe".to_string(),
        })
    }
}

#[test]
fn locator_basic_query() {
    let (_, app) = mock_app();
    let loc = app.locator("button[name=\"Submit\"]");
    let element = loc.element().unwrap();
    assert_eq!(element.role, Role::Button);
    assert_eq!(element.name.as_deref(), Some("Submit"));
    assert!(loc.exists().unwrap());
}

#[test]
fn locator_press_dispatches_action() {
    let (p, app) = mock_app();
    let loc = app.locator("button[name=\"Submit\"]");
    loc.press().unwrap();
    let (idx, action) = p.last_action.lock().unwrap().unwrap();
    assert_eq!(action, Action::Press);
    // Submit button is index 6 in sample_tree
    assert_eq!(idx, 6);
}

#[test]
fn locator_not_found() {
    let (_, app) = mock_app();
    let loc = app.locator("button[name=\"NonExistent\"]");
    assert!(!loc.exists().unwrap());
    assert!(loc.press().is_err());
}

#[test]
fn locator_nth() {
    let (_, app) = mock_app();
    // There are 3 buttons: Back(2), Submit(6), Cancel(7)
    let loc = app.locator("button").nth(1);
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Submit"));
}

#[test]
fn locator_count() {
    let (_, app) = mock_app();
    let loc = app.locator("button");
    assert_eq!(loc.count().unwrap(), 3);
}

#[test]
fn locator_child() {
    let (_, app) = mock_app();
    let loc = app.locator("toolbar").child("button");
    // Toolbar has one button child: "Back"
    assert_eq!(loc.element().unwrap().name.as_deref(), Some("Back"));
}

#[test]
fn locator_states() {
    let (_, app) = mock_app();
    // Cancel button is disabled
    let loc = app.locator("button[name=\"Cancel\"]");
    let element = loc.element().unwrap();
    assert!(!element.states.enabled);
    assert!(element.states.visible);
}

#[test]
fn locator_selector_getter() {
    let (_, app) = mock_app();
    let loc = app.locator("button").child("text_field");
    assert_eq!(loc.selector(), "button > text_field");
}
