"""Fixtures for driving `xa11y mcp` with the official MCP Python SDK.

Needs the `xa11y` binary and nothing else: no display, no accessibility bus,
no test application. Every assertion here is about protocol shape, and the
tool calls deliberately use targets that cannot resolve, so a failure result
is as informative as a successful one and the suite runs anywhere.
"""

from __future__ import annotations

import asyncio
import sys
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(PROJECT_ROOT))

from tests.harness.launch import (  # noqa: E402
    cli_binary_not_found_message,
    find_cli_binary,
)

# Revision the SDK speaks natively, and the newest handshake-era revision this
# server offers a `mode="legacy"` client. Kept in step with
# xa11y/src/mcp/protocol.rs.
MODERN_VERSION = "2026-07-28"
LEGACY_VERSION = "2025-11-25"

# The two ways a real client opens a session, and the reason this server is
# dual-era. `auto` probes with `server/discover` and stays stateless; `legacy`
# goes straight to the `initialize` handshake.
CONNECT_MODES = ("auto", "legacy")


def flatten_exception(exc: BaseException) -> list[BaseException]:
    """Flatten anyio's nested ExceptionGroups down to the real errors.

    The SDK runs its session in a task group, so a JSON-RPC error surfaces
    wrapped two or three groups deep. Asserting on `str(group)` gets you
    "unhandled errors in a TaskGroup" and tells you nothing.
    """
    if isinstance(exc, BaseExceptionGroup):
        return [leaf for sub in exc.exceptions for leaf in flatten_exception(sub)]
    return [exc]


@pytest.fixture(scope="session")
def cli_bin() -> str:
    found = find_cli_binary()
    if found is None:
        pytest.fail(cli_binary_not_found_message())
    return found


@pytest.fixture(params=CONNECT_MODES, ids=CONNECT_MODES)
def mode(request) -> str:
    """One connection mode. Every test runs once per protocol era."""
    return request.param


@pytest.fixture
def run_client(cli_bin: str, mode: str):
    """Return a runner that opens an SDK session and awaits `body(client)`.

    Each call launches a fresh `xa11y mcp` subprocess and shuts it down by
    closing the session, which is the spec's graceful-shutdown path.
    """
    from mcp import Client, StdioServerParameters, stdio_client

    def _run(body: Callable[[Any], Awaitable[Any]]) -> Any:
        async def _session():
            params = StdioServerParameters(command=cli_bin, args=["mcp"])
            client = Client(stdio_client(params), mode=mode)
            async with client:
                return await body(client)

        return asyncio.run(_session())

    return _run
