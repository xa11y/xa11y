"""Which platform capabilities this session can actually exercise.

Screenshot capture and input synthesis both depend on grants and session
properties that no amount of correct test code can substitute for: Screen
Recording on macOS, a reachable X server or a working portal on Linux, an
interactive desktop session on Windows. A suite that treats their absence as
a failure is red for reasons the developer cannot act on; a suite that treats
it as a pass is not testing anything. The right answer is a skip that says
which grant is missing.
"""

from __future__ import annotations

import os
from collections.abc import Iterator
from contextlib import contextmanager
from enum import Enum

import pytest
import xa11y


class Capability(str, Enum):
    """The capabilities a session may or may not be able to exercise.

    A ``str`` subclass, so a plain string keeps working everywhere a
    capability is named — in the marker, on the command line, in
    ``guard()``. The enum exists for completion and for a spelling that a
    type checker can see; it is not a second vocabulary.
    """

    SCREENSHOT = "screenshot"
    INPUT_SIM = "input_sim"

    def __str__(self) -> str:  # pragma: no cover - trivial
        # `str, Enum` renders as "Capability.SCREENSHOT" in f-strings before
        # 3.11's StrEnum; messages should show the name users write.
        return self.value


SCREENSHOT = Capability.SCREENSHOT.value
INPUT_SIM = Capability.INPUT_SIM.value
KNOWN_CAPABILITIES = tuple(member.value for member in Capability)

# Platform errors that mean "this session has no usable capture path", as
# opposed to "capture is broken". Matched on message because the platform
# layer reports them as a generic Error::Platform with the OS's own wording:
# X11 rejects a region capture outside the root window's reported extents
# under a bare Xvfb, macOS surfaces a ScreenCaptureKit failure the same way,
# and a headless Linux session reports an unsupported backend inside a
# Platform error rather than as Error::Unsupported.
#
# This list is the whole definition of "unavailable". Anything else is a real
# failure and is re-raised — a capture pipeline that has genuinely broken must
# not be able to turn a suite green by skipping.
_CAPTURE_UNAVAILABLE_MARKERS = (
    "GetImage",
    "BadMatch",
    "SCScreenshotManager",
    "no DISPLAY or WAYLAND_DISPLAY",
    "Unsupported: screenshot",
)


class Capabilities:
    """Probes and records what this session can do. One per test session."""

    def __init__(self, disabled: tuple[str, ...] = ()) -> None:
        self._disabled = tuple(disabled)
        self._cache: dict[str, tuple[bool, str | None]] = {}
        # Probes that raised. Cached so the failure is reported once, not
        # re-attempted for every test that asks.
        self._failures: dict[str, tuple[type, str]] = {}

    def _declared_unavailable(self, name: str) -> str | None:
        """Reason this capability was switched off out of band, if it was."""
        if name in self._disabled:
            return f"disabled via --xa11y-skip={name}"
        if name == INPUT_SIM and os.environ.get("XA11Y_SKIP_INPUT_SIM") == "1":
            return "disabled via XA11Y_SKIP_INPUT_SIM=1"
        return None

    def available(self, name: str) -> bool:
        """Whether ``name`` can be exercised here."""
        return self.check(name)[0]

    def reason(self, name: str) -> str | None:
        """Why ``name`` is unavailable, or ``None`` when it is available."""
        return self.check(name)[1]

    def check(self, name: str) -> tuple[bool, str | None]:
        """Return ``(available, reason)``, probing at most once per session.

        "At most once" holds on the failure path too: a probe that raises has
        its exception cached and re-raised, the way pytest caches a fixture
        error. Without that, a thirty-test screenshot module would attempt
        thirty screen captures and print thirty copies of the same traceback.
        """
        if name not in KNOWN_CAPABILITIES:
            raise ValueError(
                f"Unknown capability {name!r}; expected one of {list(KNOWN_CAPABILITIES)}."
            )
        if name in self._failures:
            # A fresh exception each time. Re-raising one object appends to
            # its __traceback__ on every raise, so a large suite would grow a
            # single unboundedly long traceback.
            kind, message = self._failures[name]
            raise kind(message)
        if name not in self._cache:
            declared = self._declared_unavailable(name)
            if declared is not None:
                self._cache[name] = (False, declared)
                return self._cache[name]
            probe = _probe_screenshot if name == SCREENSHOT else _probe_input_sim
            try:
                self._cache[name] = probe()
            except xa11y.XA11yError as exc:
                # Say what was being done and what the way out is: the bare
                # platform error alone does not tell a reader that this was a
                # capability probe rather than their own call (tenet 6).
                enriched = type(exc)(
                    f"probing the {name!r} capability failed: {exc}. This is not one of "
                    f"the errors that mean the session simply lacks {name}, so it is "
                    f"reported rather than skipped. Pass --xa11y-skip={name} if this "
                    f"machine genuinely cannot do it."
                )
                self._failures[name] = (type(exc), str(enriched))
                raise enriched from exc
        return self._cache[name]

    def skip_unless(self, name: str) -> None:
        """Skip the current test unless ``name`` is available."""
        ok, reason = self.check(name)
        if not ok:
            pytest.skip(f"{name} unavailable: {reason}")

    @contextmanager
    def guard(self, name: str) -> Iterator[None]:
        """Turn a capability-unavailable error inside the block into a skip.

        Needed in addition to the marker because availability is not a single
        yes/no: a full-display capture can succeed in a session where a region
        capture is rejected, so the honest check happens at the call.

            with capabilities.guard("screenshot"):
                shot = xa11y.screenshot(region=(0, 0, 50, 40))
        """
        try:
            yield
        except xa11y.ActionNotSupportedError as exc:
            pytest.skip(f"{name} unsupported in this session: {exc}")
        except xa11y.PermissionDeniedError as exc:
            pytest.skip(f"{name} permission not granted: {exc}")
        except xa11y.PlatformError as exc:
            if name == SCREENSHOT and _is_capture_unavailable(exc):
                pytest.skip(f"{name} not available in this session: {exc}")
            raise

    def summary(self) -> str:
        """One line per capability, for the session header."""
        parts = []
        for name in KNOWN_CAPABILITIES:
            ok, reason = self.check(name)
            parts.append(f"{name}={'yes' if ok else 'no'}" + ("" if ok else f" ({reason})"))
        return ", ".join(parts)


def _is_capture_unavailable(exc: Exception) -> bool:
    message = str(exc)
    return any(marker in message for marker in _CAPTURE_UNAVAILABLE_MARKERS)


def _probe_screenshot() -> tuple[bool, str | None]:
    """Attempt a capture and report whether this session has a capture path.

    Two things this deliberately does not do.

    It does not treat an unrecognised ``PlatformError`` as unavailable. Only
    the errors that mean "no capture path here" — an unsupported backend, a
    missing grant, the known headless signatures — produce a skip; anything
    else propagates and fails the test. A capture pipeline that is genuinely
    broken must not turn a suite green by way of a module-wide skip, which is
    the same rule ``guard()`` follows. The two must agree: they are one policy
    applied at two moments.

    It does not probe with a region. A full-display capture can succeed in a
    session where a region capture is rejected (X11 rejects regions outside
    the root window's reported extents under a bare Xvfb), so probing with
    the more fragile of the two paths would skip modules whose captures would
    have worked. The probe asks the weakest question the marker can be taken
    to mean: can this session capture anything at all?
    """
    try:
        xa11y.screenshot()
    except xa11y.ActionNotSupportedError as exc:
        return False, f"unsupported in this session ({exc})"
    except xa11y.PermissionDeniedError as exc:
        return False, f"permission not granted ({exc})"
    except xa11y.PlatformError as exc:
        if _is_capture_unavailable(exc):
            return False, f"no capture path in this session ({exc})"
        raise
    return True, None


def _probe_input_sim() -> tuple[bool, str | None]:
    """Detect input synthesis, as far as any platform permits detecting it.

    Only Linux gives a real answer: both backends validate eagerly, so an
    unreachable X server or an unopenable ``/dev/uinput`` raises here.

    macOS and Windows cannot be probed. ``CGEventPost`` returns void, so
    without the Accessibility and Input Monitoring grants the events are
    simply discarded with no error at any layer — construction, the call, and
    the return value all look exactly like success. Windows constructs
    unconditionally and only reports trouble from ``SendInput``'s return
    count. On those platforms this reports available, and a session that
    genuinely lacks the grant must say so with ``--xa11y-skip=input_sim`` or
    ``XA11Y_SKIP_INPUT_SIM=1`` — which is why that switch exists rather than
    being a probe we pretend to have.
    """
    try:
        xa11y.input_sim()
    except xa11y.PermissionDeniedError as exc:
        return False, f"permission not granted ({exc})"
    except xa11y.ActionNotSupportedError as exc:
        return False, f"unsupported in this session ({exc})"
    except xa11y.PlatformError as exc:
        return False, f"no input backend in this session ({exc})"
    return True, None
