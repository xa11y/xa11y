//! Integration tests for xa11y-core types — serialization, tree operations, selectors.

use xa11y_core::*;

/// Helper to create a test node.
fn make_node(id: NodeId, role: Role, name: Option<&str>) -> Node {
    Node {
        id,
        role,
        name: name.map(String::from),
        value: None,
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
    }
}

/// Build a realistic test tree:
/// ```text
/// 0: Window "Test App"
/// ├── 1: Toolbar "Main Toolbar"
/// │   ├── 2: Button "Back"
/// │   ├── 3: Button "Forward"
/// │   └── 4: TextField "Address Bar"
/// ├── 5: Group "Content"
/// │   ├── 6: Heading "Welcome"
/// │   ├── 7: StaticText "Hello World"
/// │   ├── 8: Button "Submit"
/// │   └── 9: CheckBox "Remember Me"
/// └── 10: Group "Status Bar"
///     └── 11: StaticText "Ready"
/// ```
fn build_test_tree() -> Tree {
    let nodes = vec![
        {
            let mut n = make_node(0, Role::Window, Some("Test App"));
            n.children = vec![1, 5, 10];
            n.depth = 0;
            n
        },
        {
            let mut n = make_node(1, Role::Toolbar, Some("Main Toolbar"));
            n.children = vec![2, 3, 4];
            n.parent = Some(0);
            n.depth = 1;
            n
        },
        {
            let mut n = make_node(2, Role::Button, Some("Back"));
            n.parent = Some(1);
            n.depth = 2;
            n.actions = vec![Action::Press];
            n
        },
        {
            let mut n = make_node(3, Role::Button, Some("Forward"));
            n.parent = Some(1);
            n.depth = 2;
            n.actions = vec![Action::Press];
            n
        },
        {
            let mut n = make_node(4, Role::TextField, Some("Address Bar"));
            n.parent = Some(1);
            n.depth = 2;
            n.actions = vec![Action::Focus, Action::SetValue];
            n.states.editable = true;
            n.value = Some("https://example.com".into());
            n
        },
        {
            let mut n = make_node(5, Role::Group, Some("Content"));
            n.children = vec![6, 7, 8, 9];
            n.parent = Some(0);
            n.depth = 1;
            n
        },
        {
            let mut n = make_node(6, Role::Heading, Some("Welcome"));
            n.parent = Some(5);
            n.depth = 2;
            n
        },
        {
            let mut n = make_node(7, Role::StaticText, Some("Hello World"));
            n.parent = Some(5);
            n.depth = 2;
            n
        },
        {
            let mut n = make_node(8, Role::Button, Some("Submit"));
            n.parent = Some(5);
            n.depth = 2;
            n.actions = vec![Action::Press];
            n
        },
        {
            let mut n = make_node(9, Role::CheckBox, Some("Remember Me"));
            n.parent = Some(5);
            n.depth = 2;
            n.actions = vec![Action::Toggle];
            n.states.checked = Some(Toggled::Off);
            n
        },
        {
            let mut n = make_node(10, Role::Group, Some("Status Bar"));
            n.children = vec![11];
            n.parent = Some(0);
            n.depth = 1;
            n
        },
        {
            let mut n = make_node(11, Role::StaticText, Some("Ready"));
            n.parent = Some(10);
            n.depth = 2;
            n
        },
    ];

    Tree::new(
        "Test App".into(),
        1234,
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    )
}

// ─── Tree Structure Tests ───────────────────────────────────────

#[test]
fn test_tree_node_count() {
    let tree = build_test_tree();
    assert_eq!(tree.len(), 12);
    assert!(!tree.is_empty());
}

#[test]
fn test_tree_root() {
    let tree = build_test_tree();
    let root = tree.root().unwrap();
    assert_eq!(root.id, 0);
    assert_eq!(root.role, Role::Window);
    assert_eq!(root.name.as_deref(), Some("Test App"));
}

#[test]
fn test_tree_get_by_id() {
    let tree = build_test_tree();
    let node = tree.get(4).unwrap();
    assert_eq!(node.role, Role::TextField);
    assert_eq!(node.name.as_deref(), Some("Address Bar"));

    assert!(tree.get(999).is_none());
}

#[test]
fn test_tree_children() {
    let tree = build_test_tree();
    let children = tree.children(1);
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].name.as_deref(), Some("Back"));
    assert_eq!(children[1].name.as_deref(), Some("Forward"));
    assert_eq!(children[2].name.as_deref(), Some("Address Bar"));
}

#[test]
fn test_tree_subtree() {
    let tree = build_test_tree();
    let subtree = tree.subtree(5);
    assert_eq!(subtree.len(), 5); // Group + 4 children
    assert_eq!(subtree[0].name.as_deref(), Some("Content"));
}

#[test]
fn test_tree_find_by_role() {
    let tree = build_test_tree();
    let buttons = tree.find_by_role(Role::Button);
    assert_eq!(buttons.len(), 3); // Back, Forward, Submit
}

#[test]
fn test_tree_find_by_name() {
    let tree = build_test_tree();

    let results = tree.find_by_name("back");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));

    // Case-insensitive substring match
    let results = tree.find_by_name("HELLO");
    assert_eq!(results.len(), 1);
}

// ─── Selector Tests ─────────────────────────────────────────────

#[test]
fn test_selector_by_role() {
    let tree = build_test_tree();
    let results = tree.query("button").unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_selector_by_name_exact() {
    let tree = build_test_tree();
    let results = tree.query(r#"[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, Role::Button);
}

#[test]
fn test_selector_by_name_contains() {
    let tree = build_test_tree();
    let results = tree.query(r#"[name*="bar"]"#).unwrap();
    // "Address Bar", "Main Toolbar", "Status Bar"
    assert_eq!(results.len(), 3);
}

#[test]
fn test_selector_by_name_starts_with() {
    let tree = build_test_tree();
    let results = tree.query(r#"[name^="addr"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn test_selector_role_plus_attribute() {
    let tree = build_test_tree();
    let results = tree.query(r#"button[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 8);
}

#[test]
fn test_selector_invalid() {
    let tree = build_test_tree();
    assert!(tree.query("").is_err());
    assert!(tree.query("nonexistent_role").is_err());
}

// ─── Serialization Tests ────────────────────────────────────────

#[test]
fn test_node_json_roundtrip() {
    let mut node = make_node(42, Role::Button, Some("OK"));
    node.actions = vec![Action::Press, Action::Focus];
    node.bounds = Some(Rect {
        x: 100,
        y: 200,
        width: 80,
        height: 30,
    });
    node.states.focused = true;

    let json = serde_json::to_string(&node).unwrap();
    let deserialized: Node = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.id, 42);
    assert_eq!(deserialized.role, Role::Button);
    assert_eq!(deserialized.name.as_deref(), Some("OK"));
    assert_eq!(deserialized.actions.len(), 2);
    assert!(deserialized.states.focused);
}

#[test]
fn test_tree_json_roundtrip() {
    let tree = build_test_tree();
    let json = serde_json::to_string(&tree).unwrap();
    let mut deserialized: Tree = serde_json::from_str(&json).unwrap();
    deserialized.rebuild_index();

    assert_eq!(deserialized.app_name, "Test App");
    assert_eq!(deserialized.pid, 1234);
    assert_eq!(deserialized.len(), 12);
    assert_eq!(
        deserialized.get(4).unwrap().name.as_deref(),
        Some("Address Bar")
    );
}

#[test]
fn test_role_serialization() {
    let role = Role::Button;
    let json = serde_json::to_string(&role).unwrap();
    assert_eq!(json, "\"Button\"");

    let deserialized: Role = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, Role::Button);
}

#[test]
fn test_action_serialization() {
    let action = Action::Press;
    let json = serde_json::to_string(&action).unwrap();
    let deserialized: Action = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, Action::Press);
}

#[test]
fn test_action_data_serialization() {
    let data = ActionData::Value("hello".into());
    let json = serde_json::to_string(&data).unwrap();
    let deserialized: ActionData = serde_json::from_str(&json).unwrap();
    match deserialized {
        ActionData::Value(v) => assert_eq!(v, "hello"),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_raw_platform_data_serialization() {
    let raw = RawPlatformData::MacOS {
        ax_role: "AXButton".into(),
        ax_subrole: None,
        ax_identifier: Some("btn_ok".into()),
    };
    let json = serde_json::to_string(&raw).unwrap();
    let deserialized: RawPlatformData = serde_json::from_str(&json).unwrap();
    match deserialized {
        RawPlatformData::MacOS {
            ax_role,
            ax_identifier,
            ..
        } => {
            assert_eq!(ax_role, "AXButton");
            assert_eq!(ax_identifier.as_deref(), Some("btn_ok"));
        }
        _ => panic!("wrong variant"),
    }
}

// ─── State Tests ────────────────────────────────────────────────

#[test]
fn test_state_defaults() {
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
}

#[test]
fn test_toggled_states() {
    let tree = build_test_tree();
    let checkbox = tree.get(9).unwrap();
    assert_eq!(checkbox.states.checked, Some(Toggled::Off));
}

// ─── Event Filter Tests ────────────────────────────────────────

#[test]
fn test_event_filter_all() {
    let filter = EventFilter::all();
    assert!(filter.kinds.is_empty());
    assert!(filter.selector.is_none());
}

#[test]
fn test_event_filter_kinds() {
    let filter = EventFilter::kinds(&[EventKind::FocusChanged, EventKind::ValueChanged]);
    assert_eq!(filter.kinds.len(), 2);

    let event = Event {
        kind: EventKind::FocusChanged,
        app: AppInfo {
            name: "Test".into(),
            pid: 1,
            bundle_id: None,
        },
        target: None,
        state_flag: None,
        state_value: None,
        timestamp: std::time::Duration::from_millis(0),
    };
    assert!(filter.matches(&event));

    let event2 = Event {
        kind: EventKind::WindowOpened,
        ..event.clone()
    };
    assert!(!filter.matches(&event2));
}

#[test]
fn test_event_filter_with_selector() {
    let filter = EventFilter::new(
        &[EventKind::ValueChanged],
        Some(r#"text_field[name="Address Bar"]"#),
    );
    assert_eq!(filter.kinds.len(), 1);
    assert!(filter.selector.is_some());
}

// ─── Event Serialization Tests ──────────────────────────────────

#[test]
fn test_event_json_roundtrip() {
    let event = Event {
        kind: EventKind::StateChanged,
        app: AppInfo {
            name: "Safari".into(),
            pid: 5678,
            bundle_id: Some("com.apple.Safari".into()),
        },
        target: Some(make_node(3, Role::CheckBox, Some("Dark Mode"))),
        state_flag: Some(StateFlag::Checked),
        state_value: Some(true),
        timestamp: std::time::Duration::from_millis(1500),
    };

    let json = serde_json::to_string(&event).unwrap();
    let deserialized: Event = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.kind, EventKind::StateChanged);
    assert_eq!(deserialized.app.name, "Safari");
    assert_eq!(deserialized.state_flag, Some(StateFlag::Checked));
    assert_eq!(deserialized.state_value, Some(true));
    assert_eq!(deserialized.timestamp.as_millis(), 1500);
}

// ─── Geometry Tests ─────────────────────────────────────────────

#[test]
fn test_rect() {
    let rect = Rect {
        x: 100,
        y: 200,
        width: 300,
        height: 400,
    };
    let json = serde_json::to_string(&rect).unwrap();
    let deserialized: Rect = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, rect);
}

#[test]
fn test_normalized_rect() {
    let rect = NormalizedRect {
        x1: 0.1,
        y1: 0.2,
        x2: 0.5,
        y2: 0.8,
    };
    let json = serde_json::to_string(&rect).unwrap();
    let deserialized: NormalizedRect = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, rect);
}

// ─── Role Tests ─────────────────────────────────────────────────

#[test]
fn test_role_from_selector() {
    assert_eq!(Role::from_selector("button"), Some(Role::Button));
    assert_eq!(Role::from_selector("text_field"), Some(Role::TextField));
    assert_eq!(Role::from_selector("check_box"), Some(Role::CheckBox));
    assert_eq!(Role::from_selector("checkbox"), Some(Role::CheckBox));
    assert_eq!(Role::from_selector("nonexistent"), None);
}

#[test]
fn test_role_roundtrip() {
    let role = Role::TextField;
    assert_eq!(Role::from_selector(role.as_selector()), Some(role));
}

#[test]
fn test_all_roles_roundtrip() {
    let roles = [
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
        Role::SplitGroup,
    ];
    for role in &roles {
        let selector = role.as_selector();
        let parsed = Role::from_selector(selector);
        assert_eq!(parsed, Some(*role), "roundtrip failed for {selector}");
    }
}

// ─── Permission Status Tests ────────────────────────────────────

#[test]
fn test_permission_status_serialization() {
    let granted = PermissionStatus::Granted;
    let json = serde_json::to_string(&granted).unwrap();
    let deserialized: PermissionStatus = serde_json::from_str(&json).unwrap();
    matches!(deserialized, PermissionStatus::Granted);

    let denied = PermissionStatus::Denied {
        instructions: "Go to System Preferences > Privacy > Accessibility".into(),
    };
    let json = serde_json::to_string(&denied).unwrap();
    let deserialized: PermissionStatus = serde_json::from_str(&json).unwrap();
    match deserialized {
        PermissionStatus::Denied { instructions } => {
            assert!(instructions.contains("System Preferences"));
        }
        _ => panic!("expected Denied"),
    }
}

// ─── Query Options Tests ────────────────────────────────────────

#[test]
fn test_query_options_defaults() {
    let opts = QueryOptions::default();
    assert_eq!(opts.max_depth, u32::MAX);
    assert_eq!(opts.max_elements, u32::MAX);
    assert!(!opts.visible_only);
    assert!(opts.roles.is_none());
    assert!(!opts.include_raw);
}

#[test]
fn test_query_options_serialization() {
    let opts = QueryOptions {
        max_depth: 5,
        max_elements: 100,
        visible_only: true,
        roles: Some(vec![Role::Button, Role::TextField]),
        include_raw: true,
    };
    let json = serde_json::to_string(&opts).unwrap();
    let deserialized: QueryOptions = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.max_depth, 5);
    assert_eq!(deserialized.max_elements, 100);
    assert!(deserialized.visible_only);
    assert_eq!(deserialized.roles.unwrap().len(), 2);
    assert!(deserialized.include_raw);
}
