"""Rendering the accessibility tree into something worth spending tokens on.

The snapshot is the tool's primary sense. It has to be complete enough to act from
and small enough to re-read every turn, which means three deliberate limits: a depth
cap, a node budget, and a filter that drops decoration. All three are reported when
they bite — a truncated tree that looks complete is worse than no tree.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

from . import _selectors
from ._refs import REFS, segment

# Controls worth acting on.
ACTIONABLE_ROLES = frozenset(
    {
        "button",
        "check_box",
        "combo_box",
        "link",
        "list_item",
        "menu_item",
        "radio_button",
        "scroll_bar",
        "slider",
        "spin_button",
        "switch",
        "tab",
        "text_area",
        "text_field",
        "tree_item",
    }
)

# Containers whose identity orients the reader even when they do nothing.
LANDMARK_ROLES = frozenset({"alert", "dialog", "menu", "menu_bar", "navigation", "toolbar", "window"})

# Nodes that carry information rather than behaviour; kept when they have text.
INFORMATIVE_ROLES = frozenset({"heading", "image", "progress_bar", "static_text", "status", "table_cell", "tooltip"})

VALUE_LIMIT = 160


@dataclass
class Node:
    """One collected node, before filtering and rendering."""

    role: str
    name: Optional[str] = None
    value: Optional[str] = None
    states: List[str] = field(default_factory=list)
    bounds: Optional[Any] = None
    stable_id: Optional[str] = None
    element: Optional[Any] = None
    path: Optional[str] = None
    children: List["Node"] = field(default_factory=list)
    keep: bool = True


class _Budget:
    """Shared node allowance across a recursive walk."""

    def __init__(self, limit: int) -> None:
        self.remaining = limit
        self.truncated = False

    def take(self) -> bool:
        if self.remaining <= 0:
            self.truncated = True
            return False
        self.remaining -= 1
        return True


def _truncate(value: Optional[str], limit: int = VALUE_LIMIT) -> Optional[str]:
    if value is None:
        return None
    value = " ".join(value.split())
    if not value:
        return None
    return value if len(value) <= limit else value[: limit - 1] + "…"


def read_states(element: Any) -> List[str]:
    """The states worth spending characters on: the ones that change what to do next."""
    states = []
    if not element.enabled:
        states.append("disabled")
    if not element.visible:
        states.append("hidden")
    if element.focused:
        states.append("focused")
    checked = element.checked
    if checked is not None:
        states.append({"on": "checked", "off": "unchecked", "mixed": "mixed"}.get(checked, f"checked={checked}"))
    if element.selected:
        states.append("selected")
    expanded = element.expanded
    if expanded is not None:
        states.append("expanded" if expanded else "collapsed")
    for flag in ("required", "busy", "modal", "active"):
        if getattr(element, flag, False):
            states.append(flag)
    return states


def _join(base: Optional[str], step: str) -> Optional[str]:
    """Extend a selector path.

    ``base`` is ``""`` at the application root, a selector when the snapshot is scoped
    to one, and ``None`` when the scope has no expressible path — in which case no
    descendant gets one either, and their refs fall back to stable_id or a live handle.
    """
    if base is None:
        return None
    if not base:
        return step
    # Distribute over clauses: a group base concatenated with " > step" would
    # bind the step to the last clause alone. See _selectors.chain.
    return _selectors.chain(base, " > ", step)


def _child_segments(children: List[Any], role_of: Any, name_of: Any) -> List[str]:
    """Path segments for a sibling list, disambiguated by position where needed."""
    counts: Dict[tuple, int] = {}
    for child in children:
        key = (role_of(child), name_of(child))
        counts[key] = counts.get(key, 0) + 1

    seen: Dict[tuple, int] = {}
    segments = []
    for child in children:
        role, name = role_of(child), name_of(child)
        key = (role, name)
        seen[key] = seen.get(key, 0) + 1
        segments.append(segment(role, name, seen[key], counts[key]))
    return segments


def collect_rich(
    element: Any, depth: int, budget: _Budget, base_path: Optional[str], include_bounds: bool
) -> Optional[Node]:
    """Walk live elements, reading per-node state. One accessibility call per property."""
    if not budget.take():
        return None
    try:
        node = Node(
            role=element.role,
            name=_truncate(element.name, 80),
            value=_truncate(element.value),
            states=read_states(element),
            bounds=element.bounds if include_bounds else None,
            stable_id=element.stable_id,
            element=element,
            path=base_path or None,
        )
    except Exception:  # noqa: BLE001 - a node that vanished mid-walk is not worth failing the snapshot over
        return None

    if depth <= 0:
        return node

    try:
        children = element.children()
    except Exception:  # noqa: BLE001 - same
        return node

    def role_of(child: Any) -> str:
        try:
            return str(child.role)
        except Exception:  # noqa: BLE001 - the path segment for a node that is already gone
            return "unknown"

    def name_of(child: Any) -> Optional[str]:
        try:
            name: Optional[str] = child.name
            return name
        except Exception:  # noqa: BLE001 - same
            return None

    # Read defensively: these run outside the per-node guard below, so an element that
    # vanished mid-walk would otherwise take the whole snapshot down with it.
    segments = _child_segments(children, role_of, name_of)
    for child, child_segment in zip(children, segments, strict=False):
        collected = collect_rich(child, depth - 1, budget, _join(base_path, child_segment), include_bounds)
        if collected is not None:
            node.children.append(collected)
        elif budget.truncated:
            # Out of budget: stop the whole walk, which the caller reports. A child that
            # merely vanished mid-walk costs only itself — dropping its later siblings too
            # would silently hand back a tree that looks complete.
            break
    return node


def collect_basic(raw: Dict[str, Any], depth: int, budget: _Budget, base_path: Optional[str]) -> Optional[Node]:
    """Walk the dict returned by Element.tree(): role, name, value, children — one bulk call."""
    if not budget.take():
        return None
    node = Node(
        role=raw.get("role") or "unknown",
        name=_truncate(raw.get("name"), 80),
        value=_truncate(raw.get("value")),
        path=base_path or None,
    )
    if depth <= 0:
        return node

    children = raw.get("children") or []
    segments = _child_segments(children, lambda child: child.get("role") or "unknown", lambda child: child.get("name"))
    for child, child_segment in zip(children, segments, strict=False):
        collected = collect_basic(child, depth - 1, budget, _join(base_path, child_segment))
        if collected is None:  # a dict node cannot fail to read, so this is always the budget
            break
        node.children.append(collected)
    return node


def _interesting(node: Node) -> bool:
    if node.role in ACTIONABLE_ROLES or node.role in LANDMARK_ROLES:
        return True
    if node.role in INFORMATIVE_ROLES:
        return bool(node.name or node.value)
    return False


def prune(node: Node) -> bool:
    """Mark nodes to drop, bottom-up. A node survives if it matters or a descendant does."""
    kept_child = False
    for child in node.children:
        kept_child = prune(child) or kept_child
    node.keep = kept_child or _interesting(node)
    return node.keep


def _format_bounds(bounds: Any) -> str:
    if bounds is None:
        return ""
    return f" @{bounds.x},{bounds.y} {bounds.width}x{bounds.height}"


def render(node: Node, app_key: str, lines: List[str], level: int, interactive_only: bool) -> None:
    """Render a node and its children, issuing a ref for every line emitted."""
    emitted = node.keep or not interactive_only
    if emitted:
        ref = REFS.issue(
            app_key,
            node.role,
            name=node.name,
            value=node.value,
            stable_id=node.stable_id,
            path=node.path,
            element=node.element,
        )
        parts = [f"{ref.ref} {node.role}"]
        if node.name:
            parts.append(f'"{node.name}"')
        if node.value:
            parts.append(f'value="{node.value}"')
        if node.states:
            parts.append(f"[{' '.join(node.states)}]")
        lines.append("  " * level + " ".join(parts) + _format_bounds(node.bounds))

    for child in node.children:
        if interactive_only and not child.keep:
            continue
        render(child, app_key, lines, level + 1 if emitted else level, interactive_only)


def describe_element(element: Any) -> str:
    """A one-line description of a live element, for 'find' results."""
    parts = [element.role]
    if element.name:
        parts.append(f'"{_truncate(element.name, 80)}"')
    value = _truncate(element.value)
    if value:
        parts.append(f'value="{value}"')
    states = read_states(element)
    if states:
        parts.append(f"[{' '.join(states)}]")
    return " ".join(parts)


def properties(element: Any) -> Dict[str, Any]:
    """Every documented property of an element, for 'read'."""
    fields = (
        "role",
        "name",
        "value",
        "description",
        "numeric_value",
        "min_value",
        "max_value",
        "stable_id",
        "pid",
        "actions",
        "enabled",
        "visible",
        "focused",
        "active",
        "checked",
        "selected",
        "expanded",
        "editable",
        "focusable",
        "modal",
        "required",
        "busy",
    )
    read: Dict[str, Any] = {}
    for name in fields:
        try:
            read[name] = getattr(element, name)
        except Exception as exc:  # noqa: BLE001 - report per-property failures rather than losing the whole read
            read[name] = f"<unavailable: {exc}>"
    try:
        bounds = element.bounds
        read["bounds"] = (
            None if bounds is None else {"x": bounds.x, "y": bounds.y, "width": bounds.width, "height": bounds.height}
        )
    except Exception as exc:  # noqa: BLE001 - same
        read["bounds"] = f"<unavailable: {exc}>"
    return read
