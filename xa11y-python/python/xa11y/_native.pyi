"""Type stubs for the xa11y native extension module."""

from __future__ import annotations

from collections.abc import Callable, Iterator
from types import TracebackType
from typing import Literal, TypedDict

class _PointerTargetDiagnosis(TypedDict):
    """Typing-only shape of the plain pointer diagnostic dictionary.

    Named anchors are strings; offsets are ``{"offset": {"dx": int, "dy": int}}``.
    Checked point/display geometry uses the named coordinate space.
    """

    point: dict[str, int]
    element_bounds: dict[str, int] | None
    anchor: AnchorName | dict[str, dict[str, int]] | None
    checked_point: dict[str, int]
    coordinate_space: str
    displays: list[dict[str, int]]
    omitted_displays: int

# Input-layer string values. These spellings are identical in the JS binding —
# like key names, they are shared across bindings rather than spelled
# per-language.
MouseButtonName = Literal["left", "right", "middle"]
AnchorName = Literal["center", "top_left", "top_right", "bottom_left", "bottom_right"]

# Why an annotated element is not in the image. Identically spelled in the JS
# binding, like every other enum value a user compares against as a literal.
OmissionReasonName = Literal["no_bounds", "zero_area", "outside_capture"]

# Why an element reads ``visible=False`` (``Element.hidden_reason``, and the
# ``hidden_reason`` of a timeout). Identically spelled in the JS binding.
#   scrolled_out      outside the viewport of a scrolling ancestor, so
#                     scroll_into_view can fix it
#   window_minimized  its window is minimized; restore the window
#   not_showing       the platform reports it not drawn without saying why
#                     (a collapsed container, an inactive tab)
#   unknown           the provider did not establish why
# The values are spelled only in the Literal (comments do not count), so the
# parity check's variant coverage notices one going missing from it.
HiddenReasonName = Literal["window_minimized", "scrolled_out", "not_showing", "unknown"]

# Whether a failed action may already have taken effect (``XA11yError.delivery``).
# ``"not_delivered"`` is safe to retry; ``"unknown"`` is not. Identically
# spelled in the JS binding.
DeliveryName = Literal["not_delivered", "unknown"]

# What an auto-waiting action's scroll attempt came to (the ``auto_scroll`` of
# a timeout). Identically spelled in the JS binding.
#   refused    the element has no semantic scroll call; asked once per wait
#   no_effect  the platform accepted the call, and the next check still read
#              the element not visible (scrolled out, or hidden otherwise)
#   revealed   the next check read it visible; something else, such as
#              enabled=False, kept the action from running
AutoScrollOutcomeName = Literal["refused", "no_effect", "revealed"]

# Shell-surface kinds. Like roles and key names, these strings are identical in
# the JS binding — a kind is a value the user writes as a literal.
ShellSurfaceKindName = Literal[
    "menu_bar",
    "status_items",
    "taskbar",
    "panel",
    "dock",
    "desktop",
    "flyout",
    "unknown",
]

# ── Exceptions ───────────────────────────────────────────────────────────────

class XA11yError(Exception):
    """Base exception for all xa11y errors."""

    delivery: DeliveryName
    """Whether the failed action may already have taken effect. Always set
    on errors xa11y raises. ``'not_delivered'``: the failure happened before
    the action was dispatched (selector not matched, action not supported,
    element stale, an auto-wait that timed out), so a retry is safe.
    ``'unknown'``: the action was dispatched or may have been (a D-Bus
    ``NoReply``, an AX ``CannotComplete``, a UIA timeout, any platform
    error), so a retry can apply it twice."""

class PermissionDeniedError(XA11yError):
    """Accessibility permissions have not been granted."""

class AccessibilityNotEnabledError(XA11yError):
    """The target app advertises an accessibility tree but it is empty.

    Raised on Linux when a Chromium/Electron app is launched without
    ``--force-renderer-accessibility``, so the renderer accessibility bridge
    never populates the window's subtree. In a bare session with no settings
    daemon (CI, Xvfb, a container) the app needs both the flag and
    ``ACCESSIBILITY_ENABLED=1`` in its environment.
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
    hidden_reason: HiddenReasonName | None
    """Why the last observed element was not visible. Set by timeouts; always
    ``None`` for this class (present for attribute parity with
    :class:`TimeoutError`)."""
    auto_scroll: AutoScrollOutcomeName | None
    """What an auto-waiting action's scroll attempt came to. Set by timeouts;
    always ``None`` for this class (present for attribute parity with
    :class:`TimeoutError`)."""
    candidates: list[str]
    """Bounded near-miss candidates, e.g. same-role elements with different
    names, or the running applications for app lookups."""
    scope: str | None
    """Bounded rendering of the search scope: a depth-limited tree dump for
    scoped locators, or the application list for rootless ones."""
    pointer_target: _PointerTargetDiagnosis | None
    """Geometry of an off-display pointer target, otherwise ``None``."""
    elapsed: float | None
    """Always ``None`` for this class (present for attribute parity with
    :class:`TimeoutError`)."""

StaleReasonName = Literal[
    "removed", "process_exited", "process_restarted", "identity_changed", "evicted"
]

class ElementStaleError(SelectorNotMatchedError):
    """The element no longer names a live node, so nothing was done to it.

    A subclass of :class:`SelectorNotMatchedError`, so existing handlers keep
    catching it. xa11y never re-matches a stale element by name: re-resolve
    with a locator to act on whatever is there now."""

    reason: StaleReasonName
    """Why: ``"removed"`` (the node is gone), ``"process_exited"``,
    ``"process_restarted"``, ``"identity_changed"`` (the platform id now
    names a node with a different role) or ``"evicted"`` (the provider's
    bounded handle table dropped it)."""
    element: str
    """What was captured: ``role "name"``, or the handle when that is all
    the provider still has."""

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
    hidden_reason: HiddenReasonName | None
    """Why the last observed element was not visible, when it matched but
    read ``visible=False``; the values are listed at :data:`HiddenReasonName`.
    ``None`` when nothing hidden was observed."""
    auto_scroll: AutoScrollOutcomeName | None
    """What the action's auto-scroll came to, when its target was enabled
    and scrolled out; the values are listed at :data:`AutoScrollOutcomeName`.
    It describes the first check after the latest accepted scroll, so
    ``revealed`` alongside ``hidden_reason`` scrolled out means the element
    came into view and later scrolled back out. ``None`` when the wait never
    tried to scroll."""
    candidates: list[str]
    """Bounded near-miss candidates (same role, different attributes).
    Collected only when the selector never matched during the wait."""
    scope: str | None
    """Bounded rendering of the search scope. Collected only when the
    selector never matched during the wait."""
    pointer_target: _PointerTargetDiagnosis | None

class InvalidSelectorError(XA11yError):
    """The selector string has invalid syntax."""

class InvalidActionDataError(XA11yError):
    """An action received invalid data (e.g. ``Locator.nth(0)``, or a
    text-selection range with ``start > end``)."""

class InvalidTextRangeError(InvalidActionDataError):
    """A text range is out of bounds, reversed, or splits a surrogate pair.

    Offsets are UTF-16 code units. ``length`` is the text's current length when
    the check knew it, so a range made stale by an edit says how long the text
    is now."""

    start: int
    """Start of the refused range (UTF-16 code units)."""
    end: int
    """End of the refused range (UTF-16 code units)."""
    length: int | None
    """The text's length when the check knew it, else ``None``."""

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

# ── Tables and collections ───────────────────────────────────────────────────

CountValue = int | Literal["unknown"]
"""A total: an ``int``, or ``"unknown"`` when the platform says it does not know."""

class TableInfo:
    """Size of a table, grid or tree grid. Each count is ``None`` when not
    reported."""

    @property
    def row_count(self) -> CountValue | None:
        """Number of rows: ``int``, ``"unknown"``, or ``None``."""
    @property
    def column_count(self) -> CountValue | None:
        """Number of columns: ``int``, ``"unknown"``, or ``None``."""
    def __repr__(self) -> str: ...

class CellInfo:
    """Row, column and spans of a table cell. Every index is 0-based; a field
    the platform did not report is ``None``."""

    @property
    def row(self) -> int | None:
        """0-based row index."""
    @property
    def column(self) -> int | None:
        """0-based column index."""
    @property
    def row_span(self) -> int | None:
        """Rows the cell covers (at least 1), or ``None`` when not reported."""
    @property
    def column_span(self) -> int | None:
        """Columns the cell covers (at least 1), or ``None`` when not reported."""
    @property
    def header(self) -> Literal["column", "row"] | None:
        """``"column"`` or ``"row"`` for a header cell, else ``None``."""
    def __repr__(self) -> str: ...

class RowInfo:
    """Row index of a table or tree-grid row (0-based)."""

    @property
    def index(self) -> int | None:
        """0-based row index in the table, or ``None`` when not reported."""
    def __repr__(self) -> str: ...

class CollectionInfo:
    """Position of an item in a list, tree, tab list or radio group.

    ``index`` and ``depth`` are 0-based: the first item is ``0``, a top-level
    tree item has depth ``0``. To show "3 of 10", add 1 to ``index``."""

    @property
    def index(self) -> int | None:
        """0-based position in the set."""
    @property
    def size(self) -> CountValue | None:
        """Number of items in the set: ``int``, ``"unknown"``, or ``None``."""
    @property
    def depth(self) -> int | None:
        """0-based nesting depth."""
    def __repr__(self) -> str: ...

# ── Text by range ────────────────────────────────────────────────────────────

TextUnitName = Literal["character", "word", "line", "paragraph"]

class TextRange:
    """A half-open range ``[start, end)`` of UTF-16 code units.

    Anywhere a range is accepted, a ``(start, end)`` tuple works too."""

    def __new__(cls, start: int, end: int) -> TextRange:
        """Raises ``InvalidTextRangeError`` when ``start > end``."""
    @property
    def start(self) -> int:
        """0-based offset of the first unit."""
    @property
    def end(self) -> int:
        """0-based offset one past the last unit."""
    def len(self) -> int:
        """Number of UTF-16 code units covered."""
    def is_empty(self) -> bool:
        """Whether the range covers nothing (a caret position)."""
    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

RangeLike = TextRange | tuple[int, int]

class TextAttributes:
    """Formatting of a run of text. Every attribute is ``None`` when the
    platform did not report it; nothing is defaulted."""

    @property
    def font_family(self) -> str | None:
        """Font family name."""
    @property
    def font_size(self) -> float | None:
        """Font size in points."""
    @property
    def font_weight(self) -> int | None:
        """Font weight on the CSS scale: 400 normal, 700 bold."""
    @property
    def italic(self) -> bool | None:
        """Italic or oblique."""
    @property
    def underline(self) -> bool | None:
        """Underlined."""
    @property
    def strikethrough(self) -> bool | None:
        """Struck through."""
    @property
    def foreground(self) -> str | None:
        """Text colour as ``"#rrggbb"`` or ``"#rrggbbaa"``."""
    @property
    def background(self) -> str | None:
        """Background colour as ``"#rrggbb"`` or ``"#rrggbbaa"``."""
    @property
    def script(self) -> Literal["baseline", "superscript", "subscript"] | None:
        """``"baseline"``, ``"superscript"`` or ``"subscript"``."""
    @property
    def spelling_error(self) -> bool | None:
        """Marked as a spelling error."""
    def __repr__(self) -> str: ...

class TextRun:
    """A run of text sharing one set of attributes."""

    @property
    def range(self) -> TextRange:
        """The text the run covers."""
    @property
    def attributes(self) -> TextAttributes:
        """Its formatting."""
    def __repr__(self) -> str: ...

class TextLines:
    """One page of rendered lines from ``Text.lines``."""

    @property
    def lines(self) -> list[TextRange]:
        """The lines, each clipped to the requested range."""
    @property
    def truncated(self) -> bool:
        """Whether the limit stopped the walk before the end of the range."""
    @property
    def next_start(self) -> int | None:
        """Where to start the next page; set exactly when ``truncated``."""
    def __repr__(self) -> str: ...

class TextRuns:
    """One page of formatting runs from ``Text.runs``."""

    @property
    def runs(self) -> list[TextRun]:
        """The runs, each clipped to the requested range."""
    @property
    def truncated(self) -> bool:
        """Whether the limit stopped the walk before the end of the range."""
    @property
    def next_start(self) -> int | None:
        """Where to start the next page; set exactly when ``truncated``."""
    def __repr__(self) -> str: ...

TextChangeKindName = Literal["inserted", "deleted"]

class TextChange:
    """What a ``text_changed`` event inserted or deleted (``Event.text_change``).

    Each field is what the platform supplied: ``range`` is ``None`` when it
    counts offsets in a unit other than UTF-16 (on Linux, every toolkit but
    Qt), and ``text`` is ``None`` when it does not send the text."""

    @property
    def kind(self) -> TextChangeKindName:
        """``"inserted"`` or ``"deleted"``."""
    @property
    def range(self) -> TextRange | None:
        """Where, in UTF-16 code units: for an insertion the range the new text
        now occupies, for a deletion the range it occupied. ``None`` when not
        reported in UTF-16."""
    @property
    def text(self) -> str | None:
        """The inserted or deleted text, or ``None`` when not supplied."""
    def __repr__(self) -> str: ...

class Text:
    """An element's text, read by range. Every method is a live platform
    read; offsets are UTF-16 code units, so use ``read`` rather than slicing
    ``Element.value`` when the text may contain emoji.

    From ``Element.text()`` it stays bound to that element. From
    ``Locator.text()`` it re-resolves the locator on every read."""

    def length(self) -> int:
        """Length in UTF-16 code units, without reading the text where the
        platform allows."""
    def caret(self) -> int | None:
        """The caret offset, or ``None`` when the element has no caret."""
    def selections(self) -> list[TextRange]:
        """The selected ranges in document order; empty when nothing is selected."""
    def read(self, range: RangeLike) -> str:
        """The text in ``range`` (a ``TextRange`` or ``(start, end)``), exactly
        as the platform reports it."""
    def read_all(self) -> str:
        """The whole text."""
    def find(
        self, needle: str, *, occurrence: int = 0, within: RangeLike | None = None
    ) -> TextRange | None:
        """The ``occurrence``-th (0-based) exact match of ``needle``, searching
        ``within`` if given. Case-sensitive, no Unicode normalization."""
    def range_at(self, offset: int, unit: TextUnitName) -> TextRange:
        """The ``unit`` (``"character"``, ``"word"``, ``"line"``,
        ``"paragraph"``) containing ``offset``. ``"line"`` is the rendered line."""
    def lines(self, range: RangeLike, limit: int = 200) -> TextLines:
        """Rendered lines covering ``range``, at most ``limit`` per page."""
    def bounds(self, range: RangeLike) -> list[Rect]:
        """Screen rectangles of ``range``, one per rendered line fragment, in the
        same coordinates as ``Element.bounds``."""
    def runs(self, range: RangeLike, limit: int = 200) -> TextRuns:
        """Formatting runs covering ``range``, at most ``limit`` per page."""
    def __repr__(self) -> str: ...

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
    TEXT_SELECTION_CHANGED: str = "text_selection_changed"
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
    @property
    def text_change(self) -> TextChange | None:
        """For ``text_changed`` events: what was inserted or deleted, when the
        platform reports it (Linux only for now).

        ``None`` for other event types, and for a ``text_changed`` with no
        delta. A ``text_selection_changed`` event carries no offsets: read
        ``Text.caret()`` / ``Text.selections()`` on the target.
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
    def is_foreground(self) -> bool:
        """Whether this application is the foreground application.

        Named ``is_foreground`` because "focused" is reserved for element-level
        keyboard focus (:attr:`Element.focused`) elsewhere in the API; this is
        the foreground-*application* flag. Populated for apps obtained via
        :meth:`list` and :meth:`find`. A point-in-time snapshot taken when the
        ``App`` was resolved.

        Tagging is window-precise. On Windows apps surface as one synthesized
        ``Application`` node per process, so a pid match there is unambiguous;
        on Linux the AT-SPI registry can surface several entries for one pid,
        and only the entry reporting the window-level ``active`` flag gets
        ``is_foreground``. Use :meth:`foreground` to resolve the foreground
        application directly, then pick the exact foreground window from its
        ``windows()``.
        """
    @property
    def focused(self) -> bool:
        """Deprecated alias for :attr:`is_foreground`.

        .. deprecated::
            Use :attr:`is_foreground`. "focused" refers to element keyboard
            focus elsewhere in the API; accessing this property emits a
            :class:`DeprecationWarning`.
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

        Queries the platform's foreground mechanism directly rather than
        enumerating and tagging by pid. The result is the foreground
        *process*'s ``Application`` node — one per process on every platform —
        not the exact window holding the focus; on Windows that is the
        synthesized node of the foreground process (the modal case, issues
        #304/#305), and this stays reliable when an app shows a modal dialog.
        To reach the exact foreground window, call ``windows()`` and pick the
        entry reporting ``active``. "Nothing focused" retries until ``timeout``;
        see ``by_name`` for ``timeout`` semantics. The returned app has
        ``is_foreground == True``.
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
    def windows(self) -> list[Element]:
        """List the top-level windows of this application.

        Each call queries the provider; results are not cached. The windows
        are the application's top-level windows with role ``window`` or
        ``dialog``, in enumeration order. On Windows the application entry is
        a synthesized process node whose children are the process's top-level
        windows (main window plus modal dialogs). On Linux the answer is
        process-complete: ``App::windows_with`` merges the filtered children
        of every same-pid AT-SPI Application entry (an app that registers
        several entries reports its whole process), so the results need not
        be the direct children of this node and no single z-order spans them.
        Calling ``windows`` on a non-application element fails with
        ``ActionNotSupportedError``.
        """
    def enable_web_accessibility(self, *, timeout: float | None = None) -> None:
        """Turn on the accessibility tree for the app's web content, then wait
        for that content to appear.

        Chromium and Electron build the accessibility tree for their web
        content only when a client asks for it. Until then a selector for an
        element on the page finds nothing. This makes that request; xa11y
        never makes it implicitly.

        On macOS it sets ``AXManualAccessibility`` (Electron) on the
        application element and reads it back, then does the same with
        ``AXEnhancedUserInterface`` (Chrome) if that did not read back as
        set, raising ``AccessibilityNotEnabledError`` when neither does. It
        does not check that the app is Chromium-based: an app that keeps
        either value counts as having taken the request, and the call then
        raises ``TimeoutError`` because no web content appears. The attribute
        stays set to true afterwards and is not restored; on an app that
        reacts to ``AXEnhancedUserInterface`` that can disable window
        animations or confuse a window manager. It then waits up to
        ``timeout`` seconds (default: the
        process-wide default timeout) until some ``web_area`` has a child,
        raising ``TimeoutError`` with a diagnosis otherwise. On Linux and
        Windows it raises ``ActionNotSupportedError`` saying what to do
        instead: on Linux, launch the app with
        ``--force-renderer-accessibility``.
        """
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

# ── ShellSurface ──────────────────────────────────────────────────────────────

class ShellSurface:
    """One OS-owned shell surface — the taskbar, a desktop panel, the dock, the
    menu bar, a process's status items, the desktop, or an open flyout.

    ``ShellSurface`` is **not** an :class:`Element`. It represents the surface
    as a whole and provides a ``locator()`` to search its accessibility tree,
    exactly as :class:`App` does for an application.
    """

    @property
    def kind(self) -> ShellSurfaceKindName:
        """What this surface is, as a snake_case string (``"taskbar"``,
        ``"status_items"``, ...). The same spelling in both bindings, in the
        CLI's ``--shell`` flag, and in the ``shell_kind`` raw attribute on the
        surface root."""
    @property
    def name(self) -> str:
        """The owning app for per-app surfaces (``"Safari"`` menu bar), the
        platform's own name otherwise (``"Taskbar"``, ``"Dock"``). Falls back
        to the kind when the platform vends no name for the root."""
    @property
    def pid(self) -> int | None:
        """Owning process where the platform reports one honestly. On Windows
        this is the shell *host* (explorer.exe), because UIA carries no
        per-icon owner — documented as the host, never faked."""
    @staticmethod
    def list() -> list[ShellSurface]:
        """List the OS shell surfaces currently on screen.

        Single enumeration, no polling. The listing is live: ``flyout``
        surfaces appear only while they are open, and enumerating never opens,
        closes, focuses, or presses anything. A platform with no surface of a
        given kind simply returns none — that is honest scope, not a failure.
        """
    @staticmethod
    def by_kind(kind: ShellSurfaceKindName, *, timeout: float | None = None) -> ShellSurface:
        """Wait for exactly one shell surface of ``kind``.

        ``kind`` is the snake_case kind name — ``ShellSurfaceKindName`` above
        is the full set, and is the only place this file spells them out. An
        unknown name raises ``ValueError`` before the accessibility API is
        touched.

        Polls until a single surface of that kind exists or ``timeout``
        (seconds) elapses — see ``App.by_name`` for ``timeout`` semantics. The
        wait is what makes the Windows overflow workflow a one-liner: press the
        taskbar's "Show Hidden Icons" button, then wait for the flyout::

            taskbar = xa11y.ShellSurface.by_kind("taskbar", timeout=0)
            taskbar.locator("button[name='Show Hidden Icons']").press()
            flyout = xa11y.ShellSurface.by_kind("flyout", timeout=3.0)

        Raises :class:`SelectorNotMatchedError` both when no surface of that
        kind is present and when *several* are — ambiguity is refused rather
        than first-matched, with the candidates on the exception's
        ``candidates`` attribute so the caller can disambiguate via
        :meth:`list` and a pid.
        """
    def locator(self, selector: str) -> Locator:
        """Create a Locator scoped to this surface's accessibility tree."""
    def children(self) -> list[Element]:
        """Get direct children of the surface root."""
    def as_element(self) -> Element:
        """Get an :class:`Element` handle for the surface root.

        Useful for invoking Element-level methods (``children()``,
        ``parent()``, etc.) without going through a locator.
        """
    def tree(self, max_depth: int | None = None) -> dict:
        """Capture this surface's accessibility tree as a recursive dict.

        Each dict has keys ``role``, ``name``, ``value``, and ``children``
        (a list of dicts with the same shape). ``max_depth`` limits traversal:
        ``0`` = only the surface root, ``1`` = root + direct children,
        ``None`` = full subtree.
        """
    def dump(self, max_depth: int | None = None) -> str:
        """Render this surface's accessibility tree as an indented string.

        Returns the string without printing it. Same depth semantics as
        :meth:`tree`. This is the primary inspection helper — call
        ``print(surface.dump())`` to discover the role and name of every
        element in the surface before writing selectors.
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
    def hidden_reason(self) -> HiddenReasonName | None:
        """Why the element is not visible, or ``None`` when it is.

        The values and what each calls for are listed at
        :data:`HiddenReasonName`. Which reasons a platform can tell apart is
        in the platform details reference.
        """
    @property
    def focused(self) -> bool: ...
    @property
    def active(self) -> bool:
        """Whether this element is the active (foreground) window.

        The window that currently receives the user's input. Meaningful for
        window-like elements (windows, dialogs); ``False`` elsewhere. Distinct
        from :attr:`focused`, which is element-level keyboard focus.
        """
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
    @property
    def password(self) -> bool:
        """Whether this is a password (secure text) field.

        The role stays ``text_field``. xa11y redacts nothing, so a caller that
        must keep a secret out of its logs checks this flag.
        """
        ...
    @property
    def minimized(self) -> bool | None:
        """Whether the window is minimized.

        ``None`` means unknown (the element is not a window, or the platform
        cannot report the state). Never a guessed ``False``.
        """
    @property
    def maximized(self) -> bool | None:
        """Whether the window is maximized. ``None`` = unknown / not a window."""
    @property
    def fullscreen(self) -> bool | None:
        """Whether the window is fullscreen. ``None`` = unknown / not a window."""
    def same_as(self, other: Element) -> bool:
        """Whether ``other`` names the same platform node. ``==`` uses this.

        Raises :class:`ElementStaleError` (``reason="evicted"``) when either
        handle is no longer held by the provider."""
    def check_alive(self) -> None:
        """Prove this element still names the node it was found as. Raises
        :class:`ElementStaleError` with the ``reason`` otherwise. Every action
        runs this check first."""
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
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
        """Click / invoke this element."""
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
    def select_text(self, start: int | RangeLike, end: int | None = None) -> None:
        """Select a text range: ``select_text(start, end)``, or
        ``select_text(range)`` with a ``TextRange`` or ``(start, end)`` tuple.
        Offsets are 0-based UTF-16 code units."""
    @property
    def table(self) -> TableInfo | None:
        """Size of a table, grid or tree grid; ``None`` when not a table or not
        reported."""
    @property
    def cell(self) -> CellInfo | None:
        """Row, column and spans of a table cell (0-based); ``None`` otherwise."""
    @property
    def table_row(self) -> RowInfo | None:
        """Row index of a table row (0-based); ``None`` otherwise."""
    @property
    def collection(self) -> CollectionInfo | None:
        """Position in a list, tree, tab list or radio group (0-based index and
        depth); ``None`` otherwise."""
    def text(self) -> Text:
        """This element's text, read by range (UTF-16 offsets). Raises
        ``ActionNotSupportedError`` when the element has no text interface."""
    def cell_at(self, row: int, column: int) -> Element | None:
        """On a table: the cell at 0-based ``row`` / ``column`` (a merged cell
        for a slot it covers), or ``None``."""
    def column_headers(self) -> list[Element]:
        """On a table: its column header cells."""
    def row_headers(self) -> list[Element]:
        """On a table: its row header cells."""
    def column_header_cells(self) -> list[Element]:
        """On a cell: the column header cells that head it."""
    def row_header_cells(self) -> list[Element]:
        """On a cell: the row header cells that head it."""
    def selected_items(self) -> list[Element]:
        """On a list, tree, table or tab list: the selected items."""
    def perform_action(self, action: str) -> None:
        """Perform an action by ``snake_case`` name."""
    def activate(self) -> None:
        """Activate this window: bring it to the foreground and give it focus."""
    def minimize(self) -> None:
        """Minimize this window."""
    def maximize(self) -> None:
        """Maximize this window.

        Distinct from ``enter_fullscreen``: this drives the platform's
        maximized state, not native fullscreen. Raises
        ``ActionNotSupportedError`` on a platform without an accessible
        maximize (macOS exposes no readable or writable zoom state).
        """
    def enter_fullscreen(self) -> None:
        """Put this window in native fullscreen (macOS ``AXFullScreen``).

        Distinct from ``maximize``; ``restore`` leaves fullscreen. Raises
        ``ActionNotSupportedError`` on platforms with no fullscreen
        accessibility API (Windows/Linux).
        """
    def restore(self) -> None:
        """Restore this window to its normal state (from minimized/maximized/fullscreen)."""
    def close(self) -> None:
        """Close this window."""
    def move_to(self, x: int, y: int) -> None:
        """Move this window to the given logical screen coordinates (top-left origin)."""
    def resize_to(self, width: int, height: int) -> None:
        """Resize this window to the given logical width and height.

        Raises ``InvalidActionDataError`` if either dimension is 0.
        ``OverflowError`` if either is negative or exceeds ``u32``.
        """
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
    def select_text(self, start: int | RangeLike, end: int | None = None) -> None:
        """Select a text range within the matched element: ``(start, end)`` or a
        single range. Offsets are 0-based UTF-16 code units."""
    def text(self) -> Text:
        """The matched element's text, read by range. Re-resolves the locator on
        every read, waiting only for the element to exist."""
    def perform_action(self, action: str) -> None:
        """Perform an action by snake_case name."""
    def activate(self) -> None:
        """Activate the matched window: bring it to the foreground and give it focus."""
    def minimize(self) -> None:
        """Minimize the matched window."""
    def maximize(self) -> None:
        """Maximize the matched window.

        Distinct from ``enter_fullscreen``: this drives the platform's
        maximized state, not native fullscreen.
        """
    def enter_fullscreen(self) -> None:
        """Put the matched window in native fullscreen.

        Distinct from ``maximize``; ``restore`` leaves fullscreen.
        """
    def restore(self) -> None:
        """Restore the matched window to its normal state (from minimized,
        maximized, or fullscreen).

        This is the inverse of ``minimize``, ``maximize``, and
        ``enter_fullscreen``: it clears every special state the platform can
        clear. There is deliberately no separate "exit fullscreen" method —
        leaving fullscreen is the same absolute state write this performs.
        """
    def close(self) -> None:
        """Close the matched window."""
    def move_to(self, x: int, y: int) -> None:
        """Move the matched window to the given logical screen coordinates (top-left origin)."""
    def resize_to(self, width: int, height: int) -> None:
        """Resize the matched window to the given logical dimensions.

        Raises ``InvalidActionDataError`` if either dimension is 0, before
        any auto-wait polling begins. ``OverflowError`` if either is negative
        or exceeds ``u32``.
        """
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
    def wait_stable(self, timeout: float | None = None) -> Element:
        """Wait until visible bounds are observed unchanged for at least 750 ms
        across repeated fresh polls, then return the element.
        Changes, missing matches, hidden elements, or absent bounds reset the
        window. This measures observed geometry; native snapshots can lag motion.

        Use before an ``InputSim`` pointer action on an element that may still
        be animating, so the click lands where it settles:
        ``sim.click(locator.wait_stable())``. Unrelated to ``stable_id``.

        Raises ``TimeoutError`` whose diagnosis names the last two differing
        bounds, a match that is not visible, or a selector that never matched.
        """
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

    Built-in backends reject pointer targets outside every active display
    before sending input. On Wayland, raw points use the configured virtual
    input extent. This checks screen membership, not visibility or hit testing.
    ``SelectorNotMatchedError.pointer_target`` carries the requested point,
    checked coordinate space, displays, and any element bounds and anchor.
    Both drag endpoints are checked before the gesture. Captured moves while
    this simulator holds a mouse button, and button releases, remain possible;
    explicit clicks and scrolls still require an on-display target.
    """

    def click(
        self,
        target: tuple[int, int] | Element,
        *,
        button: MouseButtonName = "left",
        count: int = 1,
        held: list[str] | None = None,
        anchor: AnchorName | tuple[int, int] | None = None,
    ) -> None:
        """Click at ``target``.

        ``button`` selects the mouse button, ``count`` repeats the click
        (``2`` = double-click), ``held`` lists keys held down for the
        duration, and ``anchor`` picks the point inside an ``Element``'s
        bounds — a named anchor or an ``(dx, dy)`` offset from its top-left
        corner. ``anchor`` is ignored when ``target`` is a raw tuple.

        On a Linux Wayland session an ``Element`` target raises
        ``ActionNotSupportedError``: Wayland clients do not know their global
        position, so accessibility bounds are not screen coordinates. Use a
        semantic action (``press``, ``focus``) or pass a raw ``(x, y)``.
        """
    def double_click(self, target: tuple[int, int] | Element) -> None:
        """Left double-click at ``target``."""
    def right_click(self, target: tuple[int, int] | Element) -> None:
        """Right-click at ``target``."""
    def move_to(self, target: tuple[int, int] | Element) -> None:
        """Move the pointer to ``target`` without pressing any button."""
    def mouse_down(self, button: MouseButtonName = "left") -> None:
        """Press a mouse button at the current pointer location, without
        releasing it.

        Pair with :meth:`mouse_up`; for a whole click use :meth:`click`.
        Takes no target — the button is pressed wherever the pointer already
        is, so call :meth:`move_to` first to position it.
        """
    def mouse_up(self, button: MouseButtonName = "left") -> None:
        """Release a mouse button at the current pointer location."""
    def drag(
        self,
        start: tuple[int, int] | Element,
        end: tuple[int, int] | Element,
        *,
        button: MouseButtonName = "left",
        held: list[str] | None = None,
        duration: float = 0.15,
    ) -> None:
        """Drag from ``start`` to ``end``.

        ``button`` selects the button held during the drag, ``held`` lists
        keys held for its duration, and ``duration`` is the time spent moving
        the pointer from start to end, in **seconds**. On macOS the drag also
        holds 200 ms after press and 500 ms before release.
        """
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
    def key_down(self, key: str) -> None:
        """Press ``key`` without releasing it. Pair with :meth:`key_up`.

        For a whole tap use :meth:`press`; to hold modifiers around one tap
        use :meth:`chord`. This is the primitive for sequences neither
        expresses, such as holding a key across several other actions.
        """
    def key_up(self, key: str) -> None:
        """Release a key previously pressed with :meth:`key_down`."""
    def type_text(self, text: str) -> None:
        """Type literal text into the currently focused control.

        ``"\\r\\n"``, ``"\\r"`` and ``"\\n"`` each type one Return/Enter
        keypress, so in a single-line field this may activate the default
        button.
        """

# ── Screenshot ───────────────────────────────────────────────────────────────

class LegendEntry:
    """One drawn annotation box: the tag in the image, and the element it
    came from.

    A model reads a tag off the PNG; ``selector`` is what acts on the element
    that tag labels::

        entry = next(e for e in shot.legend if e.tag == "B1")
        app.locator(entry.selector).set_value("42")
    """

    @property
    def tag(self) -> str:
        """What is drawn in the box — ``"B7"``. A group letter and a 1-based
        index, with no separator that downscaling could lose."""
    @property
    def group(self) -> int:
        """1-based, matching the position of this element's locator in
        ``annotate=``."""
    @property
    def index(self) -> int:
        """1-based, and exactly the ``:nth(n)`` argument in :attr:`selector`."""
    @property
    def selector(self) -> str:
        """A selector usable as-is against the same scope the group had —
        ``"button:nth(7)"``."""
    @property
    def role(self) -> str:
        """The element's role, snake_case as everywhere else."""
    @property
    def name(self) -> str | None:
        """The element's accessible name, when it has one."""
    @property
    def bounds(self) -> Rect:
        """The element's bounds in logical screen coordinates."""
    @property
    def color(self) -> tuple[int, int, int]:
        """The box colour as an ``(r, g, b)`` tuple, for correlating a box
        with its entry by eye."""
    def __repr__(self) -> str: ...

class Omission:
    """An element that matched an ``annotate=`` selector but is not in the
    image.

    Reported rather than dropped: a legend that disagreed with the picture,
    with no way to find out why, is what this exists to prevent.
    """

    @property
    def selector(self) -> str:
        """The selector that would reach this element."""
    @property
    def role(self) -> str:
        """The element's role, snake_case."""
    @property
    def name(self) -> str | None:
        """The element's accessible name, when it has one."""
    @property
    def reason(self) -> OmissionReasonName:
        """Why it could not be drawn: no bounds at all, zero-area bounds, or
        bounds outside what was captured (a second monitor, or outside an
        explicit ``region``)."""
    def __repr__(self) -> str: ...

class Screenshot:
    """A captured image: raw RGBA8 pixels plus dimensions and mapping.

    ``width`` and ``height`` describe the actual returned image. ``scale`` is
    a compatibility hint for simple single-display captures; use the explicit
    coordinate methods for mixed-DPI, cropped, or resized images. ``pixels``
    has length ``width * height * 4``.

    ``legend``, ``omitted`` and ``truncated`` describe what ``annotate=``
    drew. They are ``[]``, ``[]`` and ``0`` on an unannotated capture, so
    consumers need no version check.
    """

    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def scale(self) -> float: ...
    @property
    def mapping_available(self) -> bool:
        """Whether desktop/image coordinate conversion metadata is available."""
    @property
    def pixels(self) -> bytes:
        """Raw RGBA8 pixel bytes (``width * height * 4``)."""
    @property
    def legend(self) -> list[LegendEntry]:
        """One entry per drawn box, in group order and then match order."""
    @property
    def omitted(self) -> list[Omission]:
        """Elements that matched an ``annotate=`` selector but could not be
        drawn, each with the reason."""
    @property
    def truncated(self) -> int:
        """How many matched elements were not described at all because the
        annotation cap was reached. ``0`` when the cap did not bite."""
    def to_png(self) -> bytes:
        """Encode the image as a PNG and return the bytes."""
    def save_png(self, path: str | bytes | object) -> None:
        """Encode as PNG and write to ``path``. Accepts ``str``, ``bytes`` or ``os.PathLike``."""
    def desktop_to_image(self, x: int, y: int) -> tuple[int, int]:
        """Convert a desktop point to a pixel in this image."""
    def image_to_desktop(self, x: int, y: int) -> tuple[int, int]:
        """Convert an image pixel to a desktop point suitable for input."""
    def desktop_rect_to_image(self, x: int, y: int, width: int, height: int) -> list[Rect]:
        """Convert a desktop rectangle to one image rectangle per display piece."""
    def crop(self, x: int, y: int, width: int, height: int) -> Screenshot:
        """Crop by image pixels while preserving coordinate mapping."""
    def resize(self, width: int, height: int) -> Screenshot:
        """Resize image pixels while scaling coordinate mapping."""
    def __repr__(self) -> str: ...

# ── Capabilities ─────────────────────────────────────────────────────────────

PlatformName = Literal["linux", "macos", "windows"]
SupportName = Literal["supported", "partial", "unsupported"]

class Capability:
    """One feature's answer on one platform."""

    @property
    def name(self) -> str:
        """Feature name, ``<area>:<name>`` (``"event:menu_opened"``)."""
    @property
    def description(self) -> str:
        """What the feature is, in one sentence."""
    @property
    def support(self) -> SupportName:
        """``"supported"``, ``"partial"`` or ``"unsupported"``."""
    @property
    def note(self) -> str | None:
        """How the platform provides it, or why it does not; ``None`` when there
        is nothing to add."""
    @property
    def since(self) -> str | None:
        """The first OS version that supports it, when older ones do not."""
    def __repr__(self) -> str: ...

class Capabilities:
    """Every feature's answer on one platform. Get one from
    :func:`xa11y.capabilities`.

    Iterating yields :class:`Capability` objects in table order; ``name in
    caps`` tests whether a feature name is known.
    """

    @staticmethod
    def names() -> list[str]:
        """Every feature name the table knows, in table order."""
    @property
    def platform(self) -> PlatformName:
        """The platform described."""
    @property
    def os_version(self) -> str | None:
        """The OS version the answers were narrowed to, or ``None`` when none
        was given or detected. Always ``None`` on Linux."""
    def entries(self) -> list[Capability]:
        """Every feature, in table order."""
    def get(self, name: str) -> Capability:
        """The capability named ``name``. Raises :class:`InvalidActionDataError`
        for an unknown name, listing the closest known ones."""
    def support(self, name: str) -> SupportName:
        """Shorthand for ``get(name).support``."""
    def note(self, name: str) -> str | None:
        """Shorthand for ``get(name).note``."""
    def has(self, name: str) -> bool:
        """Whether ``name`` is a feature the table knows (also ``name in caps``)."""
    def __len__(self) -> int: ...
    def __contains__(self, name: object) -> bool: ...
    def __getitem__(self, name: str) -> Capability: ...
    def __iter__(self) -> Iterator[Capability]: ...
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

def capabilities(
    platform: PlatformName | None = None, os_version: str | None = None
) -> Capabilities:
    """What a platform's backend can report: one answer per feature, read from
    a static table without connecting to any app.

    With no arguments, describes the running platform and OS version. Pass
    ``platform`` to describe another one, and ``os_version`` to narrow it to
    an OS release.
    """

def input_sim() -> InputSim:
    """Construct an ``InputSim`` backed by the platform's native input path."""

def screenshot(
    *,
    element: Element | None = None,
    region: tuple[int, int, int, int] | None = None,
    annotate: list[Locator | str] | None = None,
) -> Screenshot:
    """Capture pixels from the screen.

    With no arguments, captures the full primary display. Pass ``element``
    to capture the pixels under an element's current bounds, or ``region``
    as ``(x, y, width, height)`` to capture an explicit rectangle in logical
    screen coordinates. Passing both raises ``ValueError``.

    ``annotate`` draws a numbered box over every element each locator
    matches, and fills in :attr:`Screenshot.legend`,
    :attr:`Screenshot.omitted` and :attr:`Screenshot.truncated`. Each entry
    is one group, with its own colour and tag letter, and must be scoped to
    an application: ``app.locator("button")``. An entry that is neither a
    ``Locator`` nor a ``str`` raises ``TypeError``.

    A rootless group raises ``InvalidSelectorError`` — a bare ``str``, or
    :func:`locator`, searches every application, and each entry's
    ``<selector>:nth(n)`` would then count within one application while the
    legend counts across all of them. The two agree only for a single
    application, so the selector on an entry would name a different element
    than the box it labels.

    Cropping and annotating are independent: annotations outside the
    captured area land in ``omitted`` rather than being clamped to an edge.

        shot = xa11y.screenshot(
            element=app.locator("window").element(),
            annotate=[app.locator("button"), app.locator("text_field")],
        )
    """

# ── CLI entry point ──────────────────────────────────────────────────────────

def _cli_main(args: list[str]) -> int:
    """Run the Rust CLI and return the process exit code.

    ``args`` excludes the program name. Returns ``0`` on success, ``1`` on an
    operation failure, ``2`` on a usage error; the message is written to
    stderr by the Rust side. Backs the ``xa11y`` console script.
    """

# ── Test helpers ─────────────────────────────────────────────────────────────

def _make_test_locator() -> Locator: ...
def _make_test_input(delay_ms: int = 0) -> InputSim: ...
def _make_test_shell_surfaces() -> list[ShellSurface]: ...
def _find_test_shell_surface(
    kind: ShellSurfaceKindName, *, timeout: float | None = None
) -> ShellSurface: ...
def _make_disconnected_subscription() -> Subscription: ...
def _make_test_text_event_subscription() -> Subscription: ...

class _TestActionProbe:
    def locator(self, selector: str) -> Locator: ...
    def actions(self) -> list[list[object]]: ...
    def clear(self) -> None: ...

def _make_test_action_probe() -> _TestActionProbe: ...
