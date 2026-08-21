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


def test_apps_lists_the_running_test_app(mcp, app_name):
    result = mcp.call_tool("apps")["result"]
    assert result["isError"] is False
    structured = result["structuredContent"]
    assert structured["count"] == len(structured["applications"])
    names = [a["name"] for a in structured["applications"]]
    assert any(app_name in name for name in names), f"{app_name} not among {names}"


def test_tree_is_depth_limited_and_reports_truncation(mcp, app_name):
    result = mcp.call_tool("tree", {"app": app_name, "max_depth": 1})["result"]
    assert result["isError"] is False
    structured = result["structuredContent"]
    assert structured["max_depth"] == 1
    assert "truncated" in structured, "a shortened tree must say so"
    root = structured["tree"]
    assert root["role"], "every node carries a role"
    for child in root.get("children", []):
        assert "children" not in child, "max_depth=1 stops after direct children"


def test_tree_defaults_to_a_bounded_depth(mcp, app_name):
    structured = mcp.call_tool("tree", {"app": app_name})["result"]["structuredContent"]
    assert structured["max_depth"] == 12, "the default must be bounded, not unlimited"


def test_find_reports_bounds_and_a_precomputed_center(mcp, app_name):
    result = mcp.call_tool("find", {"app": app_name, "selector": "button"})["result"]
    assert result["isError"] is False
    structured = result["structuredContent"]
    assert structured["match_count"] >= 1
    first = structured["matches"][0]
    assert first["role"] == "button"
    bounds = first["bounds"]
    center = first["center"]
    assert center["x"] == bounds["x"] + bounds["width"] // 2
    assert center["y"] == bounds["y"] + bounds["height"] // 2


def test_find_respects_its_limit_and_flags_the_truncation(mcp, app_name):
    structured = mcp.call_tool(
        "find", {"app": app_name, "selector": "*", "limit": 1}
    )["result"]["structuredContent"]
    assert structured["returned"] == 1
    if structured["match_count"] > 1:
        assert structured["truncated"] is True


def test_element_payloads_omit_the_platform_blob(mcp, app_name):
    """`raw` and `handle` are large and meaningless to a model."""
    structured = mcp.call_tool(
        "find", {"app": app_name, "selector": "button", "limit": 1}
    )["result"]["structuredContent"]
    first = structured["matches"][0]
    assert "raw" not in first
    assert "handle" not in first


def test_a_failed_lookup_is_a_tool_error_carrying_its_diagnosis(mcp, app_name):
    """Tenet 6 reaching the model: the retry should be informed, not a guess."""
    response = mcp.call_tool(
        "find", {"app": app_name, "selector": 'button[name="DefinitelyNotHere"]'}
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


def test_action_presses_a_button(mcp, app_name):
    result = mcp.call_tool(
        "action", {"app": app_name, "action": "press", "selector": "button"}
    )["result"]
    assert result["isError"] is False, result["content"]
    assert result["structuredContent"]["ok"] is True


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
