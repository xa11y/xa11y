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

use serde_json::{json, Map, Value};

use super::base64;
use super::events::{Registry, BUFFER_CAPACITY, EXPIRY};
use crate::cli::{
    self, parse_button, parse_held, parse_key_name, resolve_app, resolve_target, CliError,
    CliResult, Opts, Target, ACTIONS_REQUIRING_VALUE, ACTION_NAMES,
};
use crate::{
    App, AppExt, ClickOptions, ClickTarget, DragOptions, Element, Locator, Rect, ScrollDelta,
    ShellSurface, ShellSurfaceExt,
};

/// Depth used by `tree` when the caller does not ask for one.
const TREE_DEFAULT_MAX_DEPTH: usize = 12;
/// Hard ceiling on nodes in one `tree` result, whatever depth was requested.
const TREE_MAX_NODES: usize = 2_000;
/// Matches returned by `find` when the caller does not ask for a limit.
const FIND_DEFAULT_LIMIT: usize = 50;
/// Ceiling on `find`'s `limit`, so a caller cannot ask for an unbounded dump.
const FIND_MAX_LIMIT: usize = 500;
/// Longest candidate list carried in a failure's diagnosis.
const MAX_DIAGNOSIS_CANDIDATES: usize = 20;
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
`modal` and `active`.";

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
}

impl Xa11yTools {
    pub(crate) fn new() -> Self {
        Self {
            events: Registry::new(),
        }
    }
}

/// Every tool name, in list order.
const TOOL_NAMES: &[&str] = &[
    "apps",
    "shell",
    "tree",
    "find",
    "action",
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
            "shell" => tool_shell(),
            "tree" => tool_tree(args),
            "find" => tool_find(args),
            "action" => tool_action(args),
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

/// Properties naming the target — an application or a shell surface — shared
/// by every a11y tool.
fn app_target_properties() -> Map<String, Value> {
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
                            refused whatever you pass.",
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
        "tree" => {
            let mut props = app_target_properties();
            props.insert(
                "max_depth".into(),
                TREE_DEPTH.property(format!(
                    "How deep to walk. Default {TREE_DEFAULT_MAX_DEPTH}. \
                     0 returns the application node alone. Results are also \
                     capped at {TREE_MAX_NODES} nodes; `truncated` in the result \
                     says whether either limit was hit."
                )),
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
                     {ACTIONS_FIELD_NOTE}"
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
            tool(
                "find",
                "Find elements",
                &format!(
                    "Find elements matching a selector. Each match reports its screen \
                     `bounds` and `center`, which are the coordinates the click, move, \
                     drag, scroll, and screenshot tools take, plus its `states` and \
                     the actions the application advertises. A selector that matches \
                     nothing comes back with the near-miss candidates that were in \
                     scope, so read those before guessing again.\n\n\
                     {ACTIONS_FIELD_NOTE}"
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
                        "Accessibility action to perform. These require `value`: {}.",
                        ACTIONS_REQUIRING_VALUE.join(", ")
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
                         `radio_button:nth(2)`.\n\n{SELECTOR_SYNTAX}"
                    ),
                }),
            );
            props.insert(
                "value".into(),
                json!({
                    "type": "string",
                    "description": "Argument for actions that need one: the text for `set-value` \
                                    and `type-text`, a number for `set-numeric-value` (e.g. \
                                    \"88\", within the element's `min_value`..`max_value`), or \
                                    `START,END` character offsets for `select-text`.",
                }),
            );
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
                     what it matched, rather than applied to the first of them.\n\n\
                     Auto-waits for the selector to match an element that is visible and \
                     enabled, re-resolving as it polls, and only then acts. The wait runs \
                     up to the default timeout, currently {timeout}. Set {timeout_env} \
                     (in seconds) in the environment the server is launched with to \
                     change it; no tool argument does. A call that is going to fail \
                     therefore takes that long and returns a `timeout` failure \
                     naming what it was waiting for and what it last saw. Do not wrap \
                     this tool in a retry loop; the wait is the retry loop.\n\n\
                     `ok: true` means the application accepted the call, not that \
                     anything changed: a control is free to accept an action and do \
                     nothing. Confirm the effect by re-reading with `tree` or `find`.\n\n\
                     {ACTIONS_FIELD_NOTE}",
                    timeout = default_timeout_label(),
                    timeout_env = crate::DEFAULT_TIMEOUT_ENV_VAR,
                ),
                object_schema(props, &["action", "selector"]),
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
                 `find`, which reports each match's `center`.",
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
                    "How long the drag takes, in milliseconds. Default 150. Slower drags \
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
                COORD.property("Vertical scroll delta. Positive scrolls down. Default 0.".into()),
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
                            care about from being evicted by ones you do not.",
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
         and state changes, windows opening, selections, announcements. Returns a \
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
         you are done.",
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
         what was dropped), `at_ms` since the subscription started, and the `target` \
         element in the same shape `find` returns — bounds, states and all. A \
         `state_changed` event also carries `state_flag` and `state_value`.\n\n\
         Two fields say what the events alone cannot. `dropped` counts events lost \
         to buffer eviction since the previous poll — poll more often, narrow \
         `kinds`, or accept the gap. `live: false` means the source is gone (the \
         application exited, or the platform dropped the subscription): drain what \
         is left, then stop polling, because nothing further can arrive.\n\n\
         An event is a snapshot from when it fired. Read the tree again to see the \
         UI as it is now."
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
fn has_target(args: &Value) -> bool {
    !target_keys(args).is_empty()
}

/// The target-naming arguments this call actually passed, in schema order.
///
/// `screenshot` names them back to the caller when they were passed without
/// `annotate`, because "a target does nothing here" is only actionable if the
/// answer says which of the three it means.
fn target_keys(args: &Value) -> Vec<&'static str> {
    ["app", "pid", "shell"]
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

fn tool_tree(args: &Value) -> CliResult<ToolOutput> {
    let max_depth = opt_usize(args, "max_depth", TREE_DEFAULT_MAX_DEPTH, TREE_DEPTH)?;
    let target = target(args)?;
    let root = target.root();

    let mut budget = TREE_MAX_NODES;
    let mut depth_capped = false;
    let node = build_node(&root, max_depth, 0, &mut budget, &mut depth_capped);

    let mut out = Map::new();
    target_fields(&target, &mut out);
    out.insert("max_depth".into(), json!(max_depth));
    out.insert(
        "truncated".into(),
        json!({
            "by_depth": depth_capped,
            "by_node_limit": budget == 0,
            "node_limit": TREE_MAX_NODES,
        }),
    );
    out.insert("tree".into(), node);
    Ok(ToolOutput::json(Value::Object(out)))
}

/// Walk `element` into a JSON node, honouring both the depth limit and a
/// shared node budget.
///
/// A child-enumeration failure is recorded inside the node rather than
/// failing the whole tree: one inaccessible subtree should not cost the
/// caller every other window (this mirrors what `xa11y tree` prints).
fn build_node(
    element: &Element,
    max_depth: usize,
    depth: usize,
    budget: &mut usize,
    depth_capped: &mut bool,
) -> Value {
    // `Element` derefs to `ElementData`, so the leaf encoding is shared with
    // `find` and the two cannot describe the same element differently.
    let mut node = element_data_json(element);
    let obj = node
        .as_object_mut()
        .expect("element_data_json always builds an object");

    if depth >= max_depth {
        *depth_capped = true;
        return node;
    }
    if *budget == 0 {
        return node;
    }

    match element.children() {
        Ok(children) => {
            let mut encoded = Vec::new();
            for child in &children {
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
                encoded.push(build_node(
                    child,
                    max_depth,
                    depth + 1,
                    budget,
                    depth_capped,
                ));
            }
            if !encoded.is_empty() {
                obj.insert("children".into(), Value::Array(encoded));
            }
        }
        Err(e) => {
            obj.insert("children_error".into(), json!(e.to_string()));
        }
    }
    node
}

fn tool_find(args: &Value) -> CliResult<ToolOutput> {
    let selector = req_str(args, "selector")?;
    let limit = opt_usize(args, "limit", FIND_DEFAULT_LIMIT, FIND_LIMIT)?;
    let target = target(args)?;

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

    let total = elements.len();
    let matches: Vec<Value> = elements
        .iter()
        .take(limit)
        .map(|el| element_data_json(el))
        .collect();

    let mut out = Map::new();
    out.insert("selector".into(), json!(selector));
    target_fields(&target, &mut out);
    out.insert("match_count".into(), json!(total));
    out.insert("returned".into(), json!(matches.len()));
    out.insert("truncated".into(), json!(total > matches.len()));
    out.insert("matches".into(), Value::Array(matches));
    Ok(ToolOutput::json(Value::Object(out)))
}

fn tool_action(args: &Value) -> CliResult<ToolOutput> {
    let action = req_str(args, "action")?;
    let selector = req_str(args, "selector")?;
    let value = opt_str(args, "value")?;
    let target = target(args)?;

    let locator = target.locator(selector);
    // The schema promises "must match exactly one element", so enforce it here
    // rather than letting the locator's document-order first-match stand in
    // for it: a model told "exactly one" that silently gets the first of
    // several has no way to notice it pressed the wrong control.
    //
    // Zero matches deliberately falls through to the action, whose auto-wait
    // is what gives an element still being built time to appear — and whose
    // timeout carries the richer "never matched" diagnosis.
    let matches = locator.elements()?;
    if matches.len() > 1 {
        return Err(ambiguous(selector, &matches));
    }

    cli::perform_action(&locator, action, value)?;

    // `ok` reports that the platform accepted the call. Whether the
    // application did anything with it is only knowable by re-reading, which
    // is what the tool's description tells the caller to do.
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("action".into(), json!(action));
    out.insert("selector".into(), json!(selector));
    target_fields(&target, &mut out);
    Ok(ToolOutput::json(Value::Object(out)))
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
        extras.push("hidden".into());
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
    let mut candidates: Vec<String> = matches
        .iter()
        .take(MAX_DIAGNOSIS_CANDIDATES)
        .map(|el| describe_candidate(el))
        .collect();
    if matches.len() > MAX_DIAGNOSIS_CANDIDATES {
        candidates.push(format!(
            "… (+{} more matches)",
            matches.len() - MAX_DIAGNOSIS_CANDIDATES
        ));
    }
    CliError::Ambiguous {
        count: matches.len(),
        diagnosis: Box::new(
            crate::Diagnosis::new()
                .selector(selector)
                .last_observed(format!("selector matched {} elements", matches.len()))
                .candidates(candidates),
        ),
    }
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
    let sub = app.subscribe()?;
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
    Ok(ToolOutput::json(registry.poll(id, max, timeout)?))
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

/// Encode element data as a wire node.
///
/// Hand-built rather than `serde_json::to_value(data)`: `ElementData` also
/// carries `raw` (the whole platform attribute blob) and `handle` (an opaque
/// pointer), neither of which means anything to a model and both of which
/// would dominate the payload. Absent fields are omitted rather than sent as
/// nulls, for the same reason.
pub(super) fn element_data_json(data: &crate::ElementData) -> Value {
    let mut node = Map::new();
    node.insert("role".into(), json!(data.role.to_snake_case()));
    insert_some(&mut node, "name", data.name.as_deref().map(Value::from));
    insert_some(&mut node, "value", data.value.as_deref().map(Value::from));
    insert_some(
        &mut node,
        "description",
        data.description.as_deref().map(Value::from),
    );
    insert_some(&mut node, "id", data.stable_id.as_deref().map(Value::from));

    if let Some(b) = data.bounds {
        node.insert(
            "bounds".into(),
            json!({ "x": b.x, "y": b.y, "width": b.width, "height": b.height }),
        );
        // The point the click / move / scroll tools want, precomputed so the
        // model does not have to do arithmetic to press a button.
        node.insert(
            "center".into(),
            json!({
                "x": b.x + (b.width as i32) / 2,
                "y": b.y + (b.height as i32) / 2,
            }),
        );
    }

    if let Some(nv) = data.numeric_value {
        node.insert("numeric_value".into(), json!(nv));
        insert_some(&mut node, "min_value", data.min_value.map(Value::from));
        insert_some(&mut node, "max_value", data.max_value.map(Value::from));
    }

    if !data.actions.is_empty() {
        node.insert("actions".into(), json!(data.actions));
    }
    node.insert("states".into(), states_json(&data.states));
    Value::Object(node)
}

fn insert_some(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.into(), value);
    }
}

/// Encode the state set, sending only what is true or explicitly known.
///
/// `enabled` and `visible` are always sent because they gate whether an
/// element can be acted on at all, and their absence would read as unknown
/// rather than as the documented default.
fn states_json(states: &crate::StateSet) -> Value {
    let mut out = Map::new();
    out.insert("enabled".into(), json!(states.enabled));
    out.insert("visible".into(), json!(states.visible));
    for (key, value) in [
        ("focused", states.focused),
        ("focusable", states.focusable),
        ("active", states.active),
        ("editable", states.editable),
        ("selected", states.selected),
        ("modal", states.modal),
        ("required", states.required),
        ("busy", states.busy),
    ] {
        if value {
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

    match err {
        CliError::Xa11y(inner) => {
            if let Some(diagnosis) = diagnosis_of(inner) {
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
        CliError::Usage(_) | CliError::NotFound(_) => {}
    }
    (text, Value::Object(structured))
}

/// Borrow the [`Diagnosis`] an error carries, if any.
fn diagnosis_of(err: &crate::Error) -> Option<&crate::Diagnosis> {
    match err {
        crate::Error::SelectorNotMatched { diagnosis, .. }
        | crate::Error::Timeout { diagnosis, .. } => diagnosis.as_deref(),
        _ => None,
    }
}

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
        let encoded = element_data_json(&data);
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

    #[test]
    fn states_send_only_what_is_true_plus_the_gating_pair() {
        let data = crate::ElementData::for_role(crate::Role::Button);
        let encoded = element_data_json(&data);
        let states = encoded["states"].as_object().unwrap();
        assert!(states.contains_key("enabled"));
        assert!(states.contains_key("visible"));
        assert!(!states.contains_key("focused"), "false states are omitted");
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
}
