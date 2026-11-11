"""A synthesized double-click selects a word in a native text field.

On X11 the clicks of one ``click(count=2)`` went out with no gap between them,
and GTK took about one double-click in fifteen as two single clicks. Each round
double-clicks the first word of the field, types over it, and reads the value
back through the accessibility API, so a lost second press shows as typing
inside the word instead of over it.
"""

from __future__ import annotations

import os
import sys
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")

pytestmark = [
    pytest.mark.xa11y_requires("input_sim"),
    pytest.mark.skipif(
        not sys.platform.startswith("linux") or APP != "gtk",
        reason="the X11 multi-click case needs the GTK app's 'hello world' field",
    ),
]

ROUNDS = 3
SETTLE = 0.3
# Longer than the GTK double-click time, so one round cannot extend the last.
BETWEEN_ROUNDS = 0.8


def test_double_click_selects_the_word_every_time(app, app_config):
    field = app.locator(app_config["textfield_selector"])
    initial = app_config["textfield_initial_value"]
    sim = xa11y.input_sim()
    results = []
    try:
        for _ in range(ROUNDS):
            field.set_value(initial)
            time.sleep(BETWEEN_ROUNDS)
            bounds = field.element().bounds
            # 14 px in from the left edge lands inside "hello", the first word.
            sim.click((bounds.x + 14, bounds.y + bounds.height // 2), count=2)
            time.sleep(SETTLE)
            sim.type_text("X")
            time.sleep(SETTLE)
            results.append(field.element().value)
    finally:
        field.set_value(initial)
    missed = [v for v in results if v != "X world"]
    assert not missed, (
        f"{len(missed)} of {ROUNDS} double-clicks did not select the word: {missed}"
    )
