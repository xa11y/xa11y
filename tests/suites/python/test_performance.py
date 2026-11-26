"""Public API benchmarks on the same Qt fixture on Linux, macOS and Windows."""

import json
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
        if report := os.getenv("XA11Y_PERF_JSONL"):
            with open(report, "a", encoding="utf-8") as stream:
                print(
                    json.dumps(
                        {
                            "platform": platform.system(),
                            "app": app_name,
                            "operation": label,
                            "median_ms": statistics.median(samples),
                            "samples_ms": samples,
                        }
                    ),
                    file=stream,
                )
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
    measure("state filter", app.locator('button[enabled="true"]').elements,
            lambda nodes: any(node.name == "OK" for node in nodes))
    measure("descendant selector", app.locator('[name="Buttons"] button[name="OK"]').element,
            lambda node: node.name == "OK")
    measure("selector union", app.locator('button[name="OK"], button[name="Cancel"]').elements,
            lambda nodes: {node.name for node in nodes} == {"OK", "Cancel"})
    union = app.locator('text_field, button')
    measure("first union match", union.first().element, lambda node: node.role == "button")
    measure("union exists", union.exists, lambda found: found is True)
    measure("count metadata union", union.count,
            lambda count: count == len(union.elements()))
    measure("count named buttons", app.locator('button[name*="o"]').count,
            lambda count: count == len(app.locator('button[name*="o"]').elements()))
    measure("ready wait", ok.wait_enabled, lambda node: node.name == "OK")


@pytest.mark.skipif(platform.system() != "Linux", reason="AT-SPI query benchmarks")
def test_atspi_query_reads(app, measure):
    def identities(nodes):
        return [node.stable_id for node in nodes]

    buttons = app.locator("button").elements()
    expected_buttons = identities(buttons)
    assert len(expected_buttons) >= 3 and all(expected_buttons)

    overlapping = app.locator('button, button[name*="o"]')
    measure(
        "overlapping button union count", overlapping.count,
        lambda count: count == len(expected_buttons),
    )
    measure("overlapping button union exists", overlapping.exists, lambda found: found is True)
    measure(
        "overlapping button union elements", overlapping.elements,
        lambda nodes: identities(nodes) == expected_buttons,
    )

    expected_filtered = sum(
        node.enabled and any(letter in (node.name or "").lower() for letter in ("o", "k"))
        for node in buttons
    )
    assert expected_filtered > 0
    name_and_state = app.locator(
        'button[name*="o"][enabled="true"], button[name*="k"][enabled="true"]'
    )
    measure(
        "overlapping name and state count", name_and_state.count,
        lambda count: count == expected_filtered,
    )

    by_bus = app.locator('button[bus_name^=":"]')
    measure(
        "compound button reference lookup", by_bus.elements,
        lambda nodes: identities(nodes) == expected_buttons,
    )

    slider = app.locator("slider").element()
    value = slider.numeric_value
    assert value is not None
    numeric = app.locator(f"slider[numeric_value={json.dumps(str(value))}]")
    measure("numeric-value slider count", numeric.count, lambda count: count == 1)
    measure(
        "numeric-value slider lookup", numeric.element,
        lambda node: slider.same_as(node) and node.numeric_value == value,
    )


def test_stable_id_query(app, measure):
    button = app.locator('button[name="OK"]').element()
    if not button.stable_id:
        pytest.skip("Qt bridge does not expose a stable ID for this button")
    locator = app.locator(f"[stable_id={json.dumps(button.stable_id, ensure_ascii=False)}]")
    measure("resolve stable ID", locator.element, button.same_as)


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


@pytest.mark.skipif(platform.system() != "Windows", reason="UIA root deduplication benchmark")
def test_multi_window_reads(app, measure):
    closer = app.locator('button[name="Close Dialog"]')
    app.locator('button[name="Open Dialog"]').press()
    closer.wait_visible(timeout=5.0)
    try:
        assert len(app.windows()) >= 2
        # UIA can parent the dialog beneath the main window. Distinct tree
        # roots need not have the same count as native windows.
        measure("multi-window tree roots", app.children, bool)
        measure(
            "multi-window unique dialog control",
            closer.elements,
            lambda nodes: len(nodes) == 1 and nodes[0].name == "Close Dialog",
        )
    finally:
        closer.press()
        closer.wait_hidden(timeout=5.0)


@pytest.mark.skipif(platform.system() != "Linux", reason="measures the AT-SPI window listener")
def test_dialog_latency_with_subscription(app, measure):
    opener = app.locator('button[name="Open Dialog"]')
    closer = app.locator('button[name="Close Dialog"]')

    def sample_dialog(kind):
        return lambda event: (
            event.event_type == kind
            and event.target is not None
            and event.target.name == "Sample Dialog"
        )

    # Long-lived automations can reuse one listener. Register before pressing
    # so the transition cannot race setup. Validate the same control as the
    # ordinary press + wait_visible workload after the window event arrives.
    with app.subscribe(kinds=["window_opened", "window_closed"], buffer=16) as sub:
        def open_dialog():
            opener.press()
            sub.wait_for(sample_dialog(xa11y.EventType.WINDOW_OPENED), timeout=5.0)
            return closer.wait_visible(timeout=5.0)

        def close_dialog():
            closer.press()
            sub.wait_for(sample_dialog(xa11y.EventType.WINDOW_CLOSED), timeout=5.0)
            closer.wait_hidden(timeout=5.0)

        measure("press to visible dialog control with existing subscription", open_dialog,
                lambda node: node.name == "Close Dialog", close_dialog)
