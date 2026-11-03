"""A stand-in for the xa11y extension module.

CI has no display, no accessibility bus, and no windows to click, so the tests drive a
fake tree instead. It implements enough of the real module — including a small selector
evaluator — that ref resolution and snapshot rendering are exercised for real rather
than mocked away.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List, Optional, Tuple

# ── Errors (same names and hierarchy as the real module) ─────────────────────


class XA11yError(Exception):
    pass


class PermissionDeniedError(XA11yError):
    pass


class AccessibilityNotEnabledError(XA11yError):
    pass


class SelectorNotMatchedError(XA11yError):
    def __init__(self, message: str = "", **diagnosis: Any) -> None:
        super().__init__(message)
        self.selector = diagnosis.get("selector")
        self.condition = diagnosis.get("condition")
        self.last_observed = diagnosis.get("last_observed")
        self.hidden_reason = diagnosis.get("hidden_reason")
        self.candidates = diagnosis.get("candidates", [])
        self.scope = diagnosis.get("scope")
        self.elapsed = None


class ActionNotSupportedError(XA11yError):
    pass


class TimeoutError(XA11yError):  # noqa: A001 - mirrors the real module's shadowing
    def __init__(self, message: str = "", **diagnosis: Any) -> None:
        super().__init__(message)
        self.elapsed = diagnosis.get("elapsed")
        self.condition = diagnosis.get("condition")
        self.selector = diagnosis.get("selector")
        self.last_observed = diagnosis.get("last_observed")
        self.hidden_reason = diagnosis.get("hidden_reason")
        self.candidates = diagnosis.get("candidates", [])
        self.scope = diagnosis.get("scope")


class InvalidSelectorError(XA11yError):
    pass


class InvalidActionDataError(XA11yError):
    pass


class PlatformError(XA11yError):
    pass


# ── Call log ─────────────────────────────────────────────────────────────────

CALLS: List[Tuple[str, tuple, dict]] = []


def record(name: str, *args: Any, **kwargs: Any) -> None:
    CALLS.append((name, args, kwargs))


def reset() -> None:
    CALLS.clear()
    APPS.clear()


# ── Elements ─────────────────────────────────────────────────────────────────

_STATE_DEFAULTS = {
    "enabled": True,
    "visible": True,
    "focused": False,
    "active": False,
    "checked": None,
    "selected": False,
    "expanded": None,
    "editable": False,
    "focusable": False,
    "modal": False,
    "required": False,
    "busy": False,
    "password": False,
}

ACTION_VERBS = (
    "press",
    "focus",
    "blur",
    "toggle",
    "expand",
    "collapse",
    "select",
    "show_menu",
    "scroll_into_view",
    "increment",
    "decrement",
)


class Rect:
    def __init__(self, x: int, y: int, width: int, height: int) -> None:
        self.x, self.y, self.width, self.height = x, y, width, height


class Element:
    def __init__(
        self,
        role: str,
        name: Optional[str] = None,
        value: Optional[str] = None,
        children: Optional[List["Element"]] = None,
        stable_id: Optional[str] = None,
        bounds: Optional[Rect] = None,
        actions: Optional[List[str]] = None,
        unsupported: Tuple[str, ...] = (),
        **states: Any,
    ) -> None:
        self.role = role
        self.name = name
        self.value = value
        self.stable_id = stable_id
        self.bounds = bounds
        self.actions = actions or []
        self.description = None
        self.numeric_value = None
        self.min_value = None
        self.max_value = None
        self.pid = None
        self._children = children or []
        self._unsupported = unsupported
        for state, default in _STATE_DEFAULTS.items():
            setattr(self, state, states.pop(state, default))
        if states:
            raise TypeError(f"unexpected states: {sorted(states)}")

    def children(self) -> List["Element"]:
        return list(self._children)

    def descendants(self) -> List["Element"]:
        found = []
        for child in self._children:
            found.append(child)
            found.extend(child.descendants())
        return found

    def tree(self, max_depth: Optional[int] = None) -> Dict[str, Any]:
        children = (
            []
            if max_depth == 0
            else [child.tree(None if max_depth is None else max_depth - 1) for child in self._children]
        )
        return {"role": self.role, "name": self.name, "value": self.value, "children": children}

    def dump(self, max_depth: Optional[int] = None) -> str:
        return repr(self.tree(max_depth))

    def set_value(self, value: str) -> None:
        record("set_value", self, value)
        self.value = value

    def set_numeric_value(self, value: float) -> None:
        record("set_numeric_value", self, value)

    def type_text(self, text: str) -> None:
        record("type_text", self, text)
        self.value = (self.value or "") + text

    def select_text(self, start: int, end: int) -> None:
        record("select_text", self, start, end)

    def perform_action(self, action: str) -> None:
        record("perform_action", self, action)

    def __repr__(self) -> str:
        return f"<Element {self.role} {self.name!r}>"


def _make_verb(verb: str):
    def call(self: Element) -> None:
        if verb in self._unsupported:
            raise ActionNotSupportedError(f"{self.role} does not support {verb}")
        record(verb, self)
        if verb == "toggle" and self.checked is not None:
            self.checked = "off" if self.checked == "on" else "on"

    call.__name__ = verb
    return call


for _verb in ACTION_VERBS:
    setattr(Element, _verb, _make_verb(_verb))


# ── Selector evaluation ──────────────────────────────────────────────────────

_STEP = re.compile(
    r"^(?P<role>[a-z_*]+)?"
    r"(?:\[(?P<attr>[a-z_]+)(?P<op>=|\^=|\$=|\*=)(?P<quote>['\"])(?P<value>.*?)(?P=quote)\])?"
    r"(?::nth\((?P<nth>\d+)\))?$"
)


def _parse(step: str):
    matched = _STEP.match(step.strip())
    if not matched or step.strip() == "":
        raise InvalidSelectorError(f"cannot parse selector step: {step!r}")
    return matched.groupdict()


def _matches(element: Element, parsed: Dict[str, Any]) -> bool:
    role = parsed["role"]
    if role and role != "*" and element.role != role:
        return False
    attr = parsed["attr"]
    if attr:
        actual = getattr(element, attr, None)
        actual = "" if actual is None else str(actual)
        wanted, op = parsed["value"], parsed["op"]
        if op == "=":
            return actual == wanted
        lowered, wanted = actual.lower(), wanted.lower()
        return {
            "^=": lowered.startswith,
            "$=": lowered.endswith,
            "*=": lambda text: text in lowered,
        }[op](wanted)
    return True


def evaluate(root: Element, selector: str) -> List[Element]:
    """Evaluate one selector against a subtree. Supports the subset the tests need."""
    results: List[Element] = []
    for alternative in selector.split(","):
        current = [root]
        for position, step in enumerate(alternative.split(">")):
            parsed = _parse(step)
            pool: List[Element] = []
            for element in current:
                candidates = element.descendants() if position == 0 else element.children()
                pool.extend(candidate for candidate in candidates if _matches(candidate, parsed))
            if parsed["nth"]:
                index = int(parsed["nth"]) - 1
                pool = pool[index : index + 1]
            current = pool
        for element in current:
            if element not in results:
                results.append(element)
    return results


class Locator:
    def __init__(self, root: Element, selector: str) -> None:
        self._root = root
        self.selector = selector

    def elements(self) -> List[Element]:
        return evaluate(self._root, self.selector)

    def count(self) -> int:
        return len(self.elements())

    def exists(self) -> bool:
        return self.count() > 0

    def element(self) -> Element:
        matches = self.elements()
        if not matches:
            raise SelectorNotMatchedError(
                f"no element matched {self.selector!r}",
                selector=self.selector,
                last_observed="selector never matched",
                candidates=["button 'Cancel'"],
                scope="window 'Fake'",
            )
        return matches[0]

    def nth(self, index: int) -> "Locator":
        return Locator(self._root, f"{self.selector}:nth({index})")

    def first(self) -> "Locator":
        return self.nth(1)

    def child(self, selector: str) -> "Locator":
        return Locator(self._root, f"{self.selector} > {selector}")

    def descendant(self, selector: str) -> "Locator":
        return Locator(self._root, f"{self.selector} {selector}")

    def tree(self, max_depth: Optional[int] = None) -> Dict[str, Any]:
        return self.element().tree(max_depth)

    def __getattr__(self, name: str):
        if name.startswith("wait_"):

            def wait(timeout: Optional[float] = None):
                record(name, self.selector, timeout)
                return self.element()

            return wait
        if name in ACTION_VERBS or name in {
            "set_value",
            "set_numeric_value",
            "type_text",
            "select_text",
            "perform_action",
        }:

            def act(*args: Any, **kwargs: Any):
                return getattr(self.element(), name)(*args, **kwargs)

            return act
        raise AttributeError(name)


# ── Applications ─────────────────────────────────────────────────────────────

APPS: List["App"] = []


class App:
    def __init__(self, name: str, pid: Optional[int] = None, root: Optional[Element] = None, foreground: bool = False):
        self.name = name
        self.pid = pid
        self.is_foreground = foreground
        self._root = root or Element("application", name)

    @staticmethod
    def list() -> List["App"]:
        return list(APPS)

    @staticmethod
    def by_name(name: str, *, timeout: Optional[float] = None) -> "App":
        for app in APPS:
            if app.name == name:
                return app
        raise SelectorNotMatchedError(
            f"no application named {name!r}", candidates=[app.name for app in APPS], scope="running applications"
        )

    @staticmethod
    def by_pid(pid: int, *, timeout: Optional[float] = None) -> "App":
        for app in APPS:
            if app.pid == pid:
                return app
        raise SelectorNotMatchedError(f"no application with pid {pid}")

    @staticmethod
    def foreground(*, timeout: Optional[float] = None) -> "App":
        for app in APPS:
            if app.is_foreground:
                return app
        raise SelectorNotMatchedError("no foreground application")

    def locator(self, selector: str) -> Locator:
        return Locator(self._root, selector)

    def as_element(self) -> Element:
        return self._root

    def children(self) -> List[Element]:
        return self._root.children()

    def tree(self, max_depth: Optional[int] = None) -> Dict[str, Any]:
        return self._root.tree(max_depth)

    def dump(self, max_depth: Optional[int] = None) -> str:
        return self._root.dump(max_depth)


# ── Input and pixels ─────────────────────────────────────────────────────────


class InputSim:
    def __getattr__(self, name: str):
        def call(*args: Any, **kwargs: Any) -> None:
            record(f"input.{name}", *args, **kwargs)

        return call


class Screenshot:
    def __init__(self, width: int = 100, height: int = 50, scale: float = 1.0, payload: bytes = b"\x89PNG-fake"):
        self.width, self.height, self.scale = width, height, scale
        self._payload = payload
        self.pixels = b"\x00" * (width * height * 4)

    def to_png(self) -> bytes:
        return self._payload

    def save_png(self, path: Any) -> None:
        record("save_png", path)
        with open(path, "wb") as handle:
            handle.write(self._payload)


NEXT_SCREENSHOT: Optional[Screenshot] = None


def input_sim() -> InputSim:
    return InputSim()


def locator(selector: str) -> Locator:
    raise SelectorNotMatchedError("rootless locators are not implemented in the fake")


def screenshot(*, element: Any = None, region: Any = None) -> Screenshot:
    record("screenshot", element, region)
    return NEXT_SCREENSHOT or Screenshot()


def set_default_timeout(timeout: float) -> None:
    record("set_default_timeout", timeout)


def get_default_timeout() -> float:
    return 5.0
