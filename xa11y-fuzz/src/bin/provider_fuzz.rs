//! Cross-platform accessibility provider fuzzer for xa11y.
//!
//! Exercises all code paths in the platform provider by randomly querying and
//! acting on a live xa11y-test-app via the Provider API. Uses a seeded PRNG
//! so crashes are reproducible: re-run with the same --seed to replay.
//!
//! Works on macOS (AXUIElement), Linux (AT-SPI2), and Windows (UIA).
//!
//! Usage: provider-fuzz [--seed N] [--iterations N] [--verbose]

fn main() {
    provider_fuzz::run();
}

mod provider_fuzz {
    use rand::prelude::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use xa11y::*;

    // ── CLI ──────────────────────────────────────────────────────────────────

    struct Args {
        seed: u64,
        iterations: u32,
        verbose: bool,
    }

    fn parse_args() -> Args {
        let mut args = std::env::args().skip(1);
        let mut seed = None;
        let mut iterations = 10_000u32;
        let mut verbose = false;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--seed" => {
                    seed = Some(
                        args.next()
                            .expect("--seed requires a value")
                            .parse::<u64>()
                            .expect("--seed must be a u64"),
                    );
                }
                "--iterations" | "-n" => {
                    iterations = args
                        .next()
                        .expect("--iterations requires a value")
                        .parse()
                        .expect("--iterations must be a u32");
                }
                "--verbose" | "-v" => verbose = true,
                "--help" | "-h" => {
                    eprintln!("Usage: macos-provider-fuzz [--seed N] [--iterations N] [--verbose]");
                    std::process::exit(0);
                }
                other => {
                    eprintln!("Unknown arg: {}", other);
                    std::process::exit(1);
                }
            }
        }

        let seed = seed.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        });

        Args {
            seed,
            iterations,
            verbose,
        }
    }

    // ── All Roles ────────────────────────────────────────────────────────────

    const ALL_ROLES: &[Role] = &[
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

    const ALL_ACTIONS: &[Action] = &[
        Action::Press,
        Action::Focus,
        Action::SetValue,
        Action::Toggle,
        Action::Expand,
        Action::Collapse,
        Action::Select,
        Action::ShowMenu,
        Action::ScrollIntoView,
        Action::Scroll,
        Action::Increment,
        Action::Decrement,
        Action::Blur,
        Action::SetTextSelection,
        Action::TypeText,
        Action::DragTo,
    ];

    // ── Selector Generation ──────────────────────────────────────────────────

    const KNOWN_SELECTORS: &[&str] = &[
        "button",
        "check_box",
        "radio_button",
        "slider",
        "text_field",
        "combo_box",
        "list_item",
        "menu_item",
        "tab",
        "table_cell",
        "table_row",
        "image",
        "link",
        "tree_item",
        "dialog",
        "alert",
        "heading",
        "scroll_bar",
        "separator",
        "progress_bar",
        "group",
        "window",
        "application",
        "toolbar",
        "menu_bar",
        "tab_group",
        "table",
        "list",
        "menu",
        "static_text",
        "split_group",
        // Attribute selectors
        "button[name=\"Submit\"]",
        "button[name*=\"mit\"]",
        "button[name^=\"Sub\"]",
        "button[name$=\"mit\"]",
        "[name=\"Volume\"]",
        "[name*=\"test\"]",
        // Combinators
        "group > button",
        "window button",
        "group > check_box",
        "list > list_item",
        "table > table_row",
        "table_row > table_cell",
        "group > slider",
        "window static_text",
        // Pseudo-classes
        "button:nth(1)",
        "button:nth(2)",
        "list_item:nth(1)",
        "tab:nth(1)",
        "static_text:nth(3)",
        // Complex
        "group > button:nth(1)",
        "window group > button[name=\"Submit\"]",
    ];

    fn random_selector(rng: &mut StdRng) -> String {
        let kind: u8 = rng.gen_range(0..10);
        match kind {
            // 60% known-valid selectors
            0..=5 => KNOWN_SELECTORS[rng.gen_range(0..KNOWN_SELECTORS.len())].to_string(),
            // 10% random role
            6 => ALL_ROLES[rng.gen_range(0..ALL_ROLES.len())]
                .to_snake_case()
                .to_string(),
            // 10% random attribute filter
            7 => {
                let role = ALL_ROLES[rng.gen_range(0..ALL_ROLES.len())].to_snake_case();
                let attrs = ["name", "value", "description"];
                let attr = attrs[rng.gen_range(0..attrs.len())];
                let ops = ["=", "*=", "^=", "$="];
                let op = ops[rng.gen_range(0..ops.len())];
                let values = ["Submit", "Cancel", "test", "", "Volume", "Alice", "x"];
                let val = values[rng.gen_range(0..values.len())];
                format!("{}[{}{}\"{}\"", role, attr, op, val) + "]"
            }
            // 10% garbage
            8 => {
                let len = rng.gen_range(0..30);
                (0..len)
                    .map(|_| rng.gen_range(b' '..=b'~') as char)
                    .collect()
            }
            // 10% empty or whitespace
            _ => {
                if rng.gen_bool(0.5) {
                    String::new()
                } else {
                    " ".to_string()
                }
            }
        }
    }

    // ── QueryOptions Generation ──────────────────────────────────────────────

    fn random_query_options(rng: &mut StdRng) -> QueryOptions {
        let max_depth = match rng.gen_range(0u8..10) {
            0..=3 => None,
            4 => Some(0),
            5 => Some(1),
            6..=7 => Some(rng.gen_range(2..6)),
            _ => Some(rng.gen_range(10..100)),
        };

        let max_elements = match rng.gen_range(0u8..10) {
            0..=5 => None,
            6 => Some(1),
            7 => Some(rng.gen_range(2..10)),
            8 => Some(rng.gen_range(10..50)),
            _ => Some(rng.gen_range(50..500)),
        };

        let visible_only = rng.gen_bool(0.3);

        let roles = if rng.gen_bool(0.2) {
            let count = rng.gen_range(1..=5);
            let mut r = Vec::with_capacity(count);
            for _ in 0..count {
                r.push(ALL_ROLES[rng.gen_range(0..ALL_ROLES.len())]);
            }
            Some(r)
        } else {
            None
        };

        let include_raw = rng.gen_bool(0.5);

        QueryOptions {
            max_depth,
            max_elements,
            visible_only,
            roles,
            include_raw,
        }
    }

    // ── ActionData Generation ────────────────────────────────────────────────

    fn random_action_data(rng: &mut StdRng, action: Action, _node: &Node) -> Option<ActionData> {
        match action {
            Action::SetValue => {
                let kind: u8 = rng.gen_range(0..10);
                match kind {
                    0..=3 => {
                        let texts = [
                            "hello",
                            "",
                            "a very long string that goes on and on and on",
                            "unicode: café ñ 日本語",
                            " ",
                            "12345",
                            "special <>&\"'",
                        ];
                        Some(ActionData::Value(
                            texts[rng.gen_range(0..texts.len())].to_string(),
                        ))
                    }
                    4..=7 => {
                        let values = [0.0, 50.0, 100.0, -1.0, 999.0, 0.5, 42.0];
                        Some(ActionData::NumericValue(
                            values[rng.gen_range(0..values.len())],
                        ))
                    }
                    _ => None,
                }
            }
            Action::TypeText => {
                let texts = ["a", "hello", "test 123", "ñ", " ", ""];
                Some(ActionData::Value(
                    texts[rng.gen_range(0..texts.len())].to_string(),
                ))
            }
            Action::Scroll => {
                let directions = [
                    ScrollDirection::Up,
                    ScrollDirection::Down,
                    ScrollDirection::Left,
                    ScrollDirection::Right,
                ];
                let amounts = [1.0, 3.0, 10.0, 0.5];
                Some(ActionData::ScrollAmount {
                    direction: directions[rng.gen_range(0..directions.len())],
                    amount: amounts[rng.gen_range(0..amounts.len())],
                })
            }
            Action::SetTextSelection => Some(ActionData::TextSelection {
                start: rng.gen_range(0..10),
                end: rng.gen_range(0..20),
            }),
            Action::DragTo => Some(ActionData::Point {
                x: rng.gen_range(0.0..500.0),
                y: rng.gen_range(0.0..500.0),
            }),
            _ => None,
        }
    }

    // ── Fuzzer State ─────────────────────────────────────────────────────────

    struct FuzzState {
        provider: Box<dyn Provider>,
        rng: StdRng,
        verbose: bool,
        tree_raw: Option<Tree>,
        tree_no_raw: Option<Tree>,
        test_app_pid: u32,
        ops: u64,
        errors: u64,
    }

    impl FuzzState {
        fn log(&self, msg: &str) {
            if self.verbose {
                eprintln!("  [fuzz] {}", msg);
            }
        }

        fn ensure_tree_raw(&mut self) {
            if self.tree_raw.is_none() {
                let opts = QueryOptions {
                    include_raw: true,
                    ..QueryOptions::default()
                };
                match self
                    .provider
                    .get_app_tree(&AppTarget::ByName("xa11y".to_string()), &opts)
                {
                    Ok(tree) => self.tree_raw = Some(tree),
                    Err(e) => self.log(&format!("ensure_tree_raw failed: {}", e)),
                }
            }
        }

        fn ensure_tree_no_raw(&mut self) {
            if self.tree_no_raw.is_none() {
                match self.provider.get_app_tree(
                    &AppTarget::ByName("xa11y".to_string()),
                    &QueryOptions::default(),
                ) {
                    Ok(tree) => self.tree_no_raw = Some(tree),
                    Err(e) => self.log(&format!("ensure_tree_no_raw failed: {}", e)),
                }
            }
        }
    }

    // ── Operations ───────────────────────────────────────────────────────────

    fn op_get_tree_by_name(state: &mut FuzzState) {
        let opts = random_query_options(&mut state.rng);
        state.log(&format!("get_app_tree(ByName, {:?})", opts));
        match state
            .provider
            .get_app_tree(&AppTarget::ByName("xa11y".to_string()), &opts)
        {
            Ok(tree) => {
                inspect_tree(&tree, &mut state.rng);
                if opts.include_raw {
                    state.tree_raw = Some(tree);
                } else {
                    state.tree_no_raw = Some(tree);
                }
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_get_tree_by_pid(state: &mut FuzzState) {
        let opts = random_query_options(&mut state.rng);
        state.log(&format!(
            "get_app_tree(ByPid({}), {:?})",
            state.test_app_pid, opts
        ));
        match state
            .provider
            .get_app_tree(&AppTarget::ByPid(state.test_app_pid), &opts)
        {
            Ok(tree) => {
                inspect_tree(&tree, &mut state.rng);
                if opts.include_raw {
                    state.tree_raw = Some(tree);
                }
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_get_tree_by_name_not_found(state: &mut FuzzState) {
        state.log("get_app_tree(ByName(\"nonexistent_app_XYZ\"))");
        let result = state.provider.get_app_tree(
            &AppTarget::ByName("nonexistent_app_XYZ_999".to_string()),
            &QueryOptions::default(),
        );
        assert!(result.is_err(), "Expected AppNotFound for bogus app name");
        state.errors += 1;
    }

    fn op_get_tree_by_pid_not_found(state: &mut FuzzState) {
        state.log("get_app_tree(ByPid(99999))");
        let result = state
            .provider
            .get_app_tree(&AppTarget::ByPid(99999), &QueryOptions::default());
        match result {
            Ok(tree) => inspect_tree(&tree, &mut state.rng),
            Err(_) => state.errors += 1,
        }
    }

    fn op_get_tree_by_window(state: &mut FuzzState) {
        state.log("get_app_tree(ByWindow) -> expected error");
        let result = state.provider.get_app_tree(
            &AppTarget::ByWindow(WindowHandle::MacOS(0)),
            &QueryOptions::default(),
        );
        assert!(result.is_err(), "Expected ByWindow to fail on macOS");
        state.errors += 1;
    }

    fn op_get_all_apps(state: &mut FuzzState) {
        let mut opts = random_query_options(&mut state.rng);
        opts.max_depth = Some(opts.max_depth.map_or(3, |d| d.min(3)));
        opts.max_elements = Some(opts.max_elements.map_or(100, |n| n.min(100)));
        state.log(&format!("get_all_apps({:?})", opts));
        match state.provider.get_all_apps(&opts) {
            Ok(tree) => {
                state.log(&format!("  -> {} nodes", tree.len()));
                let _ = tree.root();
                let _ = tree.len();
                let _ = tree.is_empty();
                let selectors = ["button", "window", "application"];
                for sel in &selectors {
                    let _ = tree.query(sel);
                }
            }
            Err(e) => {
                state.log(&format!("  -> error: {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_check_permissions(state: &mut FuzzState) {
        state.log("check_permissions()");
        let status = state.provider.check_permissions().unwrap();
        match status {
            PermissionStatus::Granted => {}
            PermissionStatus::Denied { instructions } => {
                panic!(
                    "Fuzzer requires accessibility permissions: {}",
                    instructions
                );
            }
        }
    }

    fn op_list_apps(state: &mut FuzzState) {
        state.log("list_apps()");
        let apps = state.provider.list_apps().unwrap();
        assert!(!apps.is_empty(), "list_apps returned empty");
        let has_test_app = apps.iter().any(|a| a.name.contains("xa11y"));
        state.log(&format!(
            "  -> {} apps, test_app_present={}",
            apps.len(),
            has_test_app
        ));
    }

    fn op_action_on_node(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        let node_count = tree.len();
        if node_count == 0 {
            return;
        }

        // Pick a random node
        let node_idx = state.rng.gen_range(0..node_count) as u32;
        let node = match tree.get(node_idx) {
            Some(n) => n,
            None => return,
        };

        // Pick action: 80% from node's supported actions, 20% random
        let action = if !node.actions.is_empty() && state.rng.gen_bool(0.8) {
            node.actions[state.rng.gen_range(0..node.actions.len())]
        } else {
            ALL_ACTIONS[state.rng.gen_range(0..ALL_ACTIONS.len())]
        };

        let data = random_action_data(&mut state.rng, action, node);
        state.log(&format!(
            "perform_action(node={}, role={:?}, action={:?}, data={:?})",
            node_idx, node.role, action, data
        ));

        match state.provider.perform_action(tree, node, action, data) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                state.tree_raw = None;
                state.tree_no_raw = None;
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_action_without_raw(state: &mut FuzzState) {
        state.ensure_tree_no_raw();
        let tree = match &state.tree_no_raw {
            Some(t) => t,
            None => return,
        };

        if tree.is_empty() {
            return;
        }

        let node_idx = state.rng.gen_range(0..tree.len()) as u32;
        let node = match tree.get(node_idx) {
            Some(n) => n,
            None => return,
        };
        state.log(&format!(
            "perform_action without include_raw, node={}",
            node_idx
        ));

        let result = state
            .provider
            .perform_action(tree, node, Action::Press, None);
        match result {
            Err(_) => state.errors += 1,
            Ok(()) => {
                state.tree_raw = None;
                state.tree_no_raw = None;
            }
        }
    }

    fn op_action_invalid_node(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        // Try performing an action via a selector that won't match
        state.log("perform via non-matching selector");
        let result = tree.perform(
            &*state.provider,
            "nonexistent_role_xyz",
            Action::Press,
            None,
        );
        // May get InvalidSelector or SelectorNotMatched
        assert!(result.is_err(), "Expected error for non-matching selector");
        state.errors += 1;
    }

    fn op_query_tree(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        let selector = random_selector(&mut state.rng);
        state.log(&format!("tree.query(\"{}\")", selector));
        match tree.query(&selector) {
            Ok(results) => {
                state.log(&format!("  -> {} matches", results.len()));
                for node in &results {
                    let _ = &node.name;
                    let _ = &node.value;
                    let _ = &node.role;
                    let _ = &node.states;
                    let _ = &node.bounds;
                    let _ = &node.bounds_normalized;
                    let _ = &node.actions;
                    let _ = &node.raw;
                }
            }
            Err(e) => {
                state.log(&format!("  -> parse error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_find_by_role(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        let role = ALL_ROLES[state.rng.gen_range(0..ALL_ROLES.len())];
        state.log(&format!("tree.find_by_role({:?})", role));
        let results = tree.find_by_role(role);
        state.log(&format!("  -> {} matches", results.len()));
    }

    fn op_find_by_name(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        let names = [
            "Submit",
            "Cancel",
            "Volume",
            "Alice",
            "xa11y",
            "",
            "nonexistent",
            "Name",
            "Option A",
            "Apple",
            "quit",
            "SUBMIT",
            "test",
        ];
        let name = names[state.rng.gen_range(0..names.len())];
        state.log(&format!("tree.find_by_name(\"{}\")", name));
        let results = tree.find_by_name(name);
        state.log(&format!("  -> {} matches", results.len()));
    }

    fn op_tree_dump(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        state.log("tree.dump()");
        let dump = tree.dump();
        assert!(!dump.is_empty(), "dump() should produce output");
    }

    fn op_tree_subtree(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        if tree.is_empty() {
            return;
        }

        let node_idx = state.rng.gen_range(0..tree.len()) as u32;
        let node = match tree.get(node_idx) {
            Some(n) => n,
            None => return,
        };
        state.log(&format!("tree.subtree({})", node_idx));
        let sub = tree.subtree(node);
        assert!(
            !sub.is_empty(),
            "subtree should not be empty for valid node"
        );
    }

    fn op_tree_children(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        if tree.is_empty() {
            return;
        }

        let node_idx = state.rng.gen_range(0..tree.len()) as u32;
        let node = match tree.get(node_idx) {
            Some(n) => n,
            None => return,
        };
        state.log(&format!("tree.children({})", node_idx));
        let children = tree.children(node);
        let _ = children.len();
    }

    fn op_tree_iterate(state: &mut FuzzState) {
        state.ensure_tree_raw();
        let tree = match &state.tree_raw {
            Some(t) => t,
            None => return,
        };

        state.log("tree.iter() — full traversal");
        let count = tree.iter().count();
        assert_eq!(count, tree.len(), "iter count should match len");

        for node in tree.iter() {
            let _ = &node.role;
            let _ = &node.name;
            let _ = &node.value;
            let _ = &node.description;
            let _ = &node.bounds;
            let _ = &node.bounds_normalized;
            let _ = &node.actions;
            let _ = &node.states;
            let _ = &node.depth;
            let _ = &node.numeric_value;
            let _ = &node.min_value;
            let _ = &node.max_value;
            let _ = &node.raw;
            let _ = &node.stable_id;
        }
    }

    // ── Tree Inspection Helper ───────────────────────────────────────────────

    fn inspect_tree(tree: &Tree, rng: &mut StdRng) {
        let _ = tree.len();
        let _ = tree.is_empty();
        let _ = tree.root();

        let inspection_count = rng.gen_range(1..=5);
        for _ in 0..inspection_count {
            match rng.gen_range(0u8..6) {
                0 => {
                    let _ = tree.dump();
                }
                1 => {
                    if !tree.is_empty() {
                        let idx = rng.gen_range(0..tree.len()) as u32;
                        if let Some(node) = tree.get(idx) {
                            let _ = tree.children(node);
                        }
                    }
                }
                2 => {
                    let role = ALL_ROLES[rng.gen_range(0..ALL_ROLES.len())];
                    let _ = tree.find_by_role(role);
                }
                3 => {
                    let _ = tree.find_by_name("test");
                }
                4 => {
                    let _ = tree.query("button");
                }
                5 => {
                    if !tree.is_empty() {
                        let idx = rng.gen_range(0..tree.len()) as u32;
                        if let Some(node) = tree.get(idx) {
                            let _ = tree.subtree(node);
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    // ── Main Loop ────────────────────────────────────────────────────────────

    pub fn run() {
        let args = parse_args();

        eprintln!("=== xa11y Provider Fuzzer ===");
        eprintln!("Seed:       {}", args.seed);
        eprintln!("Iterations: {}", args.iterations);
        eprintln!();

        let provider = create_provider().expect("Failed to create provider");

        match provider.check_permissions().unwrap() {
            PermissionStatus::Granted => eprintln!("Permissions: granted"),
            PermissionStatus::Denied { instructions } => {
                eprintln!("ERROR: {}", instructions);
                std::process::exit(1);
            }
        }

        let mut test_app_pid = 0u32;
        for attempt in 0..10 {
            if let Ok(apps) = provider.list_apps() {
                if let Some(app) = apps.iter().find(|a| a.name.contains("xa11y")) {
                    test_app_pid = app.pid;
                    eprintln!("Test app:   {} (PID {})", app.name, app.pid);
                    break;
                }
            }
            if attempt < 9 {
                eprintln!("Waiting for xa11y-test-app... (attempt {})", attempt + 1);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        if test_app_pid == 0 {
            eprintln!("ERROR: xa11y-test-app not found. Launch it first.");
            std::process::exit(1);
        }

        eprintln!();

        let mut state = FuzzState {
            provider,
            rng: StdRng::seed_from_u64(args.seed),
            verbose: args.verbose,
            tree_raw: None,
            tree_no_raw: None,
            test_app_pid,
            ops: 0,
            errors: 0,
        };

        type OpFn = fn(&mut FuzzState);
        let ops: Vec<(u32, &str, OpFn)> = vec![
            (15, "get_tree_by_name", op_get_tree_by_name as OpFn),
            (8, "get_tree_by_pid", op_get_tree_by_pid),
            (
                2,
                "get_tree_by_name_not_found",
                op_get_tree_by_name_not_found,
            ),
            (1, "get_tree_by_pid_not_found", op_get_tree_by_pid_not_found),
            (1, "get_tree_by_window", op_get_tree_by_window),
            (1, "get_all_apps", op_get_all_apps),
            (1, "check_permissions", op_check_permissions),
            (2, "list_apps", op_list_apps),
            (20, "action_on_node", op_action_on_node),
            (3, "action_without_raw", op_action_without_raw),
            (2, "action_invalid_node", op_action_invalid_node),
            (15, "query_tree", op_query_tree),
            (5, "find_by_role", op_find_by_role),
            (5, "find_by_name", op_find_by_name),
            (3, "tree_dump", op_tree_dump),
            (3, "tree_subtree", op_tree_subtree),
            (3, "tree_children", op_tree_children),
            (3, "tree_iterate", op_tree_iterate),
        ];

        let total_weight: u32 = ops.iter().map(|(w, _, _)| *w).sum();

        for i in 0..args.iterations {
            let mut roll = state.rng.gen_range(0..total_weight);
            let mut chosen_name = "";
            let mut chosen_fn: OpFn = op_check_permissions;
            for (weight, name, f) in &ops {
                if roll < *weight {
                    chosen_name = name;
                    chosen_fn = *f;
                    break;
                }
                roll -= weight;
            }

            if args.verbose {
                eprintln!("[{}/{}] {}", i + 1, args.iterations, chosen_name);
            }

            chosen_fn(&mut state);
            state.ops += 1;

            if (i + 1) % 1000 == 0 && !args.verbose {
                eprintln!(
                    "  [{}/{}] ops={}, errors={}",
                    i + 1,
                    args.iterations,
                    state.ops,
                    state.errors,
                );
            }
        }

        eprintln!();
        eprintln!("=== Fuzzing Complete ===");
        eprintln!("Seed:       {}", args.seed);
        eprintln!("Operations: {}", state.ops);
        eprintln!("Errors:     {} (expected)", state.errors);
        eprintln!("OK — no crashes found.");
    }
}
