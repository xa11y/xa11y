"""Type stubs for the xa11y native extension module."""

from __future__ import annotations

from collections.abc import Callable, Iterator

# ── Exceptions ───────────────────────────────────────────────────────────────

class XA11yError(Exception):
    """Base exception for all xa11y errors."""

class PermissionDeniedError(XA11yError):
    """Accessibility permissions have not been granted."""

class AppNotFoundError(XA11yError):
    """The target application is not running or not exposing an accessibility tree."""

class SelectorNotMatchedError(XA11yError):
    """No element in the tree matched the given selector."""

class ActionNotSupportedError(XA11yError):
    """The requested action is not supported on the target element."""

class TimeoutError(XA11yError):
    """An operation exceeded its timeout."""

class InvalidSelectorError(XA11yError):
    """The selector string has invalid syntax."""

class PlatformError(XA11yError):
    """An OS-level accessibility error occurred."""

# ── Data Classes ─────────────────────────────────────────────────────────────

class Rect:
    """A bounding rectangle in screen coordinates (pixels)."""

    @property
    def x(self) -> int:
        """X coordinate of the top-left corner."""
    @property
    def y(self) -> int:
        """Y coordinate of the top-left corner."""
    @property
    def width(self) -> int:
        """Width in pixels."""
    @property
    def height(self) -> int:
        """Height in pixels."""
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...

class AppInfo:
    """Information about a running application."""

    @property
    def name(self) -> str:
        """Application name."""
    @property
    def pid(self) -> int:
        """Process ID."""
    @property
    def bundle_id(self) -> str | None:
        """Bundle identifier (macOS only, ``None`` on other platforms)."""
    def __repr__(self) -> str: ...

# ── Node ─────────────────────────────────────────────────────────────────────

class Node:
    """A single element in the accessibility tree.

    Nodes are immutable snapshots that form a navigable graph —
    use :attr:`children` and :attr:`parent` to traverse.
    """

    @property
    def role(self) -> str:
        """Role name (e.g. ``"button"``, ``"text_field"``)."""
    @property
    def name(self) -> str | None:
        """Accessible name."""
    @property
    def value(self) -> str | None:
        """Current value (e.g. text content, slider position)."""
    @property
    def description(self) -> str | None:
        """Accessible description."""
    @property
    def numeric_value(self) -> float | None:
        """Numeric value (for sliders, progress bars, etc.)."""
    @property
    def min_value(self) -> float | None:
        """Minimum numeric value."""
    @property
    def max_value(self) -> float | None:
        """Maximum numeric value."""
    @property
    def stable_id(self) -> str | None:
        """Platform-stable identifier that persists across tree snapshots."""
    @property
    def actions(self) -> list[str]:
        """List of supported action names."""
    @property
    def children(self) -> list[Node]:
        """Direct children of this node."""
    @property
    def parent(self) -> Node | None:
        """Parent node, or ``None`` for the root."""
    @property
    def bounds(self) -> Rect | None:
        """Bounding rectangle in screen coordinates."""
    @property
    def enabled(self) -> bool:
        """Whether the element is interactive."""
    @property
    def visible(self) -> bool:
        """Whether the element is visible."""
    @property
    def focused(self) -> bool:
        """Whether the element has keyboard focus."""
    @property
    def checked(self) -> str | None:
        """Check state: ``"on"``, ``"off"``, ``"mixed"``, or ``None``."""
    @property
    def selected(self) -> bool:
        """Whether the element is selected."""
    @property
    def expanded(self) -> bool | None:
        """Expansion state (``None`` if not expandable)."""
    @property
    def editable(self) -> bool:
        """Whether the element supports text editing."""
    @property
    def focusable(self) -> bool:
        """Whether the element can receive focus."""
    @property
    def modal(self) -> bool:
        """Whether the element is a modal dialog."""
    @property
    def required(self) -> bool:
        """Whether the element is a required form field."""
    @property
    def busy(self) -> bool:
        """Whether the element is in a busy/loading state."""
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...
    def __len__(self) -> int: ...

# ── Target type for action methods ───────────────────────────────────────────

_Target = Node | str

# ── Tree ─────────────────────────────────────────────────────────────────────

class Tree:
    """A snapshot of an application's accessibility tree.

    Iterable over all nodes in depth-first order. Supports ``len(tree)``
    to get the total node count.
    """

    @property
    def app_name(self) -> str:
        """Name of the application this tree belongs to."""
    @property
    def pid(self) -> int | None:
        """Process ID of the application."""
    @property
    def screen_size(self) -> tuple[int, int]:
        """Screen dimensions as ``(width, height)`` in pixels."""
    @property
    def root(self) -> Node:
        """The root node of the tree."""
    def query(self, selector: str) -> list[Node]:
        """Find all nodes matching a CSS-like selector string."""
    def find_by_role(self, role: str) -> list[Node]:
        """Find all nodes with the given role name."""
    def find_by_name(self, pattern: str) -> list[Node]:
        """Find all nodes whose name contains *pattern* (case-insensitive)."""
    def perform(
        self,
        target: _Target,
        action: str,
        *,
        value: str | None = None,
        numeric_value: float | None = None,
        direction: str | None = None,
        amount: float | None = None,
        start: int | None = None,
        end: int | None = None,
    ) -> None:
        """Perform a named action on a node or selector target."""
    def press(self, target: _Target) -> None:
        """Press / activate an element."""
    def focus(self, target: _Target) -> None:
        """Move keyboard focus to an element."""
    def blur(self, target: _Target) -> None:
        """Remove keyboard focus from an element."""
    def toggle(self, target: _Target) -> None:
        """Toggle a checkbox or switch."""
    def expand(self, target: _Target) -> None:
        """Expand a collapsible element."""
    def collapse(self, target: _Target) -> None:
        """Collapse an expanded element."""
    def select(self, target: _Target) -> None:
        """Select an item (e.g. in a list or tab bar)."""
    def increment(self, target: _Target) -> None:
        """Increment a slider or stepper."""
    def decrement(self, target: _Target) -> None:
        """Decrement a slider or stepper."""
    def show_menu(self, target: _Target) -> None:
        """Open the context menu for an element."""
    def scroll_into_view(self, target: _Target) -> None:
        """Scroll until the element is visible in its viewport."""
    def set_value(self, target: _Target, value: str) -> None:
        """Set the text value of an element."""
    def set_numeric_value(self, target: _Target, value: float) -> None:
        """Set the numeric value of an element (e.g. a slider)."""
    def type_text(self, target: _Target, text: str) -> None:
        """Type text into an element."""
    def scroll(self, target: _Target, direction: str, amount: float = 1.0) -> None:
        """Scroll an element. *direction* is ``"up"``, ``"down"``, ``"left"``, or ``"right"``."""
    def select_text(self, target: _Target, start: int, end: int) -> None:
        """Select a text range by character offsets."""
    def locator(
        self,
        selector: str,
        *,
        max_depth: int | None = None,
        max_elements: int | None = None,
        visible_only: bool = False,
        roles: list[str] | None = None,
    ) -> Locator:
        """Create a :class:`Locator` for resilient, lazy element interaction."""
    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[Node]: ...
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── Locator ──────────────────────────────────────────────────────────────────

class Locator:
    """A resilient element reference that re-queries the tree on each interaction.

    Locators are chainable and support waiting for element state changes.
    """

    @property
    def selector(self) -> str:
        """The CSS-like selector string for this locator."""
    def nth(self, n: int) -> Locator:
        """Select the *n*-th match (0-indexed)."""
    def first(self) -> Locator:
        """Select the first match (shorthand for ``nth(0)``)."""
    def child(self, selector: str) -> Locator:
        """Narrow to direct children matching *selector*."""
    def descendant(self, selector: str) -> Locator:
        """Narrow to descendants matching *selector*."""
    def role(self) -> str:
        """Get the role of the matched element."""
    def name(self) -> str | None:
        """Get the accessible name of the matched element."""
    def value(self) -> str | None:
        """Get the current value of the matched element."""
    def description(self) -> str | None:
        """Get the accessible description of the matched element."""
    def is_visible(self) -> bool:
        """Check whether the matched element is visible."""
    def is_enabled(self) -> bool:
        """Check whether the matched element is enabled."""
    def is_focused(self) -> bool:
        """Check whether the matched element has keyboard focus."""
    def exists(self) -> bool:
        """Check whether the selector matches any element."""
    def count(self) -> int:
        """Count the number of elements matching the selector."""
    def get(self) -> Node:
        """Resolve the locator to a :class:`Node` snapshot."""
    def press(self) -> None:
        """Press / activate the matched element."""
    def focus(self) -> None:
        """Move keyboard focus to the matched element."""
    def blur(self) -> None:
        """Remove keyboard focus from the matched element."""
    def toggle(self) -> None:
        """Toggle a checkbox or switch."""
    def expand(self) -> None:
        """Expand a collapsible element."""
    def collapse(self) -> None:
        """Collapse an expanded element."""
    def select_item(self) -> None:
        """Select an item (e.g. in a list or tab bar)."""
    def show_menu(self) -> None:
        """Open the context menu for the matched element."""
    def scroll_into_view(self) -> None:
        """Scroll until the matched element is visible."""
    def increment(self) -> None:
        """Increment a slider or stepper."""
    def decrement(self) -> None:
        """Decrement a slider or stepper."""
    def set_value(self, value: str) -> None:
        """Set the text value of the matched element."""
    def set_numeric_value(self, value: float) -> None:
        """Set the numeric value of the matched element."""
    def type_text(self, text: str) -> None:
        """Type text into the matched element."""
    def select_text(self, start: int, end: int) -> None:
        """Select a text range by character offsets."""
    def scroll(self, direction: str, amount: float = 1.0) -> None:
        """Scroll the matched element.

        *direction*: ``"up"``, ``"down"``, ``"left"``, or ``"right"``.
        """
    def wait_visible(self, timeout: float = 5.0) -> None:
        """Wait until the element is visible (default 5s timeout)."""
    def wait_attached(self, timeout: float = 5.0) -> None:
        """Wait until the selector matches an element (default 5s timeout)."""
    def wait_detached(self, timeout: float = 5.0) -> None:
        """Wait until the selector no longer matches (default 5s timeout)."""
    def wait_enabled(self, timeout: float = 5.0) -> None:
        """Wait until the element is enabled (default 5s timeout)."""
    def wait_hidden(self, timeout: float = 5.0) -> None:
        """Wait until the element is hidden (default 5s timeout)."""
    def wait_disabled(self, timeout: float = 5.0) -> None:
        """Wait until the element is disabled (default 5s timeout)."""
    def wait_focused(self, timeout: float = 5.0) -> None:
        """Wait until the element is focused (default 5s timeout)."""
    def wait_unfocused(self, timeout: float = 5.0) -> None:
        """Wait until the element loses focus (default 5s timeout)."""
    def wait_until(self, predicate: Callable[[Node], bool], timeout: float = 5.0) -> None:
        """Wait until *predicate(node)* returns ``True`` (default 5s timeout)."""
    def __repr__(self) -> str: ...

# ── Module-level functions ───────────────────────────────────────────────────

def app(
    name: str | None = None,
    *,
    pid: int | None = None,
    max_depth: int | None = None,
    max_elements: int | None = None,
    visible_only: bool = False,
    roles: list[str] | None = None,
) -> Tree:
    """Get the accessibility tree for the given app.

    Identify the app by *name* (substring match) or *pid* (exact).
    """

def all_apps(
    *,
    max_depth: int | None = None,
    max_elements: int | None = None,
    visible_only: bool = False,
    roles: list[str] | None = None,
) -> Tree:
    """Get a combined accessibility tree for all running applications."""

def locator(
    name: str | None = None,
    *,
    pid: int | None = None,
    selector: str,
    max_depth: int | None = None,
    max_elements: int | None = None,
    visible_only: bool = False,
    roles: list[str] | None = None,
) -> Locator:
    """Create a :class:`Locator` for lazy element resolution."""

def list_apps() -> list[AppInfo]:
    """List all running applications that expose accessibility trees."""

def check_permissions() -> str:
    """Check whether accessibility permissions are granted.

    Returns ``"granted"`` or raises :exc:`PermissionDeniedError`.
    """

# ── Test helpers ─────────────────────────────────────────────────────────────

def _make_test_tree() -> Tree: ...
def _make_test_apps() -> list[AppInfo]: ...
