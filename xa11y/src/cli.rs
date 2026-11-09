// CLI implementation for `xa11y` — accessibility tree explorer.
//
// This module is `#[doc(hidden)]` and not part of the public API.
// It powers both `cargo install xa11y` and `pip install xa11y` via PyO3.

use std::collections::HashSet;
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
    /// An MCP event-subscription handle could not be resolved — exit code 1.
    ///
    /// Constructed by the `events_poll` / `events_stop` tools. MCP has no
    /// protocol-level session, so a subscription is addressed by an opaque
    /// handle, and a handle can miss for two reasons a model must tell apart:
    /// it was issued and is now closed — stopped, or reclaimed for idling —
    /// in which case starting another one works; or it was never issued, in
    /// which case the id itself is wrong. `expired` carries that distinction
    /// and `failure_kind` maps the two to separate tags.
    ///
    /// `live` names the handles that *are* open, so the recovery is readable
    /// straight off the error rather than requiring another call (tenet 6).
    NoSubscription {
        /// The handle the caller passed.
        id: String,
        /// Whether this handle was issued and later reclaimed for idling, as
        /// opposed to never having been issued at all.
        expired: bool,
        /// Handles that are open right now, in issue order.
        live: Vec<String>,
    },
    /// An MCP element `ref` names a capture the server no longer keeps (or
    /// one it never issued) — exit code 1.
    ///
    /// The server keeps the refs of its most recent `tree` / `find` results
    /// only; `kept` names those captures so the recovery (call `tree` or
    /// `find` again) is readable off the error (tenet 6). A ref is never
    /// re-resolved by selector.
    RefExpired {
        /// The ref the caller passed.
        reference: String,
        /// Captures whose refs are still held, oldest first (`c7`, `c8`, ...).
        kept: Vec<String>,
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
            | CliError::NoSubscription { .. }
            | CliError::RefExpired { .. }
            | CliError::Xa11y(_) => 1,
        }
    }
}

impl CliError {
    /// Whether the failed operation may already have taken effect, so a
    /// caller can tell a safe retry from a double-submit.
    ///
    /// `None` for a usage error: no operation was attempted, so there is
    /// nothing to retry or not. Every other failure answers. The CLI-level
    /// failures (`NotFound`, `Ambiguous`, `AmbiguousShellSurface`,
    /// `NoSubscription`) are refused before anything is dispatched, so they
    /// are `NotDelivered`; an underlying error answers for itself through
    /// [`Error::delivery`].
    pub fn delivery(&self) -> Option<crate::Delivery> {
        match self {
            CliError::Usage(_) => None,
            CliError::NotFound(_)
            | CliError::Ambiguous { .. }
            | CliError::AmbiguousShellSurface { .. }
            | CliError::NoSubscription { .. }
            | CliError::RefExpired { .. } => Some(crate::Delivery::NotDelivered),
            CliError::Xa11y(e) => Some(e.delivery()),
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
            CliError::NoSubscription { id, expired, live } => {
                let cause = if *expired {
                    "is no longer open: `events_stop` closed it, or it expired \
                     after idling past the retention window `events_start` \
                     reported as `expires_after_ms`"
                } else {
                    "is not a handle this server issued: check the id against \
                     what `events_start` returned"
                };
                let open = if live.is_empty() {
                    "no subscriptions are open".to_string()
                } else {
                    format!("open subscriptions: {}", live.join(", "))
                };
                write!(
                    f,
                    "subscription \"{id}\" {cause}. Start another with \
                     `events_start`; {open}"
                )
            }
            CliError::RefExpired { reference, kept } => {
                let held = if kept.is_empty() {
                    "no captures are held".to_string()
                } else {
                    format!("refs are held for captures {}", kept.join(", "))
                };
                write!(
                    f,
                    "ref \"{reference}\" is not held: the server keeps the refs of its {} most \
                     recent {} only, and never re-resolves one by selector. Call `tree` or \
                     `find` again for a fresh ref; {held}",
                    crate::mcp::refs::MAX_CAPTURES,
                    crate::mcp::refs::ISSUERS,
                )
            }
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
            if let Some(line) = delivery_line(args.first().map(|s| s.as_str()).unwrap_or(""), &e) {
                eprintln!("{line}");
            }
            offer_accessibility_settings_hint(args.first().map(|s| s.as_str()).unwrap_or(""), &e);
            e.exit_code()
        }
    }
}

/// The commands that dispatch an action, and so owe their failures a
/// delivery disposition. Read-only commands (`tree`, `find`, `apps`, ...)
/// dispatch nothing, so the line would be noise there.
///
/// `enable-web-accessibility` counts: it sets an attribute on the app, and a
/// timeout while waiting for web content leaves that request applied.
const ACTION_COMMANDS: [&str; 8] = [
    "action",
    "click",
    "move",
    "drag",
    "scroll",
    "key",
    "type",
    "enable-web-accessibility",
];

/// The stderr line that follows a failed action command's error, e.g.
/// `delivery: unknown (the action may have been applied; re-read the state before retrying)`.
///
/// `None` for a command that dispatches nothing and for a usage error.
fn delivery_line(command: &str, error: &CliError) -> Option<String> {
    if !ACTION_COMMANDS.contains(&command) {
        return None;
    }
    let delivery = error.delivery()?;
    let advice = if delivery.is_safe_to_retry() {
        "the action did not reach the application, so a retry is safe"
    } else {
        "the action may have been applied, so re-read the state before retrying"
    };
    Some(format!("delivery: {delivery} ({advice})"))
}

/// Run the CLI with the given arguments (excluding the program name).
///
/// Returns `Ok(())` on success, or an `Err` with a human-readable message
/// on failure. Most callers want [`run_main`], which renders the error and
/// produces the exit code.
pub fn run(args: &[String]) -> CliResult<()> {
    match args.first().map(|s| s.as_str()) {
        Some("apps") => cmd_apps(&args[1..]),
        Some("shell") => cmd_shell(&args[1..]),
        Some("windows") => cmd_windows(&args[1..]),
        Some("enable-web-accessibility") => cmd_enable_web_accessibility(&args[1..]),
        Some("tree") => cmd_tree(&args[1..]),
        Some("find") => cmd_find(&args[1..]),
        Some("action") => cmd_action(&args[1..]),
        Some("text") => cmd_text(&args[1..]),
        Some("table") => cmd_table(&args[1..]),
        Some("relations") => cmd_relations(&args[1..]),
        Some("events") => cmd_events(&args[1..]),
        Some("click") => cmd_click(&args[1..]),
        Some("move") => cmd_move(&args[1..]),
        Some("drag") => cmd_drag(&args[1..]),
        Some("scroll") => cmd_scroll(&args[1..]),
        Some("key") => cmd_key(&args[1..]),
        Some("type") => cmd_type(&args[1..]),
        Some("screenshot") => cmd_screenshot(&args[1..]),
        Some("mcp") => crate::mcp::serve(&args[1..]),
        Some("guide") => cmd_guide(&args[1..]),
        Some("capabilities") => cmd_capabilities(&args[1..]),
        _ => {
            print_usage();
            Ok(())
        }
    }
}

/// The `xa11y --help` text.
///
/// Built as a value so a test can hold it against [`ACTION_NAMES`]: this
/// text carries its own hand-written action list, and `set-numeric-value`
/// worked on both the CLI and MCP while appearing in neither it nor its
/// value-requiring line.
fn usage_text() -> String {
    format!(
        "\
xa11y — accessibility tree explorer

Usage:

Accessibility tree:
  xa11y apps   [--backend B]                List running applications
  xa11y shell                               List OS shell surfaces: KIND PID NAME
  xa11y windows [--app NAME | --pid PID]    List top-level windows
  xa11y tree   [TARGET] [--max-nodes N] [--max-depth N]
                   [--detail text|formatting|relations,...]
                                            Print the accessibility tree, at
                                            most N nodes (default 2000).
                                            --detail adds each element's text,
                                            formatting runs (implies text)
                                            or relations under its line
  xa11y find   SELECTOR [TARGET] [-o pretty|bounds|center] [--stable]
                                            Find elements matching a selector.
                                            --stable waits (up to the default
                                            timeout) until the one match is
                                            visible and its bounds hold still
                                            for at least 750 ms; the selector must
                                            then match exactly one element
  xa11y action ACTION SELECTOR [TARGET] [--value V]
                   [--range START,END] [--at X,Y] [--size W,H]
                                            Perform an action on an element
  xa11y text   OP SELECTOR [TARGET] [--range START,END]
                   [--find TEXT [--occurrence N]] [--offset N [--unit U]]
                   [--limit N]              Read an element's text by range;
                                            prints JSON; see Text below
  xa11y table  OP SELECTOR [TARGET] [--row N --column N]
                                            Table lookups: {table_ops}
  xa11y relations SELECTOR [KIND...] [TARGET]
                                            Elements related to one: who labels
                                            or describes it, what it controls,
                                            where reading flows. No KIND means
                                            every kind this platform reports:
                                            {relation_kinds}
  xa11y events [--app NAME | --pid PID] [--kinds K,K]
                                            Stream accessibility events
  xa11y enable-web-accessibility (--app NAME | --pid PID)
                                            Turn on a Chromium/Electron app's
                                            web content tree (macOS) and wait
                                            for it to appear
  xa11y capabilities [NAME | AREA: ...] [--backend native|uia|ia2] [--platform linux|macos|windows]
                   [--os-version V] [--json]
                                            What this platform (or another)
                                            can report: supported, partial or
                                            unsupported, per feature

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
      Kinds are platform-specific though the list is not: menu_bar,
      status_items and dock are macOS only, taskbar is Windows only,
      panel is Linux only, desktop and flyout are macOS and Windows.
      `xa11y shell` lists what this machine actually has.
  --backend B                               Accessibility source for an
                                            application: {backends} (default:
                                            XA11Y_ACCESSIBILITY_SOURCE, else
                                            native; uia and ia2 are Windows
                                            only). Every command that takes
                                            --app takes it, and so does apps;
                                            not with --shell
  A flag a command does not take is a usage error, not ignored.

Text (offsets are UTF-16 code units: an emoji outside the BMP is two):
  OP is one of {text_ops}.
  info       length, caret, and the selected ranges
  read       the text of --range (default: all of it), at most --limit units
             (default {read_default}, max {read_max}); `next_start` pages on
  find       the --occurrence-th (0-based) exact match of --find, searched
             within --range when given; `found: false` when there is none
  range-at   the --unit (character, word, line, paragraph) at --offset
  object-at  the embedded control or link at --offset, as `object` (one line
             in the form find prints), or null when there is none
  lines     rendered lines of --range, --limit per page (default {page})
  bounds     screen rectangles of --range, one per rendered line
  runs       formatting runs of --range, --limit per page (default {page})
  read/lines/bounds/runs take their range as --range START,END or as
  --find TEXT [--occurrence N]: the match is the range. Every range in the
  output carries its `text`, so copy the numbers rather than computing them.
  The selector must match exactly one element; it waits (up to the default
  timeout) for one to be present, not for it to be visible.

Table (indices are 0-based; the selector must match exactly one element):
  cell-at              on a table: the cell covering --row/--column (a merged
                       cell answers for every slot it spans)
  column-headers       on a table: its column header cells
  row-headers          on a table: its row header cells
  column-header-cells  on a cell: the column headers that head it
  row-header-cells     on a cell: the row headers that head it
  selected-items       on a list, tree, table or tab list: what is selected
  A cell by position is also a selector: table_cell[row=\"2\"][column=\"0\"].

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

Agent guide:
  xa11y guide                               Print the guide for agents driving
                                            xa11y: selectors, auto-wait, result
                                            bounds, failure kinds. `xa11y mcp`
                                            serves the same text.

Compose a11y + input/screenshot via `find -o bounds|center`:
  region=$(xa11y find 'button[name=\"OK\"]' --app Safari -o bounds)
  xa11y screenshot --region \"$region\" --out button.png
  xa11y click --at \"$(xa11y find 'button[name=\"OK\"]' --app Safari -o center)\"
  Add --stable to that find when the target may still be animating into place.
  On Linux with the uinput input backend (Wayland sessions), element bounds
  are not screen coordinates: `-o center` fails there, and the way to act on
  an element is `xa11y action press SELECTOR ...` instead of a click.

Actions: press, focus, blur, toggle, expand, collapse, select, show-menu,
  scroll-into-view, increment, decrement,
  set-value (requires --value), type-text (requires --value),
  set-numeric-value (requires --value),
  select-text (requires --range START,END, in UTF-16 code units;
    --value START,END is accepted too, but not both),
  minimize, maximize, enter-fullscreen, restore, close, activate,
  move-to (requires --at X,Y), resize-to (requires --size W,H)
  --value, --range, --at and --size are refused on any verb not listed
  with them here.

Exit codes:
  0  success
  1  operation failed (app not found, no selector match, platform error)
  2  usage error (unknown flag, a flag the command does not take, bad flag
     value, missing or invalid argument)

When an action command (action, click, move, drag, scroll, key, type,
enable-web-accessibility) fails with exit 1, a `delivery:` line follows the
error on stderr: `not_delivered` (nothing reached the application, so a retry
is safe) or `unknown` (it may have been applied: re-read the state first).",
        // Derived, not spelled out: see `shell_kind_names`.
        kinds = shell_kind_names().join(", "),
        text_ops = TEXT_OPS.join(", "),
        table_ops = TABLE_OPS.join(", "),
        relation_kinds = relation_kind_names().join(", "),
        backends = BACKENDS.join(", "),
        read_default = TEXT_READ_DEFAULT_UNITS,
        read_max = TEXT_READ_MAX_UNITS,
        page = TEXT_PAGE_DEFAULT,
    )
}

// ── macOS Accessibility hint ───────────────────────────────────────────────

/// After an operation fails, offer the one fix that is a hop away in the same
/// terminal: the macOS TCC Accessibility grant. Asks only when stdin is
/// interactive — a prompt in a pipe, CI, or a binding launcher would hang the
/// caller, and one in `xa11y mcp` would read from the JSON-RPC wire — and
/// only for the specific denial [`xa11y_macos::MacOSProvider::new`] raises;
/// the Screen Recording denial and every other error keep the plain message.
#[cfg(target_os = "macos")]
fn offer_accessibility_settings_hint(command: &str, error: &CliError) {
    if command == "mcp" {
        // stdin is the protocol wire; reading from it would corrupt the
        // session even on an interactive TTY.
        return;
    }
    if !is_accessibility_permission_denied(error) {
        return;
    }
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        // Non-interactive (a script, CI, or a launcher that redirects stdin):
        // the error message already names the pane; prompting would just hang.
        return;
    }
    eprintln!();
    eprintln!(
        "Hint: grant Accessibility to the app running xa11y (the terminal it was launched from)"
    );
    eprintln!("{}", xa11y_macos::accessibility_grant_instructions());
    eprintln!("then restart the app (or the terminal).");
    eprint!("Open System Settings now? [y/N] ");
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_ok() && parse_yes(&answer) {
        // Best-effort: the original permission error is the failure being
        // reported. A failed `open` — an unavailable `open` binary, or a
        // macOS version that re-keys the deep-link scheme — must not bury it;
        // the pane name is already on stderr above.
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
}

#[cfg(not(target_os = "macos"))]
fn offer_accessibility_settings_hint(_command: &str, _error: &CliError) {}

/// True when `error` is the macOS TCC Accessibility denial that
/// [`xa11y_macos::MacOSProvider::new`] raises. The denial is recognized by
/// its instruction text, compared against the same builder the provider
/// crate uses to produce it — so a rewording in `xa11y-macos` (including the
/// macOS 27+ pane rename) updates both sides in the same change rather than
/// drifting.
#[cfg(target_os = "macos")]
fn is_accessibility_permission_denied(error: &CliError) -> bool {
    match error {
        CliError::Xa11y(crate::Error::PermissionDenied { instructions }) => {
            instructions == &xa11y_macos::accessibility_grant_instructions()
        }
        _ => false,
    }
}

/// The prompt's accept words. Anything else — including an empty line, the
/// `[y/N]` default — declines.
#[cfg(target_os = "macos")]
fn parse_yes(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

fn print_usage() {
    eprintln!("{}", usage_text());
}

// ── Argument helpers ────────────────────────────────────────────────────────

// Debug is needed so tests can `expect_err` on `parse_opts` results.
#[derive(Debug, Default)]
pub(crate) struct Opts {
    pub backend: Option<String>,
    pub app: Option<String>,
    pub pid: Option<u32>,
    /// Shell surface kind, as its snake_case spelling. Mutually exclusive with
    /// `app`; combines with `pid` to disambiguate same-kind surfaces.
    pub shell: Option<String>,
    pub value: Option<String>,
    // Input simulation / screenshot
    pub at: Option<String>,
    pub size: Option<String>,
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
    /// `find --stable`: wait for the single match's bounds to settle.
    pub stable: bool,
    /// `events --kinds a,b`: the event kinds to show.
    pub kinds: Option<String>,
    /// `tree --max-nodes N`: the node limit (default 2000).
    pub max_nodes: Option<usize>,
    /// `tree --max-depth N`: the depth limit (default none).
    pub max_depth: Option<usize>,
    /// `tree --detail K,K`: the extra reads per node, one entry per
    /// occurrence of the flag (each a comma-separated list), parsed by
    /// [`parse_tree_detail`].
    pub detail: Vec<String>,
    // `text` / `table`
    /// `--range START,END`: a text range in UTF-16 code units.
    pub range: Option<String>,
    /// `--find TEXT`: the needle of `text find`, or the text that names the
    /// range for the other `text` operations.
    pub find: Option<String>,
    /// `--occurrence N`: which match of `--find`, 0-based.
    pub occurrence: Option<usize>,
    /// `--offset N`: the offset `text range-at` looks at.
    pub offset: Option<u32>,
    /// `--unit U`: the unit `text range-at` returns.
    pub unit: Option<String>,
    /// `--limit N`: the page size of `text read` / `lines` / `runs`.
    pub limit: Option<usize>,
    /// `--row N` / `--column N`: the slot `table cell-at` looks up.
    pub row: Option<u32>,
    pub column: Option<u32>,
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

/// Every accessibility source `--backend` (and the MCP tools' `backend`)
/// accepts. The library's own parser is what finally interprets the name;
/// `backend_names_are_the_ones_the_library_accepts` holds the two together.
pub(crate) const BACKENDS: [&str; 3] = ["native", "uia", "ia2"];

/// Validate a backend name, so a typo is a usage error that names the
/// argument the caller typed (`--backend`, or MCP's `"backend"`) rather than
/// an operation failure naming `XA11Y_ACCESSIBILITY_SOURCE`, an environment
/// variable the caller never set.
pub(crate) fn parse_backend(raw: &str, name: &str) -> CliResult<String> {
    if BACKENDS.contains(&raw) {
        Ok(raw.to_string())
    } else {
        Err(CliError::Usage(format!(
            "unknown {name} value {raw:?} (expected {})",
            BACKENDS.join(", ")
        )))
    }
}

/// Every flag [`parse_opts`] recognises. A command accepts a subset of them,
/// declared in its [`CommandFlags`]; `known_flags_all_parse` keeps this list
/// and the parser's arms in step.
const KNOWN_FLAGS: &[&str] = &[
    "--backend",
    "--app",
    "--pid",
    "--shell",
    "--value",
    "--at",
    "--size",
    "--from",
    "--to",
    "--button",
    "--count",
    "--held",
    "--dx",
    "--dy",
    "--duration-ms",
    "--region",
    "--out",
    "--annotate",
    "--legend",
    "-o",
    "--range",
    "--find",
    "--occurrence",
    "--offset",
    "--unit",
    "--limit",
    "--kinds",
    "--max-nodes",
    "--max-depth",
    "--detail",
    "--row",
    "--column",
    "--stable",
];

/// What one command's argument list may contain.
///
/// Every command shares one flag parser, so without this a flag meant for one
/// command parsed cleanly on all of them and was then ignored by the ones that
/// do not read it: `tree --limit 5` printed the whole tree, `apps --backend
/// uia` listed through the default source. Each command declares the flags
/// its handler reads, and anything else is a usage error (tenet 1).
pub(crate) struct CommandFlags {
    /// The command's name, as the error message spells it.
    pub command: &'static str,
    /// The flags the handler reads. Anything else in [`KNOWN_FLAGS`] is
    /// refused.
    pub flags: &'static [&'static str],
    /// The most positional arguments the handler reads; `None` for no limit.
    pub max_positional: Option<usize>,
    /// Positionals are literal text (`type TEXT`), so one that starts with
    /// `--` is text to type rather than a misspelt flag.
    pub literal_positionals: bool,
}

/// The target flags every element-reading command takes, plus `extra`.
macro_rules! target_flags {
    ($($extra:literal),* $(,)?) => {
        &["--app", "--pid", "--shell", "--backend" $(, $extra)*]
    };
}

const APPS_FLAGS: CommandFlags = CommandFlags {
    command: "apps",
    flags: &["--backend"],
    max_positional: Some(0),
    literal_positionals: false,
};
// `windows`, `enable-web-accessibility` and `events` take `--shell` only so
// that each can refuse it with a message saying why.
const WINDOWS_FLAGS: CommandFlags = CommandFlags {
    command: "windows",
    flags: target_flags!(),
    max_positional: Some(0),
    literal_positionals: false,
};
const ENABLE_WEB_FLAGS: CommandFlags = CommandFlags {
    command: "enable-web-accessibility",
    flags: target_flags!(),
    max_positional: Some(0),
    literal_positionals: false,
};
const TREE_FLAGS: CommandFlags = CommandFlags {
    command: "tree",
    flags: target_flags!("--max-nodes", "--max-depth", "--detail"),
    max_positional: Some(0),
    literal_positionals: false,
};
const FIND_FLAGS: CommandFlags = CommandFlags {
    command: "find",
    flags: target_flags!("-o", "--stable"),
    max_positional: Some(1),
    literal_positionals: false,
};
const ACTION_FLAGS: CommandFlags = CommandFlags {
    command: "action",
    flags: target_flags!("--value", "--range", "--at", "--size"),
    max_positional: Some(2),
    literal_positionals: false,
};
const TEXT_FLAGS: CommandFlags = CommandFlags {
    command: "text",
    flags: target_flags!(
        "--range",
        "--find",
        "--occurrence",
        "--offset",
        "--unit",
        "--limit"
    ),
    max_positional: Some(2),
    literal_positionals: false,
};
const TABLE_FLAGS: CommandFlags = CommandFlags {
    command: "table",
    flags: target_flags!("--row", "--column"),
    max_positional: Some(2),
    literal_positionals: false,
};
const RELATIONS_FLAGS: CommandFlags = CommandFlags {
    command: "relations",
    flags: target_flags!(),
    max_positional: None,
    literal_positionals: false,
};
const EVENTS_FLAGS: CommandFlags = CommandFlags {
    command: "events",
    flags: target_flags!("--kinds"),
    max_positional: Some(0),
    literal_positionals: false,
};
const CLICK_FLAGS: CommandFlags = CommandFlags {
    command: "click",
    flags: &["--at", "--button", "--count", "--held"],
    max_positional: Some(0),
    literal_positionals: false,
};
const MOVE_FLAGS: CommandFlags = CommandFlags {
    command: "move",
    flags: &["--at"],
    max_positional: Some(0),
    literal_positionals: false,
};
const DRAG_FLAGS: CommandFlags = CommandFlags {
    command: "drag",
    flags: &["--from", "--to", "--button", "--duration-ms", "--held"],
    max_positional: Some(0),
    literal_positionals: false,
};
const SCROLL_FLAGS: CommandFlags = CommandFlags {
    command: "scroll",
    flags: &["--at", "--dx", "--dy"],
    max_positional: Some(0),
    literal_positionals: false,
};
const KEY_FLAGS: CommandFlags = CommandFlags {
    command: "key",
    flags: &["--held"],
    max_positional: Some(1),
    literal_positionals: false,
};
const TYPE_FLAGS: CommandFlags = CommandFlags {
    command: "type",
    flags: &[],
    max_positional: Some(1),
    literal_positionals: true,
};
const SCREENSHOT_FLAGS: CommandFlags = CommandFlags {
    command: "screenshot",
    flags: target_flags!("--region", "--out", "--annotate", "--legend"),
    max_positional: Some(0),
    literal_positionals: false,
};

/// Parse `args` for the command `spec` describes: [`parse_opts`], then refuse
/// every flag the command does not read, any unrecognised `--flag`, and any
/// positional argument beyond what the command takes.
///
/// A refused flag is reported before its value is parsed, so `click --pid x`
/// says `--pid` does not apply rather than that `x` is not a pid.
pub(crate) fn parse_command_opts(
    spec: &CommandFlags,
    args: &[String],
) -> CliResult<(Opts, Vec<String>)> {
    let accepted = || {
        if spec.flags.is_empty() {
            format!("`xa11y {}` takes no flags", spec.command)
        } else {
            format!("`xa11y {}` takes {}", spec.command, spec.flags.join(", "))
        }
    };
    let (opts, positional) = parse_opts_checked(args, &|flag| {
        if spec.flags.contains(&flag) {
            Ok(())
        } else {
            Err(CliError::Usage(format!(
                "{flag} does not apply to `xa11y {}`; {}",
                spec.command,
                accepted()
            )))
        }
    })?;
    if !spec.literal_positionals {
        if let Some(flag) = positional
            .iter()
            .find(|p| p.starts_with("--") && p.len() > 2)
        {
            return Err(CliError::Usage(format!(
                "unknown flag {flag}; {}",
                accepted()
            )));
        }
    }
    if let Some(max) = spec.max_positional {
        if let Some(extra) = positional.get(max) {
            return Err(CliError::Usage(format!(
                "xa11y {}: unexpected argument: {extra}",
                spec.command
            )));
        }
    }
    Ok((opts, positional))
}

/// Parse known flags from a slice, returning the parsed Opts and the
/// remaining positional arguments.
///
/// Accepts every flag in [`KNOWN_FLAGS`]; commands go through
/// [`parse_command_opts`], which narrows that to the flags each one reads.
/// Unknown flags are left in the positional output rather than swallowed.
/// Known flags require a value: a trailing flag or an unparsable numeric
/// value is a usage error, not a silently-absent option.
///
/// Test-only: every command parses through [`parse_command_opts`], and this
/// unrestricted form is what the per-flag parsing tests exercise.
#[cfg(test)]
pub(crate) fn parse_opts(args: &[String]) -> CliResult<(Opts, Vec<String>)> {
    parse_opts_checked(args, &|_| Ok(()))
}

/// [`parse_opts`], asking `allow` about each recognised flag before its value
/// is read.
fn parse_opts_checked(
    args: &[String],
    allow: &dyn Fn(&str) -> CliResult<()>,
) -> CliResult<(Opts, Vec<String>)> {
    let mut opts = Opts::default();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(flag) = KNOWN_FLAGS.iter().find(|f| **f == args[i]) {
            allow(flag)?;
        }
        match args[i].as_str() {
            "--backend" => {
                i += 1;
                opts.backend = Some(parse_backend(
                    flag_value(args, i, "--backend")?,
                    "--backend",
                )?);
            }
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
            "--size" => {
                i += 1;
                opts.size = Some(flag_value(args, i, "--size")?.to_string());
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
            "--range" => {
                i += 1;
                opts.range = Some(flag_value(args, i, "--range")?.to_string());
            }
            "--find" => {
                i += 1;
                opts.find = Some(flag_value(args, i, "--find")?.to_string());
            }
            "--occurrence" => {
                i += 1;
                opts.occurrence = Some(flag_value_parsed(
                    args,
                    i,
                    "--occurrence",
                    "a 0-based match index",
                )?);
            }
            "--offset" => {
                i += 1;
                opts.offset = Some(flag_value_parsed(
                    args,
                    i,
                    "--offset",
                    "a UTF-16 code-unit offset",
                )?);
            }
            "--unit" => {
                i += 1;
                opts.unit = Some(flag_value(args, i, "--unit")?.to_string());
            }
            "--limit" => {
                i += 1;
                opts.limit = Some(flag_value_parsed(args, i, "--limit", "a positive integer")?);
            }
            "--kinds" => {
                i += 1;
                opts.kinds = Some(flag_value(args, i, "--kinds")?.to_string());
            }
            "--max-nodes" => {
                i += 1;
                opts.max_nodes = Some(flag_value_parsed(
                    args,
                    i,
                    "--max-nodes",
                    "a positive integer",
                )?);
            }
            "--max-depth" => {
                i += 1;
                opts.max_depth = Some(flag_value_parsed(
                    args,
                    i,
                    "--max-depth",
                    "a 0-based depth",
                )?);
            }
            // Repeatable, like `--annotate`: a second occurrence adds to the
            // first rather than silently replacing it.
            "--detail" => {
                i += 1;
                opts.detail
                    .push(flag_value(args, i, "--detail")?.to_string());
            }
            "--row" => {
                i += 1;
                opts.row = Some(flag_value_parsed(args, i, "--row", "a 0-based row index")?);
            }
            "--column" => {
                i += 1;
                opts.column = Some(flag_value_parsed(
                    args,
                    i,
                    "--column",
                    "a 0-based column index",
                )?);
            }
            // A switch: takes no value.
            "--stable" => opts.stable = true,
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

/// Parse a `W,H` size for `resize-to`. Width and height use desktop units and
/// must be positive — a 0-sized window is not a valid resize request.
pub(crate) fn parse_size_arg(s: &str, ctx: &str) -> CliResult<(u32, u32)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return Err(CliError::Usage(format!("{ctx} must be W,H (got: {s})")));
    }
    let width: u32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid W in {ctx}: {}", parts[0])))?;
    let height: u32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid H in {ctx}: {}", parts[1])))?;
    if width == 0 || height == 0 {
        return Err(CliError::Usage(format!(
            "{ctx} must be W,H with positive values (got: {s})"
        )));
    }
    Ok((width, height))
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

fn selected_provider(opts: &Opts) -> CliResult<std::sync::Arc<dyn Provider>> {
    Ok(match opts.backend.as_deref() {
        Some(b) => crate::provider_for_backend(b)?,
        None => crate::provider()?,
    })
}

pub(crate) fn resolve_app(opts: &Opts) -> CliResult<App> {
    if let Some(name) = &opts.app {
        Ok(App::by_name_with(
            selected_provider(opts)?,
            name,
            std::time::Duration::ZERO,
        )?)
    } else if let Some(pid) = opts.pid {
        Ok(App::by_pid_with(
            selected_provider(opts)?,
            pid,
            std::time::Duration::ZERO,
        )?)
    } else {
        Err(CliError::Usage("specify --app NAME or --pid PID".into()))
    }
}

/// Resolve the application(s) a `--app` / `--pid` filter selects.
///
/// Both filters yield exactly one `App` entry. `--app` is a singular name
/// lookup; `--pid` resolves the process directly via `App::by_pid` — no
/// `App::list()` foreground query — and the entry's `windows()` stays
/// process-complete on Linux, because `App::windows_with` merges every
/// same-PID AT-SPI entry itself (a pid filter reaches the same window set
/// without collecting each entry here). A pid with no entry is the
/// not-matched error, surfaced rather than an empty listing.
pub(crate) fn resolve_apps(opts: &Opts) -> CliResult<Vec<App>> {
    if let Some(name) = &opts.app {
        Ok(vec![App::by_name_with(
            selected_provider(opts)?,
            name,
            std::time::Duration::ZERO,
        )?])
    } else if let Some(pid) = opts.pid {
        // `App::list` would run an unrelated foreground query (focused_app)
        // whose failure could reject a PID whose app and windows are
        // otherwise accessible, and would re-run the same-pid merge once per
        // matching entry. The direct lookup needs neither.
        Ok(vec![App::by_pid_with(
            selected_provider(opts)?,
            pid,
            std::time::Duration::ZERO,
        )?])
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
    /// A bounded [`Capture`] of the target's tree, rooted at its root.
    pub(crate) fn capture(&self, options: &CaptureOptions) -> crate::Result<Capture> {
        match self {
            Target::App(app) => app.capture(options),
            Target::Shell(surface) => surface.capture(options),
        }
    }

    /// A [`Locator`] rooted at the target.
    pub(crate) fn locator(&self, selector: &str) -> Locator {
        match self {
            Target::App(app) => app.locator(selector),
            Target::Shell(surface) => surface.locator(selector),
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
    if opts.shell.is_some() && opts.backend.is_some() {
        return Err(CliError::Usage(
            "backend selection applies to application attachments, not shell surfaces".into(),
        ));
    }
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

/// One element on one line. Text fields are quoted and escaped (`{:?}`), so
/// a multi-line value cannot split a `tree` node across lines or forge one.
pub(crate) fn format_element_oneline(el: &ElementData) -> String {
    let mut parts = Vec::new();

    parts.push(el.role.to_snake_case().to_string());

    if let Some(name) = &el.name {
        parts.push(format!("{name:?}"));
    }

    if let Some(value) = &el.value {
        parts.push(format!("value={value:?}"));
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
        parts.push(format!("description={desc:?}"));
    }

    // Table and collection positions, spelled as the selector attributes that
    // match them (`table_cell[row="1"][column="2"]`), so a line can be turned
    // into a selector by copying. Only what the platform reported is shown.
    let positions = format_positions(el);
    if !positions.is_empty() {
        parts.push(positions);
    }

    // Document semantics (#476), spelled as their selector attributes.
    if let Some(level) = el.heading_level {
        parts.push(format!("heading_level={level}"));
    }
    if let Some(landmark) = el.landmark_type {
        parts.push(format!("landmark_type={landmark}"));
    }
    if let Some(web_role) = &el.web_role {
        parts.push(format!("web_role=\"{web_role}\""));
    }
    if let Some(url) = &el.link_url {
        parts.push(format!("url=\"{url}\""));
    }
    if let Some(politeness) = el.live_region.as_ref().and_then(|l| l.politeness) {
        parts.push(format!("live={}", politeness.to_snake_case()));
    }

    // States. A hidden element says why (`hidden=scrolled_out`), which is
    // what decides how to make it visible (issue #590).
    let hidden = match el.states.hidden_reason {
        Some(reason) => format!("hidden={reason}"),
        None => "hidden".to_string(),
    };
    let mut states = Vec::new();
    if el.states.enabled {
        states.push("enabled");
    } else {
        states.push("disabled");
    }
    if el.states.visible {
        states.push("visible");
    } else {
        states.push(hidden.as_str());
    }
    if el.states.focused {
        states.push("focused");
    }
    if el.states.active {
        states.push("active");
    }
    // Window states render only when the platform reports them (Some). A
    // `None` — unknown/not a window — stays silent rather than rendering a
    // guessed `not minimized`-style label.
    if el.states.minimized == Some(true) {
        states.push("minimized");
    }
    if el.states.maximized == Some(true) {
        states.push("maximized");
    }
    if el.states.fullscreen == Some(true) {
        states.push("fullscreen");
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
    if el.states.password {
        states.push("password");
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

/// The table / cell / row / collection fields of `el` as `attr=value` pairs,
/// named after the selector attributes that match them, so a line can be
/// pasted back into a selector. Empty when none is reported.
pub(crate) fn format_positions(el: &ElementData) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            out.push(format!("{name}={v}"));
        }
    };
    if let Some(t) = &el.table {
        push("row_count", t.row_count.map(|c| c.to_string()));
        push("column_count", t.column_count.map(|c| c.to_string()));
    }
    if let Some(c) = &el.cell {
        push("row", c.row.map(|n| n.to_string()));
        push("column", c.column.map(|n| n.to_string()));
        push("row_span", c.row_span.map(|n| n.to_string()));
        push("column_span", c.column_span.map(|n| n.to_string()));
        push("header", c.header.map(|h| h.to_snake_case().to_string()));
    } else if let Some(r) = &el.table_row {
        // A row's index matches the selector attribute `row`, as a cell's
        // row does.
        push("row", r.index.map(|n| n.to_string()));
    }
    if let Some(c) = &el.collection {
        push("index", c.index.map(|n| n.to_string()));
        push("size", c.size.map(|n| n.to_string()));
        push("depth", c.depth.map(|n| n.to_string()));
    }
    out.join(" ")
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The agent guide, embedded so that what an agent reads always describes the
/// binary it is talking to.
///
/// `xa11y guide` prints it and `xa11y mcp` serves it as a resource and a
/// prompt. Its one source is `xa11y/AGENT_GUIDE.md`; the docs site page is
/// rendered from that file by `cargo xtask sync-agent-guide`, and CI fails
/// when the two differ.
pub(crate) const AGENT_GUIDE: &str = include_str!("../AGENT_GUIDE.md");

fn cmd_guide(args: &[String]) -> CliResult<()> {
    // Takes no arguments; one passed anyway is a mistake worth reporting
    // rather than ignoring (tenet 1).
    if let Some(extra) = args.first() {
        return Err(CliError::Usage(format!(
            "xa11y guide takes no arguments (got {extra:?})"
        )));
    }
    print!("{AGENT_GUIDE}");
    Ok(())
}

/// The capability table for `platform` (default: the running one), narrowed
/// to `os_version` when given. Shared by `xa11y capabilities` and the MCP
/// `capabilities` tool; prints nothing.
///
/// A bad platform name or version is a usage error: it is the caller's
/// argument, not an operation that failed.
///
/// `backend` is the accessibility source to describe (already validated by
/// [`parse_backend`] under the caller's own spelling of the argument); `None`
/// keeps the library default.
pub(crate) fn resolve_capabilities(
    platform: Option<&str>,
    os_version: Option<&str>,
    backend: Option<&str>,
) -> CliResult<Capabilities> {
    let usage = |e: Error| match e {
        Error::InvalidActionData { message, .. } => CliError::Usage(message),
        other => CliError::Xa11y(other),
    };
    let platform = platform.map(Platform::parse).transpose().map_err(usage)?;
    crate::capabilities_for_backend(platform, os_version, backend).map_err(usage)
}

/// The entries of `caps` that `filters` select: each filter is a feature
/// name (`event:menu_opened`) or an area ending in `:` (`window:`). No
/// filters selects everything. A filter that selects nothing is a usage
/// error naming the closest known names, so a typo is not read as "no such
/// capability".
pub(crate) fn select_capabilities<'a>(
    caps: &'a Capabilities,
    filters: &[String],
) -> CliResult<Vec<&'a Capability>> {
    if filters.is_empty() {
        return Ok(caps.entries().iter().collect());
    }
    let mut out: Vec<&Capability> = Vec::new();
    for filter in filters {
        let before = out.len();
        if filter.ends_with(':') {
            out.extend(
                caps.entries()
                    .iter()
                    .filter(|c| c.name.starts_with(filter.as_str())),
            );
            if out.len() == before {
                let areas: std::collections::BTreeSet<&str> = caps
                    .entries()
                    .iter()
                    .filter_map(|c| c.name.split(':').next())
                    .collect();
                return Err(CliError::Usage(format!(
                    "no capabilities in area {filter:?}; areas are: {}",
                    areas
                        .into_iter()
                        .map(|a| format!("{a}:"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        } else {
            match caps.get(filter) {
                Ok(c) => out.push(c),
                Err(Error::InvalidActionData { message, .. }) => {
                    return Err(CliError::Usage(message))
                }
                Err(other) => return Err(CliError::Xa11y(other)),
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.name));
    Ok(out)
}

/// JSON form of one capability, the same for `--json` and MCP.
pub(crate) fn capability_json(c: &Capability) -> serde_json::Value {
    serde_json::json!({
        "name": c.name,
        "support": c.support.to_snake_case(),
        "note": c.note,
        "since": c.since,
        "description": c.description,
    })
}

/// `xa11y capabilities`: what a platform's backend can report.
///
/// Text output is one `name\tsupport[\tnote]` line per feature after a
/// `platform:` header line; `--json` prints one object.
fn cmd_capabilities(args: &[String]) -> CliResult<()> {
    let mut platform = None;
    let mut backend = None;
    let mut os_version = None;
    let mut json = false;
    let mut filters = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                i += 1;
                backend = Some(parse_backend(
                    flag_value(args, i, "--backend")?,
                    "--backend",
                )?);
            }
            "--platform" => {
                i += 1;
                platform = Some(flag_value(args, i, "--platform")?);
            }
            "--os-version" => {
                i += 1;
                os_version = Some(flag_value(args, i, "--os-version")?);
            }
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                return Err(CliError::Usage(format!(
                    "unknown flag {flag} for capabilities; expected --backend, --platform, \
                     --os-version or --json"
                )));
            }
            name => filters.push(name.to_string()),
        }
        i += 1;
    }
    let caps = resolve_capabilities(platform, os_version, backend.as_deref())?;
    let selected = select_capabilities(&caps, &filters)?;
    if json {
        let out = serde_json::json!({
            "platform": caps.platform.to_snake_case(),
            "backend": caps.backend,
            "os_version": caps.os_version,
            "capabilities": selected.iter().map(|c| capability_json(c)).collect::<Vec<_>>(),
        });
        let rendered = serde_json::to_string_pretty(&out).map_err(|e| {
            CliError::Xa11y(Error::Platform {
                code: -1,
                message: format!("serialize the capability table: {e}"),
            })
        })?;
        println!("{rendered}");
        return Ok(());
    }
    println!(
        "platform: {}\tos_version: {}",
        caps.platform.to_snake_case(),
        caps.os_version.as_deref().unwrap_or("-")
    );
    for c in selected {
        match &c.note {
            Some(note) => println!("{}\t{}\t{}", c.name, c.support.to_snake_case(), note),
            None => println!("{}\t{}", c.name, c.support.to_snake_case()),
        }
    }
    Ok(())
}

fn cmd_apps(args: &[String]) -> CliResult<()> {
    let (opts, _) = parse_command_opts(&APPS_FLAGS, args)?;
    let apps = App::list_with(selected_provider(&opts)?)?;
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

/// Stable dedup key for a top-level window's [`ElementData`], collapsing the
/// same window surfaced through several Application entries of one pid. Only
/// Linux multi-registers a pid (the AT-SPI registry can surface one process
/// twice); macOS dedups `App::list` by pid and Windows queries by pid, so
/// one Application entry per pid is the contract there.
///
/// Key on the per-enumeration `handle` everywhere except `bus_name`-carrying
/// (Linux) entries. The handle is a fresh monotonic counter, unique per built
/// node within a single listing, so it can never merge two distinct windows —
/// which is why macOS makes an id-based key wrong: `AXIdentifier` is what
/// `stable_id` reads there, and AppKit does not guarantee it unique per window
/// (Terminal's windows all answer `_NS:136`). Keying two real windows with the
/// same id to one entry silently drops one of them from `xa11y windows`.
/// Presentation data (shared title and bounds) is likewise not identity.
///
/// The dedup exists only for the Linux multi-registration case, reachable
/// purely through the `bus_name` marker the provider records in `raw`; only
/// there is a stable_id keyed on. The AT-SPI identity is really
/// `(bus_name, object path)`: two bus connections of one process can expose
/// *distinct* windows under the same object path, so a Linux `stable_id` is
/// scoped by the `bus_name`. A `bus_name`-carrying entry without a stable_id
/// falls back to the handle — that only happens outside multi-registration,
/// where there is nothing to collapse. The pid scopes the key so equal ids
/// from different processes never collide.
///
/// Shared by `cmd_windows` and the MCP `windows` tool so both surfaces agree
/// on what "the same window" means.
pub(crate) fn window_key(data: &ElementData) -> String {
    let identity = match data.raw.get("bus_name") {
        Some(serde_json::Value::String(bus)) => data
            .stable_id
            .clone()
            .map(|sid| format!("{bus}:{sid}"))
            // Identity-less duplicate stays duplicated, but that only happens
            // outside multi-registration, where there is nothing to collapse.
            .unwrap_or_else(|| format!("h{}", data.handle)),
        // Handle, not stable_id or (name, bounds): macOS `AXIdentifier` is
        // not unique per window, and presentation data is not identity —
        // either could merge two distinct same-process windows.
        _ => format!("h{}", data.handle),
    };
    format!("{}:{identity}", data.pid.unwrap_or(0))
}

/// `xa11y windows` — enumerate top-level windows (winlenium's list-windows).
///
/// Without a filter, all windows across all applications are listed. With
/// `--app NAME` / `--pid PID` only that application's windows are shown.
///
/// Failure policy: one app whose `windows()` fails (a dying process, a flaky
/// bridge) must not abort a listing that already holds healthy windows.
/// The successful windows are printed, each failure is reported on stderr
/// naming the app, and the command exits `1` so a script can tell a complete
/// listing from a partial one without parsing stderr — the same honest
/// partial result the MCP `windows` tool returns in its `errors` array
/// (tenet 1: the failure is surfaced, never swallowed).
fn cmd_windows(args: &[String]) -> CliResult<()> {
    let (opts, _) = parse_command_opts(&WINDOWS_FLAGS, args)?;
    // `--shell` names an OS shell surface (`xa11y shell`), which this command
    // does not enumerate. Silently ignoring the flag would list every
    // application's windows instead of the requested surface — a filter that
    // fell away instead of an unsupported target being refused (tenet 1).
    if opts.shell.is_some() {
        return Err(CliError::Usage(
            "usage: xa11y windows [--app NAME | --pid PID]; --shell is not supported here — \
             `xa11y shell` lists OS shell surfaces"
                .into(),
        ));
    }
    let apps = if opts.app.is_some() || opts.pid.is_some() {
        resolve_apps(&opts)?
    } else {
        App::list_with(selected_provider(&opts)?)?
    };

    // One Application node per process on every platform (the unified
    // contract), so listing each app's windows exactly once covers every
    // top-level window — the old one-entry-per-window Windows shape needed
    // per-pid dedup, which is gone with it.
    let mut windows: Vec<Element> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for app in &apps {
        match app.windows() {
            Ok(ws) => windows.extend(ws),
            Err(e) => errors.push(format!("{}: {e}", app.name)),
        }
    }

    // One Application node per process is the contract on Windows/macOS, but
    // Linux multi-registration can surface several entries for one pid whose
    // windows overlap. Deduplicate by stable identity so a window is never
    // printed twice and the count below is honest.
    let mut seen = HashSet::new();
    windows.retain(|w| seen.insert(window_key(w.data())));

    if windows.is_empty() {
        // With errors, "No windows found." would be a lie — windows may well
        // exist, just not enumerable; stderr carries the real reason.
        if errors.is_empty() {
            println!("No windows found.");
        }
    } else {
        for w in &windows {
            println!("{}", format_element_oneline(w));
        }
        println!(
            "({} window{})",
            windows.len(),
            if windows.len() == 1 { "" } else { "s" }
        );
    }

    if errors.is_empty() {
        return Ok(());
    }
    for e in &errors {
        eprintln!("error: {e}");
    }
    Err(CliError::Xa11y(Error::Platform {
        code: -1,
        message: format!(
            "{} of {} application(s) could not be enumerated; see stderr for details",
            errors.len(),
            apps.len()
        ),
    }))
}

/// Resolve the `--app` / `--pid` target and turn on its web content's
/// accessibility tree, waiting up to the default timeout for the content.
///
/// The value-producing half shared with the MCP `enable_web_accessibility`
/// tool; [`cmd_enable_web_accessibility`] is the printing half.
pub(crate) fn enable_web_accessibility(opts: &Opts) -> CliResult<App> {
    // Read before the app lookup: an invalid XA11Y_DEFAULT_TIMEOUT is a
    // configuration error, and it must not be masked by a lookup failure.
    let timeout = crate::default_timeout()?;
    let app = resolve_app(opts)?;
    app.enable_web_accessibility(timeout)?;
    Ok(app)
}

fn cmd_enable_web_accessibility(args: &[String]) -> CliResult<()> {
    const USAGE: &str = "usage: xa11y enable-web-accessibility (--app NAME | --pid PID)";
    let (opts, _) = parse_command_opts(&ENABLE_WEB_FLAGS, args)?;
    // A shell surface has no web content switch; refuse the flag rather than
    // ignore it (tenet 1).
    if opts.shell.is_some() {
        return Err(CliError::Usage(format!(
            "{USAGE}; --shell is not supported here"
        )));
    }
    let app = enable_web_accessibility(&opts)?;
    let pid = app.pid.map(|p| format!(" (pid={p})")).unwrap_or_default();
    println!("web accessibility enabled for \"{}\"{pid}", app.name);
    Ok(())
}

/// What `tree` reads per node besides structure: `xa11y tree --detail` and
/// the MCP `tree` tool's `detail`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TreeDetail {
    /// Each element's text (`text`, and implied by `formatting`).
    pub text: bool,
    /// Each element's formatting runs (`formatting`).
    pub formatting: bool,
    /// Each element's relations (`relations`).
    pub relations: bool,
}

impl TreeDetail {
    /// `options` with these reads switched on.
    pub(crate) fn apply(self, options: CaptureOptions) -> CaptureOptions {
        options
            .text(self.text)
            .formatting(self.formatting)
            .relations(self.relations)
    }
}

/// Every `detail` name `tree` accepts, on both surfaces.
pub(crate) const TREE_DETAILS: &[&str] = &["text", "formatting", "relations"];

/// Parse `detail` names into a [`TreeDetail`]. `name` spells the argument as
/// the calling surface does (`--detail`, or MCP's `"detail"`). An unknown
/// name is a usage error, never ignored: a typo'd `relation` silently reading
/// no relations would look like an element that has none.
pub(crate) fn parse_tree_detail<'a>(
    items: impl IntoIterator<Item = &'a str>,
    name: &str,
) -> CliResult<TreeDetail> {
    let mut detail = TreeDetail::default();
    for item in items {
        match item {
            "text" => detail.text = true,
            // Runs are offsets into the text, which is meaningless without it.
            "formatting" => {
                detail.text = true;
                detail.formatting = true;
            }
            "relations" => detail.relations = true,
            other => {
                return Err(CliError::Usage(format!(
                    "unknown {name} {other:?}; expected one of {}",
                    TREE_DETAILS.join(", ")
                )))
            }
        }
    }
    Ok(detail)
}

fn cmd_tree(args: &[String]) -> CliResult<()> {
    let (opts, _) = parse_command_opts(&TREE_FLAGS, args)?;
    let max_nodes = opts
        .max_nodes
        .unwrap_or(xa11y_core::capture::DEFAULT_MAX_NODES);
    if max_nodes == 0 {
        return Err(CliError::Usage(
            "--max-nodes must be at least 1: the root is a node".into(),
        ));
    }
    // `--detail text,relations`: each occurrence is a comma-separated list.
    let detail = parse_tree_detail(
        opts.detail
            .iter()
            .flat_map(|list| list.split(','))
            .map(str::trim),
        "--detail",
    )?;
    let target = resolve_target(&opts)?;
    let mut options = detail.apply(CaptureOptions::new().max_nodes(max_nodes));
    if let Some(depth) = opts.max_depth {
        options = options.max_depth(depth);
    }
    let cap = target.capture(&options)?;
    for line in format_capture(&cap)? {
        println!("{line}");
    }
    Ok(())
}

/// Render a capture as the `tree` command prints it: one element per line
/// with box-drawing connectors. A node whose children were cut ends with a
/// `… N more` line naming the limit, and a capture cut by a limit ends with a
/// summary line, so a shortened tree never reads as a complete one.
///
/// What `--detail` read sits under its element's line, before its children,
/// each line led by the reading's name (`text`, `run`, a relation kind) so
/// it cannot be mistaken for a child.
pub(crate) fn format_capture(cap: &Capture) -> CliResult<Vec<String>> {
    fn walk(
        cap: &Capture,
        index: usize,
        prefix: &str,
        is_last: bool,
        out: &mut Vec<String>,
    ) -> CliResult<()> {
        let node = &cap.nodes[index];
        let is_root = index == 0;
        let connector = match (is_root, is_last) {
            (true, _) => "",
            (false, true) => "└── ",
            (false, false) => "├── ",
        };
        out.push(format!(
            "{prefix}{connector}{}",
            format_element_oneline(node)
        ));
        let child_prefix = match (is_root, is_last) {
            (true, _) => prefix.to_string(),
            (false, true) => format!("{prefix}    "),
            (false, false) => format!("{prefix}│   "),
        };
        let errors: Vec<&CaptureError> = node
            .errors
            .iter()
            .filter(|e| matches!(e.step, CaptureStep::Children | CaptureStep::Cycle))
            .collect();
        let missing = match node.children_count {
            Some(count) if node.children_truncated => count.saturating_sub(node.children.len()),
            _ => 0,
        };
        let not_read =
            node.children_truncated && node.children_count.is_none() && errors.is_empty();
        let tail = errors.len() + usize::from(missing > 0 || not_read);
        // The branch line keeps running past the detail lines when anything
        // hangs below them.
        let bar = if node.children.is_empty() && tail == 0 {
            "  "
        } else {
            "│ "
        };
        for line in detail_lines(cap, node)? {
            out.push(format!("{child_prefix}{bar}{line}"));
        }
        for (i, &child) in node.children.iter().enumerate() {
            let last = i + 1 == node.children.len() && tail == 0;
            walk(cap, child, &child_prefix, last, out)?;
        }
        let mut left = tail;
        let mut tail_line = |text: String, out: &mut Vec<String>| {
            left -= 1;
            let connector = if left == 0 {
                "└── "
            } else {
                "├── "
            };
            out.push(format!("{child_prefix}{connector}{text}"));
        };
        for err in errors {
            tail_line(format!("<error: {}>", err.error), out);
        }
        if missing > 0 {
            let why = if cap.options.max_depth.is_some_and(|d| node.depth >= d) {
                "--max-depth"
            } else {
                "--max-nodes"
            };
            tail_line(format!("… {missing} more (cut by {why})"), out);
        } else if not_read {
            tail_line("… children not read (out of time)".into(), out);
        }
        Ok(())
    }

    let mut out = Vec::new();
    walk(cap, 0, "", true, &mut out)?;
    let t = cap.truncated;
    if t.any() {
        let mut why = Vec::new();
        if t.by_nodes {
            why.push(format!(
                "the node limit of {} (--max-nodes)",
                cap.options.max_nodes
            ));
        }
        if t.by_depth {
            why.push("the depth limit (--max-depth)".to_string());
        }
        if t.by_text {
            why.push(format!(
                "the text limit of {} bytes (--detail text)",
                cap.options.max_text_bytes
            ));
        }
        if t.by_formatting {
            why.push(format!(
                "the run limit of {} (--detail formatting)",
                cap.options.max_runs
            ));
        }
        if t.by_time {
            why.push("the time budget".to_string());
        }
        if t.by_cancel {
            why.push("cancellation".to_string());
        }
        out.push(format!(
            "(truncated: {} nodes shown, cut by {})",
            cap.nodes.len(),
            why.join(" and ")
        ));
    }
    Ok(out)
}

/// The `--detail` readings of one captured node, one line each: its text,
/// its formatting runs, its relations, and any of those reads that failed.
/// Empty when no detail was asked for.
fn detail_lines(cap: &Capture, node: &xa11y_core::CapturedNode) -> CliResult<Vec<String>> {
    let mut lines = Vec::new();
    for err in &node.errors {
        let what = match err.step {
            CaptureStep::Text => "text",
            CaptureStep::Formatting => "formatting",
            CaptureStep::Relations => "relations",
            // Children and cycles are reported in the child list; identity
            // only feeds cycle detection and relation resolution, whose
            // failures are reported there.
            _ => continue,
        };
        lines.push(format!("<{what} error: {}>", err.error));
    }
    if let Some(text) = &node.text {
        // Debug-quoted, so a newline in the text cannot start a line that
        // reads as another element.
        lines.push(format!(
            "text ({} units{}): {:?}",
            text.length,
            if text.truncated {
                ", cut by the text limit"
            } else {
                ""
            },
            text.content
        ));
        if let Some(runs) = &text.runs {
            for run in runs {
                lines.push(format!(
                    "run [{},{}): {}",
                    run.range.start,
                    run.range.end,
                    json_without_nulls("text attributes", &run.attributes)?
                ));
            }
            if text.runs_truncated {
                lines.push("… more runs (cut by the run limit)".to_string());
            }
        }
    }
    for (kind, targets) in &node.relations {
        for target in targets {
            let described = match target {
                xa11y_core::RelationTarget::Node(i) => describe_brief(&cap.nodes[*i].element),
                xa11y_core::RelationTarget::Outside(el) => {
                    format!("{} (outside the tree)", describe_brief(el))
                }
            };
            lines.push(format!("{} → {described}", kind.to_snake_case()));
        }
    }
    Ok(lines)
}

/// `role "name"`, the part of an element's line that tells a relation target
/// apart; the target's full line is in the tree when it was captured.
fn describe_brief(el: &ElementData) -> String {
    match &el.name {
        Some(name) => format!("{} \"{name}\"", el.role.to_snake_case()),
        None => el.role.to_snake_case().to_string(),
    }
}

fn cmd_find(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_command_opts(&FIND_FLAGS, args)?;
    let selector = positional.first().ok_or_else(|| {
        CliError::Usage(
            "usage: xa11y find SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [-o pretty|bounds|center] [--stable]"
                .into(),
        )
    })?;
    // Validated before the target is resolved, so a typo'd format is a usage
    // error rather than something reported after a full stability wait.
    let fmt = opts.output_format.as_deref().unwrap_or("pretty");
    if !matches!(fmt, "pretty" | "bounds" | "center") {
        return Err(CliError::Usage(format!(
            "unknown -o format: {fmt} (expected pretty|bounds|center)"
        )));
    }
    // Before the target is resolved, for the same reason: on a backend where
    // element bounds are not screen coordinates (Wayland), `-o center` exists
    // only to feed `click --at`, which would then land somewhere else. Refuse
    // it rather than print a point that is not one (issue #573).
    if fmt == "center" {
        check_center_output(element_bounds_are_pointer_targets())?;
    }

    let target = resolve_target(&opts)?;
    let elements = if opts.stable {
        match find_stable(&target, selector)? {
            Ok(element) => vec![element],
            Err(matches) => {
                return Err(ambiguous_selector(selector, &matches, 8, |el| {
                    format_element_oneline(el)
                }))
            }
        }
    } else {
        target.locator(selector).elements()?
    };
    if elements.is_empty() {
        return Err(CliError::NotFound(format!(
            "no elements matched selector: {selector}"
        )));
    }
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

/// Resolve `selector` under `target` to its single match once that match is
/// visible and its bounds are observed unchanged for at least 750 ms
/// ([`Locator::wait_stable`]), waiting up to the default timeout.
///
/// The value-producing half of `find --stable`, shared by the CLI and the
/// MCP `find` tool. `Ok(Err(matches))` means the selector matched more than
/// one element: the caller renders that as its own ambiguity error. It is
/// checked before the wait, so a selector that is ambiguous from the start is
/// refused at once, and again after it, because the wait follows the first
/// match only.
///
/// A wait that times out on a selector that matches nothing reports the
/// diagnosed not-found error ([`Locator::element`]'s), the same failure a
/// plain `find` miss gives; any other timeout keeps its stability diagnosis.
pub(crate) fn find_stable(
    target: &Target,
    selector: &str,
) -> CliResult<std::result::Result<Element, Vec<Element>>> {
    let locator = target.locator(selector);
    let before = locator.elements()?;
    if before.len() > 1 {
        return Ok(Err(before));
    }
    let element = match locator.wait_stable(crate::default_timeout()?) {
        Ok(element) => element,
        Err(timeout @ Error::Timeout { .. }) => {
            return match locator.element() {
                // Nothing matches now: the diagnosed miss is the failure to
                // report (any other error from the re-query propagates).
                Err(miss @ Error::SelectorNotMatched { .. }) => {
                    Err(miss_after_stable_wait(miss, &timeout).into())
                }
                Err(other) => Err(other.into()),
                // Something matches; it just never held still.
                Ok(_) => Err(timeout.into()),
            };
        }
        Err(e) => return Err(e.into()),
    };
    let after = locator.elements()?;
    if after.len() > 1 {
        return Ok(Err(after));
    }
    Ok(Ok(element))
}

/// The not-found error a `--stable` wait ends with when nothing matches any
/// more. The miss carries the near-miss candidates and scope; the wait's own
/// timeout knew whether the selector matched at some point during it, and
/// that is kept as the miss's `last_observed` rather than dropped (tenet 6).
fn miss_after_stable_wait(miss: Error, timeout: &Error) -> Error {
    let waited = timeout
        .diagnosis()
        .and_then(|d| d.last_observed.as_deref())
        .filter(|seen| seen.contains("matched earlier"));
    let Some(waited) = waited else {
        return miss;
    };
    let mut d = miss.diagnosis().cloned().unwrap_or_default();
    d.last_observed = Some(match d.last_observed.take() {
        Some(now) => format!("{waited}; {now}"),
        None => waited.to_string(),
    });
    miss.diagnose(d)
}

/// The "matched more than one" failure for an operation that acts on exactly
/// one element, with a bounded candidate list (tenet 6). Shared by the CLI
/// and the MCP tools, which differ only in how a candidate is rendered and
/// how many are listed.
///
/// The list is bounded *here*, not only where it is serialized, so the
/// rendered message is bounded too.
pub(crate) fn ambiguous_selector(
    selector: &str,
    matches: &[Element],
    max_candidates: usize,
    describe: impl Fn(&Element) -> String,
) -> CliError {
    let mut candidates: Vec<String> = matches.iter().take(max_candidates).map(describe).collect();
    if matches.len() > max_candidates {
        candidates.push(format!(
            "… (+{} more matches)",
            matches.len() - max_candidates
        ));
    }
    CliError::Ambiguous {
        count: matches.len(),
        diagnosis: Box::new(
            Diagnosis::new()
                .selector(selector)
                .last_observed(format!("selector matched {} elements", matches.len()))
                .candidates(candidates),
        ),
    }
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

/// Whether an element's center is a valid pointer target for the active input
/// backend — the value-producing decision behind `find -o center` and the
/// `center` the MCP tools precompute (issue #573).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PointerCenter {
    /// Element bounds are screen coordinates: a center is a click target.
    Usable,
    /// Element bounds are not usable as pointer targets; the reason says why.
    Unavailable(String),
}

impl PointerCenter {
    /// Build from [`element_bounds_are_pointer_targets`]'s answer, or the
    /// failure to get one (an invalid `XA11Y_LINUX_INPUT_BACKEND`), which
    /// leaves the coordinate space unknown.
    pub(crate) fn from_capability(capability: Result<bool>) -> Self {
        match capability {
            Ok(true) => Self::Usable,
            Ok(false) => Self::Unavailable(ELEMENT_BOUNDS_NOT_SCREEN.to_string()),
            Err(e) => Self::Unavailable(format!(
                "could not tell whether element bounds are screen coordinates: {e}"
            )),
        }
    }

    pub(crate) fn is_usable(&self) -> bool {
        matches!(self, Self::Usable)
    }
}

/// Why an element's center is withheld on a backend whose bounds are not
/// screen coordinates. Mirrors the library's own refusal of element-targeted
/// pointer actions there (`InputProvider::element_bounds_are_screen_coordinates`).
pub(crate) const ELEMENT_BOUNDS_NOT_SCREEN: &str =
    "element bounds are not screen coordinates with this input backend (Linux \
     uinput, used on Wayland): applications do not know their global position, \
     so a point computed from an element's bounds would land somewhere else. Use \
     a semantic action (press, focus, ...) on the element instead";

/// Whether element bounds are screen coordinates, so a center computed from
/// them is a pointer target.
///
/// The same decision [`Mouse`](crate::input::Mouse) refuses element targets
/// by, read without building an input backend: on macOS and Windows bounds
/// are always screen coordinates, and on Linux it is the session the input
/// backend is chosen from (X11 yes, Wayland no). A machine that cannot
/// simulate input still reports correct centers; only the session decides.
pub(crate) fn element_bounds_are_pointer_targets() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        xa11y_linux::LinuxInputProvider::selected_backend_uses_screen_coordinates()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(true)
    }
}

/// The [`PointerCenter`] for this process's input backend.
pub(crate) fn pointer_center() -> PointerCenter {
    PointerCenter::from_capability(element_bounds_are_pointer_targets())
}

/// The `find -o center` refusal, or `Ok` when centers are pointer targets.
///
/// An operation failure (exit 1), not a usage error: the invocation is
/// well-formed, and it is the session that cannot honour it.
fn check_center_output(capability: Result<bool>) -> CliResult<()> {
    match capability {
        Ok(true) => Ok(()),
        Ok(false) => Err(CliError::Xa11y(Error::Unsupported {
            feature: format!(
                "find -o center: {ELEMENT_BOUNDS_NOT_SCREEN}, e.g. \
                 `xa11y action press SELECTOR --app NAME`"
            ),
        })),
        Err(e) => Err(CliError::Xa11y(e)),
    }
}

fn cmd_action(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_command_opts(&ACTION_FLAGS, args)?;
    if positional.len() < 2 {
        return Err(CliError::Usage(
            "usage: xa11y action ACTION SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [--value V] [--range START,END] [--at X,Y] [--size W,H]"
                .into(),
        ));
    }
    let action_name = &positional[0];
    let selector = &positional[1];
    let value = opts.value.clone();

    // Parse the payload before resolving the target: a missing or malformed
    // `--value` / `--at` / `--size` is a usage error, and it must surface
    // even when the target itself would also be unresolvable — and must not
    // spend an app-resolution / accessibility round-trip first (parse
    // arguments before the first OS call).
    let range = opts
        .range
        .as_deref()
        .map(|raw| parse_range_flag(raw, "--range"))
        .transpose()?;
    let action_args = parse_action_args(
        action_name,
        value.as_deref(),
        range,
        opts.at.as_deref(),
        opts.size.as_deref(),
        &CLI_ACTION_ARG_NAMES,
    )?;

    let target = resolve_target(&opts)?;
    let locator = target.locator(selector);
    perform_action(&locator, action_name, &action_args)?;
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
    "minimize",
    "maximize",
    "enter-fullscreen",
    "restore",
    "close",
    "activate",
    "move-to",
    "resize-to",
];

/// Actions that require a `--value` (MCP: `value`) argument.
pub(crate) const ACTIONS_REQUIRING_VALUE: &[&str] =
    &["set-value", "set-numeric-value", "type-text"];

/// Actions that require a text range: `--range START,END` (MCP: `start` and
/// `end`), or the same range as `--value START,END` (MCP: `value`).
pub(crate) const ACTIONS_REQUIRING_RANGE: &[&str] = &["select-text"];

/// Actions that require a geometry argument: `--at X,Y` (MCP: `at`) for
/// `move-to`, `--size W,H` (MCP: `size`) for `resize-to`.
pub(crate) const ACTIONS_REQUIRING_AT: &[&str] = &["move-to"];
pub(crate) const ACTIONS_REQUIRING_SIZE: &[&str] = &["resize-to"];

/// How a surface spells the action arguments, for usage errors.
pub(crate) struct ActionArgNames {
    /// The value argument (`--value`, MCP `"value"`).
    pub value: &'static str,
    /// The `select-text` range (`--range`, MCP `"start"`/`"end"`).
    pub range: &'static str,
    /// How a caller writes a range in that argument, for the message that
    /// says one is missing (`--range START,END`, MCP `"start" and "end"`).
    pub range_form: &'static str,
    /// The `move-to` point (`--at`, MCP `"at"`).
    pub at: &'static str,
    /// The `resize-to` size (`--size`, MCP `"size"`).
    pub size: &'static str,
}

/// The CLI's flag spellings.
pub(crate) const CLI_ACTION_ARG_NAMES: ActionArgNames = ActionArgNames {
    value: "--value",
    range: "--range",
    range_form: "--range START,END",
    at: "--at",
    size: "--size",
};

/// Verbs that read `--value` (MCP `value`): the value verbs, plus
/// `select-text`, which takes its range there as `START,END`.
const ACTIONS_READING_VALUE: &[&str] =
    &["set-value", "set-numeric-value", "type-text", "select-text"];

/// Refuses an argument given to a verb that does not read it.
///
/// An argument the verb would ignore is refused rather than dropped: a caller
/// who passed `value` to `press` expected it to do something, and silently
/// pressing reads as having done it.
fn refuse_unread_arg(action: &str, given: bool, name: &str, readers: &[&str]) -> CliResult<()> {
    if given && !readers.contains(&action) {
        return Err(CliError::Usage(format!(
            "{name} applies only to {}, not {action}",
            readers.join(", ")
        )));
    }
    Ok(())
}

/// Parsed action payload, built up front by [`parse_action_args`] so the
/// callers can validate it before resolving the target.
#[derive(Debug, Default)]
pub(crate) struct ActionArgs {
    /// `set-value` / `type-text`: the raw value string.
    pub value: Option<String>,
    /// `set-numeric-value`: the parsed number.
    pub numeric: Option<f64>,
    /// `select-text`: the parsed `START,END` range.
    pub range: Option<(u32, u32)>,
    /// `move-to`: the parsed `--at` point.
    pub at: Option<(i32, i32)>,
    /// `resize-to`: the parsed `--size`.
    pub size: Option<(u32, u32)>,
}

/// Parse the action-specific payload for `action`, before the target is
/// resolved (parse arguments before the first OS call).
///
/// A missing or malformed `--value` / `--at` / `--size` is a usage error, and
/// it must win over app-resolution failures rather than be masked by one:
/// `xa11y action move-to --at 1,x --app nope` must say the `--at` is bad, not
/// that the app was not found — and must not spend an accessibility
/// round-trip first. Verbs without a payload ignore `value` / `at` / `size`,
/// exactly as dispatch ignores them (a stray `--at` on `press` is still not
/// an error).
///
/// `select-text` takes its range either as `range` (CLI `--range START,END`,
/// MCP integer `start` / `end`, the spelling the `text` command uses) or as
/// the older `value` string `START,END`. Giving both is refused rather than
/// one silently winning, and `range` on any other verb is refused: the
/// argument is new and has no other meaning.
pub(crate) fn parse_action_args(
    action: &str,
    value: Option<&str>,
    range: Option<TextRange>,
    at: Option<&str>,
    size: Option<&str>,
    names: &ActionArgNames,
) -> CliResult<ActionArgs> {
    // An unknown verb is reported as unknown by the dispatcher, not as a
    // verb that does not take this argument.
    if ACTION_NAMES.contains(&action) {
        refuse_unread_arg(action, value.is_some(), names.value, ACTIONS_READING_VALUE)?;
        refuse_unread_arg(
            action,
            range.is_some(),
            names.range,
            ACTIONS_REQUIRING_RANGE,
        )?;
        refuse_unread_arg(action, at.is_some(), names.at, ACTIONS_REQUIRING_AT)?;
        refuse_unread_arg(action, size.is_some(), names.size, ACTIONS_REQUIRING_SIZE)?;
    }
    let need_value = |verb: &str| -> CliResult<&str> {
        value.ok_or_else(|| CliError::Usage(format!("{verb} requires a value")))
    };
    let mut args = ActionArgs::default();
    match action {
        "set-value" | "type-text" => args.value = Some(need_value(action)?.to_string()),
        "set-numeric-value" => {
            // A bad number is refused here, so the auto-wait timeout is not
            // burnt and the platform is never touched for a payload that
            // cannot ever succeed.
            args.numeric = Some(parse_numeric_value(need_value(action)?)?);
        }
        "select-text" => {
            args.range = Some(match (range, value) {
                (Some(_), Some(_)) => {
                    return Err(CliError::Usage(format!(
                        "select-text takes its range as {} or as {}, not both",
                        names.range, names.value
                    )))
                }
                (Some(r), None) => {
                    // Refused here for the same reason `parse_text_range`
                    // refuses it: the provider's check would only run after
                    // the auto-wait.
                    if r.start > r.end {
                        return Err(CliError::Usage(format!(
                            "select-text {} start must not exceed end, got {}..{}",
                            names.range, r.start, r.end
                        )));
                    }
                    (r.start, r.end)
                }
                (None, Some(raw)) => parse_text_range(raw)?,
                (None, None) => {
                    return Err(CliError::Usage(format!(
                        "select-text requires a range: {} (or {} as START,END), \
                         in UTF-16 code units",
                        names.range_form, names.value
                    )))
                }
            });
        }
        "move-to" => {
            let raw =
                at.ok_or_else(|| CliError::Usage(format!("{action} requires {} X,Y", names.at)))?;
            let pt = parse_point_arg(raw, names.at)?;
            args.at = Some((pt.x, pt.y));
        }
        "resize-to" => {
            let raw = size
                .ok_or_else(|| CliError::Usage(format!("{action} requires {} W,H", names.size)))?;
            args.size = Some(parse_size_arg(raw, names.size)?);
        }
        _ => {}
    }
    Ok(args)
}

/// The verb match behind [`perform_action`] and [`perform_action_on`].
///
/// A macro rather than a function generic over a trait because the two
/// targets share method *names*, not a trait: `Locator`'s auto-wait and then
/// act, `Element`'s act on the snapshot they hold. One body keeps the verb
/// list and the payload checks identical for both.
macro_rules! dispatch_action {
    ($target:expr, $verb:expr, $args:expr) => {
        match $verb {
            "press" => $target.press()?,
            "focus" => $target.focus()?,
            "blur" => $target.blur()?,
            "toggle" => $target.toggle()?,
            "expand" => $target.expand()?,
            "collapse" => $target.collapse()?,
            "select" => $target.select()?,
            "show-menu" => $target.show_menu()?,
            "scroll-into-view" => $target.scroll_into_view()?,
            "increment" => $target.increment()?,
            "decrement" => $target.decrement()?,
            "set-value" => {
                let v = $args
                    .value
                    .as_deref()
                    .ok_or_else(|| CliError::Usage(format!("{} requires a value", $verb)))?;
                $target.set_value(v)?;
            }
            "set-numeric-value" => {
                let v = $args
                    .numeric
                    .ok_or_else(|| CliError::Usage(format!("{} requires a value", $verb)))?;
                $target.set_numeric_value(v)?;
            }
            "type-text" => {
                let v = $args
                    .value
                    .as_deref()
                    .ok_or_else(|| CliError::Usage(format!("{} requires a value", $verb)))?;
                $target.type_text(v)?;
            }
            "select-text" => {
                let (start, end) = $args
                    .range
                    .ok_or_else(|| CliError::Usage(format!("{} requires a value", $verb)))?;
                $target.select_text(start, end)?;
            }
            "minimize" => $target.minimize()?,
            "maximize" => $target.maximize()?,
            "enter-fullscreen" => $target.enter_fullscreen()?,
            "restore" => $target.restore()?,
            "close" => $target.close()?,
            "activate" => $target.activate()?,
            "move-to" => {
                let (x, y) = $args
                    .at
                    .ok_or_else(|| CliError::Usage(format!("{} requires --at X,Y", $verb)))?;
                $target.move_to(x, y)?;
            }
            "resize-to" => {
                let (w, h) = $args
                    .size
                    .ok_or_else(|| CliError::Usage(format!("{} requires --size W,H", $verb)))?;
                $target.resize_to(w, h)?;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown action: {other} (expected one of: {})",
                    ACTION_NAMES.join(", ")
                )));
            }
        }
    };
}

/// Dispatch a named action verb onto `locator`, auto-waiting on it.
///
/// `xa11y action`'s half; the MCP `action` tool uses [`perform_action_on`].
/// Both expand one `dispatch_action!` body, so the two cannot drift
/// on which verbs exist or which of them need a value/geometry argument.
/// The payload is parsed by [`parse_action_args`], which the callers run
/// *before* resolving the target, so a bad `X,Y` / `W,H` / value is refused
/// as a usage error before any OS call — it cannot burn the auto-wait
/// timeout, reach the platform, or be masked by an app-resolution failure
/// (parse arguments before the first OS call). The `ok_or_else` arms below
/// are defense in depth (tenet 1: an error is surfaced where it happens);
/// [`parse_action_args`] guarantees the fields for the verb. Writes nothing
/// to stdout: the MCP stdio transport allows only protocol messages there,
/// so the "ok" line stays in the CLI half.
pub(crate) fn perform_action(
    locator: &Locator,
    action_name: &str,
    args: &ActionArgs,
) -> CliResult<()> {
    // The dispatch runs inside a closure so every arm's failure passes through
    // `relabel_action_error` on the way out, and the verb spelling a caller is
    // told about is the one this function accepts.
    let dispatch = || -> CliResult<()> {
        dispatch_action!(locator, action_name, args);
        Ok(())
    };
    dispatch().map_err(|e| relabel_action_error(e, action_name))
}

/// Dispatch a named action verb onto an already-resolved `element`, without
/// re-resolving or waiting.
///
/// The MCP `action` tool's half: it resolves through
/// [`Locator::auto_wait_unique`], which proves the selector matched exactly
/// one actionable element, and then acts on *that* element. Going back
/// through [`perform_action`] would re-resolve the selector, and a UI that
/// changed in between would get the action on an element nobody counted.
pub(crate) fn perform_action_on(
    element: &Element,
    action_name: &str,
    args: &ActionArgs,
) -> CliResult<()> {
    let dispatch = || -> CliResult<()> {
        dispatch_action!(element, action_name, args);
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

/// Re-spell the action name in an `ActionNotSupported`, and the leading
/// action name of an `Unsupported` feature, as the verb the caller typed.
///
/// Providers report the failing action by its Rust method name
/// (`show_menu`, `scroll_into_view`), so the error told the user to use a
/// spelling `xa11y action` and the MCP `action` tool both reject. Only the
/// name is rewritten, and only when it is the same verb modulo the separator
/// — a provider naming some *other* action passes through untouched, because
/// that difference is information, not noise. An `Unsupported` feature is a
/// sentence, and the window verbs' features lead with that same method name
/// (`enter_fullscreen on window: ...`), so the leading token is rewritten the
/// same way; the rest, which is the provider's diagnosis, is kept verbatim.
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
        CliError::Xa11y(Error::Unsupported { feature }) => {
            // The method name is the verb with the separator flipped, the
            // same equivalence the `ActionNotSupported` arm uses.
            let method = verb.replace('-', "_");
            if let Some(rest) = feature.strip_prefix(method.as_str()) {
                // Only a whole token: a feature that merely starts with the
                // name (`closed ...` for `close`) is a different word and is
                // left alone.
                if rest
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                {
                    return CliError::Xa11y(Error::Unsupported {
                        feature: format!("{verb}{rest}"),
                    });
                }
            }
            CliError::Xa11y(Error::Unsupported { feature })
        }
        other => other,
    }
}

/// Parse a `START,END` range for `select-text`, in UTF-16 code units (the
/// unit every text offset in xa11y uses; issue #646).
pub(crate) fn parse_text_range(raw: &str) -> CliResult<(u32, u32)> {
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != 2 {
        return Err(CliError::Usage(format!(
            "select-text value must be START,END in UTF-16 code units (e.g. 0,5), got: {raw}"
        )));
    }
    let start: u32 = parts[0].trim().parse().map_err(|_| {
        CliError::Usage(format!("invalid START in select-text value: {}", parts[0]))
    })?;
    let end: u32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CliError::Usage(format!("invalid END in select-text value: {}", parts[1])))?;
    // Refused here rather than by the provider: the provider's check runs
    // only after the auto-wait, so a reversed range would spend the whole
    // timeout before failing on a value that could never succeed.
    if start > end {
        return Err(CliError::Usage(format!(
            "select-text START must not exceed END, got: {raw}"
        )));
    }
    Ok((start, end))
}

// ── Text by range (`xa11y text`, MCP `text`) ────────────────────────────────
//
// The value-producing half both surfaces share. Everything here returns JSON
// rather than printing it, so the MCP handler can call it on the stdio
// transport; `cmd_text` is the only part that writes to stdout.

/// Every `text` operation, in the order the CLI help and the MCP schema list
/// them. One list, so the two surfaces cannot disagree on which exist.
pub(crate) const TEXT_OPS: &[&str] = &[
    "info",
    "read",
    "find",
    "range-at",
    "object-at",
    "lines",
    "bounds",
    "runs",
];

/// UTF-16 code units `text read` returns when no `limit` is given.
pub(crate) const TEXT_READ_DEFAULT_UNITS: usize = 4_096;
/// Ceiling on `text read`'s `limit`.
pub(crate) const TEXT_READ_MAX_UNITS: usize = 65_536;
/// Lines or runs one `text lines` / `text runs` page holds by default.
pub(crate) const TEXT_PAGE_DEFAULT: usize = 100;
/// Ceiling on a `lines` / `runs` page.
pub(crate) const TEXT_PAGE_MAX: usize = 1_000;
/// UTF-16 code units of `text` carried on each range inside a list (a
/// selection, a line, a run, a match). The range's own `start` / `end` stay
/// exact; a longer `text` is cut and says so with `text_truncated`.
pub(crate) const RANGE_TEXT_MAX_UNITS: u32 = 256;
/// Most selections `text info` lists.
pub(crate) const MAX_SELECTIONS: usize = 100;

/// Which text a range-taking operation covers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RangeSpec {
    /// The whole text: `0..length`.
    Whole,
    /// Explicit offsets.
    Offsets(TextRange),
    /// The `occurrence`-th (0-based) exact match of `needle`, searched within
    /// `within` when given.
    Find {
        needle: String,
        occurrence: usize,
        within: Option<TextRange>,
    },
}

/// A parsed `text` operation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TextOp {
    Info,
    ObjectAt {
        offset: u32,
    },
    Read {
        range: RangeSpec,
        limit: usize,
    },
    Find {
        needle: String,
        occurrence: usize,
        within: Option<TextRange>,
    },
    RangeAt {
        offset: u32,
        unit: TextUnit,
    },
    Lines {
        range: RangeSpec,
        limit: usize,
    },
    Bounds {
        range: RangeSpec,
    },
    Runs {
        range: RangeSpec,
        limit: usize,
    },
}

/// The raw, surface-neutral arguments of a `text` operation. The CLI fills it
/// from flags and MCP from tool arguments; [`parse_text_op`] validates both
/// the same way.
#[derive(Debug, Default)]
pub(crate) struct TextArgs<'a> {
    pub range: Option<TextRange>,
    pub find: Option<&'a str>,
    pub occurrence: Option<usize>,
    pub offset: Option<u32>,
    pub unit: Option<&'a str>,
    pub limit: Option<usize>,
}

/// Validate a `text` operation before anything is resolved (parse arguments
/// before the first OS call).
///
/// An argument the operation does not use is refused rather than ignored:
/// `--offset` on `read` reads as if it chose where to start, and silently
/// reading from 0 instead would hand back text the caller did not ask for.
/// `names` spells each argument the way the calling surface does (`--range`
/// or `start`/`end`), so the refusal names something the caller can type.
pub(crate) fn parse_text_op(op: &str, a: &TextArgs<'_>, names: &TextArgNames) -> CliResult<TextOp> {
    let refuse = |given: bool, name: &str| -> CliResult<()> {
        if given {
            Err(CliError::Usage(format!("text {op} does not take {name}")))
        } else {
            Ok(())
        }
    };
    if let Some(r) = a.range {
        if r.start > r.end {
            return Err(CliError::Usage(format!(
                "{} start must not exceed end, got {}..{}",
                names.range, r.start, r.end
            )));
        }
    }
    if a.occurrence.is_some() && a.find.is_none() {
        return Err(CliError::Usage(format!(
            "{} picks a match of {}, which was not given",
            names.occurrence, names.find
        )));
    }
    if a.find == Some("") {
        return Err(CliError::Usage(format!("{} must not be empty", names.find)));
    }
    let range_spec = || match a.find {
        Some(needle) => RangeSpec::Find {
            needle: needle.to_string(),
            occurrence: a.occurrence.unwrap_or(0),
            within: a.range,
        },
        None => a.range.map_or(RangeSpec::Whole, RangeSpec::Offsets),
    };
    let limit = |default: usize, max: usize| -> CliResult<usize> {
        match a.limit {
            None => Ok(default),
            Some(n) if (1..=max).contains(&n) => Ok(n),
            Some(n) => Err(CliError::Usage(format!(
                "{} for text {op} must be between 1 and {max}, got {n}",
                names.limit
            ))),
        }
    };
    let offset_free = || -> CliResult<()> {
        refuse(a.offset.is_some(), names.offset)?;
        refuse(a.unit.is_some(), names.unit)
    };
    Ok(match op {
        "info" => {
            refuse(a.range.is_some(), names.range)?;
            refuse(a.find.is_some(), names.find)?;
            refuse(a.limit.is_some(), names.limit)?;
            offset_free()?;
            TextOp::Info
        }
        "read" => {
            offset_free()?;
            TextOp::Read {
                range: range_spec(),
                limit: limit(TEXT_READ_DEFAULT_UNITS, TEXT_READ_MAX_UNITS)?,
            }
        }
        "find" => {
            offset_free()?;
            refuse(a.limit.is_some(), names.limit)?;
            let needle = a.find.ok_or_else(|| {
                CliError::Usage(format!(
                    "text find requires {} (the text to look for)",
                    names.find
                ))
            })?;
            TextOp::Find {
                needle: needle.to_string(),
                occurrence: a.occurrence.unwrap_or(0),
                within: a.range,
            }
        }
        "object-at" => {
            refuse(a.range.is_some(), names.range)?;
            refuse(a.find.is_some(), names.find)?;
            refuse(a.unit.is_some(), names.unit)?;
            refuse(a.limit.is_some(), names.limit)?;
            TextOp::ObjectAt {
                offset: a.offset.ok_or_else(|| {
                    CliError::Usage(format!("text object-at requires {}", names.offset))
                })?,
            }
        }
        "range-at" => {
            refuse(a.range.is_some(), names.range)?;
            refuse(a.find.is_some(), names.find)?;
            refuse(a.limit.is_some(), names.limit)?;
            let offset = a.offset.ok_or_else(|| {
                CliError::Usage(format!("text range-at requires {}", names.offset))
            })?;
            let unit = a.unit.ok_or_else(|| {
                CliError::Usage(format!(
                    "text range-at requires {} (character, word, line or paragraph)",
                    names.unit
                ))
            })?;
            let unit = TextUnit::parse(unit).map_err(|e| CliError::Usage(e.to_string()))?;
            TextOp::RangeAt { offset, unit }
        }
        "lines" => {
            offset_free()?;
            TextOp::Lines {
                range: range_spec(),
                limit: limit(TEXT_PAGE_DEFAULT, TEXT_PAGE_MAX)?,
            }
        }
        "bounds" => {
            offset_free()?;
            refuse(a.limit.is_some(), names.limit)?;
            TextOp::Bounds {
                range: range_spec(),
            }
        }
        "runs" => {
            offset_free()?;
            TextOp::Runs {
                range: range_spec(),
                limit: limit(TEXT_PAGE_DEFAULT, TEXT_PAGE_MAX)?,
            }
        }
        other => {
            return Err(CliError::Usage(format!(
                "unknown text operation: {other} (expected one of: {})",
                TEXT_OPS.join(", ")
            )))
        }
    })
}

/// How a surface spells the `text` arguments, for usage errors.
pub(crate) struct TextArgNames {
    pub range: &'static str,
    pub find: &'static str,
    pub occurrence: &'static str,
    pub offset: &'static str,
    pub unit: &'static str,
    pub limit: &'static str,
}

/// The CLI's flag spellings.
pub(crate) const CLI_TEXT_ARG_NAMES: TextArgNames = TextArgNames {
    range: "--range",
    find: "--find",
    occurrence: "--occurrence",
    offset: "--offset",
    unit: "--unit",
    limit: "--limit",
};

/// Resolve `selector` under `target` to the one element a text or table read
/// acts on.
///
/// Text waits, as [`Locator::text`] does, up to the default timeout for the
/// selector to match an attached element; it does not require it to be
/// visible or enabled. `Ok(Err(matches))` is a selector that matched several:
/// both surfaces refuse it with their own candidate rendering rather than
/// reading the first, because a model told "exactly one" cannot tell it read
/// the wrong field.
pub(crate) fn resolve_text_element(
    target: &Target,
    selector: &str,
) -> CliResult<std::result::Result<Element, Vec<Element>>> {
    Ok(target.locator(selector).auto_wait_unique("read_text")?)
}

/// Read at most `cap` units of `range`, returning the text and the offset
/// the read stopped at.
///
/// A cut that would split a surrogate pair is refused by the backend with
/// `InvalidTextRange`; the cut is then moved back one unit, which cannot
/// split it. Only a cut this function chose is moved: a caller's own range
/// that splits a pair is the caller's error and propagates.
fn capped_read(text: &Text, range: TextRange, cap: u32) -> CliResult<(String, u32)> {
    let end = range.end.min(range.start.saturating_add(cap));
    let cut = TextRange {
        start: range.start,
        end,
    };
    match text.read(cut) {
        Ok(s) => Ok((s, end)),
        // A cut strictly inside a range already checked against the length
        // can only fail by splitting a surrogate pair (see `resolve_range`).
        Err(Error::InvalidTextRange { .. }) if end < range.end => {
            // The cut split a surrogate pair. Back off one unit, or, when
            // that would read nothing (a cap of 1), take the whole pair.
            let end = if end > range.start + 1 {
                end - 1
            } else {
                end + 1
            };
            let s = text.read(TextRange {
                start: range.start,
                end,
            })?;
            Ok((s, end))
        }
        Err(e) => Err(e.into()),
    }
}

/// A range as both surfaces report it: its offsets and the text it covers,
/// so a caller copies numbers rather than computing them.
fn range_json(text: &Text, range: TextRange) -> CliResult<serde_json::Value> {
    let (s, read_end) = capped_read(text, range, RANGE_TEXT_MAX_UNITS)?;
    let mut out = serde_json::Map::new();
    out.insert("start".into(), range.start.into());
    out.insert("end".into(), range.end.into());
    out.insert("text".into(), s.into());
    if read_end < range.end {
        // `text` is a prefix; the offsets above are still the whole range.
        out.insert("text_truncated".into(), true.into());
        out.insert("text_end".into(), read_end.into());
    }
    Ok(serde_json::Value::Object(out))
}

fn resolve_range(text: &Text, spec: &RangeSpec) -> CliResult<TextRange> {
    match spec {
        RangeSpec::Whole => Ok(TextRange {
            start: 0,
            end: text.length()?,
        }),
        RangeSpec::Offsets(r) => {
            // Checked against the length up front so a range past the end is
            // refused with the caller's own offsets, not with whatever page of
            // it a capped read happened to ask for.
            crate::text_api::check_in_bounds(*r, text.length()?)?;
            Ok(*r)
        }
        RangeSpec::Find {
            needle,
            occurrence,
            within,
        } => find_range(text, needle, *occurrence, *within)?.ok_or_else(|| {
            CliError::NotFound(not_found_message(text, needle, *occurrence, *within))
        }),
    }
}

fn find_range(
    text: &Text,
    needle: &str,
    occurrence: usize,
    within: Option<TextRange>,
) -> CliResult<Option<TextRange>> {
    let mut options = FindOptions::new().occurrence(occurrence);
    if let Some(r) = within {
        options = options.within(r);
    }
    Ok(text.find_with(needle, options)?)
}

/// What a miss says: the needle, which match, where it looked, and how long
/// the text is, so a caller can tell a typo from an occurrence past the last.
fn not_found_message(
    text: &Text,
    needle: &str,
    occurrence: usize,
    within: Option<TextRange>,
) -> String {
    let scope = match within {
        Some(r) => format!("within {r}"),
        None => "in the whole text".to_string(),
    };
    let length = match text.length() {
        Ok(l) => format!("text length {l}"),
        Err(e) => format!("text length unreadable: {e}"),
    };
    format!(
        "text {needle:?} has no match at occurrence {occurrence} (0-based) {scope} \
         ({length}; matching is exact and case-sensitive)"
    )
}

/// JSON with every `null` field dropped, so an attribute the platform did not
/// report is absent rather than present-and-empty. Serialization of these
/// plain data types cannot fail; an error is still surfaced rather than
/// swallowed.
pub(crate) fn json_without_nulls<T: serde::Serialize>(
    what: &str,
    value: &T,
) -> CliResult<serde_json::Value> {
    let mut v = serde_json::to_value(value).map_err(|e| {
        CliError::Xa11y(Error::Platform {
            code: -1,
            message: format!("serialize {what}: {e}"),
        })
    })?;
    if let serde_json::Value::Object(map) = &mut v {
        map.retain(|_, field| !field.is_null());
    }
    Ok(v)
}

/// How the CLI's `text object-at` reports the element it found: the same
/// one-line form `find`, `table` and `relations` print, as a JSON string.
pub(crate) fn text_object_cli(element: &Element) -> serde_json::Value {
    serde_json::Value::String(format_element_oneline(element))
}

/// Run a `text` operation against `text`, producing the result both surfaces
/// report. Offsets everywhere are UTF-16 code units.
///
/// `render_object` encodes the element `object-at` finds, the way the calling
/// surface renders every other element: [`text_object_cli`] on the CLI, and
/// MCP's element encoding plus a `ref`. Serializing the raw `ElementData`
/// instead leaked its PascalCase role, its platform `raw` blob and uncapped
/// strings, and gave MCP nothing to act on.
pub(crate) fn run_text_op(
    text: &Text,
    op: &TextOp,
    render_object: &mut dyn FnMut(&Element) -> serde_json::Value,
) -> CliResult<serde_json::Value> {
    use serde_json::json;
    Ok(match op {
        TextOp::Info => {
            let length = text.length()?;
            let mut out = serde_json::Map::new();
            out.insert("length".into(), length.into());
            // A platform that cannot report a caret at all is reported as
            // such, by name, rather than failing the length and selection
            // reads alongside it. Every other failure propagates.
            match text.caret() {
                Ok(caret) => {
                    out.insert("caret".into(), json!(caret));
                }
                Err(Error::Unsupported { feature }) => {
                    out.insert("caret".into(), serde_json::Value::Null);
                    out.insert("caret_unsupported".into(), feature.into());
                }
                Err(e) => return Err(e.into()),
            }
            let selections = text.selections()?;
            let listed = selections
                .iter()
                .take(MAX_SELECTIONS)
                .map(|r| range_json(text, *r))
                .collect::<CliResult<Vec<_>>>()?;
            out.insert("selection_count".into(), selections.len().into());
            out.insert(
                "selections_truncated".into(),
                (selections.len() > listed.len()).into(),
            );
            out.insert("selections".into(), listed.into());
            serde_json::Value::Object(out)
        }
        TextOp::Read { range, limit } => {
            let r = resolve_range(text, range)?;
            let cap = u32::try_from(*limit).unwrap_or(u32::MAX);
            let (s, read_end) = capped_read(text, r, cap)?;
            let truncated = read_end < r.end;
            let mut out = serde_json::Map::new();
            out.insert("start".into(), r.start.into());
            out.insert("end".into(), read_end.into());
            out.insert("text".into(), s.into());
            out.insert("requested_end".into(), r.end.into());
            out.insert("truncated".into(), truncated.into());
            if truncated {
                out.insert("next_start".into(), read_end.into());
            }
            serde_json::Value::Object(out)
        }
        TextOp::Find {
            needle,
            occurrence,
            within,
        } => {
            let found = find_range(text, needle, *occurrence, *within)?;
            json!({
                "find": needle,
                "occurrence": occurrence,
                "within": within.map(|r| json!({ "start": r.start, "end": r.end })),
                "found": found.is_some(),
                "match": found.map(|r| range_json(text, r)).transpose()?,
            })
        }
        TextOp::ObjectAt { offset } => {
            let object = match text.object_at(*offset)? {
                Some(element) => render_object(&element),
                None => serde_json::Value::Null,
            };
            json!({ "offset": offset, "object": object })
        }
        TextOp::RangeAt { offset, unit } => {
            let r = text.range_at(*offset, *unit)?;
            json!({
                "offset": offset,
                "unit": unit.to_snake_case(),
                "range": range_json(text, r)?,
            })
        }
        TextOp::Lines { range, limit } => {
            let r = resolve_range(text, range)?;
            let page = text.lines(r, *limit)?;
            let lines = page
                .lines
                .iter()
                .map(|l| range_json(text, *l))
                .collect::<CliResult<Vec<_>>>()?;
            json!({
                "range": { "start": r.start, "end": r.end },
                "count": lines.len(),
                "lines": lines,
                "truncated": page.truncated,
                "next_start": page.next_start,
            })
        }
        TextOp::Bounds { range } => {
            let r = resolve_range(text, range)?;
            let rects: Vec<serde_json::Value> = text
                .bounds(r)?
                .into_iter()
                .map(|b| json!({ "x": b.x, "y": b.y, "width": b.width, "height": b.height }))
                .collect();
            json!({
                "range": range_json(text, r)?,
                "rects": rects,
            })
        }
        TextOp::Runs { range, limit } => {
            let r = resolve_range(text, range)?;
            let page = text.runs(r, *limit)?;
            let runs = page
                .runs
                .iter()
                .map(|run| {
                    let mut v = range_json(text, run.range)?;
                    let attributes = json_without_nulls("text attributes", &run.attributes)?;
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("attributes".into(), attributes);
                    }
                    Ok(v)
                })
                .collect::<CliResult<Vec<_>>>()?;
            json!({
                "range": { "start": r.start, "end": r.end },
                "count": runs.len(),
                "runs": runs,
                "truncated": page.truncated,
                "next_start": page.next_start,
            })
        }
    })
}

fn cmd_text(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_command_opts(&TEXT_FLAGS, args)?;
    if positional.len() < 2 {
        return Err(CliError::Usage(format!(
            "usage: xa11y text OP SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [--range START,END] [--find TEXT [--occurrence N]] [--offset N --unit U] \
             [--limit N]  (OP is one of: {})",
            TEXT_OPS.join(", ")
        )));
    }
    let (op_name, selector) = (&positional[0], &positional[1]);
    let range = opts
        .range
        .as_deref()
        .map(|raw| parse_range_flag(raw, "--range"))
        .transpose()?;
    let op = parse_text_op(
        op_name,
        &TextArgs {
            range,
            find: opts.find.as_deref(),
            occurrence: opts.occurrence,
            offset: opts.offset,
            unit: opts.unit.as_deref(),
            limit: opts.limit,
        },
        &CLI_TEXT_ARG_NAMES,
    )?;
    let target = resolve_target(&opts)?;
    let element = match resolve_text_element(&target, selector)? {
        Ok(element) => element,
        Err(matches) => {
            return Err(ambiguous_selector(selector, &matches, 8, |el| {
                format_element_oneline(el)
            }))
        }
    };
    let result = run_text_op(&element.text()?, &op, &mut text_object_cli)?;
    let rendered = serde_json::to_string_pretty(&result).map_err(|e| {
        CliError::Xa11y(Error::Platform {
            code: -1,
            message: format!("serialize the text result: {e}"),
        })
    })?;
    println!("{rendered}");
    Ok(())
}

/// Parse a `START,END` UTF-16 code-unit range flag.
pub(crate) fn parse_range_flag(raw: &str, flag: &str) -> CliResult<TextRange> {
    let (start, end) = raw.split_once(',').ok_or_else(|| {
        CliError::Usage(format!(
            "{flag} must be START,END in UTF-16 code units (e.g. 0,5), got: {raw}"
        ))
    })?;
    let parse = |part: &str, what: &str| -> CliResult<u32> {
        part.trim()
            .parse()
            .map_err(|_| CliError::Usage(format!("invalid {what} in {flag}: {part}")))
    };
    let (start, end) = (parse(start, "START")?, parse(end, "END")?);
    if start > end {
        return Err(CliError::Usage(format!(
            "{flag} START must not exceed END, got: {raw}"
        )));
    }
    Ok(TextRange { start, end })
}

// ── Table lookups (`xa11y table`, MCP `table`) ──────────────────────────────

/// Every `table` operation, shared by the CLI help and the MCP schema.
pub(crate) const TABLE_OPS: &[&str] = &[
    "cell-at",
    "column-headers",
    "row-headers",
    "column-header-cells",
    "row-header-cells",
    "selected-items",
];

/// A parsed `table` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableOp {
    /// On a table: the cell covering 0-based `row` / `column`.
    CellAt { row: u32, column: u32 },
    /// On a table: its header cells of one kind.
    Headers(HeaderKind),
    /// On a cell: the header cells of one kind that head it.
    HeaderCells(HeaderKind),
    /// On a list, tree, table or tab list: the selected items.
    SelectedItems,
}

/// Validate a `table` operation before anything is resolved. `row` /
/// `column` belong to `cell-at` alone and are refused elsewhere.
pub(crate) fn parse_table_op(
    op: &str,
    row: Option<u32>,
    column: Option<u32>,
    row_name: &str,
    column_name: &str,
) -> CliResult<TableOp> {
    if op != "cell-at" {
        for (given, name) in [(row.is_some(), row_name), (column.is_some(), column_name)] {
            if given {
                return Err(CliError::Usage(format!("table {op} does not take {name}")));
            }
        }
    }
    Ok(match op {
        "cell-at" => match (row, column) {
            (Some(row), Some(column)) => TableOp::CellAt { row, column },
            _ => {
                return Err(CliError::Usage(format!(
                    "table cell-at requires {row_name} and {column_name} (both 0-based)"
                )))
            }
        },
        "column-headers" => TableOp::Headers(HeaderKind::Column),
        "row-headers" => TableOp::Headers(HeaderKind::Row),
        "column-header-cells" => TableOp::HeaderCells(HeaderKind::Column),
        "row-header-cells" => TableOp::HeaderCells(HeaderKind::Row),
        "selected-items" => TableOp::SelectedItems,
        other => {
            return Err(CliError::Usage(format!(
                "unknown table operation: {other} (expected one of: {})",
                TABLE_OPS.join(", ")
            )))
        }
    })
}

/// Run a `table` operation on `element`. `cell-at` yields zero or one
/// element; the others a list.
pub(crate) fn run_table_op(element: &Element, op: TableOp) -> CliResult<Vec<Element>> {
    Ok(match op {
        TableOp::CellAt { row, column } => element.cell_at(row, column)?.into_iter().collect(),
        TableOp::Headers(HeaderKind::Column) => element.column_headers()?,
        TableOp::Headers(HeaderKind::Row) => element.row_headers()?,
        TableOp::HeaderCells(HeaderKind::Column) => element.column_header_cells()?,
        TableOp::HeaderCells(HeaderKind::Row) => element.row_header_cells()?,
        TableOp::SelectedItems => element.selected_items()?,
    })
}

/// Resolve the one element a `table` operation acts on: read once, no wait,
/// as `find` does. Several matches are `Ok(Err(..))`; none is the diagnosed
/// miss from [`Locator::element`].
pub(crate) fn resolve_table_element(
    target: &Target,
    selector: &str,
) -> CliResult<std::result::Result<Element, Vec<Element>>> {
    let locator = target.locator(selector);
    let mut matches = locator.elements()?;
    match matches.len() {
        0 => Ok(Ok(locator.element()?)),
        1 => Ok(Ok(matches.remove(0))),
        _ => Ok(Err(matches)),
    }
}

/// The miss `cell-at` reports: the slot asked for, and the size the table
/// itself reports, so an off-by-one reads as one.
pub(crate) fn no_cell_message(table: &ElementData, row: u32, column: u32) -> String {
    let size = match table.table {
        Some(t) => format!(
            "the table reports {} rows and {} columns",
            t.row_count
                .map_or("unreported".to_string(), |c| c.to_string()),
            t.column_count
                .map_or("unreported".to_string(), |c| c.to_string()),
        ),
        None => "the element reports no table size".to_string(),
    };
    format!(
        "no cell at row {row}, column {column} (0-based): the slot is out of range or \
         not realized; {size}"
    )
}

fn cmd_table(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_command_opts(&TABLE_FLAGS, args)?;
    if positional.len() < 2 {
        return Err(CliError::Usage(format!(
            "usage: xa11y table OP SELECTOR [--app NAME | --pid PID | --shell KIND] \
             [--row N --column N]  (OP is one of: {})",
            TABLE_OPS.join(", ")
        )));
    }
    let (op_name, selector) = (&positional[0], &positional[1]);
    let op = parse_table_op(op_name, opts.row, opts.column, "--row", "--column")?;
    let target = resolve_target(&opts)?;
    let element = match resolve_table_element(&target, selector)? {
        Ok(element) => element,
        Err(matches) => {
            return Err(ambiguous_selector(selector, &matches, 8, |el| {
                format_element_oneline(el)
            }))
        }
    };
    let found = run_table_op(&element, op)?;
    if let TableOp::CellAt { row, column } = op {
        if found.is_empty() {
            return Err(CliError::NotFound(no_cell_message(&element, row, column)));
        }
    }
    for el in &found {
        println!("{}", format_element_oneline(el));
    }
    if !matches!(op, TableOp::CellAt { .. }) {
        println!(
            "({} element{})",
            found.len(),
            if found.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Every relation kind's name, for usage text and the MCP schema.
pub(crate) fn relation_kind_names() -> Vec<&'static str> {
    RelationKind::ALL
        .iter()
        .map(|k| k.to_snake_case())
        .collect()
}

/// Parse relation kind names. A bad name is a usage error naming the valid
/// ones, raised before anything is resolved.
pub(crate) fn parse_relation_kinds<S: AsRef<str>>(names: &[S]) -> CliResult<Vec<RelationKind>> {
    names
        .iter()
        .map(|n| RelationKind::parse(n.as_ref()).map_err(|e| CliError::Usage(e.to_string())))
        .collect()
}

fn cmd_relations(args: &[String]) -> CliResult<()> {
    let (opts, positional) = parse_command_opts(&RELATIONS_FLAGS, args)?;
    let Some((selector, kind_names)) = positional.split_first() else {
        return Err(CliError::Usage(format!(
            "usage: xa11y relations SELECTOR [KIND...] [--app NAME | --pid PID | --shell KIND]  \
             (KIND is one of: {})",
            relation_kind_names().join(", ")
        )));
    };
    let kinds = parse_relation_kinds(kind_names)?;
    let target = resolve_target(&opts)?;
    // One element, read once like `table`: relations of several matches
    // would not say whose they are.
    let element = match resolve_table_element(&target, selector)? {
        Ok(element) => element,
        Err(matches) => {
            return Err(ambiguous_selector(selector, &matches, 8, |el| {
                format_element_oneline(el)
            }))
        }
    };
    for (kind, targets) in element.relations(&kinds)? {
        println!("{kind} ({}):", targets.len());
        for el in &targets {
            println!("  {}", format_element_oneline(el));
        }
    }
    Ok(())
}

fn cmd_events(args: &[String]) -> CliResult<()> {
    let (opts, _) = parse_command_opts(&EVENTS_FLAGS, args)?;
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
    let kinds: Vec<String> = opts
        .kinds
        .as_deref()
        .map(|k| k.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    if let Some(bad) = kinds
        .iter()
        .find(|k| !crate::event_kind_names().contains(&k.as_str()))
    {
        return Err(CliError::Usage(format!(
            "unknown event kind {bad:?} for --kinds; expected one of {}",
            crate::event_kind_names().join(", ")
        )));
    }
    let app = resolve_app(&opts)?;
    // A kind this platform never sends fails here rather than leaving the
    // command listening for nothing.
    let sub = app.subscribe_with(&SubscribeOptions::new().kinds(kinds))?;
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
        EventKind::TextSelectionChanged => "text_selection_changed",
        EventKind::Announcement => "announcement",
        // `EventKind` is `#[non_exhaustive]`. A kind this build predates is
        // still worth printing as a line — the CLI is a debugging tool, and
        // dropping the event entirely would be worse than naming it vaguely.
        _ => "unknown",
    }
}

/// Every spelling [`format_event_kind`] can produce, for MCP's `events_start`
/// `kinds` filter to validate a requested name against.
///
/// Derived by running the formatter over one value of each variant rather than
/// written out a second time, so the filter cannot advertise a name the
/// formatter never emits, and cannot spell one differently.
///
/// What it cannot do is notice a variant missing from `KNOWN` below.
/// [`EventKind`] is `#[non_exhaustive]`, so a `match` here would compile with
/// a `_` arm just as `format_event_kind` does, and there is nothing to
/// enumerate the variants at runtime. The guards are the
/// `[[types.variant_coverage]]` entry for `EventKind`, which requires this
/// file to name every variant, and
/// `every_advertised_event_kind_name_round_trips`, which fails if an entry
/// here formats as `unknown` or repeats another. **A new variant belongs in
/// both `format_event_kind` and `KNOWN`.**
pub(crate) fn event_kind_names() -> &'static [&'static str] {
    /// One value per variant. `StateChanged`'s payload is arbitrary: only the
    /// variant reaches `format_event_kind`.
    const KNOWN: &[EventKind] = &[
        EventKind::FocusChanged,
        EventKind::ValueChanged,
        EventKind::NameChanged,
        EventKind::StateChanged {
            flag: StateFlag::Enabled,
            value: true,
        },
        EventKind::StructureChanged,
        EventKind::WindowOpened,
        EventKind::WindowClosed,
        EventKind::WindowActivated,
        EventKind::WindowDeactivated,
        EventKind::SelectionChanged,
        EventKind::MenuOpened,
        EventKind::MenuClosed,
        EventKind::TextChanged,
        EventKind::TextSelectionChanged,
        EventKind::Announcement,
    ];
    static NAMES: std::sync::LazyLock<Vec<&'static str>> =
        std::sync::LazyLock::new(|| KNOWN.iter().map(format_event_kind).collect());
    NAMES.as_slice()
}

/// snake_case name for a state flag, matching the spelling both bindings use
/// for `Event.state_flag` and the one `states` uses in element payloads.
///
/// Hand-mapped rather than derived from `Debug`, which would render a future
/// multi-word variant as one lowercase run. `[[types.variant_coverage]]` lists
/// this file for `StateFlag` so a new variant cannot quietly reach a client as
/// `unknown`.
pub(crate) fn format_state_flag(flag: StateFlag) -> &'static str {
    match flag {
        StateFlag::Enabled => "enabled",
        StateFlag::Visible => "visible",
        StateFlag::Focused => "focused",
        StateFlag::Checked => "checked",
        StateFlag::Selected => "selected",
        StateFlag::Expanded => "expanded",
        StateFlag::Editable => "editable",
        StateFlag::Focusable => "focusable",
        StateFlag::Modal => "modal",
        StateFlag::Required => "required",
        StateFlag::Busy => "busy",
        StateFlag::Minimized => "minimized",
        StateFlag::Maximized => "maximized",
        StateFlag::Fullscreen => "fullscreen",
        // `StateFlag` is `#[non_exhaustive]`. Same reasoning as
        // `format_event_kind`: naming the flag vaguely beats dropping the
        // event that carries it.
        _ => "unknown",
    }
}

pub(crate) fn format_event_detail(event: &Event) -> String {
    if let EventKind::StateChanged { flag, value } = event.kind {
        return format!(" {flag:?}={value}");
    }
    if let Some(a) = &event.announcement {
        let mut out = format!(" {}", a.origin);
        if let Some(p) = a.politeness {
            out.push_str(&format!(" {}", p.to_snake_case()));
        }
        if let Some(text) = &a.text {
            let (shown, truncated) = clip_utf16(text, RANGE_TEXT_MAX_UNITS);
            out.push_str(&format!(" {shown:?}"));
            if truncated {
                out.push_str(&format!(" (truncated, {} units)", utf16_units(text)));
            }
        }
        return out;
    }
    let Some(change) = &event.text_change else {
        return String::new();
    };
    let mut out = format!(" {}", format_text_change_kind(change.kind));
    if let Some(range) = change.range {
        out.push_str(&format!(" {range}"));
    }
    if let Some(text) = &change.text {
        let (shown, truncated) = clip_utf16(text, RANGE_TEXT_MAX_UNITS);
        out.push_str(&format!(" {shown:?}"));
        if truncated {
            out.push_str(&format!(" (truncated, {} units)", utf16_units(text)));
        }
    }
    out
}

/// The `snake_case` spelling of a text change kind, as both bindings and
/// MCP report it. Spelled out arm by arm (rather than through
/// `to_snake_case`) so `[[types.variant_coverage]]` sees every variant here.
pub(crate) fn format_text_change_kind(kind: TextChangeKind) -> &'static str {
    match kind {
        TextChangeKind::Inserted => "inserted",
        TextChangeKind::Deleted => "deleted",
    }
}

/// Length of `s` in UTF-16 code units, the unit every offset uses.
fn utf16_units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The longest prefix of `s` that fits in `max` UTF-16 code units without
/// splitting a character, and whether anything was cut.
pub(crate) fn clip_utf16(s: &str, max: u32) -> (&str, bool) {
    let max = max as usize;
    let mut units = 0;
    for (at, ch) in s.char_indices() {
        units += ch.len_utf16();
        if units > max {
            return (&s[..at], true);
        }
    }
    (s, false)
}

/// An announcement's payload as `events_poll` reports it: `origin`, then each
/// other field only when set. `text` is cut to [`RANGE_TEXT_MAX_UNITS`]
/// UTF-16 units the way [`text_change_json`] cuts an edit's, and says so the
/// same way, with `text_truncated: true` and the full `text_length`.
pub(crate) fn announcement_json(a: &crate::AnnouncementInfo) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("origin".into(), a.origin.to_snake_case().into());
    if let Some(text) = &a.text {
        let (shown, truncated) = clip_utf16(text, RANGE_TEXT_MAX_UNITS);
        out.insert("text".into(), shown.into());
        if truncated {
            out.insert("text_truncated".into(), true.into());
            out.insert("text_length".into(), utf16_units(text).into());
        }
    }
    if let Some(p) = a.politeness {
        out.insert("politeness".into(), p.to_snake_case().into());
    }
    if let Some(p) = &a.platform_priority {
        out.insert("platform_priority".into(), p.clone().into());
    }
    if let Some(id) = &a.activity_id {
        out.insert("activity_id".into(), id.clone().into());
    }
    serde_json::Value::Object(out)
}

/// An event's [`TextChange`] as `events_poll` reports it: `kind`, `range`
/// (`null` when the platform reported none), and `text` (`null` when not
/// supplied), cut to [`RANGE_TEXT_MAX_UNITS`] UTF-16 units. A cut text says so
/// with `text_truncated: true` and the full `text_length`, so a shortened
/// string never reads as the whole edit.
pub(crate) fn text_change_json(change: &TextChange) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), format_text_change_kind(change.kind).into());
    out.insert(
        "range".into(),
        match change.range {
            Some(r) => serde_json::json!({ "start": r.start, "end": r.end }),
            None => serde_json::Value::Null,
        },
    );
    match &change.text {
        Some(text) => {
            let (shown, truncated) = clip_utf16(text, RANGE_TEXT_MAX_UNITS);
            out.insert("text".into(), shown.into());
            if truncated {
                out.insert("text_truncated".into(), true.into());
                out.insert("text_length".into(), utf16_units(text).into());
            }
        }
        None => {
            out.insert("text".into(), serde_json::Value::Null);
        }
    }
    serde_json::Value::Object(out)
}

// ── Input simulation ────────────────────────────────────────────────────────

fn cmd_click(args: &[String]) -> CliResult<()> {
    let (opts, _) = parse_command_opts(&CLICK_FLAGS, args)?;
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
    let (opts, _) = parse_command_opts(&MOVE_FLAGS, args)?;
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
    let (opts, _) = parse_command_opts(&DRAG_FLAGS, args)?;
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
    let (opts, _) = parse_command_opts(&SCROLL_FLAGS, args)?;
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
    let (opts, positional) = parse_command_opts(&KEY_FLAGS, args)?;
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
    let (_opts, positional) = parse_command_opts(&TYPE_FLAGS, args)?;
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
    let (opts, _) = parse_command_opts(&SCREENSHOT_FLAGS, args)?;
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
        // A target is read only to resolve --annotate selectors; it never
        // crops the capture. Capturing the whole screen and printing nothing
        // about the ignored flag would read as a capture of that target.
        let named: Vec<&str> = [
            ("--app", opts.app.is_some()),
            ("--pid", opts.pid.is_some()),
            ("--shell", opts.shell.is_some()),
            ("--backend", opts.backend.is_some()),
        ]
        .into_iter()
        .filter_map(|(flag, given)| given.then_some(flag))
        .collect();
        if !named.is_empty() {
            return Err(CliError::Usage(format!(
                "{} without --annotate does nothing: a target is only read to resolve \
                 annotation selectors, and it never crops the capture. Add --annotate \
                 SELECTOR, or use --region X,Y,W,H to capture part of the screen",
                named.join(", ")
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

/// Longest accessible name rendered in a text-legend column.
///
/// The text legend pads every row to the longest name, so one pathological
/// name costs `MAX_ANNOTATIONS` times its length in spaces. That is not
/// hypothetical: on AT-SPI a text area's accessible name is often its whole
/// contents, so a 100 KB name produced megabytes of padding. `--legend json`
/// and the MCP result do not pad and are not truncated here.
const MAX_LEGEND_NAME: usize = 120;

/// A name for a legend column: quoted and escaped, or `-` when the element has
/// none. `-` rather than `""`, so a nameless element is distinguishable from
/// one whose name is the empty string.
///
/// Truncation is marked with `…` inside the quotes, so a shortened name never
/// reads as the element's real one — use `--legend json` for the full text.
fn legend_name(name: Option<&str>) -> String {
    match name {
        Some(n) if n.chars().count() > MAX_LEGEND_NAME => {
            // Truncate by chars, not bytes, so a multi-byte name cannot be cut
            // mid-codepoint. The ellipsis goes inside the quotes: `{:?}` keeps
            // printable non-ASCII as-is, so it renders as part of the name.
            let mut kept: String = n.chars().take(MAX_LEGEND_NAME).collect();
            kept.push('…');
            format!("{kept:?}")
        }
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
/// short.
///
/// The boundary errs toward over-reporting, never under: a group whose matches
/// were *all* omitted (every one lacking bounds) contributes no legend entry,
/// so the maximum comes out lower and a group that was in fact resolved in full
/// is flagged too. Wrongly warning that a group may be short is recoverable;
/// wrongly claiming one is complete is not.
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

    /// One mock-backed `App` (the mock's lone `Role::Application` entry),
    /// for exercising locator dispatch against the shared provider.
    fn mock_app(provider: &std::sync::Arc<xa11y_core::mock::MockProvider>) -> App {
        // Coerce to the trait object explicitly: `App::list_with` takes
        // `Arc<dyn Provider>`, and `Arc::clone` would otherwise infer its
        // target from the parameter and reject the concrete handle.
        let provider: std::sync::Arc<dyn xa11y_core::Provider> = provider.clone();
        App::list_with(provider)
            .expect("the mock must list its app")
            .into_iter()
            .next()
            .expect("the mock must vend one app")
    }

    // ── Delivery disposition ────────────────────────────────────────────────

    #[test]
    fn every_dispatched_command_is_classified_as_action_or_read_only() {
        // `ACTION_COMMANDS` is hand-kept. Read the dispatcher's own arms, so
        // a new command that nobody classified fails here instead of
        // silently lacking the `delivery:` line.
        const NOT_ACTIONS: [&str; 12] = [
            "apps",
            "capabilities",
            "shell",
            "windows",
            "tree",
            "find",
            "text",
            "table",
            "relations",
            "events",
            "screenshot",
            "guide",
        ];
        let source = include_str!("cli.rs");
        let body = source
            .split("pub fn run(args: &[String]) -> CliResult<()> {")
            .nth(1)
            .expect("run() is in this file")
            .split("/// The `xa11y --help` text.")
            .next()
            .expect("run() ends before usage_text");
        let dispatched: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Some(\""))
            .filter_map(|l| l.split('"').next())
            .collect();
        assert!(
            dispatched.len() >= 10,
            "dispatcher not found: {dispatched:?}"
        );
        for name in dispatched {
            if name == "mcp" {
                continue; // a server, not a one-shot operation
            }
            let action = ACTION_COMMANDS.contains(&name);
            let read_only = NOT_ACTIONS.contains(&name);
            assert!(
                action != read_only,
                "`{name}` must be in exactly one of ACTION_COMMANDS and NOT_ACTIONS"
            );
        }
        for name in ACTION_COMMANDS {
            assert!(
                body.contains(&format!("Some(\"{name}\")")),
                "ACTION_COMMANDS lists `{name}`, which the dispatcher does not route"
            );
        }
    }

    #[test]
    fn a_failed_action_command_names_its_delivery() {
        let miss = CliError::Xa11y(Error::selector_not_matched("button"));
        let line = delivery_line("action", &miss).expect("an action failure reports delivery");
        assert!(line.starts_with("delivery: not_delivered"), "{line}");

        let platform = CliError::Xa11y(Error::Platform {
            code: -25204,
            message: "cannot complete".into(),
        });
        let line = delivery_line("click", &platform).expect("a click failure reports delivery");
        assert!(line.starts_with("delivery: unknown"), "{line}");
    }

    #[test]
    fn off_display_pointer_failure_prints_geometry_and_delivery() {
        let input = crate::InputSim::new(std::sync::Arc::new(
            xa11y_core::mock::MockInputProvider::new(std::time::Duration::ZERO),
        ));
        let error = CliError::Xa11y(input.mouse().click((500, 500)).unwrap_err());
        let text = error.to_string();
        assert!(
            text.contains("pointer target: Point { x: 500, y: 500 }"),
            "{text}"
        );
        assert!(text.contains("mock desktop pixels"), "{text}");
        assert!(text.contains("displays:"), "{text}");
        assert_eq!(error.exit_code(), 1);
        let disposition = delivery_line("click", &error).expect("click reports delivery");
        assert!(
            disposition.starts_with("delivery: not_delivered"),
            "{disposition}"
        );
    }

    #[test]
    fn read_only_commands_and_usage_errors_print_no_delivery_line() {
        let miss = CliError::Xa11y(Error::selector_not_matched("button"));
        assert_eq!(delivery_line("find", &miss), None);
        assert_eq!(delivery_line("tree", &miss), None);
        assert_eq!(
            delivery_line("action", &CliError::Usage("bad".into())),
            None
        );
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
    fn a_stable_miss_keeps_that_the_selector_matched_during_the_wait() {
        let miss = Error::selector_not_matched("button[name=\"Gone\"]").diagnose(
            Diagnosis::new()
                .last_observed("2 near misses")
                .candidates(vec!["button \"Go\"".into()]),
        );
        let timeout = Error::timeout(std::time::Duration::from_secs(1)).diagnose(
            Diagnosis::new()
                .last_observed("selector matched earlier in the wait but no longer matches"),
        );
        let err = miss_after_stable_wait(miss, &timeout);
        assert!(matches!(err, Error::SelectorNotMatched { .. }), "{err:?}");
        let d = err.diagnosis().expect("the miss keeps its diagnosis");
        assert_eq!(
            d.last_observed.as_deref(),
            Some("selector matched earlier in the wait but no longer matches; 2 near misses")
        );
        assert_eq!(d.candidates, vec!["button \"Go\"".to_string()]);
    }

    #[test]
    fn a_stable_miss_that_never_matched_is_the_plain_miss() {
        let miss = Error::selector_not_matched("x")
            .diagnose(Diagnosis::new().last_observed("nothing close"));
        let timeout = Error::timeout(std::time::Duration::from_secs(1))
            .diagnose(Diagnosis::new().last_observed("selector never matched"));
        let err = miss_after_stable_wait(miss, &timeout);
        assert_eq!(
            err.diagnosis().and_then(|d| d.last_observed.as_deref()),
            Some("nothing close")
        );
    }

    #[test]
    fn parse_opts_stable_is_a_switch_that_consumes_no_value() {
        let args = strs(&["button", "--stable", "-o", "center"]);
        let (opts, pos) = parse_opts(&args).expect("flags must parse");
        assert!(opts.stable);
        assert_eq!(opts.output_format.as_deref(), Some("center"));
        assert_eq!(pos, vec!["button".to_string()]);
        let (plain, _) = parse_opts(&strs(&["button"])).expect("flags must parse");
        assert!(!plain.stable);
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
    fn cmd_windows_rejects_positional_arg() {
        // `xa11y windows stray` used to silently ignore the extra argument
        // (tenet 1); it is now a usage error.
        let err = cmd_windows(&strs(&["stray"]))
            .expect_err("a positional argument to `windows` must be a usage error");
        assert!(matches!(err, CliError::Usage(_)));
        let msg = format!("{err}");
        assert!(
            msg.contains("stray"),
            "message must name the offending argument: {msg}"
        );
    }

    #[test]
    fn cmd_enable_web_accessibility_refuses_bad_arguments_before_any_platform_call() {
        for args in [&["--shell", "taskbar"][..], &["Electron"][..]] {
            let err = cmd_enable_web_accessibility(&strs(args))
                .expect_err("the argument must be refused");
            assert!(matches!(err, CliError::Usage(_)), "{args:?}: {err}");
        }
        let err = cmd_enable_web_accessibility(&[]).expect_err("no target");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
    }

    #[test]
    fn cmd_windows_rejects_shell_flag() {
        // `xa11y windows --shell taskbar` used to silently list every app's
        // windows (the filter fell away); it must now be a usage error naming
        // the unsupported flag.
        let err = cmd_windows(&strs(&["--shell", "taskbar"]))
            .expect_err("--shell must be rejected by `windows`");
        assert!(matches!(err, CliError::Usage(_)));
        let msg = format!("{err}");
        assert!(
            msg.contains("--shell"),
            "message must name the offending flag: {msg}"
        );
    }

    #[test]
    fn window_key_is_stable_across_rebuilt_snapshots_and_pid_scoped() {
        // Outside the Linux `bus_name` path the key is the per-enumeration
        // handle, scoped by pid. The handle never merges distinct windows,
        // even when they share a stable_id — macOS `AXIdentifier` is not
        // unique per window, so keying two real windows that answer the same
        // id to one entry would drop one from `xa11y windows`. The same built
        // node (same handle) must still key equal.
        let mut a = ElementData::for_role(Role::Window);
        a.pid = Some(7);
        a.stable_id = Some("_NS:136".into());
        let mut b = a.clone();
        assert_eq!(window_key(&a), window_key(&b), "same built node, same key");

        // Same stable_id, same pid, different handle: two distinct windows
        // sharing an AXIdentifier must stay distinct. Regression for the
        // macOS case where AppKit gives every Terminal window the same id.
        b.handle += 1;
        assert_ne!(
            window_key(&a),
            window_key(&b),
            "same stable_id + pid under distinct handles must not merge (shared AXIdentifier)"
        );

        // The key must still be pid-scoped: equal identity under different
        // processes must not collide.
        b.handle = a.handle;
        b.pid = Some(8);
        assert_ne!(
            window_key(&a),
            window_key(&b),
            "same identity under a different pid must not collide"
        );

        // Without a stable_id the per-snapshot handle is the identity:
        // distinct windows (distinct handles) with identical name and bounds
        // must key differently — presenting "same title, same place" as one
        // window would silently discard one.
        let mut c = ElementData::for_role(Role::Window);
        c.pid = Some(7);
        c.name = Some("xa11y Test App".into());
        let mut d = c.clone();
        assert_eq!(
            window_key(&c),
            window_key(&d),
            "the same built node must key equal"
        );
        d.handle += 1;
        assert_ne!(
            window_key(&c),
            window_key(&d),
            "distinct handles must not merge on identical name+bounds"
        );

        // Linux, and only Linux, still collapses true duplicates: the AT-SPI
        // registry surfaces one process twice and the provider records the
        // `bus_name` marker. The identity is (bus_name, object path), so two
        // connections of one process exposing distinct windows under the same
        // `stable_id` must not merge, while one registration re-surfaced
        // across queries (rebuilt node, fresh handle, same bus_name + path)
        // must key equal.
        let mut l = ElementData::for_role(Role::Window);
        l.pid = Some(7);
        l.stable_id = Some("org/a11y/atspi/accessible/2/3".into());
        let mut m = l.clone();
        m.raw
            .insert("bus_name".into(), serde_json::Value::String(":1.42".into()));
        assert_ne!(
            window_key(&l),
            window_key(&m),
            "same path under different bus names must not merge"
        );
        let mut rebuilt = m.clone();
        rebuilt.handle += 100;
        assert_eq!(
            window_key(&m),
            window_key(&rebuilt),
            "same bus_name + path must key equal across rebuilt snapshots"
        );
        let mut n = m.clone();
        n.pid = Some(7);
        assert_eq!(
            window_key(&m),
            window_key(&n),
            "same bus name and path under one pid must key equal"
        );

        // A `bus_name`-carrying entry without a stable_id falls back to the
        // handle — that only happens outside multi-registration, where there
        // is nothing to collapse, so distinct handles must stay distinct.
        let mut p = ElementData::for_role(Role::Window);
        p.pid = Some(7);
        p.raw
            .insert("bus_name".into(), serde_json::Value::String(":1.42".into()));
        let mut q = p.clone();
        q.handle += 1;
        assert_ne!(
            window_key(&p),
            window_key(&q),
            "bus_name entry without a stable_id keys on the distinct handles"
        );
    }

    // ── Window management ───────────────────────────────────────────────────

    #[test]
    fn move_to_rejects_malformed_at_before_reaching_the_provider() {
        // The geometry is parsed before the auto-waiting verb, so a bad
        // `X,Y` cannot burn the timeout or touch the platform (tenet 4's
        // "parse arguments before the first OS call").
        let provider = xa11y_core::mock::build_provider();
        let err = parse_action_args(
            "move-to",
            None,
            None,
            Some("10,not-a-number"),
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect_err("malformed --at must be refused");
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
        assert!(
            provider.actions().is_empty(),
            "no action may reach the provider: {:?}",
            provider.actions()
        );
    }

    #[test]
    fn move_to_and_resize_to_dispatch_valid_geometry_to_the_window_verbs() {
        let provider = xa11y_core::mock::build_provider();
        let app = mock_app(&provider);
        let locator = app.locator("window");
        let move_args = parse_action_args(
            "move-to",
            None,
            None,
            Some("10,20"),
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("a valid move-to must parse");
        perform_action(&locator, "move-to", &move_args).expect("a valid move-to must succeed");
        let resize_args = parse_action_args(
            "resize-to",
            None,
            None,
            None,
            Some("800,600"),
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("a valid resize-to must parse");
        perform_action(&locator, "resize-to", &resize_args)
            .expect("a valid resize-to must succeed");
        let actions = provider.actions();
        assert!(
            actions
                .iter()
                .any(|(_, name, data)| name == "move_to" && data.as_deref() == Some("10,20")),
            "move-to must reach the provider with the parsed point: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .any(|(_, name, data)| name == "resize_to" && data.as_deref() == Some("800x600")),
            "resize-to must reach the provider with the parsed size: {actions:?}"
        );
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
    fn guide_takes_no_arguments() {
        let err = run(&["guide".into(), "--json".into()]).expect_err("must reject");
        assert!(
            matches!(&err, CliError::Usage(m) if m.contains("--json")),
            "{err:?}"
        );
    }

    #[test]
    fn the_usage_text_lists_the_guide_command() {
        assert!(usage_text().contains("xa11y guide"));
    }

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
    fn format_element_hidden_says_why() {
        let mut el = make_element(Role::Button, Some("Long Entry 10"));
        el.states.visible = false;
        el.states.hidden_reason = Some(crate::HiddenReason::ScrolledOut);
        let out = format_element_oneline(&el);
        assert!(out.contains("hidden=scrolled_out"), "{out}");
        assert!(!out.contains(" visible"), "{out}");
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
    fn format_element_keeps_multiline_text_on_one_line() {
        let mut el = make_element(Role::TextArea, Some("Say \"hi\""));
        el.value = Some("Line 1\nLine 2".into());
        el.description = Some("a\tb".into());
        let out = format_element_oneline(&el);
        assert!(!out.contains('\n'), "{out}");
        assert!(out.contains(r#""Say \"hi\"""#), "{out}");
        assert!(out.contains(r#"value="Line 1\nLine 2""#), "{out}");
        assert!(out.contains(r#"description="a\tb""#), "{out}");
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
    fn format_element_password() {
        let mut el = make_element(Role::TextField, Some("PIN"));
        assert!(!format_element_oneline(&el).contains("password"));
        el.states.password = true;
        let out = format_element_oneline(&el);
        assert!(out.starts_with("text_field"), "{out}");
        assert!(out.contains(" password"), "{out}");
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
    fn format_event_detail_names_an_announcements_origin_and_text() {
        let event = xa11y_core::mock::announcement_events().remove(1);
        assert_eq!(format_event_detail(&event), r#" request polite "Saved""#);
        let alert = xa11y_core::mock::announcement_events().remove(3);
        assert_eq!(format_event_detail(&alert), " alert assertive");
    }

    #[test]
    fn an_unknown_events_kind_is_a_usage_error_before_any_lookup() {
        let err = cmd_events(&strs(&["--kinds", "focus", "--pid", "1"])).expect_err("refused");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert!(err.to_string().contains("focus_changed"), "{err}");
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
    fn every_advertised_event_kind_name_round_trips() {
        // `event_kind_names` is what MCP's `kinds` filter validates against.
        // A name that formats as "unknown" would be advertised and never
        // match; a repeat would mean two variants collapsed onto one spelling.
        let names = event_kind_names();
        assert!(!names.contains(&"unknown"), "{names:?}");
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate spelling in {names:?}");
        assert!(names.contains(&"focus_changed"));
        assert!(names.contains(&"state_changed"));
    }

    #[test]
    fn state_flags_are_snake_case_and_match_the_binding_spellings() {
        assert_eq!(format_state_flag(StateFlag::Checked), "checked");
        assert_eq!(format_state_flag(StateFlag::Focusable), "focusable");
    }

    #[test]
    fn a_missing_subscription_says_which_kind_of_miss_and_what_is_open() {
        // Tenet 6: the recovery is readable off the error rather than needing
        // another call to discover.
        let expired = CliError::NoSubscription {
            id: "sub_1".into(),
            expired: true,
            live: vec!["sub_2".into(), "sub_3".into()],
        };
        let text = expired.to_string();
        assert!(text.contains("sub_1"), "{text}");
        assert!(text.contains("expired"), "{text}");
        assert!(text.contains("sub_2, sub_3"), "{text}");
        assert_eq!(expired.exit_code(), 1);

        let unknown = CliError::NoSubscription {
            id: "sub_9".into(),
            expired: false,
            live: Vec::new(),
        }
        .to_string();
        assert!(
            unknown.contains("not a handle this server issued"),
            "{unknown}"
        );
        assert!(
            unknown.contains("no subscriptions are open"),
            "an empty list still has to read as a sentence: {unknown}"
        );
    }

    fn text_change_event(text: Option<&str>, range: Option<TextRange>) -> Event {
        let mut event = Event::new(EventKind::TextChanged, "App", 1);
        event.text_change = Some(
            TextChangeParts {
                kind: TextChangeKind::Inserted,
                range,
                text: text.map(str::to_string),
            }
            .into(),
        );
        event
    }

    #[test]
    fn format_event_detail_text_change() {
        let event = text_change_event(Some("hi"), Some(TextRange { start: 3, end: 5 }));
        assert_eq!(format_event_detail(&event), " inserted 3..5 \"hi\"");
        let event = text_change_event(Some("hi"), None);
        assert_eq!(format_event_detail(&event), " inserted \"hi\"");
        let event = text_change_event(None, None);
        assert_eq!(format_event_detail(&event), " inserted");
        // A TextChanged with no delta (macOS, Windows) prints like before.
        let event = Event::new(EventKind::TextChanged, "App", 1);
        assert!(format_event_detail(&event).is_empty());
    }

    #[test]
    fn text_change_json_is_bounded_and_says_so() {
        let event = text_change_event(Some("abc"), Some(TextRange { start: 0, end: 3 }));
        let json = text_change_json(event.text_change.as_ref().expect("set"));
        assert_eq!(
            json,
            serde_json::json!({"kind": "inserted", "range": {"start": 0, "end": 3}, "text": "abc"})
        );

        let event = text_change_event(None, None);
        let json = text_change_json(event.text_change.as_ref().expect("set"));
        assert_eq!(
            json,
            serde_json::json!({"kind": "inserted", "range": null, "text": null})
        );

        // 300 emoji are 600 UTF-16 units; the cut lands on a character
        // boundary at or under the cap and reports the full length.
        let long = "😀".repeat(300);
        let event = text_change_event(Some(&long), None);
        let json = text_change_json(event.text_change.as_ref().expect("set"));
        let shown = json["text"].as_str().expect("text");
        assert_eq!(shown.encode_utf16().count(), RANGE_TEXT_MAX_UNITS as usize);
        assert_eq!(json["text_truncated"], true);
        assert_eq!(json["text_length"], 600);
        let line = format_event_detail(&event);
        assert!(line.ends_with("(truncated, 600 units)"), "{line}");
    }

    #[test]
    fn announcement_json_reports_the_full_length_of_a_cut_text() {
        // The same contract as a text change: a shortened string never reads
        // as the whole announcement.
        let long = "😀".repeat(300);
        let info: crate::AnnouncementInfo = crate::AnnouncementParts {
            origin: crate::AnnouncementOrigin::Request,
            text: Some(long),
            politeness: None,
            platform_priority: None,
            activity_id: None,
        }
        .into();
        let json = announcement_json(&info);
        let shown = json["text"].as_str().expect("text");
        assert_eq!(shown.encode_utf16().count(), RANGE_TEXT_MAX_UNITS as usize);
        assert_eq!(json["text_truncated"], true);
        assert_eq!(json["text_length"], 600);
    }

    #[test]
    fn clip_utf16_never_splits_a_surrogate_pair() {
        assert_eq!(clip_utf16("a😀", 2), ("a", true));
        assert_eq!(clip_utf16("a😀", 3), ("a😀", false));
        assert_eq!(clip_utf16("", 0), ("", false));
    }

    #[test]
    fn text_change_kinds_have_snake_case_spellings() {
        assert_eq!(
            format_text_change_kind(TextChangeKind::Inserted),
            "inserted"
        );
        assert_eq!(format_text_change_kind(TextChangeKind::Deleted), "deleted");
        assert_eq!(
            format_event_kind(&EventKind::TextSelectionChanged),
            "text_selection_changed"
        );
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
        // The geometry-requiring verbs (move-to / resize-to) are advertised
        // the same way.
        for verb in ACTIONS_REQUIRING_AT {
            assert!(ACTION_NAMES.contains(verb), "{verb} is not an action");
        }
        for verb in ACTIONS_REQUIRING_SIZE {
            assert!(ACTION_NAMES.contains(verb), "{verb} is not an action");
        }
        for verb in ACTIONS_REQUIRING_RANGE {
            assert!(ACTION_NAMES.contains(verb), "{verb} is not an action");
        }
    }

    /// The text legend pads every row to the longest name, so an unbounded
    /// name is multiplied by the row count. On AT-SPI a text area's accessible
    /// name is routinely its whole contents.
    #[test]
    fn a_runaway_accessible_name_cannot_inflate_the_text_legend() {
        let huge = "x".repeat(100_000);
        let rendered = legend_name(Some(&huge));

        assert!(
            rendered.chars().count() < MAX_LEGEND_NAME + 8,
            "a {}-char name rendered as {} chars",
            huge.len(),
            rendered.chars().count()
        );
        assert!(
            rendered.ends_with("…\""),
            "truncation must be visible: {rendered}"
        );
    }

    #[test]
    fn a_name_within_the_limit_is_left_exactly_as_it_is() {
        assert_eq!(legend_name(Some("OK")), "\"OK\"");
        assert_eq!(legend_name(None), "-");
    }

    /// Truncating by bytes would split a multi-byte character and panic.
    #[test]
    fn a_multibyte_name_truncates_on_a_character_boundary() {
        let name = "\u{e9}".repeat(MAX_LEGEND_NAME + 50);
        let rendered = legend_name(Some(&name));
        assert!(rendered.ends_with("…\""), "{rendered}");
    }

    #[test]
    fn set_numeric_value_is_offered_and_needs_a_value() {
        assert!(ACTION_NAMES.contains(&"set-numeric-value"));
        assert!(ACTIONS_REQUIRING_VALUE.contains(&"set-numeric-value"));
    }

    /// `ACTION_NAMES` is the single source of truth for what `action` accepts,
    /// but `--help` spells its own list out. `set-numeric-value` shipped in
    /// the dispatcher and the MCP schema while `--help` never mentioned it, so
    /// the two lists are held together here rather than by convention.
    #[test]
    fn every_action_the_cli_accepts_appears_in_the_help_text() {
        let usage = usage_text();
        for action in ACTION_NAMES {
            assert!(
                usage.contains(action),
                "`{action}` is dispatchable but missing from `xa11y --help`"
            );
        }
    }

    /// The same for the value-requiring split: a verb listed without
    /// "(requires --value)" reads as callable bare, and fails at the platform.
    #[test]
    fn every_value_requiring_action_says_so_in_the_help_text() {
        let usage = usage_text();
        for action in ACTIONS_REQUIRING_VALUE {
            let marked = usage
                .lines()
                .filter(|line| line.contains(action))
                .any(|line| line.contains("requires --value"));
            assert!(
                marked,
                "`{action}` needs a --value but `xa11y --help` does not say so"
            );
        }
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
    fn an_unsupported_feature_leading_with_the_method_name_is_respelled() {
        // The window verbs' `Unsupported` features lead with the provider's
        // method name; the caller typed the kebab verb, so the leading token
        // is respelled while the provider's diagnosis stays verbatim.
        let err = relabel_action_error(
            CliError::Xa11y(Error::Unsupported {
                feature: "enter_fullscreen on window: AT-SPI has no API to alter window state"
                    .into(),
            }),
            "enter-fullscreen",
        );
        assert_eq!(
            err.to_string(),
            "Unsupported: enter-fullscreen on window: AT-SPI has no API to alter window state"
        );
    }

    #[test]
    fn an_unsupported_feature_naming_another_operation_is_left_alone() {
        for (feature, verb) in [
            // A feature that merely starts with the name is a different word.
            ("closed capture surface is unavailable", "close"),
            // A feature naming another operation is information, not a
            // spelling to normalize.
            (
                "handle 1 is a synthesized Application node",
                "enter-fullscreen",
            ),
        ] {
            let err = relabel_action_error(
                CliError::Xa11y(Error::Unsupported {
                    feature: feature.into(),
                }),
                verb,
            );
            assert!(err.to_string().contains(feature), "{err}");
        }
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

    // ── `-o center` on a backend whose bounds are not screen coordinates ────

    #[test]
    fn center_output_is_allowed_where_bounds_are_screen_coordinates() {
        assert!(check_center_output(Ok(true)).is_ok());
    }

    #[test]
    fn center_output_is_refused_where_bounds_are_not_screen_coordinates() {
        let err = check_center_output(Ok(false)).expect_err("must refuse");
        // An operation failure (exit 1), not a usage error (exit 2).
        let CliError::Xa11y(Error::Unsupported { feature }) = &err else {
            panic!("expected Unsupported, got {err:?}");
        };
        assert!(feature.contains("not screen coordinates"), "{feature}");
        assert!(feature.contains("xa11y action press"), "{feature}");
    }

    #[test]
    fn center_output_propagates_an_input_backend_failure() {
        let err = check_center_output(Err(Error::PermissionDenied {
            instructions: "join the input group".into(),
        }))
        .expect_err("must propagate");
        assert!(
            matches!(err, CliError::Xa11y(Error::PermissionDenied { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn pointer_center_follows_the_backend_capability() {
        assert_eq!(
            PointerCenter::from_capability(Ok(true)),
            PointerCenter::Usable
        );
        let PointerCenter::Unavailable(reason) = PointerCenter::from_capability(Ok(false)) else {
            panic!("bounds that are not screen coordinates must withhold the center");
        };
        assert!(reason.contains("not screen coordinates"), "{reason}");
        let PointerCenter::Unavailable(reason) = PointerCenter::from_capability(Err(
            Error::invalid_action_data("invalid XA11Y_LINUX_INPUT_BACKEND value \"xyz\""),
        )) else {
            panic!("an unknown coordinate space must withhold the center");
        };
        assert!(reason.contains("XA11Y_LINUX_INPUT_BACKEND"), "{reason}");
    }

    #[test]
    fn usage_states_the_wayland_center_caveat() {
        let usage = usage_text();
        assert!(usage.contains("not screen coordinates"), "{usage}");
        assert!(usage.contains("`-o center` fails there"), "{usage}");
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

    // ── macOS Accessibility settings hint ───────────────────────────────────

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_yes_accepts_only_the_yes_words() {
        for yes in ["y", "Y", "yes", "YES", "Yes", "  y  ", "\tyes\n"] {
            assert!(parse_yes(yes), "{yes:?} must accept");
        }
        for no in ["n", "no", "", " ", "maybe", "o", "yep"] {
            assert!(!parse_yes(no), "{no:?} must decline");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_accessibility_denial_is_recognized_but_the_screen_recording_is_not() {
        let instructions = xa11y_macos::accessibility_grant_instructions();

        // Provider-init failures reach the CLI as the `PermissionDenied` the
        // provider constructs — the singleton cache preserves the variant.
        let direct = CliError::Xa11y(crate::Error::PermissionDenied { instructions });
        assert!(
            is_accessibility_permission_denied(&direct),
            "the direct PermissionDenied shape must be recognized"
        );

        // Screen Recording is also PermissionDenied, but names a different
        // pane; offering the Accessibility one would be wrong.
        let recording = CliError::Xa11y(crate::Error::PermissionDenied {
            instructions: "Enable Screen Recording in System Settings → Privacy & Security → \
                           Screen & System Audio Recording."
                .to_string(),
        });
        assert!(
            !is_accessibility_permission_denied(&recording),
            "the Screen Recording denial must not open the Accessibility pane"
        );

        // An unrelated platform failure must never trigger the hint.
        let unrelated = CliError::Xa11y(crate::Error::Platform {
            code: -1,
            message: "kAXErrorCannotComplete".into(),
        });
        assert!(!is_accessibility_permission_denied(&unrelated));
        let usage = CliError::Usage("bad flag".into());
        assert!(!is_accessibility_permission_denied(&usage));
    }
    // ── text / table ────────────────────────────────────────────────────────

    #[test]
    fn text_and_table_flags_parse_and_reject_bad_numbers() {
        let (opts, pos) = parse_opts(&strs(&[
            "read",
            "text_area",
            "--range",
            "0,5",
            "--find",
            "x",
            "--occurrence",
            "2",
            "--offset",
            "3",
            "--unit",
            "word",
            "--limit",
            "10",
            "--row",
            "1",
            "--column",
            "2",
        ]))
        .expect("parses");
        assert_eq!(pos, strs(&["read", "text_area"]));
        assert_eq!(opts.range.as_deref(), Some("0,5"));
        assert_eq!(opts.find.as_deref(), Some("x"));
        assert_eq!(
            (
                opts.occurrence,
                opts.offset,
                opts.limit,
                opts.row,
                opts.column
            ),
            (Some(2), Some(3), Some(10), Some(1), Some(2))
        );
        assert_eq!(opts.unit.as_deref(), Some("word"));
        for bad in [["--occurrence", "-1"], ["--offset", "x"], ["--row", "1.5"]] {
            let err = parse_opts(&strs(&bad)).expect_err("bad number");
            assert_eq!(err.exit_code(), 2, "{bad:?}");
        }
    }

    #[test]
    fn range_flags_are_utf16_start_end_pairs() {
        assert_eq!(
            parse_range_flag("2, 7", "--range").expect("parses"),
            TextRange { start: 2, end: 7 }
        );
        for bad in ["7,2", "1", "a,2", "1,2,3"] {
            let err = parse_range_flag(bad, "--range").expect_err(bad);
            assert_eq!(err.exit_code(), 2, "{bad}");
        }
        let err = parse_range_flag("x", "--range").expect_err("no comma");
        assert!(err.to_string().contains("UTF-16 code units"), "{err}");
        let err = parse_text_range("0").expect_err("select-text needs two");
        assert!(err.to_string().contains("UTF-16 code units"), "{err}");
    }

    #[test]
    fn a_find_names_the_range_of_the_range_taking_ops() {
        let a = TextArgs {
            find: Some("Total"),
            occurrence: Some(1),
            range: Some(TextRange { start: 0, end: 9 }),
            ..Default::default()
        };
        assert_eq!(
            parse_text_op("bounds", &a, &CLI_TEXT_ARG_NAMES).expect("valid"),
            TextOp::Bounds {
                range: RangeSpec::Find {
                    needle: "Total".into(),
                    occurrence: 1,
                    within: Some(TextRange { start: 0, end: 9 }),
                }
            }
        );
        assert_eq!(
            parse_text_op("read", &TextArgs::default(), &CLI_TEXT_ARG_NAMES).expect("valid"),
            TextOp::Read {
                range: RangeSpec::Whole,
                limit: TEXT_READ_DEFAULT_UNITS
            }
        );
        let err = parse_text_op("info", &a, &CLI_TEXT_ARG_NAMES).expect_err("info takes none");
        assert!(err.to_string().contains("does not take --range"), "{err}");
    }

    #[test]
    fn the_tree_printer_marks_every_cut() {
        let provider = xa11y_core::mock::build_provider();
        let target = Target::App(mock_app(&provider));
        let full = format_capture(&target.capture(&CaptureOptions::new()).expect("capture"))
            .expect("formats");
        assert!(full
            .iter()
            .all(|l| !l.contains("more") && !l.starts_with("(truncated")));
        assert!(full[0].starts_with("application"), "{full:?}");
        assert!(full.iter().any(|l| l.contains("└── ")), "{full:?}");

        let cut = format_capture(
            &target
                .capture(&CaptureOptions::new().max_nodes(3))
                .expect("capture"),
        )
        .expect("formats");
        let last = cut.last().expect("lines");
        assert!(
            last.starts_with("(truncated: 3 nodes shown, cut by the node limit of 3"),
            "{cut:?}"
        );
        assert!(
            cut.iter().any(|l| l.contains("more (cut by --max-nodes)")),
            "{cut:?}"
        );

        let shallow = format_capture(
            &target
                .capture(&CaptureOptions::new().max_depth(1))
                .expect("capture"),
        )
        .expect("formats");
        assert!(
            shallow
                .iter()
                .any(|l| l.contains("more (cut by --max-depth)")),
            "{shallow:?}"
        );
    }

    #[test]
    fn tree_detail_parses_like_the_mcp_detail_and_refuses_unknown_names() {
        let (opts, _) = parse_command_opts(
            &TREE_FLAGS,
            &strs(&["--detail", "text,relations", "--detail", "formatting"]),
        )
        .expect("flags parse");
        assert_eq!(opts.detail, ["text,relations", "formatting"]);
        assert_eq!(
            parse_tree_detail(["formatting"], "--detail").expect("valid"),
            TreeDetail {
                text: true,
                formatting: true,
                relations: false
            },
            "formatting implies text"
        );
        for bad in ["styles", "text,", ""] {
            let err = cmd_tree(&strs(&["--detail", bad, "--pid", "1"])).expect_err(bad);
            assert!(matches!(err, CliError::Usage(_)), "{bad}: {err}");
            assert!(err.to_string().contains("--detail"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_tree_printer_shows_detail_under_its_element() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let options = TreeDetail {
            text: true,
            formatting: true,
            relations: false,
        }
        .apply(CaptureOptions::new());
        let lines = format_capture(&target.capture(&options).expect("capture")).expect("formats");
        let notes = lines
            .iter()
            .position(|l| l.contains("text_area \"Notes\""))
            .unwrap_or_else(|| panic!("no Notes line: {lines:?}"));
        let text = &lines[notes + 1];
        assert!(
            text.ends_with(r#"text (26 units): "Hello 😀 world\nsecond line""#),
            "the text is one escaped line under its element: {text}"
        );
        assert!(lines[notes + 2].contains("run [0,"), "{lines:?}");
        assert!(
            lines[notes + 2].contains("\"font_weight\":700"),
            "{lines:?}"
        );

        let provider = xa11y_core::mock::build_document_provider();
        let target = Target::App(mock_app(&provider));
        let options = TreeDetail {
            text: false,
            formatting: false,
            relations: true,
        }
        .apply(CaptureOptions::new());
        let lines = format_capture(&target.capture(&options).expect("capture")).expect("formats");
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with(r#"labelled_by → static_text "Email address""#)),
            "{lines:?}"
        );
    }

    #[test]
    fn the_tree_summary_names_a_text_budget_cut() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let options = TreeDetail {
            text: true,
            formatting: false,
            relations: false,
        }
        .apply(CaptureOptions::new().max_text_bytes(4));
        let cap = target.capture(&options).expect("capture");
        assert!(cap.truncated.by_text, "a 4-byte budget cuts the notes");
        let lines = format_capture(&cap).expect("formats");
        let last = lines.last().expect("lines");
        assert!(
            last.contains("the text limit of 4 bytes (--detail text)"),
            "{last}"
        );
        assert!(
            lines.iter().any(|l| l.contains("cut by the text limit):")),
            "{lines:?}"
        );
    }

    #[test]
    fn every_action_argument_is_refused_on_a_verb_that_does_not_read_it() {
        let range = Some(TextRange { start: 0, end: 1 });
        for verb in ACTION_NAMES {
            let value_ok = ACTIONS_READING_VALUE.contains(verb);
            let cases: [(&str, bool, CliResult<ActionArgs>); 4] = [
                (
                    "--value",
                    value_ok,
                    parse_action_args(verb, Some("1"), None, None, None, &CLI_ACTION_ARG_NAMES),
                ),
                (
                    "--range",
                    ACTIONS_REQUIRING_RANGE.contains(verb),
                    parse_action_args(verb, None, range, None, None, &CLI_ACTION_ARG_NAMES),
                ),
                (
                    "--at",
                    ACTIONS_REQUIRING_AT.contains(verb),
                    parse_action_args(verb, None, None, Some("1,2"), None, &CLI_ACTION_ARG_NAMES),
                ),
                (
                    "--size",
                    ACTIONS_REQUIRING_SIZE.contains(verb),
                    parse_action_args(verb, None, None, None, Some("3,4"), &CLI_ACTION_ARG_NAMES),
                ),
            ];
            for (flag, reads, result) in cases {
                match result {
                    Err(CliError::Usage(msg))
                        if msg.starts_with(&format!("{flag} applies only to")) =>
                    {
                        assert!(!reads, "{verb} reads {flag} but refused it: {msg}");
                        assert!(msg.ends_with(&format!(", not {verb}")), "{msg}");
                    }
                    // A verb that reads the flag may still refuse this payload
                    // for another reason (e.g. a reversed range, a missing
                    // companion), but never as "applies only to".
                    _ => assert!(reads, "{verb} accepted {flag}, which it never reads"),
                }
            }
        }
        // An unknown verb stays an unknown verb, not one that "does not take"
        // the argument.
        parse_action_args(
            "frobnicate",
            Some("x"),
            None,
            None,
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("unknown verbs are refused by the dispatcher");
    }

    #[test]
    fn cli_action_refuses_an_unread_flag_before_the_target_is_resolved() {
        let err = cmd_action(&strs(&[
            "press",
            "button",
            "--app",
            "NoSuchAppAnywhere",
            "--value",
            "x",
        ]))
        .expect_err("refused");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        assert_eq!(
            err.to_string(),
            "usage error: --value applies only to set-value, set-numeric-value, type-text, \
             select-text, not press",
        );
    }

    #[test]
    fn select_text_takes_range_or_value_but_not_both() {
        let r = |start, end| Some(TextRange { start, end });
        let ok = parse_action_args(
            "select-text",
            None,
            r(1, 4),
            None,
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("--range alone");
        assert_eq!(ok.range, Some((1, 4)));
        let ok = parse_action_args(
            "select-text",
            Some("1,4"),
            None,
            None,
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("--value alone");
        assert_eq!(ok.range, Some((1, 4)));
        for (value, range, needle) in [
            (Some("1,4"), r(1, 4), "not both"),
            (None, r(4, 1), "must not exceed"),
            (None, None, "requires a range: --range START,END"),
        ] {
            let err = parse_action_args(
                "select-text",
                value,
                range,
                None,
                None,
                &CLI_ACTION_ARG_NAMES,
            )
            .expect_err(needle);
            assert!(matches!(err, CliError::Usage(_)), "{err}");
            assert!(err.to_string().contains(needle), "{err}");
        }
        let err = parse_action_args("press", None, r(0, 1), None, None, &CLI_ACTION_ARG_NAMES)
            .expect_err("range on another verb");
        assert!(
            err.to_string()
                .contains("--range applies only to select-text"),
            "{err}"
        );
    }

    #[test]
    fn cli_select_text_range_is_refused_before_the_target_is_resolved() {
        // `--app` names nothing; a range mistake must still win as exit 2.
        for args in [
            &["--range", "0,2", "--value", "0,2"][..],
            &["--range", "x"][..],
            &[][..],
        ] {
            let mut argv = strs(&["select-text", "text_field", "--app", "NoSuchAppAnywhere"]);
            argv.extend(strs(args));
            let err = cmd_action(&argv).expect_err("refused");
            assert!(matches!(err, CliError::Usage(_)), "{args:?}: {err}");
        }
    }

    #[test]
    fn cli_select_text_range_reaches_the_provider() {
        let provider = xa11y_core::mock::build_table_provider();
        let app = mock_app(&provider);
        let args = parse_action_args(
            "select-text",
            None,
            Some(TextRange { start: 0, end: 5 }),
            None,
            None,
            &CLI_ACTION_ARG_NAMES,
        )
        .expect("parses");
        perform_action(
            &app.locator(r#"text_area[name="Notes"]"#),
            "select-text",
            &args,
        )
        .expect("select-text by --range");
        assert!(
            provider
                .actions()
                .iter()
                .any(|(_, name, data)| name == "set_text_selection"
                    && data.as_deref() == Some("0..5")),
            "{:?}",
            provider.actions()
        );
    }

    #[test]
    fn tree_limits_parse_and_zero_nodes_is_a_usage_error() {
        let (opts, _) =
            parse_opts(&strs(&["--max-nodes", "10", "--max-depth", "2"])).expect("flags parse");
        assert_eq!(opts.max_nodes, Some(10));
        assert_eq!(opts.max_depth, Some(2));
        let err = cmd_tree(&strs(&["--max-nodes", "0", "--pid", "1"])).expect_err("refused");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
    }

    #[test]
    fn cli_text_object_at_renders_the_element_like_other_cli_output() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let element = resolve_table_element(&target, r#"table[name="Prices"]"#)
            .expect("resolves")
            .expect("one table");
        let rendered = text_object_cli(&element);
        assert_eq!(
            rendered,
            serde_json::Value::String(format_element_oneline(&element))
        );
        assert!(
            rendered.as_str().expect("a line").starts_with("table "),
            "{rendered}"
        );
    }

    #[test]
    fn cli_text_runs_through_the_shared_half_against_the_mock() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let element = resolve_text_element(&target, r#"text_area[name="Notes"]"#)
            .expect("resolves")
            .expect("one match");
        let op = parse_text_op(
            "find",
            &TextArgs {
                find: Some("line"),
                ..Default::default()
            },
            &CLI_TEXT_ARG_NAMES,
        )
        .expect("valid");
        let out = run_text_op(
            &element.text().expect("has text"),
            &op,
            &mut text_object_cli,
        )
        .expect("reads");
        assert_eq!(out["match"]["text"], "line");
        assert_eq!(out["match"]["start"], 22);
    }

    #[test]
    fn capped_read_with_a_one_unit_cap_takes_a_whole_surrogate_pair() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let element = resolve_text_element(&target, r#"text_area[name="Notes"]"#)
            .expect("resolves")
            .expect("one match");
        let text = element.text().expect("text");
        // "Hello 😀 world...": the emoji is units 6..8. A 1-unit cap at 6
        // would split it; backing off would read nothing, so it takes both.
        let (s, end) = capped_read(&text, TextRange { start: 6, end: 14 }, 1).expect("reads");
        assert_eq!((s.as_str(), end), ("😀", 8));
        // A cap that splits the pair further in backs off instead.
        let (s, end) = capped_read(&text, TextRange { start: 5, end: 14 }, 2).expect("reads");
        assert_eq!((s.as_str(), end), (" ", 6));
    }

    #[test]
    fn oneline_output_spells_positions_as_selector_attributes() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let line = |sel: &str| {
            let el = target.locator(sel).element().expect("matches");
            format_element_oneline(&el)
        };
        let price = line(r#"table_cell[name="Price"]"#);
        assert!(
            price.contains("row=0 column=1 row_span=1 column_span=2 header=column"),
            "{price}"
        );
        assert!(line(r#"table[name="Prices"]"#).contains("row_count=3 column_count=3"));
        assert!(line(r#"table_row[row="2"]"#).contains("row=2"));
        assert!(line(r#"list_item[name="First"]"#).contains("index=0 size=unknown"));
        // Each spelling round-trips as a selector.
        assert_eq!(
            target
                .locator(r#"table_cell[row="0"][column="1"][column_span="2"]"#)
                .count()
                .expect("counts"),
            1
        );
    }

    #[test]
    fn table_cell_at_misses_name_the_reported_size() {
        let provider = xa11y_core::mock::build_table_provider();
        let target = Target::App(mock_app(&provider));
        let table = resolve_table_element(&target, r#"table[name="Prices"]"#)
            .expect("resolves")
            .expect("one table");
        let op = parse_table_op("cell-at", Some(7), Some(0), "--row", "--column").expect("valid");
        assert!(run_table_op(&table, op).expect("asks").is_empty());
        let msg = no_cell_message(&table, 7, 0);
        assert!(
            msg.contains("row 7, column 0") && msg.contains("3 rows"),
            "{msg}"
        );
        let several = resolve_table_element(&target, "table_cell").expect("resolves");
        assert!(several.is_err(), "several cells are refused");
    }

    #[test]
    fn the_help_lists_every_text_and_table_op() {
        let help = usage_text();
        for op in TEXT_OPS.iter().chain(TABLE_OPS) {
            assert!(help.contains(op), "--help omits {op}");
        }
        assert!(help.contains("select-text (requires --range START,END, in UTF-16 code units"));
    }

    const ALL_COMMAND_FLAGS: [&CommandFlags; 17] = [
        &APPS_FLAGS,
        &WINDOWS_FLAGS,
        &ENABLE_WEB_FLAGS,
        &TREE_FLAGS,
        &FIND_FLAGS,
        &ACTION_FLAGS,
        &TEXT_FLAGS,
        &TABLE_FLAGS,
        &RELATIONS_FLAGS,
        &EVENTS_FLAGS,
        &CLICK_FLAGS,
        &MOVE_FLAGS,
        &DRAG_FLAGS,
        &SCROLL_FLAGS,
        &KEY_FLAGS,
        &TYPE_FLAGS,
        &SCREENSHOT_FLAGS,
    ];

    /// A sample value each flag's parser accepts.
    fn sample_value(flag: &str) -> Option<&'static str> {
        match flag {
            "--stable" => None,
            "--backend" => Some("native"),
            "--pid" | "--count" | "--dx" | "--dy" | "--duration-ms" | "--occurrence"
            | "--offset" | "--limit" | "--max-nodes" | "--max-depth" | "--row" | "--column" => {
                Some("1")
            }
            _ => Some("v"),
        }
    }

    #[test]
    fn known_flags_all_parse() {
        // KNOWN_FLAGS is what `parse_command_opts` checks against; a flag the
        // parser has an arm for but the list lacks would slip past every
        // command's allow-list as a positional.
        for flag in KNOWN_FLAGS {
            let mut args = vec![flag.to_string()];
            args.extend(sample_value(flag).map(str::to_string));
            let (_, pos) = parse_opts(&args).unwrap_or_else(|e| panic!("{flag}: {e}"));
            assert!(pos.is_empty(), "{flag} was not consumed: {pos:?}");
        }
    }

    #[test]
    fn every_command_flag_is_a_known_flag() {
        for spec in ALL_COMMAND_FLAGS {
            for flag in spec.flags {
                assert!(
                    KNOWN_FLAGS.contains(flag),
                    "{} lists {flag}, which the parser does not know",
                    spec.command
                );
            }
        }
    }

    #[test]
    fn every_command_accepts_its_own_flags() {
        for spec in ALL_COMMAND_FLAGS {
            for flag in spec.flags {
                let mut args = vec![flag.to_string()];
                args.extend(sample_value(flag).map(str::to_string));
                parse_command_opts(spec, &args)
                    .unwrap_or_else(|e| panic!("{} {flag}: {e}", spec.command));
            }
        }
    }

    #[test]
    fn a_flag_the_command_does_not_read_is_a_usage_error() {
        for (argv, flag) in [
            (&["tree", "--app", "X", "--limit", "5"][..], "--limit"),
            (
                &["find", "button", "--app", "X", "--kinds", "focus_changed"],
                "--kinds",
            ),
            (&["text", "info", "x", "--app", "X", "--stable"], "--stable"),
            (
                &["table", "cell-at", "x", "--app", "X", "--range", "0,1"],
                "--range",
            ),
            (
                &["action", "press", "x", "--app", "X", "--max-nodes", "3"],
                "--max-nodes",
            ),
            (&["events", "--app", "X", "--max-depth", "2"], "--max-depth"),
            (&["apps", "--app", "X"], "--app"),
            (&["click", "--at", "1,2", "--find", "x"], "--find"),
            (&["key", "a", "--unit", "word"], "--unit"),
            (&["type", "hi", "--held", "Shift"], "--held"),
            (&["screenshot", "--out", "x.png", "--row", "1"], "--row"),
        ] {
            let err = run(&strs(argv)).expect_err(&format!("{argv:?} must be refused"));
            assert_eq!(err.exit_code(), 2, "{argv:?}: {err}");
            assert!(
                err.to_string().contains(&format!("{flag} does not apply")),
                "{argv:?}: {err}"
            );
        }
    }

    #[test]
    fn a_refused_flag_is_named_before_its_value_is_parsed() {
        let err = run(&strs(&["click", "--at", "1,2", "--pid", "abc"])).expect_err("refused");
        assert!(err.to_string().contains("--pid does not apply"), "{err}");
    }

    #[test]
    fn unknown_flags_and_extra_positionals_are_usage_errors() {
        for argv in [
            &["tree", "--app", "X", "--bogus"][..],
            &["apps", "extra"],
            &["tree", "--app", "X", "button"],
            &["events", "--app", "X", "focus_changed"],
            &["find", "button", "label", "--app", "X"],
            &["action", "press", "button", "extra", "--app", "X"],
            &["key", "a", "b"],
            &["type", "hello", "world"],
            &["click", "--at", "1,2", "extra"],
        ] {
            let err = run(&strs(argv)).expect_err(&format!("{argv:?} must be refused"));
            assert_eq!(err.exit_code(), 2, "{argv:?}: {err}");
        }
    }

    #[test]
    fn type_text_may_look_like_a_flag() {
        let (_, pos) =
            parse_command_opts(&TYPE_FLAGS, &strs(&["--not-a-flag"])).expect("literal text");
        assert_eq!(pos, vec!["--not-a-flag"]);
    }

    #[test]
    fn a_bad_backend_is_a_usage_error_naming_the_flag() {
        for argv in [
            &["tree", "--app", "X", "--backend", "foo"][..],
            &["apps", "--backend", "foo"],
            &["capabilities", "--backend", "foo"],
        ] {
            let err = run(&strs(argv)).expect_err("unknown backend");
            assert_eq!(err.exit_code(), 2, "{argv:?}: {err}");
            let msg = err.to_string();
            assert!(msg.contains("--backend"), "{msg}");
            assert!(!msg.contains("XA11Y_ACCESSIBILITY_SOURCE"), "{msg}");
        }
    }

    #[test]
    fn a_bad_platform_is_a_usage_error_with_or_without_backend() {
        for argv in [
            &["capabilities", "--platform", "bogus"][..],
            &["capabilities", "--platform", "bogus", "--backend", "native"],
        ] {
            let err = run(&strs(argv)).expect_err("unknown platform");
            assert_eq!(err.exit_code(), 2, "{argv:?}: {err}");
        }
    }

    #[test]
    fn backend_names_are_the_ones_the_library_accepts() {
        for backend in BACKENDS {
            crate::capabilities_for_backend(Some(Platform::Windows), None, Some(backend))
                .unwrap_or_else(|e| panic!("{backend}: {e}"));
        }
        assert!(crate::capabilities_for_backend(Some(Platform::Windows), None, Some("x")).is_err());
    }

    #[test]
    fn screenshot_refuses_a_target_without_annotate() {
        for flag in [
            &["--app", "X"][..],
            &["--pid", "1"],
            &["--shell", "dock"],
            &["--backend", "native"],
        ] {
            let mut argv = vec!["screenshot", "--out", "x.png"];
            argv.extend_from_slice(flag);
            let err = run(&strs(&argv)).expect_err("inert target");
            assert_eq!(err.exit_code(), 2, "{argv:?}: {err}");
            assert!(err.to_string().contains(flag[0]), "{err}");
        }
    }
}
