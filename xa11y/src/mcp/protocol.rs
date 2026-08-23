//! JSON-RPC envelope handling and MCP method routing.
//!
//! # Two protocol eras
//!
//! MCP revision `2026-07-28` is *stateless*: there is no handshake, every
//! request carries its protocol version in
//! `_meta["io.modelcontextprotocol/protocolVersion"]`, and servers must
//! implement `server/discover`. Revisions up to `2025-11-25` are *legacy*:
//! they open with an `initialize` request and a `notifications/initialized`.
//!
//! Deployed clients are still mostly legacy, so this server is **dual-era**.
//! That costs little because both eras share `tools/list` and `tools/call`
//! verbatim — the era changes only the opening exchange and whether results
//! carry `resultType`. The era is a property of the session (the spec: "a
//! dual-era server selects its behavior from how the client opens"), so it is
//! latched on the first request that identifies one and reused thereafter.
//!
//! Everything here is a pure `Value -> Option<Value>` transform over an
//! injected [`ToolHost`], so the routing, the version negotiation, and the
//! error mapping are all unit-testable with no display and no subprocess.

use serde_json::{json, Map, Value};

use super::tools::ToolHost;
use crate::cli::{CliError, CliResult};

/// The modern (stateless, per-request metadata) revision this server speaks.
pub(crate) const MODERN_VERSION: &str = "2026-07-28";

/// Legacy (`initialize`-handshake) revisions this server speaks. `tools/list`
/// and `tools/call` are identical across these, and this server uses no
/// feature that differs between them, so supporting the range is honest
/// rather than aspirational.
pub(crate) const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// Every revision this server accepts, newest first. Advertised verbatim in
/// `server/discover` and in the `data.supported` of an unsupported-version
/// error, so a client can always pick a workable revision from one reply.
pub(crate) fn supported_versions() -> Vec<&'static str> {
    let mut all = vec![MODERN_VERSION];
    all.extend_from_slice(LEGACY_VERSIONS);
    all
}

/// How long a client may consider `server/discover` and `tools/list` fresh.
///
/// Both are compile-time constants here — the tool table is a `const` slice and
/// the version list never changes — and on stdio the process *is* the session,
/// so neither can change under a connected client. The hour is arbitrary but
/// safe. The alternative invalidation channel, `listChanged`, is deliberately
/// not advertised: there is nothing to notify about.
const CACHE_TTL_MS: u64 = 3_600_000;

/// Neither cacheable result carries user-specific data, so a shared gateway or
/// caching proxy may serve them to any caller. See the spec's guidance:
/// `"public"` is for lists that are identical for every user.
const CACHE_SCOPE: &str = "public";

/// Stamp the caching hints onto a cacheable result.
///
/// The spec is a MUST: every `resultType: "complete"` result from
/// `server/discover`, `tools/list`, and the other cacheable operations carries
/// `ttlMs` and `cacheScope`. Omitting them is not a lenient degradation — the
/// official SDK's revision-pinned wire models mark both fields required, so a
/// real client rejects the response outright.
fn with_cache_hints(result: &mut Map<String, Value>) {
    result.insert("ttlMs".into(), json!(CACHE_TTL_MS));
    result.insert("cacheScope".into(), json!(CACHE_SCOPE));
}

/// `_meta` key carrying the protocol version on a modern request.
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key carrying the server identity on a modern result.
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

// JSON-RPC / MCP error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Guidance handed to the model alongside the tool list. Worth spending a
/// few lines on: it is the only place to say that a selector language exists
/// and that the coordinate tools compose with `find`.
const INSTRUCTIONS: &str = "\
Read and drive desktop application UIs through platform accessibility APIs.

Start with `apps` to find a running application, then `tree` to see its \
structure. Address elements with xa11y selectors, which are CSS-like: \
`button[name=\"OK\"]`, `text_field[name*=\"Search\"]`, `window > group button`. \
Prefer `action` over the mouse and keyboard tools — it calls the application's \
own accessibility action and does not depend on window position or focus.

OS shell UI — the taskbar, the desktop, docks and panels, the menu bar, status \
items, open flyouts — is reached the same way: `shell` lists what is on screen, \
and `tree`, `find` and `action` take a `shell` argument in place of `app`.

`action` acts on exactly one element and auto-waits for it, so a selector that \
matches several is refused rather than applied to the first, and no retry loop \
of your own is needed. Its `ok: true` means the application accepted the call, \
not that anything changed — confirm by reading the tree again.

To watch what an application does rather than poll its tree, \
`events_start` returns a handle, `events_poll` drains buffered events, and \
`events_stop` closes it. Start the subscription before the action you want to \
observe.

The input and screenshot tools work in screen coordinates only. Get those from \
`find`, which returns each match's `bounds` and `center`.

Trees are depth-limited and match lists are capped; both say so in the result \
when output was truncated.";

/// Which protocol era this session is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    /// No request has identified an era yet.
    Undecided,
    /// Stateless, per-request `_meta` (revision 2026-07-28 and later).
    Modern,
    /// `initialize` handshake (revision 2025-11-25 and earlier).
    Legacy,
}

/// One MCP stdio session.
pub(crate) struct Session<H: ToolHost> {
    host: H,
    era: Era,
}

impl<H: ToolHost> Session<H> {
    pub(crate) fn new(host: H) -> Self {
        Self {
            host,
            era: Era::Undecided,
        }
    }

    #[cfg(test)]
    pub(crate) fn era(&self) -> Era {
        self.era
    }

    /// Handle one decoded message.
    ///
    /// Returns the response to write, or `None` for a notification (JSON-RPC
    /// forbids responding to a message with no `id`, and the stdio binding
    /// forbids the server writing requests at all).
    pub(crate) fn handle(&mut self, message: Value) -> Option<Value> {
        let obj = match message.as_object() {
            Some(o) => o,
            None => {
                return Some(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "request must be a JSON object",
                    None,
                ))
            }
        };

        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(Value::as_str);

        let Some(method) = method else {
            // A message with no method is a JSON-RPC *response*, which the
            // stdio binding forbids clients from sending. Answer only if it
            // carries an id; otherwise there is nothing to answer.
            return id.map(|id| {
                error_response(id, INVALID_REQUEST, "request must carry a \"method\"", None)
            });
        };

        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        self.latch_era(method, &params);

        // Notifications get no reply, whatever they are. `notifications/
        // initialized` and `notifications/cancelled` are the two this server
        // sees; both are no-ops for a synchronous single-threaded server.
        let id = id?;

        Some(match self.dispatch(method, &params) {
            Ok(result) => success_response(id, result),
            Err(err) => err.into_response(id),
        })
    }

    /// Latch the session era from the first request that identifies one.
    fn latch_era(&mut self, method: &str, params: &Value) {
        if self.era != Era::Undecided {
            return;
        }
        if request_protocol_version(params).is_some() {
            self.era = Era::Modern;
        } else if method == "initialize" {
            self.era = Era::Legacy;
        }
    }

    /// Whether results should carry the modern `resultType` discriminator.
    ///
    /// An undecided session is treated as modern: a client that jumped
    /// straight to `tools/call` without `_meta` gets the newer shape, and
    /// legacy clients always announce themselves via `initialize` first.
    fn modern_results(&self) -> bool {
        self.era != Era::Legacy
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        self.check_requested_version(params)?;
        match method {
            "server/discover" => Ok(self.discover_result()),
            "initialize" => self.initialize_result(params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list_result()),
            "tools/call" => self.tools_call_result(params),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            )),
        }
    }

    /// Reject a modern request that names a revision this server does not
    /// speak, naming what it does speak so the client can retry (tenet 1: the
    /// mismatch is surfaced, not quietly served under a different version).
    fn check_requested_version(&self, params: &Value) -> Result<(), RpcError> {
        let Some(requested) = request_protocol_version(params) else {
            return Ok(());
        };
        if supported_versions().contains(&requested) {
            return Ok(());
        }
        Err(
            RpcError::new(UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version").with_data(
                json!({
                    "supported": supported_versions(),
                    "requested": requested,
                }),
            ),
        )
    }

    fn discover_result(&self) -> Value {
        let mut result = Map::new();
        result.insert("resultType".into(), json!("complete"));
        result.insert("supportedVersions".into(), json!(supported_versions()));
        result.insert("capabilities".into(), json!({ "tools": {} }));
        result.insert("instructions".into(), json!(INSTRUCTIONS));
        result.insert("_meta".into(), json!({ META_SERVER_INFO: server_info() }));
        with_cache_hints(&mut result);
        Value::Object(result)
    }

    /// The legacy handshake reply.
    ///
    /// Echoes the client's requested revision when this server speaks it, and
    /// otherwise names the newest legacy revision it does speak. A legacy
    /// client has no fall-forward mechanism, so answering with *some*
    /// supported version beats an error it cannot act on.
    fn initialize_result(&self, params: &Value) -> Result<Value, RpcError> {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let negotiated = if LEGACY_VERSIONS.contains(&requested) {
            requested
        } else {
            LEGACY_VERSIONS[0]
        };
        Ok(json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": {} },
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        }))
    }

    fn tools_list_result(&self) -> Value {
        let mut result = Map::new();
        if self.modern_results() {
            result.insert("resultType".into(), json!("complete"));
            // Caching hints exist only on the modern revisions; a legacy
            // client has no field to put them in.
            with_cache_hints(&mut result);
        }
        result.insert("tools".into(), Value::Array(self.host.list()));
        Value::Object(result)
    }

    fn tools_call_result(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call requires a \"name\""))?;

        // Absent arguments means "no arguments", which is legal for the
        // zero-parameter tools. A non-object `arguments` is malformed.
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(v) if v.is_object() => v.clone(),
            Some(_) => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "tools/call \"arguments\" must be an object",
                ))
            }
        };

        if !self.host.has_tool(name) {
            // Per the spec, an unknown tool is a *protocol* error: it is not
            // something the model can fix by adjusting arguments.
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown tool: {name}"),
            ));
        }

        // Everything the handler itself raises comes back as a tool execution
        // error (`isError: true`) rather than a JSON-RPC error, because those
        // are the failures a model can act on: a selector that matched
        // nothing, an app that is not running, an element that is not
        // actionable. The accompanying `Diagnosis` is what makes the retry
        // informed rather than a guess.
        let mut result = Map::new();
        if self.modern_results() {
            result.insert("resultType".into(), json!("complete"));
        }
        match self.host.call(name, &arguments) {
            Ok(output) => {
                result.insert("content".into(), Value::Array(output.content));
                if let Some(structured) = output.structured {
                    result.insert("structuredContent".into(), structured);
                }
                result.insert("isError".into(), json!(false));
            }
            Err(err) => {
                let (text, structured) = super::tools::describe_failure(name, &err);
                result.insert("content".into(), json!([{ "type": "text", "text": text }]));
                result.insert("structuredContent".into(), structured);
                result.insert("isError".into(), json!(true));
            }
        }
        Ok(Value::Object(result))
    }
}

/// Read the protocol version a modern request declares in `params._meta`.
fn request_protocol_version(params: &Value) -> Option<&str> {
    params
        .get("_meta")?
        .get(META_PROTOCOL_VERSION)?
        .as_str()
        .filter(|s| !s.is_empty())
}

fn server_info() -> Value {
    json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// A JSON-RPC error, carried until it can be paired with a request id.
pub(crate) struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    fn into_response(self, id: Value) -> Value {
        error_response(id, self.code, &self.message, self.data)
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".into(), json!(code));
    error.insert("message".into(), json!(message));
    if let Some(data) = data {
        error.insert("data".into(), data);
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": Value::Object(error) })
}

/// Response to a line that did not parse as JSON.
///
/// The id is unknowable, so it is null per JSON-RPC. The session continues:
/// framing is line-based, so exactly one message was lost.
pub(crate) fn parse_error_response(err: &CliError) -> Value {
    error_response(
        Value::Null,
        PARSE_ERROR,
        &format!("Parse error: {err}"),
        None,
    )
}

/// Reject unexpected arguments to `xa11y mcp`.
///
/// The subcommand takes none. Silently ignoring a flag someone passed would
/// leave them believing it took effect (tenet 1).
pub(crate) fn check_no_args(args: &[String]) -> CliResult<()> {
    if let Some(first) = args.first() {
        return Err(CliError::Usage(format!(
            "xa11y mcp takes no arguments (got: {first}). \
             It serves the accessibility tools over stdio for an MCP client."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::{ToolHost, ToolOutput};

    /// A host with one always-succeeding tool and one always-failing tool, so
    /// routing and error mapping are testable with no accessibility backend.
    struct StubHost;

    impl ToolHost for StubHost {
        fn list(&self) -> Vec<Value> {
            vec![json!({
                "name": "apps",
                "description": "stub",
                "inputSchema": { "type": "object", "additionalProperties": false },
            })]
        }

        fn has_tool(&self, name: &str) -> bool {
            matches!(name, "apps" | "boom")
        }

        fn call(&self, name: &str, _args: &Value) -> CliResult<ToolOutput> {
            match name {
                "apps" => Ok(ToolOutput::text("stub ok")),
                _ => Err(CliError::NotFound("nothing matched".into())),
            }
        }
    }

    fn session() -> Session<StubHost> {
        Session::new(StubHost)
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn modern_meta() -> Value {
        json!({ "_meta": { META_PROTOCOL_VERSION: MODERN_VERSION } })
    }

    #[test]
    fn discover_advertises_every_supported_version() {
        let mut s = session();
        let resp = s.handle(req(1, "server/discover", modern_meta())).unwrap();
        let result = &resp["result"];
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"][0], MODERN_VERSION);
        assert_eq!(
            result["supportedVersions"].as_array().unwrap().len(),
            supported_versions().len()
        );
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["_meta"][META_SERVER_INFO]["name"], "xa11y");
    }

    #[test]
    fn discover_carries_the_required_caching_hints() {
        // A MUST in the spec, and the official SDK's revision-pinned wire
        // model marks both fields required — omitting them made a real client
        // reject the response, which is how this was found.
        let mut s = session();
        let resp = s.handle(req(1, "server/discover", modern_meta())).unwrap();
        assert_eq!(resp["result"]["ttlMs"], CACHE_TTL_MS);
        assert_eq!(resp["result"]["cacheScope"], CACHE_SCOPE);
    }

    #[test]
    fn modern_tools_list_carries_the_required_caching_hints() {
        let mut s = session();
        s.handle(req(1, "server/discover", modern_meta()));
        let resp = s.handle(req(2, "tools/list", Value::Null)).unwrap();
        assert_eq!(resp["result"]["ttlMs"], CACHE_TTL_MS);
        assert_eq!(resp["result"]["cacheScope"], CACHE_SCOPE);
    }

    #[test]
    fn legacy_tools_list_omits_the_caching_hints() {
        // The legacy revisions have no field for them.
        let mut s = session();
        s.handle(req(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-06-18" }),
        ));
        let resp = s.handle(req(2, "tools/list", Value::Null)).unwrap();
        assert!(resp["result"].get("ttlMs").is_none());
        assert!(resp["result"].get("cacheScope").is_none());
    }

    #[test]
    fn discover_latches_the_modern_era() {
        let mut s = session();
        s.handle(req(1, "server/discover", modern_meta()));
        assert_eq!(s.era(), Era::Modern);
    }

    #[test]
    fn initialize_latches_the_legacy_era_and_echoes_a_supported_version() {
        let mut s = session();
        let resp = s
            .handle(req(
                1,
                "initialize",
                json!({ "protocolVersion": "2025-06-18" }),
            ))
            .unwrap();
        assert_eq!(s.era(), Era::Legacy);
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(resp["result"]["serverInfo"]["name"], "xa11y");
    }

    #[test]
    fn initialize_with_an_unknown_version_names_one_we_speak() {
        // A legacy client has no fall-forward path, so answering with a
        // supported revision beats an error it cannot act on.
        let mut s = session();
        let resp = s
            .handle(req(
                1,
                "initialize",
                json!({ "protocolVersion": "1999-01-01" }),
            ))
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
    }

    #[test]
    fn unsupported_modern_version_is_rejected_with_the_supported_list() {
        let mut s = session();
        let params = json!({ "_meta": { META_PROTOCOL_VERSION: "1900-01-01" } });
        let resp = s.handle(req(1, "server/discover", params)).unwrap();
        assert_eq!(resp["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
        assert_eq!(resp["error"]["data"]["requested"], "1900-01-01");
        assert_eq!(resp["error"]["data"]["supported"][0], MODERN_VERSION);
    }

    #[test]
    fn legacy_results_omit_result_type() {
        let mut s = session();
        s.handle(req(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-06-18" }),
        ));
        let resp = s.handle(req(2, "tools/list", Value::Null)).unwrap();
        assert!(
            resp["result"].get("resultType").is_none(),
            "legacy clients must not receive the modern discriminator"
        );
        assert!(resp["result"]["tools"].is_array());
    }

    #[test]
    fn modern_results_carry_result_type() {
        let mut s = session();
        s.handle(req(1, "server/discover", modern_meta()));
        let resp = s.handle(req(2, "tools/list", Value::Null)).unwrap();
        assert_eq!(resp["result"]["resultType"], "complete");
    }

    #[test]
    fn notifications_get_no_response() {
        let mut s = session();
        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(s.handle(notification).is_none());
        let cancelled = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 1 },
        });
        assert!(s.handle(cancelled).is_none());
    }

    #[test]
    fn ping_answers_empty() {
        let mut s = session();
        let resp = s.handle(req(1, "ping", Value::Null)).unwrap();
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let mut s = session();
        let resp = s.handle(req(1, "resources/list", Value::Null)).unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn unknown_tool_is_a_protocol_error_not_a_tool_error() {
        let mut s = session();
        let params = json!({ "name": "nope", "arguments": {} });
        let resp = s.handle(req(1, "tools/call", params)).unwrap();
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
        assert!(resp["error"]["message"].as_str().unwrap().contains("nope"));
    }

    #[test]
    fn handler_failures_are_tool_errors_so_the_model_can_retry() {
        let mut s = session();
        let params = json!({ "name": "boom", "arguments": {} });
        let resp = s.handle(req(1, "tools/call", params)).unwrap();
        assert!(resp.get("error").is_none(), "must not be a protocol error");
        assert_eq!(resp["result"]["isError"], json!(true));
        assert!(resp["result"]["structuredContent"].is_object());
    }

    #[test]
    fn successful_call_reports_is_error_false() {
        let mut s = session();
        let params = json!({ "name": "apps", "arguments": {} });
        let resp = s.handle(req(1, "tools/call", params)).unwrap();
        assert_eq!(resp["result"]["isError"], json!(false));
        assert_eq!(resp["result"]["content"][0]["text"], "stub ok");
    }

    #[test]
    fn missing_arguments_is_treated_as_no_arguments() {
        let mut s = session();
        let resp = s
            .handle(req(1, "tools/call", json!({ "name": "apps" })))
            .unwrap();
        assert_eq!(resp["result"]["isError"], json!(false));
    }

    #[test]
    fn non_object_arguments_is_invalid_params() {
        let mut s = session();
        let params = json!({ "name": "apps", "arguments": "oops" });
        let resp = s.handle(req(1, "tools/call", params)).unwrap();
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn non_object_message_is_invalid_request() {
        let mut s = session();
        let resp = s.handle(json!([1, 2, 3])).unwrap();
        assert_eq!(resp["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn request_without_method_but_with_id_is_answered() {
        let mut s = session();
        let resp = s.handle(json!({ "jsonrpc": "2.0", "id": 7 })).unwrap();
        assert_eq!(resp["error"]["code"], INVALID_REQUEST);
        assert_eq!(resp["id"], 7);
    }

    #[test]
    fn responses_echo_the_request_id_including_string_ids() {
        let mut s = session();
        let msg = json!({ "jsonrpc": "2.0", "id": "discover-1", "method": "ping" });
        let resp = s.handle(msg).unwrap();
        assert_eq!(resp["id"], "discover-1");
        assert_eq!(resp["jsonrpc"], "2.0");
    }

    #[test]
    fn mcp_subcommand_rejects_arguments() {
        let err = check_no_args(&["--port".to_string()]).expect_err("must reject");
        assert!(matches!(err, CliError::Usage(_)));
    }
}
