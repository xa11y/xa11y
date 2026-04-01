use std::process;

use xa11y::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(|s| s.as_str()) {
        Some("apps") => cmd_apps(),
        Some("tree") => cmd_tree(&args[2..]),
        Some("find") => cmd_find(&args[2..]),
        Some("action") => cmd_action(&args[2..]),
        Some("events") => cmd_events(&args[2..]),
        _ => {
            print_usage();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn print_usage() {
    eprintln!(
        "\
xa11y — accessibility tree explorer

Usage:
  xa11y apps                                List running applications
  xa11y tree   [--app NAME | --pid PID]     Print the accessibility tree
  xa11y find   SELECTOR [--app NAME | --pid PID]
                                            Find elements matching a selector
  xa11y action ACTION SELECTOR [--app NAME | --pid PID] [--value V]
                                            Perform an action on an element
  xa11y events [--app NAME | --pid PID]     Stream accessibility events

Actions: press, focus, blur, toggle, expand, collapse, select, show-menu,
  scroll-into-view, scroll-down, scroll-right, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  select-text (requires --value START,END)"
    );
}

// ── Argument helpers ────────────────────────────────────────────────────────

struct Opts {
    app: Option<String>,
    pid: Option<u32>,
}

/// Parse --app NAME and --pid PID from a slice, returning the Opts and
/// remaining positional arguments.
fn parse_opts(args: &[String]) -> (Opts, Vec<String>) {
    let mut opts = Opts {
        app: None,
        pid: None,
    };
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                i += 1;
                opts.app = args.get(i).cloned();
            }
            "--pid" => {
                i += 1;
                opts.pid = args.get(i).and_then(|s| s.parse().ok());
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    (opts, positional)
}

fn resolve_app(opts: &Opts) -> Result<App> {
    if let Some(name) = &opts.app {
        App::by_name(name)
    } else if let Some(pid) = opts.pid {
        App::by_pid(pid)
    } else {
        Err(Error::Platform {
            code: -1,
            message: "specify --app NAME or --pid PID".into(),
        })
    }
}

// ── Output helpers ──────────────────────────────────────────────────────────

fn format_element_oneline(el: &ElementData) -> String {
    let mut parts = Vec::new();

    parts.push(el.role.to_snake_case().to_string());

    if let Some(name) = &el.name {
        parts.push(format!("\"{}\"", name));
    }

    if let Some(value) = &el.value {
        parts.push(format!("value=\"{}\"", value));
    }

    if let Some(nv) = el.numeric_value {
        let mut range = format!("numeric_value={nv}");
        if let Some(min) = el.min_value {
            range.push_str(&format!(" min={min}"));
        }
        if let Some(max) = el.max_value {
            range.push_str(&format!(" max={max}"));
        }
        parts.push(range);
    }

    if let Some(desc) = &el.description {
        parts.push(format!("description=\"{}\"", desc));
    }

    // States
    let mut states = Vec::new();
    if el.states.enabled {
        states.push("enabled");
    } else {
        states.push("disabled");
    }
    if el.states.visible {
        states.push("visible");
    } else {
        states.push("hidden");
    }
    if el.states.focused {
        states.push("focused");
    }
    if el.states.focusable {
        states.push("focusable");
    }
    if el.states.editable {
        states.push("editable");
    }
    if el.states.selected {
        states.push("selected");
    }
    if el.states.modal {
        states.push("modal");
    }
    if el.states.required {
        states.push("required");
    }
    if el.states.busy {
        states.push("busy");
    }
    if let Some(checked) = &el.states.checked {
        states.push(match checked {
            Toggled::Off => "checked=off",
            Toggled::On => "checked=on",
            Toggled::Mixed => "checked=mixed",
        });
    }
    if let Some(expanded) = el.states.expanded {
        if expanded {
            states.push("expanded");
        } else {
            states.push("collapsed");
        }
    }
    if !states.is_empty() {
        parts.push(format!("[{}]", states.join(" ")));
    }

    if let Some(bounds) = &el.bounds {
        parts.push(format!(
            "bounds=({},{},{},{})",
            bounds.x, bounds.y, bounds.width, bounds.height
        ));
    }

    if let Some(id) = &el.stable_id {
        parts.push(format!("id=\"{}\"", id));
    }

    if !el.actions.is_empty() {
        let action_names: Vec<&str> = el
            .actions
            .iter()
            .map(|a| match a {
                Action::Press => "press",
                Action::Focus => "focus",
                Action::Blur => "blur",
                Action::SetValue => "set-value",
                Action::Toggle => "toggle",
                Action::Expand => "expand",
                Action::Collapse => "collapse",
                Action::Select => "select",
                Action::ShowMenu => "show-menu",
                Action::ScrollIntoView => "scroll-into-view",
                Action::ScrollDown => "scroll-down",
                Action::ScrollRight => "scroll-right",
                Action::Increment => "increment",
                Action::Decrement => "decrement",
                Action::SetTextSelection => "select-text",
                Action::TypeText => "type-text",
            })
            .collect();
        parts.push(format!("actions=[{}]", action_names.join(",")));
    }

    parts.join(" ")
}

fn print_tree_recursive(el: &Element, prefix: &str, is_last: bool, is_root: bool) {
    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };
    println!("{prefix}{connector}{}", format_element_oneline(el));

    let children = match el.children() {
        Ok(c) => c,
        Err(e) => {
            let child_prefix = if is_root {
                prefix.to_string()
            } else if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│   ")
            };
            println!("{child_prefix}└── <error: {e}>");
            return;
        }
    };

    let child_prefix = if is_root {
        prefix.to_string()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    for (i, child) in children.iter().enumerate() {
        let child_is_last = i == children.len() - 1;
        print_tree_recursive(child, &child_prefix, child_is_last, false);
    }
}

// ── Commands ────────────────────────────────────────────────────────────────

fn cmd_apps() -> Result<()> {
    let apps = App::list()?;
    if apps.is_empty() {
        println!("No applications found.");
        return Ok(());
    }
    for app in &apps {
        let pid_str = app.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        println!("{}\t{}", pid_str, app.name);
    }
    Ok(())
}

fn cmd_tree(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let app = resolve_app(&opts)?;
    let root_el = Element::new(app.data.clone(), app.provider().clone());
    print_tree_recursive(&root_el, "", true, true);
    Ok(())
}

fn cmd_find(args: &[String]) -> Result<()> {
    let (opts, positional) = parse_opts(args);
    let selector = positional.first().ok_or(Error::Platform {
        code: -1,
        message: "usage: xa11y find SELECTOR [--app NAME | --pid PID]".into(),
    })?;

    let app = resolve_app(&opts)?;
    let elements = app.locator(selector).elements()?;
    for el in &elements {
        println!("{}", format_element_oneline(el));
    }
    println!(
        "({} match{})",
        elements.len(),
        if elements.len() == 1 { "" } else { "es" }
    );
    Ok(())
}

fn cmd_action(args: &[String]) -> Result<()> {
    let (opts, positional) = parse_opts(args);
    if positional.len() < 2 {
        return Err(Error::Platform {
            code: -1,
            message: "usage: xa11y action ACTION SELECTOR [--app NAME | --pid PID] [--value V]"
                .into(),
        });
    }
    let action_name = &positional[0];
    let selector = &positional[1];

    // Extract --value from the raw args (before opts parsing strips it)
    let value = extract_flag_value(args, "--value");

    let app = resolve_app(&opts)?;
    let locator = app.locator(selector);

    match action_name.as_str() {
        "press" => locator.press()?,
        "focus" => locator.focus()?,
        "blur" => locator.blur()?,
        "toggle" => locator.toggle()?,
        "expand" => locator.expand()?,
        "collapse" => locator.collapse()?,
        "select" => locator.select()?,
        "show-menu" => locator.show_menu()?,
        "scroll-into-view" => locator.scroll_into_view()?,
        "scroll-down" => {
            let amount = value
                .as_deref()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            locator.scroll_down(amount)?;
        }
        "scroll-right" => {
            let amount = value
                .as_deref()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            locator.scroll_right(amount)?;
        }
        "increment" => locator.increment()?,
        "decrement" => locator.decrement()?,
        "set-value" => {
            let v = value.ok_or(Error::Platform {
                code: -1,
                message: "set-value requires --value".into(),
            })?;
            locator.set_value(&v)?;
        }
        "type-text" => {
            let v = value.ok_or(Error::Platform {
                code: -1,
                message: "type-text requires --value".into(),
            })?;
            locator.type_text(&v)?;
        }
        "select-text" => {
            let v = value.ok_or(Error::Platform {
                code: -1,
                message: "select-text requires --value START,END".into(),
            })?;
            let parts: Vec<&str> = v.split(',').collect();
            if parts.len() != 2 {
                return Err(Error::Platform {
                    code: -1,
                    message: "select-text --value must be START,END (e.g. 0,5)".into(),
                });
            }
            let start: u32 = parts[0].trim().parse().map_err(|_| Error::Platform {
                code: -1,
                message: "invalid START in select-text --value".into(),
            })?;
            let end: u32 = parts[1].trim().parse().map_err(|_| Error::Platform {
                code: -1,
                message: "invalid END in select-text --value".into(),
            })?;
            locator.select_text(start, end)?;
        }
        other => {
            return Err(Error::Platform {
                code: -1,
                message: format!("unknown action: {other}"),
            });
        }
    }
    println!("ok");
    Ok(())
}

fn cmd_events(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let app = resolve_app(&opts)?;
    let sub = app.subscribe()?;
    eprintln!(
        "Listening for events on \"{}\" (ctrl-c to stop)...",
        app.name
    );
    for event in sub.iter() {
        let target_str = event
            .target
            .as_ref()
            .map(|t| {
                let name_part = t
                    .name
                    .as_ref()
                    .map(|n| format!(" \"{}\"", n))
                    .unwrap_or_default();
                format!("{}{name_part}", t.role.to_snake_case())
            })
            .unwrap_or_else(|| "-".into());
        let detail = format_event_detail(&event);
        println!("[{:?}] {target_str}{detail}", event.event_type);
    }
    Ok(())
}

fn format_event_detail(event: &Event) -> String {
    let mut parts = Vec::new();
    if let Some(flag) = &event.state_flag {
        let val = event.state_value.unwrap_or(false);
        parts.push(format!(" {flag:?}={val}"));
    }
    if let Some(tc) = &event.text_change {
        let pos = tc.position.map(|p| format!(" @{p}")).unwrap_or_default();
        parts.push(format!(" {:?}{pos}", tc.change_type));
    }
    parts.join("")
}

fn extract_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}
