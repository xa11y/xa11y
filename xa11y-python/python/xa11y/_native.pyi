"""Type stubs for the xa11y native extension module."""

from __future__ import annotations

from collections.abc import Callable, Iterator
from types import TracebackType

# ── Exceptions ───────────────────────────────────────────────────────────────

class XA11yError(Exception):
    """Base exception for all xa11y errors."""

class PermissionDeniedError(XA11yError):
    """Accessibility permissions have not been granted."""

class AccessibilityNotEnabledError(XA11yError):
    """The target app advertises an accessibility tree but it is empty.

    Raised on Linux when a Chromium/Electron app is launched without
    ``--force-renderer-accessibility`` (or the ``ACCESSIBILITY_ENABLED=1``
    environment variable), so the renderer accessibility bridge never
    populates the window's subtree.
    """

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
    def x(self) -> int: ...
    @property
    def y(self) -> int: ...
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...

# ── EventType ────────────────────────────────────────────────────────────────

class EventType:
    """Accessibility event type constants."""

    FOCUS_CHANGED: str
    VALUE_CHANGED: str
    NAME_CHANGED: str
    STATE_CHANGED: str
    STRUCTURE_CHANGED: str
    WINDOW_OPENED: str
    WINDOW_CLOSED: str
    WINDOW_ACTIVATED: str
    WINDOW_DEACTIVATED: str
    SELECTION_CHANGED: str
    MENU_OPENED: str
    MENU_CLOSED: str
    ALERT: str
    TEXT_CHANGED: str

# ── Event ────────────────────────────────────────────────────────────────────

class Event:
    """An accessibility event delivered to subscribers."""

    @property
    def event_type(self) -> str: ...
    @property
    def app_name(self) -> str: ...
    @property
    def app_pid(self) -> int: ...
    @property
    def target(self) -> Element | None: ...
    def __repr__(self) -> str: ...

# ── Subscription ─────────────────────────────────────────────────────────────

class Subscription:
    """A live event subscription."""

    def try_recv(self) -> Event | None: ...
    def recv(self, timeout: float = 5.0) -> Event: ...
    def wait_for(
        self,
        predicate: Callable[[Event], bool],
        timeout: float = 5.0,
    ) -> Event: ...
    def close(self) -> None: ...
    def __enter__(self) -> Subscription: ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> bool: ...
    def __iter__(self) -> Iterator[Event]: ...
    def __next__(self) -> Event: ...
    def __repr__(self) -> str: ...

# ── App ───────────────────────────────────────────────────────────────────────

class App:
    """A running application — the entry point for accessibility queries."""

    @property
    def name(self) -> str: ...
    @property
    def pid(self) -> int | None: ...
    @staticmethod
    def by_name(name: str) -> App:
        """Find an application by exact name."""
    @staticmethod
    def by_pid(pid: int) -> App:
        """Find an application by process ID."""
    @staticmethod
    def list() -> list[App]:
        """List all running applications."""
    def locator(self, selector: str) -> Locator:
        """Create a Locator scoped to this application's accessibility tree."""
    def subscribe(self) -> Subscription:
        """Subscribe to accessibility events from this application."""
    def children(self) -> list[Element]:
        """Get direct children (typically windows) of this application."""
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── Element ───────────────────────────────────────────────────────────────────

class Element:
    """A live element with lazy navigation."""

    @property
    def role(self) -> str: ...
    @property
    def name(self) -> str | None: ...
    @property
    def value(self) -> str | None: ...
    @property
    def description(self) -> str | None: ...
    @property
    def numeric_value(self) -> float | None: ...
    @property
    def min_value(self) -> float | None: ...
    @property
    def max_value(self) -> float | None: ...
    @property
    def stable_id(self) -> str | None: ...
    @property
    def pid(self) -> int | None: ...
    @property
    def actions(self) -> list[str]: ...
    @property
    def bounds(self) -> Rect | None: ...
    @property
    def enabled(self) -> bool: ...
    @property
    def visible(self) -> bool: ...
    @property
    def focused(self) -> bool: ...
    @property
    def checked(self) -> str | None: ...
    @property
    def selected(self) -> bool: ...
    @property
    def expanded(self) -> bool | None: ...
    @property
    def editable(self) -> bool: ...
    @property
    def focusable(self) -> bool: ...
    @property
    def modal(self) -> bool: ...
    @property
    def required(self) -> bool: ...
    @property
    def busy(self) -> bool: ...
    def children(self) -> list[Element]:
        """Get direct children (lazy — each call queries the provider)."""
    def parent(self) -> Element | None:
        """Get parent element (lazy — each call queries the provider)."""
    def subscribe(self) -> Subscription:
        """Subscribe to accessibility events for this element (typically an app)."""
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── Locator ──────────────────────────────────────────────────────────────

class Locator:
    """A resilient element reference that re-queries on each interaction.

    Locators never hold a live reference to a UI element. Instead, they store
    a selector and resolve it on demand, making them immune to staleness.
    Action methods auto-wait for the element to appear before acting.
    """

    @property
    def selector(self) -> str:
        """The CSS-like selector string for this locator."""
    def nth(self, n: int) -> Locator:
        """Return a new Locator that selects the *n*-th match (1-based)."""
    def first(self) -> Locator:
        """Return a new Locator that selects the first match."""
    def child(self, selector: str) -> Locator:
        """Return a new Locator scoped to direct children matching *selector*."""
    def descendant(self, selector: str) -> Locator:
        """Return a new Locator scoped to descendants matching *selector*."""
    def exists(self) -> bool:
        """Check if a matching element exists."""
    def count(self) -> int:
        """Count matching elements."""
    def element(self) -> Element:
        """Get a single Element handle for the matched element."""
    def elements(self) -> list[Element]:
        """Get all matching elements."""
    def press(self) -> None:
        """Click / invoke the matched element."""
    def focus(self) -> None:
        """Set keyboard focus on the matched element."""
    def blur(self) -> None:
        """Remove keyboard focus from the matched element."""
    def toggle(self) -> None:
        """Toggle the matched element (checkbox, switch)."""
    def expand(self) -> None:
        """Expand the matched element."""
    def collapse(self) -> None:
        """Collapse the matched element."""
    def select(self) -> None:
        """Select the matched element (list item, tab, etc.)."""
    def show_menu(self) -> None:
        """Show the context menu for the matched element."""
    def scroll_into_view(self) -> None:
        """Scroll the matched element into the visible area."""
    def increment(self) -> None:
        """Increment the matched element (slider, spinner)."""
    def decrement(self) -> None:
        """Decrement the matched element (slider, spinner)."""
    def set_value(self, value: str) -> None:
        """Set the text value of the matched element."""
    def set_numeric_value(self, value: float) -> None:
        """Set the numeric value of the matched element (slider, spinner)."""
    def type_text(self, text: str) -> None:
        """Type text at the current cursor position on the matched element."""
    def select_text(self, start: int, end: int) -> None:
        """Select a text range within the matched element (0-based offsets)."""
    def scroll_up(self, amount: float = 1.0) -> None:
        """Scroll the matched element upward."""
    def scroll_down(self, amount: float = 1.0) -> None:
        """Scroll the matched element downward."""
    def scroll_left(self, amount: float = 1.0) -> None:
        """Scroll the matched element leftward."""
    def scroll_right(self, amount: float = 1.0) -> None:
        """Scroll the matched element rightward."""
    def perform_action(self, action: str) -> None:
        """Perform an action by snake_case name."""
    def wait_visible(self, timeout: float = 5.0) -> Element:
        """Wait until the element is visible, polling the provider."""
    def wait_attached(self, timeout: float = 5.0) -> Element:
        """Wait until the element exists in the tree."""
    def wait_detached(self, timeout: float = 5.0) -> None:
        """Wait until the element is removed from the tree."""
    def wait_enabled(self, timeout: float = 5.0) -> Element:
        """Wait until the element is enabled."""
    def wait_hidden(self, timeout: float = 5.0) -> None:
        """Wait until the element is hidden or removed."""
    def wait_disabled(self, timeout: float = 5.0) -> Element:
        """Wait until the element is disabled."""
    def wait_focused(self, timeout: float = 5.0) -> Element:
        """Wait until the element has keyboard focus."""
    def wait_unfocused(self, timeout: float = 5.0) -> Element:
        """Wait until the element does not have keyboard focus."""
    def wait_until(self, predicate: Callable[[Element | None], bool], timeout: float = 5.0) -> None:
        """Wait until an arbitrary predicate is satisfied."""
    def __repr__(self) -> str: ...

# ── Module-level functions ───────────────────────────────────────────────────

def locator(selector: str) -> Locator:
    """Create a top-level Locator searching from the system root."""

# ── Test helpers ─────────────────────────────────────────────────────────────

def _make_test_locator() -> Locator: ...
