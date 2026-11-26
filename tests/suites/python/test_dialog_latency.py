"""Measure the public press and wait path against a real desktop provider."""

import os
import platform
import statistics
import time

import pytest


def test_dialog_latency(app, app_name):
    if app_name != "qt":
        pytest.skip("timed fixture is Qt")
    opener = app.locator('button[name="Open Dialog"]')
    closer = app.locator('button[name="Close Dialog"]')
    samples = []
    for trial in range(6):
        start = time.perf_counter_ns()
        opener.press()
        closer.wait_visible(timeout=5.0)
        elapsed = (time.perf_counter_ns() - start) / 1_000_000
        closer.press()
        closer.wait_hidden(timeout=5.0)
        if trial:
            samples.append(round(elapsed, 2))
    result = (
        f"{platform.system()} Qt press to visible dialog control: "
        f"median {statistics.median(samples)} ms, samples {samples}"
    )
    print(result)
    if summary := os.getenv("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as stream:
            print(result, file=stream)
