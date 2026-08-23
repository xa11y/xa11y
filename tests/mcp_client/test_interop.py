"""Interoperability: `xa11y mcp` driven by the official MCP Python SDK.

`tests/suites/cli/test_mcp.py` speaks raw JSON-RPC and asserts the shapes this
project believes the spec asks for. This suite asserts that a real client
agrees, which is a different question — and the difference is not academic.
The SDK's revision-pinned wire models mark as required several fields the
prose describes as MUST, and the server shipped `tools/list` and
`server/discover` without their mandatory caching hints until this suite ran.
A lenient hand-rolled parser cannot find that class of bug.

The two suites divide as follows. This one covers what a real client actually
emits: the era probe, the handshake, model-validated tool schemas, and result
parsing. The raw-JSON suite covers what the SDK will not emit — malformed
lines, unsupported protocol versions, notifications sent out of order — which
the SDK rejects client-side before they reach a socket.

Runs against the Rust binary only. Launcher parity is `test_mcp.py`'s job, and
the server bytes behind all three launchers are identical.
"""

from __future__ import annotations

import jsonschema
import pytest

from tests.mcp_client.conftest import LEGACY_VERSION, MODERN_VERSION, flatten_exception

pytestmark = pytest.mark.timeout(120)

EXPECTED_TOOLS = [
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
]

# Every `ShellSurfaceKind` spelling, as `shell` reports them and the `shell`
# argument accepts them. The Rust side derives its list from
# `ShellSurfaceKind::ALL`, so this set is not a copy of it but the assertion
# against it (`set(shell["enum"]) == SHELL_KINDS` below): adding a core variant
# fails here until someone confirms the new kind belongs on the wire.
SHELL_KINDS = {
    "menu_bar",
    "status_items",
    "taskbar",
    "panel",
    "dock",
    "desktop",
    "flyout",
    "unknown",
}


async def _identity(value):
    """Adapt a plain attribute read to the awaitable that `run_client` expects.

    Connection metadata (`protocol_version`, `server_info`) is a plain
    attribute on the SDK client, but it is only populated inside the session,
    so reading it has to happen in the same async body as the calls.
    """
    return value


# ── Connection ───────────────────────────────────────────────────────────────


def test_a_real_client_connects_in_both_protocol_eras(run_client, mode):
    """The reason the server is dual-era.

    `auto` sends `server/discover` and stays stateless; `legacy` opens with the
    `initialize` handshake. Deployed clients still do both.
    """
    negotiated = run_client(lambda c: _identity(c.protocol_version))
    expected = MODERN_VERSION if mode == "auto" else LEGACY_VERSION
    assert negotiated == expected


def test_server_identity_survives_model_validation(run_client):
    info = run_client(lambda c: _identity(c.server_info))
    assert info.name == "xa11y"
    assert info.version, "serverInfo must carry a version"


def test_the_tools_capability_is_advertised(run_client):
    caps = run_client(lambda c: _identity(c.server_capabilities))
    assert caps.tools is not None, "a server with tools must declare the capability"


def test_instructions_reach_the_client(run_client):
    """The only place to tell a model that selectors exist before it guesses."""
    instructions = run_client(lambda c: _identity(c.instructions))
    assert instructions
    assert "selector" in instructions.lower()


def test_ping_round_trips(run_client, mode):
    """`ping` was removed in 2026-07-28; the SDK only sends it under legacy.

    The server still answers it in both eras. Replying to a method a client
    will not send costs nothing, and refusing it would strand any client that
    still does.
    """
    if mode != "legacy":
        pytest.skip("the SDK does not send ping on the modern revisions")
    run_client(lambda c: c.send_ping())


# ── Tool listing ─────────────────────────────────────────────────────────────


def test_every_tool_parses_into_the_sdk_model(run_client):
    """Validates each `inputSchema` against a real parser rather than ours."""
    result = run_client(lambda c: c.list_tools())
    assert [t.name for t in result.tools] == EXPECTED_TOOLS
    for tool in result.tools:
        assert tool.description, f"{tool.name} needs a description"
        assert tool.input_schema["type"] == "object", f"{tool.name} schema"


def test_tool_schemas_are_valid_json_schema(run_client):
    """A malformed schema is silently useless: the model just guesses badly."""
    result = run_client(lambda c: c.list_tools())
    for tool in result.tools:
        jsonschema.Draft202012Validator.check_schema(tool.input_schema)


def test_required_properties_are_declared(run_client):
    result = run_client(lambda c: c.list_tools())
    for tool in result.tools:
        schema = tool.input_schema
        for name in schema.get("required", []):
            assert name in schema.get("properties", {}), (
                f"{tool.name} requires {name!r} without declaring it"
            )


def test_the_tool_list_is_stable_across_calls(run_client):
    """The spec asks for a deterministic order so clients can cache the list."""

    async def body(client):
        first = await client.list_tools()
        second = await client.list_tools()
        return [t.name for t in first.tools], [t.name for t in second.tools]

    first, second = run_client(body)
    assert first == second


def test_modern_results_carry_the_mandatory_caching_hints(run_client, mode):
    """The regression this suite was written to catch.

    The spec makes `ttlMs` and `cacheScope` a MUST on `resultType: "complete"`
    results from `tools/list` and `server/discover`. Omitting them is not a
    lenient degradation: the SDK's wire model for `2026-07-28` marks both
    required, so the client rejected the response outright.
    """
    if mode != "auto":
        pytest.skip("caching hints exist only on the modern revisions")
    result = run_client(lambda c: c.list_tools())
    assert result.ttl_ms >= 0
    assert result.cache_scope in {"public", "private"}


# ── Tool calls ───────────────────────────────────────────────────────────────


def test_a_failing_call_parses_as_a_tool_error(run_client):
    """A failure a model could fix must not arrive as a protocol error.

    The target cannot resolve, so this exercises the error path without a
    display or an accessibility bus.
    """
    result = run_client(
        lambda c: c.call_tool("find", {"selector": "button", "app": "NoSuchApplicationHere"})
    )
    assert result.is_error is True
    assert result.content, "an error result still needs content the model can read"
    assert result.content[0].type == "text"


def test_a_failing_call_carries_structured_context(run_client):
    """Tenet 6 across the wire: the retry should be informed, not a guess."""
    result = run_client(
        lambda c: c.call_tool("find", {"selector": "button", "app": "NoSuchApplicationHere"})
    )
    structured = result.structured_content
    assert structured is not None, "the SDK must be able to parse structuredContent"
    assert structured["tool"] == "find"
    assert structured["kind"], "every failure carries a machine-readable kind"
    assert structured["message"]


def test_an_argument_mistake_is_reported_as_something_to_fix(run_client):
    """A missing argument should be retryable, not a hard protocol error."""
    result = run_client(lambda c: c.call_tool("find", {"app": "whatever"}))
    assert result.is_error is True
    assert "selector" in result.structured_content["message"]


def test_a_missing_target_is_named_in_the_tools_own_vocabulary(run_client):
    """An MCP caller passes `app` and `pid`; `--app` and `--pid` are CLI flags.

    The handlers share `resolve_app` with the CLI, whose "specify one" message
    named the flags. A model reading it has no flag to reach for.
    """
    result = run_client(lambda c: c.call_tool("find", {"selector": "button"}))
    assert result.is_error is True
    message = result.structured_content["message"]
    assert '"app"' in message and '"pid"' in message, message
    assert "--" not in message, f"there are no flags on this surface: {message}"


# ── What the tool list tells a model ─────────────────────────────────────────


def _tool(run_client, name: str):
    result = run_client(lambda c: c.list_tools())
    return next(t for t in result.tools if t.name == name)


def test_the_action_verbs_include_setting_a_numeric_value(run_client):
    """`set-value` is text-only, which left sliders reachable only by stepping.

    `Locator::set_numeric_value` already existed in core; neither the CLI's
    verb list nor the tool that reads it surfaced it, so moving a slider from
    51 to 88 meant 37 `increment` round-trips.
    """
    action = _tool(run_client, "action")
    assert "set-numeric-value" in action.input_schema["properties"]["action"]["enum"]
    value = action.input_schema["properties"]["value"]["description"]
    assert "set-numeric-value" in value, f"the value format must be stated: {value}"


def test_the_action_tool_states_the_contract_a_caller_would_otherwise_guess(run_client):
    """Each of these was a wrong assumption an agent made against a live app."""
    description = _tool(run_client, "action").description
    assert "exactly one" in description, "it acts on one element, or refuses"
    assert "Auto-waits" in description, "a failing call blocks; say so"
    assert "XA11Y_DEFAULT_TIMEOUT" in description, "and name the knob that changes it"
    assert "not that anything changed" in description, "`ok: true` is not verification"


def test_the_element_tools_say_what_the_actions_field_is_not(run_client):
    """`actions` reads as a capability list and is not one.

    A slider advertising no actions still increments; a check box advertising
    only `press` still toggles. The field is what the application exposes
    through the platform's action interface, which is a different question
    from what the `action` tool accepts.
    """
    for name in ("tree", "find"):
        description = _tool(run_client, name).description
        assert "neither the set of verbs" in description, name
        assert "numeric_value" in description, f"{name} must name what to read instead"


def test_the_advertised_selector_example_is_valid_syntax(run_client):
    """`find` advertised `checkbox[checked]`, in which both halves are wrong.

    The role is `check_box`, and there is no presence-only attribute form —
    the selector engine answers `expected operator (=, *=, ^=, $=)`.
    """
    selector = _tool(run_client, "find").input_schema["properties"]["selector"]
    description = selector["description"]
    assert "checkbox[checked]" not in description
    assert 'check_box[checked="on"]' in description
    assert "presence-only" in description, "say why `[checked]` fails"
    assert ":nth(n)" in description, "and what the only pseudo-class is"


def test_the_shell_tool_round_trips_through_a_real_client(run_client):
    """Either answer is informative: this suite runs with no desktop at all.

    A listing parses as structured content; a machine with no accessibility
    bus produces a tool error. What must not happen is a protocol error, or a
    result the SDK cannot parse.
    """
    result = run_client(lambda c: c.call_tool("shell", {}))
    structured = result.structured_content
    assert structured is not None, "the SDK must be able to parse structuredContent"
    if result.is_error:
        assert structured["kind"] and structured["message"]
        return
    assert structured["count"] == len(structured["surfaces"])
    for surface in structured["surfaces"]:
        assert surface["kind"] in SHELL_KINDS, surface
        assert surface["name"]


def test_the_shell_tool_states_its_contract(run_client):
    """Enumeration is inert and a flyout is transient — neither is guessable."""
    description = _tool(run_client, "shell").description
    assert "listing is live" in description
    assert "only while it is open" in description
    assert "never opens or presses anything" in description
    assert "Show Hidden Icons" in description, "spell the overflow workflow out"


def test_the_element_tools_take_a_shell_surface_as_a_target(run_client):
    for name in ("tree", "find", "action"):
        shell = _tool(run_client, name).input_schema["properties"]["shell"]
        assert set(shell["enum"]) == SHELL_KINDS, name
        assert "Mutually exclusive with `app`" in shell["description"], name


def test_naming_both_an_app_and_a_shell_surface_is_a_fixable_error(run_client):
    result = run_client(
        lambda c: c.call_tool("find", {"selector": "button", "app": "X", "shell": "taskbar"})
    )
    assert result.is_error is True
    structured = result.structured_content
    assert structured["kind"] == "invalid_arguments"
    assert '"shell"' in structured["message"]
    assert "--" not in structured["message"], "there are no flags on this surface"


def test_the_instructions_mention_the_shell_surfaces(run_client):
    """The only place a model learns OS chrome is reachable at all."""
    instructions = run_client(lambda c: _identity(c.instructions))
    assert "shell" in instructions


def test_the_instructions_warn_that_ok_is_not_verification(run_client):
    instructions = run_client(lambda c: _identity(c.instructions))
    assert "exactly one element" in instructions
    assert "not that anything changed" in instructions


def test_an_unknown_tool_is_a_protocol_error(run_client):
    """Not fixable by adjusting arguments, so the spec puts it on the envelope.

    The SDK surfaces a JSON-RPC error by raising rather than by returning an
    `isError` result, which is the distinction being asserted: a model is shown
    tool errors to retry from, and protocol errors mean something else is
    wrong.
    """
    with pytest.raises(BaseException) as excinfo:
        run_client(lambda c: c.call_tool("teleport", {}))

    errors = flatten_exception(excinfo.value)
    codes = [getattr(e, "code", None) for e in errors]
    assert -32602 in codes, f"expected an invalid-params error, got {errors}"
    assert any("teleport" in str(e) for e in errors), errors


def test_a_session_survives_a_failed_call(run_client):
    """A tool error must not desynchronize the stream behind it."""

    async def body(client):
        await client.call_tool("find", {"selector": "button", "app": "NoSuchApplicationHere"})
        return await client.list_tools()

    result = run_client(body)
    assert [t.name for t in result.tools] == EXPECTED_TOOLS


def test_many_calls_stay_in_order_on_one_connection(run_client):
    """Framing holds across a long session: replies must match their requests."""

    async def body(client):
        seen = []
        for _ in range(10):
            result = await client.list_tools()
            seen.append(len(result.tools))
        return seen

    assert run_client(body) == [len(EXPECTED_TOOLS)] * 10
