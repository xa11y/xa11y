// CLI implementation for `xa11y` — accessibility tree explorer.
//
// This module is `#[doc(hidden)]` and not part of the public API.
// It powers both `cargo install xa11y` and `pip install xa11y` via PyO3.

use crate::*;

/// Run the CLI with the given arguments (excluding the program name).
///
/// Returns `Ok(())` on success, or an `Err` with a human-readable message
/// on failure. The caller is responsible for printing the error and exiting.
pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("apps") => cmd_apps(),
        Some("tree") => cmd_tree(&args[1..]),
        Some("find") => cmd_find(&args[1..]),
        Some("action") => cmd_action(&args[1..]),
        Some("events") => cmd_events(&args[1..]),
        _ => {
            print_usage();
            Ok(())
        }
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
  scroll-into-view, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  select-text (requires --value START,END)"
    );
}

// ── Argument helpers ────────────────────────────────────────────────────────

pub(crate) struct Opts {
    pub app: Option<String>,
    pub pid: Option<u32>,
}

/// Parse --app NAME and --pid PID from a slice, returning the Opts and
/// remaining positional arguments.
pub(crate) fn parse_opts(args: &[String]) -> (Opts, Vec<String>) {
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

pub(crate) fn resolve_app(opts: &Opts) -> Result<App> {
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

pub(crate) fn format_element_oneline(el: &ElementData) -> String {
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
        let names: Vec<&str> = el.actions.iter().map(|a| a.as_str()).collect();
        parts.push(format!("actions=[{}]", names.join(",")));
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
        println!("[{:?}] {target_str}{detail}", event.kind);
    }
    Ok(())
}

pub(crate) fn format_event_detail(event: &Event) -> String {
    if let EventKind::StateChanged { flag, value } = event.kind {
        format!(" {flag:?}={value}")
    } else {
        String::new()
    }
}

pub(crate) fn extract_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── Argument parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_opts_app_flag() {
        let args = strs(&["--app", "Safari"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.app.as_deref(), Some("Safari"));
        assert!(opts.pid.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_pid_flag() {
        let args = strs(&["--pid", "1234"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.pid, Some(1234));
        assert!(opts.app.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_positional_and_flags() {
        let args = strs(&["button[name='OK']", "--app", "MyApp"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.app.as_deref(), Some("MyApp"));
        assert_eq!(pos, vec![s("button[name='OK']")]);
    }

    #[test]
    fn parse_opts_multiple_positional() {
        let args = strs(&["press", "button", "--app", "Test"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.app.as_deref(), Some("Test"));
        assert_eq!(pos, vec![s("press"), s("button")]);
    }

    #[test]
    fn parse_opts_empty() {
        let args: Vec<String> = vec![];
        let (opts, pos) = parse_opts(&args);
        assert!(opts.app.is_none());
        assert!(opts.pid.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn extract_flag_value_found() {
        let args = strs(&["--app", "Foo", "--value", "hello"]);
        assert_eq!(extract_flag_value(&args, "--value"), Some(s("hello")));
    }

    #[test]
    fn extract_flag_value_missing() {
        let args = strs(&["--app", "Foo"]);
        assert_eq!(extract_flag_value(&args, "--value"), None);
    }

    #[test]
    fn extract_flag_value_at_end() {
        let args = strs(&["--value"]);
        assert_eq!(extract_flag_value(&args, "--value"), None);
    }

    // ── Format element ──────────────────────────────────────────────────────

    fn make_element(role: Role, name: Option<&str>) -> ElementData {
        ElementData {
            role,
            name: name.map(String::from),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: std::collections::HashMap::new(),
            handle: 0,
        }
    }

    #[test]
    fn format_element_basic() {
        let el = make_element(Role::Button, Some("OK"));
        let out = format_element_oneline(&el);
        assert!(out.starts_with("button"));
        assert!(out.contains("\"OK\""));
        assert!(out.contains("enabled"));
        assert!(out.contains("visible"));
    }

    #[test]
    fn format_element_no_name() {
        let el = make_element(Role::WebArea, None);
        let out = format_element_oneline(&el);
        assert!(out.starts_with("web_area"));
        assert!(!out.contains('"'));
    }

    #[test]
    fn format_element_with_value() {
        let mut el = make_element(Role::TextField, Some("Search"));
        el.value = Some("query".into());
        let out = format_element_oneline(&el);
        assert!(out.contains("value=\"query\""));
    }

    #[test]
    fn format_element_with_bounds() {
        let mut el = make_element(Role::Button, Some("X"));
        el.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        });
        let out = format_element_oneline(&el);
        assert!(out.contains("bounds=(10,20,30,40)"));
    }

    #[test]
    fn format_element_disabled() {
        let mut el = make_element(Role::Button, Some("Cancel"));
        el.states.enabled = false;
        let out = format_element_oneline(&el);
        assert!(out.contains("disabled"));
        assert!(!out.contains("enabled"));
    }

    #[test]
    fn format_element_checked() {
        let mut el = make_element(Role::CheckBox, Some("Agree"));
        el.states.checked = Some(Toggled::On);
        let out = format_element_oneline(&el);
        assert!(out.contains("checked=on"));
    }

    #[test]
    fn format_element_expanded() {
        let mut el = make_element(Role::TreeItem, Some("Folder"));
        el.states.expanded = Some(true);
        let out = format_element_oneline(&el);
        assert!(out.contains("expanded"));
    }

    #[test]
    fn format_element_collapsed() {
        let mut el = make_element(Role::TreeItem, Some("Folder"));
        el.states.expanded = Some(false);
        let out = format_element_oneline(&el);
        assert!(out.contains("collapsed"));
    }

    #[test]
    fn format_element_with_actions() {
        let mut el = make_element(Role::Button, Some("Go"));
        el.actions = vec!["press".to_string(), "focus".to_string()];
        let out = format_element_oneline(&el);
        assert!(out.contains("actions=[press,focus]"));
    }

    #[test]
    fn format_element_with_stable_id() {
        let mut el = make_element(Role::Button, Some("X"));
        el.stable_id = Some("btn-close".into());
        let out = format_element_oneline(&el);
        assert!(out.contains("id=\"btn-close\""));
    }

    #[test]
    fn format_element_with_description() {
        let mut el = make_element(Role::Button, Some("Back"));
        el.description = Some("Navigate back".into());
        let out = format_element_oneline(&el);
        assert!(out.contains("description=\"Navigate back\""));
    }

    #[test]
    fn format_element_with_numeric_value() {
        let mut el = make_element(Role::Slider, Some("Volume"));
        el.numeric_value = Some(75.0);
        el.min_value = Some(0.0);
        el.max_value = Some(100.0);
        let out = format_element_oneline(&el);
        assert!(out.contains("numeric_value=75"));
        assert!(out.contains("min=0"));
        assert!(out.contains("max=100"));
    }

    // ── Event formatting ────────────────────────────────────────────────────

    #[test]
    fn format_event_detail_state_change() {
        let event = Event {
            kind: EventKind::StateChanged {
                flag: StateFlag::Focused,
                value: true,
            },
            app_name: "App".into(),
            app_pid: 1,
            target: None,
            timestamp: std::time::Instant::now(),
        };
        let detail = format_event_detail(&event);
        assert!(detail.contains("Focused=true"));
    }

    #[test]
    fn format_event_detail_empty() {
        let event = Event {
            kind: EventKind::FocusChanged,
            app_name: "App".into(),
            app_pid: 1,
            target: None,
            timestamp: std::time::Instant::now(),
        };
        assert!(format_event_detail(&event).is_empty());
    }

    // ── resolve_app error ───────────────────────────────────────────────────

    #[test]
    fn resolve_app_no_flags_is_error() {
        let opts = Opts {
            app: None,
            pid: None,
        };
        let err = resolve_app(&opts).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--app") || msg.contains("--pid"));
    }
}
