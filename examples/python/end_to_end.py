"""End-to-end xa11y example: drive the AccessKit test app from launch to teardown.

This script is a complete, copy-pasteable starting point for writing your first
xa11y test in Python. It targets the AccessKit test app shipped with this repo
(``test-apps/accesskit``) so it runs identically on Linux, macOS, and Windows.

What it demonstrates:

* Launching a test app and polling the accessibility API until the OS registers it.
* Dumping the tree (``App.dump``) to discover the role and name of every element
  before writing selectors.
* The ``Locator`` pattern with auto-waiting actions (``press``, ``set_value``).
* Wait helpers: ``wait_visible``, ``wait_until``.
* Reading element fields (``role``, ``name``, ``actions``, ``checked``).
* Subscribing to accessibility events with ``app.subscribe`` and the
  ``Subscription.wait_for`` helper.
* Tearing the subprocess down cleanly.

Run from the repo root:

    cargo build -p xa11y-test-app
    python examples/python/end_to_end.py

Prerequisites: ``xa11y`` installed (``pip install -e xa11y-python``). On macOS,
the Python interpreter needs accessibility permission. On Linux, an X server
and an AT-SPI registry must be running.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import xa11y

REPO_ROOT = Path(__file__).resolve().parents[2]
BINARY = REPO_ROOT / "target" / "debug" / ("xa11y-test-app.exe" if sys.platform == "win32" else "xa11y-test-app")
STARTUP_TIMEOUT = 30.0


def main() -> int:
    if not BINARY.exists():
        sys.exit(f"Build the test app first: cargo build -p xa11y-test-app (looked at {BINARY})")

    # 1. Launch the test app as a subprocess. The example owns its lifecycle so
    #    that a CI run never leaks processes between attempts.
    proc = subprocess.Popen([str(BINARY)])
    try:
        # 2. Wait for the accessibility API to expose the app. ``by_pid`` polls
        #    internally until the timeout elapses; ``App.by_name`` is the
        #    alternative for apps you didn't launch yourself.
        app = xa11y.App.by_pid(proc.pid, timeout=STARTUP_TIMEOUT)
        print(f"App registered: {app.name} (pid={app.pid})")

        # 3. Dump the tree once to discover the role/name of every element.
        #    Copy a selector out of this output instead of guessing.
        print("\n--- Tree (depth 4) ---")
        print(app.dump(max_depth=4))

        # 4. Locators auto-wait and re-resolve on every operation, so they
        #    stay correct even if the UI mutates between calls.
        submit = app.locator('button[name="Submit"]')
        submit.wait_visible(timeout=5.0)

        # 5. Read element properties. Locators with a single match expose the
        #    underlying ``Element`` via ``.element()``.
        button = submit.element()
        assert button.role == "button", button.role
        assert button.enabled, "Submit should be enabled at startup"
        assert "press" in button.actions, button.actions

        # 6. Press the primary button.
        submit.press()

        # 7. Drive a text input. ``wait_until`` polls until the predicate is
        #    true — preferable to a fixed ``time.sleep``.
        #
        #    Some platform providers don't implement editable-text writes for
        #    every widget (e.g. Linux AT-SPI's AccessKit bridge doesn't expose
        #    ``EditableText`` — surfaced as ``ActionNotSupportedError``).
        #    Real apps usually expose it via Qt/GTK; the test app here is
        #    pure AccessKit, so we tolerate the error explicitly rather than
        #    swallowing it silently.
        name_field = app.locator('text_field[name="Name"]')
        try:
            name_field.set_value("Ada Lovelace")
        except xa11y.ActionNotSupportedError:
            print("note: set_value not supported by this provider (e.g. Linux AT-SPI on AccessKit)")
        else:
            try:
                name_field.wait_until(
                    lambda el: el is not None and el.value == "Ada Lovelace",
                    timeout=2.0,
                )
            except xa11y.TimeoutError:
                # Some providers accept set_value but don't echo it back
                # through the tree. The call still went through.
                print("note: text value not echoed back via accessibility (adapter quirk)")

        # 8. Toggle the checkbox via the ``press`` semantic verb and confirm
        #    the new state with ``wait_until``.
        checkbox = app.locator('check_box[name="I agree to terms"]')
        before = checkbox.element().checked
        checkbox.press()
        checkbox.wait_until(lambda el: el is not None and el.checked != before, timeout=2.0)
        print(f"checkbox toggled: {before!r} -> {checkbox.element().checked!r}")

        # 9. Iterate matching elements. ``.elements()`` returns all matches.
        buttons = app.locator("button").elements()
        print(f"discovered {len(buttons)} buttons total")
        assert len(buttons) >= 2

        # 10. Subscribe to events, trigger a press, and wait for the next
        #     event. In real code you would filter the predicate by
        #     ``e.event_type`` and/or ``e.target`` fields. Here we just
        #     demonstrate the API — pressing Submit mutates ``status_text``
        #     on the test app so an event is guaranteed to fire shortly after.
        with app.subscribe() as sub:
            submit.press()
            event = sub.wait_for(lambda _e: True, timeout=5.0)
            target_name = event.target.name if event.target else None
            print(f"observed event: {event.event_type} on {target_name!r}")

        print("\nOK — example completed successfully.")
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
