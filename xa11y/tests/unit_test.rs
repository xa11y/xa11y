//! Unit tests for xa11y core types, tree operations, and selector engine.
//!
//! These tests exercise the public API without requiring platform accessibility
//! permissions, using manually constructed trees. No real accessibility backend
//! or running applications are needed.

use xa11y::*;

/// Helper to build a sample accessibility tree for testing.
fn sample_tree() -> Tree {
    let nodes = vec![
        Node {
            id: 0,
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
            bounds_normalized: Some(NormalizedRect {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
            actions: vec![],
            states: StateSet::default(),
            children: vec![1, 4],
            parent: None,
            depth: 0,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 1,
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
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            children: vec![2, 3],
            parent: Some(0),
            depth: 1,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 2,
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
            bounds_normalized: None,
            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(1),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 3,
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
            bounds_normalized: None,
            actions: vec![Action::Focus, Action::SetValue],
            states: StateSet {
                enabled: true,
                visible: true,
                editable: true,
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(1),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 4,
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
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            children: vec![5, 6, 7, 8],
            parent: Some(0),
            depth: 1,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 5,
            role: Role::Heading,
            name: Some("Welcome".to_string()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            children: vec![],
            parent: Some(4),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 6,
            role: Role::Button,
            name: Some("Submit".to_string()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: true,
                visible: true,
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(4),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 7,
            role: Role::Button,
            name: Some("Cancel".to_string()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: false,
                visible: true,
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(4),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
        Node {
            id: 8,
            role: Role::CheckBox,
            name: Some("I agree to terms".to_string()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![Action::Press, Action::Toggle],
            states: StateSet {
                enabled: true,
                visible: true,
                checked: Some(Toggled::Off),
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(4),
            depth: 2,
            app_name: Some("My App".to_string()),
            raw: None,
        },
    ];

    Tree::new(
        1,
        "My App".to_string(),
        Some(1234),
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    )
}

// ── Tree basic operations ──

#[test]
fn tree_root() {
    let tree = sample_tree();
    let root = tree.root();
    assert_eq!(root.id, 0);
    assert_eq!(root.role, Role::Window);
    assert_eq!(root.name.as_deref(), Some("My App"));
}

#[test]
fn tree_get_by_id() {
    let tree = sample_tree();
    let button = tree.get(2).unwrap();
    assert_eq!(button.role, Role::Button);
    assert_eq!(button.name.as_deref(), Some("Back"));
}

#[test]
fn tree_get_nonexistent() {
    let tree = sample_tree();
    assert!(tree.get(999).is_none());
}

#[test]
fn tree_len() {
    let tree = sample_tree();
    assert_eq!(tree.len(), 9);
    assert!(!tree.is_empty());
}

#[test]
fn tree_children() {
    let tree = sample_tree();
    let children = tree.children(1);
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].role, Role::Button);
    assert_eq!(children[1].role, Role::TextField);
}

#[test]
fn tree_subtree() {
    let tree = sample_tree();
    let subtree = tree.subtree(1);
    assert_eq!(subtree.len(), 3);
    assert_eq!(subtree[0].id, 1);
    assert_eq!(subtree[1].id, 2);
    assert_eq!(subtree[2].id, 3);
}

#[test]
fn tree_find_by_role() {
    let tree = sample_tree();
    let buttons = tree.find_by_role(Role::Button);
    assert_eq!(buttons.len(), 3);
}

#[test]
fn tree_find_by_name() {
    let tree = sample_tree();
    let results = tree.find_by_name("submit");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn tree_find_by_name_case_insensitive() {
    let tree = sample_tree();
    let results = tree.find_by_name("BACK");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 2);
}

#[test]
fn tree_find_by_name_substring() {
    let tree = sample_tree();
    let results = tree.find_by_name("bar");
    assert_eq!(results.len(), 2); // "Main Toolbar" and "Address Bar"
}

#[test]
fn tree_dump() {
    let tree = sample_tree();
    let dump = tree.dump();
    assert!(dump.contains("[0] window \"My App\""));
    assert!(dump.contains("  [1] toolbar \"Main Toolbar\""));
    assert!(dump.contains("    [2] button \"Back\""));
    assert!(dump.contains("    [3] text_field \"Address Bar\""));
    assert!(dump.contains("value=\"https://example.com\""));
}

#[test]
fn tree_iter() {
    let tree = sample_tree();
    let ids: Vec<NodeId> = tree.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
}

// ── Selector queries ──

#[test]
fn query_by_role() {
    let tree = sample_tree();
    let buttons = tree.query("button").unwrap();
    assert_eq!(buttons.len(), 3);
}

#[test]
fn query_by_exact_name() {
    let tree = sample_tree();
    let results = tree.query(r#"[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 6);
}

#[test]
fn query_role_and_name() {
    let tree = sample_tree();
    let results = tree.query(r#"button[name="Submit"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 6);
}

#[test]
fn query_name_contains() {
    let tree = sample_tree();
    let results = tree.query(r#"[name*="addr"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_starts_with() {
    let tree = sample_tree();
    let results = tree.query(r#"[name^="addr"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Address Bar"));
}

#[test]
fn query_name_ends_with() {
    let tree = sample_tree();
    let results = tree.query(r#"[name$="bar"]"#).unwrap();
    assert_eq!(results.len(), 2); // "Main Toolbar" and "Address Bar"
}

#[test]
fn query_direct_child() {
    let tree = sample_tree();
    let results = tree.query("toolbar > button").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn query_descendant() {
    let tree = sample_tree();
    let results = tree.query("window button").unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn query_nth() {
    let tree = sample_tree();
    let results = tree.query("button:nth(2)").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Submit"));
}

#[test]
fn query_nth_out_of_range() {
    let tree = sample_tree();
    let results = tree.query("button:nth(99)").unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_complex() {
    let tree = sample_tree();
    let results = tree
        .query(r#"toolbar > text_field[name*="Address"]"#)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 3);
    assert_eq!(results[0].value.as_deref(), Some("https://example.com"));
}

#[test]
fn query_by_value() {
    let tree = sample_tree();
    let results = tree.query(r#"[value*="example"]"#).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 3);
}

#[test]
fn query_no_match() {
    let tree = sample_tree();
    let results = tree.query("slider").unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn query_invalid_selector() {
    let tree = sample_tree();
    let result = tree.query("foobar");
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
    let tree = sample_tree();
    assert!(tree.query("").is_err());
}

#[test]
fn query_check_box() {
    let tree = sample_tree();
    let results = tree.query("check_box").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].states.checked, Some(Toggled::Off));
}

#[test]
fn query_web_area_children() {
    let tree = sample_tree();
    let results = tree.query("web_area > button").unwrap();
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
}

#[test]
fn toggled_variants() {
    assert_ne!(Toggled::Off, Toggled::On);
    assert_ne!(Toggled::On, Toggled::Mixed);
    assert_ne!(Toggled::Off, Toggled::Mixed);
}

// ── Rect and NormalizedRect ──

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

#[test]
fn normalized_rect_range() {
    let rect = NormalizedRect {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
    assert!(rect.left >= 0.0 && rect.left <= 1.0);
    assert!(rect.right >= 0.0 && rect.right <= 1.0);
}

// ── Action ──

#[test]
fn action_display() {
    assert_eq!(format!("{}", Action::Press), "Press");
    assert_eq!(format!("{}", Action::SetValue), "SetValue");
    assert_eq!(format!("{}", Action::ScrollIntoView), "ScrollIntoView");
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

    let err = Error::NodeNotFound { node_id: 42 };
    assert!(format!("{}", err).contains("42"));

    let err = Error::ElementStale { node_id: 7 };
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
fn tree_json_roundtrip() {
    let tree = sample_tree();
    let json = serde_json::to_string(&tree).unwrap();
    let mut deserialized: Tree = serde_json::from_str(&json).unwrap();
    deserialized.rebuild_index();

    assert_eq!(deserialized.app_name, "My App");
    assert_eq!(deserialized.pid, Some(1234));
    assert_eq!(deserialized.screen_size, (1920, 1080));
    assert_eq!(deserialized.len(), 9);

    let root = deserialized.root();
    assert_eq!(root.role, Role::Window);
    assert_eq!(root.name.as_deref(), Some("My App"));

    let buttons = deserialized.query("button").unwrap();
    assert_eq!(buttons.len(), 3);
}

#[test]
fn node_json_serialization() {
    let node = Node {
        id: 0,
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
        bounds_normalized: None,
        actions: vec![Action::Press],
        states: StateSet {
            enabled: true,
            visible: true,
            focused: true,
            ..StateSet::default()
        },
        children: vec![],
        parent: None,
        depth: 0,
        app_name: None,
        raw: None,
    };

    let json = serde_json::to_string_pretty(&node).unwrap();
    let deserialized: Node = serde_json::from_str(&json).unwrap();
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

// ── Event types ──

#[test]
fn event_filter_all() {
    let filter = EventFilter::all();
    assert!(filter.kinds.is_empty());
    assert!(filter.selector.is_none());
    assert!(filter.state_flags.is_empty());
}

#[test]
fn event_filter_kinds() {
    let filter = EventFilter::kinds(&[EventKind::FocusChanged, EventKind::ValueChanged]);
    assert_eq!(filter.kinds.len(), 2);
}

#[test]
fn event_filter_selector() {
    let filter = EventFilter::selector("button[name=\"Submit\"]");
    assert_eq!(filter.selector.as_deref(), Some("button[name=\"Submit\"]"));
}

#[test]
fn event_filter_combined() {
    let filter = EventFilter::new(&[EventKind::StateChanged], Some("check_box"));
    assert_eq!(filter.kinds.len(), 1);
    assert_eq!(filter.selector.as_deref(), Some("check_box"));
}

// ── QueryOptions ──

#[test]
fn query_options_default() {
    let opts = QueryOptions::default();
    assert!(opts.max_depth.is_none());
    assert!(opts.max_elements.is_none());
    assert!(!opts.visible_only);
    assert!(opts.roles.is_none());
    assert!(!opts.include_raw);
}

// ── Provider trait / AppTarget ──

#[test]
fn app_target_variants() {
    let by_name = AppTarget::ByName("Safari".to_string());
    let by_pid = AppTarget::ByPid(1234);
    let by_window = AppTarget::ByWindow(WindowHandle::MacOS(42));

    let json = serde_json::to_string(&by_name).unwrap();
    assert!(json.contains("Safari"));

    let json = serde_json::to_string(&by_pid).unwrap();
    assert!(json.contains("1234"));

    let json = serde_json::to_string(&by_window).unwrap();
    assert!(json.contains("42"));
}

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
//
// These tests require a D-Bus session (Linux) or platform accessibility.
// They are skipped gracefully if `create_provider()` fails (e.g., in headless CI
// without a D-Bus session).

#[test]
fn platform_provider_creates_or_fails_gracefully() {
    // create_provider may fail if no D-Bus session is available (Linux)
    // or if accessibility isn't enabled — that's fine for unit tests.
    let _result = xa11y::create_provider();
}

#[test]
fn platform_provider_check_permissions() {
    let provider = match xa11y::create_provider() {
        Ok(p) => p,
        Err(_) => return, // skip if no provider available
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
        Err(_) => return, // skip if no provider available
    };

    let result = provider.get_app_tree(
        &AppTarget::ByName("NonexistentApp12345".to_string()),
        &QueryOptions::default(),
    );
    assert!(result.is_err());

    // list_apps may succeed on a real system, so we just verify it doesn't panic
    let _result = provider.list_apps();
}

// ── Selector edge cases ──

#[test]
fn selector_multiple_attr_filters() {
    let tree = sample_tree();
    let results = tree
        .query(r#"[name*="address"][role="text_field"]"#)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 3);
}

#[test]
fn selector_descendant_chain() {
    let tree = sample_tree();
    let results = tree.query("window toolbar button").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name.as_deref(), Some("Back"));
}

#[test]
fn selector_mixed_combinators() {
    let tree = sample_tree();
    let results = tree.query("window > web_area button").unwrap();
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

    let scroll = ActionData::ScrollAmount {
        direction: ScrollDirection::Down,
        amount: 100.0,
    };
    let json = serde_json::to_string(&scroll).unwrap();
    assert!(json.contains("Down"));

    let point = ActionData::Point { x: 10.0, y: 20.0 };
    let json = serde_json::to_string(&point).unwrap();
    assert!(json.contains("10.0"));
}
