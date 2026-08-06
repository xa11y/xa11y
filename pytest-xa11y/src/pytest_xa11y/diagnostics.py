"""What gets attached to a failing test.

A desktop test fails against a UI that no longer exists by the time anyone
reads the report. Re-running under manual tree dumps is the usual next step,
and it is exactly the step xa11y's structured errors were meant to remove.
This module extends the same idea from the exception to the pytest report:
when a test fails, capture what the tree looked like, what the platform's
focus state was, and what events had arrived — bounded, on the failure path
only.
"""

from __future__ import annotations

import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Callable

import xa11y

from .capabilities import SCREENSHOT, Capabilities
from .frontmost import macos_frontmost, macos_visible_processes

# Extra collectors registered by a suite. Each receives the live App and
# returns a string; a collector that raises is reported, never silently
# dropped, so a broken collector cannot quietly stop contributing.
_EXTRA_COLLECTORS: list[tuple[str, Callable[[xa11y.App], str]]] = []

# Attributes carried by xa11y's structured errors (tenet 6). Rendered as
# their own report block rather than left inside one long message string.
_DIAGNOSIS_FIELDS = ("condition", "selector", "elapsed", "last_observed", "candidates", "scope")


def register_diagnostic(name: str, collector: Callable[[xa11y.App], str]) -> None:
    """Add a suite-specific collector to every failure report.

    For state the plugin cannot know about — the contents of an app's own
    debug panel, a log view, a re-probe that distinguishes "lost a race" from
    "cannot work at all"::

        pytest_xa11y.register_diagnostic(
            "event log",
            lambda app: app.locator('text_area[name="Event log"]').element().value or "",
        )
    """
    _EXTRA_COLLECTORS.append((name, collector))


def clear_diagnostics() -> None:
    """Drop all registered collectors (used by the plugin's own tests)."""
    _EXTRA_COLLECTORS.clear()


def collect(
    app: xa11y.App | None,
    *,
    dump_depth: int,
    process_output: Sequence[str] = (),
    events: str | None = None,
    platform_state: bool = True,
) -> str:
    """Build the failure-report block for the app under test.

    ``platform_state`` covers the session-wide macOS facts (which app holds
    the front, which processes are visible). Those describe the desktop, not
    one app, so a caller reporting several live apps asks for them once — two
    ``osascript`` round trips per app would be repeated answers at a cost.
    """
    lines: list[str] = []

    if app is None:
        lines.append("app: <never started>")
    else:
        lines.append(_app_identity(app))
        lines.append(_tree_dump(app, dump_depth))

    if platform_state and sys.platform == "darwin":
        front_pid, front_name = macos_frontmost()
        lines.append(f"macOS frontmost: {front_name!r} (pid={front_pid})")
        lines.append(f"macOS visible processes: {macos_visible_processes()}")

    if events:
        lines.append(events)

    lines.extend(process_output)

    if app is not None:
        for name, collector in _EXTRA_COLLECTORS:
            try:
                lines.append(f"{name}: {collector(app)}")
            except Exception as exc:
                lines.append(f"{name}: <collector raised {exc!r}>")

    return "\n".join(lines)


def _app_identity(app: xa11y.App) -> str:
    try:
        return f"app: {app.name!r} (pid={app.pid})"
    except xa11y.XA11yError as exc:
        return f"app: <unreadable: {exc!r}>"


def _tree_dump(app: xa11y.App, depth: int) -> str:
    try:
        return f"accessibility tree (depth<={depth}):\n{app.dump(max_depth=depth)}"
    except xa11y.XA11yError as exc:
        return f"accessibility tree: <dump failed: {exc!r}>"


def render_diagnosis(exc: BaseException) -> str | None:
    """Render an xa11y error's structured diagnosis as its own report block.

    The same content is already inside the exception message, but as one run
    of prose. Pulling the fields out gives the reader the search scope and
    near-miss candidates as a labelled list, which is the part people
    actually scan for.
    """
    if not isinstance(exc, xa11y.XA11yError):
        return None
    present = [(field, getattr(exc, field, None)) for field in _DIAGNOSIS_FIELDS]
    present = [(field, value) for field, value in present if value not in (None, [], "")]
    if not present:
        return None

    # Formatted defensively. These attributes come from whatever xa11y the
    # consumer resolved, and the dependency is a lower bound with no upper
    # one: a future release that makes `candidates` a mapping or `elapsed` a
    # timedelta must not turn every failing test into an INTERNALERROR.
    lines = [f"{type(exc).__name__} diagnosis:"]
    for field, value in present:
        if field == "candidates" and isinstance(value, (list, tuple)):
            lines.append("  candidates:")
            lines.extend(f"    - {candidate}" for candidate in value)
        elif field == "elapsed" and isinstance(value, (int, float)):
            lines.append(f"  elapsed: {value:.2f}s")
        elif field == "scope":
            lines.append("  search scope (bounded):")
            lines.extend(f"    {line}" for line in str(value).splitlines())
        else:
            lines.append(f"  {field}: {value}")
    return "\n".join(lines)


def write_screenshot(
    app: xa11y.App,
    capabilities: Capabilities,
    directory: Path,
    stem: str,
) -> str | None:
    """Save a screenshot of the app's window; return the path, or a reason.

    Returns ``None`` when capture is unavailable in this session — that is
    reported once in the session header, not repeated per failure.
    """
    try:
        # Inside the try: `available()` probes, and a probe can raise. This
        # runs from `pytest_runtest_makereport`, where an exception is fatal —
        # pytest reports an INTERNALERROR and the failing test's own assertion
        # is never printed. Diagnostics must never be able to do that.
        if not capabilities.available(SCREENSHOT):
            return None
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / f"{stem}.png"
        # The app's own window, not the whole display: on a shared CI runner
        # the display is mostly other people's business, and the artifact is
        # meant to show what the test was looking at. Falls back to the full
        # display when the app exposes no window with bounds — a headless or
        # accessory app has nothing to frame.
        window = _app_window_bounds(app)
        if window is not None:
            xa11y.screenshot(region=window).save_png(path)
        else:
            xa11y.screenshot().save_png(path)
        return str(path)
    except Exception as exc:  # see above: this must never be fatal
        return f"<screenshot failed: {exc!r}>"


def _app_window_bounds(app: xa11y.App):
    """The app's active window rect, or ``None`` if it has no usable one."""
    try:
        bounds = app.locator("window").element().bounds
    except xa11y.XA11yError:
        return None
    if bounds is None or bounds.width <= 0 or bounds.height <= 0:
        return None
    return (bounds.x, bounds.y, bounds.width, bounds.height)
