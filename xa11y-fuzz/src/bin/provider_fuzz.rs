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
        Action::ScrollDown,
        Action::ScrollRight,
        Action::Increment,
        Action::Decrement,
        Action::Blur,
        Action::SetTextSelection,
        Action::TypeText,
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
        let kind: u8 = rng.random_range(0..10);
        match kind {
            // 60% known-valid selectors
            0..=5 => KNOWN_SELECTORS[rng.random_range(0..KNOWN_SELECTORS.len())].to_string(),
            // 10% random role
            6 => ALL_ROLES[rng.random_range(0..ALL_ROLES.len())]
                .to_snake_case()
                .to_string(),
            // 10% random attribute filter
            7 => {
                let role = ALL_ROLES[rng.random_range(0..ALL_ROLES.len())].to_snake_case();
                let attrs = ["name", "value", "description"];
                let attr = attrs[rng.random_range(0..attrs.len())];
                let ops = ["=", "*=", "^=", "$="];
                let op = ops[rng.random_range(0..ops.len())];
                let values = ["Submit", "Cancel", "test", "", "Volume", "Alice", "x"];
                let val = values[rng.random_range(0..values.len())];
                format!("{}[{}{}\"{}\"", role, attr, op, val) + "]"
            }
            // 10% garbage
            8 => {
                let len = rng.random_range(0..30);
                (0..len)
                    .map(|_| rng.random_range(b' '..=b'~') as char)
                    .collect()
            }
            // 10% empty or whitespace
            _ => {
                if rng.random_bool(0.5) {
                    String::new()
                } else {
                    " ".to_string()
                }
            }
        }
    }

    // ── ActionData Generation ────────────────────────────────────────────────

    fn random_action_data(
        rng: &mut StdRng,
        action: Action,
        _element: &ElementData,
    ) -> Option<ActionData> {
        match action {
            Action::SetValue => {
                let kind: u8 = rng.random_range(0..10);
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
                            texts[rng.random_range(0..texts.len())].to_string(),
                        ))
                    }
                    4..=7 => {
                        let values = [0.0, 50.0, 100.0, -1.0, 999.0, 0.5, 42.0];
                        Some(ActionData::NumericValue(
                            values[rng.random_range(0..values.len())],
                        ))
                    }
                    _ => None,
                }
            }
            Action::TypeText => {
                let texts = ["a", "hello", "test 123", "ñ", " ", ""];
                Some(ActionData::Value(
                    texts[rng.random_range(0..texts.len())].to_string(),
                ))
            }
            Action::ScrollDown | Action::ScrollRight => {
                let amounts = [1.0, 3.0, 10.0, 0.5, -1.0, -3.0];
                Some(ActionData::ScrollAmount(
                    amounts[rng.random_range(0..amounts.len())],
                ))
            }
            Action::SetTextSelection => Some(ActionData::TextSelection {
                start: rng.random_range(0..10),
                end: rng.random_range(0..20),
            }),
            _ => None,
        }
    }

    // ── Fuzzer State ─────────────────────────────────────────────────────────

    struct FuzzState {
        provider: std::sync::Arc<dyn Provider>,
        rng: StdRng,
        verbose: bool,
        root: Option<Element>,
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

        fn ensure_root(&mut self) {
            if self.root.is_none() {
                match self.provider.get_elements(self.test_app_pid) {
                    Ok(root) => self.root = Some(root),
                    Err(e) => self.log(&format!("ensure_root failed: {}", e)),
                }
            }
        }
    }

    // ── Operations ───────────────────────────────────────────────────────────

    fn op_get_elements(state: &mut FuzzState) {
        state.log(&format!("get_elements({})", state.test_app_pid));
        match state.provider.get_elements(state.test_app_pid) {
            Ok(root) => {
                inspect_root(&root, &mut state.rng);
                state.root = Some(root);
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_resolve_pid_not_found(state: &mut FuzzState) {
        state.log("resolve_pid_by_name(\"nonexistent_app_XYZ_999\")");
        let result = state
            .provider
            .resolve_pid_by_name("nonexistent_app_XYZ_999");
        assert!(result.is_err(), "Expected AppNotFound for bogus app name");
        state.errors += 1;
    }

    fn op_get_elements_not_found(state: &mut FuzzState) {
        state.log("get_elements(99999)");
        let result = state.provider.get_elements(99999);
        match result {
            Ok(root) => inspect_root(&root, &mut state.rng),
            Err(_) => state.errors += 1,
        }
    }

    fn op_get_apps(state: &mut FuzzState) {
        state.log("get_apps()");
        match state.provider.get_apps() {
            Ok(root) => {
                let subtree = root.subtree();
                state.log(&format!("  -> {} elements", subtree.len()));
                let _ = &root.role;
                let _ = &root.name;
                let selectors = ["button", "window", "application"];
                for sel in &selectors {
                    let _ = root.query_selector(sel);
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

    fn op_action_on_element(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        let elements = root.subtree();
        if elements.is_empty() {
            return;
        }

        // Pick a random element
        let element_idx = state.rng.random_range(0..elements.len());
        let target = &elements[element_idx];

        // Pick action: 80% from element's supported actions, 20% random
        let action = if !target.actions.is_empty() && state.rng.random_bool(0.8) {
            target.actions[state.rng.random_range(0..target.actions.len())]
        } else {
            ALL_ACTIONS[state.rng.random_range(0..ALL_ACTIONS.len())]
        };

        let data = random_action_data(&mut state.rng, action, target);
        state.log(&format!(
            "perform_action(element={}, role={:?}, action={:?}, data={:?})",
            element_idx, target.role, action, data
        ));

        match state.provider.perform_action(target, action, data) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                state.root = None;
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_action_press(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        let elements = root.subtree();
        if elements.is_empty() {
            return;
        }

        let element_idx = state.rng.random_range(0..elements.len());
        let target = &elements[element_idx];
        state.log(&format!("perform_action press, element={}", element_idx));

        let result = state.provider.perform_action(target, Action::Press, None);
        match result {
            Err(_) => state.errors += 1,
            Ok(()) => {
                state.root = None;
            }
        }
    }

    fn op_query_tree(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        let selector = random_selector(&mut state.rng);
        state.log(&format!("root.query_selector(\"{}\")", selector));
        match root.query_selector(&selector) {
            Ok(results) => {
                state.log(&format!("  -> {} matches", results.len()));
                for element in &results {
                    let _ = &element.name;
                    let _ = &element.value;
                    let _ = &element.role;
                    let _ = &element.states;
                    let _ = &element.bounds;
                    let _ = &element.actions;
                    let _ = &element.raw;
                }
            }
            Err(e) => {
                state.log(&format!("  -> parse error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_tree_dump(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        state.log("root.to_string()");
        let display = root.to_string();
        assert!(!display.is_empty(), "Display should produce output");
    }

    fn op_tree_subtree(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        let elements = root.subtree();
        if elements.is_empty() {
            return;
        }

        let element_idx = state.rng.random_range(0..elements.len());
        let target = &elements[element_idx];
        state.log(&format!("element.subtree({})", element_idx));
        let sub = target.subtree();
        assert!(
            !sub.is_empty(),
            "subtree should not be empty for valid element"
        );
    }

    fn op_tree_children(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        let elements = root.subtree();
        if elements.is_empty() {
            return;
        }

        let element_idx = state.rng.random_range(0..elements.len());
        let target = &elements[element_idx];
        state.log(&format!("element.children({})", element_idx));
        let children = target.children();
        let _ = children.len();
    }

    fn op_tree_iterate(state: &mut FuzzState) {
        state.ensure_root();
        let root = match &state.root {
            Some(r) => r,
            None => return,
        };

        state.log("root.subtree() — full traversal");
        let elements = root.subtree();

        for element in &elements {
            let _ = &element.role;
            let _ = &element.name;
            let _ = &element.value;
            let _ = &element.description;
            let _ = &element.bounds;
            let _ = &element.actions;
            let _ = &element.states;
            let _ = &element.numeric_value;
            let _ = &element.min_value;
            let _ = &element.max_value;
            let _ = &element.raw;
            let _ = &element.stable_id;
        }
    }

    // ── Tree Inspection Helper ───────────────────────────────────────────────

    fn inspect_root(root: &Element, rng: &mut StdRng) {
        let subtree = root.subtree();
        let _ = subtree.len();
        let _ = &root.role;
        let _ = &root.name;

        let inspection_count = rng.random_range(1..=5);
        for _ in 0..inspection_count {
            match rng.random_range(0u8..4) {
                0 => {
                    let _ = root.to_string();
                }
                1 => {
                    if !subtree.is_empty() {
                        let idx = rng.random_range(0..subtree.len());
                        let _ = subtree[idx].children();
                    }
                }
                2 => {
                    let _ = root.query_selector("button");
                }
                3 => {
                    if !subtree.is_empty() {
                        let idx = rng.random_range(0..subtree.len());
                        let _ = subtree[idx].subtree();
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
            if let Ok(root) = provider.get_apps() {
                if let Some(app) = root
                    .subtree()
                    .into_iter()
                    .find(|n| n.name.as_deref().is_some_and(|name| name.contains("xa11y")))
                {
                    test_app_pid = app.pid.unwrap_or(0);
                    eprintln!(
                        "Test app:   {} (PID {})",
                        app.name.as_deref().unwrap_or("?"),
                        test_app_pid
                    );
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
            root: None,
            test_app_pid,
            ops: 0,
            errors: 0,
        };

        type OpFn = fn(&mut FuzzState);
        let ops: Vec<(u32, &str, OpFn)> = vec![
            (20, "get_elements", op_get_elements as OpFn),
            (2, "resolve_pid_not_found", op_resolve_pid_not_found),
            (1, "get_elements_not_found", op_get_elements_not_found),
            (1, "get_apps", op_get_apps),
            (1, "check_permissions", op_check_permissions),
            (20, "action_on_element", op_action_on_element),
            (3, "action_press", op_action_press),
            (15, "query_tree", op_query_tree),
            (3, "tree_display", op_tree_dump),
            (3, "tree_subtree", op_tree_subtree),
            (3, "tree_children", op_tree_children),
            (3, "tree_iterate", op_tree_iterate),
        ];

        let total_weight: u32 = ops.iter().map(|(w, _, _)| *w).sum();

        for i in 0..args.iterations {
            let mut roll = state.rng.random_range(0..total_weight);
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
