"""Clause-aware selector-string manipulation.

xa11y selectors may be a *group*: comma-separated clauses, matched as a union.
``"button[name='All Clear'], button[name='Clear']"`` is the form the selector
reference teaches for "either of these".

That matters here because this package builds selector *paths* — the strings a
ref resolves through later — by extending a selector the model supplied. xa11y
splits a selector on its top-level commas first and applies everything else per
clause, so extending a group by plain concatenation silently rebinds it:

    "a, b" + " > step"   parses as   ["a", "b > step"]      -- scope lost on "a"
    "a, b" + ":nth(2)"   parses as   ["a", "b:nth(2)"]      -- nth on one clause

A path built that way can still resolve to exactly one element, which is all
``resolve_ref`` checks — so the agent acts on the wrong control and is told it
succeeded. These helpers keep the clause structure intact.
"""

from __future__ import annotations

from typing import List, Optional

__all__ = ["split_clauses", "is_group", "chain", "nth"]


def split_clauses(selector: str) -> List[str]:
    """Split on top-level commas, ignoring those inside quotes or brackets.

    Mirrors xa11y's own ``split_top_level_commas``: a comma inside
    ``[name="a,b"]`` is part of the value, not a clause boundary.
    """
    clauses: List[str] = []
    current: List[str] = []
    quote: Optional[str] = None
    depth = 0
    escaped = False

    for char in selector:
        if escaped:
            current.append(char)
            escaped = False
            continue
        if quote and char == "\\":
            current.append(char)
            escaped = True
            continue
        if quote:
            current.append(char)
            if char == quote:
                quote = None
            continue
        if char in "\"'":
            quote = char
            current.append(char)
            continue
        if char == "[":
            depth += 1
        elif char == "]":
            depth = max(0, depth - 1)
        if char == "," and depth == 0:
            clauses.append("".join(current).strip())
            current = []
            continue
        current.append(char)

    clauses.append("".join(current).strip())
    return [c for c in clauses if c]


def is_group(selector: str) -> bool:
    """True when ``selector`` has more than one top-level clause."""
    return len(split_clauses(selector)) > 1


def chain(base: str, combinator: str, suffix: str) -> str:
    """Distribute ``combinator + suffix`` over every clause of ``base``.

    ``chain("a, b", " > ", "c")`` is ``"a > c, b > c"`` — the same rule xa11y's
    own ``Locator::child`` / ``descendant`` apply, so a path built here means
    what the equivalent locator chain would mean.
    """
    return ", ".join(f"{clause}{combinator}{suffix}" for clause in split_clauses(base))


def nth(selector: str, position: int) -> Optional[str]:
    """``selector`` narrowed to its ``position``-th match, or ``None``.

    Returns ``None`` for a group. ``:nth`` binds *within* a clause, so there is
    no selector string that means "the nth element of a comma union" — writing
    ``"a:nth(2), b:nth(2)"`` selects two elements, not one. A ref with no path
    falls back to a stable id or a live handle, which is correct; a path that
    resolves to the wrong element is not.
    """
    if is_group(selector):
        return None
    return f"{selector}:nth({position})"
