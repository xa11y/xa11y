"""Turning xa11y failures into something an agent can act on.

xa11y attaches a structured diagnosis to its lookup failures — the selector, what
the wait was for, what it last saw, near-miss candidates, and a bounded dump of the
search scope. That is precisely the context a model needs to correct its own next
call, so it is passed through rather than flattened into "element not found".
"""

from __future__ import annotations

import importlib
from typing import Any, Dict, List, Optional

# Guidance keyed by xa11y exception class name. Each entry answers "what do I do now?"
# for a human reading the transcript and for the model reading the tool result.
_GUIDANCE = {
    "PermissionDeniedError": (
        "Accessibility permission has not been granted to the process running this agent.\n"
        "  macOS: System Settings > Privacy & Security > Accessibility, add the terminal or app "
        "hosting the agent, then restart it. Screenshots additionally need Screen & System Audio Recording.\n"
        "  Linux: make sure AT-SPI2 is running (standard on GNOME).\n"
        "  Windows: no grant needed; if this appears, the target app is likely running elevated "
        "while the agent is not."
    ),
    "AccessibilityNotEnabledError": (
        "The app is reachable but publishes an empty tree. Chromium and Electron apps only expose "
        "their accessibility tree when launched with --force-renderer-accessibility (or with "
        "ACCESSIBILITY_ENABLED=1 in the environment). Relaunch the app that way, or fall back to "
        "screenshot plus point-targeted input."
    ),
    # xa11y maps three distinct core errors onto this one class, and `describe`
    # keys on the class name alone — so the guidance has to cover all three
    # rather than assume the common one. Telling a model to "fall back to a
    # click" after Error::Unsupported sends it back to the mechanism that just
    # reported itself unavailable.
    "ActionNotSupportedError": (
        "Read the message before choosing a fallback. If it names an action on an element, that "
        "element does not expose that verb: use 'read' to see its 'actions' list, try a different "
        "verb, or fall back to a point-targeted click or a key press. If it begins 'Unsupported', "
        "the operation has no implementation on this platform or session at all (for example "
        "pointer warping on Wayland without a portal grant) — the input fallback is unavailable "
        "too, so change approach rather than retrying. If it is about a text value, the element "
        "accepts no text through the accessibility API; focus it and type instead."
    ),
    "InvalidSelectorError": (
        "Selector syntax is invalid. Roles are snake_case (button, text_field, check_box); "
        "attribute values must be quoted (button[name='Save']); combinators are ' ' (descendant) "
        "and '>' (direct child); position is :nth(1), 1-based."
    ),
    "InvalidActionDataError": "The action received an out-of-range value. Check offsets and numeric bounds.",
    "SelectorNotMatchedError": (
        "Nothing matched. The near misses above are the elements that came closest, and the search "
        "scope is what was actually there — usually the name is slightly off, or the element lives "
        "in a different application. Re-run 'snapshot' and read the tree rather than guessing again."
    ),
    "TimeoutError": (
        "The condition never came true in time. 'last observed' distinguishes the two cases: a "
        "selector that never matched is a targeting problem, while one that matched but stayed in "
        "the wrong state means the UI is still working or the action meant to trigger it never landed."
    ),
    "PlatformError": (
        "The OS accessibility layer returned an error. The app may be busy, mid-relaunch, or showing "
        "a modal that blocks queries. Retry, or re-run 'snapshot' to resync."
    ),
}

# Fields carrying xa11y's structured diagnosis, in the order they read best.
_DIAGNOSIS_FIELDS = ("condition", "selector", "last_observed", "elapsed")


class ToolError(Exception):
    """An error raised by this package rather than by xa11y itself."""


def xa11y() -> Any:
    """Import xa11y on demand.

    Kept lazy so that importing the tools — to inspect their schema, or in a test
    suite — never requires a working accessibility stack.
    """
    try:
        return importlib.import_module("xa11y")
    except ImportError as exc:  # pragma: no cover - exercised only without the dependency
        raise ToolError(
            "The xa11y package is required but not installed. Install it with: pip install strands-xa11y"
        ) from exc


def _class_names(exc: BaseException) -> List[str]:
    return [cls.__name__ for cls in type(exc).__mro__]


def describe(exc: BaseException) -> str:
    """Render an exception as an agent-facing message."""
    names = _class_names(exc)
    # str(exc) is empty for an exception raised with no message; anything else is kept
    # verbatim, including a message that legitimately ends in a colon.
    message = str(exc)
    parts = [f"{names[0]}: {message}" if message else names[0]]

    diagnosis = []
    for field in _DIAGNOSIS_FIELDS:
        value = getattr(exc, field, None)
        if value is not None:
            diagnosis.append(f"  {field.replace('_', ' ')}: {value}")
    candidates = getattr(exc, "candidates", None)
    if candidates:
        diagnosis.append("  near misses: " + "; ".join(str(candidate) for candidate in candidates))
    scope = getattr(exc, "scope", None)
    if scope:
        diagnosis.append(f"  search scope:\n{_indent(str(scope))}")
    if diagnosis:
        parts.append("\n".join(diagnosis))

    for name in names:
        if name in _GUIDANCE:
            parts.append(_GUIDANCE[name])
            break

    return "\n".join(parts)


def _indent(text: str, prefix: str = "    ") -> str:
    return "\n".join(prefix + line for line in text.splitlines())


def error_result(message: str) -> Dict[str, Any]:
    """A Strands tool result carrying an error."""
    return {"status": "error", "content": [{"text": message}]}


def success_result(text: str, extra: Optional[List[Dict[str, Any]]] = None) -> Dict[str, Any]:
    """A Strands tool result carrying text, plus any additional content blocks."""
    content: List[Dict[str, Any]] = [{"text": text}]
    if extra:
        content.extend(extra)
    return {"status": "success", "content": content}
