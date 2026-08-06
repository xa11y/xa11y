"""Process lifetime: launch failures, readiness, mid-run death, teardown.

``xa11y`` is swapped for a stand-in namespace so these run with no
accessibility bus and no GUI — the subprocesses are ordinary Python.
"""

from __future__ import annotations

import sys
import time
import types
from typing import ClassVar

import pytest
import xa11y

from pytest_xa11y import AppLauncher, AppSession
from pytest_xa11y import session as session_module
from pytest_xa11y.errors import AppDied, AppLaunchError
from pytest_xa11y.session import _tail

ALIVE = [sys.executable, "-c", "import time; time.sleep(60)"]
DIES = [sys.executable, "-c", "import sys; sys.stderr.write('boom\\n'); sys.exit(3)"]
CHATTY_THEN_DIES = [
    sys.executable,
    "-c",
    "import sys; sys.stdout.write('x' * 200000); sys.exit(1)",
]


class FakeApp:
    def __init__(self, pid=1, name="fake-app", dump="window"):
        self.pid = pid
        self.name = name
        self._dump = dump
        self.located = []

    def dump(self, max_depth=None):
        return self._dump

    def locator(self, selector):
        self.located.append(selector)
        return FakeLocator(selector)


class FakeLocator:
    ready_selectors: ClassVar[set] = set()

    def __init__(self, selector):
        self.selector = selector

    def wait_attached(self, timeout=None):
        if self.selector in FakeLocator.ready_selectors:
            return object()
        raise xa11y.TimeoutError(f"Timeout after {timeout}s; waiting for: attached")


def _fake_xa11y(*, find, listed=()):
    """A stand-in for the xa11y module exposing only what AppSession touches."""
    app_ns = types.SimpleNamespace(find=find, list=lambda: list(listed))
    return types.SimpleNamespace(
        App=app_ns,
        TimeoutError=xa11y.TimeoutError,
        SelectorNotMatchedError=xa11y.SelectorNotMatchedError,
        PlatformError=xa11y.PlatformError,
        XA11yError=xa11y.XA11yError,
    )


@pytest.fixture
def never_finds(monkeypatch):
    def find(predicate, timeout=None):
        time.sleep(0.05)
        raise xa11y.TimeoutError("Timeout; waiting for: app")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))


@pytest.fixture
def finds_immediately(monkeypatch):
    app = FakeApp()

    def find(predicate, timeout=None):
        return app

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    return app


def _session(command, **kwargs):
    launcher = AppLauncher(command=command, **kwargs)
    return AppSession(launcher, startup_timeout=kwargs.pop("startup_timeout", 2.0))


def test_missing_binary_names_the_command():
    session = AppSession(
        AppLauncher(command=["/nonexistent/xa11y-test-binary"]), startup_timeout=1.0
    )
    with pytest.raises(AppLaunchError, match="Could not launch"):
        session.start()


def test_early_exit_reports_code_and_stderr(never_finds):
    session = AppSession(AppLauncher(command=DIES), startup_timeout=5.0)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "exited during startup (code 3)" in message
    assert "boom" in message
    session.stop()


def test_large_output_does_not_deadlock_the_child(never_finds):
    # Captured to a temp file, not a pipe: 200 KB overflows a pipe buffer and
    # would block the child forever with nobody draining it.
    session = AppSession(AppLauncher(command=CHATTY_THEN_DIES), startup_timeout=10.0)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    assert "exited during startup (code 1)" in str(excinfo.value)
    assert "truncated" in str(excinfo.value)
    session.stop()


def test_app_never_registers_reports_scope_and_hint(monkeypatch):
    def find(predicate, timeout=None):
        time.sleep(0.05)
        raise xa11y.TimeoutError("Timeout")

    listed = [FakeApp(pid=9, name="Some Other App")]
    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find, listed=listed))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=0.3)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "did not register with the accessibility API" in message
    assert "Some Other App" in message
    assert "app_names" in message  # the hint


def test_platform_errors_during_startup_are_reported_not_discarded(monkeypatch):
    def find(predicate, timeout=None):
        time.sleep(0.05)
        raise xa11y.PlatformError("Platform error (2): bus not ready")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=0.3)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    assert "last accessibility error" in str(excinfo.value)
    assert "bus not ready" in str(excinfo.value)


def test_readiness_selector_gates_startup(finds_immediately, monkeypatch):
    monkeypatch.setattr(FakeLocator, "ready_selectors", set())
    session = AppSession(AppLauncher(command=ALIVE, ready='button[name="OK"]'), startup_timeout=0.3)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "content never became ready" in message
    assert 'button[name="OK"]' in message
    assert "tree:" in message


def test_readiness_selector_passes(finds_immediately, monkeypatch):
    monkeypatch.setattr(FakeLocator, "ready_selectors", {'button[name="OK"]'})
    session = AppSession(AppLauncher(command=ALIVE, ready='button[name="OK"]'), startup_timeout=2.0)
    try:
        assert session.start() is finds_immediately
    finally:
        session.stop()


def test_check_alive_raises_once_the_app_dies(finds_immediately):
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=2.0)
    session.start()
    session.check_alive()  # still running

    session.process.kill()
    session.process.wait(timeout=5)
    with pytest.raises(AppDied, match="exited mid-run"):
        session.check_alive()
    session.stop()


def test_stop_terminates_and_is_idempotent(finds_immediately):
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=2.0)
    session.start()
    process = session.process
    session.stop()
    session.stop()
    assert process.poll() is not None


def test_attach_never_terminates_a_process_it_did_not_start(finds_immediately):
    session = AppSession(AppLauncher(attach_pid=1234), startup_timeout=2.0)
    session.start()
    assert session.process is None
    session.stop()  # must not raise


def test_reset_errors_propagate(finds_immediately):
    def broken_reset(app):
        raise RuntimeError("reset target is gone")

    session = AppSession(AppLauncher(command=ALIVE, reset=broken_reset), startup_timeout=2.0)
    session.start()
    try:
        # A reset that has stopped working is reported, not swallowed — it is
        # the cause of the *next* test's inexplicable failure.
        with pytest.raises(RuntimeError, match="reset target is gone"):
            session.run_reset()
    finally:
        session.stop()


def _matcher_for(launcher, monkeypatch, candidates):
    """Run AppSession's discovery against a fixed candidate list."""
    seen = {}

    def find(predicate, timeout=None):
        for candidate in candidates:
            if predicate(candidate):
                seen["matched"] = candidate
                return candidate
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    return seen


class Candidate:
    def __init__(self, pid, name):
        self.pid = pid
        self.name = name


def test_app_name_prefix_narrows_within_our_own_process(monkeypatch):
    # A DCC-hosted Qt dialog registers as its own accessibility app sharing
    # the host's pid; matching on pid alone attaches to the host.
    session = AppSession(
        AppLauncher(attach_pid=99, app_name_prefix="Submit to "), startup_timeout=1.0
    )
    seen = _matcher_for(
        session.launcher,
        monkeypatch,
        [Candidate(99, "Cinema 4D"), Candidate(99, "Submit to AWS Deadline Cloud")],
    )
    session.start()
    assert seen["matched"].name == "Submit to AWS Deadline Cloud"


def test_without_a_prefix_the_host_process_wins(monkeypatch):
    # Documents the default: pid alone matches whichever app the platform
    # enumerates first for that process.
    session = AppSession(AppLauncher(attach_pid=99), startup_timeout=1.0)
    seen = _matcher_for(
        session.launcher,
        monkeypatch,
        [Candidate(99, "Cinema 4D"), Candidate(99, "Submit to AWS Deadline Cloud")],
    )
    session.start()
    assert seen["matched"].name == "Cinema 4D"


def test_app_names_widen_to_a_different_process(monkeypatch):
    session = AppSession(AppLauncher(attach_pid=1, app_names=["electron"]), startup_timeout=1.0)
    seen = _matcher_for(
        session.launcher, monkeypatch, [Candidate(7, "some-other"), Candidate(9, "Electron Helper")]
    )
    session.start()
    assert seen["matched"].pid == 9


def test_attach_mode_detects_a_dead_pid(finds_immediately):
    import subprocess as sp

    proc = sp.Popen(ALIVE)
    session = AppSession(AppLauncher(attach_pid=proc.pid), startup_timeout=2.0)
    session.start()
    session.check_alive()  # still running

    proc.kill()
    proc.wait(timeout=5)
    # Attach mode is the normal path in CI, so a liveness check that did
    # nothing there would be a feature that never runs where it is needed.
    with pytest.raises(AppDied, match="no longer running"):
        session.check_alive()


def test_liveness_reports_alive_when_it_cannot_tell(monkeypatch):
    # An inconclusive probe ends the whole run if it answers "dead", so it
    # must answer "alive". A false alive costs only the old behaviour.
    monkeypatch.setattr(session_module.sys, "platform", "win32")

    def no_tasklist(*args, **kwargs):
        raise FileNotFoundError("tasklist not found")

    monkeypatch.setattr(session_module.subprocess, "run", no_tasklist)
    assert session_module._pid_alive(4242) is True


def test_liveness_reports_alive_when_tasklist_errors(monkeypatch):
    import subprocess as sp

    monkeypatch.setattr(session_module.sys, "platform", "win32")
    monkeypatch.setattr(
        session_module.subprocess,
        "run",
        lambda *a, **k: sp.CompletedProcess(a, returncode=1, stdout="", stderr="boom"),
    )
    assert session_module._pid_alive(4242) is True


def test_app_discovery_makes_one_find_call_for_the_whole_budget(monkeypatch):
    # Not a chunked loop: core attaches a full app enumeration to each timeout,
    # so a caller that retries on timeout pays for one per iteration and throws
    # it away (the anti-pattern tenet 6 names).
    calls = []

    def find(predicate, timeout=None):
        calls.append(timeout)
        time.sleep(0.05)
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=1.0)
    with pytest.raises(AppLaunchError):
        session.start()
    session.stop()
    assert len(calls) == 1, f"expected one App.find call, got {len(calls)}"
    assert calls[0] == pytest.approx(1.0, abs=0.2)


def test_a_transient_platform_error_is_still_retried(monkeypatch):
    # The one case worth looping on: core propagates bus errors immediately
    # rather than polling through them, and they are transient during
    # accessibility registration.
    app = FakeApp()
    attempts = []

    def find(predicate, timeout=None):
        attempts.append(timeout)
        if len(attempts) < 3:
            raise xa11y.PlatformError("Platform error (2): bus not ready")
        return app

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=5.0)
    try:
        assert session.start() is app
    finally:
        session.stop()
    assert len(attempts) == 3


def test_a_process_that_dies_unenumerated_is_reported_as_a_crash(monkeypatch):
    # Nothing to enumerate means the predicate never runs, so death is only
    # noticed when the wait ends. It must still be reported as the crash it
    # is, not as an accessibility-registration failure.
    def find(predicate, timeout=None):
        time.sleep(0.2)
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=DIES), startup_timeout=1.0)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "exited during startup (code 3)" in message
    assert "did not register with the accessibility API" not in message


def test_the_predicate_aborts_the_search_when_the_process_dies(monkeypatch):
    # With candidates to evaluate, death is caught inside the predicate — one
    # poll tick rather than the whole timeout.
    def find(predicate, timeout=None):
        deadline = time.monotonic() + (timeout or 0)
        while time.monotonic() < deadline:
            predicate(FakeApp(pid=999, name="someone-else"))
            time.sleep(0.05)
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=DIES), startup_timeout=30.0)
    started = time.monotonic()
    with pytest.raises(AppLaunchError, match="exited during startup"):
        session.start()
    # Well inside the 30s budget: the abort came from the predicate.
    assert time.monotonic() - started < 10


def test_bus_errors_are_retried_with_a_throttle(monkeypatch):
    # The only looping branch, so it carries the throttle. Unthrottled it spins
    # at ~270k App.find calls a second and pins a core for the whole budget.
    attempts = []

    def find(predicate, timeout=None):
        attempts.append(time.monotonic())
        raise xa11y.PlatformError("Platform error (-1): no D-Bus session bus")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=1.0)
    with pytest.raises(AppLaunchError):
        session.start()
    session.stop()
    # 1s budget at a 0.25s throttle is a handful of attempts, not thousands.
    assert len(attempts) <= 8, f"{len(attempts)} attempts in 1s — the retry is not throttled"


def test_an_unusable_bus_is_named_as_the_cause(monkeypatch):
    def find(predicate, timeout=None):
        raise xa11y.PlatformError("Platform error (-1): no D-Bus session bus")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=0.6)
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    session.stop()
    message = str(excinfo.value)
    # The app is not at fault for the session having no accessibility API.
    assert "accessibility API is not usable in this session" in message
    assert "no D-Bus session bus" in message.splitlines()[0]


def test_spawns_and_exits_tolerates_the_launched_process_exiting(monkeypatch):
    # `spawns_and_exits` is for launcher shims that hand off and exit. For
    # those, our process exiting is normal — reporting it as a crash makes the
    # documented use case impossible.
    target = FakeApp(pid=424242, name="MyApp")

    def find(predicate, timeout=None):
        deadline = time.monotonic() + (timeout or 0)
        while time.monotonic() < deadline:
            if predicate(target):
                return target
            time.sleep(0.02)
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(
        AppLauncher(command=DIES, app_names=["MyApp"], spawns_and_exits=True),
        startup_timeout=5.0,
    )
    try:
        # The shim exits immediately; the predicate must not treat that as fatal.
        assert session.start() is target
        session.check_alive()  # and nor must the between-tests check
    finally:
        session.stop()


def test_app_names_alone_does_not_switch_off_death_detection(never_finds):
    # `app_names` widens the accessibility-tree match; the predicate still
    # tries the spawned PID first. An app that needs it (Electron, a Qt app
    # whose AT-SPI name lags its PID) is still an app we launched, and a crash
    # must still be reported as a crash — not as "never registered" after the
    # whole startup budget. Every launcher in this repo sets app_names.
    session = AppSession(
        AppLauncher(command=DIES, app_names=["xa11y-qt-test-app"]), startup_timeout=5.0
    )
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "exited during startup (code 3)" in message
    assert "boom" in message
    assert "did not register" not in message
    session.stop()


def test_app_names_alone_does_not_switch_off_the_liveness_check(finds_immediately):
    session = AppSession(
        AppLauncher(command=ALIVE, app_names=["xa11y-qt-test-app"]), startup_timeout=2.0
    )
    session.start()
    session.check_alive()

    session.process.kill()
    session.process.wait(timeout=5)
    with pytest.raises(AppDied, match="exited mid-run"):
        session.check_alive()
    session.stop()


def test_attach_mode_detects_a_dead_pid_even_with_app_names(finds_immediately):
    # The shape `tests/launchers.py` builds whenever the harness exports
    # XA11Y_TEST_APP_NAME, which is every attached CI run.
    import subprocess as sp

    proc = sp.Popen(ALIVE)
    session = AppSession(
        AppLauncher(attach_pid=proc.pid, app_names=["xa11y-qt-test-app"]), startup_timeout=2.0
    )
    session.start()
    session.check_alive()

    proc.kill()
    proc.wait(timeout=5)
    with pytest.raises(AppDied, match="no longer running"):
        session.check_alive()


def test_not_found_reports_the_real_process_state(monkeypatch):
    # A `spawns_and_exits` launcher is the one case with no death detection,
    # so this report is where its exit has to show up. Claiming "alive" for a
    # process that is gone points the reader away from the actual failure.
    def find(predicate, timeout=None):
        time.sleep(0.05)
        raise xa11y.TimeoutError("Timeout")

    monkeypatch.setattr(session_module, "xa11y", _fake_xa11y(find=find))
    session = AppSession(
        AppLauncher(command=DIES, app_names=["MyApp"], spawns_and_exits=True),
        startup_timeout=0.5,
    )
    with pytest.raises(AppLaunchError) as excinfo:
        session.start()
    message = str(excinfo.value)
    assert "process: exited (code 3)" in message
    assert "process: alive" not in message


def test_reading_the_output_tail_does_not_disturb_the_app_writing_it(never_finds):
    # `subprocess` gives the child a dup of the capture descriptor, and a dup
    # shares the file *offset*. Seeking the handle the child holds would move
    # where its next write lands, so a tail taken mid-run could punch a hole
    # in the log it is reporting. `output_tails()` runs on every failing test.
    import os as _os

    session = AppSession(
        AppLauncher(
            command=[
                sys.executable,
                "-u",
                "-c",
                "import sys\nfor i in range(500): sys.stdout.write('L%04d\\n' % i)\n",
            ]
        ),
        startup_timeout=2.0,
    )
    session._spawn()
    before = _os.lseek(session._stdout.fileno(), 0, _os.SEEK_CUR)
    for _ in range(50):
        session.output_tails()
    after = _os.lseek(session._stdout.fileno(), 0, _os.SEEK_CUR)
    assert after >= before, "the app's write offset moved backwards"

    session.process.wait(timeout=10)
    written = _os.stat(session._stdout_path).st_size
    assert written == 500 * 6, f"log is {written} bytes, expected {500 * 6}"

    tail = _tail(session._stdout_path)
    assert tail.endswith("L0499\n")
    assert len(tail.splitlines()[-1]) == 5
    session.stop()


def test_stop_removes_the_capture_files(finds_immediately):
    import os as _os

    session = AppSession(AppLauncher(command=ALIVE), startup_timeout=2.0)
    session.start()
    paths = [session._stdout_path, session._stderr_path]
    assert all(_os.path.exists(path) for path in paths)
    session.stop()
    assert not any(_os.path.exists(path) for path in paths)
