"""Typed action schema for the desktop tools.

Every action is its own model in a discriminated union, so the model sees exactly
the fields a given action needs instead of one flat bag of optional parameters.
"""

from __future__ import annotations

from typing import List, Literal, Optional, Tuple, Union

from pydantic import BaseModel, Field, model_validator

# ── Targets ──────────────────────────────────────────────────────────────────


class ElementTarget(BaseModel):
    """An accessibility element, addressed by selector or by snapshot ref."""

    app: Optional[str] = Field(
        default=None,
        description=(
            "Application to act on: an exact or partial name, 'pid:1234', or 'foreground'. "
            "Defaults to the foreground application. Ignored when 'ref' is given, since a "
            "ref already carries its application."
        ),
    )
    selector: Optional[str] = Field(
        default=None,
        description=(
            "xa11y selector, e.g. \"button[name='Save']\", \"text_field[name^='Search']\", "
            '"group > button", "menu_item[name=\'Copy\']:nth(2)".'
        ),
    )
    ref: Optional[str] = Field(
        default=None,
        description="Ref from an earlier snapshot or find, e.g. 'e12'. Preferred over selector when available.",
    )

    @model_validator(mode="after")
    def _exactly_one_locator(self) -> "ElementTarget":
        given = [name for name in ("selector", "ref") if getattr(self, name) is not None]
        if len(given) != 1:
            raise ValueError(f"provide exactly one of 'selector' or 'ref' (got: {', '.join(given) or 'neither'})")
        return self


class PointerTarget(BaseModel):
    """A pointer destination: an element, or a raw screen point.

    Prefer an element — it keeps the action anchored to the accessibility tree even
    when the pointer path is synthesised. Points are the fallback for surfaces the
    tree does not expose, such as a canvas.
    """

    app: Optional[str] = Field(default=None, description="See ElementTarget.app.")
    selector: Optional[str] = Field(default=None, description="See ElementTarget.selector.")
    ref: Optional[str] = Field(default=None, description="See ElementTarget.ref.")
    point: Optional[Tuple[int, int]] = Field(
        default=None,
        description=(
            "Absolute screen point (x, y) in logical coordinates, origin at the top-left of the primary display."
        ),
    )

    @model_validator(mode="after")
    def _exactly_one_locator(self) -> "PointerTarget":
        given = [name for name in ("selector", "ref", "point") if getattr(self, name) is not None]
        if len(given) != 1:
            raise ValueError(
                f"provide exactly one of 'selector', 'ref', or 'point' (got: {', '.join(given) or 'none'})"
            )
        return self


# ── Tier 1: perceive ─────────────────────────────────────────────────────────


class ListAppsAction(BaseModel):
    """List running applications reachable through the accessibility bridge."""

    type: Literal["list_apps"] = Field(description="List running applications and which one is in the foreground")


class SnapshotAction(BaseModel):
    """Render an application's accessibility tree with a ref on every node."""

    type: Literal["snapshot"] = Field(
        description="Read the accessibility tree of an app as ref-annotated text. Start here, not with a screenshot"
    )
    app: Optional[str] = Field(default=None, description="See ElementTarget.app.")
    selector: Optional[str] = Field(
        default=None, description="Scope the snapshot to the subtree under the first element matching this selector."
    )
    ref: Optional[str] = Field(default=None, description="Scope the snapshot to the subtree under this ref.")
    max_depth: int = Field(default=12, ge=0, description="Maximum tree depth to descend.")
    max_nodes: int = Field(default=200, ge=1, description="Node budget; truncation is always reported, never silent.")
    detail: Literal["basic", "rich"] = Field(
        default="rich",
        description=(
            "'rich' reads per-node state (disabled, checked, focused, ...) — one accessibility call per property, "
            "which is slow over D-Bus on Linux. 'basic' reads role/name/value only, in a single bulk call."
        ),
    )
    interactive_only: bool = Field(
        default=True, description="Drop decorative nodes, keeping actionable controls, text, and their ancestors."
    )
    include_bounds: bool = Field(default=False, description="Append each node's on-screen rectangle.")

    @model_validator(mode="after")
    def _one_scope(self) -> "SnapshotAction":
        if self.selector is not None and self.ref is not None:
            raise ValueError("scope the snapshot with 'selector' or 'ref', not both")
        return self


class FindAction(BaseModel):
    """Query for every element matching a selector."""

    type: Literal["find"] = Field(description="Find all elements matching a selector and assign them refs")
    selector: str = Field(description="xa11y selector to match.")
    app: Optional[str] = Field(default=None, description="See ElementTarget.app.")
    limit: int = Field(default=20, ge=1, description="Maximum matches to return.")


class ReadAction(BaseModel):
    """Read the full property set of one element."""

    type: Literal["read"] = Field(description="Read every accessibility property of a single element")
    target: ElementTarget


class WaitAction(BaseModel):
    """Block until an element reaches a state."""

    type: Literal["wait"] = Field(description="Wait for an element to reach a state before continuing")
    target: ElementTarget
    condition: Literal["visible", "hidden", "attached", "detached", "enabled", "disabled", "focused", "unfocused"] = (
        Field(default="visible", description="State to wait for.")
    )
    timeout: float = Field(default=5.0, ge=0, description="Seconds to wait before giving up.")


# ── Tier 2: act through the accessibility layer ──────────────────────────────


class ClickAction(BaseModel):
    """Activate an element."""

    type: Literal["click"] = Field(description="Activate an element (the accessibility press action where possible)")
    target: PointerTarget
    button: Literal["left", "right", "middle"] = Field(default="left", description="Mouse button.")
    count: int = Field(default=1, ge=1, le=3, description="Click count; 2 is a double-click.")
    modifiers: List[str] = Field(default_factory=list, description="Keys held during the click, e.g. ['Shift'].")


class TypeAction(BaseModel):
    """Enter text."""

    type: Literal["type"] = Field(description="Type text into an element, or into whatever currently has focus")
    text: str = Field(description="Text to enter.")
    target: Optional[ElementTarget] = Field(
        default=None, description="Element to type into. Omit to type into the focused element."
    )
    replace: bool = Field(default=False, description="Replace the existing value instead of inserting at the cursor.")
    press_enter: bool = Field(default=False, description="Send Enter after the text.")


class FocusAction(BaseModel):
    """Move keyboard focus to an element."""

    type: Literal["focus"] = Field(description="Move keyboard focus to an element")
    target: ElementTarget


class ActAction(BaseModel):
    """Perform a semantic accessibility action other than click/type/focus."""

    type: Literal["act"] = Field(description="Perform a semantic accessibility action on an element")
    target: ElementTarget
    verb: Literal[
        "toggle",
        "check",
        "uncheck",
        "select",
        "expand",
        "collapse",
        "increment",
        "decrement",
        "set_number",
        "select_text",
        "show_menu",
        "scroll_into_view",
        "blur",
        "raw",
    ] = Field(description="Action to perform. 'check'/'uncheck' toggle only when the current state differs.")
    number: Optional[float] = Field(default=None, description="Value for 'set_number'.")
    start: Optional[int] = Field(default=None, description="Start offset for 'select_text'.")
    end: Optional[int] = Field(default=None, description="End offset for 'select_text'.")
    repeat: int = Field(default=1, ge=1, description="Repetitions for 'increment' / 'decrement'.")
    action_name: Optional[str] = Field(
        default=None,
        description="Platform action name for 'raw', taken from an element's 'actions' list. The escape hatch.",
    )

    @model_validator(mode="after")
    def _verb_requirements(self) -> "ActAction":
        required = {
            "set_number": ("number",),
            "select_text": ("start", "end"),
            "raw": ("action_name",),
        }.get(self.verb, ())
        missing = [field for field in required if getattr(self, field) is None]
        if missing:
            raise ValueError(f"verb '{self.verb}' requires: {', '.join(missing)}")
        return self


# ── Tier 3: synthesised input and pixels ─────────────────────────────────────


class KeyAction(BaseModel):
    """Send keystrokes to whatever has focus."""

    type: Literal["key"] = Field(
        description=("Send keystrokes. Global shortcuts have no accessibility equivalent, so this is how to send them")
    )
    keys: List[str] = Field(
        description=(
            "Keys to tap in order. Printable characters are literal ('a', '7'); named keys use their Pascal name "
            "('Enter', 'Escape', 'Tab', 'ArrowUp', 'F5'). Common aliases such as 'esc' or 'cmd' are normalised."
        )
    )
    hold: List[str] = Field(
        default_factory=list,
        description="Modifiers held for the whole sequence: 'Shift', 'Ctrl', 'Alt', 'Meta' (Command on macOS).",
    )
    repeat: int = Field(default=1, ge=1, description="Times to repeat the sequence.")
    app: Optional[str] = Field(default=None, description="Bring this application to the foreground first.")


class MouseAction(BaseModel):
    """Move or hold the pointer without completing a click."""

    type: Literal["mouse"] = Field(description="Move the pointer, or press/release a button without a full click")
    target: Optional[PointerTarget] = Field(default=None, description="Required for 'move'; ignored for 'down'/'up'.")
    op: Literal["move", "down", "up"] = Field(default="move", description="Pointer operation.")
    button: Literal["left", "right", "middle"] = Field(default="left", description="Button for 'down' / 'up'.")

    @model_validator(mode="after")
    def _move_needs_target(self) -> "MouseAction":
        if self.op == "move" and self.target is None:
            raise ValueError("op 'move' requires a target")
        return self


class DragAction(BaseModel):
    """Drag from one place to another."""

    type: Literal["drag"] = Field(description="Drag between two points or elements")
    start: PointerTarget
    end: PointerTarget
    button: Literal["left", "right", "middle"] = Field(default="left", description="Button held during the drag.")
    modifiers: List[str] = Field(default_factory=list, description="Keys held during the drag.")
    duration: float = Field(
        default=0.15,
        gt=0,
        description=(
            "Time spent moving the pointer from start to end, in seconds. On macOS the drag also holds "
            "200 ms after press and 500 ms before release."
        ),
    )


class ScrollAction(BaseModel):
    """Turn the scroll wheel over a target."""

    type: Literal["scroll"] = Field(description="Scroll the wheel over an element or point")
    target: PointerTarget
    dx: int = Field(default=0, description="Horizontal steps; positive scrolls right.")
    dy: int = Field(default=0, description="Vertical steps; positive scrolls down.")

    @model_validator(mode="after")
    def _needs_a_direction(self) -> "ScrollAction":
        if self.dx == 0 and self.dy == 0:
            raise ValueError("scroll needs a non-zero dx or dy")
        return self


class ScreenshotAction(BaseModel):
    """Capture pixels. The fallback for anything the tree does not describe."""

    type: Literal["screenshot"] = Field(
        description="Capture pixels — a fallback for canvases, video, and rendering bugs. Prefer snapshot"
    )
    target: Optional[ElementTarget] = Field(default=None, description="Capture only this element's bounds.")
    region: Optional[Tuple[int, int, int, int]] = Field(
        default=None, description="Capture the rectangle (x, y, width, height) in logical screen coordinates."
    )
    send_image: bool = Field(
        default=False,
        description=(
            "Return the image to the model. Off by default: it is expensive in tokens and can expose whatever "
            "else is on the user's screen. Turn it on only when the user asked to see the screen or the "
            "accessibility tree genuinely cannot answer the question."
        ),
    )
    save_path: Optional[str] = Field(default=None, description="Also write the PNG to this path.")

    @model_validator(mode="after")
    def _one_capture_area(self) -> "ScreenshotAction":
        if self.target is not None and self.region is not None:
            raise ValueError("provide 'target' or 'region', not both")
        return self


# ── Lifecycle ────────────────────────────────────────────────────────────────


class OpenAppAction(BaseModel):
    """Launch an application and wait for it to register with the accessibility bridge."""

    type: Literal["open_app"] = Field(description="Launch an application and wait until it is reachable")
    name: str = Field(min_length=1, description="Application name or executable.")
    timeout: float = Field(default=30.0, ge=0, description="Seconds to wait for the app to become reachable.")


class CloseAppAction(BaseModel):
    """Terminate an application's processes."""

    type: Literal["close_app"] = Field(description="Terminate an application's processes")
    name: str = Field(
        min_length=2,
        description=(
            "Application name to match against running process names, case-insensitively, as a "
            "substring. Matching is deliberately broad, so give the most specific name you have — "
            "every match is terminated."
        ),
    )


PerceiveAction = Union[ListAppsAction, SnapshotAction, FindAction, ReadAction, WaitAction, ScreenshotAction]

AnyAction = Union[
    ListAppsAction,
    SnapshotAction,
    FindAction,
    ReadAction,
    WaitAction,
    ClickAction,
    TypeAction,
    FocusAction,
    ActAction,
    KeyAction,
    MouseAction,
    DragAction,
    ScrollAction,
    ScreenshotAction,
    OpenAppAction,
    CloseAppAction,
]


class DesktopInput(BaseModel):
    """Input envelope for the read-write desktop tool."""

    action: AnyAction = Field(discriminator="type", description="The action to perform.")


class InspectInput(BaseModel):
    """Input envelope for the read-only desktop tool."""

    action: PerceiveAction = Field(discriminator="type", description="The read-only action to perform.")


# Actions that change the machine's state, and so require consent.
MUTATING_ACTIONS = frozenset(
    {"click", "type", "focus", "act", "key", "mouse", "drag", "scroll", "open_app", "close_app"}
)
