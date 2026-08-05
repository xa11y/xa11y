"""The pytest plugin: options, markers, fixtures, and report hooks."""

from __future__ import annotations

import os
from collections.abc import Iterator
from pathlib import Path
from typing import Callable

import pytest
import xa11y

from .capabilities import INPUT_SIM, KNOWN_CAPABILITIES, Capabilities
from .diagnostics import collect, render_diagnosis, write_screenshot
from .errors import LauncherNotConfigured
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
        self.session: AppSession | None = None
        self.recorders: list[EventRecorder] = []
        self.diagnostics_emitted = 0


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
    try:
        yield session.start()
    finally:
        session.stop()
        state.session = None


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
    try:
        yield session.start()
    finally:
        session.stop()


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
        return session.start()

    try:
        yield launch
    finally:
        for session in reversed(sessions):
            session.stop()


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
            recorder.close()
            if recorder in state.recorders:
                state.recorders.remove(recorder)


@pytest.fixture(autouse=True)
def _xa11y_per_test(request: pytest.FixtureRequest) -> Iterator[None]:
    """Liveness check, reset, and frontmost claim around each test.

    Autouse, but inert unless a session app exists: a suite that never
    requests ``xa11y_app`` never launches one.
    """
    state = _state(request.config)
    session = state.session
    if session is None:
        yield
        return

    session.check_alive()
    if request.node.get_closest_marker("xa11y_frontmost") and session.app is not None:
        ok, detail = ensure_macos_frontmost(session.app.pid)
        if not ok:
            pytest.skip(detail)
    session.run_reset()
    yield


# ---------------------------------------------------------------------------
# Markers
# ---------------------------------------------------------------------------


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

    session = state.session
    if session is None or session.app is None:
        return
    if state.diagnostics_emitted >= state.max_diagnostics:
        return
    state.diagnostics_emitted += 1

    events = None
    if state.recorders:
        events = "\n".join(recorder.render() for recorder in state.recorders)

    try:
        block = collect(
            session.app,
            dump_depth=state.dump_depth,
            process_output=session.output_tails(),
            events=events,
        )
    except Exception as exc:
        block = f"<diagnostics collection raised {exc!r}>"

    if state.artifacts_dir is not None:
        stem = item.nodeid.replace("/", "_").replace("::", "__").replace(" ", "_")
        saved = write_screenshot(session.app, state.capabilities, state.artifacts_dir, stem)
        if saved:
            block += f"\nscreenshot: {saved}"

    report.sections.append(("xa11y app state", block))

    if state.diagnostics_emitted == state.max_diagnostics:
        report.sections.append(
            ("xa11y app state", f"(diagnostics capped at {state.max_diagnostics} per run)")
        )
