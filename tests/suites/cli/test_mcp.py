"""`xa11y mcp` — the Model Context Protocol server, over every CLI entry point.

The point of this suite is the *cross-launcher* claim. One Rust implementation
backs three launchers (the `xa11y` binary, the Python console script, the Node
bin), and every test here runs against each one that is built, so "MCP works
the same in Rust, Python, and JS" is verified rather than asserted.

Protocol shapes come from the MCP specification:
https://modelcontextprotocol.io/specification/2026-07-28

Framing rules the tests lean on: one JSON-RPC message per line, UTF-8, no
embedded newlines, and nothing on stdout that is not a protocol message.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent.parent
sys.path.insert(0, str(PROJECT_ROOT))

from tests.harness.launch import cli_entry_points  # noqa: E402

# The newest revision the server speaks, and one legacy revision. Kept in step
# with `MODERN_VERSION` / `LEGACY_VERSIONS` in xa11y/src/mcp/protocol.rs.
MODERN_VERSION = "2026-07-28"
LEGACY_VERSION = "2025-06-18"

PROTOCOL_VERSION_META = "io.modelcontextprotocol/protocolVersion"
SERVER_INFO_META = "io.modelcontextprotocol/serverInfo"


# ── Entry points ─────────────────────────────────────────────────────────────


def _available_entry_points() -> dict[str, list[str]]:
    found = {name: cmd for name, cmd in cli_entry_points().items() if cmd is not None}
    if not found:
        pytest.fail(
            "no xa11y CLI entry point is available. Build at least the Rust "
            "binary with `cargo build -p xa11y`."
        )
    return found


ENTRY_POINTS = _available_entry_points()


def test_every_entry_point_is_built_when_required():
    """CI must exercise all three launchers, not whichever happened to build.

    A missing launcher is an error rather than a skip: a cell that quietly
    tested one entry point is indistinguishable from one that tested three,
    which is the failure mode this suite exists to prevent.
    """
    if not os.environ.get("XA11Y_REQUIRE_ALL_CLI"):
        pytest.skip("XA11Y_REQUIRE_ALL_CLI is not set (local run)")
    missing = sorted(name for name, cmd in cli_entry_points().items() if cmd is None)
    assert not missing, (
        f"CLI entry points not built: {missing}. "
        "Build them with `cargo build -p xa11y`, `cargo xtask test-python`, "
        "and `cargo xtask test-js`."
    )


@pytest.fixture(params=sorted(ENTRY_POINTS), ids=sorted(ENTRY_POINTS))
def entry_point(request) -> list[str]:
    """One CLI launcher's command prefix. Every test runs once per launcher."""
    return ENTRY_POINTS[request.param]


# ── JSON-RPC over stdio ──────────────────────────────────────────────────────


class McpSession:
    """A live `xa11y mcp` subprocess, spoken to in newline-delimited JSON-RPC."""

    def __init__(self, command: list[str]):
        self._proc = subprocess.Popen(
            [*command, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )

    def send_raw(self, line: str) -> None:
        assert self._proc.stdin is not None
        self._proc.stdin.write(line + "\n")
        self._proc.stdin.flush()

    def send(self, message: dict) -> None:
        self.send_raw(json.dumps(message))

    def recv(self) -> dict:
        assert self._proc.stdout is not None
        line = self._proc.stdout.readline()
        assert line, "server closed stdout without responding"
        assert line.endswith("\n"), "every message must be newline-terminated"
        return json.loads(line)

    def request(self, id_, method: str, params: dict | None = None) -> dict:
        message = {"jsonrpc": "2.0", "id": id_, "method": method}
        if params is not None:
            message["params"] = params
        self.send(message)
        response = self.recv()
        assert response["jsonrpc"] == "2.0"
        assert response["id"] == id_, "responses must echo the request id"
        return response

    def call_tool(self, name: str, arguments: dict | None = None, id_=99) -> dict:
        return self.request(
            id_, "tools/call", {"name": name, "arguments": arguments or {}}
        )

    def close(self, timeout: float = 10.0) -> int:
        """Close stdin and wait, which is the spec's graceful shutdown."""
        assert self._proc.stdin is not None
        self._proc.stdin.close()
        return self._proc.wait(timeout=timeout)

    def kill(self) -> None:
        if self._proc.poll() is None:
            self._proc.kill()
            self._proc.wait(timeout=10)


@pytest.fixture
def mcp(entry_point: list[str]):
    """A session with the legacy handshake already completed."""
    session = McpSession(entry_point)
    session.request(
        "init",
        "initialize",
        {"protocolVersion": LEGACY_VERSION, "capabilities": {}, "clientInfo": {"name": "pytest"}},
    )
    session.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    try:
        yield session
    finally:
        session.kill()


# ── Lifecycle ────────────────────────────────────────────────────────────────


def test_legacy_initialize_reports_identity_and_tool_capability(entry_point):
    session = McpSession(entry_point)
    try:
        result = session.request(
            1, "initialize", {"protocolVersion": LEGACY_VERSION, "capabilities": {}}
        )["result"]
        assert result["protocolVersion"] == LEGACY_VERSION, "must echo a version we speak"
        assert result["serverInfo"]["name"] == "xa11y"
        assert result["serverInfo"]["version"], "serverInfo must carry a version"
        assert "tools" in result["capabilities"]
        assert result["instructions"], "instructions guide the model's first move"
    finally:
        session.kill()


def test_modern_discover_needs_no_handshake(entry_point):
    session = McpSession(entry_point)
    try:
        result = session.request(
            "d1",
            "server/discover",
            {"_meta": {PROTOCOL_VERSION_META: MODERN_VERSION}},
        )["result"]
        assert result["resultType"] == "complete"
        assert result["supportedVersions"][0] == MODERN_VERSION
        assert LEGACY_VERSION in result["supportedVersions"], "dual-era server"
        assert result["_meta"][SERVER_INFO_META]["name"] == "xa11y"
    finally:
        session.kill()


def test_unknown_protocol_version_names_the_supported_ones(entry_point):
    session = McpSession(entry_point)
    try:
        error = session.request(
            1, "server/discover", {"_meta": {PROTOCOL_VERSION_META: "1900-01-01"}}
        )["error"]
        assert error["code"] == -32022
        assert error["data"]["requested"] == "1900-01-01"
        assert MODERN_VERSION in error["data"]["supported"]
    finally:
        session.kill()


def test_closing_stdin_exits_zero(entry_point):
    """The spec's only portable shutdown signal."""
    session = McpSession(entry_point)
    session.request(1, "ping")
    assert session.close() == 0


def test_ping_answers_empty(mcp):
    assert mcp.request(1, "ping")["result"] == {}


def test_notifications_are_not_answered(mcp):
    """A reply to a notification would desynchronize every later response."""
    mcp.send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})
    assert mcp.request(7, "ping")["id"] == 7, "next response must be the ping's"


def test_unknown_method_is_method_not_found(mcp):
    assert mcp.request(1, "resources/list")["error"]["code"] == -32601


# ── Framing ──────────────────────────────────────────────────────────────────


def test_malformed_line_costs_one_message_not_the_session(mcp):
    mcp.send_raw("{ not json at all")
    parse_error = mcp.recv()
    assert parse_error["error"]["code"] == -32700
    assert parse_error["id"] is None, "an unparsable message has no id"
    assert mcp.request(2, "ping")["result"] == {}, "session survives"


def test_responses_are_single_lines_of_valid_json(mcp):
    """stdout carries protocol messages only, one per line."""
    response = mcp.request(1, "tools/list")
    assert response["result"]["tools"], "tool list must not be empty"
    # `recv` already asserted one newline-terminated line parsed as JSON; a
    # tool list with eleven schemas is the largest thing this server writes,
    # so if anything were going to wrap, it would be this.


def test_stderr_is_not_protocol_and_does_not_corrupt_stdout(entry_point):
    session = McpSession(entry_point)
    try:
        session.request(1, "ping")
        # Deliberately provoke a tool failure, which logs nothing but proves
        # the response stream stays aligned after an error path.
        session.call_tool("find", {"selector": "button", "app": "NoSuchApp"}, id_=2)
        assert session.request(3, "ping")["result"] == {}
    finally:
        session.kill()


# ── Tools ────────────────────────────────────────────────────────────────────


def test_tools_list_is_complete_and_deterministic(mcp):
    first = [t["name"] for t in mcp.request(1, "tools/list")["result"]["tools"]]
    second = [t["name"] for t in mcp.request(2, "tools/list")["result"]["tools"]]
    assert first == second, "the spec asks for a deterministic order"
    assert first == [
        "apps",
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
    ]


def test_every_tool_declares_a_usable_schema(mcp):
    for tool in mcp.request(1, "tools/list")["result"]["tools"]:
        name = tool["name"]
        assert tool["description"], f"{name} needs a description"
        schema = tool["inputSchema"]
        assert schema["type"] == "object", f"{name} inputSchema must be an object schema"
        assert 1 <= len(name) <= 128
        for required in schema.get("required", []):
            assert required in schema["properties"], (
                f"{name} requires {required!r} but does not declare it"
            )


def test_unknown_tool_is_a_protocol_error(mcp):
    """Not something a model can fix by adjusting arguments, so not isError."""
    response = mcp.call_tool("teleport", {})
    assert "result" not in response
    assert response["error"]["code"] == -32602


def test_apps_lists_the_running_test_app(mcp, app_pid):
    """By pid, which is the only identifier the fixture actually knows.

    The matrix key (`qt`, `gtk`) is a launcher recipe name, not an
    accessibility name: the Qt app reports itself as `Python` on macOS,
    because that is the process the interpreter runs as.
    """
    result = mcp.call_tool("apps")["result"]
    assert result["isError"] is False
    structured = result["structuredContent"]
    assert structured["count"] == len(structured["applications"])
    listed = {a["pid"]: a for a in structured["applications"]}
    assert app_pid in listed, f"pid {app_pid} not among {sorted(listed)}"
    assert listed[app_pid]["name"], "a listed application must carry a name"


def test_the_app_argument_resolves_the_same_application_as_pid(mcp, app_pid):
    """Both targeting arguments must reach the same application.

    `app` matches the name exactly (`App::by_name`), so the name has to come
    from the `apps` listing rather than from the matrix key — which is the
    same thing a model has to do, and the reason `apps` exists.
    """
    listing = mcp.call_tool("apps")["result"]["structuredContent"]["applications"]
    reported = next(a["name"] for a in listing if a["pid"] == app_pid)
    if sum(1 for a in listing if a["name"] == reported) > 1:
        pytest.skip(f"more than one running application is named {reported!r}")

    by_name = mcp.call_tool("tree", {"app": reported, "max_depth": 0})["result"]
    assert by_name["isError"] is False, by_name["content"]
    assert by_name["structuredContent"]["pid"] == app_pid


def test_the_app_argument_does_not_match_a_substring(mcp, app_pid):
    """The schema says exact, so a prefix of a real name must not resolve.

    The suite itself made this mistake: it passed the matrix key (`winforms`)
    where the application answers to `xa11y-winforms-test-app`.
    """
    listing = mcp.call_tool("apps")["result"]["structuredContent"]["applications"]
    reported = next(a["name"] for a in listing if a["pid"] == app_pid)
    if len(reported) < 2:
        pytest.skip(f"{reported!r} is too short to take a proper prefix of")
    prefix = reported[:-1]
    if any(a["name"] == prefix for a in listing):
        pytest.skip(f"{prefix!r} is itself a running application")

    result = mcp.call_tool("tree", {"app": prefix})["result"]
    assert result["isError"] is True, f"{prefix!r} must not resolve {reported!r}"
    assert result["structuredContent"]["kind"] in {"no_match", "timeout"}


def test_tree_is_depth_limited_and_reports_truncation(mcp, app_pid):
    result = mcp.call_tool("tree", {"pid": app_pid, "max_depth": 1})["result"]
    assert result["isError"] is False
    structured = result["structuredContent"]
    assert structured["max_depth"] == 1
    assert "truncated" in structured, "a shortened tree must say so"
    root = structured["tree"]
    assert root["role"], "every node carries a role"
    for child in root.get("children", []):
        assert "children" not in child, "max_depth=1 stops after direct children"


def test_tree_defaults_to_a_bounded_depth(mcp, app_pid):
    structured = mcp.call_tool("tree", {"pid": app_pid})["result"]["structuredContent"]
    assert structured["max_depth"] == 12, "the default must be bounded, not unlimited"


def test_find_reports_bounds_and_a_precomputed_center(mcp, app_pid):
    """Named, because `button` alone leads with window chrome on Windows.

    Title-bar buttons report no bounds at all, so indexing `matches[0]` was a
    `KeyError` there rather than the assertion this test means to make.
    """
    result = mcp.call_tool("find", {"pid": app_pid, "selector": 'button[name="OK"]'})[
        "result"
    ]
    assert result["isError"] is False, result["content"]
    structured = result["structuredContent"]
    assert structured["match_count"] >= 1
    first = structured["matches"][0]
    assert first["role"] == "button"
    bounds = first["bounds"]
    center = first["center"]
    assert center["x"] == bounds["x"] + bounds["width"] // 2
    assert center["y"] == bounds["y"] + bounds["height"] // 2


def test_bounds_and_center_are_reported_together_or_not_at_all(mcp, app_pid):
    """The handler inserts both from one `Option`, so neither can appear alone.

    Checked across every button the app has, chrome included, which is the
    part the named lookup above deliberately does not reach.
    """
    structured = mcp.call_tool("find", {"pid": app_pid, "selector": "button"})["result"][
        "structuredContent"
    ]
    assert structured["match_count"] >= 1
    for match in structured["matches"]:
        assert ("bounds" in match) == ("center" in match), match
        if "bounds" not in match:
            continue
        bounds, center = match["bounds"], match["center"]
        assert center["x"] == bounds["x"] + bounds["width"] // 2, match
        assert center["y"] == bounds["y"] + bounds["height"] // 2, match


def test_find_respects_its_limit_and_flags_the_truncation(mcp, app_pid):
    structured = mcp.call_tool(
        "find", {"pid": app_pid, "selector": "*", "limit": 1}
    )["result"]["structuredContent"]
    assert structured["returned"] == 1
    if structured["match_count"] > 1:
        assert structured["truncated"] is True


def test_element_payloads_omit_the_platform_blob(mcp, app_pid):
    """`raw` and `handle` are large and meaningless to a model."""
    structured = mcp.call_tool(
        "find", {"pid": app_pid, "selector": "button", "limit": 1}
    )["result"]["structuredContent"]
    first = structured["matches"][0]
    assert "raw" not in first
    assert "handle" not in first


def test_a_failed_lookup_is_a_tool_error_carrying_its_diagnosis(mcp, app_pid):
    """Tenet 6 reaching the model: the retry should be informed, not a guess."""
    response = mcp.call_tool(
        "find", {"pid": app_pid, "selector": 'button[name="DefinitelyNotHere"]'}
    )
    assert "error" not in response, "a model can fix this, so it must be a tool error"
    result = response["result"]
    assert result["isError"] is True
    structured = result["structuredContent"]
    assert structured["tool"] == "find"
    assert structured["kind"] in {"no_match", "timeout"}
    assert structured["message"]


def test_a_missing_argument_comes_back_as_a_fixable_tool_error(mcp):
    result = mcp.call_tool("find", {"app": "whatever"})["result"]
    assert result["isError"] is True
    assert "selector" in result["structuredContent"]["message"]


def test_a_bad_key_name_names_the_key_it_rejected(mcp):
    result = mcp.call_tool("key", {"key": "NotAKey"})["result"]
    assert result["isError"] is True
    assert "NotAKey" in result["structuredContent"]["message"]


def test_a_partial_screenshot_region_is_rejected(mcp):
    """Three of four coordinates means a region was intended, not the screen."""
    result = mcp.call_tool("screenshot", {"x": 0, "y": 0, "width": 10})["result"]
    assert result["isError"] is True
    assert "height" in result["structuredContent"]["message"]


def test_action_presses_a_button(mcp, app_pid):
    """By name, because an ordinal does not say which control it lands on.

    `button` alone is what `test_action_refuses_a_selector_that_matches_several`
    now forbids, but `button:nth(1)` is not the fix: document order on Windows
    starts with the title bar, so it pressed `Restore` / `Maximize` / `Close`
    rather than anything the app owns. That took the window out of the
    accessibility tree and every later test in the run with it, including two
    in `test_tree.py` that have nothing to do with MCP.

    On Electron it landed on Chromium's own chrome instead, whose buttons
    report `Action press not supported on button`.

    On Tauri it was worse than a failure. The app's first button navigates to
    `input-events.html`, a page carrying none of the widgets, so pressing it
    made every later test skip for want of a slider or a check box and the
    cell reported success while covering almost nothing.

    Every test app has an `OK` button, and `test_actions.py` already presses
    it by name through the CLI on every one of them — naming it is also what
    the `action` tool tells a model to do.
    """
    selector = 'button[name="OK"]'
    result = mcp.call_tool(
        "action", {"pid": app_pid, "action": "press", "selector": selector}
    )["result"]
    assert result["isError"] is False, result["content"]
    assert result["structuredContent"]["ok"] is True
    assert result["structuredContent"]["selector"] == selector


def test_action_refuses_a_selector_that_matches_several(mcp, app_pid):
    """The schema promises exactly one match, so acting on the first is a bug.

    An agent that writes `button[name*="Save"]` against an app with "Save" and
    "Save As" pressed the wrong one and was told `ok: true`.
    """
    found = mcp.call_tool("find", {"pid": app_pid, "selector": "button"})["result"]
    total = found["structuredContent"]["match_count"]
    if total < 2:
        pytest.skip("the app under test has fewer than two buttons")

    result = mcp.call_tool(
        "action", {"pid": app_pid, "action": "press", "selector": "button"}
    )["result"]
    assert result["isError"] is True, "acting on the first of several is the defect"
    structured = result["structuredContent"]
    assert structured["kind"] == "ambiguous_selector"
    assert structured["match_count"] == total
    candidates = structured["diagnosis"]["candidates"]
    assert candidates, "without the candidates the caller has to go find them again"
    # Both recoveries have to be readable off the message alone: clients on the
    # oldest revision this server speaks get no structuredContent at all.
    assert ":nth(n)" in structured["message"]
    assert "[name=" in structured["message"]


def test_a_refused_ambiguous_selector_lists_what_it_matched(mcp, app_pid):
    """The candidate list must be the way out, not just proof of the problem."""
    found = mcp.call_tool("find", {"pid": app_pid, "selector": "button"})["result"]
    if found["structuredContent"]["match_count"] < 2:
        pytest.skip("the app under test has fewer than two buttons")
    names = [m.get("name") for m in found["structuredContent"]["matches"] if m.get("name")]
    # A name shared by two buttons would be ambiguous in its own right, which
    # is a different case from the one under test.
    unique = [n for n in names if names.count(n) == 1 and '"' not in n]
    if not unique:
        pytest.skip("the app's buttons have no distinct names")

    result = mcp.call_tool(
        "action", {"pid": app_pid, "action": "press", "selector": "button"}
    )["result"]
    candidates = " ".join(result["structuredContent"]["diagnosis"]["candidates"])
    assert unique[0] in candidates, candidates

    # And the selector the candidate list points at is one the tool accepts.
    narrowed = mcp.call_tool(
        "action",
        {"pid": app_pid, "action": "focus", "selector": f'button[name="{unique[0]}"]'},
    )["result"]
    if narrowed["isError"]:
        # `focus` is advisory and not every AT bridge implements it; what must
        # not come back is a second complaint about the selector.
        assert narrowed["structuredContent"]["kind"] != "ambiguous_selector"


def test_find_says_what_it_did_see_when_nothing_matched(mcp, app_pid):
    """`find` is the tool whose whole job is finding things.

    Its miss used to be `{"kind": "no_match", "message": "no elements matched
    selector: ..."}` and nothing else, while `action`'s carried candidates and
    a scope snapshot for the same typo.
    """
    selector = 'button[name="Sbumit"]'
    result = mcp.call_tool("find", {"pid": app_pid, "selector": selector})["result"]
    assert result["isError"] is True
    structured = result["structuredContent"]
    assert structured["kind"] == "no_match"
    diagnosis = structured["diagnosis"]
    assert diagnosis["selector"] == selector, "as a field, not only inside the prose"
    assert diagnosis["candidates"], "a miss must name the near misses it found"
    assert diagnosis["scope"], "and describe where it looked"


def test_states_are_selectable_with_the_syntax_the_schema_advertises(mcp, app_pid):
    """The advertised example was `checkbox[checked]`: wrong role, wrong syntax."""
    tools = {t["name"]: t for t in mcp.request(1, "tools/list")["result"]["tools"]}
    description = tools["find"]["inputSchema"]["properties"]["selector"]["description"]
    assert 'check_box[checked="on"]' in description
    assert "checkbox[checked]" not in description

    result = mcp.call_tool(
        "find", {"pid": app_pid, "selector": 'check_box[checked="on"]'}
    )["result"]
    if result["isError"]:
        # No checked box in this app is fine. A syntax error is not.
        assert result["structuredContent"]["kind"] == "no_match", result["structuredContent"]


def test_set_numeric_value_moves_a_slider_in_one_call(mcp, app_pid):
    """Without this verb the only route from 51 to 88 was 37 `increment` calls."""
    found = mcp.call_tool(
        "find", {"pid": app_pid, "selector": "slider:nth(1)"}
    )["result"]
    if found["isError"]:
        pytest.skip("no slider in the app under test")
    element = found["structuredContent"]["matches"][0]
    if not {"numeric_value", "min_value", "max_value"} <= element.keys():
        pytest.skip("the slider reports no numeric range")

    low, high = element["min_value"], element["max_value"]
    target = low + (high - low) * 0.75
    if abs(target - element["numeric_value"]) < 1:
        target = low + (high - low) * 0.25

    result = mcp.call_tool(
        "action",
        {
            "pid": app_pid,
            "action": "set-numeric-value",
            "selector": "slider:nth(1)",
            "value": str(target),
        },
    )["result"]
    # The tool contract, which is what this suite owns: the verb reaches the
    # provider and the call is accepted.
    assert result["isError"] is False, result["content"]

    after = mcp.call_tool("find", {"pid": app_pid, "selector": "slider:nth(1)"})["result"]
    moved = after["structuredContent"]["matches"][0]["numeric_value"]
    if moved == pytest.approx(element["numeric_value"], abs=0.001):
        # Whether the *toolkit* honours the write is a platform property, not
        # an MCP one, and it is already owned by the python suite:
        # `test_slider_set_numeric_value` is xfail(strict=False) for "WebKit2GTK
        # / WKWebView: SetCurrentValue not reliable for HTML range inputs" and
        # for Qt AT-SPI2. Accepting-then-ignoring is exactly what the `action`
        # description warns `ok: true` means, so it is not a failure here.
        pytest.skip(
            f"this toolkit accepted set-numeric-value without moving the slider "
            f"(still {moved}); see test_slider_set_numeric_value in the python suite"
        )
    assert moved == pytest.approx(target, abs=1.0)


def test_a_bad_numeric_value_is_rejected_before_anything_waits(mcp, app_pid):
    """Parsed before the first platform call, so it cannot cost the auto-wait."""
    start = time.monotonic()
    result = mcp.call_tool(
        "action",
        {
            "pid": app_pid,
            "action": "set-numeric-value",
            "selector": "slider:nth(1)",
            "value": "loud",
        },
    )["result"]
    assert result["isError"] is True
    assert result["structuredContent"]["kind"] == "invalid_arguments"
    assert "loud" in result["structuredContent"]["message"]
    assert time.monotonic() - start < 3, "a bad argument must not spend the timeout"


def test_the_action_schema_offers_the_numeric_setter(mcp):
    properties = {
        t["name"]: t["inputSchema"].get("properties", {})
        for t in mcp.request(1, "tools/list")["result"]["tools"]
    }["action"]
    assert "set-numeric-value" in properties["action"]["enum"]
    assert "set-numeric-value" in properties["value"]["description"], (
        "a verb that needs a value must say what the value looks like"
    )


def test_an_unsupported_action_is_spelled_the_way_it_must_be_typed(mcp, app_pid):
    """The enum takes `show-menu`; the failure used to say `show_menu`."""
    found = mcp.call_tool("find", {"pid": app_pid, "selector": "static_text"})["result"]
    if found["isError"]:
        pytest.skip("no static text in the app under test")

    result = mcp.call_tool(
        "action", {"pid": app_pid, "action": "show-menu", "selector": "static_text:nth(1)"}
    )["result"]
    if not result["isError"]:
        pytest.skip("show-menu is supported on this element")
    structured = result["structuredContent"]
    if structured["kind"] != "action_not_supported":
        pytest.skip(f"show-menu failed as {structured['kind']}, not as unsupported")
    assert "show_menu" not in structured["message"], structured["message"]
    assert "show-menu" in structured["message"]


def test_a_missing_target_is_named_in_the_tools_own_vocabulary(mcp):
    """`--app` and `--pid` are CLI flags; an MCP caller passes `app` and `pid`."""
    result = mcp.call_tool("find", {"selector": "button"})["result"]
    assert result["isError"] is True
    message = result["structuredContent"]["message"]
    assert '"app"' in message and '"pid"' in message, message
    assert "--" not in message, f"no flags exist on this surface: {message}"


# ── Cross-entry-point parity ─────────────────────────────────────────────────


def test_all_entry_points_expose_the_same_tools():
    """The claim this suite exists for: one implementation, three launchers."""
    listings = {}
    for name, command in ENTRY_POINTS.items():
        session = McpSession(command)
        try:
            session.request(1, "initialize", {"protocolVersion": LEGACY_VERSION})
            listings[name] = session.request(2, "tools/list")["result"]["tools"]
        finally:
            session.kill()

    reference_name, reference = next(iter(listings.items()))
    for name, tools in listings.items():
        assert tools == reference, (
            f"the {name} entry point serves a different tool list than {reference_name}"
        )


def test_all_entry_points_report_the_same_version():
    versions = {}
    for name, command in ENTRY_POINTS.items():
        session = McpSession(command)
        try:
            result = session.request(1, "initialize", {"protocolVersion": LEGACY_VERSION})
            versions[name] = result["result"]["serverInfo"]["version"]
        finally:
            session.kill()
    assert len(set(versions.values())) == 1, f"version drift across launchers: {versions}"


# ── Exit codes ───────────────────────────────────────────────────────────────


def _run(command: list[str], *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [*command, *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=30,
    )


def test_usage_errors_exit_two_from_every_launcher(entry_point):
    """The documented contract used to hold only for the Rust binary.

    The Python console script mapped every failure to 1, so `xa11y find`
    with a bad flag was indistinguishable from a real operation failure.
    """
    result = _run(entry_point, "mcp", "--not-a-flag")
    assert result.returncode == 2, result.stderr
    assert "usage error:" in result.stderr


def test_usage_errors_are_not_double_prefixed(entry_point):
    result = _run(entry_point, "mcp", "--not-a-flag")
    assert "error: usage error:" not in result.stderr, "one prefix, not two"


def test_operation_failures_exit_one_from_every_launcher(entry_point):
    result = _run(entry_point, "find", "button", "--app", "NoSuchApplicationHere")
    assert result.returncode == 1, result.stderr


def test_success_exits_zero_from_every_launcher(entry_point):
    result = _run(entry_point, "apps")
    assert result.returncode == 0, result.stderr
