// CLI implementation for `xa11y` — accessibility tree explorer.
//
// This module is `#[doc(hidden)]` and not part of the public API.
// It powers both `cargo install xa11y` and `pip install xa11y` via PyO3.

use std::time::Duration;

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
        Some("click") => cmd_click(&args[1..]),
        Some("move") => cmd_move(&args[1..]),
        Some("drag") => cmd_drag(&args[1..]),
        Some("scroll") => cmd_scroll(&args[1..]),
        Some("key") => cmd_key(&args[1..]),
        Some("type") => cmd_type(&args[1..]),
        Some("screenshot") => cmd_screenshot(&args[1..]),
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

Accessibility tree:
  xa11y apps                                List running applications
  xa11y tree   [--app NAME | --pid PID]     Print the accessibility tree
  xa11y find   SELECTOR [--app NAME | --pid PID] [-o pretty|bounds|center]
                                            Find elements matching a selector
  xa11y action ACTION SELECTOR [--app NAME | --pid PID] [--value V]
                                            Perform an action on an element
  xa11y events [--app NAME | --pid PID]     Stream accessibility events

Input simulation (coords only — no selectors, no a11y):
  xa11y click  --at X,Y [--button left|right|middle] [--count N] [--held K,K]
  xa11y move   --at X,Y
  xa11y drag   --from X,Y --to X,Y [--button B] [--duration-ms MS] [--held K,K]
  xa11y scroll --at X,Y [--dx N] [--dy N]
  xa11y key    KEY [--held K,K]
  xa11y type   TEXT

Screenshot (regions only — no selectors, no a11y):
  xa11y screenshot [--region X,Y,W,H] --out PATH
                                            --out - writes PNG bytes to stdout

Compose a11y + input/screenshot via `find -o bounds|center`:
  region=$(xa11y find 'button[name=\"OK\"]' --app Safari -o bounds)
  xa11y screenshot --region \"$region\" --out button.png
  xa11y click --at \"$(xa11y find 'button[name=\"OK\"]' --app Safari -o center)\"

Actions: press, focus, blur, toggle, expand, collapse, select, show-menu,
  scroll-into-view, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  select-text (requires --value START,END)"
    );
}

// ── Argument helpers ────────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct Opts {
    pub app: Option<String>,
    pub pid: Option<u32>,
    pub value: Option<String>,
    // Input simulation / screenshot
    pub at: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub button: Option<String>,
    pub count: Option<u32>,
    pub held: Option<String>,
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    pub duration_ms: Option<u64>,
    pub region: Option<String>,
    pub out: Option<String>,
    // Output format for `find`
    pub output_format: Option<String>,
}

/// Parse known flags from a slice, returning the parsed Opts and the
/// remaining positional arguments.
///
/// Unknown flags are left in the positional output (so downstream callers
/// see them and can surface a sensible error) rather than swallowed.
pub(crate) fn parse_opts(args: &[String]) -> (Opts, Vec<String>) {
    let mut opts = Opts::default();
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
            "--value" => {
                i += 1;
                opts.value = args.get(i).cloned();
            }
            "--at" => {
                i += 1;
                opts.at = args.get(i).cloned();
            }
            "--from" => {
                i += 1;
                opts.from = args.get(i).cloned();
            }
            "--to" => {
                i += 1;
                opts.to = args.get(i).cloned();
            }
            "--button" => {
                i += 1;
                opts.button = args.get(i).cloned();
            }
            "--count" => {
                i += 1;
                opts.count = args.get(i).and_then(|s| s.parse().ok());
            }
            "--held" => {
                i += 1;
                opts.held = args.get(i).cloned();
            }
            "--dx" => {
                i += 1;
                opts.dx = args.get(i).and_then(|s| s.parse().ok());
            }
            "--dy" => {
                i += 1;
                opts.dy = args.get(i).and_then(|s| s.parse().ok());
            }
            "--duration-ms" => {
                i += 1;
                opts.duration_ms = args.get(i).and_then(|s| s.parse().ok());
            }
            "--region" => {
                i += 1;
                opts.region = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                opts.out = args.get(i).cloned();
            }
            "-o" => {
                i += 1;
                opts.output_format = args.get(i).cloned();
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    (opts, positional)
}

// ── Parsers for complex flag values ─────────────────────────────────────────

fn missing(what: &str) -> Error {
    Error::Platform {
        code: -1,
        message: format!("missing {what}"),
    }
}

pub(crate) fn parse_point_arg(s: &str, ctx: &str) -> Result<Point> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return Err(Error::Platform {
            code: -1,
            message: format!("{ctx} must be X,Y (got: {s})"),
        });
    }
    let x: i32 = parts[0].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid X in {ctx}: {}", parts[0]),
    })?;
    let y: i32 = parts[1].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid Y in {ctx}: {}", parts[1]),
    })?;
    Ok(Point::new(x, y))
}

pub(crate) fn parse_region_arg(s: &str) -> Result<Rect> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(Error::Platform {
            code: -1,
            message: format!("--region must be X,Y,W,H (got: {s})"),
        });
    }
    let x: i32 = parts[0].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid X in --region: {}", parts[0]),
    })?;
    let y: i32 = parts[1].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid Y in --region: {}", parts[1]),
    })?;
    let width: u32 = parts[2].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid W in --region: {}", parts[2]),
    })?;
    let height: u32 = parts[3].trim().parse().map_err(|_| Error::Platform {
        code: -1,
        message: format!("invalid H in --region: {}", parts[3]),
    })?;
    Ok(Rect {
        x,
        y,
        width,
        height,
    })
}

/// Parse a key-name string into a [`Key`]. Accepts single chars (`"a"`,
/// `"7"`), named modifiers (`"Shift"`, `"Ctrl"`/`"Control"`, `"Alt"`/
/// `"Option"`, `"Meta"`/`"Cmd"`/`"Command"`/`"Super"`/`"Win"`), named keys
/// (`"Enter"`, `"Tab"`, `"Escape"`, `"ArrowUp/Down/Left/Right"`, …), and
/// function keys (`"F1"` … `"F24"`). Mirrors the Python bindings.
pub(crate) fn parse_key_name(name: &str) -> Result<Key> {
    let k = match name {
        "Shift" => Key::Shift,
        "Ctrl" | "Control" => Key::Ctrl,
        "Alt" | "Option" => Key::Alt,
        "Meta" | "Cmd" | "Command" | "Super" | "Win" => Key::Meta,
        "Enter" | "Return" => Key::Enter,
        "Escape" | "Esc" => Key::Escape,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Space" => Key::Space,
        "Delete" => Key::Delete,
        "Insert" => Key::Insert,
        "ArrowUp" | "Up" => Key::ArrowUp,
        "ArrowDown" | "Down" => Key::ArrowDown,
        "ArrowLeft" | "Left" => Key::ArrowLeft,
        "ArrowRight" | "Right" => Key::ArrowRight,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        s if s.starts_with('F') && s.len() >= 2 && s[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = s[1..].parse().map_err(|_| Error::InvalidActionData {
                message: format!("invalid function key: {s}"),
            })?;
            Key::F(n)
        }
        s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap()),
        _ => {
            return Err(Error::InvalidActionData {
                message: format!("unknown key name: {name}"),
            });
        }
    };
    Ok(k)
}

pub(crate) fn parse_held(raw: Option<&str>) -> Result<Vec<Key>> {
    match raw {
        None => Ok(Vec::new()),
        Some("") => Ok(Vec::new()),
        Some(s) => s.split(',').map(|p| parse_key_name(p.trim())).collect(),
    }
}

pub(crate) fn parse_button(raw: &str) -> Result<MouseButton> {
    match raw {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(Error::InvalidActionData {
            message: format!("unknown button: {other} (expected left|right|middle)"),
        }),
    }
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
        message: "usage: xa11y find SELECTOR [--app NAME | --pid PID] [-o pretty|bounds|center]"
            .into(),
    })?;

    let app = resolve_app(&opts)?;
    let elements = app.locator(selector).elements()?;
    let fmt = opts.output_format.as_deref().unwrap_or("pretty");
    match fmt {
        "pretty" => {
            for el in &elements {
                println!("{}", format_element_oneline(el));
            }
            println!(
                "({} match{})",
                elements.len(),
                if elements.len() == 1 { "" } else { "es" }
            );
        }
        "bounds" => {
            for el in &elements {
                println!("{}", format_bounds_line(el)?);
            }
        }
        "center" => {
            for el in &elements {
                println!("{}", format_center_line(el)?);
            }
        }
        other => {
            return Err(Error::Platform {
                code: -1,
                message: format!("unknown -o format: {other} (expected pretty|bounds|center)"),
            });
        }
    }
    Ok(())
}

/// Format an element's bounds as `X,Y,W,H` — the input to `--region`.
pub(crate) fn format_bounds_line(el: &ElementData) -> Result<String> {
    let b = el.bounds.ok_or(Error::NoElementBounds)?;
    Ok(format!("{},{},{},{}", b.x, b.y, b.width, b.height))
}

/// Format the center of an element's bounds as `X,Y` — the input to `--at`.
pub(crate) fn format_center_line(el: &ElementData) -> Result<String> {
    let b = el.bounds.ok_or(Error::NoElementBounds)?;
    let cx = b.x + (b.width as i32) / 2;
    let cy = b.y + (b.height as i32) / 2;
    Ok(format!("{cx},{cy}"))
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
    let value = opts.value.clone();

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

// ── Input simulation ────────────────────────────────────────────────────────

fn cmd_click(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let at = parse_point_arg(
        opts.at.as_deref().ok_or_else(|| missing("--at X,Y"))?,
        "--at",
    )?;
    let click_opts = build_click_options(&opts)?;

    let sim = crate::input_sim()?;
    sim.mouse().click_with(ClickTarget::Point(at), click_opts)?;
    println!("ok");
    Ok(())
}

/// Translate parsed flags into [`ClickOptions`]. Extracted so the flag
/// → options mapping is unit-testable without a live input backend.
pub(crate) fn build_click_options(opts: &Opts) -> Result<ClickOptions> {
    let button = opts
        .button
        .as_deref()
        .map(parse_button)
        .transpose()?
        .unwrap_or(MouseButton::Left);
    let count = opts.count.unwrap_or(1);
    let held = parse_held(opts.held.as_deref())?;
    Ok(ClickOptions {
        button,
        count,
        held,
        anchor: Anchor::Center,
    })
}

fn cmd_move(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let at = parse_point_arg(
        opts.at.as_deref().ok_or_else(|| missing("--at X,Y"))?,
        "--at",
    )?;
    let sim = crate::input_sim()?;
    sim.mouse().move_to(at)?;
    println!("ok");
    Ok(())
}

fn cmd_drag(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let from = parse_point_arg(
        opts.from.as_deref().ok_or_else(|| missing("--from X,Y"))?,
        "--from",
    )?;
    let to = parse_point_arg(
        opts.to.as_deref().ok_or_else(|| missing("--to X,Y"))?,
        "--to",
    )?;
    let drag_opts = build_drag_options(&opts)?;

    let sim = crate::input_sim()?;
    sim.mouse().drag_with(from, to, drag_opts)?;
    println!("ok");
    Ok(())
}

/// Translate parsed flags into [`DragOptions`]. Extracted so the flag
/// → options mapping is unit-testable without a live input backend.
pub(crate) fn build_drag_options(opts: &Opts) -> Result<DragOptions> {
    let button = opts
        .button
        .as_deref()
        .map(parse_button)
        .transpose()?
        .unwrap_or(MouseButton::Left);
    let held = parse_held(opts.held.as_deref())?;
    let duration = Duration::from_millis(opts.duration_ms.unwrap_or(150));
    Ok(DragOptions {
        button,
        held,
        duration,
    })
}

fn cmd_scroll(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let at = parse_point_arg(
        opts.at.as_deref().ok_or_else(|| missing("--at X,Y"))?,
        "--at",
    )?;
    let dx = opts.dx.unwrap_or(0);
    let dy = opts.dy.unwrap_or(0);
    let sim = crate::input_sim()?;
    sim.mouse().scroll(at, ScrollDelta::new(dx, dy))?;
    println!("ok");
    Ok(())
}

fn cmd_key(args: &[String]) -> Result<()> {
    let (opts, positional) = parse_opts(args);
    let name = positional.first().ok_or(Error::Platform {
        code: -1,
        message: "usage: xa11y key KEY [--held K,K]".into(),
    })?;
    let key = parse_key_name(name)?;
    let held = parse_held(opts.held.as_deref())?;
    let sim = crate::input_sim()?;
    if held.is_empty() {
        sim.keyboard().press(key)?;
    } else {
        sim.keyboard().chord(key, &held)?;
    }
    println!("ok");
    Ok(())
}

fn cmd_type(args: &[String]) -> Result<()> {
    let (_opts, positional) = parse_opts(args);
    let text = positional.first().ok_or(Error::Platform {
        code: -1,
        message: "usage: xa11y type TEXT".into(),
    })?;
    let sim = crate::input_sim()?;
    sim.keyboard().type_text(text)?;
    println!("ok");
    Ok(())
}

// ── Screenshot ──────────────────────────────────────────────────────────────

fn cmd_screenshot(args: &[String]) -> Result<()> {
    let (opts, _pos) = parse_opts(args);
    let out = opts
        .out
        .as_deref()
        .ok_or_else(|| missing("--out PATH (use - for stdout)"))?;

    let shot = if let Some(region_str) = opts.region.as_deref() {
        let rect = parse_region_arg(region_str)?;
        crate::screenshot_region(rect)?
    } else {
        crate::screenshot()?
    };

    if out == "-" {
        use std::io::Write;
        let bytes = shot.to_png()?;
        std::io::stdout()
            .write_all(&bytes)
            .map_err(|e| Error::Platform {
                code: e.raw_os_error().unwrap_or(-1) as i64,
                message: format!("write stdout: {e}"),
            })?;
    } else {
        shot.save_png(out)?;
        eprintln!(
            "wrote {out} ({}x{} @{}x)",
            shot.width, shot.height, shot.scale
        );
    }
    Ok(())
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
    fn parse_opts_value_flag() {
        let args = strs(&["--value", "hello"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.value.as_deref(), Some("hello"));
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_value_before_positional_does_not_leak() {
        // Regression: `--value` used to fall into the positional arm, so
        // an args list that placed it before the selector produced a
        // positional list of ["action", "--value", "text", "selector"],
        // and the CLI mistook "--value" for the selector.
        let args = strs(&["set-value", "--value", "hello", "button[name='OK']"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.value.as_deref(), Some("hello"));
        assert_eq!(pos, vec![s("set-value"), s("button[name='OK']")]);
    }

    #[test]
    fn parse_opts_value_missing_trailing_arg() {
        let args = strs(&["--value"]);
        let (opts, pos) = parse_opts(&args);
        assert!(opts.value.is_none());
        assert!(pos.is_empty());
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
        let opts = Opts::default();
        let err = resolve_app(&opts).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--app") || msg.contains("--pid"));
    }

    // ── Input-sim / screenshot flag parsing ─────────────────────────────────

    #[test]
    fn parse_opts_at_flag() {
        let args = strs(&["--at", "100,200"]);
        let (opts, pos) = parse_opts(&args);
        assert_eq!(opts.at.as_deref(), Some("100,200"));
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_from_to_flags() {
        let args = strs(&["--from", "1,2", "--to", "3,4"]);
        let (opts, _) = parse_opts(&args);
        assert_eq!(opts.from.as_deref(), Some("1,2"));
        assert_eq!(opts.to.as_deref(), Some("3,4"));
    }

    #[test]
    fn parse_opts_button_count_held() {
        let args = strs(&["--button", "right", "--count", "2", "--held", "Shift,Meta"]);
        let (opts, _) = parse_opts(&args);
        assert_eq!(opts.button.as_deref(), Some("right"));
        assert_eq!(opts.count, Some(2));
        assert_eq!(opts.held.as_deref(), Some("Shift,Meta"));
    }

    #[test]
    fn parse_opts_scroll_deltas() {
        let args = strs(&["--dx", "-3", "--dy", "5"]);
        let (opts, _) = parse_opts(&args);
        assert_eq!(opts.dx, Some(-3));
        assert_eq!(opts.dy, Some(5));
    }

    #[test]
    fn parse_opts_duration_region_out() {
        let args = strs(&[
            "--duration-ms",
            "250",
            "--region",
            "10,20,30,40",
            "--out",
            "shot.png",
        ]);
        let (opts, _) = parse_opts(&args);
        assert_eq!(opts.duration_ms, Some(250));
        assert_eq!(opts.region.as_deref(), Some("10,20,30,40"));
        assert_eq!(opts.out.as_deref(), Some("shot.png"));
    }

    #[test]
    fn parse_opts_output_format() {
        let args = strs(&["-o", "bounds"]);
        let (opts, _) = parse_opts(&args);
        assert_eq!(opts.output_format.as_deref(), Some("bounds"));
    }

    // ── Point / region parsers ──────────────────────────────────────────────

    #[test]
    fn parse_point_basic() {
        let pt = parse_point_arg("100,200", "--at").unwrap();
        assert_eq!(pt, Point::new(100, 200));
    }

    #[test]
    fn parse_point_trims_whitespace() {
        let pt = parse_point_arg("100, 200", "--at").unwrap();
        assert_eq!(pt, Point::new(100, 200));
    }

    #[test]
    fn parse_point_negative() {
        let pt = parse_point_arg("-5,-10", "--at").unwrap();
        assert_eq!(pt, Point::new(-5, -10));
    }

    #[test]
    fn parse_point_wrong_arity_errors() {
        assert!(parse_point_arg("100", "--at").is_err());
        assert!(parse_point_arg("1,2,3", "--at").is_err());
    }

    #[test]
    fn parse_point_non_numeric_errors() {
        assert!(parse_point_arg("abc,200", "--at").is_err());
        assert!(parse_point_arg("100,xyz", "--at").is_err());
    }

    #[test]
    fn parse_region_basic() {
        let r = parse_region_arg("10,20,30,40").unwrap();
        assert_eq!(r.x, 10);
        assert_eq!(r.y, 20);
        assert_eq!(r.width, 30);
        assert_eq!(r.height, 40);
    }

    #[test]
    fn parse_region_wrong_arity_errors() {
        assert!(parse_region_arg("10,20,30").is_err());
        assert!(parse_region_arg("10,20,30,40,50").is_err());
    }

    #[test]
    fn parse_region_rejects_negative_dimensions() {
        // W/H are u32 — parsing "-1" as u32 must fail.
        assert!(parse_region_arg("0,0,-1,100").is_err());
    }

    // ── Key / button / held parsers ─────────────────────────────────────────

    #[test]
    fn parse_key_named() {
        assert!(matches!(parse_key_name("Enter").unwrap(), Key::Enter));
        assert!(matches!(parse_key_name("Return").unwrap(), Key::Enter));
        assert!(matches!(parse_key_name("Shift").unwrap(), Key::Shift));
        assert!(matches!(parse_key_name("Cmd").unwrap(), Key::Meta));
        assert!(matches!(parse_key_name("ArrowUp").unwrap(), Key::ArrowUp));
        assert!(matches!(parse_key_name("Up").unwrap(), Key::ArrowUp));
    }

    #[test]
    fn parse_key_char_single() {
        assert!(matches!(parse_key_name("a").unwrap(), Key::Char('a')));
        assert!(matches!(parse_key_name("7").unwrap(), Key::Char('7')));
        assert!(matches!(parse_key_name(";").unwrap(), Key::Char(';')));
    }

    #[test]
    fn parse_key_function() {
        assert!(matches!(parse_key_name("F1").unwrap(), Key::F(1)));
        assert!(matches!(parse_key_name("F12").unwrap(), Key::F(12)));
    }

    #[test]
    fn parse_key_unknown_errors() {
        assert!(parse_key_name("NotAKey").is_err());
        assert!(parse_key_name("").is_err());
    }

    #[test]
    fn parse_held_none_and_empty_are_empty() {
        assert!(parse_held(None).unwrap().is_empty());
        assert!(parse_held(Some("")).unwrap().is_empty());
    }

    #[test]
    fn parse_held_multi() {
        let keys = parse_held(Some("Shift,Meta")).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(matches!(keys[0], Key::Shift));
        assert!(matches!(keys[1], Key::Meta));
    }

    #[test]
    fn parse_held_trims_whitespace() {
        let keys = parse_held(Some(" Shift , Ctrl ")).unwrap();
        assert!(matches!(keys[0], Key::Shift));
        assert!(matches!(keys[1], Key::Ctrl));
    }

    #[test]
    fn parse_button_names() {
        assert!(matches!(parse_button("left").unwrap(), MouseButton::Left));
        assert!(matches!(parse_button("right").unwrap(), MouseButton::Right));
        assert!(matches!(
            parse_button("middle").unwrap(),
            MouseButton::Middle
        ));
    }

    #[test]
    fn parse_button_unknown_errors() {
        assert!(parse_button("Left").is_err()); // case-sensitive
        assert!(parse_button("nope").is_err());
    }

    // ── `find -o bounds|center` output formatters ───────────────────────────

    #[test]
    fn format_bounds_line_basic() {
        let mut el = make_element(Role::Button, Some("OK"));
        el.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        });
        assert_eq!(format_bounds_line(&el).unwrap(), "10,20,30,40");
    }

    #[test]
    fn format_bounds_line_negative_origin() {
        // Negative X/Y are legal on multi-monitor layouts — propagate verbatim.
        let mut el = make_element(Role::Button, Some("B"));
        el.bounds = Some(Rect {
            x: -5,
            y: -10,
            width: 20,
            height: 30,
        });
        assert_eq!(format_bounds_line(&el).unwrap(), "-5,-10,20,30");
    }

    #[test]
    fn format_bounds_line_errors_without_bounds() {
        let el = make_element(Role::Button, Some("X"));
        assert!(matches!(
            format_bounds_line(&el),
            Err(Error::NoElementBounds)
        ));
    }

    #[test]
    fn format_center_line_basic() {
        let mut el = make_element(Role::Button, Some("OK"));
        el.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        });
        // Center of (10,20,30,40) = (10+15, 20+20) = (25, 40).
        assert_eq!(format_center_line(&el).unwrap(), "25,40");
    }

    #[test]
    fn format_center_line_odd_dimensions_floor() {
        // Integer division — center of (0,0,5,7) = (2, 3), not (2.5, 3.5).
        let mut el = make_element(Role::Button, Some("B"));
        el.bounds = Some(Rect {
            x: 0,
            y: 0,
            width: 5,
            height: 7,
        });
        assert_eq!(format_center_line(&el).unwrap(), "2,3");
    }

    #[test]
    fn format_center_line_errors_without_bounds() {
        let el = make_element(Role::Button, Some("X"));
        assert!(matches!(
            format_center_line(&el),
            Err(Error::NoElementBounds)
        ));
    }

    // ── Flags → ClickOptions / DragOptions round-trip ───────────────────────

    #[test]
    fn build_click_options_defaults() {
        let opts = Opts::default();
        let co = build_click_options(&opts).unwrap();
        assert!(matches!(co.button, MouseButton::Left));
        assert_eq!(co.count, 1);
        assert!(co.held.is_empty());
        assert!(matches!(co.anchor, Anchor::Center));
    }

    #[test]
    fn build_click_options_from_parsed_args() {
        let args = strs(&["--button", "right", "--count", "3", "--held", "Shift,Meta"]);
        let (opts, _) = parse_opts(&args);
        let co = build_click_options(&opts).unwrap();
        assert!(matches!(co.button, MouseButton::Right));
        assert_eq!(co.count, 3);
        assert_eq!(co.held.len(), 2);
        assert!(matches!(co.held[0], Key::Shift));
        assert!(matches!(co.held[1], Key::Meta));
    }

    #[test]
    fn build_click_options_bad_button_errors() {
        let args = strs(&["--button", "nope"]);
        let (opts, _) = parse_opts(&args);
        assert!(build_click_options(&opts).is_err());
    }

    #[test]
    fn build_click_options_bad_held_errors() {
        let args = strs(&["--held", "NotAKey"]);
        let (opts, _) = parse_opts(&args);
        assert!(build_click_options(&opts).is_err());
    }

    #[test]
    fn build_drag_options_defaults_150ms() {
        let opts = Opts::default();
        let d = build_drag_options(&opts).unwrap();
        assert!(matches!(d.button, MouseButton::Left));
        assert!(d.held.is_empty());
        assert_eq!(d.duration, Duration::from_millis(150));
    }

    #[test]
    fn build_drag_options_from_parsed_args() {
        let args = strs(&[
            "--button",
            "middle",
            "--held",
            "Ctrl",
            "--duration-ms",
            "500",
        ]);
        let (opts, _) = parse_opts(&args);
        let d = build_drag_options(&opts).unwrap();
        assert!(matches!(d.button, MouseButton::Middle));
        assert_eq!(d.held.len(), 1);
        assert!(matches!(d.held[0], Key::Ctrl));
        assert_eq!(d.duration, Duration::from_millis(500));
    }
}
