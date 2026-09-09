"""CLI integration tests for ``xa11y windows`` and the window verbs.

Each test runs the CLI against the live test app and verifies exit codes and
basic output. The unit test (`cmd_windows_rejects_positional_arg`) covers only
the negative argument case; this is the first real exercise of provider
enumeration, the filtered/unfiltered paths, output formatting, and the
geometry-argument dispatch for ``move-to`` / ``resize-to``.

Not every matrix app exposes a window-like element (the GTK app's AT-SPI tree
starts at a plain group, with no frame node), so a ``No windows found``
listing skips rather than fails; and a window-like element may be Role::Dialog
(the Cocoa app's top level), which the ``window, dialog`` selector and the
line-format check both accept.
"""

from __future__ import annotations

import re

import pytest

# Window-like roles, as ``format_element_oneline`` spells them.
WINDOW_LIKE_ROLES = ("window", "dialog")

# The opened-dialog selector for the close test, as a role+name filter.
#
# ``xa11y action`` resolves document-order first-match, so the bare
# ``window, dialog`` alternation the other verbs use matches the *main*
# window — which precedes the dialog and is present from the start — and
# ``close`` would dispatch to it, not to the dialog the test just opened.
# Every dialog-bearing matrix app names its dialog after "Dialog"
# (accesskit: "xa11y Test Dialog"; qt / gtk / wpf: "Sample Dialog") and no
# main window name contains it, so the contains filter (``[name*=..]``,
# case-insensitive) targets the opened dialog — and the auto-wait on
# ``close`` is what waits for the dialog to appear. The role alternation
# covers both shapes: AccessKit's dialog is Role::Window, Qt/GTK/WPF's is
# Role::Dialog.
DIALOG_SELECTOR = 'window[name*="Dialog"], dialog[name*="Dialog"]'


def _window_bounds(run_cli, app_pid) -> tuple[int, int, int, int]:
    """Return the first window-like line's ``bounds=(x,y,w,h)``.

    The ``windows`` listing renders an element's bounds as
    ``bounds=(x,y,w,h)`` (see ``format_element_oneline`` in
    ``xa11y/src/cli.rs``). A live, non-minimized window always carries them;
    a line without a usable (non-degenerate) bounds field skips the test,
    because these dispatch tests rely on the round-trip of current geometry.
    """
    rc, stdout, _ = run_cli("windows", "--pid", str(app_pid))
    assert rc == 0, "the windows listing must succeed"
    for line in stdout.splitlines():
        # Origins may be negative: a monitor left of or above the primary
        # display puts a window there, and such bounds are still valid
        # geometry. Width/height are unsigned, so only the first two fields
        # accept a sign.
        m = re.search(r"bounds=\((-?\d+),(-?\d+),(\d+),(\d+)\)", line)
        if m:
            x, y, w, h = (int(g) for g in m.groups())
            if w == 0 or h == 0:
                pytest.skip(
                    f"window bounds are degenerate ({w}x{h}); cannot dispatch non-mutating"
                )
            return (x, y, w, h)
    pytest.skip("window line carries no bounds; cannot dispatch non-mutating")


def _assert_windows_listing(rc: int, stdout: str, stderr: str) -> None:
    """Exit 0 with window lines (or a documented empty listing) and a summary."""
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    lines = stdout.strip().splitlines()
    assert lines, "expected non-empty output"
    if "No windows found" in stdout:
        pytest.skip("this app exposes no window-like element")
    summary = lines[-1]
    assert "window" in summary, f"expected a window summary, got {summary!r}"
    window_lines = lines[:-1]
    assert any(
        line.split()[0] in WINDOW_LIKE_ROLES for line in window_lines if line.split()
    ), f"no window-like lines:\n{stdout}"


def _require_windows(run_cli, app_pid) -> None:
    """Skip the geometry tests on apps that expose no window-like element.

    A selector that never matches (the GTK app's tree has no window node at
    all) times out with a *timeout*, not `Unsupported` — which would
    otherwise read as a dispatch regression when it is really the honest
    absence of a window. The `windows` listing is the unambiguous gate.
    """
    rc, stdout, _ = run_cli("windows", "--pid", str(app_pid))
    assert rc == 0, "the windows listing must succeed"
    if "No windows found" in stdout:
        pytest.skip("this app exposes no window-like element")


def test_windows_lists_the_test_app_by_pid(run_cli, app_pid):
    """``xa11y windows --pid PID`` lists the app's windows via the provider."""
    rc, stdout, stderr = run_cli("windows", "--pid", str(app_pid))
    _assert_windows_listing(rc, stdout, stderr)


def test_windows_unfiltered_includes_the_test_app(run_cli, app_pid):
    """The unfiltered path (all applications) surfaces the same window set.

    This is the path whose per-pid dedup keeps a window entry and a dialog of
    one process from double-listing. If the test app's window dropped out of
    the unfiltered listing, that would be a regression in the very path the
    dedup touches.

    The check keys on a window line captured from the filtered ``--pid``
    listing rather than on "some window-like line exists": on a desktop that
    is not empty (the container's window manager, another agent's app), an
    unrelated window satisfies the weak check even when the test app was
    dropped by the dedup.
    """
    rc, filtered, _ = run_cli("windows", "--pid", str(app_pid))
    assert rc == 0, "the filtered windows listing must succeed"
    if "No windows found" in filtered:
        pytest.skip("this app exposes no window-like element")
    # Role + quoted name is the stable, distinctive part of an oneline
    # (format_element_oneline): bounds and stable-id can differ between
    # invocations, the window's name does not.
    marker = None
    for line in filtered.splitlines():
        m = re.match(rf'^({"|".join(WINDOW_LIKE_ROLES)}) "(.+?)"', line)
        if m:
            marker = f'{m.group(1)} "{m.group(2)}"'
            break
    if marker is None:
        pytest.skip(
            "the filtered window line carries no name; cannot key the "
            "unfiltered check on it"
        )

    rc, stdout, stderr = run_cli("windows")
    assert rc == 0, f"expected exit 0, got {rc}\nstderr: {stderr}"
    if "No windows found" in stdout:
        pytest.skip("this app exposes no window-like element")
    assert marker in stdout, (
        f"the test app's window line {marker!r} is missing from the unfiltered "
        f"listing:\n{stdout}"
    )


def test_action_move_to_rejects_a_malformed_point_as_a_usage_error(run_cli, app_pid):
    """A bad ``--at`` must be refused at parse time, before any OS call.

    The CLI exits 2 for usage errors, and the message must name the flag —
    the unit tests verify the same on the mock; here it is the real surface.
    """
    rc, stdout, stderr = run_cli(
        "action",
        "move-to",
        "--at",
        "12,not-a-number",
        "window, dialog",
        "--pid",
        str(app_pid),
    )
    assert rc == 2, f"expected usage exit 2, got {rc}\nstderr: {stderr}"
    assert "--at" in stderr or "--at" in stdout, f"the flag must be named:\n{stderr}"


def test_action_move_to_dispatch_matches_the_round_trip(run_cli, app_pid):
    """A valid ``--at`` reaches the window verb.

    The selector is the ``window, dialog`` alternation because window-like
    elements surface as either role depending on the app. The dispatch uses
    the window's *current* position rather than a fixed point: the harness
    runs the ``cli`` suite against the shared app instance before
    ``python-window`` / ``js-window``, and a real move churns the UIA/AX
    cache exactly like the final-position suites were introduced to isolate
    (see ``tests/harness/launch.py``). Moving to the current coordinates
    keeps the call real while leaving the app's geometry untouched. Where
    the platform has no window-geometry API (Linux AT-SPI), the operation
    fails surfaceably with exit 1 — tolerated exactly like the other
    advisory actions in ``test_actions.py``; the semantics guard is the Rust
    integ suite, this is the dispatch surface.
    """
    _require_windows(run_cli, app_pid)
    x, y, _, _ = _window_bounds(run_cli, app_pid)
    rc, stdout, stderr = run_cli(
        "action", "move-to", "--at", f"{x},{y}", "window, dialog", "--pid", str(app_pid)
    )
    if rc != 0:
        lower = stderr.lower()
        assert "unsupported" in lower or "not supported" in lower, (
            f"unexpected failure:\nstderr: {stderr}"
        )
    else:
        assert "ok" in stdout, f"expected 'ok' in stdout, got: {stdout!r}"


def test_action_resize_to_requires_a_size_and_dispatches_it(run_cli, app_pid):
    """``resize-to`` without ``--size`` is a usage error; with it, dispatch.

    Splitting requirement and dispatch keeps the malformed case deterministic
    on every platform: parsing a missing argument fails before any OS call.
    Like the move-to dispatch test, the valid call resizes to the window's
    *current* size so the shared app's geometry is never left altered for the
    following window suites (see the move-to docstring and
    ``tests/harness/launch.py``).
    """
    rc, stdout, stderr = run_cli(
        "action", "resize-to", "window, dialog", "--pid", str(app_pid)
    )
    assert rc == 2, (
        f"expected usage exit 2 for a missing --size, got {rc}\nstderr: {stderr}"
    )
    assert "--size" in stderr or "--size" in stdout, (
        f"the flag must be named:\n{stderr}"
    )

    _require_windows(run_cli, app_pid)
    _, _, width, height = _window_bounds(run_cli, app_pid)
    rc, stdout, stderr = run_cli(
        "action",
        "resize-to",
        "--size",
        f"{width},{height}",
        "window, dialog",
        "--pid",
        str(app_pid),
    )
    if rc != 0:
        lower = stderr.lower()
        assert "unsupported" in lower or "not supported" in lower, (
            f"unexpected failure:\nstderr: {stderr}"
        )
    else:
        assert "ok" in stdout, f"expected 'ok' in stdout, got: {stdout!r}"


def _dispatch_window_verb(
    run_cli, app_pid, verb: str, selector: str = "window, dialog"
) -> bool:
    """Dispatch a nullary window verb; True when the platform performed it.

    A non-zero exit is tolerated only when the platform genuinely has no
    such action (the documented ``Unsupported`` wording): the selector had
    already matched a window-like element, so any other failure is a
    verb-to-method routing regression, not a capability gap (the unit tests
    cover the mock; this is the real CLI dispatch surface).
    """
    rc, stdout, stderr = run_cli("action", verb, selector, "--pid", str(app_pid))
    if rc != 0:
        lower = stderr.lower()
        assert "unsupported" in lower or "not supported" in lower, (
            f"unexpected failure for `{verb}`:\nstderr: {stderr}"
        )
        return False
    assert "ok" in stdout, f"expected 'ok' in stdout for `{verb}`, got: {stdout!r}"
    return True


def test_action_raise_dispatches(run_cli, app_pid):
    """``action raise`` is dispatched for a window-like selector."""
    _require_windows(run_cli, app_pid)
    _dispatch_window_verb(run_cli, app_pid, "raise")


def test_action_minimize_restore_round_trip_dispatches(run_cli, app_pid):
    """minimize then restore, both dispatched through the CLI.

    The window is restored right away so the shared app is left in its
    original state for the suites that follow; the restore only runs after
    a successful minimize (an unsupported minimize has nothing to undo).
    """
    _require_windows(run_cli, app_pid)
    if _dispatch_window_verb(run_cli, app_pid, "minimize"):
        _dispatch_window_verb(run_cli, app_pid, "restore")


def test_action_maximize_restore_round_trip_dispatches(run_cli, app_pid):
    """maximize then restore, both dispatched through the CLI."""
    _require_windows(run_cli, app_pid)
    if _dispatch_window_verb(run_cli, app_pid, "maximize"):
        _dispatch_window_verb(run_cli, app_pid, "restore")


def test_action_close_dispatches_on_an_opened_dialog(run_cli, app_pid):
    """``action close`` is dispatched against a dialog the test opens.

    The shared app's main window is never closed — that would kill the app
    the harness still needs; main-window close wiring stays covered by the
    mock bindings and the Rust integ suite. Apps whose tree has no
    ``Open Dialog`` button skip (not every CLI-matrix app exposes the
    test-app dialog).

    Unlike the other window verbs, the dispatch selector is
    ``DIALOG_SELECTOR`` rather than the ``window, dialog`` alternation: the
    locator's document-order first-match would resolve the main window
    (present from the start), dispatch ``close`` to it immediately, and the
    auto-wait would never wait for the dialog. The filtered selector matches
    only the opened dialog, so the auto-wait is what waits for the dialog to
    appear, and the dispatch lands where the test says it does.

    Where the platform has no close API the dispatch is unsupported; the
    ``finally`` block then closes the fixture dialog through its own
    ``Close Dialog`` button so the leftover modal cannot disturb the
    python-window / js-window suites that reuse this app afterwards.
    """
    rc, _, _ = run_cli(
        "action", "press", "button[name='Open Dialog']", "--pid", str(app_pid)
    )
    if rc != 0:
        pytest.skip(f"this app has no Open Dialog button (exit {rc})")
    rc = -1
    lower = ""
    try:
        rc, stdout, stderr = run_cli(
            "action", "close", DIALOG_SELECTOR, "--pid", str(app_pid)
        )
        lower = stderr.lower()
        if rc != 0:
            if "timeout" in lower:
                # The dialog never matched the name filter (an app whose dialog
                # is named differently, or whose dialog window the button could
                # not open). That is a missing fixture, not a dispatch failure:
                # assert-nothing was dispatched rather than risk the main window.
                pytest.skip(
                    f"no dialog window named with 'Dialog' appeared; nothing was closed "
                    f"(rc={rc}): {stderr}"
                )
            assert "unsupported" in lower or "not supported" in lower, (
                f"unexpected failure for `close`:\nstderr: {stderr}"
            )
        else:
            assert "ok" in stdout, (
                f"expected 'ok' in stdout for `close`, got: {stdout!r}"
            )
    finally:
        # Where the platform has no close API the dialog is still open, and
        # the harness reuses this app for the python-window / js-window suites
        # that follow — a leftover dialog changes their enumeration and
        # re-introduces exactly the cross-suite state the window suites
        # isolate. Close it through the fixture's own button (named
        # ``Close Dialog`` in every dialog-bearing app). A failure here is a
        # cleanup concern, not the dispatch claim: the assertion above already
        # decided the outcome. A timeout means no dialog ever appeared, so
        # there is nothing to close and nothing to wait for.
        if rc != 0 and "timeout" not in lower:
            run_cli(
                "action", "press", "button[name='Close Dialog']", "--pid", str(app_pid)
            )
