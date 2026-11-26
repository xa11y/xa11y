"""Public API benchmarks on the same Qt fixture on Linux, macOS and Windows."""

import os
import platform
import statistics
import time

import pytest

import xa11y


@pytest.fixture
def measure(app_name, capsys):
    if app_name != "qt":
        pytest.skip("timed fixture is Qt")

    def run(label, operation, validate, reset=None):
        samples = []
        for trial in range(6):
            start = time.perf_counter_ns()
            try:
                result = operation()
                elapsed = (time.perf_counter_ns() - start) / 1_000_000
                assert validate(result), f"{label} returned an unexpected result"
            finally:
                if reset:
                    reset()
            if trial:
                samples.append(round(elapsed, 3))
        line = (
            f"{platform.system()} Qt {label}: median {statistics.median(samples)} ms, "
            f"samples {samples}"
        )
        with capsys.disabled():
            print(line, flush=True)
        if summary := os.getenv("GITHUB_STEP_SUMMARY"):
            with open(summary, "a", encoding="utf-8") as stream:
                print(f"- {line}", file=stream)

    return run


def test_discovery_and_tree_reads(app, measure):
    pid = app.pid
    measure("attach by PID", lambda: xa11y.App.by_pid(pid), lambda found: found.pid == pid)
    measure("enumerate windows", app.windows, bool)
    measure("read direct children", app.children, bool)
    measure("read shallow tree", lambda: app.tree(max_depth=2), lambda tree: bool(tree["children"]))
    measure("capture 32 nodes", lambda: app.capture(max_nodes=32), lambda cap: len(cap) == 32)


def test_locator_queries(app, measure):
    ok = app.locator('button[name="OK"]')
    buttons = app.locator("button")
    missing = app.locator('button[name="missing benchmark button"]')
    measure("exact name", ok.element, lambda node: node.name == "OK")
    measure("exists miss", missing.exists, lambda found: found is False)
    measure("first button", buttons.first().element, lambda node: node.role == "button")
    measure("count buttons", buttons.count, lambda count: count >= 3)
    measure("list buttons", buttons.elements, lambda nodes: len(nodes) >= 3)
    measure("state filter", app.locator('button[enabled="false"]').elements,
            lambda nodes: any(node.name == "Cancel" for node in nodes))
    measure("descendant selector", app.locator('[name="Buttons"] button[name="OK"]').element,
            lambda node: node.name == "OK")
    measure("selector union", app.locator('button[name="OK"], button[name="Cancel"]').elements,
            lambda nodes: {node.name for node in nodes} == {"OK", "Cancel"})
    measure("ready wait", ok.wait_enabled, lambda node: node.name == "OK")


def test_text_edit(app, measure):
    field = app.locator('text_field[name="Search"]')
    original = field.element().value or ""
    value = original + " benchmark"
    def wait_value(expected):
        field.wait_until(lambda node: node is not None and node.value == expected, timeout=5.0)
        return True

    def restore():
        field.set_value(original)
        wait_value(original)

    measure("set text", lambda: field.set_value(value), lambda _: wait_value(value), restore)


def test_dialog_latency(app, measure):
    opener = app.locator('button[name="Open Dialog"]')
    closer = app.locator('button[name="Close Dialog"]')

    def open_dialog():
        opener.press()
        return closer.wait_visible(timeout=5.0)

    def close_dialog():
        closer.press()
        closer.wait_hidden(timeout=5.0)

    measure("press to visible dialog control", open_dialog,
            lambda node: node.name == "Close Dialog", close_dialog)
