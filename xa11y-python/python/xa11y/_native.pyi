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
    """No element in the tree matched the given selector.

    Carries a structured diagnosis (tenet 6) so the failure is
    understandable without re-running it under manual tree dumps. All
    attributes are always present (``None`` / ``[]`` when not applicable);
    the same content is rendered into the exception message.
    """

    selector: str | None
    """The selector that failed to match."""
    condition: str | None
    """What the operation was waiting for / trying to find, if known."""
    last_observed: str | None
    """What the failing operation last observed (e.g. ``'selector matched
    2 element(s); nth(5) requested'``)."""
    candidates: list[str]
    """Bounded near-miss candidates, e.g. same-role elements with different
    names, or the running applications for app lookups."""
    scope: str | None
    """Bounded rendering of the search scope: a depth-limited tree dump for
    scoped locators, or the application list for rootless ones."""
    elapsed: float | None
    """Always ``None`` for this class (present for attribute parity with
    :class:`TimeoutError`)."""

class ActionNotSupportedError(XA11yError):
    """The requested action is not supported on the target element."""

class TimeoutError(XA11yError):
    """An operation exceeded its timeout.

    Carries a structured diagnosis (tenet 6): what the wait was for
    (``condition`` + ``selector``), what it last observed, and — when the
    selector never matched — bounded scope context (``candidates`` +
    ``scope``). All attributes are always present (``None`` / ``[]`` when
    not applicable); the same content is rendered into the message, e.g.::

        Timeout after 60.0s; waiting for: visible; selector:
        dialog[name^="Submit"]; last observed: selector never matched;
        candidates: window "Untitled — MyApp"
        search scope (bounded):
        ...
    """

    elapsed: float | None
    """Wall-clock seconds the operation waited before giving up."""
    condition: str | None
    """What the wait was for: ``'visible'``, ``'attached'``, ``'press
    target actionable (visible && enabled)'``, ``'event matching
    predicate'``, ``'custom predicate'``, ..."""
    selector: str | None
    """The selector being resolved, when the wait had one."""
    last_observed: str | None
    """The last poll's observation: ``'matched button "Export"
    (visible=false, enabled=true, focused=false)'`` vs ``'selector never
    matched'``."""
    candidates: list[str]
    """Bounded near-miss candidates (same role, different attributes).
    Collected only when the selector never matched during the wait."""
    scope: str | None
    """Bounded rendering of the search scope. Collected only when the
    selector never matched during the wait."""

class InvalidSelectorError(XA11yError):
    """The selector string has invalid syntax."""

class InvalidActionDataError(XA11yError):
    """An action received invalid data (e.g. ``Locator.nth(0)``, or a
    text-selection range with ``start > end``)."""

class PlatformError(XA11yError):
    """An OS-level accessibility error occurred."""

# ── Data Classes ─────────────────────────────────────────────────────────────

class Rect:
    """A bounding rectangle in logical screen coordinates (device-independent
    points) on every platform, origin at the top-left of the primary display.

    Same coordinate space as ``screenshot(region=...)`` and input targets, so
    bounds pass straight through. To index into a captured image multiply by
    the screenshot's ``scale`` (``physical = logical * scale``)."""

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
    """Accessibility event type constants.

    Each constant's value is the string carried in :attr:`Event.event_type`,
    so handlers can compare against the constant or the literal string
    interchangeably.
    """

    FOCUS_CHANGED: str = "focus_changed"
    VALUE_CHANGED: str = "value_changed"
    NAME_CHANGED: str = "name_changed"
    STATE_CHANGED: str = "state_changed"
    STRUCTURE_CHANGED: str = "structure_changed"
    WINDOW_OPENED: str = "window_opened"
    WINDOW_CLOSED: str = "window_closed"
    WINDOW_ACTIVATED: str = "window_activated"
    WINDOW_DEACTIVATED: str = "window_deactivated"
    SELECTION_CHANGED: str = "selection_changed"
    MENU_OPENED: str = "menu_opened"
    MENU_CLOSED: str = "menu_closed"
    TEXT_CHANGED: str = "text_changed"
    ANNOUNCEMENT: str = "announcement"

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
    @property
    def state_flag(self) -> str | None:
        """For ``state_changed`` events: the flag that changed (e.g. ``'checked'``).

        ``None`` for other event types.
        """
    @property
    def state_value(self) -> bool | None:
        """For ``state_changed`` events: the new boolean value of the flag.

        ``None`` for other event types.
        """
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
    @property
    def focused(self) -> bool:
        """Whether this application currently holds the foreground / input focus.

        Mirrors :attr:`Element.focused` one level up: an application is
        ``focused`` when it is the foreground app. Populated for apps obtained
        via :meth:`list` and :meth:`find` (where it is also visible to the
        predicate, so ``App.find(lambda a: a.focused)`` selects the foreground
        app). A point-in-time snapshot taken when the ``App`` was resolved.

        On Windows apps are top-level windows, so every top-level window of the
        foreground process reports ``focused``; use :meth:`foreground` to
        obtain the exact foreground window.
        """
    @staticmethod
    def by_name(name: str, *, timeout: float | None = None) -> App:
        """Find an application by exact name.

        Polls the accessibility API until the app appears or ``timeout``
        (seconds) elapses. ``timeout=None`` (the default) uses the
        process-wide default timeout — 5 seconds unless overridden via
        :func:`set_default_timeout` or the ``XA11Y_DEFAULT_TIMEOUT``
        environment variable. Pass ``timeout=0`` for a single attempt with
        no waiting. Only "not found" errors trigger a retry; other errors
        fail fast.
        """
    @staticmethod
    def by_pid(pid: int, *, timeout: float | None = None) -> App:
        """Find an application by process ID.

        This is the supported way to *wait* for a freshly launched process
        to surface in the accessibility tree: the lookup polls until the app
        becomes reachable through the platform bridge or ``timeout``
        (seconds) elapses, covering the gap between the process starting and
        its accessibility registration completing. There is no need to
        hand-roll a poll over ``App.list()``. Where the platform supports it
        (macOS AX, Windows UIA), the lookup attaches to the process directly
        instead of filtering app enumeration, so an app whose window is
        still unnamed mid-startup is found as soon as the accessibility API
        can reach it. See ``by_name`` for ``timeout`` semantics.
        """
    @staticmethod
    def find(predicate: Callable[[App], bool], *, timeout: float | None = None) -> App:
        """Find an application matching ``predicate``.

        ``predicate`` is called with an :class:`App` for each running
        application on every poll; the first for which it returns truthy is
        returned. A falsy return means "not this one, keep polling"; if the
        predicate *raises*, the search aborts immediately and that exception
        propagates. See ``by_name`` for ``timeout`` semantics. There is no
        need to hand-roll a poll over ``App.list()``.

        Use this when neither a name nor a PID alone identifies the target
        — e.g. a Qt dialog that registers as its own accessibility
        application sharing the host process's PID::

            app = xa11y.App.find(
                lambda a: a.pid == pid and a.name.startswith("My Dialog"),
                timeout=30.0,
            )
        """
    @staticmethod
    def foreground(*, timeout: float | None = None) -> App:
        """Resolve the application that currently holds the system foreground.

        Queries the platform's foreground mechanism directly, so it returns the
        exact foreground window on Windows and stays reliable when an app shows
        a modal dialog. "Nothing focused" retries until ``timeout``; see
        ``by_name`` for ``timeout`` semantics. The returned app has
        ``focused == True``.
        """
    @staticmethod
    def list() -> list[App]:
        """List all running applications.

        Single enumeration, no polling. To wait for an app that is still
        starting up, use ``by_pid`` / ``by_name`` / ``find`` with a
        ``timeout`` instead of polling this in a loop.
        """
    def locator(self, selector: str) -> Locator:
        """Create a Locator scoped to this application's accessibility tree."""
    def subscribe(self) -> Subscription:
        """Subscribe to accessibility events from this application."""
    def children(self) -> list[Element]:
        """Get direct children (typically windows) of this application."""
    def as_element(self) -> Element:
        """Get an :class:`Element` handle for the application root.

        Useful for invoking Element-level methods (``children()``,
        ``parent()``, etc.) without going through a locator.
        """
    def tree(self, max_depth: int | None = None) -> dict:
        """Capture this application's accessibility tree as a recursive dict.

        Each dict has keys ``role``, ``name``, ``value``, and ``children``
        (a list of dicts with the same shape). ``max_depth`` limits traversal:
        ``0`` = only the application node, ``1`` = application + direct
        children (typically windows), ``None`` = full subtree.

        Equivalent to ``Element.tree(...)`` on the application's root element.
        """
    def dump(self, max_depth: int | None = None) -> str:
        """Render this application's accessibility tree as an indented string.

        Returns the string without printing it. Same depth semantics as
        :meth:`tree`. This is the primary inspection helper — call
        ``print(app.dump())`` to discover the role and name of every element
        in the app before writing selectors.

        For the same output from the shell, use ``xa11y tree --app NAME``.
        """
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
    def raw(self) -> dict[str, object]:
        """Platform-specific raw data attached to this element.

        Keys are provider-defined (e.g. ``"ax_role"`` on macOS,
        ``"uia_control_type"`` on Windows). Values are JSON-compatible
        (strings, numbers, booleans, lists, nested dicts). Intended for
        debugging and platform-specific queries — prefer the cross-platform
        fields (``role``, ``name``, etc.) for portable logic.
        """
    @property
    def enabled(self) -> bool: ...
    @property
    def visible(self) -> bool: ...
    @property
    def focused(self) -> bool: ...
    @property
    def checked(self) -> str | None:
        """Tri-state toggle value: ``'on'``, ``'off'``, ``'mixed'``, or ``None``.

        ``None`` means the element has no checked state (it is not a
        checkbox / radio button / toggle). These four are the only possible
        values. Compare against them explicitly — ``bool(element.checked)``
        is ``True`` for *every* non-``None`` value, including ``'off'``::

            is_checked = element.checked == "on"
        """
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
    def tree(self, max_depth: int | None = None) -> dict:
        """Capture the subtree rooted at this element as a recursive dict snapshot.

        Each dict has keys ``role``, ``name``, ``value``, and ``children``
        (a list of dicts with the same shape). ``max_depth`` limits traversal:
        ``0`` = only this node, ``1`` = node + direct children, ``None`` = full
        subtree.

        Use this when you need to inspect or analyze the tree programmatically.
        For an indented human-readable string, see :meth:`dump`. For ad-hoc
        exploration from the shell, see the ``xa11y tree`` CLI command.
        """
    def dump(self, max_depth: int | None = None) -> str:
        """Render the subtree rooted at this element as an indented string.

        Returns the string without printing. Same depth semantics as
        :meth:`tree`. Useful as a first step when writing a test against an
        unfamiliar app — call ``print(app.dump())`` to discover the role and
        name of every element, then turn those into selectors.

        For the same output from the shell, use ``xa11y tree --app NAME``.
        """
    def subscribe(self) -> Subscription:
        """Subscribe to accessibility events for this element (typically an app)."""
    def press(self) -> None:
        """Press (default activate) this element."""
    def focus(self) -> None:
        """Move keyboard focus to this element."""
    def blur(self) -> None:
        """Remove keyboard focus from this element."""
    def toggle(self) -> None:
        """Toggle this element's checked state."""
    def expand(self) -> None:
        """Expand this element (e.g. tree node, combo box)."""
    def collapse(self) -> None:
        """Collapse this element."""
    def select(self) -> None:
        """Select this element (e.g. list item, tab)."""
    def show_menu(self) -> None:
        """Show this element's context menu."""
    def scroll_into_view(self) -> None:
        """Scroll this element into view."""
    def increment(self) -> None:
        """Increment this element's value (e.g. slider, spinner)."""
    def decrement(self) -> None:
        """Decrement this element's value."""
    def set_value(self, value: str) -> None:
        """Replace this element's text value."""
    def set_numeric_value(self, value: float) -> None:
        """Set this element's numeric value."""
    def type_text(self, text: str) -> None:
        """Insert text at the current cursor position."""
    def select_text(self, start: int, end: int) -> None:
        """Select the text range from ``start`` to ``end`` (0-based offsets)."""
    def perform_action(self, action: str) -> None:
        """Perform an action by ``snake_case`` name."""
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── Locator ──────────────────────────────────────────────────────────────

class Locator:
    """A resilient element reference that re-queries on each interaction.

    Locators never hold a live reference to a UI element. Instead, they store
    a selector and resolve it on demand, making them immune to staleness.
    Action methods auto-wait for the element to appear before acting, using
    the process-wide default timeout (5 seconds unless overridden via
    :func:`set_default_timeout` or the ``XA11Y_DEFAULT_TIMEOUT`` environment
    variable). ``wait_*`` methods use the same default when no explicit
    ``timeout=`` is passed.
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
    def tree(self, max_depth: int | None = None) -> dict:
        """Capture the subtree rooted at the matched element as a recursive dict.

        Each dict has keys ``role``, ``name``, ``value``, and ``children``
        (a list of dicts with the same shape). ``max_depth`` limits traversal:
        ``0`` = only this node, ``1`` = node + direct children, ``None`` =
        full subtree.

        Resolves the selector once; fails fast with
        :class:`SelectorNotMatchedError` if no match — does not auto-wait.
        """
    def dump(self, max_depth: int | None = None) -> str:
        """Render the subtree rooted at the matched element as an indented string.

        Returns the string without printing it. Same depth and resolution
        semantics as :meth:`tree`.
        """
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
    def perform_action(self, action: str) -> None:
        """Perform an action by snake_case name."""
    def wait_visible(self, timeout: float | None = None) -> Element:
        """Wait until the element is visible, polling the provider.

        ``timeout=None`` (the default) uses the process-wide default timeout
        — see :func:`set_default_timeout`. Applies to all ``wait_*`` methods.
        """
    def wait_attached(self, timeout: float | None = None) -> Element:
        """Wait until the element exists in the tree."""
    def wait_detached(self, timeout: float | None = None) -> None:
        """Wait until the element is removed from the tree."""
    def wait_enabled(self, timeout: float | None = None) -> Element:
        """Wait until the element is enabled."""
    def wait_hidden(self, timeout: float | None = None) -> None:
        """Wait until the element is hidden or removed."""
    def wait_disabled(self, timeout: float | None = None) -> Element:
        """Wait until the element is disabled."""
    def wait_focused(self, timeout: float | None = None) -> Element:
        """Wait until the element has keyboard focus."""
    def wait_unfocused(self, timeout: float | None = None) -> Element:
        """Wait until the element does not have keyboard focus."""
    def wait_until(
        self,
        predicate: Callable[[Element | None], bool],
        timeout: float | None = None,
    ) -> None:
        """Wait until an arbitrary predicate is satisfied.

        A predicate that *raises* aborts the wait immediately and propagates
        the exception — it is not swallowed as "not yet" (which would
        resurface later as a misleading timeout). Mirrors ``App.find``.
        """
    def __repr__(self) -> str: ...

# ── InputSim ─────────────────────────────────────────────────────────────────

class InputSim:
    """Input-simulation façade for synthesised pointer and keyboard events.

    Targets are either a ``(x, y)`` tuple in logical screen coordinates (same
    space as ``Element.bounds``), or an ``Element`` (uses its bounds centre);
    each backend converts to physical device pixels at the OS boundary. Key
    values are strings: printable characters
    are literal (``"a"``, ``"7"``, ``";"``); named keys use their Pascal name
    (``"Enter"``, ``"ArrowUp"``, ``"F5"``); modifiers are ``"Shift"``,
    ``"Ctrl"``, ``"Alt"``, ``"Meta"``.

    Input simulation is distinct from the accessibility action layer — prefer
    ``Locator.press()`` / ``Locator.type_text()`` when the target exposes the
    semantic action. Use ``InputSim`` for gestures with no a11y equivalent
    (drag-and-drop, scroll wheels, global shortcuts).
    """

    def click(self, target: tuple[int, int] | Element) -> None:
        """Left-click once at ``target``."""
    def double_click(self, target: tuple[int, int] | Element) -> None:
        """Left double-click at ``target``."""
    def right_click(self, target: tuple[int, int] | Element) -> None:
        """Right-click at ``target``."""
    def move_to(self, target: tuple[int, int] | Element) -> None:
        """Move the pointer to ``target`` without pressing any button."""
    def drag(
        self,
        start: tuple[int, int] | Element,
        end: tuple[int, int] | Element,
    ) -> None:
        """Left-drag from ``start`` to ``end``."""
    def scroll(
        self,
        target: tuple[int, int] | Element,
        dx: int = 0,
        dy: int = 0,
    ) -> None:
        """Scroll at ``target``. ``dx`` positive → right, ``dy`` positive → down."""
    def press(self, key: str) -> None:
        """Tap a key (press + release)."""
    def chord(self, key: str, held: list[str] = ...) -> None:
        """Tap ``key`` while the keys in ``held`` are held down."""
    def type_text(self, text: str) -> None:
        """Type literal text into the currently focused control."""

# ── Screenshot ───────────────────────────────────────────────────────────────

class Screenshot:
    """A captured image: raw RGBA8 pixels plus dimensions and scale.

    ``width`` and ``height`` are in physical pixels. ``scale`` is the
    physical-to-logical ratio (1.0 on standard displays, 2.0 on typical
    Retina). ``pixels`` has length ``width * height * 4``.
    """

    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def scale(self) -> float: ...
    @property
    def pixels(self) -> bytes:
        """Raw RGBA8 pixel bytes (``width * height * 4``)."""
    def to_png(self) -> bytes:
        """Encode the image as a PNG and return the bytes."""
    def save_png(self, path: str | bytes | object) -> None:
        """Encode as PNG and write to ``path``. Accepts ``str``, ``bytes`` or ``os.PathLike``."""
    def __repr__(self) -> str: ...

# ── Module-level functions ───────────────────────────────────────────────────

def locator(selector: str) -> Locator:
    """Create a top-level Locator searching from the system root."""

def set_default_timeout(timeout: float) -> None:
    """Set the process-wide default timeout, in seconds.

    Becomes the default for every auto-waiting action method, ``wait_*``
    call, and app lookup (``App.by_name`` / ``App.by_pid`` / ``App.find``)
    that doesn't pass an explicit ``timeout=``. An explicit per-call
    ``timeout=`` always wins. Takes precedence over the
    ``XA11Y_DEFAULT_TIMEOUT`` environment variable (seconds, read once at
    import).

    Pass ``0`` for "single attempt, no polling" semantics. Raises
    ``ValueError`` for negative or non-finite values.
    """

def get_default_timeout() -> float:
    """Get the effective process-wide default timeout, in seconds.

    Resolution order: the :func:`set_default_timeout` value, else the
    ``XA11Y_DEFAULT_TIMEOUT`` environment variable, else the built-in 5.0.
    """

def input_sim() -> InputSim:
    """Construct an ``InputSim`` backed by the platform's native input path."""

def screenshot(
    *,
    element: Element | None = None,
    region: tuple[int, int, int, int] | None = None,
) -> Screenshot:
    """Capture pixels from the screen.

    With no arguments, captures the full primary display. Pass ``element``
    to capture the pixels under an element's current bounds, or ``region``
    as ``(x, y, width, height)`` to capture an explicit rectangle in logical
    screen coordinates. Passing both raises ``ValueError``.
    """

# ── Test helpers ─────────────────────────────────────────────────────────────

def _make_test_locator() -> Locator: ...
def _make_disconnected_subscription() -> Subscription: ...

class _TestActionProbe:
    def locator(self, selector: str) -> Locator: ...
    def actions(self) -> list[list[object]]: ...
    def clear(self) -> None: ...

def _make_test_action_probe() -> _TestActionProbe: ...
