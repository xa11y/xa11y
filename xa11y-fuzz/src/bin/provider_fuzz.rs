//! Cross-platform accessibility provider fuzzer for xa11y.
//!
//! Exercises all code paths in the platform provider by randomly querying and
//! acting on a live xa11y-test-app via the Provider API. Uses a seeded PRNG
//! so crashes are reproducible: re-run with the same --seed to replay.
//!
//! Usage: provider-fuzz [--seed N] [--iterations N] [--verbose]

fn main() {
    provider_fuzz::run();
}

mod provider_fuzz {
    use rand::prelude::*;
    use std::sync::Arc;
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
                    eprintln!("Usage: provider-fuzz [--seed N] [--iterations N] [--verbose]");
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

    // ── All Actions ─────────────────────────────────────────────────────────

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

    // ── Selector Generation ─────────────────────────────────────────────────

    const KNOWN_SELECTORS: &[&str] = &[
        "button",
        "check_box",
        "slider",
        "text_field",
        "list_item",
        "tab",
        "window",
        "application",
        "toolbar",
        "group",
        "static_text",
        r#"button[name="Submit"]"#,
        r#"button[name*="mit"]"#,
        r#"[name="Volume"]"#,
        "group > button",
        "window button",
        "list > list_item",
        "button:nth(1)",
        "button:nth(2)",
    ];

    fn random_selector(rng: &mut StdRng) -> String {
        let kind: u8 = rng.random_range(0..10);
        match kind {
            0..=7 => KNOWN_SELECTORS[rng.random_range(0..KNOWN_SELECTORS.len())].to_string(),
            8 => {
                let len = rng.random_range(0..30);
                (0..len)
                    .map(|_| rng.random_range(b' '..=b'~') as char)
                    .collect()
            }
            _ => String::new(),
        }
    }

    // ── ActionData Generation ───────────────────────────────────────────────

    fn random_action_data(rng: &mut StdRng, action: Action) -> Option<ActionData> {
        match action {
            Action::SetValue => {
                if rng.random_bool(0.5) {
                    Some(ActionData::Value("hello".to_string()))
                } else {
                    Some(ActionData::NumericValue(rng.random_range(0.0..100.0)))
                }
            }
            Action::TypeText => Some(ActionData::Value("test".to_string())),
            Action::ScrollDown | Action::ScrollRight => {
                Some(ActionData::ScrollAmount(rng.random_range(-3.0..3.0)))
            }
            Action::SetTextSelection => Some(ActionData::TextSelection {
                start: rng.random_range(0..10),
                end: rng.random_range(0..20),
            }),
            _ => None,
        }
    }

    // ── Fuzzer State ────────────────────────────────────────────────────────

    struct FuzzState {
        provider: Arc<dyn Provider>,
        rng: StdRng,
        verbose: bool,
        app_element: Option<ElementData>,
        ops: u64,
        errors: u64,
    }

    impl FuzzState {
        fn log(&self, msg: &str) {
            if self.verbose {
                eprintln!("  [fuzz] {}", msg);
            }
        }

        fn ensure_app(&mut self) {
            if self.app_element.is_some() {
                return;
            }
            let sel = Selector::parse(r#"application[name*="xa11y"]"#).unwrap();
            match self.provider.find_elements(None, &sel, Some(1), None) {
                Ok(matches) if !matches.is_empty() => {
                    self.app_element = Some(matches.into_iter().next().unwrap());
                }
                Ok(_) => self.log("ensure_app: no match"),
                Err(e) => self.log(&format!("ensure_app failed: {}", e)),
            }
        }
    }

    // ── Operations ──────────────────────────────────────────────────────────

    fn op_get_children(state: &mut FuzzState) {
        state.ensure_app();
        let app = match &state.app_element {
            Some(a) => a.clone(),
            None => return,
        };
        state.log("get_children(app)");
        match state.provider.get_children(Some(&app)) {
            Ok(children) => {
                state.log(&format!("  -> {} children", children.len()));
                // Drill into a random child
                if !children.is_empty() {
                    let idx = state.rng.random_range(0..children.len());
                    let _ = state.provider.get_children(Some(&children[idx]));
                }
            }
            Err(e) => {
                state.log(&format!("  -> error: {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_get_parent(state: &mut FuzzState) {
        state.ensure_app();
        let app = match &state.app_element {
            Some(a) => a.clone(),
            None => return,
        };
        state.log("get_parent(app)");
        match state.provider.get_parent(&app) {
            Ok(parent) => {
                state.log(&format!(
                    "  -> parent: {:?}",
                    parent.as_ref().map(|p| &p.role)
                ));
            }
            Err(e) => {
                state.log(&format!("  -> error: {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_find_elements(state: &mut FuzzState) {
        state.ensure_app();
        let app = state.app_element.clone();
        let selector_str = random_selector(&mut state.rng);
        state.log(&format!("find_elements(\"{}\")", selector_str));
        let selector = match Selector::parse(&selector_str) {
            Ok(s) => s,
            Err(_) => {
                state.errors += 1;
                return;
            }
        };
        match state
            .provider
            .find_elements(app.as_ref(), &selector, Some(10), None)
        {
            Ok(results) => {
                state.log(&format!("  -> {} matches", results.len()));
                for el in &results {
                    let _ = &el.name;
                    let _ = &el.role;
                    let _ = &el.states;
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
        state.ensure_app();
        let app = match &state.app_element {
            Some(a) => a.clone(),
            None => return,
        };

        // Find some elements to act on
        let sel = Selector::parse("button").unwrap();
        let elements = match state
            .provider
            .find_elements(Some(&app), &sel, Some(20), None)
        {
            Ok(e) => e,
            Err(_) => return,
        };
        if elements.is_empty() {
            return;
        }

        let target = &elements[state.rng.random_range(0..elements.len())];
        let action = if !target.actions.is_empty() && state.rng.random_bool(0.8) {
            target.actions[state.rng.random_range(0..target.actions.len())]
        } else {
            ALL_ACTIONS[state.rng.random_range(0..ALL_ACTIONS.len())]
        };

        let data = random_action_data(&mut state.rng, action);
        state.log(&format!(
            "perform_action(role={:?}, action={:?})",
            target.role, action
        ));

        match state.provider.perform_action(target, action, data) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                state.app_element = None; // force re-fetch
            }
            Err(e) => {
                state.log(&format!("  -> error (expected): {}", e));
                state.errors += 1;
            }
        }
    }

    fn op_tree_children(state: &mut FuzzState) {
        state.ensure_app();
        let app = match &state.app_element {
            Some(a) => a.clone(),
            None => return,
        };

        state.log("recursive children walk");
        fn walk(provider: &dyn Provider, el: &ElementData, depth: u32) {
            if depth > 3 {
                return;
            }
            if let Ok(children) = provider.get_children(Some(el)) {
                for child in &children {
                    let _ = &child.role;
                    let _ = &child.name;
                    walk(provider, child, depth + 1);
                }
            }
        }
        walk(&*state.provider, &app, 0);
    }

    // ── Main Loop ───────────────────────────────────────────────────────────

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

        // Find the test app
        let sel = Selector::parse(r#"application[name*="xa11y"]"#).unwrap();
        let mut app_element = None;
        for attempt in 0..10 {
            if let Ok(matches) = provider.find_elements(None, &sel, Some(1), None) {
                if let Some(app) = matches.into_iter().next() {
                    eprintln!(
                        "Test app:   {} (PID {:?})",
                        app.name.as_deref().unwrap_or("?"),
                        app.pid
                    );
                    app_element = Some(app);
                    break;
                }
            }
            if attempt < 9 {
                eprintln!("Waiting for xa11y-test-app... (attempt {})", attempt + 1);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        if app_element.is_none() {
            eprintln!("ERROR: xa11y-test-app not found. Launch it first.");
            std::process::exit(1);
        }

        eprintln!();

        let mut state = FuzzState {
            provider,
            rng: StdRng::seed_from_u64(args.seed),
            verbose: args.verbose,
            app_element,
            ops: 0,
            errors: 0,
        };

        type OpFn = fn(&mut FuzzState);
        let ops: Vec<(u32, &str, OpFn)> = vec![
            (20, "get_children", op_get_children as OpFn),
            (5, "get_parent", op_get_parent),
            (15, "find_elements", op_find_elements),
            (1, "check_permissions", op_check_permissions),
            (20, "action_on_element", op_action_on_element),
            (5, "tree_children", op_tree_children),
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
