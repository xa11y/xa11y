//! The MCP tool table and its handlers.
//!
//! One tool per CLI verb, with the same name, so the CLI reference and the
//! tool list describe the same operations. The handlers reuse `cli`'s parsers
//! and dispatchers ([`crate::cli::resolve_app`], [`crate::cli::perform_action`],
//! [`crate::cli::parse_key_name`]) rather than re-deriving them, so the two
//! surfaces cannot drift on what `--app` means or which key names exist.
//!
//! What they deliberately do *not* reuse is `cli`'s `cmd_*` functions: those
//! write to stdout, which on the stdio transport carries protocol messages
//! only.
//!
//! # Bounded output
//!
//! Every result here lands in a model's context window, so the tree is
//! depth- and node-limited and match lists are capped. Truncation is always
//! reported in the payload — a silently shortened tree reads as a complete
//! one, which is worse than no tree.

use super::refs::{Capture, RefTable};
use serde_json::{json, Map, Value};

use super::base64;
use super::events::{Registry, BUFFER_CAPACITY, EXPIRY, MAX_SUBSCRIPTIONS};
use crate::cli::{
    self, parse_button, parse_held, parse_key_name, resolve_app, resolve_target, CliError,
    CliResult, Opts, Target, TreeDetail, ACTIONS_REQUIRING_AT, ACTIONS_REQUIRING_RANGE,
    ACTIONS_REQUIRING_SIZE, ACTIONS_REQUIRING_VALUE, ACTION_NAMES, TREE_DETAILS,
};
use crate::{
    App, AppExt, ClickOptions, ClickTarget, DragOptions, Element, Locator, Rect, ScrollDelta,
    ShellSurface, ShellSurfaceExt,
};

/// Depth used by `tree` when the caller does not ask for one.
const TREE_DEFAULT_MAX_DEPTH: usize = 12;
/// Hard ceiling on nodes in one `tree` result, whatever depth was requested,
/// and the default of its `max_nodes`.
const TREE_MAX_NODES: usize = 2_000;
/// Matches returned by `find` when the caller does not ask for a limit.
const FIND_DEFAULT_LIMIT: usize = 50;
/// Ceiling on `find`'s `limit`, so a caller cannot ask for an unbounded dump.
const FIND_MAX_LIMIT: usize = 500;
/// Longest candidate list carried in a failure's diagnosis.
const MAX_DIAGNOSIS_CANDIDATES: usize = 20;
/// Ceiling, in UTF-8 bytes, on each free-text field of an element node
/// (`name`, `value`, `description`).
///
/// The node and match limits bound how many elements a result carries, not
/// how big one of them is: a single text area holding a document would
/// otherwise put the whole document into the context window. A cut field
/// says so beside itself (`<field>_truncated`, `<field>_bytes`), because a
/// silently shortened value reads as the complete one.
pub(super) const MAX_FIELD_BYTES: usize = 1_024;
/// Events returned by one `events_poll` when the caller does not ask.
const EVENTS_DEFAULT_MAX: usize = 100;
/// Ceiling on `events_poll`'s `max`, so one poll cannot dump a full buffer.
const EVENTS_MAX_MAX: usize = 500;

/// Inclusive bounds for one integer tool argument.
///
/// The schema fragment and the handler's range check are both derived from
/// the same value, so a client that validates against `inputSchema` and one
/// that does not are refused on exactly the same inputs. They had already
/// disagreed in both directions: `click`'s `count` declared `minimum: 1` and
/// accepted `0`, which reached the backend as "click zero times" and came
/// back `ok: true`; and it capped at ten without the schema ever saying so,
/// which reads to a model as an arbitrary refusal.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    min: i64,
    max: i64,
}

/// Screen coordinates. The range is `i32`'s because that is what
/// [`crate::Point`] and [`crate::Rect`] carry; stating it beats a caller
/// discovering it from an "out of range" reply.
const COORD: Bounds = Bounds {
    min: i32::MIN as i64,
    max: i32::MAX as i64,
};
/// Process ids. Zero is not a process any accessibility API reports.
const PID: Bounds = Bounds {
    min: 1,
    max: u32::MAX as i64,
};
/// `tree`'s `max_depth`. The ceiling is far past any real UI: it exists so a
/// caller cannot ask for a walk the node budget would have to cut anyway.
const TREE_DEPTH: Bounds = Bounds { min: 0, max: 64 };
/// `tree`'s `max_nodes`. At least the root; at most the ceiling that keeps a
/// result inside a context window, which is also the default.
const TREE_NODES: Bounds = Bounds {
    min: 1,
    max: TREE_MAX_NODES as i64,
};
/// `find`'s `limit`. Zero would return nothing while reporting matches.
const FIND_LIMIT: Bounds = Bounds {
    min: 1,
    max: FIND_MAX_LIMIT as i64,
};
/// `click`'s `count`. Zero clicks is not a click.
const CLICK_COUNT: Bounds = Bounds { min: 1, max: 10 };
/// `drag`'s `duration_ms`. A minute is already far longer than any gesture an
/// application animates.
const DRAG_DURATION_MS: Bounds = Bounds {
    min: 0,
    max: 60_000,
};
/// A screenshot region's width and height, which cannot be empty.
const SCREENSHOT_EXTENT: Bounds = Bounds {
    min: 1,
    max: u32::MAX as i64,
};
/// A UTF-16 code-unit offset for the `text` tool. The range is `u32`'s
/// because that is what [`crate::TextRange`] carries.
const TEXT_OFFSET: Bounds = Bounds {
    min: 0,
    max: u32::MAX as i64,
};
/// `text`'s `occurrence`, 0-based.
const TEXT_OCCURRENCE: Bounds = Bounds {
    min: 0,
    max: u32::MAX as i64,
};
/// `text`'s `limit`. The schema states the widest cap (`read`'s, in units);
/// `cli::parse_text_op` applies the per-operation one and names it.
const TEXT_LIMIT: Bounds = Bounds {
    min: 1,
    max: cli::TEXT_READ_MAX_UNITS as i64,
};
/// `table`'s `row` and `column`, 0-based.
const TABLE_INDEX: Bounds = Bounds {
    min: 0,
    max: u32::MAX as i64,
};
/// `events_poll`'s `max`. Zero would report no events while draining none.
const EVENTS_MAX: Bounds = Bounds {
    min: 1,
    max: EVENTS_MAX_MAX as i64,
};
/// `events_poll`'s `timeout_ms`.
///
/// The ceiling is well under any typical client request timeout, and it has a
/// second reason to stay low: the stdio session loop handles one message at a
/// time, so a blocking poll blocks every other tool call for its duration.
const EVENTS_TIMEOUT_MS: Bounds = Bounds {
    min: 0,
    max: 15_000,
};

impl Bounds {
    /// The JSON Schema fragment declaring an integer argument in this range.
    fn property(self, description: String) -> Value {
        json!({
            "type": "integer",
            "minimum": self.min,
            "maximum": self.max,
            "description": description,
        })
    }

    /// Range-check a value, naming the bounds the schema advertised.
    fn check(self, key: &str, value: i64) -> CliResult<i64> {
        if value < self.min || value > self.max {
            return Err(usage(format!(
                "\"{key}\" must be between {} and {}, got {value}",
                self.min, self.max
            )));
        }
        Ok(value)
    }
}

/// What an element's `actions` field is, and what it is not.
///
/// Carried by both element-returning tools, because the field is the first
/// thing a model reads to decide what it may call — and reading it as a
/// capability list is wrong in both directions. It is a faithful report of
/// what the application advertises through the platform's action interface
/// (AT-SPI `Action`, UIA patterns, `AXActionNames`), which is a different set
/// from the verbs `action` accepts: a slider that lists nothing still
/// increments, and a check box that lists only `press` still toggles.
const ACTIONS_FIELD_NOTE: &str = "\
`actions` on an element lists the actions the application advertises through \
the platform's accessibility action interface. It is neither the set of verbs \
the `action` tool accepts nor a capability list, and an absent or empty \
`actions` rules nothing out. Choose the verb from the element's reported \
properties instead: `increment` / `decrement` / `set-numeric-value` apply to \
anything reporting `numeric_value` (with `min_value` / `max_value` as its \
range), `set-value` / `type-text` / `select-text` to anything whose `states` \
include `editable`, `focus` to anything `focusable`, `toggle` to anything \
carrying a `checked` state, and `expand` / `collapse` to anything carrying \
`expanded`. `press` is the general activation verb and applies to any \
control, whether or not it lists one. A verb the element genuinely cannot do comes back as \
`action_not_supported` rather than doing something else, so trying the one \
the properties imply is safe.";

/// What an element's `center` is, and where there is none (issue #573).
///
/// On Linux's uinput input backend (Wayland sessions) the library refuses an
/// element-targeted pointer action, because accessibility bounds are not
/// screen coordinates there. A description that promised `center` as a click
/// target without this would be a contract the handler cannot keep.
pub(super) const CENTER_NOTE: &str = "\
Where element bounds are not screen coordinates (Linux with the uinput input \
backend, which Wayland sessions use), elements carry no `center`, the result's \
`center_unavailable` says why, and `bounds` is not a position on screen. There, \
act on an element with `action` (press, focus, ...) rather than with the \
coordinate tools.";

/// The per-field byte cap, as the element-returning tools state it.
///
/// Built from [`MAX_FIELD_BYTES`] so the number a model reads is the number
/// the encoder enforces.
fn field_cap_note() -> String {
    format!(
        "`name`, `value` and `description` are each cut to at most {MAX_FIELD_BYTES} \
         bytes of UTF-8, so one element holding a document cannot flood the result. A \
         cut field is followed by `<field>_truncated: true` and `<field>_bytes`, its \
         full length in bytes; the field then holds a prefix, not the whole text. \
         Selectors still match against the full text."
    )
}

/// Selector syntax, as a model needs to be told it.
///
/// Every rule here is one an agent got wrong against a real application: it
/// wrote `checkbox` for `check_box`, `[checked]` for `[checked="on"]`,
/// `[name*="item" i]` for `[name*="item"]`, and `:checked` / `:focused` /
/// `:nth-child(1)` where only attribute filters and `:nth(n)` exist.
const SELECTOR_SYNTAX: &str = "\
Roles are snake_case: `check_box`, `radio_button`, `text_field`, \
`static_text`, `list_item`, `menu_item`. Attribute filters are \
`[attr=\"value\"]` and the value must be quoted — there is no presence-only \
form, so `[checked]` is a syntax error where `[checked=\"on\"]` works. \
Operators are `=` (exact, case-sensitive), `*=` (contains), `^=` (starts \
with) and `$=` (ends with); the last three are already case-insensitive and \
there is no trailing `i` flag. Chain filters to AND them: \
`button[name=\"OK\"][enabled=\"true\"]`. Combinators are a space (descendant) \
and `>` (direct child), and a comma unions clauses. `:nth(n)` picks the nth \
match in document order, 1-based, and is the only pseudo-class — `:checked`, \
`:focused` and `:nth-child()` do not exist here. States are matchable as \
ordinary attributes: `checked` takes `\"on\"` / `\"off\"` / `\"mixed\"`, and \
`\"true\"` / `\"false\"` work for `enabled`, `visible`, `focused`, \
`focusable`, `selected`, `editable`, `expanded`, `required`, `busy`, \
`modal`, `active`, `password`, and the window states `minimized`, `maximized`, and \
`fullscreen`. A window state that the platform cannot report reads as absent \
(no attribute) rather than `\"false\"`, so match `minimized=\"true\"` if you \
need to be sure the state is known. `hidden_reason` is present only on an \
element that is not visible, and says why: `\"scrolled_out\"` (scroll it into \
view), `\"window_minimized\"`, `\"not_showing\"`, or `\"unknown\"`. \
Table and list positions are numeric attributes, 0-based like every index \
except `:nth`: `row`, `column`, `row_span` and `column_span` on a cell (`row` \
also on a table row), `index`, `size` and `depth` on a list, tree or tab \
item, and `row_count` / `column_count` on a table. They take a quoted \
non-negative integer and only `=`, as in `table_cell[row=\"2\"][column=\"0\"]`; \
`size`, `row_count` and `column_count` also take `\"unknown\"`.";

/// What one tool call produced.
#[derive(Debug)]
pub(crate) struct ToolOutput {
    /// Unstructured content blocks, as MCP `content` entries.
    pub(crate) content: Vec<Value>,
    /// Machine-readable result, mirrored into `structuredContent`.
    pub(crate) structured: Option<Value>,
}

impl ToolOutput {
    /// A plain text result with no structured counterpart.
    ///
    /// Every real handler returns structured data, so this exists for the
    /// protocol layer's stub host, which needs a trivially-satisfiable
    /// success to test routing against.
    #[cfg(test)]
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![json!({ "type": "text", "text": text.into() })],
            structured: None,
        }
    }

    /// A structured result.
    ///
    /// The serialized JSON is repeated in a text block because the spec asks
    /// a tool returning `structuredContent` to do so, and because the oldest
    /// revision this server speaks (2025-03-26) predates `structuredContent`
    /// entirely — for those clients the text block is the only copy.
    fn json(value: Value) -> Self {
        let text = serde_json::to_string(&value)
            .unwrap_or_else(|e| format!("<result could not be serialized: {e}>"));
        Self {
            content: vec![json!({ "type": "text", "text": text })],
            structured: Some(value),
        }
    }

    /// A PNG image result.
    fn png(bytes: &[u8], summary: Value) -> Self {
        Self {
            content: vec![
                json!({
                    "type": "image",
                    "data": base64::encode(bytes),
                    "mimeType": "image/png",
                }),
                json!({
                    "type": "text",
                    "text": serde_json::to_string(&summary).unwrap_or_default(),
                }),
            ],
            structured: Some(summary),
        }
    }
}

/// The set of tools a session can list and call.
///
/// A trait so the protocol layer's routing, version negotiation, and error
/// mapping are testable against a stub, with no accessibility backend and no
/// display.
pub(crate) trait ToolHost {
    /// Tool definitions, in a stable order (the spec asks for determinism so
    /// clients can cache the list).
    fn list(&self) -> Vec<Value>;
    /// Whether `name` is a tool this host serves.
    fn has_tool(&self, name: &str) -> bool;
    /// Invoke a tool. Errors become `isError: true` results.
    fn call(&self, name: &str, args: &Value) -> CliResult<ToolOutput>;
}

/// The real tool host, backed by the platform accessibility provider.
///
/// Holds the session's event subscriptions. Everything else here is
/// stateless, and this is state only because MCP has no session of its own to
/// hang a live subscription on — see [`super::events`]. Dropping the host
/// stops every drainer and cancels every platform subscription.
pub(crate) struct Xa11yTools {
    events: Registry,
    refs: RefTable,
}

impl Xa11yTools {
    pub(crate) fn new() -> Self {
        Self {
            events: Registry::new(),
            refs: RefTable::default(),
        }
    }
}

/// Every tool name, in list order.
const TOOL_NAMES: &[&str] = &[
    "apps",
    "capabilities",
    "shell",
    "windows",
    "enable_web_accessibility",
    "tree",
    "find",
    "action",
    "text",
    "table",
    "relations",
    "click",
    "move",
    "drag",
    "scroll",
    "key",
    "type",
    "screenshot",
    "events_start",
    "events_poll",
    "events_stop",
];

impl ToolHost for Xa11yTools {
    fn list(&self) -> Vec<Value> {
        TOOL_NAMES
            .iter()
            .map(|name| tool_definition(name))
            .collect()
    }

    fn has_tool(&self, name: &str) -> bool {
        TOOL_NAMES.contains(&name)
    }

    fn call(&self, name: &str, args: &Value) -> CliResult<ToolOutput> {
        match name {
            "apps" => tool_apps(),
            "capabilities" => tool_capabilities(args),
            "shell" => tool_shell(),
            "windows" => tool_windows(args, &self.refs),
            "enable_web_accessibility" => tool_enable_web_accessibility(args),
            "tree" => tool_tree(args, &self.refs),
            "find" => tool_find(args, &self.refs),
            "action" => tool_action(args, &self.refs),
            "text" => tool_text(args, &self.refs),
            "table" => tool_table(args, &self.refs),
            "relations" => tool_relations(args, &self.refs),
            "click" => tool_click(args),
            "move" => tool_move(args),
            "drag" => tool_drag(args),
            "scroll" => tool_scroll(args),
            "key" => tool_key(args),
            "type" => tool_type(args),
            "screenshot" => tool_screenshot(args),
            "events_start" => tool_events_start(&self.events, args),
            "events_poll" => tool_events_poll(&self.events, args),
            "events_stop" => tool_events_stop(&self.events, args),
            // Unreachable through `Session`, which checks `has_tool` first.
            // Kept as a real error rather than an `unreachable!` so a future
            // caller that skips the check gets a diagnosis, not a panic
            // (tenet 4).
            other => Err(CliError::Usage(format!("unknown tool: {other}"))),
        }
    }
}

// ── Tool definitions ────────────────────────────────────────────────────────

/// Properties naming the *application* half of a target — `app` and `pid` —
/// shared by every a11y tool.
fn app_properties() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "app".into(),
        json!({
            "type": "string",
            "description": "Application name, matched exactly and case-sensitively — \
                            not a substring, so the WinForms test app answers to \
                            \"xa11y-winforms-test-app\" and not to \"winforms\". Take \
                            the spelling from the `apps` tool rather than guessing it: \
                            an application often reports its interpreter or bundle name \
                            (a Qt app run under Python reports \"python\"). Give this, \
                            `pid`, or `shell`.",
        }),
    );
    props.insert("backend".into(), json!({"type":"string", "enum": cli::BACKENDS, "description":"Accessibility source for this attachment; default: server environment. IA2 requires Windows."}));
    props.insert(
        "pid".into(),
        PID.property(
            "Process id of the target application. Give this or `app`. Alongside \
             `shell` it picks between surfaces of one kind rather than naming an \
             application — and it is the *only* way to pick between them, so it \
             cannot separate several surfaces owned by one process (two panel rows \
             from one xfce4-panel). Those stay ambiguous and the call is refused."
                .into(),
        ),
    );
    props
}

/// Properties naming the target — an application or a shell surface — shared
/// by every a11y tool.
/// The `ref` property of `action`, `text` and `table`: an alternative to
/// `selector` naming an element from an earlier `tree` / `find`.
fn ref_property() -> Value {
    json!({
        "type": "string",
        "description": format!(
            "An element `ref` (`c3:17`) from a recent {issuers}, in place of \
             `selector`: give exactly one of the two, and no `app` / `pid` / `shell` / \
             `backend` with a ref. A ref names exactly that element. If it was removed, its \
             application restarted, or its id now names a different element, the call fails \
             with `element_stale` (and `stale_reason`) rather than finding a similar one. \
             The server keeps the refs of its {max} most recent results that returned \
             elements; an older ref fails with `ref_expired`, and an index past the end of a \
             result it still holds with `invalid_arguments`.",
            issuers = super::refs::ISSUERS,
            max = super::refs::MAX_CAPTURES,
        ),
    })
}

fn app_target_properties() -> Map<String, Value> {
    let mut props = app_properties();
    props.insert(
        "shell".into(),
        json!({
            "type": "string",
            "enum": cli::shell_kind_names(),
            "description": "Target an OS shell surface instead of an application: the \
                            taskbar, the desktop, a dock or panel, the menu bar, a \
                            process's status items, or an open flyout. Call the `shell` \
                            tool for what is on screen right now and pass a listed \
                            `kind` here. Mutually exclusive with `app` — passing both \
                            is refused. When several surfaces share a kind, add `pid` \
                            to pick one; without it the call comes back as \
                            `ambiguous_shell_surface` with the candidates. `pid` is \
                            the only disambiguator there is: surfaces of one kind \
                            owned by the same process cannot be told apart, and stay \
                            refused whatever you pass.\n\n\
                            Kinds are platform-specific, and this list is the same on \
                            every platform: `menu_bar`, `status_items` and `dock` exist \
                            only on macOS, `taskbar` only on Windows, `panel` only on \
                            Linux, `desktop` and `flyout` on macOS and Windows. Asking \
                            for one the platform cannot produce is not an error — it \
                            comes back as `no_match` after the wait, exactly like a \
                            surface that is merely not open. Call `shell` first rather \
                            than guessing a kind: it lists what actually exists here.",
        }),
    );
    props
}

fn object_schema(properties: Map<String, Value>, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
        "additionalProperties": false,
    })
}

fn tool(name: &str, title: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn held_property() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Modifier keys held for the duration of the gesture, \
                        e.g. [\"Shift\", \"Meta\"].",
    })
}

fn tool_definition(name: &str) -> Value {
    match name {
        "apps" => tool(
            "apps",
            "List applications",
            "List running applications with their process ids. Start here to find \
             the `app` or `pid` the other tools need.",
            json!({ "type": "object", "additionalProperties": false }),
        ),
        "capabilities" => {
            let mut props = Map::new();
            props.insert("backend".into(), json!({"type":"string", "enum": cli::BACKENDS, "description":"Accessibility source to describe. Default: the server environment's source for the running platform, and native for another `platform`. IA2 has its own table, including unsupported events."}));
            props.insert(
                "platform".into(),
                json!({
                    "type": "string",
                    "enum": crate::Platform::ALL.iter().map(|p| p.to_snake_case()).collect::<Vec<_>>(),
                    "description": "Platform to describe. Default: the one this server runs on.",
                }),
            );
            props.insert(
                "os_version".into(),
                json!({
                    "type": "string",
                    "description": "Dotted OS version (\"14.5\", \"10.0.19045\") to narrow the \
                        answers to. Default: the running version on macOS and Windows. Linux \
                        takes none.",
                }),
            );
            props.insert(
                "names".into(),
                json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Feature names (\"event:menu_opened\") or areas ending in \
                        \":\" (\"window:\") to return. Default: every feature.",
                }),
            );
            tool(
                "capabilities",
                "What this platform can report",
                "Say, per feature, whether this platform's accessibility backend can \
                 report it: `supported`, `partial` (some toolkits, apps or display servers \
                 only; `note` says which) or `unsupported` (never). Reads a fixed table and \
                 touches no application.\n\n\
                 Use it to tell \"this element or app has nothing to report\" from \
                 \"this platform never reports it\". A field that is always absent, an \
                 event kind that never arrives from `events_poll`, or an action that \
                 fails as unsupported is explained here. Check an event kind before waiting \
                 on it: `event:menu_opened` is `unsupported` on Linux, so a wait for it \
                 there can only time out.\n\n\
                 Names are `<area>:<name>`; the areas are `event:`, `action:`, `app:`, \
                 `window:`, `state:`, `table:` and `text:`. An unknown name is an \
                 `invalid_arguments` failure listing the closest known names.",
                object_schema(props, &[]),
            )
        }
        "shell" => tool(
            "shell",
            "List OS shell surfaces",
            "List the OS shell surfaces on screen: the taskbar, the desktop, docks and \
             panels, the menu bar, per-process status items, and any open flyout. Each \
             row gives the `kind` to pass as `shell` to `tree`, `find` and `action`, \
             plus the surface's `name` and owning `pid`.\n\n\
             The listing is live rather than a fixed table. A `flyout` exists only \
             while it is open, and a platform with no surface of a kind lists none — \
             that is scope, not failure. Enumerating never opens or presses anything, so \
             calling this tool cannot change what is on screen.\n\n\
             Content that exists only behind a press has to be opened by you first. On \
             Windows the hidden tray icons are not in the taskbar surface at all: press \
             the taskbar button named \"Show Hidden Icons\" with `action`, call `shell` \
             again, and target the flyout that has appeared with `shell: \"flyout\"`.",
            json!({ "type": "object", "additionalProperties": false }),
        ),
        "windows" => {
            // Application-only target: `windows` lists application windows,
            // never shell surfaces, so `shell` is not advertised here (it is
            // a real filter the handler does not implement — advertising it
            // and then ignoring it would let a model's `{"shell": "taskbar"}`
            // silently widen into "all windows"). The handler rejects it
            // defensively too, in case a client sends it anyway.
            let mut props = app_properties();
            props.insert(
                "limit".into(),
                FIND_LIMIT.property(format!(
                    "Maximum windows to return. Default {FIND_DEFAULT_LIMIT}."
                )),
            );
            tool(
                "windows",
                "List top-level windows",
                &format!(
                    "List the top-level windows of one application (give `app` or \
                 `pid`) or of every running application when neither is given. \
                 Each window reports its `name`, `bounds`, `states` (including \
                 `minimized` / `maximized` / `fullscreen` when the platform can \
                 report them) \
                 and the actions it advertises, in the element shape `find` \
                 uses. Each window carries a `ref` (`c3:0`) that `action`, \
                 `text`, `table` and `relations` accept in place of a selector, \
                 to act on exactly that window; a window selector works too. \
                 Results are capped at the `limit`, and \
                 `truncated` in the result says whether anything was dropped. \
                 If one application cannot be enumerated, it is skipped and \
                 reported in `errors` (with `count` covering only the windows \
                 that made it), rather than failing the whole listing. \
                 `errors` is capped at the same `limit`, with \
                 `errors_truncated` saying whether error entries were \
                 dropped. \
                 Windows whose state is unknown to the platform simply omit \
                 those states rather than reporting a guessed value.\n\n\
                 The verbs `action` accepts (a window selector) cover this \
                 surface: `activate`, `minimize`, `maximize`, `enter-fullscreen`, \
                 `restore`, `close`, `move-to` (with `at`) and `resize-to` (with \
                 `size`). `maximize` and `enter-fullscreen` are distinct: the \
                 former is the platform's maximized state (Windows), the latter \
                 native fullscreen (macOS); a platform without one does not \
                 advertise it. `restore` is the inverse of all three entry \
                 verbs — it leaves minimized, maximized, or fullscreen; there \
                 is no separate exit-fullscreen action.\n\n{field_cap}",
                    field_cap = field_cap_note(),
                ),
                object_schema(props, &[]),
            )
        }
        "enable_web_accessibility" => tool(
            "enable_web_accessibility",
            "Turn on web content accessibility",
            &format!(
                "Ask a Chromium or Electron application (give `app` or `pid`) to build \
                 the accessibility tree for its web content, then wait for that content \
                 to appear. The call does not check that the app is Chromium-based. Until an app is asked, `tree` shows its window and an empty \
                 `web_area`, and `find` matches nothing on the page. A not-found failure \
                 inside such an app says so in its `last_observed`.\n\n\
                 macOS only. The request is never made implicitly. Linux and Windows \
                 return an `unsupported` failure saying what to do instead (on Linux, \
                 relaunch the app with `--force-renderer-accessibility`). On macOS it \
                 sets `AXManualAccessibility` on the application; an app that neither \
                 accepts the write nor reads it back as set returns \
                 `accessibility_not_enabled` naming what the write and the read returned (relaunch such an app with \
                 `--force-renderer-accessibility`). `AXEnhancedUserInterface` is never \
                 set. An app that keeps the value but has no web content, such as a native \
                 one, ends in a `timeout` instead. The attribute stays set to true \
                 afterwards and is not restored, so call this only for apps whose web \
                 content you need.\n\n\
                 The wait runs up to the default timeout, currently {timeout}. Set \
                 {timeout_env} (in seconds) in the environment the server is launched \
                 with to change it. A `timeout` failure says how many web areas the app \
                 had. `ok: true` means some web area has content; read it with `tree`.",
                timeout = default_timeout_label(),
                timeout_env = crate::DEFAULT_TIMEOUT_ENV_VAR,
            ),
            object_schema(app_properties(), &[]),
        ),
        "tree" => {
            let mut props = app_target_properties();
            props.insert(
                "max_depth".into(),
                TREE_DEPTH.property(format!(
                    "How deep to walk. Default {TREE_DEFAULT_MAX_DEPTH}. \
                     0 returns the application node alone. Results are also \
                     capped at `max_nodes` nodes; `truncated` in the result \
                     says whether either limit was hit, and each node that lost \
                     children to one is marked where it happened."
                )),
            );
            props.insert(
                "max_nodes".into(),
                TREE_NODES.property(format!(
                    "Most nodes to return, read breadth-first. Default and maximum \
                     {TREE_MAX_NODES}: a larger value is refused rather than \
                     lowered, so ask for less to keep a result small, and use \
                     `find` or a deeper `tree` on one node to see past the cap."
                )),
            );
            props.insert(
                "detail".into(),
                json!({
                    "type": "array",
                    "items": { "type": "string", "enum": TREE_DETAILS },
                    "description": format!(
                        "Extra reads per node. `text`: the full text of each element \
                         with a text interface, as `text` (`content`, `length` in UTF-16 \
                         units, `truncated`), within {} bytes in total, so document \
                         text that no action needs is still read. `formatting`: \
                         `text` as well, plus `text.runs`, each `start`, `end` (UTF-16, \
                         0-based, half-open) and `attributes`, within {} runs in total. \
                         `relations`: \
                         `relations` maps each reported kind (`labelled_by`, \
                         `controls`, ...) to its targets, each a `ref`; a target outside \
                         the result also carries `role`, `name` and `outside: true`. \
                         Each costs a platform read per node.",
                        crate::capture::DEFAULT_MAX_TEXT_BYTES,
                        crate::capture::DEFAULT_MAX_RUNS
                    ),
                }),
            );
            tool(
                "tree",
                "Read accessibility tree",
                &format!(
                    "Read an application's accessibility tree: roles, names, values, \
                     states, screen bounds, and advertised actions. Use this to \
                     understand a window before acting on it, and again afterwards to \
                     confirm what an action changed. Pass `shell` in place of `app` / \
                     `pid` to read an OS shell surface's tree instead.\n\n\
                     A node whose children were left out by `max_depth` or \
                     `max_nodes` (at most {TREE_MAX_NODES}) carries `subtree_truncated: true` and \
                     `children_count` (how many children it really has; `children`, \
                     if present, holds only the first of them). A node without \
                     `subtree_truncated` is complete: no `children` means a leaf. To \
                     see what was cut, call `find` with a selector reaching into that \
                     node (`list[name=\"Files\"] list_item`), or `tree` again with a larger `max_depth`.\n\n\
                     {CENTER_NOTE}\n\n\
                     Every node carries a `ref` (`c3:17`) that `action`, `text` and `table` \
                     accept in place of a selector, to act on exactly that element.\n\n\
                     {field_cap}\n\n\
                     {ACTIONS_FIELD_NOTE}",
                    field_cap = field_cap_note(),
                ),
                object_schema(props, &[]),
            )
        }
        "find" => {
            let mut props = app_target_properties();
            props.insert(
                "selector".into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "xa11y selector, CSS-like. Examples: `button[name=\"OK\"]`, \
                         `text_field[name*=\"Search\"]`, `window > group button`, \
                         `check_box[checked=\"on\"]`, `radio_button:nth(2)`, \
                         `button[name=\"Save\"], button[name=\"Save As\"]`.\n\n\
                         {SELECTOR_SYNTAX}"
                    ),
                }),
            );
            props.insert(
                "limit".into(),
                FIND_LIMIT.property(format!(
                    "Maximum matches to return. Default {FIND_DEFAULT_LIMIT}."
                )),
            );
            props.insert(
                "stable".into(),
                json!({
                    "type": "boolean",
                    "description": "Wait until the one match is visible and its bounds \
                                    are observed unchanged for at least 750 ms before \
                                    reporting it. Default false.",
                }),
            );
            tool(
                "find",
                "Find elements",
                &format!(
                    "Find elements matching a selector. Each match reports its screen \
                     `bounds` and `center`, which are the coordinates the click, move, \
                     drag, scroll, and screenshot tools take, plus its `states` and \
                     the actions the application advertises. A selector that matches \
                     nothing comes back with the near-miss candidates that were in \
                     scope, so read those before guessing again. Each match also carries \
                     a `ref` that `action`, `text` and `table` accept in place of a \
                     selector.\n\n\
                     {CENTER_NOTE}\n\n\
                     Without `stable` this reads the tree once and does not wait. Set \
                     `stable: true` before clicking or dragging something that may \
                     still be moving (sliding in, resizing, settling after a layout \
                     change): the call then waits up to the default timeout, \
                     currently {timeout} (set {timeout_env}, in seconds, where the \
                     server is launched to change it), until the match is visible and \
                     its bounds are observed unchanged for at least 750 ms, and \
                     reports those observed coordinates. Changes, missing matches, hidden \
                     elements, or absent bounds restart the 750 ms window. Native \
                     snapshots can lag motion. With `stable` the selector \
                     must match exactly one element: one that matches several is \
                     refused with an `ambiguous_selector` failure listing what it \
                     matched. A match that never settles returns a `timeout` failure \
                     naming the last two bounds it saw; one that never appears \
                     returns the usual not-found failure once the wait is over.\n\n\
                     {field_cap}\n\n\
                     {ACTIONS_FIELD_NOTE}",
                    field_cap = field_cap_note(),
                    timeout = default_timeout_label(),
                    timeout_env = crate::DEFAULT_TIMEOUT_ENV_VAR,
                ),
                object_schema(props, &["selector"]),
            )
        }
        "action" => {
            let mut props = app_target_properties();
            props.insert(
                "action".into(),
                json!({
                    "type": "string",
                    "enum": ACTION_NAMES,
                    "description": format!(
                        "Accessibility action to perform. These require `value`: {}. \
                         `{range_verb}` requires a range: `start` and `end`, or the \
                         same range as `value` `START,END` (not both). \
                         `{at_verb}` requires `at` (X,Y) and `{size_verb}` requires \
                         `size` (W,H): the platform needs coordinates and cannot be \
                         told a feature off the action name alone.",
                        ACTIONS_REQUIRING_VALUE.join(", "),
                        range_verb = ACTIONS_REQUIRING_RANGE[0],
                        at_verb = ACTIONS_REQUIRING_AT[0],
                        size_verb = ACTIONS_REQUIRING_SIZE[0],
                    ),
                }),
            );
            props.insert(
                "selector".into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "Selector for the element to act on. Must match exactly one \
                         element: a selector matching several is refused with \
                         `ambiguous_selector` and the list of what it matched, rather \
                         than acted on. Narrow it with an attribute filter or pick \
                         one with `:nth(n)`. Examples: `button[name=\"Save As\"]`, \
                         `text_field[name=\"Search\"]`, `check_box[checked=\"off\"]`, \
                         `radio_button:nth(2)`, `window[name=\"Settings\"]`.\n\n{SELECTOR_SYNTAX}"
                    ),
                }),
            );
            props.insert(
                "value".into(),
                json!({
                    "type": "string",
                    "description": "Argument for actions that need one: the text for `set-value` \
                                    and `type-text`, or a number for `set-numeric-value` (e.g. \
                                    \"88\", within the element's `min_value`..`max_value`). \
                                    `select-text` also accepts its range here as `START,END` \
                                    in UTF-16 code units, in place of `start` / `end` and \
                                    never together with them. Refused on any other verb.",
                }),
            );
            for (key, which) in [("start", "first"), ("end", "one past the last")] {
                props.insert(
                    key.into(),
                    TEXT_OFFSET.property(format!(
                        "`select-text` only: the {which} UTF-16 code unit of the range to \
                         select (0-based, half-open; an emoji outside the Basic \
                         Multilingual Plane is two units). Give `start` and `end` \
                         together. The `text` tool reports ranges in the same units and \
                         spelling, so copy its `start` / `end` here. Refused on any \
                         other verb."
                    )),
                );
            }
            props.insert(
                "at".into(),
                json!({
                    "type": "string",
                    "description": "`X,Y` desktop coordinates (physical pixels on Windows, logical points elsewhere) for `move-to`. \
                                    Required for `move-to`; refused on any other verb.",
                }),
            );
            props.insert(
                "size".into(),
                json!({
                    "type": "string",
                    "description": "`W,H` desktop dimensions (physical pixels on Windows, logical points elsewhere) for `resize-to`, both positive. \
                                    Required for `resize-to`; refused on any other verb.",
                }),
            );
            props.insert("ref".into(), ref_property());
            tool(
                "action",
                "Perform accessibility action",
                &format!(
                    "Perform an accessibility action on an element. Prefer this over the \
                     mouse and keyboard tools: it calls the application's own action, so \
                     it does not depend on window position, focus, or anything being \
                     visible on screen.\n\n\
                     The selector must match exactly one element. One that matches \
                     several is refused with an `ambiguous_selector` failure listing \
                     what it matched, rather than applied to the first of them. Pass a \
                     `ref` from `tree` or `find` instead to act on exactly the element \
                     you read: it acts at once (no wait) or fails `element_stale`.\n\n\
                     A verb takes only its own arguments: `value` goes with the value \
                     verbs and `select-text`, `start` / `end` with `select-text`, `at` \
                     with `move-to`, `size` with `resize-to`. Any of them on another \
                     verb is refused with `invalid_arguments` naming the verbs that take \
                     it, before anything is looked up, rather than ignored.\n\n\
                     Auto-waits for the selector to match an element that is visible and \
                     enabled, re-resolving as it polls, and only then acts. The window verbs \
                     (`activate`, `minimize`, `maximize`, `enter-fullscreen`, `restore`, \
                     `close`, `move-to`, `resize-to`) and `scroll-into-view` are the one \
                     exception: they wait \
                     only for `enabled`, because a minimized window is legitimately not \
                     visible and must still be reachable by the very verbs that restore or \
                     activate it. `restore` is the inverse of `minimize`/`maximize`/ \
                     `enter-fullscreen`: it leaves minimized, maximized, or fullscreen. \
                     When the element it finds is enabled and reported as scrolled out of \
                     view, the wait may ask the application to scroll it into view (its own \
                     scroll call, not a mouse gesture) and check again. It asks only then: \
                     not for a disabled element, nor on a platform that never reports \
                     `scrolled_out`. The wait runs \
                     up to the default timeout, currently {timeout}. Set {timeout_env} \
                     (in seconds) in the environment the server is launched with to \
                     change it; no tool argument does. A call that is going to fail \
                     therefore takes that long and returns a `timeout` failure \
                     naming what it was waiting for and what it last saw. When it \
                     matched an element that was not visible, the diagnosis also has \
                     `hidden_reason` (`scrolled_out`, or `window_minimized` when its window \
                     has to be restored), and `auto_scroll` when the wait tried to scroll: \
                     `refused` (asked once; the element has no scroll call), `no_effect` \
                     (accepted, yet the next check still found it not visible) or `revealed` \
                     (the next check found it visible and something else, such as being \
                     disabled, kept the wait going). No `auto_scroll` means no scroll was \
                     tried. The timeout includes the reads the wait makes: when the \
                     application stops answering, the call still ends on time and the \
                     diagnosis has `pending`, naming the platform call it hung in. Do not wrap \
                     this tool in a retry loop; the wait is the retry loop.\n\n\
                     Every failure of every tool carries `delivery`, which says whether a \
                     retry is safe. `not_delivered` means the action never reached the application (no \
                     match, not supported, element gone, the wait expired), so retrying \
                     cannot apply it twice. `unknown` means it was dispatched and the \
                     outcome is unconfirmed (the application did not reply, the platform \
                     call errored): re-read with `tree` or `find` before acting again, \
                     or a second \"Send\" can be submitted twice.\n\n\
                     `ok: true` means the application accepted the call, not that \
                     anything changed: a control is free to accept an action and do \
                     nothing. Confirm the effect by re-reading with `tree` or `find`.\n\n\
                     {ACTIONS_FIELD_NOTE}",
                    timeout = default_timeout_label(),
                    timeout_env = crate::DEFAULT_TIMEOUT_ENV_VAR,
                ),
                object_schema(props, &["action"]),
            )
        }
        "click" => {
            let mut props = point_properties("Screen coordinate to click.");
            props.insert(
                "button".into(),
                json!({
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button. Default \"left\".",
                }),
            );
            props.insert(
                "count".into(),
                CLICK_COUNT.property("Consecutive clicks. 2 is a double-click. Default 1.".into()),
            );
            props.insert("held".into(), held_property());
            tool(
                "click",
                "Click",
                "Click at a screen coordinate. Coordinates only: get them from \
                 `find`, which reports each match's `center`. On Linux with the \
                 uinput input backend (Wayland sessions) element bounds are not \
                 screen coordinates, so `find` reports no `center` there; press \
                 the element with `action` instead.",
                object_schema(props, &["x", "y"]),
            )
        }
        "move" => tool(
            "move",
            "Move pointer",
            "Move the mouse pointer to a screen coordinate without pressing anything.",
            object_schema(
                point_properties("Screen coordinate to move to."),
                &["x", "y"],
            ),
        ),
        "drag" => {
            let mut props = Map::new();
            for (key, what) in [
                ("from_x", "Starting X"),
                ("from_y", "Starting Y"),
                ("to_x", "Ending X"),
                ("to_y", "Ending Y"),
            ] {
                props.insert(
                    key.into(),
                    COORD.property(format!("{what}, in screen coordinates.")),
                );
            }
            props.insert(
                "button".into(),
                json!({
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button held during the drag. Default \"left\".",
                }),
            );
            props.insert(
                "duration_ms".into(),
                DRAG_DURATION_MS.property(
                    "Time spent moving the pointer from start to end, in milliseconds. \
                     Default 150. On macOS the drag also holds 200 ms after press and 500 ms \
                     before release. Slower drags \
                     are more reliable in applications that animate."
                        .into(),
                ),
            );
            props.insert("held".into(), held_property());
            tool(
                "drag",
                "Drag",
                "Press at one screen coordinate, move to another, and release.",
                object_schema(props, &["from_x", "from_y", "to_x", "to_y"]),
            )
        }
        "scroll" => {
            let mut props = point_properties("Screen coordinate to scroll over.");
            props.insert(
                "dx".into(),
                COORD
                    .property("Horizontal scroll delta. Positive scrolls right. Default 0.".into()),
            );
            props.insert(
                "dy".into(),
                COORD.property("Vertical scroll delta, in wheel ticks. Positive scrolls down, toward the end of the content. Default 0.".into()),
            );
            tool(
                "scroll",
                "Scroll",
                "Scroll the wheel over a screen coordinate.",
                object_schema(props, &["x", "y"]),
            )
        }
        "key" => {
            let mut props = Map::new();
            props.insert(
                "key".into(),
                json!({
                    "type": "string",
                    "description": "Key to press: a single character (\"a\", \"7\"), a named key \
                                    (\"Enter\", \"Tab\", \"Escape\", \"ArrowUp\", \"Home\"), a \
                                    function key (\"F1\"..\"F24\"), or a modifier (\"Shift\", \
                                    \"Ctrl\", \"Alt\", \"Meta\").",
                }),
            );
            props.insert("held".into(), held_property());
            tool(
                "key",
                "Press key",
                "Press a key, optionally as a chord with modifiers held. Goes to \
                 whatever currently has keyboard focus.",
                object_schema(props, &["key"]),
            )
        }
        "type" => {
            let mut props = Map::new();
            props.insert(
                "text".into(),
                json!({ "type": "string", "description": "Text to type." }),
            );
            tool(
                "type",
                "Type text",
                "Type text into whatever currently has keyboard focus. To put text \
                 into a specific field, prefer `action` with `set-value` or \
                 `type-text`, which targets the element directly.",
                object_schema(props, &["text"]),
            )
        }
        "screenshot" => {
            // The target properties are the shared ones rather than a second
            // set: `annotate` resolves selectors, and a selector on this tool
            // has to name a target on exactly the terms it does everywhere
            // else. They stay optional here — an unannotated capture reads no
            // accessibility tree and needs no target at all.
            let mut props = app_target_properties();
            for (key, axis, bounds) in [
                ("x", "Left edge", COORD),
                ("y", "Top edge", COORD),
                ("width", "Width", SCREENSHOT_EXTENT),
                ("height", "Height", SCREENSHOT_EXTENT),
            ] {
                props.insert(
                    key.into(),
                    bounds.property(format!(
                        "{axis} of the region to capture, in screen coordinates. \
                         Give all four to capture a region, or none to capture the \
                         whole screen."
                    )),
                );
            }
            props.insert(
                "annotate".into(),
                json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Selectors to draw onto the capture, one annotation \
                                    group per entry and in order: the first selector is \
                                    group `A`, the second `B`. Every element a selector \
                                    matches gets an outlined box and a tag, and one \
                                    `legend` entry naming it. Needs a target — `app`, \
                                    `pid` or `shell` — for the selectors to resolve \
                                    against. Omit it for a plain capture, which reads \
                                    no accessibility tree.",
                }),
            );
            tool(
                "screenshot",
                "Capture screenshot",
                &screenshot_description(),
                object_schema(props, &[]),
            )
        }
        "text" => {
            let mut props = app_target_properties();
            props.insert(
                "op".into(),
                json!({
                    "type": "string",
                    "enum": cli::TEXT_OPS,
                    "description": "What to read. `info`: `length`, `caret` and the \
                                    selected ranges. `read`: the text of the range. \
                                    `find`: the `occurrence`-th match of `find`. \
                                    `range-at`: the `unit` containing `offset`. \
                                    `object-at`: the embedded control or link at \
                                    `offset`, as `object` in the shape `find` returns, with a `ref`, or null when \
                                    there is none. `lines`: \
                                    the rendered lines of the range. `bounds`: screen \
                                    rectangles of the range, one per rendered line. \
                                    `runs`: formatting runs of the range.",
                }),
            );
            props.insert(
                "selector".into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "Selector for the element whose text to read, usually a \
                         `text_field`, `text_area` or `document`. Must match exactly \
                         one element: several are refused with `ambiguous_selector` \
                         and the candidates.\n\n{SELECTOR_SYNTAX}"
                    ),
                }),
            );
            for (key, what) in [
                ("start", "Start of the range, inclusive"),
                ("end", "End of the range, exclusive"),
            ] {
                props.insert(
                    key.into(),
                    TEXT_OFFSET.property(format!(
                        "{what}, in UTF-16 code units. Give `start` and `end` together. \
                         Without `find` they are the range `read`, `lines`, `bounds` \
                         and `runs` cover (default: the whole text); with `find` they \
                         limit where it searches."
                    )),
                );
            }
            props.insert(
                "find".into(),
                json!({
                    "type": "string",
                    "description": "Text to look for, matched exactly: case-sensitive, \
                                    no normalization. Required for `find`. For `read`, \
                                    `lines`, `bounds` and `runs` the match becomes the \
                                    range, so `{\"op\": \"bounds\", \"find\": \"Total\"}` \
                                    gets the rectangle of that word without computing \
                                    offsets.",
                }),
            );
            props.insert(
                "occurrence".into(),
                TEXT_OCCURRENCE.property(
                    "Which match of `find`, 0-based: 0 is the first. Default 0. Matches \
                     do not overlap."
                        .into(),
                ),
            );
            props.insert(
                "offset".into(),
                TEXT_OFFSET.property(
                    "For `range-at` and `object-at`: the offset, in UTF-16 code units. \
                     For `range-at`, at the end of the text it names the unit that ends \
                     there."
                        .into(),
                ),
            );
            props.insert(
                "unit".into(),
                json!({
                    "type": "string",
                    "enum": ["character", "word", "line", "paragraph"],
                    "description": "For `range-at`. `line` is a rendered line, so a \
                                    wrapped paragraph has several; `paragraph` is \
                                    newline-delimited; `character` is one Unicode \
                                    scalar (two units for a surrogate pair).",
                }),
            );
            props.insert(
                "limit".into(),
                TEXT_LIMIT.property(format!(
                    "Page size. For `read`, UTF-16 code units of text (default \
                     {read_default}, at most {read_max}). For `lines` and `runs`, \
                     entries (default {page}, at most {page_max}). Refused by the \
                     other operations.",
                    read_default = cli::TEXT_READ_DEFAULT_UNITS,
                    read_max = cli::TEXT_READ_MAX_UNITS,
                    page = cli::TEXT_PAGE_DEFAULT,
                    page_max = cli::TEXT_PAGE_MAX,
                )),
            );
            props.insert("ref".into(), ref_property());
            tool(
                "text",
                "Read text by range",
                &format!(
                    "Read an element's text by range: caret and selection, a range of \
                     text, an exact search, the word / line / paragraph at an offset, \
                     the embedded control or link at an offset, rendered lines, their \
                     screen rectangles, and formatting runs. Use it \
                     instead of slicing `value`, which `tree` and `find` cut to \
                     {MAX_FIELD_BYTES} bytes.\n\n\
                     Every offset is a UTF-16 code unit, on input and output alike: an \
                     emoji outside the Basic Multilingual Plane counts two. Every range \
                     in a result carries `start`, `end` and the `text` it covers, so copy \
                     those numbers (into `select-text`, or into another `text` call) \
                     rather than computing them. A range inside a list (a selection, \
                     line, run or match) carries at most {range_cap} units of `text`; a \
                     longer one is followed by `text_truncated: true` and `text_end`, \
                     where its text stops, while `start` and `end` stay exact.\n\n\
                     Results are paged, and each page says whether it was cut. `read` \
                     returns at most `limit` units with `truncated`, and `next_start` to \
                     pass as `start` for the next page (`requested_end` is where the \
                     range ended). `lines` and `runs` do the same per entry. `bounds` \
                     refuses a range spanning more than {bounds_cap} rendered lines \
                     with `invalid_text_range`; page with `lines` and ask per page. \
                     `info` lists at most {sel_cap} selections, with `selection_count` \
                     and `selections_truncated`. `caret` is null when the element has \
                     no caret (not focused, or not editable), and `caret_unsupported` \
                     names the reason when the platform cannot report one at all.\n\n\
                     `find` answers `found: false` and `match: null` when there is no \
                     match. Where `find` names the range of another operation, a miss \
                     fails with `no_match` saying where it looked and the text length. \
                     A reversed range fails with `invalid_arguments` before any read. \
                     A range past the end, or one that splits a surrogate pair, \
                     fails with `invalid_text_range`, whose `text_range` gives `start`, \
                     `end` and the text's current `length`. Ranges do not follow edits: \
                     re-read after the text changes.\n\n\
                     Waits up to the default timeout, currently {timeout}, for the \
                     selector to match an element; the element does not have to be \
                     visible or enabled. An element with no text interface fails with \
                     `action_not_supported`.",
                    range_cap = cli::RANGE_TEXT_MAX_UNITS,
                    bounds_cap = crate::text_api::MAX_BOUNDS_LINES,
                    sel_cap = cli::MAX_SELECTIONS,
                    timeout = default_timeout_label(),
                ),
                object_schema(props, &["op"]),
            )
        }
        "relations" => {
            let mut props = app_target_properties();
            props.insert(
                "selector".into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "Selector for the element to ask. Must match exactly one \
                         element: several are refused with `ambiguous_selector` and \
                         the candidates.\n\n{SELECTOR_SYNTAX}"
                    ),
                }),
            );
            props.insert(
                "kinds".into(),
                json!({
                    "type": "array",
                    "items": { "type": "string", "enum": cli::relation_kind_names() },
                    "description": "Relation kinds to read. Omit for every kind this \
                                    platform reports (the `relation:*` rows of \
                                    `capabilities`). Naming a kind the platform never \
                                    reports fails with `unsupported`.",
                }),
            );
            props.insert(
                "limit".into(),
                FIND_LIMIT.property(format!(
                    "Most elements to return per kind. Default {FIND_DEFAULT_LIMIT}."
                )),
            );
            props.insert("ref".into(), ref_property());
            tool(
                "relations",
                "Element relations",
                "The elements one element is related to: who labels it \
                 (`labelled_by`) or describes it (`described_by`), what it controls \
                 (`controls`), where reading flows next (`flows_to`), its error \
                 message, and the reverse of each. Each kind comes back once, in \
                 request order, as `{kind, count, returned, truncated, elements}`; a \
                 kind with no targets has an empty list, never an absent entry. \
                 Elements are in the shape `find` returns, each with a `ref`.\n\n\
                 The kinds are reported as the platform reports them: macOS reports \
                 `aria-controls` and `aria-flowto` together as `linked`, and Windows \
                 reports `aria-details` as `details`. Reads once and does not wait.",
                object_schema(props, &[]),
            )
        }
        "table" => {
            let mut props = app_target_properties();
            props.insert(
                "op".into(),
                json!({
                    "type": "string",
                    "enum": cli::TABLE_OPS,
                    "description": "`cell-at` (on a table, needs `row` and `column`): the \
                                    cell covering that slot. `column-headers` / \
                                    `row-headers` (on a table): its header cells. \
                                    `column-header-cells` / `row-header-cells` (on a \
                                    cell): the headers that head it. `selected-items` (on \
                                    a list, tree, table or tab list): the selected rows, \
                                    cells or items.",
                }),
            );
            props.insert(
                "selector".into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "Selector for the table, cell or list to ask. Must match \
                         exactly one element: several are refused with \
                         `ambiguous_selector` and the candidates.\n\n{SELECTOR_SYNTAX}"
                    ),
                }),
            );
            props.insert(
                "row".into(),
                TABLE_INDEX.property("For `cell-at`: 0-based row index.".into()),
            );
            props.insert(
                "column".into(),
                TABLE_INDEX.property("For `cell-at`: 0-based column index.".into()),
            );
            props.insert(
                "limit".into(),
                FIND_LIMIT.property(format!(
                    "Most elements to return. Default {FIND_DEFAULT_LIMIT}."
                )),
            );
            props.insert("ref".into(), ref_property());
            tool(
                "table",
                "Table lookups",
                &format!(
                    "Ask a table about its structure: the cell at a row and column, the \
                     header cells of the table or of one cell, and what a list, tree or \
                     table has selected. Elements come back in the shape `find` returns, \
                     each with a `ref`, including `cell` (`row`, `column`, `row_span`, `column_span`, \
                     `header`), `table_row`, `table` (`row_count`, `column_count`) and \
                     `collection` (`index`, `size`, `depth`). Every index is 0-based. \
                     A count reads \"unknown\" when the platform says it does not know \
                     it, as virtualized content does.\n\n\
                     A plain cell lookup is also a selector, and `find` with \
                     `table_cell[row=\"2\"][column=\"0\"]` needs no table call. Use \
                     `cell-at` when a merged cell may cover the slot: it returns the \
                     merged cell, which a selector on `column` misses because the cell \
                     reports only its top-left slot. The header relationships have no \
                     selector form at all. `cell-at` answers `found: false` with the \
                     `reason` and the table's reported size when no cell covers the \
                     slot or it is not realized; content is never scrolled into \
                     existence to answer.\n\n\
                     Reads once and does not wait, like `find` without `stable`. Lists \
                     are capped at `limit`, with `count` and `truncated` saying whether \
                     anything was dropped. An element that is not a table (for the \
                     `*-header-cells` operations, not a cell) fails with \
                     `action_not_supported` rather than answering an empty list, and a \
                     platform that cannot answer at all fails with `unsupported`.\n\n\
                     {field_cap}",
                    field_cap = field_cap_note(),
                ),
                object_schema(props, &["op"]),
            )
        }
        "events_start" => tool(
            "events_start",
            "Start watching events",
            &events_start_description(),
            object_schema(events_start_properties(), &[]),
        ),
        "events_poll" => {
            let mut props = subscription_id_property();
            props.insert(
                "max".into(),
                EVENTS_MAX.property(format!(
                    "Most events to return in one call. Default {EVENTS_DEFAULT_MAX}. \
                     `buffered` in the result says how many are still waiting and \
                     `truncated` says whether any were."
                )),
            );
            props.insert(
                "timeout_ms".into(),
                EVENTS_TIMEOUT_MS.property(
                    "How long to wait for the first event when the buffer is empty. \
                     Default 0, which drains whatever has arrived and returns straight \
                     away — poll again rather than holding a call open unless you are \
                     waiting for a specific event. A blocking poll returns as soon as \
                     one event lands, not after the whole timeout, and it blocks every \
                     other tool call for as long as it waits."
                        .into(),
                ),
            );
            tool(
                "events_poll",
                "Poll buffered events",
                &events_poll_description(),
                object_schema(props, &["subscription_id"]),
            )
        }
        "events_stop" => tool(
            "events_stop",
            "Stop watching events",
            "Close a subscription and release the underlying platform subscription. \
             Reports how many events it delivered, how many it dropped, and how many \
             were still buffered when it closed. Stopping a subscription you are done \
             with is worth doing rather than letting it expire: until it does, the \
             application keeps delivering events into a buffer nobody reads.",
            object_schema(subscription_id_property(), &["subscription_id"]),
        ),
        // `TOOL_NAMES` is the single source of truth and every entry has an
        // arm above; an unnamed tool would be a programming error, so it
        // surfaces as one rather than shipping an empty definition.
        other => json!({
            "name": other,
            "description": "internal error: tool has no definition",
            "inputSchema": { "type": "object" },
        }),
    }
}

/// `events_start`'s arguments: an application, and an optional kind filter.
///
/// Written out rather than taken from [`app_target_properties`] because
/// `shell` is not among them — accessibility events are subscribed per
/// application, exactly as `xa11y events` refuses `--shell` — and an argument
/// a schema advertises but a handler refuses is worse than one that is absent.
fn events_start_properties() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert("backend".into(), json!({"type":"string", "enum": cli::BACKENDS, "description":"Explicit accessibility source; IA2 event subscriptions are unsupported."}));
    props.insert(
        "app".into(),
        json!({
            "type": "string",
            "description": "Application name, matched exactly and case-sensitively. \
                            Take the spelling from the `apps` tool rather than \
                            guessing it. Give this or `pid`.",
        }),
    );
    props.insert(
        "pid".into(),
        PID.property("Process id of the application to watch. Give this or `app`.".into()),
    );
    props.insert(
        "kinds".into(),
        json!({
            "type": "array",
            "items": { "type": "string", "enum": cli::event_kind_names() },
            "description": "Kinds to buffer, e.g. [\"focus_changed\", \"value_changed\"]. \
                            Omit for every kind. Filtering happens before buffering, so \
                            on a chatty application it is what keeps the events you \
                            care about from being evicted by ones you do not. A kind \
                            this platform never sends fails with `unsupported` (see the \
                            `event:*` rows of `capabilities`) instead of never arriving.",
        }),
    );
    props
}

/// The `subscription_id` argument, shared by `events_poll` and `events_stop`.
fn subscription_id_property() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "subscription_id".into(),
        json!({
            "type": "string",
            "description": "Handle returned by `events_start`.",
        }),
    );
    props
}

/// The `events_start` tool's description.
///
/// Built rather than written as a literal because it states the retention
/// window and the buffer size, both of which are constants in
/// [`super::events`] — a description naming a different number than the
/// registry enforces is the drift this file exists to avoid. The
/// specification asks a stateful tool to state its handle's retention here,
/// which is the only place a model reads before calling.
fn events_start_description() -> String {
    format!(
        "Start watching an application's accessibility events — focus moves, value \
         and state changes, text edits and caret moves, windows opening, selections, \
         announcements. Returns a \
         `subscription_id` to pass to `events_poll` and `events_stop`.\n\n\
         Watching is per application: there is no `shell` argument, because \
         accessibility events are delivered by an application's own \
         subscription.\n\n\
         Events are buffered from the moment this returns, so start the \
         subscription *before* the action you want to observe, then act, then \
         poll. Up to {BUFFER_CAPACITY} events are held; past that the oldest are \
         evicted and every poll reports how many were lost, so a gap is always \
         visible rather than silent.\n\n\
         The handle lives in this server process and no longer: it is reclaimed \
         after {} minutes without a poll, and a reclaimed handle comes back from \
         `events_poll` as a `subscription_expired` failure. Call `events_stop` when \
         you are done — stopping a handle that was already reclaimed succeeds, so \
         cleanup is always safe to call.\n\n\
         At most {MAX_SUBSCRIPTIONS} subscriptions may be open at once; starting \
         another is refused with the open handles named. Watch one application at \
         a time and stop it before moving on rather than accumulating handles.",
        EXPIRY.as_secs() / 60,
    )
}

/// The `events_poll` tool's description.
fn events_poll_description() -> String {
    format!(
        "Take buffered events from a subscription, oldest first. Returns at most \
         `max` of them ({EVENTS_DEFAULT_MAX} by default), and by default does not \
         block: an empty `events` means nothing has happened yet, not that the \
         subscription is broken.\n\n\
         Each event carries a `kind`, a `sequence` (monotonic, so a gap is exactly \
         what was dropped), `at_ms` since the subscription started, `time_ms` (when it was received, \
         milliseconds since the Unix epoch), and the `target` \
         element in the element shape `find` uses (bounds, states and all) but \
         without a `ref`: it is a snapshot from when the event fired, so call \
         `find` for an element to act on. A \
         `state_changed` event also carries `state_flag` and `state_value`. A \
         `text_changed` event carries `text_change` when the platform reports the \
         edit (Linux only): `kind` (`inserted` / `deleted`), `range` in UTF-16 units \
         or null when the platform counts in another unit, and `text` or null, cut \
         to {range_cap} units with `text_truncated` and `text_length` \
         when longer. An `announcement` event carries `announcement`: `origin` \
         (`request` when the application asked for text to be spoken, \
         `live_region` or `alert` when one changed or appeared, which carry no \
         text: read the target), and when the platform reports them `text` (cut \
         the same way, with `text_truncated` and `text_length`), `politeness` \
         (`polite` / `assertive`), `platform_priority` as the platform spells it, \
         and `activity_id` (Windows). `text_selection_changed` means the caret or text selection \
         moved; it carries no offsets, so read them with the `text` tool.\n\n\
         Two fields say what the events alone cannot. `dropped` counts events lost \
         to buffer eviction since the previous poll — poll more often, narrow \
         `kinds`, or accept the gap. `live: false` means the platform dropped the \
         subscription: drain what is left, then stop polling, because nothing \
         further can arrive. It is not a liveness check on the application — no \
         platform reports an application exiting as a dropped subscription, so an \
         application that quits keeps returning `live: true` with no events. Use \
         `apps` to find out whether it is still running.\n\n\
         An event is a snapshot from when it fired. Read the tree again to see the \
         UI as it is now.\n\n\
         {field_cap}",
        field_cap = field_cap_note(),
        range_cap = cli::RANGE_TEXT_MAX_UNITS,
    )
}

/// The `screenshot` tool's description.
///
/// Built rather than written as a literal because the annotation cap is
/// [`crate::MAX_ANNOTATIONS`], and a description that names a different number
/// than the handler enforces is the drift this file exists to avoid.
///
/// Four things are stated here that a model would otherwise pay calls to
/// discover: that the boxes come from the accessibility tree (so an
/// application without one gets none), the tag format, that a tag's number is
/// the `:nth(n)` argument and `legend[i].selector` is usable as-is, and the
/// cap with the field that reports when it bit.
fn screenshot_description() -> String {
    format!(
        "Capture the screen, or a region of it, as a PNG. Region coordinates come \
         from `find`, which reports each match's `bounds`.\n\n\
         `annotate` draws the accessibility tree onto the capture: each selector is \
         one group, and every element it matches gets an outlined box with a short \
         tag. It needs a target — `app`, `pid` or `shell` — for its selectors to \
         resolve against. The target scopes what is boxed and never crops the image; \
         `x`/`y`/`width`/`height` are what crop it. The boxes come from the tree, so \
         an application that exposes no accessibility tree gets no annotations and \
         the capture comes back plain. A target passed without `annotate` is refused \
         rather than ignored, since nothing would read it.\n\n\
         A tag is a letter for the group (`A` is the first selector, `B` the second) \
         and a 1-based number within that group, so `B7` is the seventh match of the \
         second selector. That number is the `:nth(n)` argument, and every `legend` \
         entry carries the finished selector: `legend[i].selector` goes straight to \
         `action` or `find` with the same target.\n\n\
         With `annotate`, the result gains `legend` (one entry per drawn box: `tag`, \
         `group`, `index`, `selector`, `role`, `name`, `bounds`, `color`), `omitted` \
         (elements that matched a selector but could not be drawn, each with a \
         `reason` of `no_bounds`, `zero_area` or `outside_capture`), and `truncated`. \
         At most {cap} elements are described in total; `truncated` counts the \
         matches past that cap, which are neither drawn nor listed. Narrow the \
         selectors when it is not zero.",
        cap = crate::MAX_ANNOTATIONS,
    )
}

/// The auto-wait timeout, as the `action` description states it.
///
/// A misconfigured [`crate::DEFAULT_TIMEOUT_ENV_VAR`] is reported rather than
/// papered over with the built-in default (tenet 1): every action call in the
/// session is about to fail with that same message, and the tool list is the
/// first place a client could learn why.
fn default_timeout_label() -> String {
    match crate::default_timeout() {
        Ok(timeout) => format!("{timeout:?}"),
        Err(e) => format!("unresolvable ({e}) — every action call will fail until it is fixed"),
    }
}

fn point_properties(what: &str) -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "x".into(),
        COORD.property(format!("{what} X, in screen coordinates.")),
    );
    props.insert(
        "y".into(),
        COORD.property(format!("{what} Y, in screen coordinates.")),
    );
    props
}

// ── Argument helpers ────────────────────────────────────────────────────────
//
// These raise `CliError::Usage`, which the protocol layer turns into an
// `isError: true` result rather than a JSON-RPC error: a missing or malformed
// argument is exactly the kind of failure a model can fix and retry.

fn usage(msg: impl Into<String>) -> CliError {
    CliError::Usage(msg.into())
}

fn req_str<'a>(args: &'a Value, key: &str) -> CliResult<&'a str> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        Some(Value::String(_)) => Err(usage(format!("\"{key}\" must not be empty"))),
        Some(_) => Err(usage(format!("\"{key}\" must be a string"))),
        None => Err(usage(format!("missing required argument \"{key}\""))),
    }
}

fn opt_str<'a>(args: &'a Value, key: &str) -> CliResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(usage(format!("\"{key}\" must be a string"))),
    }
}

/// Read a boolean argument, rejecting any other JSON type rather than
/// guessing at truthiness.
fn opt_bool(args: &Value, key: &str) -> CliResult<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(usage(format!("\"{key}\" must be a boolean"))),
    }
}

/// Read an integer argument, rejecting non-integers and out-of-range values
/// explicitly rather than letting a lossy `as` cast invent a coordinate.
fn opt_int(args: &Value, key: &str) -> CliResult<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| usage(format!("\"{key}\" must be a whole number, got {n}"))),
        Some(other) => Err(usage(format!("\"{key}\" must be a number, got {other}"))),
    }
}

/// Read a required integer argument and range-check it against `bounds`.
fn req_bounded(args: &Value, key: &str, bounds: Bounds) -> CliResult<i64> {
    let v =
        opt_int(args, key)?.ok_or_else(|| usage(format!("missing required argument \"{key}\"")))?;
    bounds.check(key, v)
}

/// Read an optional integer argument, range-checked against `bounds`.
///
/// `default` is not checked: it is this module's own value, not the caller's,
/// and `every_bounded_default_is_inside_its_own_bounds` asserts it is legal.
fn opt_bounded(args: &Value, key: &str, default: i64, bounds: Bounds) -> CliResult<i64> {
    match opt_int(args, key)? {
        None => Ok(default),
        Some(v) => bounds.check(key, v),
    }
}

/// A coordinate, whose bounds are exactly `i32`'s.
fn req_i32(args: &Value, key: &str) -> CliResult<i32> {
    let v = req_bounded(args, key, COORD)?;
    // `COORD` is `i32::MIN..=i32::MAX`, so the conversion cannot fail.
    i32::try_from(v).map_err(|_| usage(format!("\"{key}\" is out of range: {v}")))
}

/// An optional coordinate-range integer (`scroll`'s deltas).
fn opt_i32(args: &Value, key: &str, default: i32) -> CliResult<i32> {
    let v = opt_bounded(args, key, i64::from(default), COORD)?;
    i32::try_from(v).map_err(|_| usage(format!("\"{key}\" is out of range: {v}")))
}

/// An optional count, range-checked and widened to `usize`.
fn opt_usize(args: &Value, key: &str, default: usize, bounds: Bounds) -> CliResult<usize> {
    let v = opt_bounded(args, key, default as i64, bounds)?;
    // Every `Bounds` used with this helper has a non-negative `min`.
    usize::try_from(v).map_err(|_| usage(format!("\"{key}\" must not be negative: {v}")))
}

/// Read the `held` modifier list, reusing the CLI's key-name parser so the
/// two surfaces accept exactly the same spellings.
fn held_keys(args: &Value) -> CliResult<Vec<crate::Key>> {
    match args.get("held") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let names = items
                .iter()
                .map(|v| match v.as_str() {
                    // An empty entry would survive the join and reach
                    // `parse_held` as "no modifiers at all", which is a
                    // silently different gesture from the one asked for.
                    Some("") => Err(usage("\"held\" entries must not be empty")),
                    Some(s) => Ok(s.to_string()),
                    None => Err(usage("\"held\" entries must be key-name strings")),
                })
                .collect::<CliResult<Vec<_>>>()?;
            parse_held(Some(&names.join(",")))
        }
        Some(_) => Err(usage("\"held\" must be an array of key names")),
    }
}

/// Read the `annotate` selector list, one entry per annotation group.
///
/// An empty entry is refused rather than resolved: the selector parser reads
/// `""` as a syntax error naming nothing, and a caller who sent one meant a
/// group they can still name.
fn annotate_selectors(args: &Value) -> CliResult<Vec<String>> {
    match args.get("annotate") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v.as_str() {
                Some("") => Err(usage("\"annotate\" entries must not be empty")),
                Some(s) => Ok(s.to_string()),
                None => Err(usage("\"annotate\" entries must be selector strings")),
            })
            .collect(),
        Some(_) => Err(usage(
            "\"annotate\" must be an array of selectors, one per annotation group",
        )),
    }
}

/// Whether any of the target-naming arguments was given.
///
/// [`target`] refuses a call with none of them, but its message answers "which
/// application?" and not "why does a screenshot need one at all". `screenshot`
/// asks this first so it can say that `annotate` is what needs the target.
///
/// `backend` is not one: it says how to attach to a target, not which.
fn has_target(args: &Value) -> bool {
    target_keys(args).iter().any(|key| *key != "backend")
}

/// The target arguments this call actually passed, in schema order: the three
/// that name a target, and `backend`, which picks how to attach to it.
///
/// `screenshot` names them back to the caller when they were passed without
/// `annotate`, because "a target does nothing here" is only actionable if the
/// answer says which of them it means. `backend` is listed because it is just
/// as inert there; leaving it out let a `backend` without `annotate` be
/// dropped while the call reported success.
fn target_keys(args: &Value) -> Vec<&'static str> {
    ["app", "pid", "shell", "backend"]
        .into_iter()
        .filter(|key| !matches!(args.get(*key), None | Some(Value::Null)))
        .collect()
}

/// Render argument names as `"a"`, `"a" and "b"`, or `"a", "b" and "c"`.
fn quoted_list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    match quoted.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Read the `backend` argument. An unknown name is `invalid_arguments`
/// naming `"backend"`, not an `invalid_action_data` failure naming the
/// server's `XA11Y_ACCESSIBILITY_SOURCE` variable, which the caller never set.
fn backend_arg(args: &Value) -> CliResult<Option<String>> {
    opt_str(args, "backend")?
        .map(|raw| cli::parse_backend(raw, "\"backend\""))
        .transpose()
}

/// Read the `pid` argument, range-checked and narrowed.
fn pid_arg(args: &Value) -> CliResult<Option<u32>> {
    match opt_int(args, "pid")? {
        None => Ok(None),
        Some(v) => {
            let v = PID.check("pid", v)?;
            // `PID` is `1..=u32::MAX`, so the conversion cannot fail.
            Ok(Some(u32::try_from(v).map_err(|_| {
                usage(format!("\"pid\" is not a valid process id: {v}"))
            })?))
        }
    }
}

/// Resolve the target — an application or a shell surface — from `app` /
/// `pid` / `shell`, through the CLI's own resolver so name matching, kind
/// parsing and the ambiguity refusal stay identical on both surfaces.
///
/// The two argument-naming errors are raised *here* rather than in
/// [`resolve_target`], whose messages name `--app` and `--shell`: those are
/// flags this surface does not have, and a model reading one has nothing to
/// reach for. Only the naming is answered here — matching a name to a running
/// application, and a kind to a surface on screen, stays in `cli`, so the two
/// surfaces cannot drift on what `app` or `shell` means.
fn target(args: &Value) -> CliResult<Target> {
    let opts = Opts {
        backend: backend_arg(args)?,
        app: opt_str(args, "app")?.map(str::to_string),
        pid: pid_arg(args)?,
        shell: opt_str(args, "shell")?.map(str::to_string),
        ..Default::default()
    };
    if opts.app.is_some() && opts.shell.is_some() {
        return Err(usage(
            "give \"app\" or \"shell\", not both: \"shell\" targets an OS shell surface \
             (the `shell` tool lists them) and \"app\" targets a running application. \
             Use \"pid\" with \"shell\" to pick between surfaces of one kind",
        ));
    }
    if opts.app.is_none() && opts.shell.is_none() && opts.pid.is_none() {
        return Err(usage(
            "specify \"app\" (application name, matched exactly) or \"pid\" \
             (process id); the `apps` tool lists both for every running application, \
             and \"shell\" targets an OS shell surface instead",
        ));
    }
    resolve_target(&opts)
}

/// Parse the target application arguments (`app` / `pid`) into an [`Opts`],
/// naming the surface's own error instead of the CLI's `--app` / `--pid`
/// flags — which this surface does not have, and a model reading one has
/// nothing to reach for. Resolution itself stays in the CLI's `resolve_app`
/// / `resolve_apps`, so the two surfaces cannot drift on what `app` means.
fn target_opts(args: &Value) -> CliResult<Opts> {
    let opts = Opts {
        backend: backend_arg(args)?,
        app: opt_str(args, "app")?.map(str::to_string),
        pid: pid_arg(args)?,
        ..Default::default()
    };
    if opts.app.is_none() && opts.pid.is_none() {
        return Err(usage(
            "specify \"app\" (application name, matched exactly) or \"pid\" \
             (process id); the `apps` tool lists both for every running application",
        ));
    }
    Ok(opts)
}

/// The target's identity, as every element-returning result reports it.
///
/// An application keeps the `application` / `pid` fields it has always
/// reported; a shell surface gets a `shell` object instead, because calling a
/// taskbar an "application" is the kind of small lie a model then repeats.
fn target_fields(target: &Target, out: &mut Map<String, Value>) {
    match target.shell() {
        None => {
            out.insert("application".into(), json!(target.name()));
        }
        Some(surface) => {
            out.insert(
                "shell".into(),
                json!({
                    "kind": surface.kind.to_snake_case(),
                    "name": surface.name,
                    "pid": surface.pid,
                }),
            );
        }
    }
    out.insert("pid".into(), json!(target.pid()));
}

// ── Handlers ────────────────────────────────────────────────────────────────

fn tool_apps() -> CliResult<ToolOutput> {
    let apps = App::list()?;
    let listed: Vec<Value> = apps
        .iter()
        .map(|app| {
            json!({
                "pid": app.pid,
                "name": app.name,
                "foreground": app.is_foreground(),
            })
        })
        .collect();
    Ok(ToolOutput::json(json!({
        "count": listed.len(),
        "applications": listed,
    })))
}

fn tool_capabilities(args: &Value) -> CliResult<ToolOutput> {
    let platform = opt_str(args, "platform")?;
    let os_version = opt_str(args, "os_version")?;
    let names: Vec<String> = match args.get("names") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| usage("\"names\" must be an array of strings".to_string()))
            })
            .collect::<CliResult<_>>()?,
        Some(_) => return Err(usage("\"names\" must be an array of strings".to_string())),
    };
    let backend = backend_arg(args)?;
    let caps = crate::cli::resolve_capabilities(platform, os_version, backend.as_deref())?;
    let selected = crate::cli::select_capabilities(&caps, &names)?;
    let listed: Vec<Value> = selected
        .iter()
        .map(|c| crate::cli::capability_json(c))
        .collect();
    Ok(ToolOutput::json(json!({
        "platform": caps.platform.to_snake_case(),
        "backend": caps.backend,
        "os_version": caps.os_version,
        "count": listed.len(),
        "capabilities": listed,
    })))
}

fn tool_shell() -> CliResult<ToolOutput> {
    let surfaces = ShellSurface::list()?;
    let listed: Vec<Value> = surfaces
        .iter()
        .map(|surface| {
            json!({
                "kind": surface.kind.to_snake_case(),
                "name": surface.name,
                "pid": surface.pid,
            })
        })
        .collect();
    Ok(ToolOutput::json(json!({
        "count": listed.len(),
        "surfaces": listed,
    })))
}

fn tool_tree(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let max_depth = opt_usize(args, "max_depth", TREE_DEFAULT_MAX_DEPTH, TREE_DEPTH)?;
    let max_nodes = opt_usize(args, "max_nodes", TREE_MAX_NODES, TREE_NODES)?;
    let detail = tree_detail(args)?;
    let target = target(args)?;
    let center = cli::pointer_center();
    let (node, cap) = tree_json(
        &target,
        max_depth,
        max_nodes,
        detail,
        center.is_usable(),
        refs,
    )?;

    let mut out = Map::new();
    target_fields(&target, &mut out);
    note_center(&mut out, &center);
    out.insert("max_depth".into(), json!(max_depth));
    let mut truncated = Map::new();
    truncated.insert("by_depth".into(), json!(cap.truncated.by_depth));
    truncated.insert("by_node_limit".into(), json!(cap.truncated.by_nodes));
    truncated.insert("node_limit".into(), json!(max_nodes));
    if detail.text {
        truncated.insert("by_text".into(), json!(cap.truncated.by_text));
        truncated.insert("text_limit_bytes".into(), json!(cap.options.max_text_bytes));
    }
    if detail.formatting {
        truncated.insert("by_formatting".into(), json!(cap.truncated.by_formatting));
        truncated.insert("run_limit".into(), json!(cap.options.max_runs));
    }
    out.insert("truncated".into(), Value::Object(truncated));
    out.insert("vanished".into(), json!(cap.vanished));
    out.insert("backend".into(), json!(cap.backend));
    out.insert("tree".into(), node);
    Ok(ToolOutput::json(Value::Object(out)))
}

/// Read `tree`'s `detail` array; the names are parsed by the CLI's
/// [`cli::parse_tree_detail`], so `--detail` and `detail` accept one list.
fn tree_detail(args: &Value) -> CliResult<TreeDetail> {
    let items = match args.get("detail") {
        None | Some(Value::Null) => return Ok(TreeDetail::default()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(usage("\"detail\" must be an array of strings")),
    };
    let names = items
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| usage("\"detail\" must be an array of strings"))
        })
        .collect::<CliResult<Vec<_>>>()?;
    cli::parse_tree_detail(names, "detail")
}

/// Capture `target` and encode it as nested JSON, every node with a ref.
///
/// A node whose children were left out by a limit carries
/// `subtree_truncated: true` and `children_count` (how many children it
/// has, of which `children` holds the first few or none). Without that a
/// pruned node is indistinguishable from a leaf, and the global `truncated`
/// flags cannot say *where* to look deeper. A child-enumeration failure is
/// recorded in the node (`children_error`) rather than failing the whole
/// tree: one inaccessible subtree should not cost the caller every other
/// window.
fn tree_json(
    target: &Target,
    max_depth: usize,
    max_nodes: usize,
    detail: TreeDetail,
    center: bool,
    refs: &RefTable,
) -> CliResult<(Value, crate::Capture)> {
    let options = detail.apply(
        crate::CaptureOptions::new()
            .max_depth(max_depth)
            .max_nodes(max_nodes),
    );
    let cap = target.capture(&options)?;

    let mut capture = refs.begin();
    let node_refs: Vec<String> = cap.nodes.iter().map(|n| capture.add(&n.element)).collect();
    // Built bottom-up: in pre-order every child comes after its parent.
    let mut built: Vec<Option<Value>> = vec![None; cap.nodes.len()];
    for node in cap.nodes.iter().rev() {
        let mut value = element_data_json(&node.element, center);
        let obj = value
            .as_object_mut()
            .expect("element_data_json always builds an object");
        obj.insert("ref".into(), json!(node_refs[node.index]));
        for err in &node.errors {
            let key = match err.step {
                crate::CaptureStep::Children | crate::CaptureStep::Cycle => "children_error",
                crate::CaptureStep::Text => "text_error",
                crate::CaptureStep::Formatting => "formatting_error",
                crate::CaptureStep::Relations => "relations_error",
                // Identity only feeds cycle detection and relation
                // resolution; its failure shows up there.
                _ => continue,
            };
            obj.insert(key.into(), json!(err.error.to_string()));
        }
        let has_children_error = node
            .errors
            .iter()
            .any(|e| e.step == crate::CaptureStep::Children);
        if node.children_truncated && !has_children_error {
            obj.insert("subtree_truncated".into(), json!(true));
            if let Some(count) = node.children_count {
                obj.insert("children_count".into(), json!(count));
            }
        }
        if let Some(text) = &node.text {
            let mut t = json!({
                "content": text.content,
                "length": text.length,
                "truncated": text.truncated,
            });
            if let Some(runs) = &text.runs {
                let runs = runs
                    .iter()
                    .map(|run| {
                        Ok(json!({
                            "start": run.range.start,
                            "end": run.range.end,
                            "attributes": cli::json_without_nulls("text attributes", &run.attributes)?,
                        }))
                    })
                    .collect::<CliResult<Vec<_>>>()?;
                t["runs"] = Value::Array(runs);
                t["runs_truncated"] = json!(text.runs_truncated);
            }
            obj.insert("text".into(), t);
        }
        let relations: Map<String, Value> = node
            .relations
            .iter()
            .filter(|(_, targets)| !targets.is_empty())
            .map(|(kind, targets)| {
                let listed: Vec<Value> = targets
                    .iter()
                    .map(|t| match t {
                        crate::RelationTarget::Node(i) => json!({ "ref": node_refs[*i] }),
                        crate::RelationTarget::Outside(el) => json!({
                            "ref": capture.add(el),
                            "role": el.role.to_snake_case(),
                            "name": el.name,
                            "outside": true,
                        }),
                    })
                    .collect();
                (kind.to_snake_case().to_string(), Value::Array(listed))
            })
            .collect();
        if !relations.is_empty() {
            obj.insert("relations".into(), Value::Object(relations));
        }
        let children: Vec<Value> = node
            .children
            .iter()
            .filter_map(|&c| built[c].take())
            .collect();
        if !children.is_empty() {
            obj.insert("children".into(), Value::Array(children));
        }
        built[node.index] = Some(value);
    }
    refs.keep(capture);
    let root = built[0].take().unwrap_or(Value::Null);
    Ok((root, cap))
}

fn tool_find(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let selector = req_str(args, "selector")?;
    let limit = opt_usize(args, "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT)?;
    let stable = opt_bool(args, "stable")?.unwrap_or(false);
    let target = target(args)?;

    if stable {
        let element = match cli::find_stable(&target, selector)? {
            Ok(element) => element,
            Err(matches) => return Err(ambiguous(selector, &matches)),
        };
        return Ok(find_output(
            selector,
            &target,
            &[element],
            limit,
            &cli::pointer_center(),
            refs,
        ));
    }

    let locator = target.locator(selector);
    let mut elements = locator.elements()?;
    if elements.is_empty() {
        // `elements()` reports a miss as an empty list, so the failure that
        // reaches the model would otherwise carry nothing but the selector it
        // already knows. `element()` is the terminal, diagnosed form of the
        // same query: its `SelectorNotMatched` names the near-miss candidates
        // and a bounded snapshot of the scope (tenet 6). The cost is paid on
        // this path only — a successful find never re-resolves.
        match locator.element() {
            Err(e) => return Err(CliError::Xa11y(e)),
            // The tree changed between the two queries. Reporting the element
            // that is there now beats reporting a miss that is no longer true
            // (tenet 1: no silent fallback either way — this is the honest
            // answer to "what matches now").
            Ok(el) => elements.push(el),
        }
    }

    Ok(find_output(
        selector,
        &target,
        &elements,
        limit,
        &cli::pointer_center(),
        refs,
    ))
}

/// The `find` result: at most `limit` of `elements`, with the counts that
/// say whether the list was cut.
///
/// `center` says whether each match carries a precomputed `center`; when it
/// does not, the result carries `center_unavailable` with the reason.
fn find_output(
    selector: &str,
    target: &Target,
    elements: &[Element],
    limit: usize,
    center: &cli::PointerCenter,
    refs: &RefTable,
) -> ToolOutput {
    let total = elements.len();
    let mut capture = refs.begin();
    let matches: Vec<Value> = elements
        .iter()
        .take(limit)
        .map(|el| {
            let mut encoded = element_data_json(el, center.is_usable());
            if let Some(obj) = encoded.as_object_mut() {
                obj.insert("ref".into(), json!(capture.add(el)));
            }
            encoded
        })
        .collect();
    refs.keep(capture);

    let mut out = Map::new();
    out.insert("selector".into(), json!(selector));
    target_fields(target, &mut out);
    note_center(&mut out, center);
    out.insert("match_count".into(), json!(total));
    out.insert("returned".into(), json!(matches.len()));
    out.insert("truncated".into(), json!(total > matches.len()));
    out.insert("matches".into(), Value::Array(matches));
    ToolOutput::json(Value::Object(out))
}

/// Which element a tool acts on: a `selector` resolved now in the given
/// target, or a `ref` from an earlier `tree` / `find` — exactly one.
enum Subject<'a> {
    Selector(&'a str),
    Ref(&'a str),
}

/// Read the `selector` / `ref` pair. Exactly one must be given; a `ref`
/// carries its own application, so it takes no `app` / `pid` / `shell`.
fn subject(args: &Value) -> CliResult<Subject<'_>> {
    match (opt_str(args, "selector")?, opt_str(args, "ref")?) {
        (Some(selector), None) => Ok(Subject::Selector(selector)),
        (None, Some(r)) => {
            if let Some(key) = ["app", "pid", "shell", "backend"]
                .into_iter()
                .find(|k| args.get(*k).is_some_and(|v| !v.is_null()))
            {
                return Err(usage(format!(
                    "\"{key}\" cannot be combined with \"ref\": a ref already names its element"
                )));
            }
            Ok(Subject::Ref(r))
        }
        (Some(_), Some(_)) => Err(usage(
            "give \"selector\" or \"ref\", not both: a ref names one element already",
        )),
        (None, None) => Err(usage("missing required argument \"selector\" (or \"ref\")")),
    }
}

/// The element a `ref` names, proven to still be the captured node before a
/// read: a read through a reused platform id would otherwise return another
/// node's data. Actions run the same check themselves.
fn resolve_ref_for_read(refs: &RefTable, reference: &str) -> CliResult<Element> {
    let element = refs.resolve(reference)?;
    element.check_alive()?;
    Ok(element)
}

/// How the `action` tool spells its arguments, for usage errors.
const MCP_ACTION_ARG_NAMES: cli::ActionArgNames = cli::ActionArgNames {
    value: "\"value\"",
    range: "\"start\"/\"end\"",
    range_form: "\"start\" and \"end\"",
    at: "\"at\"",
    size: "\"size\"",
};

fn tool_action(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let action = req_str(args, "action")?;
    let subject = subject(args)?;
    let value = opt_str(args, "value")?;
    let at = opt_str(args, "at")?;
    let size = opt_str(args, "size")?;
    // `select-text`'s range, spelled as the `text` tool spells it.
    let range = match (
        opt_u32(args, "start", TEXT_OFFSET)?,
        opt_u32(args, "end", TEXT_OFFSET)?,
    ) {
        (Some(start), Some(end)) => Some(crate::TextRange { start, end }),
        (None, None) => None,
        // Half a range would have to be completed by guessing.
        _ => return Err(usage("give \"start\" and \"end\" together, or neither")),
    };
    // Field parsing and the action-payload parse come before target
    // resolution (parse arguments before the first OS call): a malformed
    // `at` / `size` / `value` is a usage error, and it must win over an
    // app-resolution failure rather than be masked by one — a model that
    // typed a bad point must be told the point is bad, not that an app was
    // not found.
    let action_args =
        cli::parse_action_args(action, value, range, at, size, &MCP_ACTION_ARG_NAMES)?;
    match subject {
        Subject::Selector(selector) => {
            let target = target(args)?;
            tool_action_parsed(&target, action, selector, &action_args)
        }
        Subject::Ref(reference) => tool_action_on_ref(refs, action, reference, &action_args),
    }
}

/// [`tool_action`] for a `ref`: the action reaches exactly the element the
/// ref names, or fails `element_stale`. No auto-wait: the element is already
/// resolved, and waiting cannot make a removed node come back.
fn tool_action_on_ref(
    refs: &RefTable,
    action: &str,
    reference: &str,
    action_args: &cli::ActionArgs,
) -> CliResult<ToolOutput> {
    if !cli::ACTION_NAMES.contains(&action) {
        return Err(usage(format!(
            "unknown action: {action} (expected one of: {})",
            cli::ACTION_NAMES.join(", ")
        )));
    }
    let element = refs.resolve(reference)?;
    cli::perform_action_on(&element, action, action_args)?;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("action".into(), json!(action));
    out.insert("ref".into(), json!(reference));
    Ok(ToolOutput::json(Value::Object(out)))
}

/// The resolving-and-dispatching half of [`tool_action`] against an explicit
/// target — the unit-testable sibling, which is what lets the mock provider
/// exercise the geometry arguments off a real desktop.
fn tool_action_parsed(
    target: &Target,
    action: &str,
    selector: &str,
    action_args: &cli::ActionArgs,
) -> CliResult<ToolOutput> {
    // An unknown verb is refused before the auto-wait can spend its timeout
    // on a call that could never be made.
    if !cli::ACTION_NAMES.contains(&action) {
        return Err(usage(format!(
            "unknown action: {action} (expected one of: {})",
            cli::ACTION_NAMES.join(", ")
        )));
    }
    let locator = target.locator(selector);
    // The schema promises "must match exactly one element", so enforce it here
    // rather than letting the locator's document-order first-match stand in
    // for it: a model told "exactly one" that silently gets the first of
    // several has no way to notice it pressed the wrong control.
    //
    // The count and the action share one resolution: `auto_wait_unique`
    // hands back the single element it counted, and that element is what is
    // acted on. Counting and then calling the locator's action re-resolved in
    // between, so a UI changing in that gap got the action on an element that
    // was never counted — possibly one of several (#528).
    //
    // Zero matches and a not-yet-actionable match keep polling, which is
    // what gives an element still being built time to appear, and the
    // timeout carries the "never matched" diagnosis. Several matches refuse
    // at once rather than waiting for the UI to settle into one.
    let element = match locator.auto_wait_unique(&action.replace('-', "_"))? {
        Ok(element) => element,
        Err(matches) => return Err(ambiguous(selector, &matches)),
    };
    cli::perform_action_on(&element, action, action_args)?;

    // `ok` reports that the platform accepted the call. Whether the
    // application did anything with it is only knowable by re-reading, which
    // is what the tool's description tells the caller to do.
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("action".into(), json!(action));
    out.insert("selector".into(), json!(selector));
    target_fields(target, &mut out);
    Ok(ToolOutput::json(Value::Object(out)))
}

// ── Text and table ──────────────────────────────────────────────────────────

/// How the `text` tool spells its arguments, for `cli::parse_text_op`'s
/// usage errors.
const MCP_TEXT_ARG_NAMES: cli::TextArgNames = cli::TextArgNames {
    range: "\"start\"/\"end\"",
    find: "\"find\"",
    occurrence: "\"occurrence\"",
    offset: "\"offset\"",
    unit: "\"unit\"",
    limit: "\"limit\"",
};

/// An optional `u32` argument, range-checked against `bounds`.
fn opt_u32(args: &Value, key: &str, bounds: Bounds) -> CliResult<Option<u32>> {
    match opt_int(args, key)? {
        None => Ok(None),
        Some(v) => {
            let v = bounds.check(key, v)?;
            // Every `Bounds` used here lies within `0..=u32::MAX`.
            Ok(Some(u32::try_from(v).map_err(|_| {
                usage(format!("\"{key}\" is out of range: {v}"))
            })?))
        }
    }
}

/// Parse the `text` tool's arguments into an operation, before anything is
/// resolved.
fn text_op_from_args(args: &Value) -> CliResult<cli::TextOp> {
    let op = req_str(args, "op")?;
    let start = opt_u32(args, "start", TEXT_OFFSET)?;
    let end = opt_u32(args, "end", TEXT_OFFSET)?;
    let range = match (start, end) {
        (Some(start), Some(end)) => Some(crate::TextRange { start, end }),
        (None, None) => None,
        // Half a range would have to be completed by guessing (0? the
        // length?), and either guess reads text nobody asked for.
        _ => return Err(usage("give \"start\" and \"end\" together, or neither")),
    };
    let occurrence = opt_u32(args, "occurrence", TEXT_OCCURRENCE)?.map(|n| n as usize);
    let limit = match opt_int(args, "limit")? {
        None => None,
        Some(v) => Some(TEXT_LIMIT.check("limit", v)? as usize),
    };
    cli::parse_text_op(
        op,
        &cli::TextArgs {
            range,
            find: opt_str(args, "find")?,
            occurrence,
            offset: opt_u32(args, "offset", TEXT_OFFSET)?,
            unit: opt_str(args, "unit")?,
            limit,
        },
        &MCP_TEXT_ARG_NAMES,
    )
}

fn tool_text(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let subject = subject(args)?;
    let op = text_op_from_args(args)?;
    match subject {
        Subject::Selector(selector) => {
            let target = target(args)?;
            tool_text_parsed(&target, selector, &op, refs)
        }
        Subject::Ref(reference) => {
            let element = resolve_ref_for_read(refs, reference)?;
            let mut out = Map::new();
            out.insert("ref".into(), json!(reference));
            text_output(out, &element, &op, refs)
        }
    }
}

/// The resolving half of [`tool_text`], against an explicit target, so the
/// mock provider can drive it.
fn tool_text_parsed(
    target: &Target,
    selector: &str,
    op: &cli::TextOp,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let element = match cli::resolve_text_element(target, selector)? {
        Ok(element) => element,
        Err(matches) => return Err(ambiguous(selector, &matches)),
    };
    let mut out = Map::new();
    out.insert("selector".into(), json!(selector));
    target_fields(target, &mut out);
    text_output(out, &element, op, refs)
}

/// The rest of a `text` result after its header fields.
///
/// The element `object-at` finds is encoded like every other element this
/// server returns, with a `ref` so the model can act on the embedded control
/// or link it found. A capture is started only when there is an element to
/// hold, so the other operations never push an older result's refs out.
fn text_output(
    mut out: Map<String, Value>,
    element: &Element,
    op: &cli::TextOp,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let center = matches!(op, cli::TextOp::ObjectAt { .. }).then(cli::pointer_center);
    let mut capture = None;
    let usable = center.as_ref().is_some_and(cli::PointerCenter::is_usable);
    let result = cli::run_text_op(&element.text()?, op, &mut |found| {
        element_with_ref(&mut capture, refs, found, usable)
    })?;
    if let Some(capture) = capture {
        refs.keep(capture);
    }
    if let Some(center) = &center {
        note_center(&mut out, center);
    }
    out.insert("units".into(), json!("utf16"));
    out.insert("result".into(), result);
    Ok(ToolOutput::json(Value::Object(out)))
}

fn tool_table(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let op_name = req_str(args, "op")?;
    let subject = subject(args)?;
    let op = cli::parse_table_op(
        op_name,
        opt_u32(args, "row", TABLE_INDEX)?,
        opt_u32(args, "column", TABLE_INDEX)?,
        "\"row\"",
        "\"column\"",
    )?;
    let limit = opt_usize(args, "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT)?;
    match subject {
        Subject::Selector(selector) => {
            let target = target(args)?;
            tool_table_parsed(
                &target,
                selector,
                op_name,
                op,
                limit,
                &cli::pointer_center(),
                refs,
            )
        }
        Subject::Ref(reference) => {
            let element = resolve_ref_for_read(refs, reference)?;
            let found = cli::run_table_op(&element, op)?;
            let mut out = Map::new();
            out.insert("op".into(), json!(op_name));
            out.insert("ref".into(), json!(reference));
            Ok(table_output(
                out,
                &element,
                op,
                &found,
                limit,
                &cli::pointer_center(),
                refs,
            ))
        }
    }
}

fn tool_relations(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    let subject = subject(args)?;
    let kinds = match args.get("kinds") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let names = items
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| usage("\"kinds\" must be an array of strings"))
                })
                .collect::<CliResult<Vec<&str>>>()?;
            if names.is_empty() {
                return Err(usage(
                    "\"kinds\" is empty; omit it to read every kind this platform reports",
                ));
            }
            cli::parse_relation_kinds(&names)?
        }
        Some(_) => return Err(usage("\"kinds\" must be an array of strings")),
    };
    let limit = opt_usize(args, "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT)?;
    match subject {
        Subject::Selector(selector) => {
            let target = target(args)?;
            tool_relations_parsed(&target, selector, &kinds, limit, refs)
        }
        Subject::Ref(reference) => {
            let element = resolve_ref_for_read(refs, reference)?;
            let mut out = Map::new();
            out.insert("ref".into(), json!(reference));
            relations_output(out, &element, &kinds, limit, refs)
        }
    }
}

/// The resolving half of [`tool_relations`], against an explicit target.
fn tool_relations_parsed(
    target: &Target,
    selector: &str,
    kinds: &[crate::RelationKind],
    limit: usize,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let element = match cli::resolve_table_element(target, selector)? {
        Ok(element) => element,
        Err(matches) => return Err(ambiguous(selector, &matches)),
    };
    let mut out = Map::new();
    out.insert("selector".into(), json!(selector));
    target_fields(target, &mut out);
    relations_output(out, &element, kinds, limit, refs)
}

/// The `relations` result: each kind with at most `limit` targets, each
/// target carrying a ref.
fn relations_output(
    mut out: Map<String, Value>,
    element: &Element,
    kinds: &[crate::RelationKind],
    limit: usize,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let center = cli::pointer_center();
    note_center(&mut out, &center);
    let center = center.is_usable();
    let found = element.relations(kinds)?;
    let mut capture = refs.begin();
    let relations: Vec<Value> = found
        .iter()
        .map(|(kind, targets)| {
            let listed: Vec<Value> = targets
                .iter()
                .take(limit)
                .map(|el| {
                    let mut encoded = element_data_json(el, center);
                    if let Some(obj) = encoded.as_object_mut() {
                        obj.insert("ref".into(), json!(capture.add(el)));
                    }
                    encoded
                })
                .collect();
            json!({
                "kind": kind.to_snake_case(),
                "count": targets.len(),
                "returned": listed.len(),
                "truncated": targets.len() > listed.len(),
                "elements": listed,
            })
        })
        .collect();
    refs.keep(capture);
    out.insert("element".into(), element_data_json(element, center));
    out.insert("relations".into(), Value::Array(relations));
    Ok(ToolOutput::json(Value::Object(out)))
}

/// The resolving half of [`tool_table`], against an explicit target.
fn tool_table_parsed(
    target: &Target,
    selector: &str,
    op_name: &str,
    op: cli::TableOp,
    limit: usize,
    center: &cli::PointerCenter,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let element = match cli::resolve_table_element(target, selector)? {
        Ok(element) => element,
        Err(matches) => return Err(ambiguous(selector, &matches)),
    };
    let found = cli::run_table_op(&element, op)?;
    let mut out = Map::new();
    out.insert("op".into(), json!(op_name));
    out.insert("selector".into(), json!(selector));
    target_fields(target, &mut out);
    Ok(table_output(out, &element, op, &found, limit, center, refs))
}

/// Encode `element` like [`element_data_json`] plus a `ref` held in
/// `capture`, which is started on the first element. A result that returns no
/// element so starts no capture, and cannot push an older result's refs out
/// of the table.
fn element_with_ref(
    capture: &mut Option<Capture>,
    refs: &RefTable,
    element: &Element,
    center: bool,
) -> Value {
    let mut encoded = element_data_json(element, center);
    let reference = capture.get_or_insert_with(|| refs.begin()).add(element);
    if let Some(obj) = encoded.as_object_mut() {
        obj.insert("ref".into(), json!(reference));
    }
    encoded
}

/// The rest of a `table` result after its header fields: the cell for
/// `cell-at`, otherwise the bounded element list. Each element carries a
/// `ref`, as `find`'s matches do.
fn table_output(
    mut out: Map<String, Value>,
    element: &Element,
    op: cli::TableOp,
    found: &[Element],
    limit: usize,
    center: &cli::PointerCenter,
    refs: &RefTable,
) -> ToolOutput {
    note_center(&mut out, center);
    let mut capture = None;
    if let cli::TableOp::CellAt { row, column } = op {
        out.insert("row".into(), json!(row));
        out.insert("column".into(), json!(column));
        out.insert("found".into(), json!(!found.is_empty()));
        match found.first() {
            Some(cell) => {
                let encoded = element_with_ref(&mut capture, refs, cell, center.is_usable());
                out.insert("cell".into(), encoded);
                if let Some(capture) = capture {
                    refs.keep(capture);
                }
            }
            None => {
                out.insert("cell".into(), Value::Null);
                out.insert(
                    "reason".into(),
                    json!(cli::no_cell_message(element, row, column)),
                );
            }
        }
        return ToolOutput::json(Value::Object(out));
    }
    let listed: Vec<Value> = found
        .iter()
        .take(limit)
        .map(|el| element_with_ref(&mut capture, refs, el, center.is_usable()))
        .collect();
    if let Some(capture) = capture {
        refs.keep(capture);
    }
    out.insert("count".into(), json!(found.len()));
    out.insert("returned".into(), json!(listed.len()));
    out.insert("truncated".into(), json!(found.len() > listed.len()));
    out.insert("elements".into(), Value::Array(listed));
    ToolOutput::json(Value::Object(out))
}

/// `windows` tool — enumerate top-level windows (winlenium's list-windows).
///
/// Without `app`/`pid` this lists the windows of every running application;
/// with either it lists those of that one application. Each application is
/// one process node whose `windows()` are its top-level windows. Bounded by
/// `limit` with an explicit `truncated` flag: a silently shortened result
/// reads as a complete one, which is worse than no result.
fn tool_windows(args: &Value, refs: &RefTable) -> CliResult<ToolOutput> {
    // `windows` enumerates application windows; a shell target is not one.
    // The schema does not advertise `shell`, but a client may send it anyway
    // (MCP input schemas are not always enforced) — refuse it rather than
    // silently falling into the unfiltered all-apps listing (tenet 1).
    if args.get("shell").is_some() {
        return Err(usage(
            "`windows` lists application windows, not shell surfaces; call `shell` and \
             pass the result to `tree` / `find` / `action`",
        ));
    }
    let limit = opt_usize(args, "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT)?;
    let apps = if args.get("app").is_some() || args.get("pid").is_some() {
        // Both filters resolve to one App entry: `pid` goes through
        // `App::by_pid` directly (no foreground query), and the entry's
        // `windows()` is process-complete on Linux because
        // `App::windows_with` merges every same-PID AT-SPI entry itself —
        // the stable-identity dedup in tool_windows_with then only guards
        // against listing the same process twice.
        cli::resolve_apps(&target_opts(args)?)?
    } else {
        App::list_with(match backend_arg(args)? {
            Some(b) => crate::provider_for_backend(&b)?,
            None => crate::provider()?,
        })?
    };
    tool_windows_with(&apps, limit, &cli::pointer_center(), refs)
}

/// `enable_web_accessibility` tool. Resolution and the wait are the CLI's
/// value-producing half, so the two surfaces cannot drift.
fn tool_enable_web_accessibility(args: &Value) -> CliResult<ToolOutput> {
    // Only an application has a web content switch. The schema does not
    // advertise `shell`, but a client may send it anyway; refuse it rather
    // than ignore it (tenet 1).
    if args.get("shell").is_some() {
        return Err(usage(
            "`enable_web_accessibility` takes an application (`app` or `pid`), \
             not a shell surface",
        ));
    }
    let app = cli::enable_web_accessibility(&target_opts(args)?)?;
    Ok(ToolOutput::json(json!({
        "ok": true,
        "application": app.name,
        "pid": app.pid,
    })))
}

/// The window-listing half against an explicit app list — the unit-testable
/// sibling of [`tool_windows`], which only resolves where the apps come from.
fn tool_windows_with(
    apps: &[App],
    limit: usize,
    center: &cli::PointerCenter,
    refs: &RefTable,
) -> CliResult<ToolOutput> {
    let mut windows: Vec<Element> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for app in apps {
        // A transient failure in one app (dying process, flaky bridge) must
        // not abort a listing that already holds healthy windows: skip it,
        // keep the successful windows, and surface the failure in `errors`
        // so the partial result is honest, not silently shortened.
        //
        // One Application node per process is the contract on Windows/macOS,
        // but Linux multi-registration can surface several entries for one
        // pid whose windows overlap — deduplicate by stable identity (same
        // key `cmd_windows` uses) BEFORE the limit, so duplicates cannot
        // burn a slot or inflate `count`.
        match app.windows() {
            Ok(ws) => windows.extend(ws),
            Err(e) => errors.push(format!("{}: {e}", app.name)),
        }
    }

    let mut seen = std::collections::HashSet::new();
    windows.retain(|w| seen.insert(cli::window_key(w.data())));

    let total = windows.len();
    // Each listed window gets a ref, as `find` matches do; only the listed
    // ones, so the capture is bounded by `limit` like the result.
    let mut capture = refs.begin();
    let listed: Vec<Value> = windows
        .iter()
        .take(limit)
        .map(|w| {
            let mut encoded = element_data_json(w, center.is_usable());
            if let Some(obj) = encoded.as_object_mut() {
                obj.insert("ref".into(), json!(capture.add(w)));
            }
            encoded
        })
        .collect();
    refs.keep(capture);

    // The error list is capped like the window list: each entry carries an
    // `Error` whose diagnosis can be large (a bounded snapshot, but a
    // snapshot), and one entry per inaccessible/dying process on a busy
    // desktop is unbounded — a silently complete-looking result with an
    // arbitrarily large payload violates the bounded-result contract.
    // Truncation is reported explicitly, never silently.
    let total_errors = errors.len();
    let listed_errors: Vec<String> = errors.into_iter().take(limit).collect();

    let mut out = json!({
        "count": total,
        "returned": listed.len(),
        "truncated": total > listed.len(),
        "windows": listed,
        "errors": listed_errors,
        "errors_truncated": total_errors > listed_errors.len(),
    });
    if let Some(obj) = out.as_object_mut() {
        note_center(obj, center);
    }
    Ok(ToolOutput::json(out))
}

/// One line naming a candidate, carrying only what tells it apart from its
/// siblings.
///
/// Deliberately not `cli::format_element_oneline`: that renders every state
/// and the platform id, which on AT-SPI is a 60-character object path. Twenty
/// of those is a paragraph of noise in a context window, and none of it
/// answers the only question here — which of these did you mean.
fn describe_candidate(data: &crate::ElementData) -> String {
    let mut line = match &data.name {
        Some(name) => format!("{} \"{}\"", data.role.to_snake_case(), truncate(name, 60)),
        None => format!("{} (unnamed)", data.role.to_snake_case()),
    };
    let mut extras: Vec<String> = Vec::new();
    if let Some(value) = &data.value {
        extras.push(format!("value=\"{}\"", truncate(value, 40)));
    }
    if let Some(checked) = &data.states.checked {
        extras.push(format!(
            "checked={}",
            match checked {
                crate::Toggled::Off => "off",
                crate::Toggled::On => "on",
                crate::Toggled::Mixed => "mixed",
            }
        ));
    }
    if data.states.selected {
        extras.push("selected".into());
    }
    if !data.states.enabled {
        extras.push("disabled".into());
    }
    if !data.states.visible {
        extras.push(match data.states.hidden_reason {
            Some(reason) => format!("hidden={reason}"),
            None => "hidden".into(),
        });
    }
    // The centre point is what tells two identically-named siblings apart,
    // and it is also what `click` would need if the caller goes that way.
    if let Some(b) = data.bounds {
        extras.push(format!(
            "at ({},{})",
            b.x + (b.width as i32) / 2,
            b.y + (b.height as i32) / 2
        ));
    }
    if !extras.is_empty() {
        line.push_str(&format!(" [{}]", extras.join(" ")));
    }
    line
}

/// Cut a string to `max` characters, marking the cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

/// Build the "matched more than one" failure, with the candidate list that
/// makes the recovery readable (tenet 6).
///
/// The candidate list is bounded *here*, not only where it is serialized, so
/// the rendered message is bounded too — it is the only copy a client on a
/// revision without `structuredContent` sees.
fn ambiguous(selector: &str, matches: &[Element]) -> CliError {
    cli::ambiguous_selector(selector, matches, MAX_DIAGNOSIS_CANDIDATES, |el| {
        describe_candidate(el)
    })
}

fn tool_click(args: &Value) -> CliResult<ToolOutput> {
    let x = req_i32(args, "x")?;
    let y = req_i32(args, "y")?;
    let button = match opt_str(args, "button")? {
        Some(raw) => parse_button(raw)?,
        None => crate::MouseButton::Left,
    };
    let count = opt_usize(args, "count", 1, CLICK_COUNT)? as u32;
    let held = held_keys(args)?;

    let opts = ClickOptions::new()
        .button(button)
        .count(count)
        .held(held)
        .anchor(crate::Anchor::Center);
    crate::input_sim()?
        .mouse()
        .click_with(ClickTarget::Point(crate::Point::new(x, y)), opts)?;
    Ok(ToolOutput::json(json!({ "ok": true, "x": x, "y": y })))
}

fn tool_move(args: &Value) -> CliResult<ToolOutput> {
    let x = req_i32(args, "x")?;
    let y = req_i32(args, "y")?;
    crate::input_sim()?
        .mouse()
        .move_to(crate::Point::new(x, y))?;
    Ok(ToolOutput::json(json!({ "ok": true, "x": x, "y": y })))
}

fn tool_drag(args: &Value) -> CliResult<ToolOutput> {
    let from = crate::Point::new(req_i32(args, "from_x")?, req_i32(args, "from_y")?);
    let to = crate::Point::new(req_i32(args, "to_x")?, req_i32(args, "to_y")?);
    let button = match opt_str(args, "button")? {
        Some(raw) => parse_button(raw)?,
        None => crate::MouseButton::Left,
    };
    let duration_ms = opt_usize(args, "duration_ms", 150, DRAG_DURATION_MS)?;
    let held = held_keys(args)?;

    let opts = DragOptions::new()
        .button(button)
        .held(held)
        .duration(std::time::Duration::from_millis(duration_ms as u64));
    crate::input_sim()?.mouse().drag_with(from, to, opts)?;
    Ok(ToolOutput::json(json!({
        "ok": true,
        "from": { "x": from.x, "y": from.y },
        "to": { "x": to.x, "y": to.y },
    })))
}

fn tool_scroll(args: &Value) -> CliResult<ToolOutput> {
    let x = req_i32(args, "x")?;
    let y = req_i32(args, "y")?;
    let dx = opt_i32(args, "dx", 0)?;
    let dy = opt_i32(args, "dy", 0)?;
    crate::input_sim()?
        .mouse()
        .scroll(crate::Point::new(x, y), ScrollDelta::new(dx, dy))?;
    Ok(ToolOutput::json(json!({
        "ok": true, "x": x, "y": y, "dx": dx, "dy": dy,
    })))
}

fn tool_key(args: &Value) -> CliResult<ToolOutput> {
    let name = req_str(args, "key")?;
    let key = parse_key_name(name)?;
    let held = held_keys(args)?;
    let sim = crate::input_sim()?;
    if held.is_empty() {
        sim.keyboard().press(key)?;
    } else {
        sim.keyboard().chord(key, &held)?;
    }
    Ok(ToolOutput::json(json!({ "ok": true, "key": name })))
}

fn tool_type(args: &Value) -> CliResult<ToolOutput> {
    let text = req_str(args, "text")?;
    crate::input_sim()?.keyboard().type_text(text)?;
    Ok(ToolOutput::json(json!({
        "ok": true, "typed_characters": text.chars().count(),
    })))
}

fn tool_screenshot(args: &Value) -> CliResult<ToolOutput> {
    const REGION_KEYS: [&str; 4] = ["x", "y", "width", "height"];
    let present: Vec<&str> = REGION_KEYS
        .iter()
        .copied()
        .filter(|k| !matches!(args.get(*k), None | Some(Value::Null)))
        .collect();

    // Every argument is read before the first OS call, so a malformed one
    // costs neither a capture nor a tree read.
    //
    // Partial regions are rejected rather than silently widened to the full
    // screen: a caller who passed three of four coordinates meant to capture
    // a region, and a full-screen image would look like it had worked.
    let region = if present.is_empty() {
        None
    } else if present.len() == REGION_KEYS.len() {
        // `SCREENSHOT_EXTENT` starts at 1, so an empty region is refused here
        // rather than captured as a zero-byte image that looks like a success.
        let width = req_bounded(args, "width", SCREENSHOT_EXTENT)? as u32;
        let height = req_bounded(args, "height", SCREENSHOT_EXTENT)? as u32;
        Some(Rect {
            x: req_i32(args, "x")?,
            y: req_i32(args, "y")?,
            width,
            height,
        })
    } else {
        let missing: Vec<&str> = REGION_KEYS
            .iter()
            .copied()
            .filter(|k| !present.contains(k))
            .collect();
        return Err(usage(format!(
            "a screenshot region needs all of x, y, width, height (missing: {})",
            missing.join(", ")
        )));
    };
    let selectors = annotate_selectors(args)?;

    // Without `annotate` this is the capture it was before annotation
    // existed: no target, no tree read, and the same four summary fields.
    if selectors.is_empty() {
        // A target is only ever read to resolve `annotate` selectors, so one
        // passed without them would have done nothing at all. Returning the
        // whole desktop and reporting `ok` is the failure mode the tool
        // descriptions exist to prevent: the model asked to target an
        // application and cannot tell from the result that it did not. It
        // also lets `{app, shell}` through, which the `shell` property's own
        // description says is refused.
        let named = target_keys(args);
        if !named.is_empty() {
            return Err(usage(format!(
                "{} without \"annotate\" does nothing: a target is only read to resolve \
                 annotation selectors, and it never crops the capture. Add \"annotate\" \
                 (an array of selectors) to box that target's elements, or drop {} for a \
                 plain capture — use \"x\"/\"y\"/\"width\"/\"height\" to capture part \
                 of the screen",
                quoted_list(&named),
                if named.len() == 1 { "it" } else { "them" },
            )));
        }
        let shot = match region {
            Some(rect) => crate::screenshot_region(rect)?,
            None => crate::screenshot()?,
        };
        let png = shot.to_png()?;
        let summary = capture_summary(&shot, &png);
        return Ok(ToolOutput::png(&png, Value::Object(summary)));
    }

    if !has_target(args) {
        // `target` would refuse this too, but its message answers a different
        // question: a model reading "specify app or pid" on a screenshot tool
        // has no reason to connect it to the selectors it just passed.
        return Err(usage(
            "\"annotate\" resolves selectors against a target, and this call names \
             none: add \"app\" (application name), \"pid\" (process id), or \"shell\" \
             (an OS shell surface). Drop \"annotate\" for a plain capture, which needs \
             no target",
        ));
    }
    let target = target(args)?;
    let groups: Vec<Locator> = selectors.iter().map(|s| target.locator(s)).collect();
    let annotated = crate::screenshot_annotated(region, &groups)?;

    let png = annotated.screenshot.to_png()?;
    let mut summary = capture_summary(&annotated.screenshot, &png);
    // Serialized from the same types the CLI's `--legend json` and the
    // bindings render, so the three surfaces cannot describe one box three
    // ways.
    summary.insert("legend".into(), legend_json("legend", &annotated.legend)?);
    summary.insert(
        "omitted".into(),
        legend_json("omitted", &annotated.omitted)?,
    );
    // Always present alongside a legend: a caller has to be able to tell a
    // complete legend from a prefix of one without knowing the cap.
    summary.insert("truncated".into(), json!(annotated.truncated));
    Ok(ToolOutput::png(&png, Value::Object(summary)))
}

/// The fields every capture reports, annotated or not.
///
/// One builder for both paths, so the plain capture cannot drift from the
/// annotated one on what it says about the image.
fn tool_events_start(registry: &Registry, args: &Value) -> CliResult<ToolOutput> {
    // Parse everything before subscribing, so a bad `kinds` entry cannot
    // leave a live platform subscription behind that nobody holds a handle to.
    let kinds = event_kinds(args)?;
    let app = events_app(args)?;
    // Core refuses a kind this platform never sends (`unsupported`) before
    // subscribing; the drainer below still filters what it buffers.
    let sub = app
        .subscribe_with(&crate::SubscribeOptions::new().kinds(kinds.clone().unwrap_or_default()))?;
    Ok(ToolOutput::json(
        registry.start(&app.name, app.pid, sub, kinds)?,
    ))
}

fn tool_events_poll(registry: &Registry, args: &Value) -> CliResult<ToolOutput> {
    let id = req_str(args, "subscription_id")?;
    let max = opt_usize(args, "max", EVENTS_DEFAULT_MAX, EVENTS_MAX)?;
    let timeout_ms = opt_bounded(args, "timeout_ms", 0, EVENTS_TIMEOUT_MS)?;
    // `EVENTS_TIMEOUT_MS.min` is 0, so the conversion cannot fail.
    let timeout = std::time::Duration::from_millis(
        u64::try_from(timeout_ms)
            .map_err(|_| usage(format!("\"timeout_ms\" must not be negative: {timeout_ms}")))?,
    );
    Ok(ToolOutput::json(registry.poll(
        id,
        max,
        timeout,
        &cli::pointer_center(),
    )?))
}

fn tool_events_stop(registry: &Registry, args: &Value) -> CliResult<ToolOutput> {
    let id = req_str(args, "subscription_id")?;
    Ok(ToolOutput::json(registry.stop(id)?))
}

/// Resolve the application to watch.
///
/// Separate from [`target`] because there is no shell surface to resolve:
/// events come from an application's own subscription. `shell` is refused by
/// name rather than by `additionalProperties`, so a client that does not
/// validate against the schema gets the reason instead of a silent
/// full-application subscription — the same answer `xa11y events` gives
/// `--shell`.
fn events_app(args: &Value) -> CliResult<crate::App> {
    if opt_str(args, "shell")?.is_some() {
        return Err(usage(
            "\"shell\" is not a target for the event tools: accessibility events are \
             subscribed per application, and a shell surface has no subscription of \
             its own. Give \"app\" or \"pid\"",
        ));
    }
    let opts = Opts {
        backend: backend_arg(args)?,
        app: opt_str(args, "app")?.map(str::to_string),
        pid: pid_arg(args)?,
        ..Default::default()
    };
    if opts.app.is_none() && opts.pid.is_none() {
        return Err(usage(
            "specify \"app\" (application name, matched exactly) or \"pid\" (process \
             id); the `apps` tool lists both for every running application",
        ));
    }
    resolve_app(&opts)
}

/// The `kinds` filter, validated against the names events actually report.
///
/// An unknown name is refused rather than accepted-and-never-matched: a
/// filter that silently matches nothing looks exactly like an application
/// that emits nothing, and the model has no way to tell which it got.
fn event_kinds(args: &Value) -> CliResult<Option<Vec<String>>> {
    let known = cli::event_kind_names();
    match args.get("kinds") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) if items.is_empty() => Err(usage(
            "\"kinds\" must name at least one event kind; omit it to receive every kind",
        )),
        Some(Value::Array(items)) => {
            let mut kinds: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                let name = match item {
                    Value::String(name) if !name.is_empty() => name,
                    _ => {
                        return Err(usage(
                            "each entry in \"kinds\" must be a non-empty event-kind name",
                        ))
                    }
                };
                if !known.contains(&name.as_str()) {
                    return Err(usage(format!(
                        "unknown event kind: \"{name}\" (expected one of {})",
                        known.join(", ")
                    )));
                }
                if !kinds.iter().any(|k| k == name) {
                    kinds.push(name.clone());
                }
            }
            Ok(Some(kinds))
        }
        Some(_) => Err(usage(
            "\"kinds\" must be an array of event-kind names, e.g. [\"focus_changed\"]",
        )),
    }
}

fn capture_summary(shot: &crate::Screenshot, png: &[u8]) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("width".into(), json!(shot.width));
    out.insert("height".into(), json!(shot.height));
    out.insert("scale".into(), json!(shot.scale));
    out.insert("bytes".into(), json!(png.len()));
    out
}

/// Serialize one of the legend lists.
///
/// `json!` would embed these through an implicit `to_value(..).unwrap()`.
/// Nothing in [`crate::LegendEntry`] or [`crate::Omission`] can fail to
/// serialize today, and this is what keeps that a reported failure rather than
/// a panic if one of them grows a field that can (tenet 4).
fn legend_json<T: serde::Serialize>(what: &str, value: &T) -> CliResult<Value> {
    serde_json::to_value(value).map_err(|e| {
        CliError::Xa11y(crate::Error::Platform {
            code: -1,
            message: format!("serialize the screenshot {what}: {e}"),
        })
    })
}

// ── Element encoding ────────────────────────────────────────────────────────

/// Result-level field naming why element encodings carry no `center`.
pub(super) const CENTER_UNAVAILABLE_FIELD: &str = "center_unavailable";

/// Record on a result why its elements carry no `center`, once, rather than
/// on every node. Absent when centers are reported.
pub(super) fn note_center(out: &mut Map<String, Value>, policy: &cli::PointerCenter) {
    if let cli::PointerCenter::Unavailable(reason) = policy {
        out.insert(CENTER_UNAVAILABLE_FIELD.into(), json!(reason));
    }
}

/// Encode element data as a wire node.
///
/// Hand-built rather than `serde_json::to_value(data)`: `ElementData` also
/// carries `raw` (the whole platform attribute blob) and `handle` (an opaque
/// pointer), neither of which means anything to a model and both of which
/// would dominate the payload. Absent fields are omitted rather than sent as
/// nulls, for the same reason.
///
/// `center` is whether to precompute the bounds' center as a pointer target.
/// The caller decides once per result from [`cli::pointer_center`] and, when
/// it is withheld, says why with [`note_center`] — a node whose `center` is
/// simply missing would read as an element without bounds (issue #573).
pub(crate) fn element_data_json(data: &crate::ElementData, center: bool) -> Value {
    let mut node = Map::new();
    node.insert("role".into(), json!(data.role.to_snake_case()));
    insert_text(&mut node, "name", data.name.as_deref());
    insert_text(&mut node, "value", data.value.as_deref());
    insert_text(&mut node, "description", data.description.as_deref());
    insert_some(&mut node, "id", data.stable_id.as_deref().map(Value::from));

    if let Some(b) = data.bounds {
        node.insert(
            "bounds".into(),
            json!({ "x": b.x, "y": b.y, "width": b.width, "height": b.height }),
        );
        // The point the click / move / scroll tools want, precomputed so the
        // model does not have to do arithmetic to press a button. Withheld
        // where element bounds are not screen coordinates (Wayland), because
        // there it is not a point those tools can use.
        if center {
            node.insert(
                "center".into(),
                json!({
                    "x": b.x + (b.width as i32) / 2,
                    "y": b.y + (b.height as i32) / 2,
                }),
            );
        }
    }

    if let Some(nv) = data.numeric_value {
        node.insert("numeric_value".into(), json!(nv));
        insert_some(&mut node, "min_value", data.min_value.map(Value::from));
        insert_some(&mut node, "max_value", data.max_value.map(Value::from));
    }

    // Table and collection positions: small fixed-size objects, sent only
    // when the platform reported one, each field only when reported (a
    // missing span is absent, never defaulted to 1). `size`, `row_count` and
    // `column_count` are a number or the string "unknown".
    insert_position(&mut node, "table", data.table.filter(|t| !t.is_empty()));
    insert_position(&mut node, "cell", data.cell.filter(|c| !c.is_empty()));
    insert_position(
        &mut node,
        "table_row",
        data.table_row.filter(|r| !r.is_empty()),
    );
    insert_position(
        &mut node,
        "collection",
        data.collection.filter(|c| !c.is_empty()),
    );

    // Document semantics (#476): each only when reported.
    insert_some(
        &mut node,
        "heading_level",
        data.heading_level.map(Value::from),
    );
    insert_some(
        &mut node,
        "landmark_type",
        data.landmark_type.map(|l| Value::from(l.to_snake_case())),
    );
    insert_some(
        &mut node,
        "web_role",
        data.web_role.as_deref().map(Value::from),
    );
    insert_some(&mut node, "url", data.link_url.as_deref().map(Value::from));
    if let Some(live) = data.live_region.as_ref().filter(|l| !l.is_empty()) {
        let mut l = Map::new();
        insert_some(
            &mut l,
            "politeness",
            live.politeness.map(|p| Value::from(p.to_snake_case())),
        );
        insert_some(&mut l, "atomic", live.atomic.map(Value::from));
        insert_some(
            &mut l,
            "relevant",
            live.relevant.as_deref().map(Value::from),
        );
        node.insert("live_region".into(), Value::Object(l));
    }

    if !data.actions.is_empty() {
        node.insert("actions".into(), json!(data.actions));
    }
    node.insert("states".into(), states_json(&data.states));
    Value::Object(node)
}

/// Insert one of the position objects, with its unreported fields dropped.
///
/// These are plain structs of integers and enums, so serializing one cannot
/// fail; if it ever did, the node says so in place of the field rather than
/// silently omitting it.
fn insert_position<T: serde::Serialize>(map: &mut Map<String, Value>, key: &str, value: Option<T>) {
    let Some(value) = value else { return };
    let encoded =
        cli::json_without_nulls(key, &value).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    map.insert(key.into(), encoded);
}

/// Insert a free-text field, cut to [`MAX_FIELD_BYTES`].
///
/// A cut field is followed by `<key>_truncated: true` and `<key>_bytes` (the
/// original length in bytes), so the model can tell a prefix from the whole
/// text and knows how much it is missing.
fn insert_text(map: &mut Map<String, Value>, key: &str, text: Option<&str>) {
    let Some(text) = text else { return };
    let kept = utf8_prefix(text, MAX_FIELD_BYTES);
    map.insert(key.into(), json!(kept));
    if kept.len() < text.len() {
        map.insert(format!("{key}_truncated"), json!(true));
        map.insert(format!("{key}_bytes"), json!(text.len()));
    }
}

/// The longest prefix of `text` that is at most `max` bytes and ends on a
/// character boundary.
fn utf8_prefix(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn insert_some(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.into(), value);
    }
}

/// The boolean states [`states_json`] sends only when true, so an absent one
/// reads as false. One table of name and accessor, so a name cannot drift
/// from the field it reports, and the agent guide's list is checked against
/// the names here.
type StateGetter = fn(&crate::StateSet) -> bool;
const SENT_WHEN_TRUE: [(&str, StateGetter); 9] = [
    ("focused", |s| s.focused),
    ("focusable", |s| s.focusable),
    ("active", |s| s.active),
    ("editable", |s| s.editable),
    ("selected", |s| s.selected),
    ("modal", |s| s.modal),
    ("required", |s| s.required),
    ("busy", |s| s.busy),
    ("password", |s| s.password),
];

/// Encode the state set, sending only what is true or explicitly known.
///
/// `enabled` and `visible` are always sent because they gate whether an
/// element can be acted on at all, and their absence would read as unknown
/// rather than as the documented default.
fn states_json(states: &crate::StateSet) -> Value {
    let mut out = Map::new();
    out.insert("enabled".into(), json!(states.enabled));
    out.insert("visible".into(), json!(states.visible));
    if let Some(reason) = states.hidden_reason {
        out.insert("hidden_reason".into(), json!(reason.to_snake_case()));
    }
    for (key, get) in SENT_WHEN_TRUE {
        if get(states) {
            out.insert(key.into(), json!(true));
        }
    }
    if let Some(checked) = &states.checked {
        out.insert(
            "checked".into(),
            json!(match checked {
                crate::Toggled::Off => "off",
                crate::Toggled::On => "on",
                crate::Toggled::Mixed => "mixed",
            }),
        );
    }
    if let Some(expanded) = states.expanded {
        out.insert("expanded".into(), json!(expanded));
    }
    // Window states are Option<bool>: `None` means "the platform cannot
    // report this" — send only what is known, never a guessed false.
    for (key, value) in [
        ("minimized", states.minimized),
        ("maximized", states.maximized),
        ("fullscreen", states.fullscreen),
    ] {
        if let Some(v) = value {
            out.insert(key.into(), json!(v));
        }
    }
    Value::Object(out)
}

// ── Failure encoding ────────────────────────────────────────────────────────

/// Render a failed tool call as `(text, structuredContent)`.
///
/// This is where tenet 6 reaches the model: a selector that matched nothing
/// comes back with what the search *did* find — near-miss candidates and a
/// bounded snapshot of the scope — so the retry is informed rather than a
/// second guess. `Diagnosis` is projected by hand instead of derived: the
/// truncation policy belongs on this side of the boundary, where the size of
/// a context window is the constraint.
pub(crate) fn describe_failure(tool: &str, err: &CliError) -> (String, Value) {
    let text = err.to_string();
    let mut structured = Map::new();
    structured.insert("tool".into(), json!(tool));
    structured.insert("message".into(), json!(text));
    structured.insert("kind".into(), json!(failure_kind(err)));
    // Whether a retry is safe, on every failure of every tool. A usage error
    // (`invalid_arguments`) attempted nothing, so it reads `not_delivered`:
    // the call can be repeated once the arguments are fixed.
    let delivery = err.delivery().unwrap_or(crate::Delivery::NotDelivered);
    structured.insert("delivery".into(), json!(delivery.to_snake_case()));

    match err {
        // The range and the text's current length as fields, so a caller
        // holding a stale range sees how long the text is now without
        // parsing the message (tenet 6).
        CliError::Xa11y(crate::Error::ElementStale { reason, element }) => {
            structured.insert("stale_reason".into(), json!(reason.to_snake_case()));
            structured.insert("element".into(), json!(element));
        }
        CliError::Xa11y(crate::Error::InvalidTextRange {
            start, end, length, ..
        }) => {
            structured.insert(
                "text_range".into(),
                json!({ "start": start, "end": end, "length": length }),
            );
        }
        CliError::Xa11y(inner) => {
            if let Some(diagnosis) = inner.diagnosis() {
                let mut encoded = diagnosis_json(diagnosis);
                // A `SelectorNotMatched` carries its selector on the error
                // rather than in the diagnosis, because the message already
                // names it. A harness reading `structuredContent` should not
                // have to parse prose to get it back, so it is filled in here
                // when the diagnosis itself did not set it.
                if let (Some(obj), crate::Error::SelectorNotMatched { selector, .. }) =
                    (encoded.as_object_mut(), inner)
                {
                    obj.entry("selector").or_insert_with(|| json!(selector));
                }
                structured.insert("diagnosis".into(), encoded);
            }
        }
        CliError::Ambiguous { count, diagnosis } => {
            structured.insert("match_count".into(), json!(count));
            structured.insert("diagnosis".into(), diagnosis_json(diagnosis));
        }
        CliError::AmbiguousShellSurface {
            count,
            kind,
            diagnosis,
        } => {
            structured.insert("match_count".into(), json!(count));
            structured.insert("shell_kind".into(), json!(kind));
            structured.insert("diagnosis".into(), diagnosis_json(diagnosis));
        }
        CliError::NoSubscription { id, expired, live } => {
            structured.insert("subscription_id".into(), json!(id));
            structured.insert("expired".into(), json!(expired));
            structured.insert("live_subscriptions".into(), json!(live));
        }
        CliError::RefExpired { reference, kept } => {
            structured.insert("ref".into(), json!(reference));
            structured.insert("kept_captures".into(), json!(kept));
        }
        CliError::Usage(_) | CliError::NotFound(_) => {}
    }
    (text, Value::Object(structured))
}

/// Borrow the [`Diagnosis`] an error carries, if any.
fn diagnosis_json(d: &crate::Diagnosis) -> Value {
    let mut out = Map::new();
    insert_some(
        &mut out,
        "condition",
        d.condition.as_deref().map(Value::from),
    );
    insert_some(&mut out, "selector", d.selector.as_deref().map(Value::from));
    insert_some(
        &mut out,
        "last_observed",
        d.last_observed.as_deref().map(Value::from),
    );
    insert_some(
        &mut out,
        "hidden_reason",
        d.hidden_reason.map(|r| Value::from(r.to_snake_case())),
    );
    insert_some(
        &mut out,
        "auto_scroll",
        d.auto_scroll.map(|o| Value::from(o.to_snake_case())),
    );
    insert_some(&mut out, "pending", d.pending.as_deref().map(Value::from));
    if !d.candidates.is_empty() {
        out.insert(
            "candidates".into(),
            json!(d
                .candidates
                .iter()
                .take(MAX_DIAGNOSIS_CANDIDATES)
                .collect::<Vec<_>>()),
        );
        if d.candidates.len() > MAX_DIAGNOSIS_CANDIDATES {
            out.insert(
                "candidates_omitted".into(),
                json!(d.candidates.len() - MAX_DIAGNOSIS_CANDIDATES),
            );
        }
    }
    insert_some(&mut out, "scope", d.scope.as_deref().map(Value::from));
    if let Some(target) = &d.pointer_target {
        out.insert("pointer_target".into(), json!(target));
    }
    // `delivery` is reported once, at the top level of the failure, where it
    // is resolved from the variant and this diagnosis together.
    Value::Object(out)
}

/// A stable machine-readable tag for a failure, so a harness can branch on
/// the kind without parsing the message.
///
/// `Error` is `#[non_exhaustive]`, so the compiler cannot force this match to
/// stay complete. `[[types.variant_coverage]]` in
/// `bindings/parity_allowlist.toml` lists this file for that reason: a new
/// variant fails `cargo xtask check-bindings-parity` until it is named here.
fn failure_kind(err: &CliError) -> &'static str {
    let inner = match err {
        CliError::Usage(_) => return "invalid_arguments",
        CliError::NotFound(_) => return "no_match",
        CliError::Ambiguous { .. } => return "ambiguous_selector",
        CliError::AmbiguousShellSurface { .. } => return "ambiguous_shell_surface",
        // Two tags, not one: an expired handle means "start another
        // subscription", an unknown one means "the id is wrong", and a model
        // that cannot tell them apart retries the wrong one.
        CliError::NoSubscription { expired: true, .. } => return "subscription_expired",
        CliError::NoSubscription { expired: false, .. } => return "subscription_not_found",
        CliError::RefExpired { .. } => return "ref_expired",
        CliError::Xa11y(inner) => inner,
    };
    match inner {
        crate::Error::PermissionDenied { .. } => "permission_denied",
        crate::Error::AccessibilityNotEnabled { .. } => "accessibility_not_enabled",
        crate::Error::SelectorNotMatched { .. } => "no_match",
        crate::Error::ElementStale { .. } => "element_stale",
        crate::Error::ActionNotSupported { .. } => "action_not_supported",
        crate::Error::TextValueNotSupported => "text_value_not_supported",
        crate::Error::Timeout { .. } => "timeout",
        crate::Error::InvalidSelector { .. } => "invalid_selector",
        crate::Error::InvalidActionData { .. } => "invalid_action_data",
        crate::Error::InvalidTextRange { .. } => "invalid_text_range",
        crate::Error::InvalidConfig { .. } => "invalid_config",
        crate::Error::NoElementBounds => "no_element_bounds",
        crate::Error::Unsupported { .. } => "unsupported",
        crate::Error::Platform { .. } => "platform",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_tree_includes_sibling_roots_without_bypassing_budgets() {
        let provider: std::sync::Arc<dyn crate::Provider> =
            xa11y_core::mock::build_split_provider();
        let app = crate::App::by_pid_with(provider, 1234, std::time::Duration::ZERO).unwrap();
        let target = cli::Target::App(app);
        let refs = RefTable::default();
        let (tree, _) = tree_json(&target, 1, 21, TreeDetail::default(), true, &refs).unwrap();
        let children = tree["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[1]["name"], "Second Window");
        assert!(children.iter().all(|child| child.get("children").is_none()));
        let (tree, cap) = tree_json(&target, 5, 2, TreeDetail::default(), true, &refs).unwrap();
        assert_eq!(tree["children"].as_array().unwrap().len(), 1);
        assert!(cap.truncated.by_nodes);
    }

    /// Which of `tree`'s limits cut something, as the result reports them.
    #[derive(Debug)]
    struct Cut {
        by_depth: bool,
        by_node_limit: bool,
    }

    /// Walk the mock app with explicit limits, returning the tree and which
    /// limits cut something. `budget` counts nodes below the root, as the
    /// old walker did; the capture's limit includes the root.
    fn mock_tree(max_depth: usize, budget: usize) -> (Value, Cut) {
        let provider: std::sync::Arc<dyn crate::Provider> = xa11y_core::mock::build_provider();
        let app = crate::App::by_pid_with(provider, 1234, std::time::Duration::ZERO)
            .expect("the mock app resolves by pid");
        let target = cli::Target::App(app);
        let (tree, cap) = tree_json(
            &target,
            max_depth,
            budget + 1,
            TreeDetail::default(),
            true,
            &RefTable::default(),
        )
        .expect("the mock tree captures");
        let cut = Cut {
            by_depth: cap.truncated.by_depth,
            by_node_limit: cap.truncated.by_nodes,
        };
        (tree, cut)
    }

    fn kids(node: &Value) -> &[Value] {
        node.get("children")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    fn assert_complete(node: &Value) {
        assert!(node.get("subtree_truncated").is_none(), "{node}");
        assert!(node.get("children_count").is_none(), "{node}");
        kids(node).iter().for_each(assert_complete);
    }

    #[test]
    fn an_uncut_tree_marks_no_node_and_reports_no_truncation() {
        let (full, cut) = mock_tree(64, TREE_MAX_NODES);
        assert!(!cut.by_depth && !cut.by_node_limit, "{cut:?}");
        assert_complete(&full);
    }

    #[test]
    fn a_depth_cut_marks_each_pruned_node_with_its_real_child_count() {
        let (full, _) = mock_tree(64, TREE_MAX_NODES);
        let (cut_tree, cut) = mock_tree(1, TREE_MAX_NODES);
        assert!(cut.by_depth, "the mock is deeper than one level");
        assert!(!cut.by_node_limit);
        assert!(
            cut_tree.get("subtree_truncated").is_none(),
            "the root kept its children"
        );

        let full_children = kids(&full);
        let cut_children = kids(&cut_tree);
        assert_eq!(full_children.len(), cut_children.len());
        let mut pruned = 0;
        for (whole, shown) in full_children.iter().zip(cut_children) {
            assert!(
                shown.get("children").is_none(),
                "depth 1 stops here: {shown}"
            );
            let real = kids(whole).len();
            if real == 0 {
                // A genuine leaf at the frontier must still read as one.
                assert!(shown.get("subtree_truncated").is_none(), "{shown}");
                assert!(shown.get("children_count").is_none(), "{shown}");
            } else {
                pruned += 1;
                assert_eq!(shown["subtree_truncated"], json!(true), "{shown}");
                assert_eq!(shown["children_count"], json!(real), "{shown}");
            }
        }
        assert!(pruned > 0, "the fixture must have a subtree to prune");
    }

    #[test]
    fn max_depth_zero_marks_the_root_itself() {
        let (full, _) = mock_tree(64, TREE_MAX_NODES);
        let (root, cut) = mock_tree(0, TREE_MAX_NODES);
        assert!(cut.by_depth);
        assert!(root.get("children").is_none());
        assert_eq!(root["subtree_truncated"], json!(true));
        assert_eq!(root["children_count"], json!(kids(&full).len()));
    }

    #[test]
    fn a_node_budget_cut_marks_the_parent_that_lost_children() {
        let (full, _) = mock_tree(64, TREE_MAX_NODES);
        let full_window = &kids(&full)[0];
        let window_children = kids(full_window).len();
        assert!(
            window_children > 1,
            "the fixture window has several children"
        );

        // Budget 1: the window is admitted with nothing left for its children.
        let (root, cut) = mock_tree(64, 1);
        assert!(cut.by_node_limit);
        assert!(
            root.get("subtree_truncated").is_none(),
            "its one child fit: {root}"
        );
        let window = &kids(&root)[0];
        assert!(window.get("children").is_none(), "{window}");
        assert_eq!(window["subtree_truncated"], json!(true), "{window}");
        assert_eq!(window["children_count"], json!(window_children));

        // Budget 2: the window keeps its first child and says how many it has.
        let (root, cut) = mock_tree(64, 2);
        assert!(cut.by_node_limit);
        let window = &kids(&root)[0];
        assert_eq!(kids(window).len(), 1, "{window}");
        assert_eq!(window["subtree_truncated"], json!(true), "{window}");
        assert_eq!(window["children_count"], json!(window_children));
    }

    #[test]
    fn spending_the_budget_exactly_is_not_a_cut() {
        let (full, _) = mock_tree(64, TREE_MAX_NODES);
        fn count(node: &Value) -> usize {
            kids(node).iter().map(|c| 1 + count(c)).sum()
        }
        let (tree, cut) = mock_tree(64, count(&full));
        assert!(!cut.by_node_limit, "every node fit: {cut:?}");
        assert_complete(&tree);
    }

    #[test]
    fn an_oversized_field_is_cut_on_a_char_boundary_and_says_so() {
        let mut data = crate::ElementData::for_role(crate::Role::TextField);
        // 1 + 2n bytes: the cap lands inside a two-byte character.
        let value = format!("a{}", "é".repeat(MAX_FIELD_BYTES));
        data.value = Some(value.clone());
        data.name = Some("x".repeat(MAX_FIELD_BYTES));
        data.description = Some("d".repeat(MAX_FIELD_BYTES + 1));
        let encoded = element_data_json(&data, true);

        let kept = encoded["value"].as_str().expect("value is a string");
        assert!(kept.len() <= MAX_FIELD_BYTES, "{}", kept.len());
        assert_eq!(
            kept.len(),
            MAX_FIELD_BYTES - 1,
            "one byte short of a split é"
        );
        assert!(value.starts_with(kept), "a prefix, not a rewrite");
        assert_eq!(encoded["value_truncated"], json!(true));
        assert_eq!(encoded["value_bytes"], json!(value.len()));

        // Exactly at the cap is whole, and says nothing.
        assert_eq!(
            encoded["name"].as_str().map(str::len),
            Some(MAX_FIELD_BYTES)
        );
        assert!(encoded.get("name_truncated").is_none());
        assert!(encoded.get("name_bytes").is_none());

        assert_eq!(
            encoded["description"].as_str().map(str::len),
            Some(MAX_FIELD_BYTES)
        );
        assert_eq!(encoded["description_truncated"], json!(true));
        assert_eq!(encoded["description_bytes"], json!(MAX_FIELD_BYTES + 1));
    }

    #[test]
    fn the_element_tools_state_the_field_cap_they_enforce() {
        for name in ["tree", "find", "windows", "events_poll"] {
            let description = tool_definition(name)["description"]
                .as_str()
                .expect("description")
                .to_owned();
            assert!(
                description.contains(&format!("at most {MAX_FIELD_BYTES} bytes")),
                "{name}: {description}"
            );
            assert!(description.contains("_truncated"), "{name}: {description}");
        }
        let tree = tool_definition("tree")["description"]
            .as_str()
            .expect("description")
            .to_owned();
        assert!(tree.contains("subtree_truncated"), "{tree}");
        assert!(tree.contains("children_count"), "{tree}");
    }

    fn args(v: Value) -> Value {
        v
    }

    /// One shell surface from the shared mock, relabelled. `ShellSurface` has
    /// no public constructor, so the mock fixture is the only way to reach a
    /// `Target::Shell` without a desktop.
    fn mock_shell_target(kind: crate::ShellSurfaceKind, name: &str, pid: Option<u32>) -> Target {
        let provider: std::sync::Arc<dyn crate::Provider> = xa11y_core::mock::build_provider();
        let mut surface = ShellSurface::list_with(provider)
            .expect("the mock must list its shell surfaces")
            .pop()
            .expect("the mock fixture must vend at least one surface");
        surface.kind = kind;
        surface.name = name.to_string();
        surface.pid = pid;
        Target::Shell(surface)
    }

    #[test]
    fn a_shell_target_is_reported_as_kind_name_and_pid() {
        // The object every tool result carries for a `--shell` target. A model
        // reads `shell.kind` to know it did not act on an application, and
        // `shell.pid` is the only value it can pass back to disambiguate.
        let mut out = Map::new();
        target_fields(
            &mock_shell_target(crate::ShellSurfaceKind::Panel, "Bottom Panel", Some(4242)),
            &mut out,
        );
        assert_eq!(
            out["shell"],
            json!({ "kind": "panel", "name": "Bottom Panel", "pid": 4242 })
        );
        assert_eq!(out["pid"], json!(4242));
        assert!(
            !out.contains_key("application"),
            "a panel is not an application: {out:?}"
        );
    }

    #[test]
    fn a_shell_target_without_a_pid_reports_null_rather_than_omitting_it() {
        // A platform that vends no owner must not look like a missing field —
        // the key is always there, and `null` is what "no honest owner" is.
        let mut out = Map::new();
        target_fields(
            &mock_shell_target(crate::ShellSurfaceKind::Desktop, "Desktop", None),
            &mut out,
        );
        assert_eq!(
            out["shell"],
            json!({ "kind": "desktop", "name": "Desktop", "pid": null })
        );
        assert_eq!(out["pid"], json!(null));
    }

    #[test]
    fn every_listed_tool_has_a_definition_and_is_callable() {
        let host = Xa11yTools::new();
        let defs = host.list();
        assert_eq!(defs.len(), TOOL_NAMES.len());
        for def in &defs {
            let name = def["name"].as_str().expect("tool name");
            assert!(host.has_tool(name), "{name} listed but not callable");
            assert_ne!(
                def["description"], "internal error: tool has no definition",
                "{name} is in TOOL_NAMES with no definition arm"
            );
            assert_eq!(def["inputSchema"]["type"], "object", "{name} schema");
        }
    }

    #[test]
    fn tool_names_are_spec_legal() {
        // Letters, digits, underscore, hyphen and dot only, 1..=128 chars.
        for name in TOOL_NAMES {
            assert!((1..=128).contains(&name.len()), "{name} length");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
                "{name} has characters MCP tool names should not use"
            );
        }
    }

    #[test]
    fn action_schema_lists_exactly_the_verbs_the_dispatcher_accepts() {
        let def = tool_definition("action");
        let listed = def["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        assert_eq!(listed.len(), ACTION_NAMES.len());
        for verb in ACTION_NAMES {
            assert!(
                listed.iter().any(|v| v == verb),
                "{verb} missing from schema"
            );
        }
    }

    #[test]
    fn missing_required_argument_is_a_usage_error() {
        let err = req_str(&args(json!({})), "selector").expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)));
        assert!(err.to_string().contains("selector"));
    }

    #[test]
    fn wrong_typed_argument_says_what_it_wanted() {
        let err = req_str(&args(json!({ "selector": 42 })), "selector").expect_err("must reject");
        assert!(err.to_string().contains("must be a string"));
    }

    #[test]
    fn fractional_coordinates_are_rejected_not_truncated() {
        let err = req_i32(&args(json!({ "x": 12.5 })), "x").expect_err("must reject");
        assert!(err.to_string().contains("whole number"), "{err}");
    }

    #[test]
    fn out_of_range_coordinates_are_rejected_and_name_the_range() {
        let err = req_i32(&args(json!({ "x": 99_999_999_999i64 })), "x").expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("must be between"), "{msg}");
        assert!(msg.contains(&i32::MAX.to_string()), "{msg}");
    }

    #[test]
    fn limits_are_capped_and_the_cap_is_stated() {
        let err = opt_usize(
            &args(json!({ "limit": FIND_MAX_LIMIT + 1 })),
            "limit",
            FIND_DEFAULT_LIMIT,
            FIND_LIMIT,
        )
        .expect_err("must reject");
        assert!(err.to_string().contains(&FIND_MAX_LIMIT.to_string()));
    }

    #[test]
    fn defaults_apply_when_an_optional_argument_is_absent() {
        let got = opt_usize(&args(json!({})), "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT).unwrap();
        assert_eq!(got, FIND_DEFAULT_LIMIT);
    }

    // ── Schema / handler agreement ─────────────────────────────────────────

    /// Every integer argument in every tool schema, as `(tool, key, bounds)`.
    fn schema_integer_bounds() -> Vec<(&'static str, String, Bounds)> {
        let mut found = Vec::new();
        for name in TOOL_NAMES {
            let def = tool_definition(name);
            let Some(props) = def["inputSchema"]["properties"].as_object() else {
                continue;
            };
            for (key, prop) in props {
                if prop["type"] != "integer" {
                    continue;
                }
                let min = prop["minimum"].as_i64().unwrap_or_else(|| {
                    panic!("{name}.{key} is an integer with no declared minimum")
                });
                let max = prop["maximum"].as_i64().unwrap_or_else(|| {
                    panic!("{name}.{key} is an integer with no declared maximum")
                });
                found.push((*name, key.clone(), Bounds { min, max }));
            }
        }
        found
    }

    #[test]
    fn every_integer_argument_declares_the_range_the_handler_enforces() {
        // An undeclared cap reads to a model as an arbitrary refusal, and a
        // declared minimum the handler does not enforce is worse: `count: 0`
        // used to reach the backend as "click zero times" and report ok.
        let bounded = schema_integer_bounds();
        assert!(!bounded.is_empty(), "no integer arguments found at all");
        for (tool, key, bounds) in bounded {
            assert!(bounds.min <= bounds.max, "{tool}.{key} has inverted bounds");
            assert!(
                bounds.check(&key, bounds.min - 1).is_err(),
                "{tool}.{key} accepts one below its declared minimum"
            );
            assert!(
                bounds.check(&key, bounds.max + 1).is_err(),
                "{tool}.{key} accepts one above its declared maximum"
            );
            assert!(bounds.check(&key, bounds.min).is_ok(), "{tool}.{key} min");
            assert!(bounds.check(&key, bounds.max).is_ok(), "{tool}.{key} max");
        }
    }

    #[test]
    fn every_bounded_default_is_inside_its_own_bounds() {
        // `opt_bounded` does not range-check the default, because the default
        // is this module's value rather than the caller's. That is only safe
        // while every one of them is legal.
        for (label, default, bounds) in [
            ("max_depth", TREE_DEFAULT_MAX_DEPTH as i64, TREE_DEPTH),
            ("limit", FIND_DEFAULT_LIMIT as i64, FIND_LIMIT),
            ("count", 1, CLICK_COUNT),
            ("duration_ms", 150, DRAG_DURATION_MS),
            ("max", EVENTS_DEFAULT_MAX as i64, EVENTS_MAX),
            ("timeout_ms", 0, EVENTS_TIMEOUT_MS),
        ] {
            assert!(
                bounds.check(label, default).is_ok(),
                "{label}'s default {default} is outside {bounds:?}"
            );
        }
    }

    #[test]
    fn a_zero_click_count_is_refused_rather_than_reported_as_a_click() {
        let err = opt_usize(&args(json!({ "count": 0 })), "count", 1, CLICK_COUNT)
            .expect_err("zero clicks is not a click");
        assert!(err.to_string().contains("between 1 and 10"), "{err}");
    }

    #[test]
    fn an_empty_screenshot_region_is_refused_before_the_capture() {
        let err = tool_screenshot(&args(json!({ "x": 0, "y": 0, "width": 0, "height": 10 })))
            .expect_err("a zero-width region is not a region");
        assert!(err.to_string().contains("width"), "{err}");
    }

    #[test]
    fn held_keys_reuse_the_cli_key_parser() {
        let keys = held_keys(&args(json!({ "held": ["Shift", "Ctrl"] }))).unwrap();
        assert_eq!(keys, vec![crate::Key::Shift, crate::Key::Ctrl]);
        let err = held_keys(&args(json!({ "held": ["Nope"] }))).expect_err("must reject");
        assert!(err.to_string().contains("Nope"), "{err}");
    }

    #[test]
    fn an_empty_held_list_means_no_modifiers_but_an_empty_entry_does_not() {
        assert!(held_keys(&args(json!({ "held": [] }))).unwrap().is_empty());
        // `[""]` used to join to `""`, which `parse_held` reads as "no
        // modifiers" — a silently different gesture from the one asked for.
        let err = held_keys(&args(json!({ "held": [""] }))).expect_err("must reject");
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn partial_screenshot_regions_are_rejected_with_the_missing_keys() {
        let err = tool_screenshot(&args(json!({ "x": 0, "y": 0, "width": 10 })))
            .expect_err("must reject a partial region");
        let msg = err.to_string();
        assert!(msg.contains("height"), "{msg}");
    }

    #[test]
    fn the_screenshot_schema_offers_annotation_and_the_shared_target() {
        let schema = tool_definition("screenshot")["inputSchema"].clone();
        let props = &schema["properties"];
        assert_eq!(props["annotate"]["type"], "array");
        assert_eq!(props["annotate"]["items"]["type"], "string");
        // The target properties are the shared ones, not a second set: the
        // `shell` enum is the proof, since it is derived from
        // `ShellSurfaceKind::ALL` in one place only.
        assert_eq!(props["shell"]["enum"], json!(cli::shell_kind_names()));
        assert!(props["app"].is_object() && props["pid"].is_object());
        // And none of them became required, so a plain capture is still a
        // call with no arguments at all.
        assert_eq!(schema["required"], json!([]));
    }

    #[test]
    fn the_screenshot_description_states_what_a_model_would_otherwise_probe_for() {
        let description = tool_definition("screenshot")["description"]
            .as_str()
            .expect("a description is a string")
            .to_string();
        // Annotations come from the tree, so an app without one gets none.
        assert!(
            description.contains("no accessibility tree gets no annotations"),
            "{description}"
        );
        // The tag format, and that its number is the `:nth(n)` argument.
        assert!(
            description.contains("`B7` is the seventh match"),
            "{description}"
        );
        assert!(description.contains(":nth(n)"), "{description}");
        assert!(description.contains("legend[i].selector"), "{description}");
        // The cap, and the field that reports when it bit.
        assert!(
            description.contains(&format!("At most {} elements", crate::MAX_ANNOTATIONS)),
            "{description}"
        );
        assert!(description.contains("`truncated` counts"), "{description}");
        // And that a target is not silently ignored without `annotate`. The
        // handler is held to that promise by
        // `a_target_without_annotate_is_refused_rather_than_captured_full_screen`.
        assert!(
            description.contains("A target passed without `annotate` is refused"),
            "{description}"
        );
    }

    #[test]
    fn annotating_without_a_target_says_which_arguments_fix_it() {
        let err = tool_screenshot(&args(json!({ "annotate": ["button"] })))
            .expect_err("selectors need something to resolve against");
        let msg = err.to_string();
        assert!(msg.contains("annotate"), "{msg}");
        for key in ["\"app\"", "\"pid\"", "\"shell\""] {
            assert!(msg.contains(key), "{key} must be offered: {msg}");
        }
        // The CLI's version of this names `--app`; no flag exists here.
        assert!(!msg.contains("--"), "no flags on this surface: {msg}");
    }

    #[test]
    fn a_target_without_annotate_is_refused_rather_than_captured_full_screen() {
        // Each of these used to return a full-screen capture and report `ok`,
        // so a model could not tell that its target had done nothing. The
        // refusal happens before any capture, which is why this runs headless.
        for (arguments, named) in [
            (json!({ "app": "Calculator" }), vec!["\"app\""]),
            (json!({ "pid": 42 }), vec!["\"pid\""]),
            (json!({ "shell": "taskbar" }), vec!["\"shell\""]),
            // This one also contradicted the `shell` property's own promise
            // that passing it together with `app` is refused.
            (
                json!({ "app": "Calculator", "shell": "taskbar" }),
                vec!["\"app\"", "\"shell\""],
            ),
            (
                json!({ "app": "Calculator", "x": 0, "y": 0, "width": 4, "height": 4 }),
                vec!["\"app\""],
            ),
            // An empty `annotate` is the plain-capture path too, so it is
            // refused for the same reason an absent one is.
            (json!({ "annotate": [], "pid": 42 }), vec!["\"pid\""]),
            // `backend` picks how to attach to a target and is just as inert
            // without `annotate`; it used to be dropped silently.
            (json!({ "backend": "native" }), vec!["\"backend\""]),
        ] {
            let err = tool_screenshot(&args(arguments.clone()))
                .expect_err(&format!("{arguments} must be refused"));
            assert!(matches!(err, CliError::Usage(_)), "{arguments}: {err}");
            let msg = err.to_string();
            for key in named {
                assert!(msg.contains(key), "{arguments}: {key} unnamed in {msg}");
            }
            // And the answer names both ways out.
            assert!(msg.contains("annotate"), "{arguments}: {msg}");
            assert!(msg.contains("plain capture"), "{arguments}: {msg}");
        }
    }

    #[test]
    fn a_backend_alone_does_not_count_as_an_annotation_target() {
        let err = tool_screenshot(&args(
            json!({ "annotate": ["button"], "backend": "native" }),
        ))
        .expect_err("annotate needs a target");
        assert!(err.to_string().contains("names none"), "{err}");
    }

    #[test]
    fn an_unknown_backend_is_invalid_arguments_naming_the_argument() {
        for (tool, arguments) in [
            ("tree", json!({ "app": "X", "backend": "foo" })),
            ("windows", json!({ "backend": "foo" })),
            ("capabilities", json!({ "backend": "foo" })),
            ("events_start", json!({ "app": "X", "backend": "foo" })),
        ] {
            let err = Xa11yTools::new()
                .call(tool, &args(arguments.clone()))
                .expect_err("unknown backend");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{tool}: {err}");
            let msg = err.to_string();
            assert!(msg.contains("\"backend\""), "{tool}: {msg}");
            assert!(!msg.contains("XA11Y_ACCESSIBILITY_SOURCE"), "{tool}: {msg}");
        }
        // A bad platform beside a valid backend is the caller's argument too.
        let err = tool_capabilities(&args(json!({ "platform": "bogus", "backend": "native" })))
            .expect_err("unknown platform");
        assert_eq!(failure_kind(&err), "invalid_arguments", "{err}");
    }

    #[test]
    fn annotate_entries_must_be_non_empty_selector_strings() {
        for (arguments, expected) in [
            (json!({ "annotate": [""] }), "must not be empty"),
            (json!({ "annotate": [7] }), "selector strings"),
            (json!({ "annotate": "button" }), "must be an array"),
        ] {
            let err = annotate_selectors(&args(arguments.clone()))
                .expect_err(&format!("{arguments} must be refused"));
            assert!(err.to_string().contains(expected), "{arguments}: {err}");
        }
        // The absent and empty forms both mean "no annotation", which is the
        // plain capture path.
        assert!(annotate_selectors(&args(json!({}))).unwrap().is_empty());
        assert!(annotate_selectors(&args(json!({ "annotate": [] })))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_bad_region_is_refused_before_the_annotation_target_is_resolved() {
        // Both are wrong here. The region is read first, so the answer names
        // the argument the caller can fix without a tree read having happened.
        let err = tool_screenshot(&args(json!({
            "x": 0, "y": 0, "width": 10, "annotate": ["button"], "pid": 1,
        })))
        .expect_err("a partial region is still a partial region");
        assert!(err.to_string().contains("height"), "{err}");
    }

    #[test]
    fn a_plain_capture_summary_keeps_exactly_the_fields_it_always_had() {
        // The unannotated result is byte-identical to what it was before
        // annotation existed; `legend` and friends appear only with
        // `annotate`, which is what `screenshot_description` promises.
        let shot = crate::Screenshot::new(4, 2, vec![0; 4 * 2 * 4], 2.0);
        let summary = capture_summary(&shot, &[0u8; 11]);
        assert_eq!(
            Value::Object(summary),
            json!({ "width": 4, "height": 2, "scale": 2.0, "bytes": 11 })
        );
    }

    #[test]
    fn the_legend_is_serialized_from_the_shared_types() {
        // One shape for the CLI's `--legend json`, this result, and the
        // bindings — asserted here so a hand-built second shape cannot creep
        // back in.
        let entry = crate::LegendEntry::new(
            2,
            7,
            "button:nth(7)",
            "button",
            Some("OK".into()),
            Rect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            [0, 114, 178],
        );
        let encoded = legend_json("legend", &vec![entry]).expect("a legend serializes");
        assert_eq!(encoded[0]["tag"], "B7");
        assert_eq!(encoded[0]["selector"], "button:nth(7)");
        assert_eq!(encoded[0]["color"], json!([0, 114, 178]));

        let omission = crate::Omission::new(
            "button:nth(9)",
            "button",
            None,
            crate::OmissionReason::OutsideCapture,
        );
        let encoded = legend_json("omitted", &vec![omission]).expect("an omission serializes");
        assert_eq!(encoded[0]["reason"], "outside_capture");
    }

    // ── Event subscriptions ─────────────────────────────────────────────

    #[test]
    fn the_event_tools_are_a_trio_and_the_handle_ties_them_together() {
        // The shape the specification's Stateful Tools guidance asks for:
        // a creation tool hands out a handle, the others take it.
        for name in ["events_poll", "events_stop"] {
            let schema = tool_definition(name)["inputSchema"].clone();
            assert_eq!(schema["required"], json!(["subscription_id"]));
            assert!(
                schema["properties"]["subscription_id"]["description"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("events_start"),
                "{name} must say where its handle comes from"
            );
        }
    }

    #[test]
    fn events_start_states_the_retention_the_registry_actually_enforces() {
        // The specification asks a stateful tool to state its handle's
        // retention, and a description naming a different number than the
        // registry enforces is worse than one that says nothing.
        let description = tool_definition("events_start")["description"]
            .as_str()
            .expect("a description")
            .to_string();
        assert!(
            description.contains(&format!("{} minutes", EXPIRY.as_secs() / 60)),
            "{description}"
        );
        assert!(
            description.contains(&BUFFER_CAPACITY.to_string()),
            "the buffer size a caller has to poll fast enough to stay inside: {description}"
        );
        assert!(
            description.contains("*before* the action"),
            "a subscription started after the click observes nothing: {description}"
        );
    }

    #[test]
    fn events_poll_says_what_an_empty_result_and_a_dropped_count_mean() {
        let definition = tool_definition("events_poll");
        let description = definition["description"].as_str().expect("a description");
        assert!(
            description.contains("does not block"),
            "a model that expects a long poll by default waits for nothing: {description}"
        );
        assert!(
            description.contains("dropped"),
            "loss is only actionable if the caller knows the field exists: {description}"
        );
        assert!(
            description.contains("live: false"),
            "polling a dead stream forever is the failure mode this prevents: {description}"
        );
    }

    #[test]
    fn events_start_offers_no_shell_argument_and_says_why_when_one_arrives() {
        // The schema is the first answer; the handler is the one a client that
        // does not validate against it gets.
        let props = tool_definition("events_start")["inputSchema"]["properties"].clone();
        assert!(props.get("shell").is_none(), "{props}");

        let err = tool_events_start(&Registry::new(), &args(json!({ "shell": "taskbar" })))
            .expect_err("a shell surface has no subscription of its own");
        assert!(
            err.to_string().contains("per application"),
            "must say why rather than reading as a misspelled argument: {err}"
        );
    }

    #[test]
    fn events_start_without_a_target_names_the_arguments_that_fix_it() {
        let err = tool_events_start(&Registry::new(), &args(json!({})))
            .expect_err("something has to be watched");
        assert!(err.to_string().contains("\"app\""), "{err}");
        assert!(err.to_string().contains("\"pid\""), "{err}");
    }

    #[test]
    fn the_kinds_filter_advertises_exactly_the_names_events_report() {
        let items =
            tool_definition("events_start")["inputSchema"]["properties"]["kinds"]["items"].clone();
        let advertised: Vec<String> = items["enum"]
            .as_array()
            .expect("the filter enumerates its names")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(advertised, cli::event_kind_names());
    }

    #[test]
    fn an_unknown_event_kind_is_refused_with_the_ones_that_exist() {
        // Accepting it would look exactly like an application that emits
        // nothing, and the caller could not tell which it got.
        let err = event_kinds(&args(json!({ "kinds": ["focus_change"] })))
            .expect_err("a near-miss spelling is still a miss");
        assert!(err.to_string().contains("focus_change"), "{err}");
        assert!(err.to_string().contains("focus_changed"), "{err}");
    }

    #[test]
    fn an_empty_kinds_list_is_refused_rather_than_read_as_no_filter() {
        // `[]` reads as "no kinds at all", which would buffer nothing forever.
        let err = event_kinds(&args(json!({ "kinds": [] }))).expect_err("must refuse");
        assert!(err.to_string().contains("omit it"), "{err}");
        assert_eq!(event_kinds(&args(json!({}))).unwrap(), None);
    }

    #[test]
    fn a_repeated_kind_is_kept_once() {
        let kinds = event_kinds(&args(
            json!({ "kinds": ["focus_changed", "focus_changed"] }),
        ))
        .expect("a repeat is a caller's redundancy, not an error");
        assert_eq!(kinds, Some(vec!["focus_changed".to_string()]));
    }

    #[test]
    fn a_poll_needs_its_handle_and_range_checks_the_rest() {
        let registry = Registry::new();
        let err = tool_events_poll(&registry, &args(json!({}))).expect_err("must refuse");
        assert!(err.to_string().contains("subscription_id"), "{err}");

        let err = tool_events_poll(
            &registry,
            &args(json!({ "subscription_id": "sub_1", "timeout_ms": 60_000 })),
        )
        .expect_err("a timeout past the cap is refused, not silently clamped");
        assert!(err.to_string().contains("15000"), "{err}");
    }

    #[test]
    fn an_unknown_handle_is_a_fixable_tool_error_carrying_its_kind() {
        let registry = Registry::new();
        let err = tool_events_poll(&registry, &args(json!({ "subscription_id": "sub_7" })))
            .expect_err("nothing was ever started");
        assert_eq!(failure_kind(&err), "subscription_not_found");
        let (_, structured) = describe_failure("events_poll", &err);
        assert_eq!(structured["subscription_id"], json!("sub_7"));
        assert_eq!(structured["expired"], json!(false));
        assert_eq!(structured["live_subscriptions"], json!([]));
    }

    #[test]
    fn an_expired_handle_is_a_different_kind_from_an_unknown_one() {
        let err = CliError::NoSubscription {
            id: "sub_1".into(),
            expired: true,
            live: vec!["sub_2".into()],
        };
        assert_eq!(failure_kind(&err), "subscription_expired");
        let (text, structured) = describe_failure("events_poll", &err);
        assert!(
            text.contains("sub_2"),
            "the open handles are the way out, so they belong in the message: {text}"
        );
        assert_eq!(structured["live_subscriptions"], json!(["sub_2"]));
    }

    // ── Element refs (#481) ─────────────────────────────────────────────────

    /// A `tree` over the mock app, kept in `refs`, and the ref of the node
    /// named `name`.
    fn ref_named(target: &Target, refs: &RefTable, name: &str) -> String {
        let (tree, _) = tree_json(
            target,
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            TreeDetail::default(),
            true,
            refs,
        )
        .expect("the mock tree captures");
        fn search(node: &Value, name: &str) -> Option<String> {
            if node["name"] == json!(name) {
                return node["ref"].as_str().map(str::to_string);
            }
            node["children"]
                .as_array()?
                .iter()
                .find_map(|child| search(child, name))
        }
        search(&tree, name).unwrap_or_else(|| panic!("no node named {name}: {tree}"))
    }

    #[test]
    fn an_action_by_ref_reaches_exactly_the_node_read() {
        let (target, provider) = mock_target_app();
        let refs = RefTable::default();
        let back = ref_named(&target, &refs, "Back");
        let args =
            cli::parse_action_args("press", None, None, None, None, &cli::CLI_ACTION_ARG_NAMES)
                .expect("args");
        let out = tool_action_on_ref(&refs, "press", &back, &args).expect("press by ref");
        let body = out.structured.expect("structured");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["ref"], json!(back));
        let (_, verb, _) = provider.actions().pop().expect("an action was recorded");
        assert_eq!(verb, "press");
    }

    #[test]
    fn an_action_by_ref_on_a_removed_node_fails_stale_with_its_reason() {
        let (target, provider) = mock_target_app();
        let refs = RefTable::default();
        let back = ref_named(&target, &refs, "Back");
        provider.remove_node("Back");
        let before = provider.actions().len();
        let args =
            cli::parse_action_args("press", None, None, None, None, &cli::CLI_ACTION_ARG_NAMES)
                .expect("args");
        let err = tool_action_on_ref(&refs, "press", &back, &args).expect_err("stale");
        assert_eq!(failure_kind(&err), "element_stale");
        let (_, structured) = describe_failure("action", &err);
        assert_eq!(structured["stale_reason"], json!("removed"));
        assert!(structured["element"]
            .as_str()
            .is_some_and(|e| e.contains("Back")));
        assert_eq!(structured["delivery"], json!("not_delivered"));
        assert_eq!(provider.actions().len(), before, "nothing reached the app");
    }

    #[test]
    fn a_ref_from_an_aged_out_capture_is_ref_expired() {
        let (target, _) = mock_target_app();
        let refs = RefTable::default();
        let back = ref_named(&target, &refs, "Back");
        for _ in 0..super::super::refs::MAX_CAPTURES {
            ref_named(&target, &refs, "Forward");
        }
        let args =
            cli::parse_action_args("press", None, None, None, None, &cli::CLI_ACTION_ARG_NAMES)
                .expect("args");
        let err = tool_action_on_ref(&refs, "press", &back, &args).expect_err("expired");
        assert_eq!(failure_kind(&err), "ref_expired");
        let (_, structured) = describe_failure("action", &err);
        assert_eq!(structured["ref"], json!(back));
    }

    #[test]
    fn selector_and_ref_are_exclusive_and_a_ref_takes_no_target() {
        let refs = RefTable::default();
        for args in [
            json!({ "action": "press" }),
            json!({ "action": "press", "selector": "button", "ref": "c1:0" }),
            json!({ "action": "press", "ref": "c1:0", "app": "TestApp" }),
            json!({ "action": "press", "ref": "not-a-ref" }),
        ] {
            let err = tool_action(&args, &refs).expect_err("must refuse");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{args}");
        }
        for tool in ["text", "table"] {
            let args = json!({ "op": "info", "ref": "c1:0", "pid": 1 });
            let err = match tool {
                "text" => tool_text(&args, &refs),
                _ => tool_table(&args, &refs),
            }
            .expect_err("must refuse");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{tool}");
        }
    }

    #[test]
    fn find_matches_carry_refs() {
        let (target, _) = mock_target_app();
        let refs = RefTable::default();
        let element = target
            .locator(r#"button[name="Back"]"#)
            .element()
            .expect("Back");
        let out = find_output(
            r#"button[name="Back"]"#,
            &target,
            &[element],
            1,
            &cli::PointerCenter::Usable,
            &refs,
        );
        let body = out.structured.expect("structured");
        let r = body["matches"][0]["ref"].as_str().expect("ref");
        assert!(refs.resolve(r).is_ok());
    }

    #[test]
    fn capabilities_tool_answers_for_any_platform() {
        let out = tool_capabilities(
            &json!({ "platform": "linux", "names": ["event:menu_opened", "window:"] }),
        )
        .expect("capabilities");
        let body = out.structured.expect("structured");
        assert_eq!(body["platform"], json!("linux"));
        assert_eq!(body["os_version"], Value::Null);
        let caps = body["capabilities"].as_array().expect("array");
        assert_eq!(caps[0]["name"], json!("event:menu_opened"));
        assert_eq!(caps[0]["support"], json!("unsupported"));
        assert!(
            caps[0]["note"].is_string(),
            "an unsupported answer says why"
        );
        assert!(caps
            .iter()
            .skip(1)
            .all(|c| c["name"].as_str().is_some_and(|n| n.starts_with("window:"))));
        assert_eq!(body["count"], json!(caps.len()));

        let all = tool_capabilities(&json!({ "platform": "windows", "os_version": "10.0.19045" }))
            .expect("capabilities")
            .structured
            .expect("structured");
        assert_eq!(all["os_version"], json!("10.0.19045"));
        assert_eq!(all["count"], json!(crate::Capabilities::names().count()));
    }

    #[test]
    fn capabilities_tool_rejects_bad_arguments_as_invalid() {
        for args in [
            json!({ "platform": "darwin" }),
            json!({ "platform": "linux", "os_version": "6.1" }),
            json!({ "platform": "windows", "os_version": "eleven" }),
            json!({ "names": ["event:menu_open"] }),
            json!({ "names": ["nope:"] }),
            json!({ "names": "event:menu_opened" }),
            json!({ "names": [1] }),
        ] {
            let err = tool_capabilities(&args).expect_err("must reject");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{args}");
        }
        let err = tool_capabilities(&json!({ "names": ["event:menu_open"] })).expect_err("typo");
        assert!(
            err.to_string().contains("event:menu_opened"),
            "closest names listed: {err}"
        );
    }

    #[test]
    fn failure_kinds_cover_the_error_surface() {
        assert_eq!(
            failure_kind(&CliError::Usage("x".into())),
            "invalid_arguments"
        );
        assert_eq!(failure_kind(&CliError::NotFound("x".into())), "no_match");
        assert_eq!(
            failure_kind(&CliError::Xa11y(crate::Error::NoElementBounds)),
            "no_element_bounds"
        );
        assert_eq!(
            failure_kind(&CliError::NoSubscription {
                id: "sub_1".into(),
                expired: false,
                live: Vec::new(),
            }),
            "subscription_not_found"
        );
    }

    /// The `kind` column of the agent guide's failure table.
    fn guide_failure_kinds() -> Vec<String> {
        crate::cli::AGENT_GUIDE
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|rest| rest.split_once("` |"))
            .map(|(kind, _)| kind.to_string())
            .filter(|kind| kind != "kind")
            .collect()
    }

    /// Every tag `failure_kind` can return, read from its own source.
    ///
    /// Parsed rather than listed by hand, so a kind added to the function is
    /// a kind this test sees without anyone remembering to update a list.
    fn failure_kinds_in_source() -> Vec<String> {
        // A Windows checkout may carry CRLF line endings, which would hide
        // the `\n}\n` that ends the function.
        let source = include_str!("tools.rs").replace("\r\n", "\n");
        let start = source
            .find("\nfn failure_kind(")
            .expect("failure_kind is defined in this file");
        let body = &source[start..];
        let end = body
            .find("\n}\n")
            .expect("failure_kind has a closing brace");
        let mut kinds: Vec<String> = body[..end]
            .lines()
            // Comment lines quote nothing that is returned.
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(|line| line.split('"').skip(1).step_by(2).map(str::to_owned))
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    #[test]
    fn the_agent_guide_explains_every_failure_kind_the_server_returns() {
        let returned = failure_kinds_in_source();
        assert!(
            returned.len() > 10,
            "the parse found too little: {returned:?}"
        );
        // Spot-check the parse against the function itself.
        assert!(returned.contains(&failure_kind(&CliError::Usage("x".into())).to_string()));
        let documented = guide_failure_kinds();
        for kind in &returned {
            assert!(
                documented.contains(kind),
                "AGENT_GUIDE.md has no row for `{kind}`: {documented:?}"
            );
        }
        for kind in &documented {
            assert!(
                returned.contains(kind),
                "AGENT_GUIDE.md documents `{kind}`, which the server never returns"
            );
        }
    }

    #[test]
    fn the_agent_guide_names_what_the_tools_actually_take() {
        let guide = crate::cli::AGENT_GUIDE;
        // Every action verb, spelled as the tool takes it.
        for verb in cli::ACTION_NAMES {
            assert!(guide.contains(&format!("`{verb}`")), "verb {verb}");
        }
        assert!(guide.contains(crate::DEFAULT_TIMEOUT_ENV_VAR));
        assert!(guide.contains(&format!("default {TREE_DEFAULT_MAX_DEPTH}")));
        assert!(guide.contains(&format!("default {FIND_DEFAULT_LIMIT}")));
        assert!(guide.contains(super::super::protocol::GUIDE_URI));
        // The workflow's numbered steps each name a tool; each must be one
        // the server lists.
        let tools: Vec<String> = Xa11yTools::new()
            .list()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        let steps: Vec<&str> = guide
            .lines()
            .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .filter_map(|l| l.split('`').nth(1))
            .collect();
        assert!(steps.len() >= 4, "{steps:?}");
        for tool in steps {
            assert!(tools.iter().any(|t| t == tool), "workflow names `{tool}`");
        }
    }

    #[test]
    fn the_agent_guide_states_the_selector_case_rule_the_engine_applies() {
        use xa11y_core::selector::{match_op, MatchOp};
        let guide = crate::cli::AGENT_GUIDE;
        assert!(guide.contains("`=` is case-sensitive. `*=`, `^=` and `$=` ignore case."));
        assert!(!match_op(&MatchOp::Exact, "ok", Some("OK")));
        assert!(match_op(&MatchOp::Contains, "ok", Some("OK")));
        assert!(match_op(&MatchOp::StartsWith, "ok", Some("OKAY")));
        assert!(match_op(&MatchOp::EndsWith, "ok", Some("BOOK")));
    }

    #[test]
    fn the_agent_guide_states_the_default_timeout() {
        assert!(crate::cli::AGENT_GUIDE.contains("5 seconds unless `XA11Y_DEFAULT_TIMEOUT`"));
        // The number is pinned against the runtime default whenever nothing
        // overrides it; a developer's environment may legitimately set it.
        if std::env::var_os(crate::DEFAULT_TIMEOUT_ENV_VAR).is_none() {
            assert_eq!(
                crate::default_timeout().expect("the built-in default resolves"),
                std::time::Duration::from_secs(5)
            );
        }
    }

    #[test]
    fn the_agent_guide_states_the_absent_state_rules_the_encoder_applies() {
        let guide = crate::cli::AGENT_GUIDE;
        // Nothing set: only the always-sent pair appears.
        let mut states = crate::StateSet::default();
        states.enabled = false;
        states.visible = false;
        let none = states_json(&states);
        let keys: Vec<&String> = none.as_object().expect("object").keys().collect();
        assert_eq!(keys, ["enabled", "visible"]);
        assert!(guide.contains("`enabled` and `visible` are always present"));
        // Each flag sent only when true is named in the guide's list of them.
        let start = guide.find("- `focused`").expect("the sent-when-true list");
        let section = &guide[start..];
        let section = &section[..section.find("\n- ").unwrap_or(section.len())];
        assert!(section.contains("appear only when true"), "{section}");
        for (key, _) in SENT_WHEN_TRUE {
            assert!(section.contains(&format!("`{key}`")), "{key} in {section}");
        }
    }

    #[test]
    fn each_sent_when_true_name_reports_its_own_field() {
        let set = |name: &str, s: &mut crate::StateSet| match name {
            "focused" => s.focused = true,
            "focusable" => s.focusable = true,
            "active" => s.active = true,
            "editable" => s.editable = true,
            "selected" => s.selected = true,
            "modal" => s.modal = true,
            "required" => s.required = true,
            "busy" => s.busy = true,
            "password" => s.password = true,
            other => panic!("no setter for {other}; add one here"),
        };
        for (name, _) in SENT_WHEN_TRUE {
            let mut states = crate::StateSet::default();
            set(name, &mut states);
            let sent = states_json(&states);
            let extra: Vec<&String> = sent
                .as_object()
                .expect("object")
                .keys()
                .filter(|k| !matches!(k.as_str(), "enabled" | "visible"))
                .collect();
            assert_eq!(extra, [name], "setting {name} alone");
        }
    }

    #[test]
    fn the_agent_guide_states_the_field_cap_the_encoder_applies() {
        let guide = crate::cli::AGENT_GUIDE;
        // Compared with whitespace collapsed: the sentence wraps, and a
        // Windows checkout wraps it with CRLF.
        let flat = guide.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(&format!("cut to {MAX_FIELD_BYTES} bytes")),
            "the guide must state the cap as {MAX_FIELD_BYTES} bytes"
        );
        let mut data = crate::ElementData::for_role(crate::Role::StaticText);
        data.name = Some("x".repeat(MAX_FIELD_BYTES + 1));
        let encoded = element_data_json(&data, true);
        for key in ["name_truncated", "name_bytes"] {
            assert!(encoded.get(key).is_some(), "{key} in {encoded}");
        }
        assert!(guide.contains("`<field>_truncated: true`"));
        assert!(guide.contains("`<field>_bytes`"));
    }

    #[test]
    fn the_agent_guide_names_the_per_node_cut_markers_the_encoder_sets() {
        let guide = crate::cli::AGENT_GUIDE;
        let (root, _) = mock_tree(0, TREE_MAX_NODES);
        let marked: Vec<&String> = root
            .as_object()
            .expect("object")
            .keys()
            .filter(|k| matches!(k.as_str(), "subtree_truncated" | "children_count"))
            .collect();
        assert_eq!(marked.len(), 2, "{root}");
        assert!(guide.contains("`subtree_truncated: true`"));
        assert!(guide.contains("`children_count`"));
    }

    #[test]
    fn a_diagnosis_reaches_the_structured_payload() {
        let err = CliError::Xa11y(crate::Error::SelectorNotMatched {
            selector: "button[name=\"Ok\"]".into(),
            diagnosis: Some(Box::new(
                crate::Diagnosis::new()
                    .condition("visible")
                    .last_observed("selector never matched")
                    .candidates(vec!["button \"OK\"".to_string()]),
            )),
        });
        let (text, structured) = describe_failure("find", &err);
        assert!(text.contains("button"), "{text}");
        assert_eq!(structured["kind"], "no_match");
        assert_eq!(structured["tool"], "find");
        assert_eq!(structured["diagnosis"]["condition"], "visible");
        assert_eq!(structured["diagnosis"]["candidates"][0], "button \"OK\"");
        assert!(structured["diagnosis"].get("hidden_reason").is_none());
        assert!(structured["diagnosis"].get("auto_scroll").is_none());
    }

    #[test]
    fn off_display_pointer_geometry_reaches_the_failure_payload() {
        let point = crate::Point::new(500, 500);
        let input = crate::InputSim::new(std::sync::Arc::new(
            xa11y_core::mock::MockInputProvider::new(std::time::Duration::ZERO),
        ));
        let err = CliError::Xa11y(input.mouse().click(point).unwrap_err());
        let (text, structured) = describe_failure("click", &err);
        assert_eq!(structured["kind"], "invalid_action_data");
        assert_eq!(structured["delivery"], "not_delivered");
        let geometry = &structured["diagnosis"]["pointer_target"];
        assert_eq!(geometry["point"], json!({"x": 500, "y": 500}));
        assert_eq!(geometry["checked_point"], geometry["point"]);
        assert_eq!(geometry["coordinate_space"], "mock desktop pixels");
        assert_eq!(geometry["element_bounds"], Value::Null);
        assert_eq!(geometry["anchor"], Value::Null);
        assert_eq!(geometry["omitted_displays"], 0);
        assert_eq!(
            geometry["displays"][0],
            json!({"x": 0, "y": 0, "width": 100, "height": 100})
        );
        assert!(text.contains("pointer target:"), "{text}");
        assert!(text.contains("mock desktop pixels"), "{text}");
    }

    #[test]
    fn a_timeout_on_a_hidden_element_names_the_reason() {
        let err = CliError::Xa11y(
            crate::Error::timeout(std::time::Duration::from_secs(1)).diagnose(
                crate::Diagnosis::new()
                    .condition("press target actionable (visible && enabled)")
                    .last_observed(
                        "matched button \"Save\" (visible=false, enabled=true, focused=false)",
                    )
                    .hidden_reason(crate::HiddenReason::ScrolledOut)
                    .auto_scroll(crate::AutoScrollOutcome::Refused),
            ),
        );
        let (text, structured) = describe_failure("action", &err);
        assert!(text.contains("hidden because: scrolled_out"), "{text}");
        assert!(text.contains("auto-scroll: refused"), "{text}");
        assert_eq!(structured["diagnosis"]["hidden_reason"], "scrolled_out");
        assert_eq!(structured["diagnosis"]["auto_scroll"], "refused");
    }

    #[test]
    fn every_failure_reports_its_delivery() {
        let cases = [
            (
                CliError::Xa11y(crate::Error::selector_not_matched("button")),
                Some("not_delivered"),
            ),
            (
                CliError::Xa11y(crate::Error::Platform {
                    code: -25204,
                    message: "cannot complete".into(),
                }),
                Some("unknown"),
            ),
            (
                // A wait that expired before acting overrides the variant.
                CliError::Xa11y(
                    crate::Error::timeout(std::time::Duration::from_secs(1))
                        .diagnose(crate::Diagnosis::new().delivery(crate::Delivery::NotDelivered)),
                ),
                Some("not_delivered"),
            ),
            (
                // A bare timeout may have been dispatched.
                CliError::Xa11y(crate::Error::timeout(std::time::Duration::from_secs(1))),
                Some("unknown"),
            ),
            (CliError::NotFound("no match".into()), Some("not_delivered")),
            // Nothing was attempted, so repeating the call is safe.
            (CliError::Usage("bad flag".into()), Some("not_delivered")),
        ];
        for (err, expected) in cases {
            let (_, structured) = describe_failure("action", &err);
            assert_eq!(
                structured.get("delivery").and_then(Value::as_str),
                expected,
                "{err}"
            );
        }
    }

    #[test]
    fn candidate_lists_are_bounded_and_say_how_many_were_dropped() {
        let many: Vec<String> = (0..MAX_DIAGNOSIS_CANDIDATES + 5)
            .map(|i| format!("button \"{i}\""))
            .collect();
        let d = crate::Diagnosis::new().candidates(many);
        let encoded = diagnosis_json(&d);
        assert_eq!(
            encoded["candidates"].as_array().unwrap().len(),
            MAX_DIAGNOSIS_CANDIDATES
        );
        assert_eq!(encoded["candidates_omitted"], 5);
    }

    #[test]
    fn an_ambiguous_selector_names_every_candidate_and_the_way_out() {
        let mut a = crate::ElementData::for_role(crate::Role::RadioButton);
        a.name = Some("Option A".into());
        a.states.checked = Some(crate::Toggled::On);
        let mut b = crate::ElementData::for_role(crate::Role::RadioButton);
        b.name = Some("Option B".into());
        b.states.checked = Some(crate::Toggled::Off);

        let err = CliError::Ambiguous {
            count: 2,
            diagnosis: Box::new(
                crate::Diagnosis::new()
                    .selector("radio_button")
                    .last_observed("selector matched 2 elements")
                    .candidates(vec![describe_candidate(&a), describe_candidate(&b)]),
            ),
        };
        let (text, structured) = describe_failure("action", &err);
        assert_eq!(structured["kind"], "ambiguous_selector");
        assert_eq!(structured["match_count"], 2);
        assert_eq!(structured["diagnosis"]["selector"], "radio_button");
        let candidates = structured["diagnosis"]["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        // The state that says which one is already selected has to be in the
        // list, or "press the other one" is still a guess.
        assert!(candidates[0].as_str().unwrap().contains("checked=on"));
        assert!(candidates[1].as_str().unwrap().contains("checked=off"));
        // The recovery must be readable off the message alone, for clients on
        // revisions with no structuredContent.
        assert!(text.contains("Option B"), "{text}");
        assert!(text.contains(":nth(n)"), "{text}");
        assert!(text.contains("[name="), "{text}");
    }

    #[test]
    fn a_candidate_line_carries_what_tells_siblings_apart_and_nothing_else() {
        let mut data = crate::ElementData::for_role(crate::Role::Button);
        data.name = Some("Save".into());
        data.stable_id = Some("/org/a11y/atspi/accessible/0/12345678901234567890".into());
        data.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 100,
            height: 40,
        });
        let line = describe_candidate(&data);
        assert_eq!(line, "button \"Save\" [at (60,40)]");
        assert!(
            !line.contains("atspi"),
            "the platform id is 60 characters of noise: {line}"
        );
    }

    #[test]
    fn a_long_candidate_name_is_cut_rather_than_carried_whole() {
        let mut data = crate::ElementData::for_role(crate::Role::StaticText);
        data.name = Some("x".repeat(500));
        let line = describe_candidate(&data);
        assert!(line.chars().count() < 100, "{}", line.chars().count());
        assert!(line.contains('…'), "the cut must be visible: {line}");
    }

    #[test]
    fn a_selector_miss_reports_the_selector_as_a_field_not_only_in_prose() {
        // A harness should be able to branch on the selector without parsing
        // the message. Core keeps it on the error, not in the diagnosis.
        let err = CliError::Xa11y(
            crate::Error::selector_not_matched("button[name=\"Sbumit\"]").diagnose(
                crate::Diagnosis::new().candidates(vec!["button \"Submit\"".to_string()]),
            ),
        );
        let (_, structured) = describe_failure("find", &err);
        assert_eq!(structured["kind"], "no_match");
        assert_eq!(
            structured["diagnosis"]["selector"],
            "button[name=\"Sbumit\"]"
        );
        assert_eq!(
            structured["diagnosis"]["candidates"][0],
            "button \"Submit\""
        );
    }

    #[test]
    fn a_diagnosis_that_names_its_own_selector_is_not_overwritten() {
        let err = CliError::Xa11y(
            crate::Error::selector_not_matched("outer")
                .diagnose(crate::Diagnosis::new().selector("inner")),
        );
        let (_, structured) = describe_failure("find", &err);
        assert_eq!(structured["diagnosis"]["selector"], "inner");
    }

    #[test]
    fn the_missing_target_error_names_the_tool_arguments_not_cli_flags() {
        // `resolve_app`'s own message says "--app NAME or --pid PID", which
        // are flags no MCP caller can pass.
        let err = target(&args(json!({}))).expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("\"app\""), "{msg}");
        assert!(msg.contains("\"pid\""), "{msg}");
        assert!(msg.contains("\"shell\""), "the third target must be named");
        assert!(!msg.contains("--"), "no CLI flags on this surface: {msg}");
    }

    // ── Shell surfaces ──────────────────────────────────────────────────────

    #[test]
    fn app_and_shell_together_are_refused_before_anything_is_enumerated() {
        // Two different things to search: picking one silently would act on a
        // target the caller did not name.
        let err = target(&args(json!({ "app": "Safari", "shell": "menu_bar" })))
            .expect_err("two targets is not a target");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("\"app\"") && msg.contains("\"shell\""),
            "{msg}"
        );
        assert!(!msg.contains("--"), "no CLI flags on this surface: {msg}");
    }

    #[test]
    fn an_unknown_shell_kind_is_a_fixable_argument_error_naming_the_kinds() {
        // Parsed before the shell is enumerated, which is also what makes this
        // testable with no display.
        let err = target(&args(json!({ "shell": "task_bar" }))).expect_err("must reject");
        assert_eq!(failure_kind(&err), "invalid_arguments");
        let msg = err.to_string();
        assert!(msg.contains("task_bar"), "must echo the bad value: {msg}");
        for name in cli::shell_kind_names() {
            assert!(msg.contains(name), "{name} must be offered: {msg}");
        }
    }

    #[test]
    fn the_shell_argument_advertises_exactly_the_kinds_the_parser_accepts() {
        // A kind in the schema's enum that the parser rejects reads to a model
        // as an arbitrary refusal; one the parser accepts but the schema omits
        // is unreachable for a client that validates before sending.
        for name in ["tree", "find", "action"] {
            let def = tool_definition(name);
            let listed = def["inputSchema"]["properties"]["shell"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} must offer the shell argument"));
            assert_eq!(listed.len(), cli::shell_kind_names().len(), "{name}");
            for kind in cli::shell_kind_names() {
                assert!(listed.iter().any(|v| v == kind), "{name} omits {kind}");
                cli::parse_shell_kind(kind)
                    .unwrap_or_else(|e| panic!("{name} advertises {kind}, which fails: {e}"));
            }
        }
    }

    #[test]
    fn the_shell_argument_says_it_cannot_be_combined_with_app() {
        for name in ["tree", "find", "action"] {
            let description = tool_definition(name)["inputSchema"]["properties"]["shell"]
                ["description"]
                .as_str()
                .expect("shell description")
                .to_string();
            assert!(
                description.contains("Mutually exclusive with `app`"),
                "{name}: {description}"
            );
            assert!(
                description.contains("ambiguous_shell_surface"),
                "{name} must name the failure a second surface produces: {description}"
            );
        }
    }

    #[test]
    fn the_shell_tool_states_the_contract_a_caller_would_otherwise_discover_by_pressing() {
        let description = tool_definition("shell")["description"]
            .as_str()
            .expect("shell description")
            .to_string();
        // The mutation model is the part an agent cannot infer: enumeration is
        // inert, and hidden tray icons only exist after *it* presses something.
        assert!(description.contains("listing is live"), "{description}");
        assert!(
            description.contains("only while it is open"),
            "a flyout is not a permanent surface: {description}"
        );
        assert!(
            description.contains("never opens or presses anything"),
            "enumeration must be stated as inert: {description}"
        );
        // The Windows overflow workflow, spelled out: an agent should not have
        // to discover the mutation model by experiment.
        assert!(description.contains("Show Hidden Icons"), "{description}");
        assert!(description.contains("flyout"), "{description}");
    }

    #[test]
    fn the_shell_tool_takes_no_arguments() {
        let def = tool_definition("shell");
        assert_eq!(def["inputSchema"]["type"], "object");
        assert_eq!(def["inputSchema"]["additionalProperties"], false);
        assert!(def["inputSchema"].get("properties").is_none());
    }

    #[test]
    fn an_ambiguous_shell_surface_is_its_own_failure_kind_with_the_candidates() {
        // The same call `action` makes for `ambiguous_selector`: refuse, and
        // hand back the list that makes the retry a choice rather than a guess.
        let err = CliError::AmbiguousShellSurface {
            count: 2,
            kind: "panel".into(),
            diagnosis: Box::new(
                crate::Diagnosis::new()
                    .condition("exactly one panel shell surface")
                    .last_observed("2 panel surfaces are present")
                    .candidates(vec![
                        "panel \"Top\" (pid=101)".into(),
                        "panel \"Dock\" (pid=102)".into(),
                    ]),
            ),
        };
        let (text, structured) = describe_failure("find", &err);
        assert_eq!(structured["kind"], "ambiguous_shell_surface");
        assert_eq!(structured["match_count"], 2);
        assert_eq!(structured["shell_kind"], "panel");
        let candidates = structured["diagnosis"]["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        // Readable off the message alone, for clients on a revision without
        // structuredContent.
        assert!(text.contains("pid=102"), "{text}");
        assert!(text.contains("pid"), "{text}");
    }

    #[test]
    fn the_action_schema_documents_the_verbs_that_need_a_value() {
        let def = tool_definition("action");
        let action = def["inputSchema"]["properties"]["action"]["description"]
            .as_str()
            .expect("action description");
        for verb in ACTIONS_REQUIRING_VALUE {
            assert!(action.contains(verb), "{verb} not named: {action}");
        }
        let value = def["inputSchema"]["properties"]["value"]["description"]
            .as_str()
            .expect("value description");
        assert!(
            value.contains("set-numeric-value"),
            "the numeric verb needs its value format spelled out: {value}"
        );
    }

    #[test]
    fn the_action_description_states_the_contract_a_caller_would_otherwise_guess() {
        let description = tool_definition("action")["description"]
            .as_str()
            .expect("action description")
            .to_string();
        // Each of these was a wrong assumption a real agent made.
        assert!(description.contains("exactly one"), "{description}");
        assert!(description.contains("Auto-waits"), "{description}");
        assert!(
            description.contains(crate::DEFAULT_TIMEOUT_ENV_VAR),
            "the timeout has to be nameable: {description}"
        );
        assert!(
            description.contains("not that anything changed"),
            "{description}"
        );
    }

    // ── Window management ───────────────────────────────────────────────────

    /// One mock-backed app target plus the provider handle, so a test can
    /// inspect exactly what reached the provider.
    fn mock_target_app() -> (Target, std::sync::Arc<xa11y_core::mock::MockProvider>) {
        let provider = xa11y_core::mock::build_provider();
        // Coerce to the trait object explicitly: `App::list_with` takes
        // `Arc<dyn Provider>`, and `Arc::clone` would otherwise infer its
        // target from the parameter and reject the concrete handle.
        let trait_provider: std::sync::Arc<dyn xa11y_core::Provider> = provider.clone();
        let app = App::list_with(trait_provider)
            .expect("the mock must list its app")
            .into_iter()
            .next()
            .expect("the mock must vend one app");
        (Target::App(app), provider)
    }

    #[test]
    fn windows_tool_lists_the_mock_windows_with_an_honest_bounded_response() {
        let (target, _provider) = mock_target_app();
        let Target::App(app) = target else {
            panic!("the mock target must be an app");
        };
        let apps = vec![app];
        let out = tool_windows_with(
            &apps,
            FIND_DEFAULT_LIMIT,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        )
        .expect("the windows listing must succeed");
        let structured = out.structured.expect("windows results are structured");
        assert_eq!(structured["count"], json!(1), "{structured}");
        assert_eq!(structured["returned"], json!(1), "{structured}");
        assert_eq!(structured["truncated"], json!(false), "{structured}");
        assert_eq!(structured["errors"], json!([]), "{structured}");
        // The mock app's single window must be the one listed.
        let listed = structured["windows"]
            .as_array()
            .expect("windows is an array")
            .clone();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["name"], json!("Main Window"), "{structured}");
        // Bounding: a limit below the count truncates and says so.
        let truncated =
            tool_windows_with(&apps, 0, &cli::PointerCenter::Usable, &RefTable::default())
                .expect("a zero-limit listing is still a valid call");
        let structured = truncated.structured.expect("structured");
        assert_eq!(structured["count"], json!(1), "{structured}");
        assert_eq!(structured["returned"], json!(0), "{structured}");
        assert_eq!(structured["truncated"], json!(true), "{structured}");
    }

    #[test]
    fn windows_tool_surfaces_non_application_entries_in_errors() {
        // The app-node unification made window/dialog entry shapes impossible
        // from `App::list` — and it also took away the per-pid dedup that
        // existed only to collapse those window-like entries. A forged
        // window entry passed straight at the listing is therefore surfaced
        // in `errors` (`ActionNotSupported`, role named) rather than
        // silently reporting "no windows" or being quietly deduplicated
        // away: the partial result stays honest (tenet 1).
        let provider = xa11y_core::mock::build_provider();
        let trait_provider: std::sync::Arc<dyn xa11y_core::Provider> = provider.clone();
        let mut main = App::list_with(trait_provider)
            .expect("mock must list its app")
            .into_iter()
            .next()
            .expect("mock must vend one app");
        main.data.role = crate::Role::Window;
        let out = tool_windows_with(
            &[main],
            FIND_DEFAULT_LIMIT,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        )
        .expect("the listing must succeed");
        let structured = out.structured.expect("structured");
        assert_eq!(structured["count"], json!(0), "{structured}");
        let errors = structured["errors"].as_array().expect("errors is an array");
        assert!(
            !errors.is_empty(),
            "a non-application entry must surface in errors"
        );
        assert!(
            errors[0]
                .as_str()
                .expect("error renderings are strings")
                .contains("ActionNotSupported")
                || errors[0]
                    .as_str()
                    .expect("error renderings are strings")
                    .contains("not supported"),
            "the error must name the refused action, got: {}",
            errors[0]
        );
    }

    #[test]
    fn windows_tool_deduplicates_overlapping_entries_of_one_pid() {
        // Linux multi-registration: one process surfaces several Application
        // entries whose windows() overlap. Two entries backed by the same
        // provider are exactly that shape — same pid, same windows() result —
        // and the dedup must collapse them before the limit so a duplicate
        // can neither appear in the result nor burn a slot.
        let provider = xa11y_core::mock::build_provider();
        let trait_provider: std::sync::Arc<dyn xa11y_core::Provider> = provider.clone();
        let first = App::list_with(trait_provider.clone())
            .expect("mock must list its app")
            .into_iter()
            .next()
            .expect("mock must vend one app");
        let pid = first.pid.expect("mock app must carry a pid");
        let second = App::by_pid_with(trait_provider, pid, std::time::Duration::ZERO)
            .expect("mock must resolve its app by pid");
        assert_eq!(first.pid, second.pid, "both entries must share the pid");

        let out = tool_windows_with(
            &[first, second],
            FIND_DEFAULT_LIMIT,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        )
        .expect("the listing must succeed");
        let structured = out.structured.expect("windows results are structured");
        let windows = structured["windows"]
            .as_array()
            .expect("windows is an array");
        assert_eq!(structured["count"], json!(1), "{structured}");
        assert_eq!(windows.len(), 1, "{structured}");
        assert_eq!(structured["truncated"], json!(false), "{structured}");
        assert_eq!(structured["errors"], json!([]), "{structured}");
    }

    #[test]
    fn windows_tool_schema_has_no_shell_property() {
        // `windows` lists application windows and does not implement shell
        // targets; advertising `shell` and then ignoring it would let a
        // model's `{"shell": "taskbar"}` silently widen into "all windows".
        let props = tool_definition("windows")["inputSchema"]["properties"].clone();
        assert!(
            props.get("shell").is_none(),
            "the windows schema must not advertise shell: {props}"
        );
        assert!(props.get("app").is_some(), "app must be available: {props}");
        assert!(props.get("pid").is_some(), "pid must be available: {props}");
        assert!(
            props.get("limit").is_some(),
            "limit must be available: {props}"
        );
    }

    #[test]
    fn windows_tool_rejects_a_shell_target() {
        // A client that sends `shell` anyway (MCP input schemas are not
        // always enforced) gets a refusal naming the flag, not the
        // unfiltered all-apps listing.
        let err = tool_windows(&json!({ "shell": "taskbar" }), &RefTable::default())
            .expect_err("a shell target must be refused by `windows`");
        assert!(err.to_string().contains("shell"), "{err}");
    }

    #[test]
    fn enable_web_accessibility_tool_takes_an_application_only() {
        let schema = tool_definition("enable_web_accessibility")["inputSchema"].clone();
        let props = &schema["properties"];
        assert!(props.get("shell").is_none(), "{schema}");
        assert!(
            props.get("app").is_some() && props.get("pid").is_some(),
            "{schema}"
        );
        let err = tool_enable_web_accessibility(&json!({ "shell": "taskbar" }))
            .expect_err("a shell target must be refused");
        assert!(err.to_string().contains("shell"), "{err}");
        let err =
            tool_enable_web_accessibility(&json!({})).expect_err("no target is a usage error");
        assert!(matches!(err, CliError::Usage(_)), "{err}");
    }

    #[test]
    fn action_geometry_rejects_malformed_at_during_parse_before_the_provider_is_touched() {
        // `parse_action_args` runs before the target is resolved (parse
        // arguments before the first OS call) — a malformed `at` is a usage
        // error, and no verb reaches the provider.
        let (_target, provider) = mock_target_app();
        let err = cli::parse_action_args(
            "move-to",
            None,
            None,
            Some("12,not-a-number"),
            None,
            &cli::CLI_ACTION_ARG_NAMES,
        )
        .expect_err("malformed --at must be refused");
        assert!(err.to_string().contains("--at"), "{err}");
        assert!(
            provider.actions().is_empty(),
            "no action may reach the provider: {:?}",
            provider.actions()
        );
        // The same ordering at the tool boundary: a malformed payload is
        // refused before target resolution.
        let tool_err = tool_action(
            &json!({
                "action": "move-to",
                "selector": "window",
                "at": "12,not-a-number",
            }),
            &RefTable::default(),
        )
        .expect_err("malformed at must be refused");
        // Named in the tool's own spelling: there are no flags on this surface.
        assert!(tool_err.to_string().contains("in \"at\""), "{tool_err}");
        assert!(!tool_err.to_string().contains("--"), "{tool_err}");
    }

    #[test]
    fn action_geometry_reaches_the_window_verb_with_valid_arguments() {
        let (target, provider) = mock_target_app();
        let move_args = cli::parse_action_args(
            "move-to",
            None,
            None,
            Some("10,20"),
            None,
            &cli::CLI_ACTION_ARG_NAMES,
        )
        .expect("a valid move-to must parse");
        tool_action_parsed(&target, "move-to", "window", &move_args)
            .expect("a valid move-to must succeed");
        let resize_args = cli::parse_action_args(
            "resize-to",
            None,
            None,
            None,
            Some("800,600"),
            &cli::CLI_ACTION_ARG_NAMES,
        )
        .expect("a valid resize-to must parse");
        tool_action_parsed(&target, "resize-to", "window", &resize_args)
            .expect("a valid resize-to must succeed");
        let actions = provider.actions();
        assert!(
            actions
                .iter()
                .any(|(_, name, data)| name == "move_to" && data.as_deref() == Some("10,20")),
            "the dispatching must reach the move_to verb with the parsed point: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .any(|(_, name, data)| name == "resize_to" && data.as_deref() == Some("800x600")),
            "the dispatching must reach the resize_to verb with the parsed size: {actions:?}"
        );
    }

    #[test]
    fn stable_find_returns_the_settled_bounds_of_the_one_match() {
        let (target, provider) = mock_target_app();
        let settled = xa11y_core::Rect {
            x: 300,
            y: 60,
            width: 50,
            height: 30,
        };
        let moving = |x| xa11y_core::Rect { x, ..settled };
        provider.animate(
            "Back",
            vec![
                (Some(moving(100)), true),
                (Some(moving(200)), true),
                (Some(settled), true),
            ],
        );
        let element = cli::find_stable(&target, r#"button[name="Back"]"#)
            .expect("the wait succeeds")
            .expect("one match");
        assert_eq!(element.bounds, Some(settled));
        let out = find_output(
            r#"button[name="Back"]"#,
            &target,
            &[element],
            1,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        );
        let structured = out.structured.expect("find results are structured");
        assert_eq!(structured["match_count"], 1);
        assert_eq!(structured["matches"][0]["bounds"]["x"], 300);
    }

    #[test]
    fn stable_find_refuses_several_matches_with_the_candidates() {
        let (target, _provider) = mock_target_app();
        let matches = cli::find_stable(&target, "button")
            .expect("no wait error")
            .expect_err("two buttons match");
        let err = ambiguous("button", &matches);
        let CliError::Ambiguous { count, diagnosis } = err else {
            panic!("expected ambiguous_selector, got {err:?}");
        };
        assert_eq!(count, 2);
        assert!(
            diagnosis.candidates.iter().any(|c| c.contains("Back")),
            "{:?}",
            diagnosis.candidates
        );
    }

    #[test]
    fn find_refuses_a_non_boolean_stable_before_resolving_a_target() {
        let err = tool_find(
            &json!({ "selector": "button", "stable": "yes" }),
            &RefTable::default(),
        )
        .expect_err("a string is not a boolean");
        assert!(
            err.to_string().contains("\"stable\" must be a boolean"),
            "{err}"
        );
    }

    #[test]
    fn action_refuses_several_matches_and_touches_none_of_them() {
        let (target, provider) = mock_target_app();
        let err = tool_action_parsed(&target, "press", "button", &cli::ActionArgs::default())
            .expect_err("two buttons match");
        let CliError::Ambiguous { count, .. } = err else {
            panic!("expected ambiguous_selector, got {err:?}");
        };
        assert_eq!(count, 2);
        assert!(provider.actions().is_empty(), "{:?}", provider.actions());
    }

    /// A mock whose UI changes after the first tree walk once armed: from the
    /// second walk on, an intruder "Back" button sits ahead of the real one.
    /// Counting in one resolution and acting in another presses the intruder.
    struct ShiftingProvider {
        inner: std::sync::Arc<xa11y_core::mock::MockProvider>,
        armed: std::sync::atomic::AtomicBool,
        walks: std::sync::atomic::AtomicUsize,
        /// When set, `press` fails the way a platform call that was sent and
        /// not answered does.
        press_fails: std::sync::atomic::AtomicBool,
    }

    const INTRUDER_HANDLE: u64 = 9_999;

    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
            $(fn $name(&self, e: &crate::ElementData $(, $arg: $ty)*) -> crate::Result<$ret> {
                self.inner.$name(e $(, $arg)*)
            })*
        };
    }

    impl crate::Provider for ShiftingProvider {
        fn get_children(
            &self,
            element: Option<&crate::ElementData>,
        ) -> crate::Result<Vec<crate::ElementData>> {
            use std::sync::atomic::Ordering::SeqCst;
            let mut children = self.inner.get_children(element)?;
            let armed = self.armed.load(SeqCst);
            if armed && element.is_some_and(|e| e.role == crate::Role::Application) {
                self.walks.fetch_add(1, SeqCst);
            }
            let shifted = armed && self.walks.load(SeqCst) > 1;
            if shifted && element.is_some_and(|e| e.name.as_deref() == Some("Navigation")) {
                let mut intruder = children[0].clone();
                intruder.handle = INTRUDER_HANDLE;
                children.insert(0, intruder);
            }
            Ok(children)
        }
        fn get_parent(&self, e: &crate::ElementData) -> crate::Result<Option<crate::ElementData>> {
            self.inner.get_parent(e)
        }
        fn list_apps(&self) -> crate::Result<Vec<crate::ElementData>> {
            self.inner.list_apps()
        }
        fn focused_app(&self) -> crate::Result<crate::ElementData> {
            self.inner.focused_app()
        }
        fn list_shell_surfaces(
            &self,
        ) -> crate::Result<Vec<(crate::ShellSurfaceKind, crate::ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        fn press(&self, e: &crate::ElementData) -> crate::Result<()> {
            if self.press_fails.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::Error::Platform {
                    code: -25204,
                    message: "AXPress did not complete".into(),
                });
            }
            self.inner.press(e)
        }
        forward! {
            focus() -> (); blur() -> (); toggle() -> (); select() -> ();
            expand() -> (); collapse() -> (); show_menu() -> (); increment() -> ();
            decrement() -> (); scroll_into_view() -> (); set_value(v: &str) -> ();
            set_numeric_value(v: f64) -> (); type_text(t: &str) -> ();
            set_text_selection(s: u32, f: u32) -> (); perform_action(a: &str) -> ();
            activate() -> (); minimize() -> (); maximize() -> (); enter_fullscreen() -> ();
            restore() -> (); close() -> (); move_to(x: i32, y: i32) -> ();
            resize_to(w: u32, h: u32) -> ();
            subscribe() -> xa11y_core::Subscription;
        }
    }

    #[test]
    fn action_acts_on_the_element_it_counted_even_when_the_ui_shifts() {
        use std::sync::atomic::Ordering::SeqCst;
        let inner = xa11y_core::mock::build_provider();
        let shifting = std::sync::Arc::new(ShiftingProvider {
            inner: inner.clone(),
            armed: std::sync::atomic::AtomicBool::new(false),
            walks: std::sync::atomic::AtomicUsize::new(0),
            press_fails: std::sync::atomic::AtomicBool::new(false),
        });
        let provider: std::sync::Arc<dyn crate::Provider> = shifting.clone();
        let app = App::list_with(provider)
            .expect("the mock lists its app")
            .into_iter()
            .next()
            .expect("one app");
        let target = Target::App(app);
        let selector = r#"button[name="Back"]"#;
        let real = target.locator(selector).elements().expect("resolve");
        assert_eq!(real.len(), 1);

        shifting.armed.store(true, SeqCst);
        tool_action_parsed(&target, "press", selector, &cli::ActionArgs::default())
            .expect("one actionable match when counted");
        let actions = inner.actions();
        assert_eq!(
            actions,
            vec![(real[0].handle, "press".to_string(), None)],
            "pressed something other than the element that was counted: {actions:?}"
        );
        // The hazard was live: a later resolution sees the intruder too.
        assert_eq!(target.locator(selector).count().expect("count"), 2);
    }

    #[test]
    fn a_dispatched_action_that_fails_reports_unknown_end_to_end() {
        // The `unknown` side through the real tool path: the element resolves,
        // the platform call is sent, and it errors. The wire result must tell
        // the caller a retry may double-apply.
        let shifting = std::sync::Arc::new(ShiftingProvider {
            inner: xa11y_core::mock::build_provider(),
            armed: std::sync::atomic::AtomicBool::new(false),
            walks: std::sync::atomic::AtomicUsize::new(0),
            press_fails: std::sync::atomic::AtomicBool::new(true),
        });
        let provider: std::sync::Arc<dyn crate::Provider> = shifting;
        let app = App::list_with(provider)
            .expect("the mock lists its app")
            .into_iter()
            .next()
            .expect("one app");
        let target = Target::App(app);
        let err = tool_action_parsed(
            &target,
            "press",
            r#"button[name="Back"]"#,
            &cli::ActionArgs::default(),
        )
        .expect_err("the platform call fails");
        let (_, structured) = describe_failure("action", &err);
        assert_eq!(structured["kind"], "platform");
        assert_eq!(structured["delivery"], "unknown");
    }

    #[test]
    fn every_action_verb_has_an_auto_wait_gate() {
        // A verb core does not know would be refused as `InvalidActionData`
        // before any wait; a known one with nothing to match times out.
        let (target, _) = mock_target_app();
        for verb in cli::ACTION_NAMES {
            let err = target
                .locator(r#"button[name="Nope"]"#)
                .with_timeout(std::time::Duration::ZERO)
                .auto_wait_unique(&verb.replace('-', "_"))
                .expect_err("nothing matches");
            assert!(
                matches!(err, crate::Error::Timeout { .. }),
                "{verb}: {err:?}"
            );
        }
    }

    #[test]
    fn an_argument_the_verb_does_not_read_is_refused_in_mcp_spelling() {
        let refs = RefTable::default();
        for (call, expected) in [
            (
                json!({ "action": "press", "value": "x" }),
                "\"value\" applies only to set-value, set-numeric-value, type-text, \
                 select-text, not press",
            ),
            (
                json!({ "action": "activate", "at": "1,2" }),
                "\"at\" applies only to move-to, not activate",
            ),
            (
                json!({ "action": "move-to", "at": "1,2", "size": "3,4" }),
                "\"size\" applies only to resize-to, not move-to",
            ),
        ] {
            let mut call = call;
            call["selector"] = json!("button");
            call["app"] = json!("NoSuchAppAnywhere");
            let err = tool_action(&call, &refs).expect_err("refused");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{call}: {err}");
            assert!(err.to_string().ends_with(expected), "{call}: {err}");
            assert!(
                !err.to_string().contains("--"),
                "no flags on this surface: {err}"
            );
        }
        // The geometry messages name the MCP argument too.
        let err = tool_action(
            &json!({ "action": "move-to", "selector": "window", "app": "NoSuchAppAnywhere" }),
            &refs,
        )
        .expect_err("needs at");
        assert!(err.to_string().contains("requires \"at\" X,Y"), "{err}");
    }

    #[test]
    fn select_text_range_mistakes_are_refused_before_the_target_is_resolved() {
        // `app` names nothing: each mistake must be reported as itself, not
        // masked by the app lookup.
        let refs = RefTable::default();
        for (extra, needle) in [
            (json!({ "start": 1 }), "together"),
            (json!({ "end": 1 }), "together"),
            (json!({ "start": 0, "end": 2, "value": "0,2" }), "not both"),
            (json!({ "start": 5, "end": 2 }), "must not exceed"),
            (json!({}), "requires a range: \"start\" and \"end\""),
            (json!({ "start": -1, "end": 2 }), "between 0 and"),
        ] {
            let mut call = json!({
                "action": "select-text",
                "selector": "text_field",
                "app": "NoSuchAppAnywhere",
            });
            for (k, v) in extra.as_object().expect("object") {
                call[k] = v.clone();
            }
            let err = tool_action(&call, &refs).expect_err(needle);
            assert_eq!(failure_kind(&err), "invalid_arguments", "{call}: {err}");
            assert!(err.to_string().contains(needle), "{call}: {err}");
        }
        let err = tool_action(
            &json!({ "action": "press", "selector": "button", "app": "NoSuchAppAnywhere",
                     "start": 0, "end": 1 }),
            &refs,
        )
        .expect_err("a range on another verb");
        assert!(
            err.to_string()
                .contains("\"start\"/\"end\" applies only to select-text"),
            "{err}"
        );
    }

    #[test]
    fn select_text_start_and_end_reach_the_provider() {
        let provider = xa11y_core::mock::build_table_provider();
        let app = App::list_with(provider.clone() as std::sync::Arc<dyn xa11y_core::Provider>)
            .expect("apps")
            .remove(0);
        let args = cli::parse_action_args(
            "select-text",
            None,
            Some(crate::TextRange { start: 2, end: 6 }),
            None,
            None,
            &MCP_ACTION_ARG_NAMES,
        )
        .expect("parses");
        tool_action_parsed(&Target::App(app), "select-text", NOTES, &args).expect("selects");
        assert!(
            provider
                .actions()
                .iter()
                .any(|(_, name, data)| name == "set_text_selection"
                    && data.as_deref() == Some("2..6")),
            "{:?}",
            provider.actions()
        );
    }

    #[test]
    fn a_tree_node_cap_outside_its_bounds_is_refused() {
        let refs = RefTable::default();
        for bad in [0, -1, TREE_MAX_NODES as i64 + 1] {
            let err = tool_tree(
                &json!({ "app": "NoSuchAppAnywhere", "max_nodes": bad }),
                &refs,
            )
            .expect_err("out of bounds");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{bad}: {err}");
            assert!(
                err.to_string()
                    .contains(&format!("between 1 and {TREE_MAX_NODES}")),
                "{err}"
            );
        }
        let max_nodes = tool_definition("tree")["inputSchema"]["properties"]["max_nodes"].clone();
        assert_eq!(max_nodes["minimum"], json!(1), "{max_nodes}");
        assert_eq!(max_nodes["maximum"], json!(TREE_MAX_NODES), "{max_nodes}");
    }

    #[test]
    fn a_listed_window_carries_a_ref_that_acts_on_it() {
        let (target, provider) = mock_target_app();
        let Target::App(app) = target else {
            panic!("the mock target must be an app");
        };
        let refs = RefTable::default();
        let out = tool_windows_with(
            &[app],
            FIND_DEFAULT_LIMIT,
            &cli::PointerCenter::Usable,
            &refs,
        )
        .expect("listing");
        let structured = out.structured.expect("structured");
        let reference = structured["windows"][0]["ref"]
            .as_str()
            .unwrap_or_else(|| panic!("a window carries a ref: {structured}"))
            .to_string();
        assert_eq!(
            refs.resolve(&reference).expect("resolves").name.as_deref(),
            Some("Main Window")
        );
        let args =
            cli::parse_action_args("activate", None, None, None, None, &MCP_ACTION_ARG_NAMES)
                .expect("args");
        tool_action_on_ref(&refs, "activate", &reference, &args).expect("activates by ref");
        assert!(
            provider
                .actions()
                .iter()
                .any(|(_, name, _)| name == "activate"),
            "{:?}",
            provider.actions()
        );
        // A zero limit lists nothing, so it issues no refs either.
        let refs = RefTable::default();
        let (target, _) = mock_target_app();
        let Target::App(app) = target else {
            panic!("the mock target must be an app");
        };
        let none = tool_windows_with(&[app], 0, &cli::PointerCenter::Usable, &refs)
            .expect("listing")
            .structured
            .expect("structured");
        assert_eq!(none["windows"], json!([]), "{none}");
        assert!(refs.resolve("c1:0").is_err(), "no ref was issued");
    }

    #[test]
    fn the_windows_description_promises_refs_and_events_poll_does_not() {
        let windows = tool_definition("windows")["description"]
            .as_str()
            .expect("description")
            .to_string();
        assert!(windows.contains("carries a `ref`"), "{windows}");
        assert!(!windows.contains("without a `ref`"), "{windows}");
        let poll = tool_definition("events_poll")["description"]
            .as_str()
            .expect("description")
            .to_string();
        assert!(poll.contains("without a `ref`"), "{poll}");
    }

    #[test]
    fn a_reversed_text_range_is_refused_before_the_wait() {
        let err = cli::parse_action_args(
            "select-text",
            Some("5,2"),
            None,
            None,
            None,
            &cli::CLI_ACTION_ARG_NAMES,
        )
        .expect_err("start > end");
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
        cli::parse_action_args(
            "select-text",
            Some("2,2"),
            None,
            None,
            None,
            &cli::CLI_ACTION_ARG_NAMES,
        )
        .expect("empty range");
    }

    #[test]
    fn an_unknown_verb_is_refused_before_anything_is_resolved() {
        let (target, provider) = mock_target_app();
        // A selector that never matches: reaching the auto-wait would spend
        // the default timeout before the verb was ever looked at.
        let start = std::time::Instant::now();
        let err = tool_action_parsed(
            &target,
            "frobnicate",
            r#"button[name="Nope"]"#,
            &cli::ActionArgs::default(),
        )
        .expect_err("not a verb");
        assert!(
            matches!(err, CliError::Usage(ref m) if m.contains("unknown action")),
            "{err:?}"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(provider.actions().is_empty());
    }

    #[test]
    fn the_element_tools_say_what_the_actions_field_is_not() {
        for name in ["tree", "find"] {
            let description = tool_definition(name)["description"]
                .as_str()
                .expect("description")
                .to_string();
            assert!(
                description.contains("neither the set of verbs"),
                "{name} must not let `actions` read as a capability list"
            );
        }
    }

    #[test]
    fn the_selector_examples_the_tools_advertise_actually_parse() {
        // The `find` schema used to advertise `checkbox[checked]`, in which
        // both the role and the syntax are wrong. Every example in a
        // description is a selector a model will copy, so each one is parsed
        // here.
        for tool in ["find", "action"] {
            let text = tool_definition(tool)["inputSchema"]["properties"]["selector"]
                ["description"]
                .as_str()
                .expect("selector description")
                .to_string();
            // Only the "Examples:" paragraph. The syntax paragraph below it
            // quotes fragments and deliberate counter-examples (`[checked]`),
            // which are not selectors anyone should paste.
            let listed = text
                .split("Examples:")
                .nth(1)
                .unwrap_or_else(|| panic!("{tool} lists no examples"))
                .split("\n\n")
                .next()
                .expect("split always yields one part");
            let examples: Vec<&str> = listed.split('`').skip(1).step_by(2).collect();
            assert!(examples.len() >= 4, "{tool}: too few examples to be useful");
            for example in examples {
                crate::SelectorGroup::parse(example)
                    .unwrap_or_else(|e| panic!("{tool} advertises {example:?}, which fails: {e}"));
            }
        }
    }

    #[test]
    fn errors_without_a_diagnosis_omit_the_field() {
        let err = CliError::Xa11y(crate::Error::NoElementBounds);
        let (_, structured) = describe_failure("click", &err);
        assert!(structured.get("diagnosis").is_none());
    }

    #[test]
    fn element_encoding_omits_absent_fields_and_precomputes_center() {
        let mut data = crate::ElementData::for_role(crate::Role::Button);
        data.name = Some("OK".into());
        data.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 100,
            height: 40,
        });
        let encoded = element_data_json(&data, true);
        assert_eq!(encoded["role"], "button");
        assert_eq!(encoded["name"], "OK");
        assert_eq!(encoded["center"]["x"], 60);
        assert_eq!(encoded["center"]["y"], 40);
        assert!(
            encoded.get("value").is_none(),
            "absent fields must be omitted"
        );
        assert!(encoded.get("raw").is_none(), "platform blob must not ship");
        assert!(
            encoded.get("handle").is_none(),
            "opaque handle must not ship"
        );
    }

    // ── `center` where element bounds are not screen coordinates (#573) ─────

    #[test]
    fn element_encoding_withholds_center_when_it_is_not_a_pointer_target() {
        let mut data = crate::ElementData::for_role(crate::Role::Button);
        data.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 100,
            height: 40,
        });
        let encoded = element_data_json(&data, false);
        assert!(encoded.get("center").is_none(), "{encoded}");
        // Bounds stay: they are what the platform reported, and the
        // result-level note says they are not a screen position.
        assert_eq!(encoded["bounds"]["x"], 10);
    }

    #[test]
    fn find_reports_center_and_no_note_where_bounds_are_screen_coordinates() {
        let (target, _provider) = mock_target_app();
        let elements = target
            .locator(r#"button[name="Back"]"#)
            .elements()
            .expect("the mock resolves");
        let out = find_output(
            r#"button[name="Back"]"#,
            &target,
            &elements,
            FIND_DEFAULT_LIMIT,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        );
        let structured = out.structured.expect("find results are structured");
        assert!(
            structured.get(CENTER_UNAVAILABLE_FIELD).is_none(),
            "{structured}"
        );
        let first = &structured["matches"][0];
        assert!(first.get("bounds").is_some(), "{first}");
        assert!(first.get("center").is_some(), "{first}");
    }

    #[test]
    fn find_omits_center_and_says_why_where_bounds_are_not_screen_coordinates() {
        let (target, _provider) = mock_target_app();
        let elements = target
            .locator("button")
            .elements()
            .expect("the mock resolves");
        assert!(!elements.is_empty());
        let policy = cli::PointerCenter::from_capability(Ok(false));
        let out = find_output(
            "button",
            &target,
            &elements,
            FIND_DEFAULT_LIMIT,
            &policy,
            &RefTable::default(),
        );
        let structured = out.structured.expect("find results are structured");
        let reason = structured[CENTER_UNAVAILABLE_FIELD]
            .as_str()
            .expect("the result must say why center is missing");
        assert!(reason.contains("not screen coordinates"), "{reason}");
        for m in structured["matches"].as_array().expect("matches") {
            assert!(m.get("center").is_none(), "{m}");
        }
    }

    #[test]
    fn tree_and_windows_follow_the_same_center_policy() {
        let policy = cli::PointerCenter::from_capability(Ok(false));
        let (target, _provider) = mock_target_app();
        let (tree, _) = tree_json(
            &target,
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            TreeDetail::default(),
            policy.is_usable(),
            &RefTable::default(),
        )
        .expect("the mock tree captures");
        fn no_center(node: &Value) {
            assert!(node.get("center").is_none(), "{node}");
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                children.iter().for_each(no_center);
            }
        }
        no_center(&tree);

        let Target::App(app) = target else {
            panic!("the mock target must be an app");
        };
        let out = tool_windows_with(&[app], FIND_DEFAULT_LIMIT, &policy, &RefTable::default())
            .expect("listing");
        let structured = out.structured.expect("structured");
        assert!(
            structured.get(CENTER_UNAVAILABLE_FIELD).is_some(),
            "{structured}"
        );
        for window in structured["windows"].as_array().expect("windows") {
            assert!(window.get("center").is_none(), "{window}");
        }
    }

    #[test]
    fn descriptions_state_where_center_is_not_a_pointer_target() {
        for name in ["find", "tree"] {
            let description = tool_definition(name)["description"]
                .as_str()
                .expect("a description")
                .to_owned();
            assert!(description.contains(CENTER_NOTE), "{name}: {description}");
        }
        assert!(CENTER_NOTE.contains("`center_unavailable`"));
        assert!(CENTER_NOTE.contains("Wayland"));
        let click = tool_definition("click")["description"]
            .as_str()
            .expect("a description")
            .to_owned();
        assert!(click.contains("Wayland"), "{click}");
        assert!(click.contains("`action`"), "{click}");
    }

    #[test]
    fn states_send_only_what_is_true_plus_the_gating_pair() {
        let data = crate::ElementData::for_role(crate::Role::Button);
        let encoded = element_data_json(&data, true);
        let states = encoded["states"].as_object().unwrap();
        assert!(states.contains_key("enabled"));
        assert!(states.contains_key("visible"));
        assert!(!states.contains_key("focused"), "false states are omitted");
        assert!(!states.contains_key("password"), "false states are omitted");
        assert!(
            !states.contains_key("hidden_reason"),
            "a visible element has no hidden reason"
        );
    }

    #[test]
    fn a_password_field_says_so_in_its_states() {
        let mut data = crate::ElementData::for_role(crate::Role::TextField);
        data.states.password = true;
        let encoded = element_data_json(&data, true);
        assert_eq!(encoded["role"], "text_field");
        assert_eq!(encoded["states"]["password"], true);
    }

    #[test]
    fn a_hidden_element_says_why() {
        let mut data = crate::ElementData::for_role(crate::Role::Button);
        data.states.visible = false;
        data.states.hidden_reason = Some(crate::HiddenReason::ScrolledOut);
        let encoded = element_data_json(&data, true);
        assert_eq!(encoded["states"]["visible"], false);
        assert_eq!(encoded["states"]["hidden_reason"], "scrolled_out");
    }

    #[test]
    fn structured_results_are_mirrored_into_a_text_block() {
        // The oldest revision this server speaks predates structuredContent,
        // so the text mirror is the only copy those clients see.
        let out = ToolOutput::json(json!({ "ok": true }));
        assert_eq!(out.content.len(), 1);
        assert_eq!(out.content[0]["type"], "text");
        assert_eq!(out.content[0]["text"], "{\"ok\":true}");
        assert_eq!(out.structured.unwrap()["ok"], true);
    }
    // ── Text and table ──────────────────────────────────────────────────────

    /// The mock app with the table / list / tree / text-area fixture.
    fn table_target() -> Target {
        let provider: std::sync::Arc<dyn xa11y_core::Provider> =
            xa11y_core::mock::build_table_provider();
        let app = App::list_with(provider)
            .expect("the mock must list its app")
            .into_iter()
            .next()
            .expect("the mock must vend one app");
        Target::App(app)
    }

    const NOTES: &str = r#"text_area[name="Notes"]"#;

    // ── Document semantics and relations (#476) ─────────────────────────────

    fn document_target() -> Target {
        let provider: std::sync::Arc<dyn xa11y_core::Provider> =
            xa11y_core::mock::build_document_provider();
        let app = App::list_with(provider)
            .expect("the mock must list its app")
            .into_iter()
            .next()
            .expect("the mock must vend one app");
        Target::App(app)
    }

    #[test]
    fn tree_detail_reads_text_and_relations_on_request() {
        let refs = RefTable::default();
        let find = |tree: &Value, name: &str| -> Value {
            fn walk(node: &Value, name: &str) -> Option<Value> {
                if node["name"] == json!(name) {
                    return Some(node.clone());
                }
                node["children"]
                    .as_array()?
                    .iter()
                    .find_map(|c| walk(c, name))
            }
            walk(tree, name).unwrap_or_else(|| panic!("no {name} in {tree}"))
        };
        let (plain, _) = tree_json(
            &document_target(),
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            TreeDetail::default(),
            true,
            &refs,
        )
        .expect("tree");
        let field = find(&plain, "Email");
        assert!(field.get("relations").is_none(), "{field}");
        assert!(field.get("text").is_none(), "{field}");

        let (rich, _) = tree_json(
            &document_target(),
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            TreeDetail {
                text: true,
                formatting: false,
                relations: true,
            },
            true,
            &refs,
        )
        .expect("tree");
        let field = find(&rich, "Email");
        let label = find(&rich, "Email address");
        assert_eq!(
            field["relations"]["labelled_by"],
            json!([{ "ref": label["ref"] }])
        );
        // Kinds with no targets are left out of the map.
        assert!(field["relations"].get("controls").is_none(), "{field}");

        let (notes, _) = tree_json(
            &table_target(),
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            TreeDetail {
                text: true,
                formatting: false,
                relations: false,
            },
            true,
            &refs,
        )
        .expect("tree");
        let notes = find(&notes, "Notes");
        assert_eq!(notes["text"]["content"], "Hello 😀 world\nsecond line");
        assert_eq!(notes["text"]["length"], 26);
        assert_eq!(notes["text"]["truncated"], false);
        assert!(notes["text"].get("runs").is_none(), "{notes}");

        let formatting = tree_detail(&args(json!({ "detail": ["formatting"] }))).expect("parses");
        assert!(formatting.text && formatting.formatting);
        let (styled, _) = tree_json(
            &table_target(),
            TREE_DEFAULT_MAX_DEPTH,
            TREE_MAX_NODES,
            formatting,
            true,
            &refs,
        )
        .expect("tree");
        let notes = find(&styled, "Notes");
        let first = &notes["text"]["runs"][0];
        assert_eq!(first["start"], 0);
        assert_eq!(first["attributes"]["font_weight"], 700);
        assert_eq!(notes["text"]["runs_truncated"], false);
    }

    #[test]
    fn an_unknown_tree_detail_is_refused() {
        for detail in [json!(["styles"]), json!("text"), json!([1])] {
            let err = tree_detail(&args(json!({ "detail": detail }))).expect_err("refused");
            assert_eq!(failure_kind(&err), "invalid_arguments", "{detail}");
        }
        assert!(tree_detail(&json!({})).is_ok());
    }

    #[test]
    fn element_nodes_carry_document_semantics_only_when_reported() {
        let target = document_target();
        let one = |sel: &str| {
            let els = target.locator(sel).elements().expect("resolves");
            assert_eq!(els.len(), 1, "{sel}");
            element_data_json(&els[0], true)
        };
        let h1 = one(r#"heading[heading_level="1"]"#);
        assert_eq!(h1["heading_level"], 1);
        let main = one(r#"landmark[landmark_type="main"]"#);
        assert_eq!(main["landmark_type"], "main");
        assert_eq!(main["web_role"], "main");
        assert_eq!(
            one(r#"link[name="Docs"]"#)["url"],
            "https://example.com/docs"
        );
        assert_eq!(
            one(r#"status[name="Saved"]"#)["live_region"],
            json!({ "politeness": "polite", "atomic": true, "relevant": "additions text" })
        );
        let field = one(r#"text_field[name="Email"]"#);
        for key in [
            "heading_level",
            "landmark_type",
            "web_role",
            "url",
            "live_region",
        ] {
            assert!(field.get(key).is_none(), "{key} must be absent: {field}");
        }
    }

    #[test]
    fn relations_lists_each_kind_once_with_refs() {
        let refs = RefTable::default();
        let kinds = [
            crate::RelationKind::LabelledBy,
            crate::RelationKind::Controls,
        ];
        let out = tool_relations_parsed(
            &document_target(),
            r#"text_field[name="Email"]"#,
            &kinds,
            FIND_DEFAULT_LIMIT,
            &refs,
        )
        .expect("relations read");
        let got = out.structured.expect("structured");
        assert_eq!(got["element"]["name"], "Email");
        let relations = got["relations"].as_array().expect("a list");
        assert_eq!(relations.len(), 2, "{got}");
        assert_eq!(relations[0]["kind"], "labelled_by");
        assert_eq!(relations[0]["count"], 1);
        assert_eq!(relations[0]["elements"][0]["name"], "Email address");
        // A supported kind with no targets is an empty list, not absent.
        assert_eq!(relations[1]["kind"], "controls");
        assert_eq!(relations[1]["elements"], json!([]));
        assert_eq!(relations[1]["truncated"], false);
        // The target's ref acts on exactly the label.
        let label_ref = relations[0]["elements"][0]["ref"].as_str().expect("a ref");
        let label = refs.resolve(label_ref).expect("the ref resolves");
        assert_eq!(label.name.as_deref(), Some("Email address"));
    }

    #[test]
    fn relations_refuses_bad_kinds_before_resolving() {
        let refs = RefTable::default();
        for kinds in [
            json!(["labeled_by"]),
            json!([]),
            json!("labelled_by"),
            json!([1]),
        ] {
            let err = tool_relations(
                &args(json!({ "selector": "button", "app": "nope", "kinds": kinds })),
                &refs,
            )
            .expect_err("refused");
            assert!(matches!(err, CliError::Usage(_)), "{kinds}: {err}");
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))] // `linked` is reported on macOS
    fn relations_of_an_unreported_kind_is_unsupported() {
        let err = tool_relations_parsed(
            &document_target(),
            r#"text_field[name="Email"]"#,
            &[crate::RelationKind::Linked],
            FIND_DEFAULT_LIMIT,
            &RefTable::default(),
        )
        .expect_err("the mock does not report linked");
        assert!(
            matches!(err, CliError::Xa11y(crate::Error::Unsupported { .. })),
            "{err}"
        );
    }

    #[test]
    fn text_object_mapping_reports_unsupported_and_checks_arguments() {
        let error = text_err(json!({"op":"object-at", "offset":0}));
        assert!(matches!(
            error,
            CliError::Xa11y(crate::Error::Unsupported { .. })
        ));
        assert!(matches!(
            text_op_from_args(&json!({"op":"object-at"})).expect_err("missing offset"),
            CliError::Usage(_)
        ));
        assert!(matches!(
            text_op_from_args(&json!({"op":"object-at", "offset":0, "unit":"word"}))
                .expect_err("extraneous unit"),
            CliError::Usage(_)
        ));
    }

    /// Run a `text` call against the fixture and return its `result`.
    fn text_result(call: Value) -> Value {
        let op = text_op_from_args(&call).expect("valid text arguments");
        let out = tool_text_parsed(&table_target(), NOTES, &op, &RefTable::default())
            .expect("text call succeeds");
        let structured = out.structured.expect("structured result");
        assert_eq!(structured["units"], "utf16");
        structured["result"].clone()
    }

    fn text_err(call: Value) -> CliError {
        let op = text_op_from_args(&call).expect("valid text arguments");
        tool_text_parsed(&table_target(), NOTES, &op, &RefTable::default())
            .expect_err("text call fails")
    }

    #[test]
    fn element_nodes_carry_reported_table_and_collection_positions() {
        let target = table_target();
        let one = |sel: &str| {
            let els = target.locator(sel).elements().expect("resolves");
            assert_eq!(els.len(), 1, "{sel}");
            element_data_json(&els[0], true)
        };
        let price = one(r#"table_cell[name="Price"]"#);
        assert_eq!(
            price["cell"],
            json!({ "row": 0, "column": 1, "row_span": 1, "column_span": 2, "header": "column" })
        );
        // An unreported span is absent, not defaulted to 1.
        let three = one(r#"table_cell[name="3.00"]"#);
        assert_eq!(three["cell"], json!({ "row": 2, "column": 1 }));
        let table = one(r#"table[name="Prices"]"#);
        assert_eq!(table["table"], json!({ "row_count": 3, "column_count": 3 }));
        let row = one(r#"table_row[row="1"]"#);
        assert_eq!(row["table_row"], json!({ "index": 1 }));
        let first = one(r#"list_item[name="First"]"#);
        assert_eq!(
            first["collection"],
            json!({ "index": 0, "size": "unknown" })
        );
        // An element without any of them carries none of the keys.
        let notes = one(NOTES);
        for key in ["table", "cell", "table_row", "collection"] {
            assert!(notes.get(key).is_none(), "{key} on a text area: {notes}");
        }
    }

    #[test]
    fn text_info_reports_length_caret_and_selections_in_utf16_units() {
        let r = text_result(json!({ "op": "info" }));
        // "Hello 😀 world\nsecond line": the emoji is two units.
        assert_eq!(r["length"], 26);
        assert_eq!(r["caret"], Value::Null);
        assert_eq!(r["selection_count"], 0);
        assert_eq!(r["selections_truncated"], false);
        assert_eq!(r["selections"], json!([]));
    }

    #[test]
    fn a_found_range_carries_its_text_and_exact_offsets() {
        let r = text_result(json!({ "op": "find", "find": "world" }));
        assert_eq!(r["found"], true);
        assert_eq!(
            r["match"],
            json!({ "start": 9, "end": 14, "text": "world" })
        );

        let miss = text_result(json!({ "op": "find", "find": "world", "occurrence": 1 }));
        assert_eq!(miss["found"], false);
        assert_eq!(miss["match"], Value::Null);

        let read = text_result(json!({ "op": "read", "find": "second" }));
        assert_eq!(read["text"], "second");
        assert_eq!(
            (read["start"].clone(), read["end"].clone()),
            (json!(15), json!(21))
        );
        assert_eq!(read["truncated"], false);
    }

    #[test]
    fn find_naming_the_range_of_another_op_fails_with_where_it_looked() {
        let err = text_err(json!({ "op": "bounds", "find": "absent" }));
        assert_eq!(failure_kind(&err), "no_match");
        let msg = err.to_string();
        assert!(
            msg.contains("\"absent\"") && msg.contains("text length 26"),
            "{msg}"
        );
    }

    #[test]
    fn a_read_page_is_cut_before_a_surrogate_pair_and_says_where_to_resume() {
        // A cut at 7 would split the emoji at 6..8, so the page stops at 6.
        let r = text_result(json!({ "op": "read", "limit": 7 }));
        assert_eq!(r["text"], "Hello ");
        assert_eq!(r["end"], 6);
        assert_eq!(r["truncated"], true);
        assert_eq!(r["next_start"], 6);
        assert_eq!(r["requested_end"], 26);
        let next = text_result(json!({ "op": "read", "start": 6, "end": 26, "limit": 7 }));
        assert!(next["text"].as_str().unwrap().starts_with("😀"), "{next}");
    }

    #[test]
    fn range_at_lines_bounds_and_runs_return_ranges_with_text() {
        let word = text_result(json!({ "op": "range-at", "offset": 10, "unit": "word" }));
        assert_eq!(word["unit"], "word");
        // The mock's word boundary keeps the trailing newline; what matters
        // here is that the range's text is the text its offsets cover.
        assert!(
            word["range"]["text"].as_str().unwrap().starts_with("world"),
            "{word}"
        );
        assert_eq!(word["range"]["start"], 9);

        let lines = text_result(json!({ "op": "lines", "limit": 1 }));
        assert_eq!(lines["count"], 1);
        assert_eq!(lines["truncated"], true);
        assert_eq!(lines["lines"][0]["text"], "Hello 😀 world\n");
        let resume = lines["next_start"].as_u64().expect("next_start");
        let rest = text_result(json!({ "op": "lines", "start": resume, "end": 26 }));
        assert_eq!(rest["truncated"], false);
        assert_eq!(rest["next_start"], Value::Null);
        assert_eq!(rest["lines"][0]["text"], "second line");

        let bounds = text_result(json!({ "op": "bounds", "find": "second" }));
        assert_eq!(bounds["range"]["text"], "second");
        assert_eq!(bounds["rects"].as_array().expect("rects").len(), 1);

        let runs = text_result(json!({ "op": "runs", "start": 0, "end": 5 }));
        let run = &runs["runs"][0];
        assert_eq!(run["text"], "Hello");
        assert_eq!(run["attributes"]["font_weight"], 700);
        // Unreported attributes are absent rather than null.
        assert!(run["attributes"].get("underline").is_none(), "{run}");
    }

    #[test]
    fn an_out_of_bounds_range_is_an_invalid_text_range_with_its_fields() {
        let err = text_err(json!({ "op": "read", "start": 0, "end": 100 }));
        assert_eq!(failure_kind(&err), "invalid_text_range");
        assert_eq!(err.delivery(), Some(crate::Delivery::NotDelivered));
        let (_, structured) = describe_failure("text", &err);
        assert_eq!(structured["kind"], "invalid_text_range");
        assert_eq!(
            structured["text_range"],
            json!({ "start": 0, "end": 100, "length": 26 })
        );
    }

    #[test]
    fn text_refuses_ambiguity_and_elements_without_text() {
        let target = table_target();
        let op = cli::TextOp::Info;
        let err = tool_text_parsed(&target, "table_cell", &op, &RefTable::default())
            .expect_err("several cells");
        assert_eq!(failure_kind(&err), "ambiguous_selector");
        let err = tool_text_parsed(
            &target,
            r#"table_cell[name="Price"]"#,
            &op,
            &RefTable::default(),
        )
        .expect_err("a cell with no value has no text interface");
        assert_eq!(failure_kind(&err), "action_not_supported");
    }

    #[test]
    fn text_arguments_an_operation_does_not_use_are_refused() {
        for (call, needle) in [
            (
                json!({ "op": "info", "start": 0, "end": 1 }),
                "does not take \"start\"/\"end\"",
            ),
            (
                json!({ "op": "read", "occurrence": 1 }),
                "\"occurrence\" picks a match",
            ),
            (
                json!({ "op": "read", "offset": 3 }),
                "does not take \"offset\"",
            ),
            (
                json!({ "op": "range-at", "offset": 3 }),
                "requires \"unit\"",
            ),
            (
                json!({ "op": "range-at", "offset": 3, "unit": "sentence" }),
                "unknown text unit",
            ),
            (json!({ "op": "find" }), "requires \"find\""),
            (json!({ "op": "find", "find": "" }), "must not be empty"),
            (
                json!({ "op": "lines", "limit": 5000 }),
                "between 1 and 1000",
            ),
            (
                json!({ "op": "bounds", "limit": 5 }),
                "does not take \"limit\"",
            ),
            (json!({ "op": "read", "start": 3 }), "together"),
            (
                json!({ "op": "read", "start": 5, "end": 3 }),
                "must not exceed",
            ),
            (json!({ "op": "explode" }), "unknown text operation"),
        ] {
            let err = text_op_from_args(&call).expect_err(needle);
            assert_eq!(failure_kind(&err), "invalid_arguments", "{call}");
            assert!(err.to_string().contains(needle), "{call}: {err}");
        }
    }

    fn table_call(
        op: &str,
        selector: &str,
        row: Option<u32>,
        column: Option<u32>,
        limit: usize,
    ) -> Value {
        let parsed =
            cli::parse_table_op(op, row, column, "\"row\"", "\"column\"").expect("valid op");
        tool_table_parsed(
            &table_target(),
            selector,
            op,
            parsed,
            limit,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        )
        .expect("table call succeeds")
        .structured
        .expect("structured")
    }

    const PRICES: &str = r#"table[name="Prices"]"#;

    #[test]
    fn cell_at_returns_the_merged_cell_covering_a_slot() {
        let r = table_call("cell-at", PRICES, Some(0), Some(2), 50);
        assert_eq!(r["found"], true);
        assert_eq!(r["cell"]["name"], "Price");
        let miss = table_call("cell-at", PRICES, Some(9), Some(0), 50);
        assert_eq!(miss["found"], false);
        assert_eq!(miss["cell"], Value::Null);
        let reason = miss["reason"].as_str().expect("reason");
        assert!(reason.contains("3 rows and 3 columns"), "{reason}");
    }

    #[test]
    fn header_and_selection_lookups_list_elements_and_report_truncation() {
        let headers = table_call("column-headers", PRICES, None, None, 50);
        assert_eq!(headers["count"], 2);
        assert_eq!(headers["truncated"], false);
        let cut = table_call("column-headers", PRICES, None, None, 1);
        assert_eq!(
            (cut["count"].clone(), cut["returned"].clone()),
            (json!(2), json!(1))
        );
        assert_eq!(cut["truncated"], true);

        let heads = table_call(
            "column-header-cells",
            r#"table_cell[name="1.00"]"#,
            None,
            None,
            50,
        );
        assert_eq!(heads["elements"][0]["name"], "Price");
        let rows = table_call(
            "row-header-cells",
            r#"table_cell[name="2.00"]"#,
            None,
            None,
            50,
        );
        assert_eq!(rows["elements"][0]["name"], "Widget");
        let selected = table_call("selected-items", PRICES, None, None, 50);
        assert_eq!(selected["elements"][0]["name"], "2.00");
    }

    #[test]
    fn element_with_ref_encodes_like_find_and_starts_a_capture_lazily() {
        // `text object-at` renders its element through this; it used to
        // serialize `ElementData` whole (PascalCase role, the `raw` blob, no
        // ref). No mock provider answers `object_at`, so the encoding is held
        // here.
        let refs = RefTable::default();
        let mut capture = None;
        let element = cli::resolve_table_element(&table_target(), PRICES)
            .expect("resolves")
            .expect("one table");
        let encoded = element_with_ref(&mut capture, &refs, &element, true);
        assert_eq!(encoded["role"], "table");
        assert!(encoded.get("raw").is_none(), "{encoded}");
        assert!(encoded.get("handle").is_none(), "{encoded}");
        let r = encoded["ref"].as_str().expect("a ref").to_owned();
        refs.keep(capture.expect("the first element starts a capture"));
        assert_eq!(
            refs.resolve(&r).expect("resolves").name.as_deref(),
            Some("Prices")
        );
    }

    #[test]
    fn table_elements_carry_refs_that_resolve() {
        // The `table` description promises find's shape, and find's matches
        // carry a ref; without one a model could see a cell and not act on it.
        let refs = RefTable::default();
        let call = |op: &str, selector: &str, row, column| {
            let parsed =
                cli::parse_table_op(op, row, column, "\"row\"", "\"column\"").expect("valid op");
            tool_table_parsed(
                &table_target(),
                selector,
                op,
                parsed,
                50,
                &cli::PointerCenter::Usable,
                &refs,
            )
            .expect("table call succeeds")
            .structured
            .expect("structured")
        };
        let cell = call("cell-at", PRICES, Some(0), Some(2));
        let r = cell["cell"]["ref"]
            .as_str()
            .expect("the cell carries a ref");
        assert_eq!(
            refs.resolve(r).expect("resolves").name.as_deref(),
            Some("Price")
        );
        let headers = call("column-headers", PRICES, None, None);
        for el in headers["elements"].as_array().expect("elements") {
            let r = el["ref"].as_str().expect("each element carries a ref");
            assert!(refs.resolve(r).is_ok(), "{r}");
        }
        // A miss starts no capture, so it cannot age out an earlier one.
        let before = call("cell-at", PRICES, Some(9), Some(0));
        assert_eq!(before["cell"], Value::Null);
        assert!(refs.resolve(r).is_ok());
    }

    #[test]
    fn table_arguments_are_checked_before_anything_resolves() {
        let err = cli::parse_table_op("cell-at", Some(1), None, "\"row\"", "\"column\"")
            .expect_err("column missing");
        assert!(
            err.to_string().contains("requires \"row\" and \"column\""),
            "{err}"
        );
        let err = cli::parse_table_op("row-headers", Some(1), None, "\"row\"", "\"column\"")
            .expect_err("row is cell-at only");
        assert!(err.to_string().contains("does not take \"row\""), "{err}");
        let err = tool_table_parsed(
            &table_target(),
            NOTES,
            "column-headers",
            cli::TableOp::Headers(crate::HeaderKind::Column),
            50,
            &cli::PointerCenter::Usable,
            &RefTable::default(),
        )
        .expect_err("a text area is not a table");
        assert_eq!(failure_kind(&err), "action_not_supported");
    }

    #[test]
    fn the_text_descriptions_state_utf16_units() {
        let text = tool_definition("text")["description"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("UTF-16 code unit"), "{text}");
        assert!(text.contains("text_truncated"), "{text}");
        let action = tool_definition("action");
        let value = action["inputSchema"]["properties"]["value"]["description"]
            .as_str()
            .unwrap();
        assert!(value.contains("UTF-16 code units"), "{value}");
        assert!(!value.contains("character offsets"), "{value}");
    }
}
