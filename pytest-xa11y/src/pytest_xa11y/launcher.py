"""The launch recipe: what to start, how to find it, when it is ready."""

from __future__ import annotations

import math
import os
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Callable

if TYPE_CHECKING:  # pragma: no cover - typing only
    import xa11y


@dataclass(frozen=True)
class AppLauncher:
    """A recipe for getting the app under test into a state tests can drive.

    Define an ``xa11y_launcher`` fixture returning one of these::

        @pytest.fixture(scope="session")
        def xa11y_launcher():
            return AppLauncher(
                command=[BINARY, "--headless"],
                env={"QT_ACCESSIBILITY": "1"},
                ready='button[name="Sign in"]',
            )

    Every field except ``command`` is optional; the defaults describe the
    common case of a self-contained binary that registers with the platform
    accessibility API under its own PID.

    Args:
        command: argv for the app, as a list. A bare string is rejected —
            splitting one is ambiguous on Windows, where the natural spelling
            (``C:\\Program Files\\App\\app.exe``) survives neither POSIX nor
            non-POSIX ``shlex`` rules intact.
        env: extra environment variables, merged over ``os.environ``.
        cwd: working directory for the subprocess.
        app_names: accessibility-tree names to match *in addition to* the
            spawned PID. Needed when the process that registers with the
            accessibility API is not the one launched — Electron helper
            processes, launchers that re-exec, and Windows shims that spawn a
            child and exit are the usual cases. Widening the match says
            nothing about which process to *watch*; see ``spawns_and_exits``.
        spawns_and_exits: the launched command hands off to another process
            and exits, so its exit is normal rather than a crash. Switches
            off death detection: this session then has no process whose
            lifetime is the app's, so a startup crash is reported as "never
            registered" and a mid-run exit is not noticed at all. Set it only
            for a genuine shim — leave it alone for an app that stays running
            under its own PID, even one that needs ``app_names`` to be found.
        app_name_prefix: match a candidate whose PID is ours *and* whose name
            starts with this. The opposite pairing to ``app_names``, and the
            one to reach for when a single process registers several
            accessibility apps: a Qt dialog hosted inside a DCC application
            appears as its own app sharing the host's PID on Windows UIA, and
            matching on PID alone would attach to the host. Mutually exclusive
            with ``app_names``, which would widen what this narrows.
        ready: a selector that must resolve before the first test runs.
            Guards against the window existing while its content is still
            loading, which is the normal state of affairs for webview apps.
        startup_timeout: seconds to wait for the app to appear and become
            ready. ``None`` uses the plugin default (``--xa11y-startup-timeout``).
        frontmost: claim and verify the macOS frontmost slot before yielding.
            Set it for apps whose tests depend on holding the front —
            ``CGEventPost`` delivers synthetic input there, and OS focus
            assertions read it. No-op off macOS.
        reset: called before each test to return the app to a known state
            (navigate home, clear a log, restore focus). Receives the
            ``xa11y.App``. Leave unset for a stateless app.
        attach_pid: attach to an already-running process instead of
            launching one. Mutually exclusive with ``command``; the plugin
            never terminates a process it did not start.
        label: name used in diagnostics and artifact filenames. Defaults to
            the command's basename.
    """

    command: Sequence[str] = ()
    env: Mapping[str, str] | None = None
    cwd: str | Path | None = None
    app_names: Sequence[str] = ()
    app_name_prefix: str | None = None
    spawns_and_exits: bool = False
    ready: str | None = None
    startup_timeout: float | None = None
    frontmost: bool = False
    reset: Callable[[xa11y.App], None] | None = field(default=None)
    attach_pid: int | None = None
    label: str | None = None

    def __post_init__(self) -> None:
        if isinstance(self.command, (str, bytes)):
            raise ValueError(
                "AppLauncher(command=...) takes a list of arguments, not a string: "
                f"pass [{self.command!r}] instead. Splitting a command string is "
                "ambiguous on Windows, where a quoted path with spaces survives "
                "neither shlex rule intact."
            )
        object.__setattr__(self, "command", tuple(self.command))
        object.__setattr__(self, "app_names", tuple(self.app_names))

        if self.attach_pid is None and not self.command:
            raise ValueError(
                "AppLauncher needs either command=[...] to launch an app or "
                "attach_pid=N to attach to a running one."
            )
        if self.attach_pid is not None and self.command:
            raise ValueError(
                "AppLauncher takes command= or attach_pid=, not both: "
                "attaching to a running process and launching a new one are "
                "different lifecycles (the plugin only terminates what it starts)."
            )
        if self.app_name_prefix is not None and self.app_names:
            raise ValueError(
                "AppLauncher takes app_names= or app_name_prefix=, not both: "
                "app_names widens the match to any process with that name, "
                "app_name_prefix narrows it to one app within our own process."
            )
        if self.attach_pid is not None and self.spawns_and_exits:
            raise ValueError(
                "AppLauncher takes attach_pid= or spawns_and_exits=, not both: "
                "spawns_and_exits describes a command this session launched, and "
                "attach mode launches nothing. The attached PID is the process to "
                "watch, whatever spawned it."
            )
        if self.attach_pid is not None and self.attach_pid <= 0:
            raise ValueError(f"AppLauncher(attach_pid={self.attach_pid!r}) must be positive.")
        if self.startup_timeout is not None:
            t = self.startup_timeout
            if not math.isfinite(t) or t <= 0:
                raise ValueError(
                    f"AppLauncher(startup_timeout={t!r}) must be a positive, finite "
                    "number of seconds."
                )
        if self.reset is not None and not callable(self.reset):
            raise ValueError("AppLauncher(reset=...) must be callable, taking the xa11y.App.")

    @property
    def display_name(self) -> str:
        """Human-readable label for diagnostics and artifact filenames."""
        if self.label:
            return self.label
        if self.command:
            return Path(self.command[0]).name
        return f"pid-{self.attach_pid}"

    def resolved_env(self) -> dict | None:
        """The subprocess environment: ``os.environ`` with ``env`` merged over it."""
        if not self.env:
            return None
        merged = os.environ.copy()
        merged.update(self.env)
        return merged
