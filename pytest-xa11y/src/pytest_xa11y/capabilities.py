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

import pytest
import xa11y

SCREENSHOT = "screenshot"
INPUT_SIM = "input_sim"
KNOWN_CAPABILITIES = (SCREENSHOT, INPUT_SIM)

# Platform errors that mean "this session has no usable capture path", as
# opposed to "capture is broken". Matched on message because the platform
# layer reports them as a generic Error::Platform with the OS's own wording:
# X11 rejects a region capture outside the root window's reported extents
# under a bare Xvfb, and macOS surfaces a ScreenCaptureKit failure the same
# way.
_CAPTURE_UNAVAILABLE_MARKERS = ("GetImage", "BadMatch", "SCScreenshotManager")


class Capabilities:
    """Probes and records what this session can do. One per test session."""

    def __init__(self, disabled: tuple[str, ...] = ()) -> None:
        self._disabled = tuple(disabled)
        self._cache: dict[str, tuple[bool, str | None]] = {}

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
        """Return ``(available, reason)``, probing at most once per session."""
        if name not in KNOWN_CAPABILITIES:
            raise ValueError(
                f"Unknown capability {name!r}; expected one of {list(KNOWN_CAPABILITIES)}."
            )
        if name not in self._cache:
            declared = self._declared_unavailable(name)
            if declared is not None:
                self._cache[name] = (False, declared)
            elif name == SCREENSHOT:
                self._cache[name] = _probe_screenshot()
            else:
                self._cache[name] = _probe_input_sim()
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
    """Attempt the smallest possible capture and see whether it lands."""
    try:
        xa11y.screenshot(region=(0, 0, 1, 1))
    except xa11y.ActionNotSupportedError as exc:
        return False, f"unsupported in this session ({exc})"
    except xa11y.PermissionDeniedError as exc:
        return False, f"permission not granted ({exc})"
    except xa11y.PlatformError as exc:
        if _is_capture_unavailable(exc):
            return False, f"no capture path in this session ({exc})"
        return False, f"capture failed ({exc})"
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
