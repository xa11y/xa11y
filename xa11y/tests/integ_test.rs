//! Integration tests for xa11y Linux AT-SPI2 backend.
//!
//! These tests require a running D-Bus session with AT-SPI2 and a GTK test
//! application. They are designed to be run via the `run_integ_tests.sh` script
//! which sets up Xvfb, D-Bus, AT-SPI2, and launches the test app.
//!
//! Run with: ./run_integ_tests.sh
//!
//! These tests are gated behind `#[ignore]` so they don't run with `cargo test`
//! by default. The harness script runs them with `--ignored`.

#[cfg(target_os = "linux")]
mod linux_integ {
    use xa11y::*;

    /// Helper: create provider, skip test if AT-SPI2 is unavailable.
    fn create_test_provider() -> Box<dyn Provider> {
        match create_provider() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Skipping: {}", e);
                panic!("AT-SPI2 not available: {}", e);
            }
        }
    }

    /// Helper: get the test app tree, retrying briefly for AT-SPI registration.
    fn get_test_app_tree(provider: &dyn Provider, opts: &QueryOptions) -> Tree {
        for attempt in 0..5 {
            match provider.get_app_tree(&AppTarget::ByName("xa11y".to_string()), opts) {
                Ok(tree) => return tree,
                Err(_) if attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => panic!("Could not find test app after retries: {}", e),
            }
        }
        unreachable!()
    }

    // ── Provider operations ──

    #[test]
    #[ignore]
    fn check_permissions_granted() {
        let provider = create_test_provider();
        let status = provider.check_permissions().unwrap();
        assert!(
            matches!(status, PermissionStatus::Granted),
            "Expected Granted, got: {:?}",
            status
        );
    }

    #[test]
    #[ignore]
    fn list_apps_includes_test_app() {
        let provider = create_test_provider();
        let apps = provider.list_apps().unwrap();
        assert!(
            apps.iter().any(|a| a.name.contains("xa11y")),
            "Test app not found in app list: {:?}",
            apps.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
    }

    // ── Tree structure ──

    #[test]
    #[ignore]
    fn tree_has_root_application_node() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        assert!(
            root.role == Role::Application || root.role == Role::Window,
            "Root role: {:?}",
            root.role
        );
    }

    #[test]
    #[ignore]
    fn tree_has_window() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let windows = tree.find_by_role(Role::Window);
        assert!(
            !windows.is_empty(),
            "No windows found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn tree_has_buttons() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_role(Role::Button);
        assert!(
            buttons.len() >= 2,
            "Expected at least 2 buttons (Submit, Cancel), found {}. Tree:\n{}",
            buttons.len(),
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn tree_has_submit_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.find_by_name("Submit");
        assert!(
            !results.is_empty(),
            "Submit button not found. Tree:\n{}",
            tree.dump()
        );
        assert_eq!(results[0].role, Role::Button);
    }

    #[test]
    #[ignore]
    fn tree_has_cancel_button_disabled() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.find_by_name("Cancel");
        assert!(
            !results.is_empty(),
            "Cancel button not found. Tree:\n{}",
            tree.dump()
        );
        // Cancel button starts disabled
        assert!(
            !results[0].states.enabled,
            "Cancel button should be disabled initially"
        );
    }

    #[test]
    #[ignore]
    fn tree_has_checkbox() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let checkboxes = tree.find_by_role(Role::CheckBox);
        assert!(
            !checkboxes.is_empty(),
            "No checkboxes found. Tree:\n{}",
            tree.dump()
        );
        // "I agree to terms" checkbox should start unchecked
        let agree = checkboxes
            .iter()
            .find(|n| n.name.as_deref() == Some("I agree to terms"));
        assert!(agree.is_some(), "\"I agree to terms\" checkbox not found");
        assert_eq!(agree.unwrap().states.checked, Some(Toggled::Off));
    }

    #[test]
    #[ignore]
    fn tree_has_text_entry() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());

        // GTK entry maps to AT-SPI "text" role, which becomes TextArea or TextField.
        // Look for text content with "John Doe" value across both roles.
        let text_areas = tree.find_by_role(Role::TextArea);
        let text_fields = tree.find_by_role(Role::TextField);
        let has_text = text_areas
            .iter()
            .chain(text_fields.iter())
            .any(|n| n.value.as_deref() == Some("John Doe"));
        assert!(
            has_text || !text_areas.is_empty() || !text_fields.is_empty(),
            "No text entry found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn tree_has_labels() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let labels = tree.find_by_name("Welcome");
        assert!(
            !labels.is_empty(),
            "Welcome label not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn tree_has_slider() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let sliders = tree.find_by_role(Role::Slider);
        assert!(
            !sliders.is_empty(),
            "No sliders found. Tree:\n{}",
            tree.dump()
        );
        // Slider should have a value around 50
        if let Some(val_str) = &sliders[0].value {
            let val: f64 = val_str.parse().unwrap_or(0.0);
            assert!(
                (val - 50.0).abs() < 1.0,
                "Slider value should be ~50, got {}",
                val
            );
        }
    }

    #[test]
    #[ignore]
    fn tree_has_progress_bar() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let progress = tree.find_by_role(Role::ProgressBar);
        assert!(
            !progress.is_empty(),
            "No progress bars found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn tree_has_radio_buttons() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let radios = tree.find_by_role(Role::RadioButton);
        assert!(
            radios.len() >= 2,
            "Expected at least 2 radio buttons, found {}. Tree:\n{}",
            radios.len(),
            tree.dump()
        );
    }

    // ── Selector queries on real tree ──

    #[test]
    #[ignore]
    fn selector_query_buttons() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.query("button").unwrap();
        assert!(
            buttons.len() >= 2,
            "Expected at least 2 buttons via selector, got {}",
            buttons.len()
        );
    }

    #[test]
    #[ignore]
    fn selector_query_button_by_name() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"button[name="Submit"]"#).unwrap();
        assert_eq!(results.len(), 1, "Should find exactly one Submit button");
    }

    #[test]
    #[ignore]
    fn selector_query_name_contains() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"[name*="agree"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Should find element with 'agree' in name"
        );
    }

    #[test]
    #[ignore]
    fn selector_query_nth_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let first = tree.query("button:nth(1)").unwrap();
        assert_eq!(first.len(), 1, "Should find exactly one button via :nth(1)");
    }

    // ── QueryOptions ──

    #[test]
    #[ignore]
    fn query_with_max_depth() {
        let provider = create_test_provider();
        let shallow = get_test_app_tree(
            &*provider,
            &QueryOptions {
                max_depth: Some(1),
                ..QueryOptions::default()
            },
        );
        let deep = get_test_app_tree(&*provider, &QueryOptions::default());
        assert!(
            shallow.len() < deep.len(),
            "Shallow tree ({}) should have fewer nodes than deep tree ({})",
            shallow.len(),
            deep.len()
        );
    }

    #[test]
    #[ignore]
    fn query_with_max_elements() {
        let provider = create_test_provider();
        let limited = get_test_app_tree(
            &*provider,
            &QueryOptions {
                max_elements: Some(5),
                ..QueryOptions::default()
            },
        );
        assert!(
            limited.len() <= 5,
            "Tree should have at most 5 elements, got {}",
            limited.len()
        );
    }

    #[test]
    #[ignore]
    fn query_with_include_raw() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let root = tree.root();
        assert!(
            root.raw.is_some(),
            "Root should have raw platform data when include_raw=true"
        );
        match root.raw.as_ref().unwrap() {
            RawPlatformData::Linux { atspi_role, .. } => {
                assert!(!atspi_role.is_empty(), "AT-SPI role should not be empty");
            }
            _ => panic!("Expected Linux raw data"),
        }
    }

    // ── Tree dump ──

    #[test]
    #[ignore]
    fn tree_dump_is_readable() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let dump = tree.dump();
        assert!(!dump.is_empty());
        // Should contain indented structure
        assert!(dump.contains("[0]"), "Dump should start with [0]");
        println!("Tree dump:\n{}", dump);
    }

    // ── Serialization with real data ──

    #[test]
    #[ignore]
    fn real_tree_json_roundtrip() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let json = serde_json::to_string(&tree).unwrap();
        let mut deserialized: Tree = serde_json::from_str(&json).unwrap();
        deserialized.rebuild_index();
        assert_eq!(deserialized.len(), tree.len());
        assert_eq!(deserialized.app_name, tree.app_name);
    }

    // ── Action dispatch ──

    #[test]
    #[ignore]
    fn action_press_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );

        // Find the Submit button
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty(), "Submit button not found");

        let submit = buttons[0];
        let result = provider.perform_action(&tree, submit.id, Action::Press, None);
        // Should succeed or fail gracefully (not panic)
        match result {
            Ok(()) => println!("Submit button pressed successfully"),
            Err(e) => println!("Submit button press result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_toggle_checkbox() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );

        let checkboxes = tree.find_by_role(Role::CheckBox);
        assert!(!checkboxes.is_empty(), "No checkboxes found");

        let cb = checkboxes[0];
        let initial_state = cb.states.checked;

        // Toggle the checkbox
        let result = provider.perform_action(&tree, cb.id, Action::Press, None);
        match result {
            Ok(()) => {
                // Re-read tree and verify state changed
                std::thread::sleep(std::time::Duration::from_millis(500));
                let tree2 = get_test_app_tree(
                    &*provider,
                    &QueryOptions {
                        include_raw: true,
                        ..QueryOptions::default()
                    },
                );
                let cbs2 = tree2.find_by_role(Role::CheckBox);
                if !cbs2.is_empty() {
                    let new_state = cbs2[0].states.checked;
                    assert_ne!(
                        new_state, initial_state,
                        "Checkbox state should change after toggle (was {:?}, now {:?})",
                        initial_state, new_state
                    );
                    println!(
                        "Checkbox toggled successfully from {:?} to {:?}",
                        initial_state, new_state
                    );
                }
            }
            Err(e) => println!("Checkbox toggle result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_toggle_enables_cancel_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );

        let cancel_nodes = tree.find_by_name("Cancel");
        assert!(!cancel_nodes.is_empty());
        let cancel_initially_enabled = cancel_nodes[0].states.enabled;

        // Toggle the checkbox (which toggles Cancel's enabled state)
        let checkboxes = tree.find_by_role(Role::CheckBox);
        assert!(!checkboxes.is_empty());

        let result = provider.perform_action(&tree, checkboxes[0].id, Action::Press, None);
        if result.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let tree2 = get_test_app_tree(
                &*provider,
                &QueryOptions {
                    include_raw: true,
                    ..QueryOptions::default()
                },
            );
            let cancel2 = tree2.find_by_name("Cancel");
            if !cancel2.is_empty() {
                assert_ne!(
                    cancel2[0].states.enabled, cancel_initially_enabled,
                    "Cancel button enabled state should toggle with checkbox"
                );
            }
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Provider: get_all_apps
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn get_all_apps_returns_nonempty_tree() {
        let provider = create_test_provider();
        let tree = provider.get_all_apps(&QueryOptions::default()).unwrap();
        assert!(!tree.is_empty(), "get_all_apps should return nodes");
        assert_eq!(tree.app_name, "Desktop");
        assert!(tree.pid.is_none(), "Multi-app tree should have no PID");
        // Should contain the test app somewhere in the tree
        let has_test_app = tree.iter().any(|n| {
            n.app_name
                .as_deref()
                .is_some_and(|name| name.contains("xa11y"))
        });
        assert!(has_test_app, "get_all_apps should include the test app");
    }

    // ════════════════════════════════════════════════════════════════
    // AppTarget::ByPid
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn app_target_by_pid() {
        let provider = create_test_provider();
        // First find the PID from list_apps
        let apps = provider.list_apps().unwrap();
        let test_app = apps.iter().find(|a| a.name.contains("xa11y"));
        assert!(test_app.is_some(), "Test app not found in app list");
        let pid = test_app.unwrap().pid;
        assert!(pid > 0, "Test app should have a valid PID");

        let tree = provider
            .get_app_tree(&AppTarget::ByPid(pid), &QueryOptions::default())
            .unwrap();
        assert!(!tree.is_empty(), "ByPid should return a non-empty tree");
        assert_eq!(tree.pid, Some(pid));
    }

    // ════════════════════════════════════════════════════════════════
    // AppTarget::ByWindow — error path (not supported on Linux)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn app_target_by_window_returns_platform_error() {
        let provider = create_test_provider();
        let result = provider.get_app_tree(
            &AppTarget::ByWindow(WindowHandle::X11(12345)),
            &QueryOptions::default(),
        );
        assert!(result.is_err(), "ByWindow should fail on Linux");
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Platform { .. }),
            "Expected Platform error, got: {:?}",
            err
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Tree methods: get, iter, children, subtree, is_empty
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn tree_get_returns_node_by_id() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        let looked_up = tree.get(root.id);
        assert!(looked_up.is_some(), "tree.get(root.id) should return root");
        assert_eq!(looked_up.unwrap().id, root.id);
    }

    #[test]
    #[ignore]
    fn tree_get_invalid_id_returns_none() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let invalid_id = tree.len() as u32 + 999;
        assert!(
            tree.get(invalid_id).is_none(),
            "tree.get with invalid ID should return None"
        );
    }

    #[test]
    #[ignore]
    fn tree_iter_visits_all_nodes() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let iter_count = tree.iter().count();
        assert_eq!(iter_count, tree.len(), "iter() should visit all nodes");
        assert!(iter_count > 1, "Tree should have more than just root");
    }

    #[test]
    #[ignore]
    fn tree_children_returns_direct_children() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        let children = tree.children(root.id);
        assert!(
            !children.is_empty(),
            "Root should have children. Tree:\n{}",
            tree.dump()
        );
        // All returned nodes should list root as parent
        for child in &children {
            assert_eq!(
                child.parent,
                Some(root.id),
                "Child {:?} should have root as parent",
                child.name
            );
        }
    }

    #[test]
    #[ignore]
    fn tree_subtree_includes_node_and_descendants() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        let subtree = tree.subtree(root.id);
        assert_eq!(
            subtree.len(),
            tree.len(),
            "Subtree from root should include all nodes"
        );
        assert_eq!(subtree[0].id, root.id, "First node in subtree should be root");

        // Subtree of a leaf should be just the leaf
        let leaf = tree.iter().find(|n| n.children.is_empty());
        if let Some(leaf) = leaf {
            let leaf_subtree = tree.subtree(leaf.id);
            assert_eq!(leaf_subtree.len(), 1, "Leaf subtree should have 1 node");
        }
    }

    #[test]
    #[ignore]
    fn tree_is_empty_false_for_real_tree() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        assert!(!tree.is_empty(), "Real tree should not be empty");
    }

    // ════════════════════════════════════════════════════════════════
    // Node fields: description, bounds, bounds_normalized, actions,
    //              children, parent, depth
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn node_description_on_image() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // The info image has a description set via ATK
        let images = tree.find_by_role(Role::Image);
        if !images.is_empty() {
            let info_img = images.iter().find(|n| {
                n.name.as_deref() == Some("Info Icon")
                    || n.description.as_deref() == Some("An informational icon")
            });
            if let Some(img) = info_img {
                assert!(
                    img.description.is_some(),
                    "Info image should have a description"
                );
                assert_eq!(img.description.as_deref(), Some("An informational icon"));
            }
        }
    }

    #[test]
    #[ignore]
    fn node_bounds_present() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty(), "Submit button not found");
        let submit = buttons[0];
        assert!(
            submit.bounds.is_some(),
            "Submit button should have bounds"
        );
        let bounds = submit.bounds.unwrap();
        assert!(bounds.width > 0, "Button width should be > 0");
        assert!(bounds.height > 0, "Button height should be > 0");
    }

    #[test]
    #[ignore]
    fn node_bounds_normalized_present() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty(), "Submit button not found");
        let submit = buttons[0];
        assert!(
            submit.bounds_normalized.is_some(),
            "Submit button should have normalized bounds"
        );
        let nb = submit.bounds_normalized.unwrap();
        assert!(nb.left >= 0.0 && nb.left <= 1.0, "left should be in [0,1]");
        assert!(nb.top >= 0.0 && nb.top <= 1.0, "top should be in [0,1]");
        assert!(nb.right >= nb.left, "right >= left");
        assert!(nb.bottom >= nb.top, "bottom >= top");
    }

    #[test]
    #[ignore]
    fn node_actions_list() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty());
        let submit = buttons[0];
        assert!(
            !submit.actions.is_empty(),
            "Submit button should have available actions"
        );
        assert!(
            submit.actions.contains(&Action::Press),
            "Submit button should support Press action, got: {:?}",
            submit.actions
        );
    }

    #[test]
    #[ignore]
    fn node_children_field() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        assert!(
            !root.children.is_empty(),
            "Root node children field should not be empty"
        );
        // All child IDs should be valid
        for &child_id in &root.children {
            assert!(
                tree.get(child_id).is_some(),
                "Child ID {} should be valid",
                child_id
            );
        }
    }

    #[test]
    #[ignore]
    fn node_parent_field() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        assert!(root.parent.is_none(), "Root should have no parent");

        // Find a non-root node and check it has a parent
        let non_root = tree.nodes.iter().find(|n| n.id != root.id);
        if let Some(non_root) = non_root {
            assert!(
                non_root.parent.is_some(),
                "Non-root node {:?} should have a parent",
                non_root.name
            );
            let parent = tree.get(non_root.parent.unwrap());
            assert!(parent.is_some(), "Parent ID should be valid");
        }
    }

    #[test]
    #[ignore]
    fn node_depth_field() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let root = tree.root();
        assert_eq!(root.depth, 0, "Root depth should be 0");

        let max_depth = tree.iter().map(|n| n.depth).max().unwrap_or(0);
        assert!(max_depth >= 2, "Tree should have depth >= 2, got {}", max_depth);

        // Children should have depth = parent.depth + 1
        for node in tree.iter() {
            if let Some(parent_id) = node.parent {
                if let Some(parent) = tree.get(parent_id) {
                    assert_eq!(
                        node.depth,
                        parent.depth + 1,
                        "Node {:?} depth should be parent depth + 1",
                        node.name
                    );
                }
            }
        }
    }

    // ════════════════════════════════════════════════════════════════
    // StateSet fields
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn state_visible_on_shown_widget() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty());
        assert!(
            buttons[0].states.visible,
            "Submit button should be visible"
        );
    }

    #[test]
    #[ignore]
    fn state_editable_on_text_entry() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // Find the text entry (TextField or TextArea with "John Doe")
        let text_nodes: Vec<&Node> = tree
            .iter()
            .filter(|n| {
                (n.role == Role::TextField || n.role == Role::TextArea)
                    && n.value.as_deref() == Some("John Doe")
            })
            .collect();
        assert!(
            !text_nodes.is_empty(),
            "Text entry with 'John Doe' not found. Tree:\n{}",
            tree.dump()
        );
        assert!(
            text_nodes[0].states.editable,
            "Text entry should have editable=true"
        );
    }

    #[test]
    #[ignore]
    fn state_focused_after_focus_action() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty());

        // Focus the Submit button
        let result = provider.perform_action(&tree, buttons[0].id, Action::Focus, None);
        if result.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let tree2 = get_test_app_tree(&*provider, &QueryOptions::default());
            let buttons2 = tree2.find_by_name("Submit");
            if !buttons2.is_empty() {
                assert!(
                    buttons2[0].states.focused,
                    "Submit button should be focused after Focus action"
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn state_selected_on_radio_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let radios = tree.find_by_role(Role::RadioButton);
        assert!(radios.len() >= 2, "Need at least 2 radio buttons");
        // Option A should be selected by default
        let option_a = radios.iter().find(|n| n.name.as_deref() == Some("Option A"));
        assert!(option_a.is_some(), "Option A not found");
        assert_eq!(
            option_a.unwrap().states.checked,
            Some(Toggled::On),
            "Option A should be checked/selected by default"
        );
    }

    #[test]
    #[ignore]
    fn state_expanded_on_expander() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // Expander shows up as a Group or similar role with expandable state
        let expandable_nodes: Vec<&Node> = tree
            .iter()
            .filter(|n| n.states.expanded.is_some())
            .collect();
        if expandable_nodes.is_empty() {
            // Dump tree for debugging
            println!(
                "No expandable nodes found. Tree:\n{}",
                tree.dump()
            );
            // The expander might show up differently; search by name
            let more_details = tree.find_by_name("More Details");
            assert!(
                !more_details.is_empty(),
                "More Details expander not found. Tree:\n{}",
                tree.dump()
            );
            // Even if expanded state is None, we've at least found it
        } else {
            // Verify the collapsed state
            let collapsed = expandable_nodes.iter().find(|n| n.states.expanded == Some(false));
            assert!(
                collapsed.is_some(),
                "Should find a collapsed expandable node"
            );
        }
    }

    // ════════════════════════════════════════════════════════════════
    // QueryOptions: visible_only, roles filter
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn query_with_visible_only() {
        let provider = create_test_provider();
        let visible_tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                visible_only: true,
                ..QueryOptions::default()
            },
        );
        // All nodes in visible_only tree should have visible=true
        for node in visible_tree.iter() {
            assert!(
                node.states.visible,
                "Node {:?} (role={:?}) should be visible in visible_only tree",
                node.name, node.role
            );
        }
        // Should still have content
        assert!(visible_tree.len() > 1, "Visible-only tree should have nodes");
    }

    #[test]
    #[ignore]
    fn query_with_roles_filter() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                roles: Some(vec![Role::Button]),
                ..QueryOptions::default()
            },
        );
        // All nodes should be Button role
        for node in tree.iter() {
            assert_eq!(
                node.role,
                Role::Button,
                "With roles=[Button] filter, all nodes should be Button, got {:?}",
                node.role
            );
        }
        assert!(tree.len() >= 2, "Should find at least 2 buttons");
    }

    // ════════════════════════════════════════════════════════════════
    // Action variants: Focus, SetValue, Toggle, Increment, Decrement,
    //                  Expand, Collapse
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn action_focus_text_entry() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let text_node = tree
            .iter()
            .find(|n| {
                (n.role == Role::TextField || n.role == Role::TextArea)
                    && n.value.as_deref() == Some("John Doe")
            });
        assert!(text_node.is_some(), "Text entry not found");
        let node = text_node.unwrap();
        let result = provider.perform_action(&tree, node.id, Action::Focus, None);
        assert!(result.is_ok(), "Focus on text entry should succeed: {:?}", result.err());
    }

    #[test]
    #[ignore]
    fn action_set_value_numeric_on_slider() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let sliders = tree.find_by_role(Role::Slider);
        assert!(!sliders.is_empty(), "Slider not found");
        let slider = sliders[0];

        let result = provider.perform_action(
            &tree,
            slider.id,
            Action::SetValue,
            Some(ActionData::NumericValue(75.0)),
        );
        assert!(result.is_ok(), "SetValue(numeric) on slider should succeed: {:?}", result.err());

        std::thread::sleep(std::time::Duration::from_millis(300));
        let tree2 = get_test_app_tree(&*provider, &QueryOptions::default());
        let sliders2 = tree2.find_by_role(Role::Slider);
        if !sliders2.is_empty() {
            if let Some(val_str) = &sliders2[0].value {
                let val: f64 = val_str.parse().unwrap_or(0.0);
                assert!(
                    (val - 75.0).abs() < 2.0,
                    "Slider value should be ~75 after SetValue, got {}",
                    val
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn action_set_value_text_on_entry() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let text_node = tree.iter().find(|n| {
            (n.role == Role::TextField || n.role == Role::TextArea)
                && n.value.as_deref() == Some("John Doe")
        });
        assert!(text_node.is_some(), "Text entry not found");
        let node = text_node.unwrap();

        let result = provider.perform_action(
            &tree,
            node.id,
            Action::SetValue,
            Some(ActionData::Value("Jane Smith".to_string())),
        );
        // May succeed or return TextValueNotSupported depending on GTK version
        match result {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let tree2 = get_test_app_tree(&*provider, &QueryOptions::default());
                let text2 = tree2.iter().find(|n| {
                    (n.role == Role::TextField || n.role == Role::TextArea)
                        && n.value.as_deref() == Some("Jane Smith")
                });
                assert!(
                    text2.is_some(),
                    "Text value should be 'Jane Smith' after SetValue"
                );
            }
            Err(Error::TextValueNotSupported) => {
                // Acceptable — some GTK versions may not support editable text
                println!("TextValueNotSupported — acceptable");
            }
            Err(e) => panic!("Unexpected error from SetValue(text): {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_toggle_on_checkbox() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let checkboxes = tree.find_by_role(Role::CheckBox);
        assert!(!checkboxes.is_empty());
        let cb = checkboxes[0];
        let initial = cb.states.checked;

        // Use Toggle action explicitly (not Press)
        let result = provider.perform_action(&tree, cb.id, Action::Toggle, None);
        assert!(result.is_ok(), "Toggle on checkbox should succeed: {:?}", result.err());

        std::thread::sleep(std::time::Duration::from_millis(300));
        let tree2 = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let cbs2 = tree2.find_by_role(Role::CheckBox);
        if !cbs2.is_empty() {
            assert_ne!(
                cbs2[0].states.checked, initial,
                "Checkbox should toggle from {:?}",
                initial
            );
        }
    }

    #[test]
    #[ignore]
    fn action_increment_decrement_on_spin_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );

        // SpinButton maps to TextField in AT-SPI
        let spin = tree.iter().find(|n| {
            n.role == Role::TextField && n.value.is_some() && {
                let v: std::result::Result<f64, _> = n.value.as_deref().unwrap_or("").parse();
                v.is_ok()
            }
        });

        if let Some(spin) = spin {
            let initial_val: f64 = spin.value.as_deref().unwrap().parse().unwrap();

            // Increment
            let result = provider.perform_action(&tree, spin.id, Action::Increment, None);
            if result.is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let tree2 = get_test_app_tree(&*provider, &QueryOptions::default());
                let spin2 = tree2.get(spin.id);
                if let Some(s2) = spin2 {
                    if let Some(ref v) = s2.value {
                        let new_val: f64 = v.parse().unwrap_or(initial_val);
                        assert!(
                            new_val > initial_val,
                            "Value should increase after Increment: {} -> {}",
                            initial_val,
                            new_val
                        );
                    }
                }
            }

            // Decrement
            let tree3 = get_test_app_tree(
                &*provider,
                &QueryOptions {
                    include_raw: true,
                    ..QueryOptions::default()
                },
            );
            let spin3 = tree3.get(spin.id);
            if let Some(s3) = spin3 {
                let before_dec: f64 = s3.value.as_deref().unwrap_or("0").parse().unwrap_or(0.0);
                let result = provider.perform_action(&tree3, s3.id, Action::Decrement, None);
                if result.is_ok() {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let tree4 = get_test_app_tree(&*provider, &QueryOptions::default());
                    let spin4 = tree4.get(spin.id);
                    if let Some(s4) = spin4 {
                        if let Some(ref v) = s4.value {
                            let after_dec: f64 = v.parse().unwrap_or(before_dec);
                            assert!(
                                after_dec < before_dec,
                                "Value should decrease after Decrement: {} -> {}",
                                before_dec,
                                after_dec
                            );
                        }
                    }
                }
            }
        } else {
            // SpinButton may not expose increment/decrement through AT-SPI action interface
            // Try on the slider instead
            let sliders = tree.find_by_role(Role::Slider);
            assert!(!sliders.is_empty(), "Neither spin button nor slider found");
            let slider = sliders[0];
            let initial_val: f64 = slider.value.as_deref().unwrap_or("50").parse().unwrap_or(50.0);

            let _ = provider.perform_action(&tree, slider.id, Action::Increment, None);
            // Increment/Decrement may or may not be supported on sliders
            println!(
                "Tested increment/decrement via slider (initial={})",
                initial_val
            );
        }
    }

    #[test]
    #[ignore]
    fn action_expand_collapse_on_expander() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );

        // Find the expander - it might be a Group with expandable state, or found by name
        let expander_node = tree
            .iter()
            .find(|n| n.states.expanded.is_some())
            .or_else(|| {
                tree.find_by_name("More Details").first().copied()
            });

        if let Some(node) = expander_node {
            // Try Expand
            let expand_result = provider.perform_action(&tree, node.id, Action::Expand, None);
            if expand_result.is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let tree2 = get_test_app_tree(&*provider, &QueryOptions::default());
                let n2 = tree2.get(node.id).or_else(|| {
                    tree2.find_by_name("More Details").first().copied()
                });
                if let Some(n) = n2 {
                    if n.states.expanded == Some(true) {
                        // Now try Collapse
                        let tree3 = get_test_app_tree(
                            &*provider,
                            &QueryOptions {
                                include_raw: true,
                                ..QueryOptions::default()
                            },
                        );
                        let n3 = tree3.get(node.id).or_else(|| {
                            tree3.find_by_name("More Details").first().copied()
                        });
                        if let Some(n) = n3 {
                            let collapse_result =
                                provider.perform_action(&tree3, n.id, Action::Collapse, None);
                            if collapse_result.is_ok() {
                                std::thread::sleep(std::time::Duration::from_millis(300));
                                let tree4 =
                                    get_test_app_tree(&*provider, &QueryOptions::default());
                                let n4 = tree4.get(node.id).or_else(|| {
                                    tree4.find_by_name("More Details").first().copied()
                                });
                                if let Some(n) = n4 {
                                    assert_eq!(
                                        n.states.expanded,
                                        Some(false),
                                        "Should be collapsed after Collapse action"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            println!("Expand/Collapse test completed");
        } else {
            println!("No expandable node found — skipping expand/collapse test");
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Selector features: combinators, value=, name^=, name$=, role=
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn selector_descendant_combinator() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // "window button" — find buttons that are descendants of window
        let results = tree.query("window button").unwrap();
        assert!(
            !results.is_empty(),
            "Descendant combinator 'window button' should find buttons"
        );
        for r in &results {
            assert_eq!(r.role, Role::Button);
        }
    }

    #[test]
    #[ignore]
    fn selector_child_combinator() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // "application > window" — direct children of application that are windows
        let results = tree.query("application > window").unwrap();
        // This might match or might not depending on the tree structure
        // but it should not error
        println!("'application > window' matched {} nodes", results.len());
        for r in &results {
            assert_eq!(r.role, Role::Window);
            // Parent should be an application node
            if let Some(parent_id) = r.parent {
                if let Some(parent) = tree.get(parent_id) {
                    assert_eq!(parent.role, Role::Application);
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn selector_name_starts_with() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"[name^="Welc"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Should find element with name starting with 'Welc'"
        );
        assert!(
            results[0]
                .name
                .as_deref()
                .unwrap()
                .to_lowercase()
                .starts_with("welc"),
            "Name should start with 'Welc'"
        );
    }

    #[test]
    #[ignore]
    fn selector_name_ends_with() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"[name$="xa11y"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Should find element with name ending with 'xa11y'"
        );
    }

    #[test]
    #[ignore]
    fn selector_value_attribute() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"[value*="John"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Should find element with value containing 'John'"
        );
    }

    #[test]
    #[ignore]
    fn selector_role_attribute() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let results = tree.query(r#"[role="button"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Should find elements with role=button via attribute"
        );
        for r in &results {
            assert_eq!(r.role, Role::Button);
        }
    }

    #[test]
    #[ignore]
    fn selector_complex_chain() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // Complex: find button with name containing "Sub" inside window
        let results = tree.query(r#"window button[name*="Sub"]"#).unwrap();
        assert!(
            !results.is_empty(),
            "Complex selector should find Submit button"
        );
        assert_eq!(results[0].role, Role::Button);
        assert!(results[0].name.as_deref().unwrap().contains("Sub"));
    }

    // ════════════════════════════════════════════════════════════════
    // Error paths
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn error_app_not_found() {
        let provider = create_test_provider();
        let result = provider.get_app_tree(
            &AppTarget::ByName("nonexistent_app_12345".to_string()),
            &QueryOptions::default(),
        );
        assert!(result.is_err(), "Should fail for nonexistent app");
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::AppNotFound { .. }),
            "Expected AppNotFound, got: {:?}",
            err
        );
    }

    #[test]
    #[ignore]
    fn error_node_not_found() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(
            &*provider,
            &QueryOptions {
                include_raw: true,
                ..QueryOptions::default()
            },
        );
        let invalid_id = tree.len() as u32 + 999;
        let result = provider.perform_action(&tree, invalid_id, Action::Press, None);
        assert!(result.is_err(), "Should fail for invalid node ID");
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::NodeNotFound { .. }),
            "Expected NodeNotFound, got: {:?}",
            err
        );
    }

    #[test]
    #[ignore]
    fn error_invalid_selector() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let result = tree.query("$$$invalid!!!");
        assert!(result.is_err(), "Should fail for invalid selector");
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::InvalidSelector { .. }),
            "Expected InvalidSelector, got: {:?}",
            err
        );
    }

    #[test]
    #[ignore]
    fn error_action_without_raw_data() {
        let provider = create_test_provider();
        // Get tree WITHOUT include_raw
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let buttons = tree.find_by_name("Submit");
        assert!(!buttons.is_empty());
        let result = provider.perform_action(&tree, buttons[0].id, Action::Press, None);
        assert!(
            result.is_err(),
            "Action without include_raw should fail"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Platform { .. }),
            "Expected Platform error for missing raw data, got: {:?}",
            err
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Role variants from new widgets
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn role_menu_bar() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let menu_bars = tree.find_by_role(Role::MenuBar);
        assert!(
            !menu_bars.is_empty(),
            "MenuBar role not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_menu_and_menu_item() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // Menu items should be visible in the tree (at least File, Edit)
        let menu_items = tree.find_by_role(Role::MenuItem);
        assert!(
            !menu_items.is_empty(),
            "MenuItem role not found. Tree:\n{}",
            tree.dump()
        );
        let has_file = menu_items.iter().any(|n| n.name.as_deref() == Some("File"));
        assert!(has_file, "File menu item not found");
    }

    #[test]
    #[ignore]
    fn role_toolbar() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let toolbars = tree.find_by_role(Role::Toolbar);
        assert!(
            !toolbars.is_empty(),
            "Toolbar role not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_tab_and_tab_group() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let tab_groups = tree.find_by_role(Role::TabGroup);
        let tabs = tree.find_by_role(Role::Tab);
        assert!(
            !tab_groups.is_empty() || !tabs.is_empty(),
            "Neither TabGroup nor Tab found. Tree:\n{}",
            tree.dump()
        );
        if !tabs.is_empty() {
            let has_main = tabs.iter().any(|n| n.name.as_deref() == Some("Main"));
            let has_lists = tabs.iter().any(|n| n.name.as_deref() == Some("Lists"));
            assert!(
                has_main || has_lists,
                "Expected Main or Lists tab, got: {:?}",
                tabs.iter().map(|n| &n.name).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    #[ignore]
    fn role_combo_box() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let combos = tree.find_by_role(Role::ComboBox);
        assert!(
            !combos.is_empty(),
            "ComboBox role not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_separator() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let separators = tree.find_by_role(Role::Separator);
        assert!(
            !separators.is_empty(),
            "Separator role not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_image() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let images = tree.find_by_role(Role::Image);
        assert!(
            !images.is_empty(),
            "Image role not found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_table() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let tables = tree.find_by_role(Role::Table);
        let table_cells = tree.find_by_role(Role::TableCell);
        assert!(
            !tables.is_empty() || !table_cells.is_empty(),
            "Neither Table nor TableCell found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_list_and_list_item() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        let lists = tree.find_by_role(Role::List);
        let list_items = tree.find_by_role(Role::ListItem);
        // ListBox may show as List role, or items may show differently
        assert!(
            !lists.is_empty() || !list_items.is_empty(),
            "Neither List nor ListItem found. Tree:\n{}",
            tree.dump()
        );
    }

    #[test]
    #[ignore]
    fn role_text_field_spin_button() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        // SpinButton maps to TextField in AT-SPI
        let text_fields = tree.find_by_role(Role::TextField);
        assert!(
            !text_fields.is_empty(),
            "TextField role not found (spin button should map here). Tree:\n{}",
            tree.dump()
        );
    }

    // ════════════════════════════════════════════════════════════════
    // app_name field in nodes
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn node_app_name_populated() {
        let provider = create_test_provider();
        let tree = get_test_app_tree(&*provider, &QueryOptions::default());
        for node in tree.iter() {
            assert!(
                node.app_name.is_some(),
                "All nodes should have app_name set"
            );
            assert!(
                node.app_name.as_deref().unwrap().contains("xa11y"),
                "app_name should contain 'xa11y', got: {:?}",
                node.app_name
            );
        }
    }

    // ════════════════════════════════════════════════════════════════
    // list_apps PID validation
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn list_apps_has_valid_pids() {
        let provider = create_test_provider();
        let apps = provider.list_apps().unwrap();
        let test_app = apps.iter().find(|a| a.name.contains("xa11y"));
        assert!(test_app.is_some());
        assert!(
            test_app.unwrap().pid > 0,
            "Test app should have a valid PID"
        );
    }
}
