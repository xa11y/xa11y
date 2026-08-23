// CLI implementation for `xa11y` — accessibility tree explorer.
//
// This module is `#[doc(hidden)]` and not part of the public API.
// It powers both `cargo install xa11y` and `pip install xa11y` via PyO3.

use std::time::Duration;

use crate::*;

/// CLI-level error, separating usage mistakes from operation failures so the
/// binary can map them to distinct exit codes.
///
/// Exit code contract (implemented in `bin/xa11y.rs`, documented in the CLI
/// help text):
/// - `0` — success
/// - `1` — operation failed (app not found, no selector match, platform error)
/// - `2` — usage / argument error (unknown flag value, missing or invalid argument)
#[derive(Debug)]
pub enum CliError {
    /// Invalid command-line usage — exit code 2.
    Usage(String),
    /// `find` matched no elements — exit code 1. Kept distinct from `Usage`
    /// so scripts can tell "ran fine but found nothing" from a bad invocation.
    NotFound(String),
    /// A selector matched several elements where the caller's contract is
    /// exactly one — exit code 1.
    ///
    /// Constructed by the MCP `action` tool, whose schema promises "must match
    /// exactly one element". `xa11y action` keeps the library's document-order
    /// first-match semantics, which its own reference documents; the MCP tool
    /// cannot, because a model that is told "exactly one" and silently gets
    /// the first of several has no way to notice.
    ///
    /// The [`Diagnosis`] carries the selector, the observed match count, and a
    /// bounded candidate list, so the recovery — an attribute filter or
    /// `:nth(n)` — is readable straight off the error (tenet 6).
    Ambiguous {
        /// How many elements the selector matched.
        count: usize,
        /// Selector, match count, and bounded candidate list.
        diagnosis: Box<Diagnosis>,
    },
    /// Several shell surfaces of one kind are present where the caller's
    /// contract is exactly one — exit code 1.
    ///
    /// Kept distinct from [`CliError::Ambiguous`] because the recovery is a
    /// different one: a selector is narrowed with an attribute filter or
    /// `:nth(n)`, whereas a shell surface is picked by process id. MCP maps
    /// the two to distinct failure kinds (`ambiguous_selector` and
    /// `ambiguous_shell_surface`) for the same reason.
    ///
    /// The [`Diagnosis`] carries the candidates — kind, name and pid of every
    /// surface that matched — so the disambiguating pid is readable straight
    /// off the error (tenet 6).
    AmbiguousShellSurface {
        /// How many shell surfaces matched.
        count: usize,
        /// The kind that was asked for, in its snake_case spelling.
        kind: String,
        /// Candidate list and what was observed.
        diagnosis: Box<Diagnosis>,
    },
    /// An underlying xa11y operation failed — exit code 1.
    Xa11y(Error),
}

impl CliError {
    /// Process exit code for this error. See the contract on [`CliError`].
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Usage(_) => 2,
            CliError::NotFound(_)
            | CliError::Ambiguous { .. }
            | CliError::AmbiguousShellSurface { .. }
            | CliError::Xa11y(_) => 1,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(msg) => write!(f, "usage error: {msg}"),
            CliError::NotFound(msg) => write!(f, "{msg}"),
            CliError::Ambiguous { count, diagnosis } => write!(
                f,
                "Ambiguous selector: matched {count} elements, but this operation acts on \
                 exactly one; narrow it with an attribute filter (e.g. [name=\"…\"]) or pick \
                 one with :nth(n), 1-based{diagnosis}"
            ),
            // Both argument spellings are named because this one message is
            // rendered on both surfaces: `--pid` for the command line, `pid`
            // for an MCP caller, who has no flags at all.
            CliError::AmbiguousShellSurface {
                count,
                kind,
                diagnosis,
            } => write!(
                f,
                "Ambiguous shell surface: {count} {kind} surfaces are present, but this \
                 operation targets exactly one; pick one by process id — `--pid PID` on \
                 the command line, the `pid` argument in MCP{diagnosis}"
            ),
            CliError::Xa11y(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::Xa11y(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for CliError {
    fn from(e: Error) -> Self {
        CliError::Xa11y(e)
    }
}

/// Result alias for CLI operations.
pub type CliResult<T> = std::result::Result<T, CliError>;

/// Run the CLI, render any failure to stderr, and return the process exit code.
///
/// This is the entry point every launcher uses — the `xa11y` binary, Python's
/// `xa11y` console script, and the Node `xa11y` bin — so the exit-code
/// contract on [`CliError`] and the error-message formatting are defined once
/// rather than re-implemented per language. They had already drifted: the
/// Python wrapper mapped every failure to exit 1, losing the documented
/// `2` for usage errors, and prefixed usage errors twice ("error: usage
/// error: ...").
///
/// Writes only to stderr, so it is safe to call for `xa11y mcp`, whose stdout
/// carries protocol messages.
pub fn run_main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            // `CliError`'s Display already prefixes usage errors with
            // "usage error: "; everything else gets the generic prefix.
            match &e {
                CliError::Usage(_) => eprintln!("{e}"),
                _ => eprintln!("error: {e}"),
            }
            e.exit_code()
        }
    }
}

/// Run the CLI with the given arguments (excluding the program name).
///
/// Returns `Ok(())` on success, or an `Err` with a human-readable message
/// on failure. Most callers want [`run_main`], which renders the error and
/// produces the exit code.
pub fn run(args: &[String]) -> CliResult<()> {
    match args.first().map(|s| s.as_str()) {
        Some("apps") => cmd_apps(),
        Some("shell") => cmd_shell(&args[1..]),
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
        Some("mcp") => crate::mcp::serve(&args[1..]),
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

Accessibility tree:
  xa11y apps                                List running applications
  xa11y shell                               List OS shell surfaces: KIND PID NAME
  xa11y tree   [TARGET]                     Print the accessibility tree
  xa11y find   SELECTOR [TARGET] [-o pretty|bounds|center]
                                            Find elements matching a selector
  xa11y action ACTION SELECTOR [TARGET] [--value V]
                                            Perform an action on an element
  xa11y events [--app NAME | --pid PID]     Stream accessibility events

TARGET — what tree/find/action search (--shell and --app are exclusive):
  --app NAME | --pid PID                    A running application
  --shell KIND [--pid PID]                  An OS shell surface, as listed by
                                            `xa11y shell`. Add --pid when
                                            several surfaces share a kind: it
                                            is the only disambiguator, and it
                                            cannot separate several surfaces
                                            owned by one process (two panel
                                            rows from one xfce4-panel).
      KIND is one of:
      {kinds}

Input simulation (coords only — no selectors, no a11y):
  xa11y click  --at X,Y [--button left|right|middle] [--count N] [--held K,K]
  xa11y move   --at X,Y
  xa11y drag   --from X,Y --to X,Y [--button B] [--duration-ms MS] [--held K,K]
  xa11y scroll --at X,Y [--dx N] [--dy N]
  xa11y key    KEY [--held K,K]
  xa11y type   TEXT

Screenshot (pixels; --annotate adds selectors and a11y):
  xa11y screenshot [--region X,Y,W,H] --out PATH
                   [--app NAME | --pid PID | --shell KIND]
                   [--annotate SELECTOR]... [--legend text|json|none]
                                            --out - writes PNG bytes to stdout.
                                            With no --annotate: a plain capture,
                                            no target, no a11y.
  Each --annotate is one group: it boxes every element its selector matches in
  TARGET, in that group's colour, and the legend on stdout maps each box's tag
  to a selector that acts on it (A7 -> button:nth(7)). Repeat for more groups.
  Boxes come from the accessibility tree, so --annotate needs a target and gains
  its failures (app not found, no match); an app with no tree gets no boxes.
  --out - and a legend both want stdout: pick --out FILE, or --legend none.

Model Context Protocol:
  xa11y mcp                                 Serve the above as MCP tools over
                                            stdio (for MCP clients, not humans)

Compose a11y + input/screenshot via `find -o bounds|center`:
  region=$(xa11y find 'button[name=\"OK\"]' --app Safari -o bounds)
  xa11y screenshot --region \"$region\" --out button.png
  xa11y click --at \"$(xa11y find 'button[name=\"OK\"]' --app Safari -o center)\"

Actions: press, focus, blur, toggle, expand, collapse, select, show-menu,
  scroll-into-view, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  select-text (requires --value START,END)

Exit codes:
  0  success
  1  operation failed (app not found, no selector match, platform error)
  2  usage error (unknown flag value, missing or invalid argument)",
        // Derived, not spelled out: see `shell_kind_names`.
        kinds = shell_kind_names().join(", ")
    );
}

// ── Argument helpers ────────────────────────────────────────────────────────

// Debug is needed so tests can `expect_err` on `parse_opts` results.
#[derive(Debug, Default)]
pub(crate) struct Opts {
    pub app: Option<String>,
    pub pid: Option<u32>,
    /// Shell surface kind, as its snake_case spelling. Mutually exclusive with
    /// `app`; combines with `pid` to disambiguate same-kind surfaces.
    pub shell: Option<String>,
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
    /// One selector per `--annotate` occurrence, in the order given. Each is
    /// one annotation *group*: it gets its own colour and tag letter, so the
    /// flag collects repeats rather than the last one winning.
    pub annotate: Vec<String>,
    /// Raw `--legend` value, parsed by [`parse_legend_format`].
    pub legend: Option<String>,
    // Output format for `find`
    pub output_format: Option<String>,
}

/// Fetch the value for a flag at `args[i]`, erroring if the flag is trailing
/// with no value (previously silently treated as absent — tenet 1).
fn flag_value<'a>(args: &'a [String], i: usize, flag: &str) -> CliResult<&'a str> {
    args.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| CliError::Usage(format!("{flag} requires a value")))
}

/// Fetch and parse the value for a flag at `args[i]`, erroring with a clear
/// message if the value doesn't parse (previously `--pid abc` was silently
/// treated as absent — tenet 1).
fn flag_value_parsed<T: std::str::FromStr>(
    args: &[String],
    i: usize,
    flag: &str,
    expected: &str,
) -> CliResult<T> {
    let raw = flag_value(args, i, flag)?;
    raw.parse().map_err(|_| {
        CliError::Usage(format!(
            "invalid {flag} value '{raw}' (expected {expected})"
        ))
    })
}

/// Parse known flags from a slice, returning the parsed Opts and the
/// remaining positional arguments.
///
/// Unknown flags are left in the positional output (so downstream callers
/// see them and can surface a sensible error) rather than swallowed. Known
/// flags require a value: a trailing flag or an unparsable numeric value is
/// a usage error, not a silently-absent option.
pub(crate) fn parse_opts(args: &[String]) -> CliResult<(Opts, Vec<String>)> {
    let mut opts = Opts::default();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                i += 1;
                opts.app = Some(flag_value(args, i, "--app")?.to_string());
            }
            "--pid" => {
                i += 1;
                opts.pid = Some(flag_value_parsed(
                    args,
                    i,
                    "--pid",
                    "an integer process id",
                )?);
            }
            "--shell" => {
                i += 1;
                opts.shell = Some(flag_value(args, i, "--shell")?.to_string());
            }
            "--value" => {
                i += 1;
                opts.value = Some(flag_value(args, i, "--value")?.to_string());
            }
            "--at" => {
                i += 1;
                opts.at = Some(flag_value(args, i, "--at")?.to_string());
            }
            "--from" => {
                i += 1;
                opts.from = Some(flag_value(args, i, "--from")?.to_string());
            }
            "--to" => {
                i += 1;
                opts.to = Some(flag_value(args, i, "--to")?.to_string());
            }
            "--button" => {
                i += 1;
                opts.button = Some(flag_value(args, i, "--button")?.to_string());
            }
            "--count" => {
                i += 1;
                opts.count = Some(flag_value_parsed(args, i, "--count", "a positive integer")?);
            }
            "--held" => {
                i += 1;
                opts.held = Some(flag_value(args, i, "--held")?.to_string());
            }
            "--dx" => {
                i += 1;
                opts.dx = Some(flag_value_parsed(args, i, "--dx", "an integer")?);
            }
            "--dy" => {
                i += 1;
                opts.dy = Some(flag_value_parsed(args, i, "--dy", "an integer")?);
            }
            "--duration-ms" => {
                i += 1;
                opts.duration_ms = Some(flag_value_parsed(
                    args,
                    i,
                    "--duration-ms",
                    "milliseconds as an integer",
                )?);
            }
            "--region" => {
                i += 1;
                opts.region = Some(flag_value(args, i, "--region")?.to_string());
            }
            "--out" => {
                i += 1;
                opts.out = Some(flag_value(args, i, "--out")?.to_string());
            }
            // The one repeatable flag: each occurrence is a distinct
            // annotation group, so a later one must not overwrite an earlier.
            "--annotate" => {
                i += 1;
                opts.annotate
                    .push(flag_value(args, i, "--annotate")?.to_string());
            }
            "--legend" => {
                i += 1;
                opts.legend = Some(flag_value(args, i, "--legend")?.to_string());
            }
            "-o" => {
                i += 1;
                opts.output_format = Some(flag_value(args, i, "-o")?.to_string());
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    Ok((opts, positional))
}

// ── Parsers for complex flag values ─────────────────────────────────────────

fn missing(what: &str) -> CliError {
    CliError::Usage(format!("missing {what}"))
}

pub(crate) fn parse_point_arg(s: &str, ctx: &str) -> CliResult<Point> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return Err(CliError::Usage(format!("{ctx} must be X,Y (got: {s})")));
    }
    let x: i32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid X in {ctx}: {}", parts[0])))?;
    let y: i32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid Y in {ctx}: {}", parts[1])))?;
    Ok(Point::new(x, y))
}

pub(crate) fn parse_region_arg(s: &str) -> CliResult<Rect> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(CliError::Usage(format!(
            "--region must be X,Y,W,H (got: {s})"
        )));
    }
    let x: i32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid X in --region: {}", parts[0])))?;
    let y: i32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid Y in --region: {}", parts[1])))?;
    let width: u32 = parts[2]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid W in --region: {}", parts[2])))?;
    let height: u32 = parts[3]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid H in --region: {}", parts[3])))?;
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
pub(crate) fn parse_key_name(name: &str) -> CliResult<Key> {
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
            let n: u8 = s[1..]
                .parse()
                .map_err(|_| CliError::Usage(format!("invalid function key: {s}")))?;
            Key::F(n)
        }
        s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap()),
        _ => {
            return Err(CliError::Usage(format!("unknown key name: {name}")));
        }
    };
    Ok(k)
}

pub(crate) fn parse_held(raw: Option<&str>) -> CliResult<Vec<Key>> {
    match raw {
        None => Ok(Vec::new()),
        Some("") => Ok(Vec::new()),
        Some(s) => s.split(',').map(|p| parse_key_name(p.trim())).collect(),
    }
}

pub(crate) fn parse_button(raw: &str) -> CliResult<MouseButton> {
    match raw {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(CliError::Usage(format!(
            "unknown button: {other} (expected left|right|middle)"
        ))),
    }
}

pub(crate) fn resolve_app(opts: &Opts) -> CliResult<App> {
    if let Some(name) = &opts.app {
        Ok(App::by_name(name, std::time::Duration::ZERO)?)
    } else if let Some(pid) = opts.pid {
        Ok(App::by_pid(pid, std::time::Duration::ZERO)?)
    } else {
        Err(CliError::Usage("specify --app NAME or --pid PID".into()))
    }
}

// ── Shell surface targeting ─────────────────────────────────────────────────

/// Every [`ShellSurfaceKind`] spelling `--shell` (MCP: `shell`) accepts.
///
/// Single source of truth for the flag's error message, the usage text, and
/// the MCP schema's `enum`, so a kind cannot be advertised by one surface and
/// rejected by another. **Derived** from [`ShellSurfaceKind::ALL`] rather than
/// written out again: `ShellSurfaceKind` is `#[non_exhaustive]`, so a `match`
/// here could not fail to compile when a variant is added, and a hand-written
/// list would silently keep advertising eight kinds out of nine. Core's
/// `every_variant_is_in_all` test is the exhaustive `match` that guards `ALL`.
pub(crate) fn shell_kind_names() -> &'static [&'static str] {
    static NAMES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        ShellSurfaceKind::ALL
            .iter()
            .map(|k| k.to_snake_case())
            .collect()
    });
    NAMES.as_slice()
}

/// Longest candidate list carried in a shell-lookup failure. Bounded per
/// tenet 6 — diagnostics must not grow with the environment.
const MAX_SHELL_CANDIDATES: usize = 20;

/// Parse a `--shell` / `shell` value into a [`ShellSurfaceKind`].
///
/// Runs before any enumeration, so a misspelled kind is a usage error rather
/// than a listing the caller has to read to discover the typo.
pub(crate) fn parse_shell_kind(raw: &str) -> CliResult<ShellSurfaceKind> {
    ShellSurfaceKind::from_snake_case(raw).ok_or_else(|| {
        CliError::Usage(format!(
            "unknown shell surface kind: {raw} (expected one of: {})",
            shell_kind_names().join(", ")
        ))
    })
}

/// Bounded `kind "name" (pid=N)` rendering of the surfaces that were present.
fn describe_shell_surfaces(surfaces: &[ShellSurface]) -> Vec<String> {
    bound_candidates(
        surfaces
            .iter()
            .map(|s| {
                let pid = s.pid.map(|p| format!(" (pid={p})")).unwrap_or_default();
                format!("{} \"{}\"{pid}", s.kind.to_snake_case(), s.name)
            })
            .collect(),
    )
}

/// Cap a candidate list at [`MAX_SHELL_CANDIDATES`], naming how many were
/// dropped.
///
/// Split out from the rendering so the cap is testable without a provider:
/// a diagnosis that grows with the environment is the tenet-6 failure that
/// costs the success path nothing and the failure path everything.
fn bound_candidates(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > MAX_SHELL_CANDIDATES {
        let dropped = lines.len() - MAX_SHELL_CANDIDATES;
        lines.truncate(MAX_SHELL_CANDIDATES);
        lines.push(format!("… (+{dropped} more)"));
    }
    lines
}

/// Resolve the one shell surface of `kind` (optionally owned by `pid`).
///
/// A single enumeration attempt, like [`resolve_app`]: these surfaces are
/// either on screen or they are not, and a caller who wants to *wait* for one
/// (the tray-overflow flyout, after pressing the chevron) uses
/// `ShellSurface::by_kind` with a timeout. The lookup is written here rather
/// than delegated to `by_kind`, which has no pid filter and reports ambiguity
/// and absence through the same error — this surface must tell them apart, so
/// a caller is told either "add a pid" or "here is what is on screen".
///
/// # Errors
///
/// - [`CliError::Usage`] for an unknown kind name.
/// - [`CliError::AmbiguousShellSurface`] when several surfaces match.
/// - [`CliError::Xa11y`] with [`Error::SelectorNotMatched`] when none do, its
///   diagnosis naming every surface that *was* enumerated.
pub(crate) fn resolve_shell_surface(kind_raw: &str, pid: Option<u32>) -> CliResult<ShellSurface> {
    let kind = parse_shell_kind(kind_raw)?;
    select_shell_surface(ShellSurface::list()?, kind, pid)
}

/// Pick the one surface of `kind` (optionally owned by `pid`) out of an
/// already-enumerated listing.
///
/// Split from [`resolve_shell_surface`] so the selection — which of match,
/// ambiguity and absence a listing produces, and which hint each failure
/// carries — is testable without a desktop. `resolve_shell_surface` is then
/// just the singleton `ShellSurface::list()` call plus this.
///
/// # Errors
///
/// - [`CliError::AmbiguousShellSurface`] when several surfaces match. The
///   diagnosis's hint depends on whether `pid` was already given: without one
///   it points at `xa11y shell`; with one it says the operation cannot pick
///   between them, because `pid` is the only lever there is and it does not
///   separate surfaces owned by a single process.
/// - [`CliError::Xa11y`] with [`Error::SelectorNotMatched`] when none do, its
///   diagnosis naming every surface that *was* enumerated.
pub(crate) fn select_shell_surface(
    surfaces: Vec<ShellSurface>,
    kind: ShellSurfaceKind,
    pid: Option<u32>,
) -> CliResult<ShellSurface> {
    let selector = match pid {
        Some(p) => format!("shell_surface[kind={kind}][pid={p}]"),
        None => format!("shell_surface[kind={kind}]"),
    };

    let (mut matched, others): (Vec<ShellSurface>, Vec<ShellSurface>) = surfaces
        .into_iter()
        .partition(|s| s.kind == kind && pid.is_none_or(|p| s.pid == Some(p)));

    if matched.len() > 1 {
        let hint = match pid {
            // A pid was already given and did not narrow it: saying "add a
            // pid" would send the caller somewhere they have been. There is
            // no second lever — one process can own several surfaces of one
            // kind (two xfce4-panel rows), and nothing distinguishes them.
            Some(p) => format!(
                "{} {kind} surfaces share pid {p}; pid is the only disambiguator, so this \
                 operation cannot pick between them",
                matched.len()
            ),
            None => format!(
                "{} {kind} surfaces are present; `xa11y shell` lists their pids",
                matched.len()
            ),
        };
        return Err(CliError::AmbiguousShellSurface {
            count: matched.len(),
            kind: kind.to_snake_case().to_string(),
            diagnosis: Box::new(
                Diagnosis::new()
                    .condition(format!("exactly one {kind} shell surface"))
                    .last_observed(hint)
                    .candidates(describe_shell_surfaces(&matched)),
            ),
        });
    }

    matched.pop().ok_or_else(|| {
        let observed = match pid {
            Some(p) => format!("no {kind} surface with pid {p} is present"),
            None => format!("no {kind} surface is present"),
        };
        CliError::Xa11y(
            Error::selector_not_matched(selector).diagnose(
                Diagnosis::new()
                    .condition(format!("a {kind} shell surface"))
                    .last_observed(format!(
                        "{observed}; {} other shell surface(s) enumerated",
                        others.len()
                    ))
                    .candidates(describe_shell_surfaces(&others)),
            ),
        )
    })
}

/// What `tree`, `find` and `action` search: a running application, or an OS
/// shell surface.
///
/// Value-producing, so the MCP handlers can share the resolution without
/// reaching for a `cmd_*` function — those print, and on the stdio transport
/// stdout carries protocol messages only.
#[derive(Debug)]
pub(crate) enum Target {
    /// A running application, from `--app` / `--pid`.
    App(App),
    /// An OS shell surface, from `--shell` (plus `--pid` to disambiguate).
    Shell(ShellSurface),
}

impl Target {
    /// A [`Locator`] rooted at the target.
    pub(crate) fn locator(&self, selector: &str) -> Locator {
        match self {
            Target::App(app) => app.locator(selector),
            Target::Shell(surface) => surface.locator(selector),
        }
    }

    /// The target's root element.
    pub(crate) fn root(&self) -> Element {
        match self {
            Target::App(app) => Element::new(app.data.clone(), app.provider().clone()),
            Target::Shell(surface) => surface.as_element(),
        }
    }

    /// The target's human-readable name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Target::App(app) => &app.name,
            Target::Shell(surface) => &surface.name,
        }
    }

    /// The owning process, where the platform reports one.
    pub(crate) fn pid(&self) -> Option<u32> {
        match self {
            Target::App(app) => app.pid,
            Target::Shell(surface) => surface.pid,
        }
    }

    /// The shell surface behind this target, if it is one.
    pub(crate) fn shell(&self) -> Option<&ShellSurface> {
        match self {
            Target::App(_) => None,
            Target::Shell(surface) => Some(surface),
        }
    }
}

/// Resolve the target of a `tree` / `find` / `action` invocation.
///
/// `--shell` and `--app` name two different things to search, so passing both
/// is a usage error rather than one of them silently winning.
pub(crate) fn resolve_target(opts: &Opts) -> CliResult<Target> {
    match (&opts.shell, &opts.app) {
        (Some(_), Some(_)) => Err(CliError::Usage(
            "--shell and --app are mutually exclusive: --shell targets an OS shell surface, \
             --app a running application. Use --pid alongside --shell to pick between \
             surfaces of one kind."
                .into(),
        )),
        (Some(kind), None) => Ok(Target::Shell(resolve_shell_surface(kind, opts.pid)?)),
        (None, _) => Ok(Target::App(resolve_app(opts)?)),
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
    if el.states.active {
        states.push("active");
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

fn cmd_apps() -> CliResult<()> {
    let apps = App::list()?;
    if apps.is_empty() {
        println!("No applications found.");
        return Ok(());
    }
    // Columns are `pid\tname`; the foreground app gets a trailing `focused`
    // field (App::list tags it via the platform's foreground query). Keeping
    // pid/name as columns 1-2 preserves the output contract for scripts that
    // parse `xa11y apps` by column position. The printed token stays `focused`
    // (a stable, documented output contract); the API name is `is_foreground`.
    for app in &apps {
        let pid_str = app.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let foreground = if app.is_foreground() { "\tfocused" } else { "" };
        println!("{}\t{}{}", pid_str, app.name, foreground);
    }
    Ok(())
}

/// List the OS shell surfaces currently on screen.
///
/// Columns are `kind\tpid\tname`, mirroring `xa11y apps`' tab-separated
/// contract: the kind leads because it is what `--shell` takes, and the pid
/// keeps its `-` for a surface the platform attributes to no process.
fn cmd_shell(args: &[String]) -> CliResult<()> {
    // The listing takes no filters. Ignoring `--shell taskbar` here would let
    // someone believe it had narrowed the output (tenet 1); the flag belongs
    // on tree / find / action.
    if let Some(first) = args.first() {
        return Err(CliError::Usage(format!(
            "xa11y shell takes no arguments (got: {first}). It lists every surface; \
             pass --shell KIND to tree, find or action to target one."
        )));
    }
    let surfaces = ShellSurface::list()?;
    if surfaces.is_empty() {
        println!("No shell surfaces found.");
        return Ok(());
    }
    for surface in &surfaces {
        let pid_str = surface
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{}\t{}\t{}",
            surface.kind.to_snake_case(),
            pid_str,
            surface.name
        );
    }
    Ok(())
}

fn cmd_tree(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
    let target = resolve_target(&opts)?;
    print_tree_recursive(&target.root(), "", true, true);
    Ok(())
}

fn cmd_find(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_opts(args)?;
    let selector = positional.first().ok_or_else(|| {
        CliError::Usage(
            "usage: xa11y find SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [-o pretty|bounds|center]"
                .into(),
        )
    })?;

    let target = resolve_target(&opts)?;
    let elements = target.locator(selector).elements()?;
    if elements.is_empty() {
        return Err(CliError::NotFound(format!(
            "no elements matched selector: {selector}"
        )));
    }
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
                match format_bounds_opt(el) {
                    Some(line) => println!("{line}"),
                    None => warn_skipped_no_bounds(el),
                }
            }
        }
        "center" => {
            for el in &elements {
                match format_center_opt(el) {
                    Some(line) => println!("{line}"),
                    None => warn_skipped_no_bounds(el),
                }
            }
        }
        other => {
            return Err(CliError::Usage(format!(
                "unknown -o format: {other} (expected pretty|bounds|center)"
            )));
        }
    }
    Ok(())
}

/// Tell the user (on stderr) that a matched element was omitted from
/// `-o bounds` / `-o center` output because it has no bounds, so the line
/// count stays explicable against the match count.
fn warn_skipped_no_bounds(el: &ElementData) {
    eprintln!(
        "warning: skipping {} \"{}\": element has no bounds",
        el.role.to_snake_case(),
        el.name.as_deref().unwrap_or("(unnamed)")
    );
}

/// Format an element's bounds as `X,Y,W,H` — the input to `--region`.
// Used in unit tests below; production callers use `format_bounds_opt`.
#[allow(dead_code)]
pub(crate) fn format_bounds_line(el: &ElementData) -> Result<String> {
    let b = el.bounds.ok_or(Error::NoElementBounds)?;
    Ok(format!("{},{},{},{}", b.x, b.y, b.width, b.height))
}

/// Format an element's bounds as `X,Y,W,H`, returning None if bounds are absent.
fn format_bounds_opt(el: &ElementData) -> Option<String> {
    let b = el.bounds?;
    Some(format!("{},{},{},{}", b.x, b.y, b.width, b.height))
}

/// Format the center of an element's bounds as `X,Y` — the input to `--at`.
// Used in unit tests below; production callers use `format_center_opt`.
#[allow(dead_code)]
pub(crate) fn format_center_line(el: &ElementData) -> Result<String> {
    let b = el.bounds.ok_or(Error::NoElementBounds)?;
    let cx = b.x + (b.width as i32) / 2;
    let cy = b.y + (b.height as i32) / 2;
    Ok(format!("{cx},{cy}"))
}

/// Format the center of an element's bounds as `X,Y`, returning None if bounds are absent.
fn format_center_opt(el: &ElementData) -> Option<String> {
    let b = el.bounds?;
    let cx = b.x + (b.width as i32) / 2;
    let cy = b.y + (b.height as i32) / 2;
    Some(format!("{cx},{cy}"))
}

fn cmd_action(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_opts(args)?;
    if positional.len() < 2 {
        return Err(CliError::Usage(
            "usage: xa11y action ACTION SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [--value V]"
                .into(),
        ));
    }
    let action_name = &positional[0];
    let selector = &positional[1];
    let value = opts.value.clone();

    let target = resolve_target(&opts)?;
    let locator = target.locator(selector);
    perform_action(&locator, action_name, value.as_deref())?;
    println!("ok");
    Ok(())
}

/// Every action verb `xa11y action` and the MCP `action` tool accept.
///
/// Single source of truth: the CLI's unknown-action error and the MCP tool's
/// `inputSchema` enum both read this, so a verb added to [`perform_action`]
/// cannot be advertised by one surface and rejected by the other.
pub(crate) const ACTION_NAMES: &[&str] = &[
    "press",
    "focus",
    "blur",
    "toggle",
    "expand",
    "collapse",
    "select",
    "show-menu",
    "scroll-into-view",
    "increment",
    "decrement",
    "set-value",
    "set-numeric-value",
    "type-text",
    "select-text",
];

/// Actions that require a `--value` (MCP: `value`) argument.
pub(crate) const ACTIONS_REQUIRING_VALUE: &[&str] =
    &["set-value", "set-numeric-value", "type-text", "select-text"];

/// Dispatch a named action verb onto `locator`.
///
/// Shared by `xa11y action` and the MCP `action` tool so the two cannot drift
/// on which verbs exist or which of them need a value. Writes nothing to
/// stdout: the MCP stdio transport allows only protocol messages there, so
/// the "ok" line stays in the CLI half.
pub(crate) fn perform_action(
    locator: &Locator,
    action_name: &str,
    value: Option<&str>,
) -> CliResult<()> {
    // Each `requires --value` arm re-checks rather than trusting a caller to
    // have consulted ACTIONS_REQUIRING_VALUE (tenet 1: the failure is
    // surfaced where it happens, not assumed away upstream).
    let need_value = |verb: &str| -> CliResult<&str> {
        value.ok_or_else(|| CliError::Usage(format!("{verb} requires a value")))
    };

    // The dispatch runs inside a closure so every arm's failure passes through
    // `relabel_action_error` on the way out, and the verb spelling a caller is
    // told about is the one this function accepts.
    let dispatch = || -> CliResult<()> {
        match action_name {
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
            "set-value" => locator.set_value(need_value("set-value")?)?,
            "set-numeric-value" => {
                // Parsed before the locator is touched, so a bad number cannot
                // burn the auto-wait timeout or reach the platform call.
                let v = parse_numeric_value(need_value("set-numeric-value")?)?;
                locator.set_numeric_value(v)?;
            }
            "type-text" => locator.type_text(need_value("type-text")?)?,
            "select-text" => {
                let v = need_value("select-text")?;
                let (start, end) = parse_text_range(v)?;
                locator.select_text(start, end)?;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown action: {other} (expected one of: {})",
                    ACTION_NAMES.join(", ")
                )));
            }
        }
        Ok(())
    };

    dispatch().map_err(|e| relabel_action_error(e, action_name))
}

/// Parse a `--value` for `set-numeric-value`.
///
/// Rejects unparsable and non-finite input here rather than letting it reach
/// the provider: the CLI and the MCP tool both take this value as a string,
/// and "parse arguments before the first OS call" is what keeps a bad number
/// from spending the auto-wait timeout before failing.
pub(crate) fn parse_numeric_value(raw: &str) -> CliResult<f64> {
    let parsed: f64 = raw.trim().parse().map_err(|_| {
        CliError::Usage(format!(
            "set-numeric-value value must be a number (e.g. 88 or 0.5), got: {raw}"
        ))
    })?;
    if !parsed.is_finite() {
        return Err(CliError::Usage(format!(
            "set-numeric-value value must be finite, got: {raw}"
        )));
    }
    Ok(parsed)
}

/// Re-spell the action name in an `ActionNotSupported` as the verb the caller
/// typed.
///
/// Providers report the failing action by its Rust method name
/// (`show_menu`, `scroll_into_view`), so the error told the user to use a
/// spelling `xa11y action` and the MCP `action` tool both reject. Only the
/// name is rewritten, and only when it is the same verb modulo the separator
/// — a provider naming some *other* action passes through untouched, because
/// that difference is information, not noise.
fn relabel_action_error(err: CliError, verb: &str) -> CliError {
    match err {
        CliError::Xa11y(Error::ActionNotSupported { action, role })
            if action.replace('_', "-") == verb =>
        {
            CliError::Xa11y(Error::ActionNotSupported {
                action: verb.to_string(),
                role,
            })
        }
        other => other,
    }
}

/// Parse a `START,END` character range for `select-text`.
pub(crate) fn parse_text_range(raw: &str) -> CliResult<(u32, u32)> {
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != 2 {
        return Err(CliError::Usage(format!(
            "select-text value must be START,END (e.g. 0,5), got: {raw}"
        )));
    }
    let start: u32 = parts[0].trim().parse().map_err(|_| {
        CliError::Usage(format!("invalid START in select-text value: {}", parts[0]))
    })?;
    let end: u32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid END in select-text value: {}", parts[1])))?;
    Ok((start, end))
}

fn cmd_events(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
    // Events are subscribed per application; there is no surface-level event
    // story yet (see design/shell-surfaces/PROPOSAL.md §10). Saying so beats
    // letting `--shell` fall through to "specify --app NAME or --pid PID",
    // which reads as though the flag were misspelled.
    if opts.shell.is_some() {
        return Err(CliError::Usage(
            "--shell is not supported by `events`: accessibility events are subscribed \
             per application. Use --app NAME or --pid PID."
                .into(),
        ));
    }
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
        println!("[{}] {target_str}{detail}", format_event_kind(&event.kind));
    }
    Ok(())
}

/// Human-readable name for an event kind, matching the snake_case style the
/// rest of the CLI output uses for roles (e.g. `focus_changed`, not the Rust
/// debug form `FocusChanged`).
pub(crate) fn format_event_kind(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::FocusChanged => "focus_changed",
        EventKind::ValueChanged => "value_changed",
        EventKind::NameChanged => "name_changed",
        EventKind::StateChanged { .. } => "state_changed",
        EventKind::StructureChanged => "structure_changed",
        EventKind::WindowOpened => "window_opened",
        EventKind::WindowClosed => "window_closed",
        EventKind::WindowActivated => "window_activated",
        EventKind::WindowDeactivated => "window_deactivated",
        EventKind::SelectionChanged => "selection_changed",
        EventKind::MenuOpened => "menu_opened",
        EventKind::MenuClosed => "menu_closed",
        EventKind::TextChanged => "text_changed",
        EventKind::Announcement => "announcement",
        // `EventKind` is `#[non_exhaustive]`. A kind this build predates is
        // still worth printing as a line — the CLI is a debugging tool, and
        // dropping the event entirely would be worse than naming it vaguely.
        _ => "unknown",
    }
}

pub(crate) fn format_event_detail(event: &Event) -> String {
    if let EventKind::StateChanged { flag, value } = event.kind {
        format!(" {flag:?}={value}")
    } else {
        String::new()
    }
}

// ── Input simulation ────────────────────────────────────────────────────────

fn cmd_click(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
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
pub(crate) fn build_click_options(opts: &Opts) -> CliResult<ClickOptions> {
    let button = opts
        .button
        .as_deref()
        .map(parse_button)
        .transpose()?
        .unwrap_or(MouseButton::Left);
    let count = opts.count.unwrap_or(1);
    let held = parse_held(opts.held.as_deref())?;
    Ok(ClickOptions::new()
        .button(button)
        .count(count)
        .held(held)
        .anchor(Anchor::Center))
}

fn cmd_move(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
    let at = parse_point_arg(
        opts.at.as_deref().ok_or_else(|| missing("--at X,Y"))?,
        "--at",
    )?;
    let sim = crate::input_sim()?;
    sim.mouse().move_to(at)?;
    println!("ok");
    Ok(())
}

fn cmd_drag(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
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
pub(crate) fn build_drag_options(opts: &Opts) -> CliResult<DragOptions> {
    let button = opts
        .button
        .as_deref()
        .map(parse_button)
        .transpose()?
        .unwrap_or(MouseButton::Left);
    let held = parse_held(opts.held.as_deref())?;
    let duration = Duration::from_millis(opts.duration_ms.unwrap_or(150));
    Ok(DragOptions::new()
        .button(button)
        .held(held)
        .duration(duration))
}

fn cmd_scroll(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
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

fn cmd_key(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_opts(args)?;
    let name = positional
        .first()
        .ok_or_else(|| CliError::Usage("usage: xa11y key KEY [--held K,K]".into()))?;
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

fn cmd_type(args: &[String]) -> CliResult<()> {
    let (_opts, positional) = parse_opts(args)?;
    let text = positional
        .first()
        .ok_or_else(|| CliError::Usage("usage: xa11y type TEXT".into()))?;
    let sim = crate::input_sim()?;
    sim.keyboard().type_text(text)?;
    println!("ok");
    Ok(())
}

// ── Screenshot ──────────────────────────────────────────────────────────────

/// How `--legend` renders the annotation legend.
///
/// The legend is deliberately *out of band* — stdout text or JSON, never
/// composited into the image. Rendered text costs image area, changes the
/// output dimensions, and is strictly worse for a model than the same data as
/// JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegendFormat {
    /// Aligned columns for a human reading a terminal. The default.
    Text,
    /// One JSON object carrying the same information, for a script.
    Json,
    /// Print nothing. The boxes are still drawn.
    None,
}

/// Parse a `--legend` value. Runs before any capture, so a misspelling is a
/// usage error rather than something discovered after the pixels are written.
pub(crate) fn parse_legend_format(raw: &str) -> CliResult<LegendFormat> {
    match raw {
        "text" => Ok(LegendFormat::Text),
        "json" => Ok(LegendFormat::Json),
        "none" => Ok(LegendFormat::None),
        other => Err(CliError::Usage(format!(
            "unknown --legend value: {other} (expected text|json|none)"
        ))),
    }
}

/// Longest inline omission list in the text legend. Bounded per tenet 6 and
/// the MCP "results are bounded" rule: the full list is always available in
/// `--legend json`.
const MAX_LEGEND_OMISSION_DETAILS: usize = 5;

fn cmd_screenshot(args: &[String]) -> CliResult<()> {
    let (opts, _pos) = parse_opts(args)?;
    let out = opts
        .out
        .as_deref()
        .ok_or_else(|| missing("--out PATH (use - for stdout)"))?;
    let region = opts.region.as_deref().map(parse_region_arg).transpose()?;

    // Without --annotate this command is exactly what it was before the
    // feature existed: no target, no tree read, no legend.
    if opts.annotate.is_empty() {
        if let Some(raw) = opts.legend.as_deref() {
            return Err(CliError::Usage(format!(
                "--legend {raw} has nothing to describe: add --annotate SELECTOR, or drop \
                 --legend"
            )));
        }
        let shot = match region {
            Some(rect) => crate::screenshot_region(rect)?,
            None => crate::screenshot()?,
        };
        return write_screenshot(&shot, out);
    }

    // Every argument is validated before the first OS call, so a bad
    // invocation cannot leave a capture or a half-written file behind.
    let legend_format = match opts.legend.as_deref() {
        Some(raw) => parse_legend_format(raw)?,
        None => LegendFormat::Text,
    };
    if out == "-" && legend_format != LegendFormat::None {
        // PNG bytes and legend text cannot share one stream. Quietly moving
        // the legend to stderr would leave a caller piping a PNG somewhere and
        // never learning that what they asked for went elsewhere (tenet 1).
        return Err(CliError::Usage(
            "--out - writes PNG bytes to stdout, and the legend would corrupt them: write \
             the image to a file with --out FILE, or drop the legend with --legend none"
                .into(),
        ));
    }
    if opts.app.is_none() && opts.pid.is_none() && opts.shell.is_none() {
        return Err(CliError::Usage(
            "--annotate resolves selectors against a target, and this command has none: add \
             --app NAME, --pid PID, or --shell KIND"
                .into(),
        ));
    }

    let target = resolve_target(&opts)?;
    let groups: Vec<Locator> = opts.annotate.iter().map(|s| target.locator(s)).collect();
    let annotated = crate::screenshot_annotated(region, &groups)?;

    write_screenshot(&annotated.screenshot, out)?;
    match legend_format {
        LegendFormat::None => {}
        LegendFormat::Text => print!("{}", render_legend_text(&opts.annotate, &annotated)),
        LegendFormat::Json => println!("{}", render_legend_json(&opts.annotate, &annotated)?),
    }
    Ok(())
}

/// Write `shot` to `out` — a file, or stdout for `-`.
///
/// Split out of [`cmd_screenshot`] so the annotated and unannotated paths
/// cannot drift in how they emit the image or report it.
fn write_screenshot(shot: &Screenshot, out: &str) -> CliResult<()> {
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

/// The tag letter for a 1-based group — `A`, `B`, … `AA`.
///
/// Derived from `tag_for` rather than reimplemented, so the letter in the
/// header and the letter drawn in the image cannot disagree: `tag_for(g, 1)`
/// is the group's letters followed by `1`, and the letters are `A-Z` only.
fn group_letter(group: usize) -> String {
    screenshot::tag_for(group, 1)
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .to_string()
}

fn hex_color(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// A name for a legend column: quoted and escaped, or `-` when the element has
/// none. `-` rather than `""`, so a nameless element is distinguishable from
/// one whose name is the empty string.
fn legend_name(name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{n:?}"),
        None => "-".to_string(),
    }
}

fn legend_bounds(b: &Rect) -> String {
    format!("bounds={},{},{},{}", b.x, b.y, b.width, b.height)
}

/// The first group whose `annotated` count may be short because the
/// annotation cap bit, or `None` when nothing was truncated.
///
/// A group starved by the cap draws nothing, and a bare `0 annotated` reads as
/// "this selector matched nothing" — the opposite meaning. `truncated` is a
/// single total with no group attribution ([`crate::Annotated`] carries none),
/// so the honest answer is a boundary rather than a per-group count:
/// `screenshot_annotated` resolves groups in flag order and stops resolving
/// at the cap, so nothing after the last group present in the legend was
/// resolved to completion. Every group from there on is reported as possibly
/// short; the ones before it are exact.
///
/// Pure, so both renderings share one rule and cannot disagree about which
/// groups are suspect.
fn first_capped_group(annotated: &crate::Annotated) -> Option<usize> {
    if annotated.truncated == 0 {
        return None;
    }
    // No legend at all means the cap could have bitten in any group, so the
    // boundary is the first one.
    Some(annotated.legend.iter().map(|e| e.group).max().unwrap_or(1))
}

/// Render the human-readable legend: a group header block, one line per drawn
/// box, then what could not be drawn.
///
/// Pure and string-returning so the layout is testable without a display.
/// `selectors` is the `--annotate` list in flag order, which is what makes a
/// group with zero drawn boxes still appear in the header — the alternative
/// reads as if the flag was ignored.
fn render_legend_text(selectors: &[String], annotated: &crate::Annotated) -> String {
    let mut out = String::new();

    let letters: Vec<String> = (1..=selectors.len()).map(group_letter).collect();
    let letter_w = letters.iter().map(String::len).max().unwrap_or(1);
    let selector_w = selectors.iter().map(String::len).max().unwrap_or(0);
    let first_capped = first_capped_group(annotated);

    for (g, selector) in selectors.iter().enumerate() {
        let group = g + 1;
        let color = screenshot::ANNOTATION_PALETTE[g % screenshot::ANNOTATION_PALETTE.len()];
        let drawn = annotated.legend.iter().filter(|e| e.group == group).count();
        let note = match (first_capped.is_some_and(|first| group >= first), drawn) {
            (false, _) => "",
            // The case this exists for: without the note, a group the cap
            // starved is byte-identical to one whose selector matched
            // nothing.
            (true, 0) => "  (cap reached at or before this group, so 0 is not \"matched nothing\")",
            (true, _) => "  (cap reached, so more may have matched)",
        };
        out.push_str(&format!(
            "{:<letter_w$}  {:<selector_w$}  {}  {} annotated{}\n",
            letters[g],
            selector,
            hex_color(color),
            drawn,
            note
        ));
    }

    if !annotated.legend.is_empty() {
        let names: Vec<String> = annotated
            .legend
            .iter()
            .map(|e| legend_name(e.name.as_deref()))
            .collect();
        let bounds: Vec<String> = annotated
            .legend
            .iter()
            .map(|e| legend_bounds(&e.bounds))
            .collect();
        let tag_w = annotated
            .legend
            .iter()
            .map(|e| e.tag.len())
            .max()
            .unwrap_or(0);
        let role_w = annotated
            .legend
            .iter()
            .map(|e| e.role.len())
            .max()
            .unwrap_or(0);
        let name_w = names.iter().map(String::len).max().unwrap_or(0);
        let bounds_w = bounds.iter().map(String::len).max().unwrap_or(0);

        out.push('\n');
        for (i, entry) in annotated.legend.iter().enumerate() {
            out.push_str(&format!(
                "{:<tag_w$}  {:<role_w$}  {:<name_w$}  {:<bounds_w$}  {}\n",
                entry.tag, entry.role, names[i], bounds[i], entry.selector
            ));
        }
    }

    if !annotated.omitted.is_empty() {
        let shown = annotated.omitted.len().min(MAX_LEGEND_OMISSION_DETAILS);
        let mut details: Vec<String> = annotated.omitted[..shown]
            .iter()
            .map(|o| {
                format!(
                    "{}: {} {}",
                    o.reason.as_str(),
                    o.role,
                    legend_name(o.name.as_deref())
                )
            })
            .collect();
        if annotated.omitted.len() > shown {
            details.push(format!("… +{} more", annotated.omitted.len() - shown));
        }
        out.push('\n');
        out.push_str(&format!(
            "omitted: {} element{} ({})\n",
            annotated.omitted.len(),
            if annotated.omitted.len() == 1 {
                ""
            } else {
                "s"
            },
            details.join(", ")
        ));
    }

    if annotated.truncated > 0 {
        out.push_str(&format!(
            "truncated: {} more element{} matched but {} not described (cap: {})\n",
            annotated.truncated,
            if annotated.truncated == 1 { "" } else { "s" },
            if annotated.truncated == 1 {
                "was"
            } else {
                "were"
            },
            crate::MAX_ANNOTATIONS
        ));
    }

    out
}

/// Render the same information as one JSON object.
///
/// `groups` repeats what the header block says (letter, colour, count, and
/// the `capped` flag from [`first_capped_group`]) so a consumer never has to
/// redo the palette arithmetic, and `truncated` is always present — a caller
/// must be able to tell a complete legend from a prefix of one without
/// checking a length against a cap it has to know.
fn render_legend_json(selectors: &[String], annotated: &crate::Annotated) -> CliResult<String> {
    let first_capped = first_capped_group(annotated);
    let groups: Vec<serde_json::Value> = selectors
        .iter()
        .enumerate()
        .map(|(g, selector)| {
            let group = g + 1;
            let color = screenshot::ANNOTATION_PALETTE[g % screenshot::ANNOTATION_PALETTE.len()];
            serde_json::json!({
                "group": group,
                "letter": group_letter(group),
                "selector": selector,
                "color": color,
                "color_hex": hex_color(color),
                "annotated": annotated.legend.iter().filter(|e| e.group == group).count(),
                "capped": first_capped.is_some_and(|first| group >= first),
            })
        })
        .collect();

    let doc = serde_json::json!({
        "groups": groups,
        "legend": annotated.legend,
        "omitted": annotated.omitted,
        "truncated": annotated.truncated,
        "cap": crate::MAX_ANNOTATIONS,
    });
    serde_json::to_string_pretty(&doc).map_err(|e| {
        CliError::Xa11y(Error::Platform {
            code: -1,
            message: format!("render legend as JSON: {e}"),
        })
    })
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
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.app.as_deref(), Some("Safari"));
        assert!(opts.pid.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_pid_flag() {
        let args = strs(&["--pid", "1234"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.pid, Some(1234));
        assert!(opts.app.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_positional_and_flags() {
        let args = strs(&["button[name='OK']", "--app", "MyApp"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.app.as_deref(), Some("MyApp"));
        assert_eq!(pos, vec![s("button[name='OK']")]);
    }

    #[test]
    fn parse_opts_multiple_positional() {
        let args = strs(&["press", "button", "--app", "Test"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.app.as_deref(), Some("Test"));
        assert_eq!(pos, vec![s("press"), s("button")]);
    }

    #[test]
    fn parse_opts_empty() {
        let args: Vec<String> = vec![];
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert!(opts.app.is_none());
        assert!(opts.pid.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_value_flag() {
        let args = strs(&["--value", "hello"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
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
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.value.as_deref(), Some("hello"));
        assert_eq!(pos, vec![s("set-value"), s("button[name='OK']")]);
    }

    #[test]
    fn parse_opts_value_missing_trailing_arg_errors() {
        // A trailing flag with no value used to silently produce None; it is
        // now a usage error (tenet 1).
        let args = strs(&["--value"]);
        let err = parse_opts(&args).expect_err("trailing --value must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
        assert!(format!("{err}").contains("--value requires a value"));
    }

    #[test]
    fn parse_opts_trailing_app_flag_errors() {
        let args = strs(&["tree", "--app"]);
        let err = parse_opts(&args).expect_err("trailing --app must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
        assert!(format!("{err}").contains("--app requires a value"));
    }

    #[test]
    fn parse_opts_non_numeric_pid_errors() {
        // `--pid abc` used to be silently treated as absent (tenet 1).
        let args = strs(&["--pid", "abc"]);
        let err = parse_opts(&args).expect_err("non-numeric --pid must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
        let msg = format!("{err}");
        assert!(msg.contains("--pid"), "message must name the flag: {msg}");
        assert!(
            msg.contains("abc"),
            "message must echo the bad value: {msg}"
        );
    }

    #[test]
    fn parse_opts_non_numeric_count_errors() {
        let args = strs(&["--count", "two"]);
        let err = parse_opts(&args).expect_err("non-numeric --count must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
    }

    #[test]
    fn parse_opts_non_numeric_duration_errors() {
        let args = strs(&["--duration-ms", "fast"]);
        let err = parse_opts(&args).expect_err("non-numeric --duration-ms must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
    }

    // ── Shell surface targeting ─────────────────────────────────────────────

    #[test]
    fn parse_opts_shell_flag() {
        let args = strs(&["--shell", "taskbar"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.shell.as_deref(), Some("taskbar"));
        assert!(opts.app.is_none());
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_shell_combines_with_pid() {
        // `--pid` alongside `--shell` disambiguates same-kind surfaces rather
        // than naming an application, so both must survive parsing together.
        let args = strs(&["--shell", "panel", "--pid", "4242"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.shell.as_deref(), Some("panel"));
        assert_eq!(opts.pid, Some(4242));
    }

    #[test]
    fn parse_opts_trailing_shell_flag_errors() {
        let args = strs(&["tree", "--shell"]);
        let err = parse_opts(&args).expect_err("trailing --shell must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
        assert!(format!("{err}").contains("--shell requires a value"));
    }

    #[test]
    fn parse_opts_shell_value_before_positional_does_not_leak() {
        let args = strs(&["press", "--shell", "taskbar", "button"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.shell.as_deref(), Some("taskbar"));
        assert_eq!(pos, vec![s("press"), s("button")]);
    }

    // ── Shell surface selection ─────────────────────────────────────────
    //
    // `select_shell_surface` is the half of `resolve_shell_surface` that has
    // no OS in it, which is the only reason these cases are reachable off a
    // desktop. `ShellSurface` has no public constructor — its provider handle
    // is private — so the fixtures come from the shared mock and are then
    // relabelled; `kind`, `name` and `pid` are all the selection reads.

    /// One mock-backed surface per `(kind, pid)` spec, in order.
    fn mock_surfaces(specs: &[(ShellSurfaceKind, Option<u32>)]) -> Vec<ShellSurface> {
        let provider: std::sync::Arc<dyn crate::Provider> = xa11y_core::mock::build_provider();
        let mut out: Vec<ShellSurface> = Vec::new();
        while out.len() < specs.len() {
            let batch = ShellSurface::list_with(std::sync::Arc::clone(&provider))
                .expect("the mock must list its shell surfaces");
            assert!(!batch.is_empty(), "the mock fixture must vend surfaces");
            out.extend(batch);
        }
        out.truncate(specs.len());
        for (surface, (kind, pid)) in out.iter_mut().zip(specs) {
            surface.kind = *kind;
            surface.pid = *pid;
            surface.name = match pid {
                Some(p) => format!("{kind}-{p}"),
                None => format!("{kind}-unowned"),
            };
        }
        out
    }

    #[test]
    fn select_shell_surface_picks_the_surface_with_the_matching_pid() {
        let surfaces = mock_surfaces(&[
            (ShellSurfaceKind::Panel, Some(11)),
            (ShellSurfaceKind::Panel, Some(22)),
            (ShellSurfaceKind::Taskbar, Some(33)),
        ]);
        let picked = select_shell_surface(surfaces, ShellSurfaceKind::Panel, Some(22))
            .expect("a pid that matches exactly one surface must resolve");
        assert_eq!(picked.kind, ShellSurfaceKind::Panel);
        assert_eq!(picked.pid, Some(22));
    }

    #[test]
    fn select_shell_surface_reports_a_pid_that_matches_nothing() {
        let surfaces = mock_surfaces(&[
            (ShellSurfaceKind::Panel, Some(11)),
            (ShellSurfaceKind::Panel, Some(22)),
        ]);
        let err = select_shell_surface(surfaces, ShellSurfaceKind::Panel, Some(99))
            .expect_err("no panel has pid 99");
        let CliError::Xa11y(e) = &err else {
            panic!("absence is a lookup failure, not ambiguity: {err:?}");
        };
        let diagnosis = e.diagnosis().expect("the terminal failure must diagnose");
        assert!(
            diagnosis
                .last_observed
                .as_deref()
                .is_some_and(|s| s.contains("pid 99")),
            "the failure must echo the pid that matched nothing: {diagnosis:?}"
        );
        // Tenet 6: the surfaces that *were* there are the way out.
        assert_eq!(diagnosis.candidates.len(), 2, "{diagnosis:?}");
    }

    #[test]
    fn select_shell_surface_reports_a_kind_that_is_not_present() {
        let surfaces = mock_surfaces(&[(ShellSurfaceKind::Panel, Some(11))]);
        let err = select_shell_surface(surfaces, ShellSurfaceKind::Dock, None)
            .expect_err("there is no dock in this listing");
        let CliError::Xa11y(e) = &err else {
            panic!("absence is a lookup failure, not ambiguity: {err:?}");
        };
        assert!(matches!(e, Error::SelectorNotMatched { .. }), "{e:?}");
        let diagnosis = e.diagnosis().expect("the terminal failure must diagnose");
        assert_eq!(diagnosis.candidates, vec!["panel \"panel-11\" (pid=11)"]);
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn select_shell_surface_refuses_ambiguity_and_points_at_the_listing() {
        let surfaces = mock_surfaces(&[
            (ShellSurfaceKind::Panel, Some(11)),
            (ShellSurfaceKind::Panel, Some(22)),
        ]);
        let err = select_shell_surface(surfaces, ShellSurfaceKind::Panel, None)
            .expect_err("two panels must be refused, not first-matched");
        let CliError::AmbiguousShellSurface {
            count,
            kind,
            diagnosis,
        } = &err
        else {
            panic!("expected AmbiguousShellSurface, got {err:?}");
        };
        assert_eq!(*count, 2);
        assert_eq!(kind, "panel");
        assert!(
            diagnosis
                .last_observed
                .as_deref()
                .is_some_and(|s| s.contains("xa11y shell")),
            "without a pid the way out is the listing: {diagnosis:?}"
        );
        assert_eq!(diagnosis.candidates.len(), 2);
    }

    #[test]
    fn select_shell_surface_says_a_pid_cannot_split_one_process() {
        // The real case: two panel frames owned by one xfce4-panel process.
        // `pid` is the only disambiguator, so the honest answer is that this
        // operation cannot pick — never "add a pid", which the caller did.
        let surfaces = mock_surfaces(&[
            (ShellSurfaceKind::Panel, Some(4242)),
            (ShellSurfaceKind::Panel, Some(4242)),
        ]);
        let err = select_shell_surface(surfaces, ShellSurfaceKind::Panel, Some(4242))
            .expect_err("one pid cannot pick between two of its own surfaces");
        let CliError::AmbiguousShellSurface { diagnosis, .. } = &err else {
            panic!("expected AmbiguousShellSurface, got {err:?}");
        };
        let observed = diagnosis
            .last_observed
            .as_deref()
            .expect("the hint is the whole point of this branch");
        assert!(observed.contains("share pid 4242"), "{observed}");
        assert!(
            observed.contains("only disambiguator"),
            "the hint must not send the caller back to --pid: {observed}"
        );
        assert!(
            !observed.contains("xa11y shell"),
            "listing pids helps nobody here: {observed}"
        );
    }

    #[test]
    fn every_advertised_shell_kind_parses() {
        // The advertised list is derived from `ShellSurfaceKind::ALL`, so this
        // closes the loop: every name the help text, the flag's error message
        // and the MCP schema offer must also parse back to the kind it names.
        for name in shell_kind_names() {
            let kind = parse_shell_kind(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(kind.to_snake_case(), *name);
        }
        assert_eq!(shell_kind_names().len(), ShellSurfaceKind::ALL.len());
    }

    #[test]
    fn an_unknown_shell_kind_is_a_usage_error_listing_the_valid_ones() {
        let err = parse_shell_kind("taskbr").expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
        let msg = format!("{err}");
        assert!(msg.contains("taskbr"), "must echo the bad value: {msg}");
        for name in shell_kind_names() {
            assert!(msg.contains(name), "{name} must be offered: {msg}");
        }
    }

    #[test]
    fn a_shell_kind_is_case_sensitive_snake_case() {
        // The same spelling crosses every surface — the bindings, `--shell`,
        // MCP's `shell` argument and the `shell_kind` raw attribute.
        assert!(parse_shell_kind("Taskbar").is_err());
        assert!(parse_shell_kind("status-items").is_err());
        assert!(parse_shell_kind("status_items").is_ok());
    }

    #[test]
    fn shell_and_app_together_are_a_usage_error_before_any_platform_call() {
        let args = strs(&["--shell", "taskbar", "--app", "Safari"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        let err = resolve_target(&opts).expect_err("two targets is not a target");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
        let msg = format!("{err}");
        assert!(msg.contains("--shell") && msg.contains("--app"), "{msg}");
    }

    #[test]
    fn an_unknown_shell_kind_is_rejected_before_the_shell_is_enumerated() {
        // Parsed first, so a typo cannot cost an enumeration — and so this is
        // testable with no display and no accessibility bus.
        let args = strs(&["--shell", "not_a_surface"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        let err = resolve_target(&opts).expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert!(format!("{err}").contains("not_a_surface"));
    }

    #[test]
    fn the_shell_listing_takes_no_arguments() {
        // Accepting and ignoring `--shell taskbar` would read as a filter that
        // silently did nothing.
        let err = cmd_shell(&strs(&["--shell", "taskbar"])).expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
        assert!(format!("{err}").contains("takes no arguments"));
    }

    #[test]
    fn events_says_why_shell_is_not_a_target_rather_than_asking_for_an_app() {
        let args = strs(&["--shell", "taskbar"]);
        let err = cmd_events(&args).expect_err("events takes no shell surface");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        let msg = format!("{err}");
        assert!(msg.contains("--shell"), "{msg}");
        assert!(msg.contains("per application"), "{msg}");
    }

    #[test]
    fn an_ambiguous_shell_surface_is_an_operation_failure_that_names_the_pids() {
        let err = CliError::AmbiguousShellSurface {
            count: 2,
            kind: "panel".into(),
            diagnosis: Box::new(
                Diagnosis::new()
                    .condition("exactly one panel shell surface")
                    .last_observed("2 panel surfaces are present; `xa11y shell` lists their pids")
                    .candidates(vec![
                        "panel \"Top\" (pid=101)".into(),
                        "panel \"Dock\" (pid=102)".into(),
                    ]),
            ),
        };
        assert_eq!(
            err.exit_code(),
            1,
            "not a usage error: the call was well-formed"
        );
        let msg = err.to_string();
        assert!(msg.contains("2 panel surfaces"), "{msg}");
        // Both spellings, because this one message is rendered on both
        // surfaces and an MCP caller has no flags.
        assert!(msg.contains("--pid PID"), "{msg}");
        assert!(msg.contains("`pid` argument"), "{msg}");
        assert!(msg.contains("panel \"Dock\" (pid=102)"), "{msg}");
    }

    #[test]
    fn shell_candidate_lists_are_bounded_and_say_how_many_were_dropped() {
        // Tenet 6: rich, but never unbounded — a machine with 30 panels must
        // not turn one failure into 30 lines of message.
        let many: Vec<String> = (0..MAX_SHELL_CANDIDATES + 3)
            .map(|i| format!("panel \"Panel {i}\" (pid={i})"))
            .collect();
        let bounded = bound_candidates(many);
        assert_eq!(bounded.len(), MAX_SHELL_CANDIDATES + 1);
        assert_eq!(bounded[0], "panel \"Panel 0\" (pid=0)");
        assert!(bounded.last().unwrap().contains("+3 more"));
    }

    #[test]
    fn a_short_shell_candidate_list_is_carried_whole() {
        let few = vec!["taskbar \"Taskbar\" (pid=4)".to_string()];
        assert_eq!(bound_candidates(few.clone()), few);
    }

    // ── Exit-code contract ──────────────────────────────────────────────────

    #[test]
    fn exit_code_usage_is_2() {
        assert_eq!(CliError::Usage("bad flag".into()).exit_code(), 2);
    }

    #[test]
    fn exit_code_not_found_is_1() {
        assert_eq!(CliError::NotFound("no match".into()).exit_code(), 1);
    }

    #[test]
    fn exit_code_xa11y_error_is_1() {
        let e = CliError::Xa11y(Error::NoElementBounds);
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn usage_error_displays_with_prefix() {
        let e = CliError::Usage("specify --app NAME or --pid PID".into());
        assert_eq!(
            format!("{e}"),
            "usage error: specify --app NAME or --pid PID"
        );
    }

    #[test]
    fn not_found_error_displays_message_verbatim() {
        let e = CliError::NotFound("no elements matched selector: button".into());
        assert_eq!(format!("{e}"), "no elements matched selector: button");
    }

    // ── Format element ──────────────────────────────────────────────────────

    fn make_element(role: Role, name: Option<&str>) -> ElementData {
        let mut data = ElementData::for_role(role);
        data.name = name.map(String::from);
        data
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
        let event = Event::new(
            EventKind::StateChanged {
                flag: StateFlag::Focused,
                value: true,
            },
            "App",
            1,
        );
        let detail = format_event_detail(&event);
        assert!(detail.contains("Focused=true"));
    }

    #[test]
    fn format_event_kind_is_snake_case_not_debug() {
        assert_eq!(format_event_kind(&EventKind::FocusChanged), "focus_changed");
        assert_eq!(
            format_event_kind(&EventKind::StateChanged {
                flag: StateFlag::Checked,
                value: true,
            }),
            "state_changed"
        );
        assert_eq!(format_event_kind(&EventKind::Announcement), "announcement");
    }

    #[test]
    fn format_event_detail_empty() {
        let event = Event::new(EventKind::FocusChanged, "App", 1);
        assert!(format_event_detail(&event).is_empty());
    }

    // ── resolve_app error ───────────────────────────────────────────────────

    #[test]
    fn resolve_app_no_flags_is_usage_error() {
        let opts = Opts::default();
        let err = resolve_app(&opts).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
        let msg = format!("{err}");
        assert!(msg.contains("--app") || msg.contains("--pid"));
    }

    // ── Input-sim / screenshot flag parsing ─────────────────────────────────

    #[test]
    fn parse_opts_at_flag() {
        let args = strs(&["--at", "100,200"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.at.as_deref(), Some("100,200"));
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_from_to_flags() {
        let args = strs(&["--from", "1,2", "--to", "3,4"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.from.as_deref(), Some("1,2"));
        assert_eq!(opts.to.as_deref(), Some("3,4"));
    }

    #[test]
    fn parse_opts_button_count_held() {
        let args = strs(&["--button", "right", "--count", "2", "--held", "Shift,Meta"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.button.as_deref(), Some("right"));
        assert_eq!(opts.count, Some(2));
        assert_eq!(opts.held.as_deref(), Some("Shift,Meta"));
    }

    #[test]
    fn parse_opts_scroll_deltas() {
        let args = strs(&["--dx", "-3", "--dy", "5"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
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
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.duration_ms, Some(250));
        assert_eq!(opts.region.as_deref(), Some("10,20,30,40"));
        assert_eq!(opts.out.as_deref(), Some("shot.png"));
    }

    #[test]
    fn parse_opts_output_format() {
        let args = strs(&["-o", "bounds"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
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

    // ── Action verbs ────────────────────────────────────────────────────────

    #[test]
    fn every_value_taking_verb_is_a_verb() {
        // `ACTIONS_REQUIRING_VALUE` is advertised to MCP callers as a subset
        // of the action enum; a verb in one list and not the other would be
        // documented and unreachable.
        for verb in ACTIONS_REQUIRING_VALUE {
            assert!(ACTION_NAMES.contains(verb), "{verb} is not an action");
        }
    }

    #[test]
    fn set_numeric_value_is_offered_and_needs_a_value() {
        assert!(ACTION_NAMES.contains(&"set-numeric-value"));
        assert!(ACTIONS_REQUIRING_VALUE.contains(&"set-numeric-value"));
    }

    #[test]
    fn numeric_values_parse_the_way_a_slider_is_written() {
        assert_eq!(parse_numeric_value("88").unwrap(), 88.0);
        assert_eq!(parse_numeric_value(" 0.5 ").unwrap(), 0.5);
        assert_eq!(parse_numeric_value("-3").unwrap(), -3.0);
    }

    #[test]
    fn a_non_numeric_value_is_rejected_before_any_platform_call() {
        let err = parse_numeric_value("loud").expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert!(err.to_string().contains("loud"), "{err}");
    }

    #[test]
    fn non_finite_values_are_rejected_rather_than_passed_on() {
        for raw in ["NaN", "inf", "-inf"] {
            let err = parse_numeric_value(raw).expect_err("must reject {raw}");
            assert!(err.to_string().contains("finite"), "{raw}: {err}");
        }
    }

    #[test]
    fn an_unsupported_action_is_named_the_way_the_caller_must_type_it() {
        // Providers report the failing action by its Rust method name, so the
        // error used to tell the user to use `show_menu`, which both surfaces
        // reject.
        let err = relabel_action_error(
            CliError::Xa11y(Error::ActionNotSupported {
                action: "show_menu".into(),
                role: Role::MenuItem,
            }),
            "show-menu",
        );
        assert_eq!(
            err.to_string(),
            "Action show-menu not supported on menu_item"
        );
    }

    #[test]
    fn an_unrelated_action_name_in_an_error_is_left_alone() {
        // A provider naming some *other* action is reporting something the
        // caller needs to see, not a spelling to normalize.
        let err = relabel_action_error(
            CliError::Xa11y(Error::ActionNotSupported {
                action: "activate".into(),
                role: Role::MenuItem,
            }),
            "press",
        );
        assert!(err.to_string().contains("activate"), "{err}");
    }

    #[test]
    fn an_ambiguous_selector_is_an_operation_failure_that_names_the_way_out() {
        let err = CliError::Ambiguous {
            count: 2,
            diagnosis: Box::new(Diagnosis::new().selector("radio_button").candidates(vec![
                "radio_button \"A\"".into(),
                "radio_button \"B\"".into(),
            ])),
        };
        assert_eq!(
            err.exit_code(),
            1,
            "not a usage error: the call was well-formed"
        );
        let msg = err.to_string();
        assert!(msg.contains("matched 2 elements"), "{msg}");
        assert!(msg.contains(":nth(n)"), "{msg}");
        assert!(msg.contains("radio_button \"B\""), "{msg}");
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
        let (opts, _) = parse_opts(&args).expect("flags must parse");
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
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert!(build_click_options(&opts).is_err());
    }

    #[test]
    fn build_click_options_bad_held_errors() {
        let args = strs(&["--held", "NotAKey"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
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
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        let d = build_drag_options(&opts).unwrap();
        assert!(matches!(d.button, MouseButton::Middle));
        assert_eq!(d.held.len(), 1);
        assert!(matches!(d.held[0], Key::Ctrl));
        assert_eq!(d.duration, Duration::from_millis(500));
    }

    // ── Screenshot annotation: flags and legend rendering ───────────────────

    #[test]
    fn parse_opts_annotate_is_repeatable() {
        // Each occurrence is a distinct group, so the last must not win.
        let args = strs(&["--annotate", "button", "--annotate", "text_field"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.annotate, vec![s("button"), s("text_field")]);
        assert!(pos.is_empty());
    }

    #[test]
    fn parse_opts_annotate_preserves_flag_order() {
        let args = strs(&["--annotate", "c", "--out", "x.png", "--annotate", "a"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(
            opts.annotate,
            vec![s("c"), s("a")],
            "group order is flag order, and it decides colour and tag letter"
        );
    }

    #[test]
    fn parse_opts_annotate_absent_is_empty_not_none() {
        let args = strs(&["--out", "x.png"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert!(opts.annotate.is_empty());
        assert!(opts.legend.is_none());
    }

    #[test]
    fn parse_opts_trailing_annotate_flag_errors() {
        let args = strs(&["--out", "x.png", "--annotate"]);
        let err = parse_opts(&args).expect_err("a trailing --annotate has no selector");
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn parse_opts_legend_flag() {
        let args = strs(&["--legend", "json"]);
        let (opts, _) = parse_opts(&args).expect("flags must parse");
        assert_eq!(opts.legend.as_deref(), Some("json"));
    }

    #[test]
    fn parse_legend_format_accepts_exactly_the_three_advertised_values() {
        assert_eq!(parse_legend_format("text").unwrap(), LegendFormat::Text);
        assert_eq!(parse_legend_format("json").unwrap(), LegendFormat::Json);
        assert_eq!(parse_legend_format("none").unwrap(), LegendFormat::None);

        let err = parse_legend_format("yaml").expect_err("unknown formats are usage errors");
        match err {
            CliError::Usage(msg) => {
                assert!(msg.contains("text|json|none"), "{msg}");
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// `--out -` puts PNG bytes on stdout and the legend wants the same
    /// stream. The command refuses and names both fixes rather than quietly
    /// moving the legend to stderr (tenet 1).
    #[test]
    fn annotating_to_stdout_with_a_legend_is_a_usage_error_naming_both_fixes() {
        let args = strs(&["--out", "-", "--app", "TestApp", "--annotate", "button"]);
        let err = cmd_screenshot(&args).expect_err("PNG and legend cannot share stdout");
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("--out FILE"), "{msg}");
        assert!(msg.contains("--legend none"), "{msg}");
    }

    #[test]
    fn annotating_to_stdout_with_legend_none_passes_argument_validation() {
        // It still fails — there is no target resolution to be had in a unit
        // test — but the failure must no longer be the stdout collision.
        let args = strs(&[
            "--out",
            "-",
            "--app",
            "no-such-app-4f2a",
            "--annotate",
            "button",
            "--legend",
            "none",
        ]);
        let err = cmd_screenshot(&args).expect_err("no such app");
        let msg = err.to_string();
        assert!(!msg.contains("--legend none"), "{msg}");
    }

    #[test]
    fn annotating_without_a_target_is_a_usage_error_naming_the_flags() {
        let args = strs(&["--out", "x.png", "--annotate", "button"]);
        let err = cmd_screenshot(&args).expect_err("--annotate needs something to search");
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("--app NAME"), "{msg}");
        assert!(msg.contains("--pid PID"), "{msg}");
        assert!(msg.contains("--shell KIND"), "{msg}");
    }

    #[test]
    fn a_legend_with_nothing_to_describe_is_a_usage_error() {
        let args = strs(&["--out", "x.png", "--legend", "json"]);
        let err = cmd_screenshot(&args).expect_err("--legend alone describes nothing");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--annotate SELECTOR"), "{err}");
    }

    #[test]
    fn a_bad_legend_value_is_rejected_before_the_target_is_touched() {
        let args = strs(&[
            "--out",
            "x.png",
            "--app",
            "no-such-app-4f2a",
            "--annotate",
            "button",
            "--legend",
            "yaml",
        ]);
        let err = cmd_screenshot(&args).expect_err("yaml is not a legend format");
        assert_eq!(err.exit_code(), 2, "parse before the first OS call");
        assert!(err.to_string().contains("text|json|none"), "{err}");
    }

    #[test]
    fn a_bad_region_is_still_rejected_when_annotating() {
        let args = strs(&[
            "--out",
            "x.png",
            "--region",
            "1,2,3",
            "--app",
            "TestApp",
            "--annotate",
            "button",
        ]);
        let err = cmd_screenshot(&args).expect_err("--region needs four numbers");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("X,Y,W,H"), "{err}");
    }

    #[test]
    fn missing_out_is_still_the_first_thing_checked() {
        let err = cmd_screenshot(&strs(&["--annotate", "button"]))
            .expect_err("--out is required either way");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--out PATH"), "{err}");
    }

    // ── Legend rendering ────────────────────────────────────────────────────

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn entry(tag: &str, group: usize, index: usize, role: &str, name: Option<&str>) -> LegendEntry {
        // `LegendEntry::new` derives the tag from the group and index, so the
        // fixture's spelling is an assertion rather than an input: a test that
        // wrote a tag the numbering could never produce would be testing a
        // shape the resolver cannot hand this renderer.
        let entry = LegendEntry::new(
            group,
            index,
            format!("{role}:nth({index})"),
            role,
            name.map(str::to_string),
            rect(104, 318, 48, 44),
            screenshot::ANNOTATION_PALETTE[(group - 1) % 7],
        );
        assert_eq!(entry.tag, tag, "fixture tag disagrees with tag_for");
        entry
    }

    /// A synthetic result. `Annotated` lives in `xa11y-core` and is
    /// `#[non_exhaustive]`, so it is built through its constructor, which
    /// takes every field — a new one changes that signature and breaks this
    /// fixture rather than defaulting silently.
    fn annotated(legend: Vec<LegendEntry>, omitted: Vec<Omission>, truncated: usize) -> Annotated {
        Annotated::for_capture(
            Screenshot::new(2, 2, vec![0; 16], 1.0),
            legend,
            omitted,
            truncated,
        )
    }

    #[test]
    fn the_text_legend_leads_with_one_header_per_group() {
        let out = render_legend_text(
            &[s("button"), s("text_field")],
            &annotated(
                vec![
                    entry("A1", 1, 1, "button", Some("7")),
                    entry("A2", 1, 2, "button", Some("8")),
                    entry("B1", 2, 1, "text_field", Some("Display")),
                ],
                vec![],
                0,
            ),
        );
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines[0], "A  button      #E69F00  2 annotated");
        assert_eq!(lines[1], "B  text_field  #56B4E9  1 annotated");
        assert_eq!(lines[2], "", "a blank line separates headers from entries");
        assert!(lines[3].starts_with("A1  button"), "{}", lines[3]);
        assert!(lines[3].contains("bounds=104,318,48,44"), "{}", lines[3]);
        assert!(lines[3].ends_with("button:nth(1)"), "{}", lines[3]);
        assert!(lines[5].ends_with("text_field:nth(1)"), "{}", lines[5]);
    }

    #[test]
    fn the_text_legend_columns_line_up_across_roles_of_different_widths() {
        let out = render_legend_text(
            &[s("*")],
            &annotated(
                vec![
                    entry("A1", 1, 1, "button", Some("Go")),
                    entry("A2", 1, 2, "text_field", Some("A much longer name")),
                ],
                vec![],
                0,
            ),
        );
        let rows: Vec<&str> = out.lines().skip(2).collect();
        let selector_col: Vec<usize> = rows
            .iter()
            .map(|l| l.find("bounds=").expect("every row has bounds"))
            .collect();
        assert_eq!(
            selector_col[0], selector_col[1],
            "the bounds column must start at the same offset on every row:\n{out}"
        );
    }

    #[test]
    fn a_group_that_matched_nothing_still_gets_a_header() {
        // Otherwise the flag reads as if it had been ignored.
        let out = render_legend_text(
            &[s("button"), s("progress_bar")],
            &annotated(vec![entry("A1", 1, 1, "button", None)], vec![], 0),
        );
        assert!(out.contains("B  progress_bar"), "{out}");
        assert!(out.contains("0 annotated"), "{out}");
    }

    #[test]
    fn a_nameless_element_renders_as_a_dash_not_empty_quotes() {
        let out = render_legend_text(
            &[s("button")],
            &annotated(vec![entry("A1", 1, 1, "button", None)], vec![], 0),
        );
        let row = out.lines().nth(2).expect("one entry row");
        assert!(row.contains(" -  "), "{row}");
        assert!(!row.contains("\"\""), "{row}");
    }

    #[test]
    fn the_text_legend_reports_what_could_not_be_drawn() {
        let out = render_legend_text(
            &[s("button")],
            &annotated(
                vec![entry("A1", 1, 1, "button", Some("7"))],
                vec![Omission::new(
                    "button:nth(2)",
                    "button",
                    Some(s("Paste")),
                    OmissionReason::OutsideCapture,
                )],
                0,
            ),
        );
        assert!(
            out.contains("omitted: 1 element (outside_capture: button \"Paste\")"),
            "{out}"
        );
    }

    #[test]
    fn the_omitted_summary_is_bounded_and_says_how_many_it_dropped() {
        let many: Vec<Omission> = (0..MAX_LEGEND_OMISSION_DETAILS + 4)
            .map(|i| {
                Omission::new(
                    format!("button:nth({i})"),
                    "button",
                    None,
                    OmissionReason::NoBounds,
                )
            })
            .collect();
        let total = many.len();
        let out = render_legend_text(&[s("button")], &annotated(vec![], many, 0));

        assert!(out.contains(&format!("omitted: {total} elements")), "{out}");
        assert!(out.contains("… +4 more"), "{out}");
    }

    #[test]
    fn the_text_legend_says_when_the_cap_bit() {
        let out = render_legend_text(
            &[s("*")],
            &annotated(vec![entry("A1", 1, 1, "button", None)], vec![], 37),
        );
        assert!(out.contains("truncated: 37 more elements"), "{out}");
        assert!(
            out.contains(&format!("cap: {}", crate::MAX_ANNOTATIONS)),
            "{out}"
        );
    }

    #[test]
    fn a_group_starved_by_the_cap_does_not_read_as_one_that_matched_nothing() {
        // A and B were resolved, the cap bit, and C never got looked at. Its
        // header used to print "0 annotated", which is byte-for-byte what
        // `a_group_that_matched_nothing_still_gets_a_header` asserts for a
        // selector that genuinely matched nothing.
        let legend = || {
            vec![
                entry("A1", 1, 1, "button", None),
                entry("B1", 2, 1, "text_field", None),
            ]
        };
        let selectors = [s("button"), s("text_field"), s("link")];
        let starved = render_legend_text(&selectors, &annotated(legend(), vec![], 12));
        let lines: Vec<&str> = starved.lines().collect();

        // A finished before the cap could bite, so its count is exact.
        assert!(!lines[0].contains("cap"), "{starved}");
        // B is the group the cap could have cut short.
        assert!(lines[1].contains("cap reached"), "{starved}");
        // C is the case this test exists for.
        assert!(lines[2].contains("0 annotated"), "{starved}");
        assert!(lines[2].contains("cap reached"), "{starved}");

        let nothing = render_legend_text(&selectors, &annotated(legend(), vec![], 0));
        let c_nothing = nothing.lines().nth(2).expect("a header per selector");
        assert!(c_nothing.contains("0 annotated"), "{nothing}");
        assert!(
            !c_nothing.contains("cap"),
            "nothing was lost here:\n{nothing}"
        );
        assert_ne!(
            lines[2], c_nothing,
            "a starved group and one that matched nothing must not render identically"
        );
    }

    #[test]
    fn the_json_groups_flag_the_ones_the_cap_may_have_shortened() {
        let selectors = [s("button"), s("text_field"), s("link")];
        let render = |truncated| {
            let json = render_legend_json(
                &selectors,
                &annotated(vec![entry("A1", 1, 1, "button", None)], vec![], truncated),
            )
            .expect("the legend must serialize");
            serde_json::from_str::<serde_json::Value>(&json).expect("valid JSON")
        };

        // `truncated` is a total that attributes the loss to no group;
        // `capped` is what tells a consumer that C's `annotated: 0` is not
        // "matched nothing".
        let capped = render(12);
        assert_eq!(capped["groups"][2]["annotated"], 0);
        assert_eq!(capped["groups"][2]["capped"], true);
        assert_eq!(capped["groups"][0]["capped"], true, "the cap bit in A");

        let complete = render(0);
        assert_eq!(complete["groups"][2]["annotated"], 0);
        assert_eq!(complete["groups"][2]["capped"], false);
        assert_eq!(complete["groups"][0]["capped"], false);
    }

    #[test]
    fn with_no_legend_at_all_every_group_is_flagged_as_possibly_capped() {
        // 100 omissions and a non-zero `truncated` leave no legend entry to
        // locate the cap by, so no group can be called exact.
        let out = render_legend_text(&[s("button"), s("link")], &annotated(vec![], vec![], 5));
        assert_eq!(out.lines().filter(|l| l.contains("cap reached")).count(), 2);
    }

    #[test]
    fn a_legend_with_nothing_in_it_is_headers_only() {
        let out = render_legend_text(&[s("button")], &annotated(vec![], vec![], 0));
        assert_eq!(out, "A  button  #E69F00  0 annotated\n");
    }

    #[test]
    fn the_json_legend_carries_the_groups_the_entries_and_the_cap() {
        let json = render_legend_json(
            &[s("button"), s("text_field")],
            &annotated(
                vec![entry("A1", 1, 1, "button", Some("7"))],
                vec![Omission::new(
                    "check_box:nth(1)",
                    "check_box",
                    Some(s("Agree")),
                    OmissionReason::NoBounds,
                )],
                3,
            ),
        )
        .expect("the legend must serialize");
        let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(doc["groups"][0]["letter"], "A");
        assert_eq!(doc["groups"][0]["selector"], "button");
        assert_eq!(doc["groups"][0]["color_hex"], "#E69F00");
        assert_eq!(doc["groups"][0]["annotated"], 1);
        assert_eq!(doc["groups"][1]["letter"], "B");
        assert_eq!(doc["groups"][1]["annotated"], 0);

        assert_eq!(doc["legend"][0]["tag"], "A1");
        assert_eq!(doc["legend"][0]["selector"], "button:nth(1)");
        assert_eq!(doc["legend"][0]["index"], 1);
        assert_eq!(doc["legend"][0]["bounds"]["x"], 104);
        assert_eq!(doc["legend"][0]["color"], serde_json::json!([230, 159, 0]));

        assert_eq!(doc["omitted"][0]["reason"], "no_bounds");
        assert_eq!(doc["omitted"][0]["selector"], "check_box:nth(1)");

        assert_eq!(doc["truncated"], 3);
        assert_eq!(doc["cap"], crate::MAX_ANNOTATIONS);
    }

    #[test]
    fn group_letters_follow_the_tag_format_past_z() {
        assert_eq!(group_letter(1), "A");
        assert_eq!(group_letter(2), "B");
        assert_eq!(group_letter(26), "Z");
        assert_eq!(group_letter(27), "AA");
        // The letter in the header and the letter drawn in the image are the
        // same function, so they cannot disagree.
        assert!(screenshot::tag_for(27, 5).starts_with(&group_letter(27)));
    }

    #[test]
    fn group_colours_cycle_with_the_palette() {
        let selectors: Vec<String> = (0..9).map(|i| format!("role{i}")).collect();
        let out = render_legend_text(&selectors, &annotated(vec![], vec![], 0));
        let hexes: Vec<&str> = out
            .lines()
            .map(|l| l.split_whitespace().nth(2).expect("a colour column"))
            .collect();
        assert_eq!(hexes[0], hexes[7], "group 8 reuses group 1's colour");
        assert_eq!(hexes[1], hexes[8]);
        assert_eq!(hexes[0], "#E69F00");
    }
}
