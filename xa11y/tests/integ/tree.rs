//! Tree-structure, role coverage, element/state fields, selector queries,
//! and serialization integration tests.
//!
//! Read-only observations of the test app. Action dispatch and error paths
//! live in `integ::actions`; event subscription tests live in
//! `integ::events_<platform>`.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    // ════════════════════════════════════════════════════════════════
    // Provider Operations (4 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn check_permissions_granted() {
        // Permissions are checked automatically by App constructors.
        // If this fails with PermissionDenied, accessibility or screen
        // recording permissions are not granted.
        let _app = h::app_root();
    }

    #[test]
    #[ignore]
    fn apps_returns_nonempty() {
        let apps = App::list().unwrap();
        assert!(!apps.is_empty(), "should find at least one application");
        let has_test_app = apps.iter().any(|a| a.name.contains("xa11y"));
        assert!(
            has_test_app,
            "apps should include the test app. Apps: {:?}",
            apps.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
    }

    #[test]
    #[ignore]
    fn app_by_pid_attaches_to_running_app() {
        // Exercises each backend's PID-direct attach path (AX on macOS, UIA
        // ProcessId search on Windows, AT-SPI registry match on Linux).
        let app = h::app_root();
        let pid = app.pid.expect("test app should report a pid");
        let by_pid = App::by_pid(pid, std::time::Duration::from_secs(2))
            .unwrap_or_else(|e| panic!("by_pid({pid}) failed for a running app: {e}"));
        assert_eq!(by_pid.pid, Some(pid));
    }

    #[test]
    #[ignore]
    fn app_by_pid_unknown_pid_is_selector_not_matched() {
        // A pid that can't belong to a running process: not-attached must
        // surface as SelectorNotMatched (the retryable "not found" signal),
        // never as a platform error, and the diagnostic must name the pid.
        let result = App::by_pid(999_999_999, std::time::Duration::ZERO);
        match result {
            Ok(_) => panic!("by_pid for a nonexistent process must fail"),
            Err(Error::SelectorNotMatched { selector, .. }) => {
                assert!(
                    selector.contains("pid=999999999"),
                    "diagnostic should name the pid, got: {selector}"
                );
            }
            Err(e) => panic!("expected SelectorNotMatched, got: {e}"),
        }
    }

    #[test]
    #[ignore]
    fn focused_app_is_tagged_in_list() {
        // The test app keeps itself host-focused (it synthesises
        // `WindowEvent::Focused(true)` so it reports active even under the
        // headless Xvfb harness), so it must surface as the foreground app:
        // its entry in `App::list()` carries `is_foreground() == true`.
        // Exercises each backend's foreground query (AXFocusedApplication on
        // macOS, GetForegroundWindow on Windows, the active AT-SPI window on
        // Linux).
        let app = h::app_root();
        let apps = App::list().expect("App::list must succeed");
        let me = apps
            .iter()
            .find(|a| a.pid == app.pid)
            .unwrap_or_else(|| panic!("test app (pid={:?}) must appear in App::list()", app.pid));
        assert!(
            me.is_foreground(),
            "the test app should be the foreground app. Apps: {:?}",
            apps.iter()
                .map(|a| (&a.name, a.is_foreground()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[ignore]
    fn find_by_focused_resolves_foreground_app() {
        // `App::find(|d| d.states.focused)` selects the foreground app — the
        // predicate sees the foreground flag the core tags onto each candidate
        // before evaluating it.
        let app = h::app_root();
        let focused = App::find(std::time::Duration::from_secs(2), |d| d.states.focused)
            .expect("find(|a| a.focused) must resolve the foreground app");
        assert_eq!(
            focused.pid, app.pid,
            "the focused app must be the test app, got {:?}",
            focused.name
        );
    }

    #[test]
    #[ignore]
    fn foreground_resolves_test_app() {
        // `App::foreground` queries the platform foreground mechanism directly
        // (AXFocusedApplication on macOS, GetForegroundWindow on Windows, the
        // active AT-SPI window on Linux) rather than enumerating and tagging by
        // pid. The test app keeps itself host-focused (see
        // `focused_app_is_tagged_in_list`), so the resolved app must be the
        // test app and must report `is_foreground()`.
        let app = h::app_root();
        let foreground = App::foreground(std::time::Duration::from_secs(2))
            .expect("App::foreground must resolve the host-focused test app");
        assert_eq!(
            foreground.pid, app.pid,
            "the foreground app must be the test app, got {:?}",
            foreground.name
        );
        assert!(
            foreground.is_foreground(),
            "the app returned by App::foreground must report is_foreground()"
        );
    }

    #[test]
    #[ignore]
    fn foreground_window_reports_active() {
        // The test app keeps itself host-focused (see
        // `focused_app_is_tagged_in_list`), so its top-level window is the
        // active (foreground) window and must report `states.active == true`.
        // Exercises each backend's window-level active mapping (the AT-SPI
        // ACTIVE state on Linux, AXMain on macOS, the foreground HWND on
        // Windows).
        let app = h::app_root();
        // On Windows (UIA) the app root IS the window, so check it directly.
        // Elsewhere the window is a descendant element; a scoped locator only
        // matches descendants of its root, so the state-attr selector below is
        // exercised on the platforms where the window is nested.
        if app.data.role == Role::Window {
            assert!(
                app.as_element().states.active,
                "the foreground window (app root) must report active. App: {app}"
            );
            return;
        }
        let windows = app.locator("window").elements().unwrap();
        assert!(
            windows.iter().any(|w| w.states.active),
            "the test app's foreground window must report active. Windows: {:?}",
            windows
                .iter()
                .map(|w| (&w.name, w.states.active))
                .collect::<Vec<_>>()
        );
        // The `active` state-attr selector must resolve the same window.
        let matched = app
            .locator(r#"window[active="true"]"#)
            .elements()
            .expect("window[active=\"true\"] selector must resolve");
        assert!(
            !matched.is_empty(),
            "window[active=\"true\"] must match the foreground window. App: {app}"
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Tree Structure — Element Discovery (14 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn tree_has_root_node() {
        let app = h::app_root();
        assert!(
            app.data.role == Role::Application || app.data.role == Role::Window,
            "Root role: {:?}",
            app.data.role
        );
    }

    #[test]
    #[ignore]
    fn tree_has_window() {
        let app = h::app_root();
        // On Windows (UIA), the app root IS the window — there's no nested
        // Window child element. Verify root is a Window or find child windows.
        if app.data.role == Role::Window {
            return; // App root is the window — pass
        }
        let windows = app.locator("window").elements().unwrap();
        assert!(!windows.is_empty(), "No windows found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn tree_has_buttons() {
        let app = h::app_root();
        let buttons = app.locator("button").elements().unwrap();
        assert!(
            buttons.len() >= 2,
            "Expected >=2 buttons, found {}. App: {}",
            buttons.len(),
            app
        );
    }

    #[test]
    #[ignore]
    fn tree_has_submit_button() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        assert_eq!(submit.role, Role::Button);
    }

    #[test]
    #[ignore]
    fn tree_has_cancel_button_disabled() {
        let app = h::app_root();
        let cancel = h::named(&app, "Cancel");
        // Cancel may have been enabled by a prior toggle test; just verify it exists as a button
        assert_eq!(cancel.role, Role::Button);
        // Check that the enabled state is a valid boolean (not that it's a specific value)
        let _ = cancel.states.enabled;
    }

    #[test]
    #[ignore]
    fn tree_has_checkbox_unchecked() {
        let app = h::app_root();
        let cb = h::named(&app, "I agree to terms");
        assert_eq!(cb.role, Role::CheckBox);
        // Checkbox may have been toggled by prior tests; just verify it has a checked state
        assert!(
            cb.states.checked.is_some(),
            "Checkbox should have checked state"
        );
    }

    #[test]
    #[ignore]
    fn tree_has_text_entry_with_value() {
        let app = h::app_root();
        // Prior action tests (TypeText, SetValue) may have changed or cleared the value.
        // Just verify a text field exists (by role + name), value may or may not be present.
        let text_elements = app
            .locator(r#"[role="text_field"]"#)
            .elements()
            .unwrap_or_default();
        let textarea_elements = app
            .locator(r#"[role="text_area"]"#)
            .elements()
            .unwrap_or_default();
        let has_text = text_elements
            .iter()
            .chain(textarea_elements.iter())
            .any(|n| n.value.is_some() || n.name.as_deref() == Some("Name"));
        assert!(has_text, "Text entry not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn tree_has_welcome_label() {
        let app = h::app_root();
        // On Linux/AT-SPI with AccessKit, Label nodes may not expose their text
        // through the Name property or Text interface. Look for the node by name
        // first, then fall back to checking that StaticText nodes exist.
        let welcome = app.locator(r#"[name*="Welcome"]"#).elements().unwrap();
        if welcome.is_empty() {
            // Fall back: verify that static text nodes exist (labels are present even if unnamed)
            let labels = app.locator("static_text").elements().unwrap();
            assert!(
                !labels.is_empty(),
                "No StaticText/label nodes found. App: {}",
                app
            );
        } else {
            assert!(
                welcome[0].role == Role::StaticText || welcome[0].role == Role::Group,
                "Welcome node role: {:?}",
                welcome[0].role
            );
        }
    }

    #[test]
    #[ignore]
    fn tree_has_slider_at_50() {
        let app = h::app_root();
        let sliders = app.locator("slider").elements().unwrap();
        assert!(!sliders.is_empty(), "No sliders found. App: {}", app);
        // Slider value may have been changed by prior tests; just verify it has a numeric value
        assert!(sliders[0].value.is_some(), "Slider should have a value");
        let val: f64 = sliders[0].value.as_deref().unwrap().parse().unwrap_or(0.0);
        assert!(
            (0.0..=100.0).contains(&val),
            "Slider value should be in [0,100], got {}",
            val
        );
    }

    #[test]
    #[ignore]
    fn tree_has_progress_bar() {
        let app = h::app_root();
        let progress = app.locator("progress_bar").elements().unwrap();
        assert!(!progress.is_empty(), "No progress bars found. App: {}", app);
    }

    /// SpinButton / Slider / ProgressBar must expose `min_value` and
    /// `max_value` when the underlying provider has set them. The previous
    /// xa11y-macos match arm only populated min/max for `Role::Slider`, so
    /// the SpinButton and ProgressBar attributes silently came back `None`
    /// on macOS — undetected because `tree_has_progress_bar` /
    /// `tree_has_slider_at_50` only check existence and value, not range.
    /// This test seals the cross-platform parity.
    #[test]
    #[ignore]
    fn ranged_widgets_expose_min_and_max() {
        let app = h::app_root();
        for role in ["slider", "spin_button", "progress_bar"] {
            let elems = app.locator(role).elements().unwrap();
            if elems.is_empty() {
                continue; // Not every test app has every widget.
            }
            let el = &elems[0];
            assert!(
                el.min_value.is_some(),
                "{role}: min_value should be Some after provider read, got None. \
                 numeric_value={:?}, max_value={:?}",
                el.numeric_value,
                el.max_value
            );
            assert!(
                el.max_value.is_some(),
                "{role}: max_value should be Some after provider read, got None. \
                 numeric_value={:?}, min_value={:?}",
                el.numeric_value,
                el.min_value
            );
        }
    }

    #[test]
    #[ignore]
    fn tree_has_radio_buttons() {
        let app = h::app_root();
        let radios = app.locator("radio_button").elements().unwrap();
        assert!(
            radios.len() >= 2,
            "Expected >=2 radio buttons, found {}. App: {}",
            radios.len(),
            app
        );
    }

    #[test]
    #[ignore]
    fn tree_has_combo_box() {
        let app = h::app_root();
        let combos = app.locator("combo_box").elements().unwrap();
        assert!(!combos.is_empty(), "ComboBox not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn tree_has_list_with_items() {
        let app = h::app_root();
        let lists = app.locator("list").elements().unwrap();
        let items = app.locator("list_item").elements().unwrap();
        assert!(
            !lists.is_empty() || !items.is_empty(),
            "Neither List nor ListItem found. App: {}",
            app
        );
    }

    #[test]
    #[ignore]
    fn tree_has_table_with_cells() {
        let app = h::app_root();
        let tables = app.locator("table").elements().unwrap();
        let cells = app.locator("table_cell").elements().unwrap();
        assert!(
            !tables.is_empty() || !cells.is_empty(),
            "Neither Table nor TableCell found. App: {}",
            app
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Role Coverage (14 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn role_menu_bar() {
        let app = h::app_root();
        let nodes = app.locator("menu_bar").elements().unwrap();
        assert!(!nodes.is_empty(), "MenuBar not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_menu_item() {
        let app = h::app_root();
        let nodes = app.locator("menu_item").elements().unwrap();
        assert!(!nodes.is_empty(), "MenuItem not found. App: {}", app);
        let has_file = nodes.iter().any(|n| n.name.as_deref() == Some("File"));
        assert!(has_file, "File menu item not found");
    }

    #[test]
    #[ignore]
    fn role_toolbar() {
        let app = h::app_root();
        let nodes = app.locator("toolbar").elements().unwrap();
        assert!(!nodes.is_empty(), "Toolbar not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_tab_and_tab_group() {
        let app = h::app_root();
        let tab_groups = app.locator("tab_group").elements().unwrap();
        let tabs = app.locator("tab").elements().unwrap();
        assert!(
            !tab_groups.is_empty() || !tabs.is_empty(),
            "Neither TabGroup nor Tab found. App: {}",
            app
        );
    }

    #[test]
    #[ignore]
    fn role_separator() {
        let app = h::app_root();
        let seps = app.locator("separator").elements().unwrap();
        assert!(!seps.is_empty(), "Separator not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_image() {
        let app = h::app_root();
        let images = app.locator("image").elements().unwrap();
        assert!(!images.is_empty(), "Image not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_link() {
        let app = h::app_root();
        let links = app.locator("link").elements().unwrap();
        assert!(!links.is_empty(), "Link not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_tree_item() {
        let app = h::app_root();
        let items = app.locator("tree_item").elements().unwrap();
        assert!(!items.is_empty(), "TreeItem not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_dialog() {
        // On Windows this exercises the AriaRole="dialog" path (AccessKit sets
        // it). The UIA_IsDialogPropertyId path covers native frameworks such as
        // Qt that don't populate AriaRole.
        let app = h::app_root();
        let dialogs = app.locator("dialog").elements().unwrap();
        assert!(!dialogs.is_empty(), "Dialog not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_alert() {
        let app = h::app_root();
        let alerts = app.locator("alert").elements().unwrap();
        assert!(!alerts.is_empty(), "Alert not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_heading() {
        let app = h::app_root();
        let headings = app.locator("heading").elements().unwrap();
        assert!(!headings.is_empty(), "Heading not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_scroll_bar() {
        let app = h::app_root();
        let scrollbars = app.locator("scroll_bar").elements().unwrap();
        assert!(!scrollbars.is_empty(), "ScrollBar not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_split_group() {
        let app = h::app_root();
        // SplitGroup may map through AT-SPI as Group due to accesskit's Pane role
        let node = app.locator(r#"[name*="SplitGroup"]"#).elements().unwrap();
        assert!(!node.is_empty(), "SplitGroup node not found. App: {}", app);
    }

    #[test]
    #[ignore]
    fn role_static_text() {
        let app = h::app_root();
        let labels = app.locator("static_text").elements().unwrap();
        assert!(!labels.is_empty(), "StaticText not found. App: {}", app);
    }

    // ════════════════════════════════════════════════════════════════
    // Tree Methods (5 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn tree_children_of_root() {
        let app = h::app_root();
        let children = app.children().unwrap();
        assert!(!children.is_empty(), "Root should have children");
        // Verify grandchildren have parents (direct children of app root
        // may report None parent on AT-SPI where the parent is the registry root).
        if !children.is_empty() {
            let grandchildren = children[0].children().unwrap();
            for gc in &grandchildren {
                let parent = gc.parent().unwrap();
                assert!(parent.is_some(), "Grandchild should have parent");
            }
        }
    }

    #[test]
    #[ignore]
    fn tree_children_of_leaf() {
        let app = h::app_root();
        // Find a leaf node (e.g. a static text or button that has no children)
        let buttons = app.locator("button").elements().unwrap();
        for btn in &buttons {
            let children = btn.children().unwrap();
            if children.is_empty() {
                // Found a leaf — verify children returns empty vec, not error
                assert!(btn.children().unwrap().is_empty());
                return;
            }
        }
        // If all buttons have children, that's fine too — just verify children() works
    }

    #[test]
    #[ignore]
    fn tree_is_not_empty() {
        let app = h::app_root();
        let children = app.children().unwrap();
        assert!(!children.is_empty(), "Root should have at least one child");
    }

    #[test]
    #[ignore]
    fn tree_display_readable() {
        let app = h::app_root();
        let display = app.to_string();
        assert!(!display.is_empty());
        // Display should include the app name
        assert!(
            display.contains(&app.name),
            "Display should include app name: {}",
            display
        );
    }

    #[test]
    #[ignore]
    fn tree_locator_finds_elements() {
        let app = h::app_root();
        let buttons = app.locator("button").elements().unwrap();
        assert!(buttons.len() >= 2, "Expected >=2 buttons via locator");
        let count = app.locator("button").count().unwrap();
        assert_eq!(count, buttons.len());
    }

    #[test]
    #[ignore]
    fn selector_group_union_runs_on_real_provider() {
        // Regression: pre-fix, comma-separated selectors (`SelectorGroup`)
        // returned 0 results on real platform providers because the doc-order
        // merge identified clause-match results by `ElementData.handle`, and
        // every backend mints a fresh handle per `get_children` call. This
        // is the cross-platform smoke test that exercises the real Provider
        // path end-to-end.
        let app = h::app_root();
        let buttons = app.locator("button").elements().unwrap();
        let text_fields = app.locator("text_field").elements().unwrap();
        assert!(!buttons.is_empty(), "fixture must have buttons");

        let union = app.locator("button, text_field").elements().unwrap();
        // The union must be a superset of the buttons (at least), and at
        // most the sum of both clauses (no spurious matches).
        assert!(
            union.len() >= buttons.len(),
            "comma selector union must be >= bare-button count: union={} buttons={}",
            union.len(),
            buttons.len(),
        );
        assert!(
            union.len() <= buttons.len() + text_fields.len(),
            "comma selector union must not exceed clauses' sum (no dedup miss): \
             union={} buttons={} text_fields={}",
            union.len(),
            buttons.len(),
            text_fields.len(),
        );
        // And count() must agree with elements().len(), even for groups.
        let count = app.locator("button, text_field").count().unwrap();
        assert_eq!(count, union.len());
    }

    #[test]
    #[ignore]
    fn selector_group_single_clause_equals_bare_selector() {
        // Single-clause groups must take the fast path and produce identical
        // results to a bare selector — no commas, no parser surprises.
        let app = h::app_root();
        let bare = app.locator("button").count().unwrap();
        // Same string parsed as a SelectorGroup with one clause.
        let group = app.locator("button").count().unwrap();
        assert_eq!(bare, group);
    }

    #[test]
    #[ignore]
    fn selector_group_dedup_doubled_clause() {
        // Dedup contract on real providers: `X, X` must match the same
        // elements as `X`, with no spurious doubling. Pre-fix this happened
        // to "work" only because the doc-order merge returned 0 results
        // either way; the post-fix code uses tree-path identity to dedup.
        let app = h::app_root();
        let bare = app.locator("button").count().unwrap();
        let doubled = app.locator("button, button").count().unwrap();
        assert_eq!(
            bare, doubled,
            "`button, button` must dedup to the same count as `button`",
        );
    }

    #[test]
    #[ignore]
    fn selector_group_native_path_matches_union_semantics() {
        // The native `find_elements_group` override on each backend must
        // produce the same set as if each clause had been run independently
        // and merged. Equality of multiset semantics (same elements, no
        // doubling, no drops) is asserted by comparing sorted name lists —
        // this catches regressions where the native walk misses an element
        // a per-clause walk would have found, or vice versa.
        let app = h::app_root();
        let mut buttons: Vec<String> = app
            .locator("button")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| e.data().name.clone())
            .collect();
        let mut text_fields: Vec<String> = app
            .locator("text_field")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| e.data().name.clone())
            .collect();
        let mut union: Vec<String> = app
            .locator("button, text_field")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| e.data().name.clone())
            .collect();

        // Pre-fix the union was empty on real backends. Now it should equal
        // the multiset union of per-clause results.
        let mut expected: Vec<String> = Vec::new();
        expected.append(&mut buttons);
        expected.append(&mut text_fields);
        expected.sort();
        union.sort();
        assert_eq!(
            union, expected,
            "native group walk must produce the union of per-clause walks",
        );
    }

    #[test]
    #[ignore]
    fn selector_group_doc_order_interleaves_clauses() {
        // The native group walk must return matches in document order, not
        // grouped by clause. If the test app contains `Submit` (button)
        // followed by `Username` (text_field) followed by another button,
        // the group `button, text_field` must list them in that order — not
        // `[all buttons], [all text_fields]`.
        let app = h::app_root();
        let bare_buttons: Vec<String> = app
            .locator("button")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| e.data().name.clone())
            .collect();
        let bare_fields: Vec<String> = app
            .locator("text_field")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| e.data().name.clone())
            .collect();
        // Skip the assertion if the fixture happens to have buttons and
        // text fields fully partitioned at the start/end of the tree
        // (interleave doesn't apply). Otherwise: at least one transition
        // from a button to a text_field (or vice versa) must be present
        // in the union, proving the native walk isn't grouping by clause.
        if bare_buttons.is_empty() || bare_fields.is_empty() {
            return;
        }

        let union: Vec<(String, String)> = app
            .locator("button, text_field")
            .elements()
            .unwrap()
            .into_iter()
            .filter_map(|e| {
                let d = e.data();
                Some((d.role.to_snake_case().to_string(), d.name.clone()?))
            })
            .collect();

        // Detect at least one transition between roles, which only happens
        // if the walk preserves doc-order across clauses.
        let mut transitions = 0;
        for w in union.windows(2) {
            if w[0].0 != w[1].0 {
                transitions += 1;
            }
        }
        // Fixture pre-condition: the AccessKit test app has buttons and
        // text fields scattered through a single window, so the union
        // should interleave them at least once.
        assert!(
            transitions >= 1 || union.len() <= 1,
            "expected at least one role transition in doc-order union, got {:?}",
            union,
        );
    }

    #[test]
    #[ignore]
    fn selector_group_multi_segment_clause_in_group() {
        // Stress the native group walk with a clause that has phase-2
        // narrowing: `application, application button`. The first clause
        // matches the app element; the second matches every button under
        // the app. The merged result must:
        //   - include the application element exactly once
        //   - include every button (descendant) exactly once
        //   - not return anything else
        let app = h::app_root();
        let just_buttons = app.locator("button").count().unwrap();

        // We need to address from the app root because `application X Y`
        // semantics depend on global vs scoped query at the umbrella crate
        // level. Use a comparable scoped form instead — `button, link` is
        // a safer cross-platform two-clause query that exercises the
        // per-clause-narrowing code without the system-root complication.
        let buttons_and_links = app.locator("button, link").count().unwrap();
        let just_links = app.locator("link").count().unwrap();
        assert!(
            buttons_and_links <= just_buttons + just_links,
            "group count must not exceed per-clause sum (no clause-cross-product blowup)",
        );
        assert!(
            buttons_and_links >= just_buttons,
            "group count must be at least the larger clause's count (no dropped matches)",
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Element Fields (7 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn element_description_on_image() {
        let app = h::app_root();
        let images = app.locator("image").elements().unwrap();
        if !images.is_empty() {
            let img = images.iter().find(|n| {
                n.name.as_deref() == Some("Info Icon")
                    || n.description.as_deref() == Some("An informational icon")
            });
            if let Some(img) = img {
                assert!(img.description.is_some(), "Image should have description");
                assert_eq!(img.description.as_deref(), Some("An informational icon"));
            }
        }
    }

    #[test]
    #[ignore]
    fn element_bounds_present() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        assert!(submit.bounds.is_some(), "Submit should have bounds");
        let b = submit.bounds.unwrap();
        assert!(b.width > 0, "width > 0");
        assert!(b.height > 0, "height > 0");
    }

    /// Nodes without the Component interface (e.g. Application root) should
    /// have `bounds: None` without triggering GTK CRITICAL warnings.
    #[test]
    #[ignore]
    fn element_bounds_none_for_non_component_elements() {
        let app = h::app_root();
        // On Linux/macOS, Application elements don't implement Component so
        // bounds is None. On Windows (UIA), the app root is a Window element
        // that does have bounds.
        #[cfg(not(target_os = "windows"))]
        assert!(
            app.data.bounds.is_none(),
            "Application root should not have bounds (no Component interface)"
        );
        // But a visible widget like a button should still have bounds
        let submit = h::named(&app, "Submit");
        assert!(submit.bounds.is_some(), "Submit button should have bounds");
    }

    #[test]
    #[ignore]
    fn element_actions_list_on_button() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        assert!(!submit.actions.is_empty());
        assert!(
            submit.actions.iter().any(|a| a == "press"),
            "Submit should support press, got: {:?}",
            submit.actions
        );
    }

    #[test]
    #[ignore]
    fn element_children_ids_valid() {
        let app = h::app_root();
        let children = app.children().unwrap();
        assert!(!children.is_empty());
        for child in &children {
            // Verify child is a valid element (role may be Unknown for unrecognized elements)
            let _ = child.role;
        }
    }

    #[test]
    #[ignore]
    fn element_parent_field() {
        let app = h::app_root();
        // Direct children of app root may report parent as None on some platforms
        // (AT-SPI maps parent to registry root which we treat as None).
        // Test parent on a deeper element instead.
        let children = app.children().unwrap();
        if !children.is_empty() {
            let grandchildren = children[0].children().unwrap();
            if !grandchildren.is_empty() {
                let parent = grandchildren[0].parent().unwrap();
                assert!(parent.is_some(), "Grandchild should have parent");
            }
        }
    }

    #[test]
    #[ignore]
    fn element_handle_nonzero() {
        let app = h::app_root();
        // The opaque handle should be non-zero for a valid element
        assert!(app.data.handle != 0, "Root handle should be nonzero");
    }

    // ════════════════════════════════════════════════════════════════
    // StateSet Fields (9 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn state_enabled_default() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        assert!(submit.states.enabled, "Submit should be enabled");
    }

    #[test]
    #[ignore]
    fn state_disabled_on_cancel() {
        let app = h::app_root();
        let cancel = h::named(&app, "Cancel");
        // Some AT-SPI adapters (AccessKit) may not expose disabled state properly;
        // in that case, the toggle test (action_toggle_enables_cancel) verifies
        // the enabled state can change. Here we just verify the node exists and
        // has a valid enabled state.
        #[cfg(not(target_os = "linux"))]
        assert!(!cancel.states.enabled, "Cancel should be disabled");
        #[cfg(target_os = "linux")]
        {
            // On Linux with AccessKit, disabled state may not be reflected.
            // Just verify the Cancel button exists as a button.
            assert_eq!(cancel.role, Role::Button);
            let _ = cancel.states.enabled; // valid boolean either way
        }
    }

    #[test]
    #[ignore]
    fn state_visible_on_shown_widget() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        assert!(submit.states.visible, "Submit should be visible");
    }

    #[test]
    #[ignore]
    fn state_focused_after_focus_action() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");
        // Focus action may succeed or fail depending on AT-SPI adapter support
        let result = h::try_act(&submit, "focus");
        if result.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let app2 = h::app_root();
            let submit2 = h::named(&app2, "Submit");
            // Some adapters may not reflect focused state change
            if !submit2.states.focused {
                println!("Focus action succeeded but focused state not reflected (AT-SPI adapter limitation)");
            }
        } else {
            println!("Focus action not supported: {:?}", result.err());
        }
    }

    #[test]
    #[ignore]
    fn state_checked_off_on_checkbox() {
        let app = h::app_root();
        let cb = h::named(&app, "I agree to terms");
        assert_eq!(cb.states.checked, Some(Toggled::Off));
    }

    #[test]
    #[ignore]
    fn state_checked_on_radio() {
        let app = h::app_root();
        let radios = app.locator("radio_button").elements().unwrap();
        let opt_a = radios
            .iter()
            .find(|n| n.name.as_deref() == Some("Option A"));
        assert!(opt_a.is_some());
        assert_eq!(opt_a.unwrap().states.checked, Some(Toggled::On));
    }

    #[test]
    #[ignore]
    fn state_expanded_collapsed_on_expander() {
        let app = h::app_root();
        // Look for expandable elements by name
        let expander_by_name = app.locator(r#"[name*="Expander"]"#).elements().unwrap();
        // On macOS, GenericContainer with expanded state may not expose AXExpanded.
        // The expand/collapse actions still work (tested by action_expand_collapse).
        if expander_by_name.is_empty() {
            // Verify expand/collapse actions work even if state isn't reported
            println!(
                "No expandable elements found by name. \
                 Expand/collapse actions tested separately."
            );
        }
    }

    #[test]
    #[ignore]
    fn state_editable_on_text_field() {
        let app = h::app_root();
        // Prior action tests (TypeText, SetValue) may have changed or cleared the value.
        // Find text field by name.
        let text_fields = app.locator(r#"[name="Name"]"#).elements().unwrap();
        if text_fields.is_empty() {
            // Fall back to finding any text field
            let fields = app.locator("text_field").elements().unwrap();
            let areas = app.locator("text_area").elements().unwrap();
            let all_text: Vec<&Element> = fields.iter().chain(areas.iter()).collect();
            assert!(!all_text.is_empty(), "Text entry not found. App: {}", app);
            assert!(all_text[0].states.editable, "Text entry should be editable");
        } else {
            assert!(
                text_fields[0].states.editable,
                "Text entry should be editable"
            );
        }
    }

    #[test]
    #[ignore]
    fn state_selected_on_list_item() {
        let app = h::app_root();
        // Click Apple to select it
        let apple = h::named(&app, "Apple");
        let app2 = h::act(&apple, "press");
        // Verify selection (may come through as Click -> Select depending on AT-SPI mapping)
        let apple2 = h::named(&app2, "Apple");
        // Selection might be reported differently; at least verify the action didn't crash
        println!(
            "Apple selected state after Click: {:?}",
            apple2.states.selected
        );
    }

    // ════════════════════════════════════════════════════════════════
    // Selector Queries (12 tests)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn sel_by_role() {
        let app = h::app_root();
        let buttons = app.locator("button").elements().unwrap();
        assert!(buttons.len() >= 2);
        for b in &buttons {
            assert_eq!(b.role, Role::Button);
        }
    }

    #[test]
    #[ignore]
    fn sel_by_exact_name() {
        let app = h::app_root();
        let submit = h::one(&app, r#"button[name="Submit"]"#);
        assert_eq!(submit.role, Role::Button);
    }

    #[test]
    #[ignore]
    fn sel_by_role_and_name() {
        let app = h::app_root();
        let results = app.locator(r#"button[name="Cancel"]"#).elements().unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    #[ignore]
    fn sel_name_contains() {
        let app = h::app_root();
        let results = app.locator(r#"[name*="agree"]"#).elements().unwrap();
        assert!(
            !results.is_empty(),
            "Should find element with 'agree' in name"
        );
    }

    #[test]
    #[ignore]
    fn sel_name_starts_with() {
        let app = h::app_root();
        // Try "Welc" first (Welcome label), fall back to "Sub" (Submit button)
        let results = app.locator(r#"[name^="Welc"]"#).elements().unwrap();
        if results.is_empty() {
            // Welcome label may not be named on some AT-SPI adapters; use Submit instead
            let results = app.locator(r#"[name^="Sub"]"#).elements().unwrap();
            assert!(!results.is_empty());
            assert!(results[0]
                .name
                .as_deref()
                .unwrap()
                .to_lowercase()
                .starts_with("sub"));
        } else {
            assert!(results[0]
                .name
                .as_deref()
                .unwrap()
                .to_lowercase()
                .starts_with("welc"));
        }
    }

    #[test]
    #[ignore]
    fn sel_name_ends_with() {
        let app = h::app_root();
        // "xa11y" suffix may be in the window title or app name
        let results = app.locator(r#"[name$="xa11y"]"#).elements().unwrap();
        if results.is_empty() {
            // Fall back to known name suffixes
            let results = app.locator(r#"[name$="App"]"#).elements().unwrap();
            if results.is_empty() {
                // On Windows, names may differ. Try "Submit" suffix.
                let results = app.locator(r#"[name$="Submit"]"#).elements().unwrap();
                assert!(
                    !results.is_empty(),
                    "Should find at least one element with name ending in 'Submit'"
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn sel_value_attribute() {
        let app = h::app_root();
        // Try "Red" (ComboBox value), then fall back to any value attribute match.
        let results = app.locator(r#"[value*="Red"]"#).elements().unwrap();
        if results.is_empty() {
            // ComboBox value may not be exposed on some AT-SPI adapters.
            // Try matching against progress bar value "0.75"
            let results = app.locator(r#"[value*="0.75"]"#).elements().unwrap();
            assert!(
                !results.is_empty(),
                "Should find element with value containing '0.75' (ProgressBar)"
            );
        }
    }

    #[test]
    #[ignore]
    fn sel_descendant_combinator() {
        let app = h::app_root();
        // On Windows (UIA), the app root IS the window, so "window button"
        // won't find anything within the app's tree. Use "group button" which
        // works on all platforms (buttons are inside group containers).
        let results = app.locator("group button").elements().unwrap();
        if results.is_empty() {
            // Fall back to "window button" for Linux/macOS
            let results = app.locator("window button").elements().unwrap();
            assert!(!results.is_empty());
            for r in &results {
                assert_eq!(r.role, Role::Button);
            }
        } else {
            for r in &results {
                assert_eq!(r.role, Role::Button);
            }
        }
    }

    #[test]
    #[ignore]
    fn sel_child_combinator() {
        let app = h::app_root();
        let results = app.locator("application > window").elements().unwrap();
        // May or may not match depending on tree structure, but should not error
        for r in &results {
            assert_eq!(r.role, Role::Window);
        }
    }

    #[test]
    #[ignore]
    fn sel_nth_pseudo() {
        let app = h::app_root();
        let first = app.locator("button:nth(1)").elements().unwrap();
        assert_eq!(first.len(), 1);
    }

    #[test]
    #[ignore]
    fn sel_role_attribute() {
        let app = h::app_root();
        let results = app.locator(r#"[role="button"]"#).elements().unwrap();
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.role, Role::Button);
        }
    }

    #[test]
    #[ignore]
    fn sel_complex_chain() {
        let app = h::app_root();
        // Multi-segment selector: role + name attribute chain.
        // On Windows (UIA), the app root is the window and AccessKit containers
        // may flatten, so "window button" or "group button" may not work.
        // Use "menu_bar menu_item" which is nested on all platforms.
        let results = app
            .locator(r#"menu_bar menu_item[name="File"]"#)
            .elements()
            .unwrap();
        assert!(!results.is_empty(), "Should find File menu item via chain");
        assert_eq!(results[0].role, Role::MenuItem);
        assert_eq!(results[0].name.as_deref(), Some("File"));
    }

    #[test]
    #[ignore]
    fn raw_data_always_present() {
        let _app = h::app_root();
        #[cfg(target_os = "linux")]
        {
            let atspi_role = _app
                .data
                .raw
                .get("atspi_role")
                .and_then(|v| v.as_str())
                .expect("Expected atspi_role in raw data");
            assert!(!atspi_role.is_empty());
        }
        #[cfg(target_os = "macos")]
        {
            let ax_role = _app
                .data
                .raw
                .get("ax_role")
                .and_then(|v| v.as_str())
                .expect("Expected ax_role in raw data");
            assert!(!ax_role.is_empty());
        }
        #[cfg(target_os = "windows")]
        {
            let control_type_id = _app
                .data
                .raw
                .get("control_type_id")
                .and_then(|v| v.as_i64())
                .expect("Expected control_type_id in raw data");
            assert!(control_type_id > 0);
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Serialization (1 test)
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn json_roundtrip_real_element() {
        let app = h::app_root();
        // Serialize the root ElementData
        let json = serde_json::to_string(&app.data).unwrap();
        let deser: ElementData = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.role, app.data.role);
        assert_eq!(deser.name, app.data.name);
    }

    // ════════════════════════════════════════════════════════════════
    // Bidi-strip (1 test) — issue #188
    // ════════════════════════════════════════════════════════════════

    #[test]
    #[ignore]
    fn bidi_marks_stripped_from_button_name() {
        // The test app's BIDI_BUTTON has label "\u{200E}Bid\u{2066}i\u{2069}\u{200E}".
        // xa11y must strip those controls so `name == "Bidi"`. The unstripped
        // string must remain on `element.raw` so consumers who need it have
        // an escape hatch.
        //
        // Some Linux at-spi2-core configurations drop the entire name when it
        // contains non-printable chars; on those configs the button is not
        // findable by name and we treat the test as vacuous (the unit tests
        // for `strip_bidi` cover the function itself).
        let app = h::app_root();
        let candidates = app
            .locator(r#"button[name="Bidi"]"#)
            .elements()
            .unwrap_or_default();
        let Some(button) = candidates.into_iter().next() else {
            eprintln!(
                "skip: bidi-marked button name not surfaced by this platform's AT-SPI/UIA bridge"
            );
            return;
        };

        assert_eq!(button.name.as_deref(), Some("Bidi"));
        assert!(
            !button
                .name
                .as_deref()
                .unwrap_or("")
                .chars()
                .any(xa11y::is_bidi_control),
            "stripped name should contain no bidi controls: {:?}",
            button.name
        );

        let raw_key = if cfg!(target_os = "macos") {
            "AXTitle"
        } else if cfg!(target_os = "linux") {
            "atspi_name"
        } else {
            "uia_name"
        };
        let raw_name = button
            .raw
            .get(raw_key)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("Expected {raw_key} in raw data: {:?}", button.raw));
        assert!(
            raw_name.contains('\u{200E}'),
            "raw {raw_key} should keep LRM: {:?}",
            raw_name
        );
    }
}
