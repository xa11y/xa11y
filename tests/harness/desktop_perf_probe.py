"""Time identical release CLI and persistent MCP workloads against a running app.

Build and start the fixture before running this probe; neither setup nor compilation
is included. No accessibility snapshots survive between requests. Example:
python tests/harness/desktop_perf_probe.py --binary target/release/xa11y.exe --pid 123
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import statistics
import subprocess
import time
from pathlib import Path


def timed(call):
    start = time.perf_counter()
    value = call()
    return value, time.perf_counter() - start


class Session:
    def __init__(self, binary):
        self.process = subprocess.Popen(
            [binary, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.sequence = 0

    def request(self, method, params):
        self.sequence += 1
        message = {
            "jsonrpc": "2.0",
            "id": self.sequence,
            "method": method,
            "params": params,
        }
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError("MCP server exited before responding")
        response = json.loads(line)
        assert response["id"] == self.sequence, response
        assert "error" not in response, response
        return response["result"]

    def tool(self, name, arguments):
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self):
        self.process.stdin.close()
        try:
            assert self.process.wait(timeout=10) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()


def run(binary, pid, repeats, window_actions=False):
    binary = str(Path(binary).resolve())
    report = {
        "binary": binary,
        "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
        "platform": platform.platform(),
        "pid": pid,
        "repeats": repeats,
        "window_actions": window_actions,
        "seconds": {},
    }
    samples = report["seconds"]
    if window_actions and platform.system() == "Darwin":
        report["unmeasured"] = [
            "minimize/restore: macOS removes minimized windows from fresh app enumeration"
        ]

    def record(name, call, expected_error=False):
        value, elapsed = timed(call)
        if isinstance(value, dict):
            assert value.get("isError", False) == expected_error, value
        samples.setdefault(name, []).append(elapsed)
        return value

    def cli(arguments, expected_code=0):
        result = subprocess.run(
            [binary, *arguments],
            capture_output=True,
            check=False,
            text=True,
            encoding="utf-8",
            timeout=120,
        )
        assert result.returncode == expected_code, (
            arguments,
            result.stdout,
            result.stderr,
        )
        return result

    session, spawn = timed(lambda: Session(binary))
    samples["mcp.spawn"] = [spawn]
    try:
        record(
            "mcp.handshake",
            lambda: session.request(
                "initialize",
                {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                },
            ),
        )
        session.process.stdin.write(
            '{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
        )
        session.process.stdin.flush()
        first = record(
            "mcp.windows.cold", lambda: session.tool("windows", {"pid": pid})
        )
        windows = first["structuredContent"]["windows"]
        assert windows, "fixture needs a window"
        window = next(w for w in windows if w.get("name") and '"' not in w["name"])
        selector = f'{window["role"]}[name="{window["name"]}"]'
        report["selector"] = selector
        missing = "xa11y-perf-probe-no-such-application-7d2056"

        def wait_window(predicate):
            deadline = time.monotonic() + 5.0
            last = None
            while time.monotonic() < deadline:
                found = session.tool(
                    "find", {"pid": pid, "selector": selector, "limit": 2}
                )
                assert not found["isError"], found
                matches = found["structuredContent"]["matches"]
                assert len(matches) == 1, matches
                last = matches[0]
                if predicate(last):
                    return last
                time.sleep(0.1)
            raise AssertionError(f"window did not complete operation: {last}")

        def window_workload(
            surface, action, restore, changed, restored, extra=None, restore_extra=None
        ):
            def act(verb, arguments):
                if surface == "mcp":
                    result = session.tool(
                        "action",
                        {
                            "pid": pid,
                            "selector": selector,
                            "action": verb,
                            **(arguments or {}),
                        },
                    )
                    assert not result["isError"], result
                else:
                    flags = [
                        part
                        for key, value in (arguments or {}).items()
                        for part in (f"--{key}", value)
                    ]
                    cli(["action", verb, selector, "--pid", str(pid), *flags])

            try:
                record(f"{surface}.{action}.dispatch", lambda: act(action, extra))
                record(f"{surface}.{action}.observe", lambda: wait_window(changed))
            finally:
                record(
                    f"{surface}.{action}.restore_dispatch",
                    lambda: act(restore, restore_extra),
                )
                record(
                    f"{surface}.{action}.restore_observe", lambda: wait_window(restored)
                )

        for _ in range(repeats):
            record("cli.help", lambda: cli(["--help"]))
            record("cli.windows", lambda: cli(["windows", "--pid", str(pid)]))
            record("cli.find", lambda: cli(["find", selector, "--pid", str(pid)]))
            record("cli.buttons", lambda: cli(["find", "button", "--pid", str(pid)]))
            record("cli.app_miss", lambda: cli(["windows", "--app", missing], 1))
            record("mcp.windows.warm", lambda: session.tool("windows", {"pid": pid}))
            record(
                "mcp.find.warm",
                lambda: session.tool("find", {"pid": pid, "selector": selector}),
            )
            record(
                "mcp.buttons.warm",
                lambda: session.tool(
                    "find", {"pid": pid, "selector": "button", "limit": 50}
                ),
            )
            record(
                "mcp.app_miss.warm",
                lambda: session.tool("windows", {"app": missing}),
                True,
            )
            record(
                "mcp.explicit_backend.windows",
                lambda: session.tool("windows", {"pid": pid, "backend": "native"}),
            )
            if window_actions:
                actions = window["actions"]
                bounds = window.get("bounds")
                for surface in ("mcp", "cli"):
                    if (
                        "minimize" in actions
                        and "restore" in actions
                        and platform.system() != "Darwin"
                    ):
                        record(
                            f"{surface}.minimize_restore",
                            lambda surface=surface: window_workload(
                                surface,
                                "minimize",
                                "restore",
                                lambda w: w["states"].get("minimized") is True,
                                lambda w: w["states"].get("minimized") is False,
                            ),
                        )
                    if "maximize" in actions and "restore" in actions:
                        record(
                            f"{surface}.maximize_restore",
                            lambda surface=surface: window_workload(
                                surface,
                                "maximize",
                                "restore",
                                lambda w: w["states"].get("maximized") is True,
                                lambda w: w["states"].get("maximized") is False,
                            ),
                        )
                    if "enter_fullscreen" in actions and "restore" in actions:
                        record(
                            f"{surface}.fullscreen_restore",
                            lambda surface=surface: window_workload(
                                surface,
                                "enter-fullscreen",
                                "restore",
                                lambda w: w["states"].get("fullscreen") is True,
                                lambda w: w["states"].get("fullscreen") is False,
                            ),
                        )
                    if "move_to" in actions and bounds:
                        x, y = bounds["x"], bounds["y"]
                        record(
                            f"{surface}.move_restore",
                            lambda surface=surface, x=x, y=y: window_workload(
                                surface,
                                "move-to",
                                "move-to",
                                lambda w: (
                                    abs(w["bounds"]["x"] - x - 10) <= 2
                                    and abs(w["bounds"]["y"] - y - 10) <= 2
                                ),
                                lambda w: (
                                    abs(w["bounds"]["x"] - x) <= 2
                                    and abs(w["bounds"]["y"] - y) <= 2
                                ),
                                {"at": f"{x + 10},{y + 10}"},
                                {"at": f"{x},{y}"},
                            ),
                        )
    finally:
        session.close()
    report["medians"] = {
        name: statistics.median(values) for name, values in samples.items()
    }
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--window-actions",
        action="store_true",
        help="measure mutations and restore the fixture after each",
    )
    args = parser.parse_args()
    assert args.repeats > 0
    report = run(args.binary, args.pid, args.repeats, args.window_actions)
    output = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(output + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
