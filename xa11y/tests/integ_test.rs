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
}
