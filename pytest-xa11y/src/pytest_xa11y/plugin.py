"""The pytest plugin: options, markers, fixtures, and report hooks."""

from __future__ import annotations

import contextlib
import os
import re
from collections.abc import Iterator
from pathlib import Path
from typing import Callable

import pytest
import xa11y

from .capabilities import INPUT_SIM, KNOWN_CAPABILITIES, Capabilities
from .diagnostics import collect, render_diagnosis, write_screenshot
from .errors import AppDied, LauncherNotConfigured
from .events import EventRecorder
from .frontmost import ensure_macos_frontmost
from .launcher import AppLauncher
from .session import AppSession

DEFAULT_STARTUP_TIMEOUT = 30.0
DEFAULT_DUMP_DEPTH = 12
DEFAULT_MAX_DIAGNOSTICS = 10

# Markers whose tests cannot share a machine with another test worker: both
# depend on process-global state (the frontmost slot, the system pointer and
# keyboard) that parallel workers would fight over.
_SERIAL_ONLY_MARKERS = ("xa11y_frontmost",)

# Every marker this plugin defines. Anything else under the reserved xa11y_
# prefix is a typo, and pytest_collection_modifyitems fails the run on it.
_KNOWN_MARKERS = ("xa11y_requires", "xa11y_frontmost")

_STATE_KEY = pytest.StashKey["_State"]()


class _State:
    """Per-session plugin state."""

    def __init__(self, config: pytest.Config) -> None:
        self.startup_timeout: float = config.getoption("xa11y_startup_timeout")
        self.dump_depth: int = config.getoption("xa11y_dump_depth")
        self.max_diagnostics: int = config.getoption("xa11y_max_diagnostics")
        artifacts = config.getoption("xa11y_artifacts")
        self.artifacts_dir: Path | None = Path(artifacts).resolve() if artifacts else None
        self.capabilities = Capabilities(tuple(config.getoption("xa11y_skip") or ()))
        # The session-scoped app, when one has been requested. Distinct from
        # `live` because `reset` is defined against this app specifically.
        self.session: AppSession | None = None
        # Every app currently running, session-scoped and per-test alike, in
        # start order. A single slot here would make the failure report
        # describe the session app while the test was driving a fresh one.
        self.live: list[AppSession] = []
        self.recorders: list[EventRecorder] = []
        self.diagnostics_emitted = 0

    def register(self, session: AppSession) -> None:
        self.live.append(session)

    def unregister(self, session: AppSession) -> None:
        if session in self.live:
            self.live.remove(session)
        if self.session is session:
            self.session = None


def _state(config: pytest.Config) -> _State:
    return config.stash[_STATE_KEY]


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    group = parser.getgroup("xa11y", "desktop UI testing with xa11y")
    group.addoption(
        "--xa11y-timeout",
        type=float,
        default=None,
        metavar="SECONDS",
        help=(
            "Default timeout for locator waits and auto-waiting actions. Calls "
            "xa11y.set_default_timeout(), so it outranks the XA11Y_DEFAULT_TIMEOUT "
            "environment variable; an explicit per-call timeout= still wins over both."
        ),
    )
    group.addoption(
        "--xa11y-startup-timeout",
        type=float,
        default=DEFAULT_STARTUP_TIMEOUT,
        metavar="SECONDS",
        help=(
            "How long to wait for the app to register with the accessibility API "
            f"and become ready (default: {DEFAULT_STARTUP_TIMEOUT:.0f}). An "
            "AppLauncher's own startup_timeout overrides this."
        ),
    )
    group.addoption(
        "--xa11y-artifacts",
        default=None,
        metavar="DIR",
        help=(
            "Write a screenshot of the app to DIR on each failing test. Off by "
            "default: capture costs time on the failure path, and a headless "
            "session has nothing to capture."
        ),
    )
    group.addoption(
        "--xa11y-skip",
        action="append",
        default=[],
        choices=list(KNOWN_CAPABILITIES),
        help=(
            "Declare a capability unavailable, skipping tests that need it. "
            "Repeatable. Needed for input_sim on macOS and Windows, where a "
            "missing grant cannot be detected — CGEventPost reports success "
            "whether or not the events are delivered."
        ),
    )
    group.addoption(
        "--xa11y-dump-depth",
        type=int,
        default=DEFAULT_DUMP_DEPTH,
        metavar="N",
        help=(
            "Depth of the accessibility-tree dump attached to failing tests "
            f"(default: {DEFAULT_DUMP_DEPTH})."
        ),
    )
    group.addoption(
        "--xa11y-max-diagnostics",
        type=int,
        default=DEFAULT_MAX_DIAGNOSTICS,
        metavar="N",
        help=(
            "Attach diagnostics to at most N failing tests per run (default: "
            f"{DEFAULT_MAX_DIAGNOSTICS}). A suite that fails wholesale should "
            "not bury its own report in identical tree dumps."
        ),
    )


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "xa11y_requires(*capabilities): skip unless every named capability "
        f"({', '.join(KNOWN_CAPABILITIES)}) is available in this session.",
    )
    config.addinivalue_line(
        "markers",
        "xa11y_frontmost: claim and verify the macOS frontmost slot before the "
        "test. No-op off macOS.",
    )
    config.stash[_STATE_KEY] = _State(config)

    timeout = config.getoption("xa11y_timeout")
    if timeout is not None:
        xa11y.set_default_timeout(timeout)


def pytest_report_header(config: pytest.Config) -> str:
    state = _state(config)
    parts = [f"startup timeout {state.startup_timeout:.0f}s"]
    timeout = config.getoption("xa11y_timeout")
    if timeout is not None:
        parts.append(f"default timeout {timeout:.1f}s")
    elif os.environ.get("XA11Y_DEFAULT_TIMEOUT"):
        parts.append(f"default timeout {os.environ['XA11Y_DEFAULT_TIMEOUT']}s (from env)")
    skipped = config.getoption("xa11y_skip")
    if skipped:
        parts.append(f"capabilities disabled: {', '.join(skipped)}")
    if state.artifacts_dir is not None:
        parts.append(f"artifacts {state.artifacts_dir}")
    return "xa11y: " + ", ".join(parts)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def xa11y_launcher() -> AppLauncher:
    """Override this to say how the app under test is started.

    The plugin ships no default: there is no sensible guess at what to launch.
    """
    raise LauncherNotConfigured(
        "pytest-xa11y needs an `xa11y_launcher` fixture. Define one in conftest.py:\n\n"
        "    import pytest\n"
        "    from pytest_xa11y import AppLauncher\n\n"
        "    @pytest.fixture(scope='session')\n"
        "    def xa11y_launcher():\n"
        "        return AppLauncher(\n"
        "            command=['./path/to/app'],\n"
        "            ready='button[name=\"Sign in\"]',\n"
        "        )\n"
    )


@pytest.fixture(scope="session")
def xa11y_capabilities(request: pytest.FixtureRequest) -> Capabilities:
    """What this session can exercise: screenshot capture, input synthesis."""
    return _state(request.config).capabilities


@pytest.fixture(scope="session")
def xa11y_artifacts(request: pytest.FixtureRequest) -> Path | None:
    """The artifacts directory, or ``None`` when ``--xa11y-artifacts`` is unset."""
    return _state(request.config).artifacts_dir


@pytest.fixture(scope="session")
def xa11y_app(request: pytest.FixtureRequest, xa11y_launcher: AppLauncher) -> Iterator[xa11y.App]:
    """The app under test, launched once for the whole session.

    Launching a desktop app costs seconds — process spawn, accessibility
    registration, content load — so this is deliberately session-scoped.
    Per-test state is handled by ``AppLauncher(reset=...)``, which runs before
    each test; use ``xa11y_fresh_app`` when a test genuinely needs a new
    process.
    """
    state = _state(request.config)
    session = AppSession(xa11y_launcher, startup_timeout=state.startup_timeout)
    state.session = session
    state.register(session)
    try:
        yield session.start()
    finally:
        session.stop()
        state.unregister(session)


@pytest.fixture
def xa11y_fresh_app(
    request: pytest.FixtureRequest, xa11y_launcher: AppLauncher
) -> Iterator[xa11y.App]:
    """A newly launched app, torn down at the end of the test.

    For the cases a reset cannot reach: first-run flows, restart behaviour,
    crash recovery.
    """
    state = _state(request.config)
    session = AppSession(xa11y_launcher, startup_timeout=state.startup_timeout)
    state.register(session)
    try:
        yield session.start()
    finally:
        session.stop()
        state.unregister(session)


@pytest.fixture(scope="session")
def xa11y_app_factory(
    request: pytest.FixtureRequest,
) -> Iterator[Callable[[AppLauncher], xa11y.App]]:
    """Launch additional apps, torn down at the end of the session.

    For suites that drive more than one process — or for a dialog that
    registers as its own accessibility application rather than appearing in
    the main window's tree, which is normal on Windows UIA.
    """
    state = _state(request.config)
    sessions: list[AppSession] = []

    def launch(launcher: AppLauncher) -> xa11y.App:
        session = AppSession(launcher, startup_timeout=state.startup_timeout)
        sessions.append(session)
        state.register(session)
        return session.start()

    try:
        yield launch
    finally:
        for session in reversed(sessions):
            session.stop()
            state.unregister(session)


@pytest.fixture
def xa11y_events(
    request: pytest.FixtureRequest,
) -> Iterator[Callable[[xa11y.App], EventRecorder]]:
    """Record accessibility events over a block of test code.

    Whatever a recorder saw is attached to the failure report, so a missing
    event shows what arrived instead.
    """
    state = _state(request.config)
    created: list[EventRecorder] = []

    def make(app: xa11y.App) -> EventRecorder:
        recorder = EventRecorder(app)
        created.append(recorder)
        state.recorders.append(recorder)
        return recorder

    try:
        yield make
    finally:
        for recorder in created:
            # Deregister first and close each one independently: a close() that
            # raises (its app just died, say) must not leave the recorder in
            # the session-wide list, where every later failure would render its
            # stale events as though they belonged to that test.
            if recorder in state.recorders:
                state.recorders.remove(recorder)
            with contextlib.suppress(Exception):
                recorder.close()


@pytest.fixture(autouse=True)
def _xa11y_per_test(request: pytest.FixtureRequest) -> Iterator[None]:
    """Liveness check, reset, and frontmost claim around each test.

    Autouse, but inert unless an app is running: a suite that never requests
    one never launches one.
    """
    state = _state(request.config)
    if not state.live:
        yield
        return

    # Every live app, not just the session one — a fresh or factory-launched
    # app that dies mid-run has to be reported too.
    for session in list(state.live):
        try:
            session.check_alive()
        except AppDied as exc:
            # End the run here. Every remaining test would fail on a lookup
            # against a process that no longer exists, burying the one message
            # that explains why under N copies of itself.
            request.session.shouldstop = f"xa11y: {exc}"
            raise

    if request.node.get_closest_marker("xa11y_frontmost"):
        # The most recently started app is the one this test is driving.
        target = state.live[-1]
        pid = target.app.pid if target.app is not None else None
        if pid is None:
            # App.pid is optional, and osascript would interpolate the None
            # into its query and then fail for ten seconds before reporting
            # something misleading about the frontmost slot.
            pytest.skip(
                f"cannot claim the macOS frontmost slot: "
                f"{target.launcher.display_name!r} reports no pid"
            )
        ok, detail = ensure_macos_frontmost(pid)
        if not ok:
            pytest.skip(detail)

    # `reset` is defined against the session app specifically: it is what
    # exists across tests and therefore what accumulates state. A fresh app
    # is new by construction and has nothing to reset.
    if state.session is not None:
        state.session.run_reset()
    yield


# ---------------------------------------------------------------------------
# Markers
# ---------------------------------------------------------------------------


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Reject markers that would silently guard nothing.

    pytest treats an unrecognised marker as a warning, and a warning in a
    thousand-line CI log is not a signal. Every way of writing one of this
    plugin's markers wrongly — a typo in the name, a capability that does not
    exist, ``xa11y_requires()`` with no arguments — otherwise produces a test
    that runs unguarded while reading as guarded. That is a test claiming
    coverage it does not have, which is the failure mode worth being strict
    about.

    The ``xa11y_`` marker prefix is reserved by this plugin for that reason.
    """
    problems: list[str] = []
    for item in items:
        for marker in item.iter_markers():
            if not marker.name.startswith("xa11y_"):
                continue
            if marker.name not in _KNOWN_MARKERS:
                problems.append(
                    f"{item.nodeid}: unknown marker @pytest.mark.{marker.name}. "
                    f"pytest-xa11y reserves the xa11y_ prefix; known markers are "
                    f"{', '.join(_KNOWN_MARKERS)}."
                )
                continue
            if marker.name != "xa11y_requires":
                continue
            if not marker.args:
                problems.append(
                    f"{item.nodeid}: @pytest.mark.xa11y_requires() needs at least one "
                    f"capability ({', '.join(KNOWN_CAPABILITIES)}); with none it guards "
                    f"nothing and the test runs as though it had no marker."
                )
            for name in marker.args:
                if name not in KNOWN_CAPABILITIES:
                    problems.append(
                        f"{item.nodeid}: unknown capability {name!r} in "
                        f"@pytest.mark.xa11y_requires; expected one of "
                        f"{', '.join(KNOWN_CAPABILITIES)}."
                    )

    if problems:
        raise pytest.UsageError("\n".join(problems))


def pytest_runtest_setup(item: pytest.Item) -> None:
    capabilities = _state(item.config).capabilities

    required = set()
    for marker in item.iter_markers("xa11y_requires"):
        required.update(marker.args)
    for name in sorted(required):
        capabilities.skip_unless(name)

    serial_only = bool(required & {INPUT_SIM}) or any(
        item.get_closest_marker(name) for name in _SERIAL_ONLY_MARKERS
    )
    if serial_only:
        workers = _worker_count(item.config)
        if workers > 1:
            pytest.skip(
                f"needs exclusive use of the desktop session, but pytest-xdist is "
                f"running {workers} workers. Input synthesis and the frontmost slot "
                f"are process-global: parallel workers steal them from each other. "
                f"Run these tests with -p no:xdist or -n0."
            )


def _worker_count(config: pytest.Config) -> int:
    """Number of xdist workers, or 1 when running serially."""
    workerinput = getattr(config, "workerinput", None)
    if not workerinput:
        return 1
    try:
        return int(workerinput.get("workercount", 1))
    except (TypeError, ValueError):
        return 1


# ---------------------------------------------------------------------------
# Failure reporting
# ---------------------------------------------------------------------------


def _artifact_stem(nodeid: str, label: str, index: int) -> str:
    """A filename-safe stem. Parametrised node ids carry characters Windows
    rejects (``:``, ``[``, ``]``, ``?``, ``*``), and the failed write was only
    reported as a lost artifact."""
    safe = re.sub(r"[^\w.-]", "_", f"{nodeid}-{label}")
    return f"{safe}-{index}" if index else safe


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo):
    outcome = yield
    report = outcome.get_result()
    if report.when != "call" or not report.failed:
        return

    state = _state(item.config)

    if call.excinfo is not None:
        diagnosis = render_diagnosis(call.excinfo.value)
        if diagnosis:
            report.sections.append(("xa11y diagnosis", diagnosis))

    running = [session for session in state.live if session.app is not None]
    if not running:
        return
    if state.diagnostics_emitted >= state.max_diagnostics:
        return
    state.diagnostics_emitted += 1

    events = None
    if state.recorders:
        events = "\n".join(recorder.render() for recorder in state.recorders)

    # Every live app gets a block. Reporting only one would describe the
    # session app while the test was driving a fresh one — a report that
    # confidently shows the wrong process's tree is worse than no report.
    blocks = []
    for index, session in enumerate(running):
        try:
            block = collect(
                session.app,
                dump_depth=state.dump_depth,
                process_output=session.output_tails(),
                # Events belong to the run, not to one app; attach them once.
                events=events if index == 0 else None,
            )
        except Exception as exc:
            block = f"<diagnostics collection raised {exc!r}>"

        if state.artifacts_dir is not None:
            saved = write_screenshot(
                session.app,
                state.capabilities,
                state.artifacts_dir,
                _artifact_stem(item.nodeid, session.launcher.display_name, index),
            )
            if saved:
                block += f"\nscreenshot: {saved}"

        if len(running) > 1:
            block = f"[{session.launcher.display_name}]\n{block}"
        blocks.append(block)

    report.sections.append(("xa11y app state", "\n\n".join(blocks)))

    if state.diagnostics_emitted == state.max_diagnostics:
        report.sections.append(
            ("xa11y app state", f"(diagnostics capped at {state.max_diagnostics} per run)")
        )
