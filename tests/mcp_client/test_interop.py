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
    "events_start",
    "events_poll",
    "events_stop",
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


# ── Screenshot annotation ────────────────────────────────────────────────────

# `xa11y::MAX_ANNOTATIONS`, quoted in the tool description. Asserted against
# rather than copied: a cap the description misstates is worse than no cap.
ANNOTATION_CAP = 100


def test_the_annotated_screenshot_schema_reaches_both_protocol_eras(run_client, mode):
    """The new fields have to survive whichever listing a client asks for.

    `auto` takes the tool list from a stateless `server/discover` session,
    whose result the SDK's wire model requires `ttlMs` and `cacheScope` on;
    `legacy` takes it after the `initialize` handshake. A schema that only
    survived one of them would be invisible to half the deployed clients, and
    a listing that grew a field without its caching hints is rejected outright
    rather than degraded.
    """
    result = run_client(lambda c: c.list_tools())
    screenshot = next(t for t in result.tools if t.name == "screenshot")
    properties = screenshot.input_schema["properties"]
    assert properties["annotate"]["type"] == "array"
    assert properties["annotate"]["items"]["type"] == "string"
    assert set(properties["shell"]["enum"]) == SHELL_KINDS, "the shared target set"
    assert {"app", "pid", "x", "y", "width", "height"} <= properties.keys()
    assert screenshot.input_schema["required"] == [], "a plain capture takes nothing"
    if mode == "auto":
        assert result.ttl_ms >= 0
        assert result.cache_scope in {"public", "private"}


def test_a_client_that_validates_arguments_can_make_an_annotated_call(run_client):
    """`additionalProperties: false` refuses anything the tool did not declare.

    A client that checks arguments against `inputSchema` before sending them
    is the reason both halves of this feature have to be in one schema: the
    selectors and the target they resolve against.
    """
    schema = _tool(run_client, "screenshot").input_schema
    jsonschema.validate({"app": "Calculator", "annotate": ["button", "text_field"]}, schema)
    jsonschema.validate({}, schema)
    for rejected in ({"annotate": "button"}, {"annotate": [1]}):
        with pytest.raises(jsonschema.ValidationError):
            jsonschema.validate(rejected, schema)


def test_the_screenshot_tool_states_the_annotation_contract(run_client):
    """Each of these costs a model a call to discover by experiment."""
    description = _tool(run_client, "screenshot").description
    assert "no accessibility tree gets no annotations" in description
    assert "`B7` is the seventh match" in description, "spell the tag format out"
    assert ":nth(n)" in description, "and say what the number is for"
    assert "legend[i].selector" in description, "the round trip is the point"
    assert f"At most {ANNOTATION_CAP} elements" in description
    assert "`truncated` counts" in description
    # The precondition
    # `test_a_target_without_annotate_is_refused_rather_than_captured_full_screen`
    # enforces. A description that claims a target is simply optional here
    # would send a client into that refusal with no warning.
    assert "A target passed without `annotate` is refused" in description


def test_annotating_without_a_target_comes_back_as_something_to_fix(run_client):
    """Refused before the capture, so this holds on a machine with no display."""
    result = run_client(lambda c: c.call_tool("screenshot", {"annotate": ["button"]}))
    assert result.is_error is True
    structured = result.structured_content
    assert structured["kind"] == "invalid_arguments"
    message = structured["message"]
    assert '"app"' in message and '"pid"' in message, message
    assert "--" not in message, f"there are no flags on this surface: {message}"


def test_a_target_without_annotate_is_refused_rather_than_captured_full_screen(run_client):
    """A validating client can send every one of these, so the handler is the guard.

    `app`, `pid` and `shell` are read only to resolve `annotate` selectors, so
    each of these once came back as a whole-desktop capture with no error, and
    a client that asked to target an application had no way to tell from the
    result that its argument did nothing. The schema cannot express the rule:
    `jsonschema.validate` below asserts that these argument sets pass the
    `inputSchema` a real client checks against before sending, which is what
    leaves the refusal to the handler.

    One session for the whole set, and refused before any capture is taken, so
    this holds on a machine with no display.
    """
    schema = _tool(run_client, "screenshot").input_schema
    cases = [
        ({"pid": 1}, ('"pid"',)),
        ({"app": "Calculator"}, ('"app"',)),
        ({"shell": "taskbar"}, ('"shell"',)),
        # Refused for naming a target, which is also the `shell` property's own
        # promise about being paired with `app`.
        ({"app": "Calculator", "shell": "taskbar"}, ('"app"', '"shell"')),
        # A crop is not a target, so the region does not excuse the `pid`.
        ({"pid": 1, "x": 0, "y": 0, "width": 40, "height": 30}, ('"pid"',)),
        # An empty array is the plain-capture path too.
        ({"annotate": [], "pid": 1}, ('"pid"',)),
    ]
    for arguments, _named in cases:
        jsonschema.validate(arguments, schema)

    async def body(client):
        return [await client.call_tool("screenshot", a) for a, _ in cases]

    for (arguments, named), result in zip(cases, run_client(body), strict=True):
        assert result.is_error is True, arguments
        structured = result.structured_content
        assert structured["kind"] == "invalid_arguments", arguments
        message = structured["message"]
        for key in named:
            assert key in message, f"{arguments}: {key} unnamed in {message}"
        assert "annotate" in message, message
        assert "plain capture" in message, message


# ── Event subscriptions ──────────────────────────────────────────────────────


def test_the_event_tools_round_trip_a_handle_through_a_real_client(run_client):
    """The stateful-tools shape end to end, with no display and no app.

    `events_start` cannot open a subscription here — there is nothing to
    watch — so this drives the half that does not need one: an id the server
    never issued has to come back as a tool error the model can act on, with
    the open handles named.
    """
    result = run_client(
        lambda c: c.call_tool("events_poll", {"subscription_id": "sub_1"})
    )
    assert result.is_error is True
    structured = result.structured_content
    assert structured["kind"] in {"subscription_expired", "subscription_not_found"}
    assert structured["subscription_id"] == "sub_1"
    assert structured["live_subscriptions"] == []


def test_the_event_tools_declare_the_arguments_a_client_validates(run_client):
    start = _tool(run_client, "events_start")
    assert "shell" not in start.input_schema["properties"], (
        "events are subscribed per application"
    )
    kinds = start.input_schema["properties"]["kinds"]
    assert "focus_changed" in kinds["items"]["enum"]

    poll = _tool(run_client, "events_poll")
    assert poll.input_schema["required"] == ["subscription_id"]
    assert poll.input_schema["properties"]["timeout_ms"]["maximum"] == 15000


def test_events_start_states_its_handles_retention(run_client):
    """The spec asks a stateful tool to say how long its handle lives."""
    description = _tool(run_client, "events_start").description
    assert "reclaimed after" in description
    assert "subscription_expired" in description


def test_a_poll_with_a_bad_timeout_is_refused_rather_than_clamped(run_client):
    result = run_client(
        lambda c: c.call_tool(
            "events_poll", {"subscription_id": "sub_1", "timeout_ms": 600000}
        )
    )
    assert result.is_error is True
    assert result.structured_content["kind"] == "invalid_arguments"


def test_the_instructions_mention_watching_events(run_client):
    """A model that never learns the trio exists polls the tree instead."""
    instructions = run_client(lambda c: _identity(c.instructions))
    assert "events_start" in instructions


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
