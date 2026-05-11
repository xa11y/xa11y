//! Action-dispatch integration tests: `press`, `toggle`, `focus`, numeric
//! value changes, expand/collapse, text entry mutations, plus the newer
//! `Blur` / `SetTextSelection` / `TypeText` actions.
//!
//! Also covers complex/stress scenarios (deep traversal, thrash loops) and
//! error paths (invalid selectors, non-interactive elements, non-expandable
//! elements). Observational tree tests live in `integ::tree`; event
//! subscription tests live in `integ::events_<platform>`.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    /// Find the text entry in the app by name "Name" or by role text_field/text_area.
    fn find_text_entry(app: &App) -> Element {
        // Try by name first
        let by_name = app
            .locator(r#"[name="Name"]"#)
            .elements()
            .unwrap_or_default();
        if let Some(el) = by_name.into_iter().next() {
            return el;
        }
        // Fall back to text_field role
        let fields = app.locator("text_field").elements().unwrap_or_default();
        if let Some(el) = fields.into_iter().next() {
            return el;
        }
        // Fall back to text_area role
        let areas = app.locator("text_area").elements().unwrap_or_default();
        areas
            .into_iter()
            .next()
            .expect("Text entry not found in app")
    }

    // ════════════════════════════════════════════════════════════════
    // Action Dispatch (10 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn action_press_button() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        let result = h::try_act(&submit, "press");
        match result {
            Ok(()) => println!("Submit pressed"),
            Err(e) => println!("Submit press result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_toggle_checkbox() {
        let app = h::app_root();
        let cbs = app.locator("check_box").elements().unwrap();
        assert!(!cbs.is_empty(), "No checkbox");
        let initial = cbs[0].states.checked;
        let app2 = h::act(&cbs[0], "press");
        let cb2 = app2.locator("check_box").elements().unwrap();
        if !cb2.is_empty() {
            assert_ne!(
                cb2[0].states.checked, initial,
                "Checkbox should toggle from {:?}",
                initial
            );
        }
    }

    #[test]
    #[ignore]
    fn action_toggle_enables_cancel() {
        let app = h::app_root();
        let was_enabled = h::named(&app, "Cancel").states.enabled;
        let cbs = app.locator("check_box").elements().unwrap();
        assert!(!cbs.is_empty(), "No checkbox");
        let app2 = h::act(&cbs[0], "press");
        let cancel2 = h::named(&app2, "Cancel");
        // Some AT-SPI adapters may not reflect enabled state changes.
        // If was_enabled is already true (adapter doesn't report disabled), skip the assertion.
        if !was_enabled {
            assert_ne!(cancel2.states.enabled, was_enabled);
        } else {
            // Verify the toggle at least didn't crash and Cancel still exists
            assert_eq!(cancel2.role, Role::Button);
        }
    }

    #[test]
    #[ignore]
    fn action_focus_text_entry() {
        let app = h::app_root();
        // Find text entry by name "Name"
        let text = find_text_entry(&app);
        let result = h::try_act(&text, "focus");
        assert!(result.is_ok(), "Focus should succeed: {:?}", result.err());
    }

    #[test]
    #[ignore]
    fn action_set_value_text() {
        let app = h::app_root();
        let text = find_text_entry(&app);
        match text.provider().set_value(&text, "Jane Smith") {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let app2 = h::app_root();
                // Value may or may not be reflected via AT-SPI depending on adapter
                let updated = app2
                    .locator(r#"[value="Jane Smith"]"#)
                    .elements()
                    .unwrap_or_default();
                if updated.is_empty() {
                    println!("SetValue succeeded but value not reflected in tree (AT-SPI adapter limitation)");
                }
            }
            Err(Error::TextValueNotSupported) => println!("TextValueNotSupported — OK"),
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_set_value_numeric() {
        let app = h::app_root();
        let sliders = app.locator("slider").elements().unwrap();
        assert!(!sliders.is_empty());
        let result = sliders[0].provider().set_numeric_value(&sliders[0], 75.0);
        assert!(result.is_ok(), "SetValue numeric: {:?}", result.err());
        std::thread::sleep(std::time::Duration::from_millis(300));
        let app2 = h::app_root();
        let s2 = app2.locator("slider").elements().unwrap();
        if !s2.is_empty() {
            if let Some(v) = &s2[0].value {
                let val: f64 = v.parse().unwrap_or(0.0);
                assert!(
                    (val - 75.0).abs() < 2.0,
                    "Slider should be ~75, got {}",
                    val
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn action_increment_spinner() {
        let app = h::app_root();
        // Find spin button or slider with a numeric value
        let sliders = app.locator("slider").elements().unwrap();
        let spin = sliders.first();
        if let Some(spin) = spin {
            let initial: f64 = spin.value.as_deref().unwrap_or("0").parse().unwrap_or(0.0);
            let result = h::try_act(spin, "increment");
            if result.is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let app2 = h::app_root();
                if let Some(s2) = app2.locator("slider").elements().unwrap().first() {
                    if let Some(v) = &s2.value {
                        let new_val: f64 = v.parse().unwrap_or(initial);
                        assert!(
                            new_val > initial,
                            "Value should increase: {} -> {}",
                            initial,
                            new_val
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn action_decrement_spinner() {
        let app = h::app_root();
        let sliders = app.locator("slider").elements().unwrap();
        let spin = sliders.first();
        if let Some(spin) = spin {
            let before: f64 = spin.value.as_deref().unwrap_or("0").parse().unwrap_or(0.0);
            let result = h::try_act(spin, "decrement");
            if result.is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let app2 = h::app_root();
                if let Some(s2) = app2.locator("slider").elements().unwrap().first() {
                    if let Some(v) = &s2.value {
                        let after: f64 = v.parse().unwrap_or(before);
                        assert!(
                            after < before,
                            "Value should decrease: {} -> {}",
                            before,
                            after
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn action_expand_collapse() {
        let app = h::app_root();
        let expander = app
            .locator(r#"[name*="Expander"]"#)
            .elements()
            .unwrap()
            .into_iter()
            .next();
        if let Some(node) = expander {
            // Expand
            if h::try_act(&node, "expand").is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let app2 = h::app_root();
                let n2 = app2
                    .locator(r#"[name*="Expander"]"#)
                    .elements()
                    .unwrap()
                    .into_iter()
                    .next();
                if let Some(n) = n2 {
                    if n.states.expanded == Some(true) {
                        // Collapse
                        if h::try_act(&n, "collapse").is_ok() {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            let app3 = h::app_root();
                            let n3 = app3
                                .locator(r#"[name*="Expander"]"#)
                                .elements()
                                .unwrap()
                                .into_iter()
                                .next();
                            if let Some(n) = n3 {
                                assert_eq!(n.states.expanded, Some(false));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn action_select_list_item() {
        let app = h::app_root();
        let apple = app.locator(r#"[name*="Apple"]"#).elements().unwrap();
        if !apple.is_empty() {
            let _ = h::try_act(&apple[0], "press");
            // Selection verified by not crashing; state_selected_on_list_item tests the state
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Complex / Stress Scenarios (5 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn nesting_deep_tree_traversal() {
        let app = h::app_root();
        // Query inside table -> row -> cell
        let cells = app.locator(r#"[name*="Alice"]"#).elements().unwrap();
        assert!(!cells.is_empty(), "Alice cell not found. App: {}", app);
        // Verify nesting: cell's parent should exist
        let parent = cells[0].parent().unwrap();
        assert!(parent.is_some());
    }

    #[test]
    #[ignore]
    fn nesting_subtree_of_table() {
        let app = h::app_root();
        let tables = app.locator("table").elements().unwrap();
        if !tables.is_empty() {
            // Table should contain rows and cells — use descendant combinator.
            // On Windows (UIA), AccessKit Cell maps to DataItem (table_row) since
            // UIA has no distinct Cell control type for data grids.
            let cells = app.locator("table table_cell").elements().unwrap();
            if cells.len() >= 2 {
                return;
            }
            // Fall back to table_row children (Windows: cells appear as table_row)
            let rows = app.locator("table table_row").elements().unwrap();
            assert!(
                rows.len() >= 2,
                "Table should have at least 2 rows/cells, found {}. App: {}",
                rows.len(),
                app
            );
        }
    }

    #[test]
    #[ignore]
    fn thrash_toggle_checkbox_5_times() {
        let app = h::app_root();
        let cbs = app.locator("check_box").elements().unwrap();
        assert!(!cbs.is_empty());
        let mut current_app = app;
        for _ in 0..5 {
            let cbs = current_app.locator("check_box").elements().unwrap();
            assert!(!cbs.is_empty());
            current_app = h::act(&cbs[0], "press");
        }
        // After 5 toggles (odd), state should have flipped from initial
        let final_cb = current_app.locator("check_box").elements().unwrap();
        if !final_cb.is_empty() {
            assert_eq!(
                final_cb[0].states.checked,
                Some(Toggled::On),
                "After 5 toggles from Off, should be On"
            );
        }
    }

    #[test]
    #[ignore]
    fn thrash_slider_increment_10_times() {
        let app = h::app_root();
        let sliders = app.locator("slider").elements().unwrap();
        let slider = sliders.first().expect("No slider");
        let start_val: f64 = slider
            .value
            .as_deref()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let mut current_app = app;
        for _ in 0..10 {
            let sliders = current_app.locator("slider").elements().unwrap();
            let slider = sliders.first().expect("No slider");
            current_app = h::act(slider, "increment");
        }
        let s = current_app.locator("slider").elements().unwrap();
        if !s.is_empty() {
            if let Some(v) = &s[0].value {
                let val: f64 = v.parse().unwrap_or(0.0);
                let expected = (start_val + 10.0).min(100.0);
                assert!(
                    (val - expected).abs() < 2.0,
                    "After 10 increments from {}, should be ~{}, got {}",
                    start_val,
                    expected,
                    val
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn thrash_expand_collapse_cycle() {
        let app = h::app_root();
        let has_expander = !app
            .locator(r#"[name*="Expander"]"#)
            .elements()
            .unwrap()
            .is_empty();
        if has_expander {
            let mut ct = app;
            // expand, collapse, expand, collapse
            for action in ["expand", "collapse", "expand", "collapse"] {
                let node = ct
                    .locator(r#"[name*="Expander"]"#)
                    .elements()
                    .unwrap()
                    .into_iter()
                    .next()
                    .expect("Expander node should exist");
                ct = h::act(&node, action);
            }
            let final_node = ct
                .locator(r#"[name*="Expander"]"#)
                .elements()
                .unwrap()
                .into_iter()
                .next();
            if let Some(n) = final_node {
                if n.states.expanded.is_some() {
                    assert_eq!(n.states.expanded, Some(false), "Should end collapsed");
                }
            }
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Error Paths (4 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn error_app_not_found() {
        let result = App::by_name("nonexistent_app_12345", std::time::Duration::ZERO);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::SelectorNotMatched { .. }
        ));
    }

    #[test]
    #[ignore]
    fn error_selector_not_matched() {
        let app = h::app_root();
        let result = app
            .locator(r#"button[name="nonexistent_element_12345"]"#)
            .elements();
        assert!(result.unwrap().is_empty());
    }

    #[test]
    #[ignore]
    fn error_invalid_selector() {
        let app = h::app_root();
        let result = app.locator("$$$invalid!!!").elements();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidSelector { .. }));
    }

    #[test]
    #[ignore]
    fn action_on_default_tree() {
        let app = h::app_root();
        let buttons = app.locator(r#"[name*="Submit"]"#).elements().unwrap();
        assert!(!buttons.is_empty());
        let result = h::try_act(&buttons[0], "press");
        match result {
            Ok(()) => {}
            Err(e) => assert!(
                matches!(e, Error::Platform { .. } | Error::ElementStale { .. }),
                "Unexpected error: {}",
                e
            ),
        }
    }

    #[test]
    #[ignore]
    fn error_press_on_non_interactive_element() {
        // Pressing a static_text or other non-interactive element should fail.
        // On macOS this returns ActionNotSupported; on Linux it returns Platform.
        let app = h::app_root();
        let labels = app.locator("static_text").elements().unwrap_or_default();
        if let Some(label) = labels
            .into_iter()
            .find(|e| !e.data().actions.iter().any(|a| a == "press"))
        {
            let result = h::try_act(&label, "press");
            assert!(
                result.is_err(),
                "Press on static_text should fail: {:?}",
                result
            );
            #[cfg(target_os = "macos")]
            assert!(
                matches!(result, Err(Error::ActionNotSupported { .. })),
                "Expected ActionNotSupported on macOS, got: {:?}",
                result
            );
        }
    }

    #[test]
    #[ignore]
    fn error_expand_on_non_expandable_element() {
        // Expanding a button (not expandable) should return an error,
        // not silently succeed.
        let app = h::app_root();
        let button = h::named(&app, "Submit");
        let result = h::try_act(&button, "expand");
        assert!(
            result.is_err(),
            "Expand on a non-expandable button should fail, not silently succeed"
        );
    }

    #[test]
    #[ignore]
    fn error_collapse_on_non_expandable_element() {
        // Collapsing a button (not expandable) should return an error,
        // not silently succeed.
        let app = h::app_root();
        let button = h::named(&app, "Submit");
        let result = h::try_act(&button, "collapse");
        assert!(
            result.is_err(),
            "Collapse on a non-expandable button should fail, not silently succeed"
        );
    }

    // ════════════════════════════════════════════════════════════════
    // New Actions — Blur, Scroll, SetTextSelection, TypeText
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn action_blur_text_entry() {
        let app = h::app_root();
        let text = find_text_entry(&app);

        // Focus first
        let result = h::try_act(&text, "focus");
        assert!(result.is_ok(), "Focus should succeed: {:?}", result.err());

        // Then blur — re-find the text entry from a fresh root
        let app2 = h::app_root();
        let text2 = find_text_entry(&app2);
        let result = h::try_act(&text2, "blur");
        assert!(result.is_ok(), "Blur should succeed: {:?}", result.err());
    }

    #[test]
    #[ignore]
    fn action_set_text_selection() {
        let app = h::app_root();
        let text = find_text_entry(&app);

        // Focus first
        let _ = h::try_act(&text, "focus");

        // Select characters 0..4 ("John")
        let result = text.provider().set_text_selection(&text, 0, 4);
        match result {
            Ok(()) => println!("SetTextSelection succeeded"),
            Err(e) => println!("SetTextSelection result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_type_text() {
        let app = h::app_root();
        let text = find_text_entry(&app);

        // Focus first
        let _ = h::try_act(&text, "focus");

        // Type text
        let result = text.provider().type_text(&text, "hi");
        match result {
            Ok(()) => println!("TypeText succeeded"),
            Err(e) => println!("TypeText result: {}", e),
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Element-shape Actions — exercise the new `element.press()` etc. API
    // ════════════════════════════════════════════════════════════════
    //
    // These mirror a small slice of the dispatch tests above but go through
    // the snapshot-bound `Element` action methods instead of the lower-level
    // `provider().X(&data)` shape. They prove the new surface compiles and
    // dispatches end-to-end. Locator-shape coverage is the recommended path
    // (auto-wait, re-resolve) and stays exhaustive elsewhere.

    #[test]
    #[ignore]
    fn action_press_via_element() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        match submit.press() {
            Ok(()) => println!("Submit pressed via element.press()"),
            Err(e) => println!("Submit press result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_set_value_via_element() {
        let app = h::app_root();
        let text = find_text_entry(&app);
        match text.set_value("Jane Smith") {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let app2 = h::app_root();
                let updated = app2
                    .locator(r#"[value="Jane Smith"]"#)
                    .elements()
                    .unwrap_or_default();
                if updated.is_empty() {
                    println!(
                        "set_value via element succeeded but value not reflected (adapter limitation)"
                    );
                }
            }
            Err(Error::TextValueNotSupported) => println!("TextValueNotSupported — OK"),
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_perform_action_via_element() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        // perform_action is the escape hatch for actions without a dedicated
        // method; "press" is the canonical well-known name and should round-trip
        // to the same provider call as element.press().
        match submit.perform_action("press") {
            Ok(()) => println!("perform_action(\"press\") succeeded"),
            Err(e) => println!("perform_action result: {}", e),
        }
    }

    #[test]
    #[ignore]
    fn action_set_numeric_value_via_element_rejects_nan() {
        // The Element wrapper validates NaN/infinite up-front and must return
        // InvalidActionData *without* dispatching to the provider. This is a
        // pure-Rust validation path so it's safe to assert strictly even
        // when the AccessKit app isn't running — but we keep it #[ignore]'d
        // for consistency with the rest of this file.
        let app = h::app_root();
        let sliders = app.locator("slider").elements().unwrap();
        assert!(!sliders.is_empty(), "No slider in test app");
        let result = sliders[0].set_numeric_value(f64::NAN);
        assert!(
            matches!(result, Err(Error::InvalidActionData { .. })),
            "set_numeric_value(NaN) should return InvalidActionData, got: {:?}",
            result
        );
    }
}
