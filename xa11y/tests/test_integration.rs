//! Integration tests for the xa11y umbrella crate.
//!
//! These tests verify that the umbrella crate correctly re-exports core types
//! and that the platform provider can be instantiated on each OS.

use xa11y::*;

#[test]
fn test_create_provider() {
    // Should not panic — provider creation should work on all supported platforms
    let _provider = create_provider();
}

#[test]
fn test_provider_check_permissions() {
    let provider = create_provider();
    // The provider should return a result (either Granted or Denied),
    // or a "not yet implemented" error — but not panic.
    let result = provider.check_permissions();
    // We don't assert success since CI may not have accessibility permissions,
    // but it should not panic.
    match result {
        Ok(PermissionStatus::Granted) => {
            // Great, we have permissions
        }
        Ok(PermissionStatus::Denied { instructions }) => {
            // Expected in CI environments
            assert!(!instructions.is_empty());
        }
        Err(_) => {
            // "not yet implemented" is acceptable during early development
        }
    }
}

#[test]
fn test_provider_list_apps() {
    let provider = create_provider();
    // list_apps should either succeed or return a "not implemented" error
    match provider.list_apps() {
        Ok(apps) => {
            // If it succeeds, each app should have a name and pid
            for app in &apps {
                assert!(!app.name.is_empty());
                assert!(app.pid > 0);
            }
        }
        Err(_) => {
            // "not yet implemented" is acceptable
        }
    }
}

#[test]
fn test_core_types_accessible() {
    // Verify core types are re-exported through the umbrella crate
    let _role = Role::Button;
    let _action = Action::Press;
    let _kind = EventKind::FocusChanged;
    let _state = ElementState::Visible;
    let _flag = StateFlag::Focused;

    let target = AppTarget::ByName("Test".into());
    match &target {
        AppTarget::ByName(name) => assert_eq!(name, "Test"),
        _ => panic!("wrong variant"),
    }

    let opts = QueryOptions::default();
    assert_eq!(opts.max_depth, u32::MAX);
}

#[test]
fn test_tree_operations_through_umbrella() {
    // Build a tree using types from the umbrella crate
    let nodes = vec![
        Node {
            id: 0,
            role: Role::Window,
            name: Some("Test Window".into()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            }),
            bounds_normalized: Some(NormalizedRect {
                x1: 0.0,
                y1: 0.0,
                x2: 0.417,
                y2: 0.556,
            }),
            actions: vec![],
            states: StateSet::default(),
            children: vec![1, 2],
            parent: None,
            depth: 0,
            app_name: Some("Test".into()),
            raw: None,
        },
        Node {
            id: 1,
            role: Role::Button,
            name: Some("OK".into()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: true,
                visible: true,
                focused: true,
                ..StateSet::default()
            },
            children: vec![],
            parent: Some(0),
            depth: 1,
            app_name: Some("Test".into()),
            raw: None,
        },
        Node {
            id: 2,
            role: Role::Button,
            name: Some("Cancel".into()),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![Action::Press],
            states: StateSet::default(),
            children: vec![],
            parent: Some(0),
            depth: 1,
            app_name: Some("Test".into()),
            raw: None,
        },
    ];

    let tree = Tree::new(
        "Test".into(),
        42,
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    );

    assert_eq!(tree.len(), 3);
    assert_eq!(tree.find_by_role(Role::Button).len(), 2);
    assert_eq!(tree.find_by_name("ok").len(), 1);
    assert_eq!(tree.query("button").unwrap().len(), 2);
    assert_eq!(tree.query(r#"button[name="OK"]"#).unwrap().len(), 1);
    assert_eq!(tree.children(0).len(), 2);
}

#[test]
fn test_tree_json_serialization() {
    let nodes = vec![Node {
        id: 0,
        role: Role::Window,
        name: Some("Test".into()),
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
    }];

    let tree = Tree::new(
        "App".into(),
        1,
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    );
    let json = serde_json::to_string_pretty(&tree).unwrap();

    // Should contain expected fields
    assert!(json.contains("\"app_name\""));
    assert!(json.contains("\"Test\""));
    assert!(json.contains("\"Window\""));

    // Should deserialize back
    let mut restored: Tree = serde_json::from_str(&json).unwrap();
    restored.rebuild_index();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored.root().unwrap().role, Role::Window);
}

#[test]
fn test_event_system_types() {
    let filter = EventFilter::new(
        &[EventKind::FocusChanged, EventKind::ValueChanged],
        Some("button"),
    );
    assert_eq!(filter.kinds.len(), 2);
    assert_eq!(filter.selector.as_deref(), Some("button"));

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
        timestamp: std::time::Duration::from_secs(1),
    };
    assert!(filter.matches(&event));
}
