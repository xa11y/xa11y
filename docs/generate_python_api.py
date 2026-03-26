#!/usr/bin/env python3
"""Generate the Python API reference MDX from the _native.pyi type stubs.

Usage:
    python docs/generate_python_api.py

Reads:  xa11y-python/python/xa11y/_native.pyi
Writes: docs/site/src/content/docs/api/python.mdx

The .pyi stubs are the single source of truth — docstrings, signatures,
and type annotations all come from there.
"""

from __future__ import annotations

import ast
import textwrap
from pathlib import Path

import re

REPO_ROOT = Path(__file__).resolve().parent.parent
STUB_PATH = REPO_ROOT / "xa11y-python" / "python" / "xa11y" / "_native.pyi"
OUTPUT_PATH = (
    REPO_ROOT / "docs" / "site" / "src" / "content" / "docs" / "api" / "python.mdx"
)

# ── Helpers ──────────────────────────────────────────────────────────────────

PRIVATE_PREFIXES = ("_",)
DUNDER_ALLOWLIST = {"__init__", "__iter__"}
SKIP_DUNDERS = {"__repr__", "__str__", "__eq__", "__enter__", "__exit__"}


def _should_include(name: str) -> bool:
    if name in SKIP_DUNDERS:
        return False
    if name in DUNDER_ALLOWLIST:
        return True
    return not name.startswith("_")


# Type aliases to expand in rendered output
_TYPE_ALIASES: dict[str, str] = {}


def _collect_type_aliases(tree: ast.Module) -> None:
    """Scan top-level assignments for type aliases like `_Target = Node | str`."""
    for node in tree.body:
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            if isinstance(node, ast.Assign) and len(node.targets) == 1:
                target = node.targets[0]
                if isinstance(target, ast.Name) and node.value is not None:
                    _TYPE_ALIASES[target.id] = ast.unparse(node.value)


def _unparse_annotation(node: ast.expr | None) -> str:
    """Turn an AST annotation node back into a string, expanding type aliases."""
    if node is None:
        return ""
    raw = ast.unparse(node)
    return _TYPE_ALIASES.get(raw, raw)


def _format_return(ann: str) -> str:
    if not ann:
        return ""
    return f" → {ann}"


def _get_docstring_value(node: ast.AST) -> str | None:
    """Extract raw docstring text from an AST node, or None."""
    if not (
        hasattr(node, "body")
        and node.body
        and isinstance(node.body[0], ast.Expr)
        and isinstance(node.body[0].value, ast.Constant)
        and isinstance(node.body[0].value.value, str)
    ):
        return None
    return node.body[0].value.value


def _first_line_docstring(node: ast.AST) -> str:
    """Extract the first line of a docstring from an AST node's body."""
    text = _get_docstring_value(node)
    if text is None:
        return ""
    return text.strip().split("\n")[0]


def _full_docstring(node: ast.AST) -> str:
    """Extract the full docstring from an AST node."""
    text = _get_docstring_value(node)
    if text is None:
        return ""
    return textwrap.dedent(text).strip()


def _format_signature(func: ast.FunctionDef, *, skip_self: bool = True) -> str:
    """Format function arguments into a signature string."""
    args = func.args
    parts: list[str] = []

    # Positional args
    all_args = args.args
    defaults = args.defaults
    num_no_default = len(all_args) - len(defaults)

    for i, arg in enumerate(all_args):
        if skip_self and i == 0 and arg.arg in ("self", "cls"):
            continue
        ann = _unparse_annotation(arg.annotation)
        name = arg.arg
        param = f"{name}: {ann}" if ann else name

        default_idx = i - num_no_default
        if default_idx >= 0 and defaults[default_idx] is not None:
            default_val = ast.unparse(defaults[default_idx])
            param += f" = {default_val}"
        parts.append(param)

    # *args
    if args.vararg:
        ann = _unparse_annotation(args.vararg.annotation)
        parts.append(f"*{args.vararg.arg}: {ann}" if ann else f"*{args.vararg.arg}")

    # keyword-only marker
    if args.kwonlyargs and not args.vararg:
        parts.append("*")

    # keyword-only args
    for i, arg in enumerate(args.kwonlyargs):
        ann = _unparse_annotation(arg.annotation)
        name = arg.arg
        param = f"{name}: {ann}" if ann else name
        if i < len(args.kw_defaults) and args.kw_defaults[i] is not None:
            default_val = ast.unparse(args.kw_defaults[i])
            param += f" = {default_val}"
        parts.append(param)

    return ", ".join(parts)


# ── AST Extraction ───────────────────────────────────────────────────────────


def _extract_classes_and_functions(
    tree: ast.Module,
) -> tuple[list[ast.ClassDef], list[ast.FunctionDef]]:
    classes = []
    functions = []
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            classes.append(node)
        elif isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
            if _should_include(node.name):
                functions.append(node)
    return classes, functions


def _classify_class(cls: ast.ClassDef) -> str:
    """Return 'exception', 'data', or 'main'."""
    for base in cls.bases:
        base_name = ast.unparse(base)
        if base_name in (
            "Exception",
            "XA11yError",
            "BaseException",
        ):
            return "exception"
    if cls.name in ("Rect", "NormalizedRect", "AppInfo"):
        return "data"
    return "main"


# ── MDX Generation ───────────────────────────────────────────────────────────

FRONTMATTER = """\
---
title: Python API Reference
description: API reference for the xa11y Python package — auto-generated from type stubs.
---

{/* Auto-generated by docs/generate_python_api.py from _native.pyi — do not edit by hand. */}

The `xa11y` Python package provides bindings to the xa11y Rust library via PyO3.

Install from PyPI:

```bash
pip install xa11y
```

Quick start:

```python
import xa11y

# One-liner
tree = xa11y.app("Safari")

# With explicit provider (recommended)
with xa11y.connect() as provider:
    tree = provider.app("Safari")
    for button in tree.query("button"):
        print(button.name)
    tree.press("button[name='OK']")
```"""


def _render_param_table(func: ast.FunctionDef, *, skip_self: bool = True) -> list[str]:
    """Render function parameters as a Markdown table."""
    args = func.args
    all_args = args.args
    defaults = args.defaults
    num_no_default = len(all_args) - len(defaults)
    rows: list[str] = []

    for i, arg in enumerate(all_args):
        if skip_self and i == 0 and arg.arg in ("self", "cls"):
            continue
        ann = _escape_table_pipe(_unparse_annotation(arg.annotation))
        default_idx = i - num_no_default
        default = ""
        if default_idx >= 0 and defaults[default_idx] is not None:
            default = f"`{ast.unparse(defaults[default_idx])}`"
        rows.append(f"| `{arg.arg}` | `{ann}` | {default} |")

    for i, arg in enumerate(args.kwonlyargs):
        ann = _escape_table_pipe(_unparse_annotation(arg.annotation))
        default = ""
        if i < len(args.kw_defaults) and args.kw_defaults[i] is not None:
            default = f"`{ast.unparse(args.kw_defaults[i])}`"
        rows.append(f"| `{arg.arg}` | `{ann}` | {default} |")

    if not rows:
        return []
    return [
        "| Parameter | Type | Default |",
        "| --------- | ---- | ------- |",
        *rows,
    ]


def _render_function_section(func: ast.FunctionDef, *, prefix: str = "xa11y") -> str:
    ret = _unparse_annotation(func.returns)
    doc = _full_docstring(func)
    lines = [f"### `{prefix}.{func.name}(){_format_return(ret)}`"]
    if doc:
        lines.append("")
        for paragraph in doc.split("\n\n"):
            cleaned = "\n".join(line.strip() for line in paragraph.splitlines())
            lines.append(cleaned)
            lines.append("")

    param_table = _render_param_table(func, skip_self=False)
    if param_table:
        lines.append("")
        lines.extend(param_table)
        lines.append("")

    return "\n".join(lines).rstrip()


def _escape_table_pipe(text: str) -> str:
    """Escape pipe characters in text destined for a Markdown table cell."""
    return text.replace("|", "\\|")


def _render_properties_table(members: list[ast.FunctionDef]) -> str:
    lines = [
        "| Property | Type | Description |",
        "| -------- | ---- | ----------- |",
    ]
    for m in members:
        ret = _escape_table_pipe(_unparse_annotation(m.returns))
        doc = _first_line_docstring(m)
        lines.append(f"| `{m.name}` | `{ret}` | {doc} |")
    return "\n".join(lines)


def _render_methods_table(
    members: list[ast.FunctionDef], *, category_label: str | None = None
) -> str:
    lines: list[str] = []
    if category_label:
        lines.append(f"#### {category_label}")
        lines.append("")
    lines.extend(
        [
            "| Method | Returns | Description |",
            "| ------ | ------- | ----------- |",
        ]
    )
    for m in members:
        sig = _escape_table_pipe(_format_signature(m))
        ret = _unparse_annotation(m.returns)
        doc = _first_line_docstring(m)
        # Suppress None return type for __init__ and void actions
        ret_display = f"`{_escape_table_pipe(ret)}`" if ret and ret != "None" else ""
        lines.append(f"| `{m.name}({sig})` | {ret_display} | {doc} |")
    return "\n".join(lines)


def _categorize_tree_methods(
    methods: list[ast.FunctionDef],
) -> dict[str, list[ast.FunctionDef]]:
    """Split Tree methods into semantic groups."""
    query_names = {"children", "parent", "query", "find_by_role", "find_by_name"}

    groups: dict[str, list[ast.FunctionDef]] = {
        "Query & Navigation": [],
        "Action Shortcuts": [],
        "Other": [],
    }
    for m in methods:
        if m.name in query_names:
            groups["Query & Navigation"].append(m)
        elif m.name.startswith("__"):
            groups["Other"].append(m)
        else:
            groups["Action Shortcuts"].append(m)
    return groups


def _categorize_locator_methods(
    methods: list[ast.FunctionDef],
) -> dict[str, list[ast.FunctionDef]]:
    nav_names = {"nth", "first", "child", "descendant"}
    inspect_names = {
        "role",
        "name",
        "value",
        "description",
        "is_visible",
        "is_enabled",
        "is_focused",
        "exists",
        "count",
        "get",
    }

    groups: dict[str, list[ast.FunctionDef]] = {
        "Navigation": [],
        "Inspection": [],
        "Actions": [],
        "Waiting": [],
    }
    for m in methods:
        if m.name in nav_names:
            groups["Navigation"].append(m)
        elif m.name in inspect_names:
            groups["Inspection"].append(m)
        elif m.name.startswith("wait_"):
            groups["Waiting"].append(m)
        else:
            groups["Actions"].append(m)
    return groups


def _render_class(cls: ast.ClassDef) -> str:
    lines: list[str] = []
    doc = _full_docstring(cls)
    lines.append(f"### `{cls.name}`")
    lines.append("")
    if doc:
        for paragraph in doc.split("\n\n"):
            cleaned = "\n".join(line.strip() for line in paragraph.splitlines())
            lines.append(cleaned)
            lines.append("")

    # Separate properties and methods
    properties: list[ast.FunctionDef] = []
    methods: list[ast.FunctionDef] = []
    for item in cls.body:
        if isinstance(item, ast.FunctionDef):
            if not _should_include(item.name):
                continue
            # Detect @property decorator
            is_prop = any(
                (isinstance(d, ast.Name) and d.id == "property")
                or (isinstance(d, ast.Attribute) and d.attr == "property")
                for d in item.decorator_list
            )
            if is_prop:
                properties.append(item)
            else:
                methods.append(item)

    if properties:
        lines.append("#### Properties")
        lines.append("")
        lines.append(_render_properties_table(properties))
        lines.append("")

    # Special grouping for Tree and Locator
    if cls.name == "Tree" and methods:
        groups = _categorize_tree_methods(methods)
        for label, group_methods in groups.items():
            if group_methods:
                if label == "Action Shortcuts":
                    lines.append("#### Action Shortcuts")
                    lines.append("")
                    lines.append(
                        "These methods accept a selector string or a `Node` as *target*:"
                    )
                    lines.append("")
                    lines.append(
                        _render_methods_table(group_methods)
                    )
                else:
                    lines.append(
                        _render_methods_table(
                            group_methods, category_label=label
                        )
                    )
                lines.append("")
    elif cls.name == "Locator" and methods:
        groups = _categorize_locator_methods(methods)
        for label, group_methods in groups.items():
            if group_methods:
                lines.append(
                    _render_methods_table(group_methods, category_label=label)
                )
                lines.append("")
    elif cls.name == "Provider" and methods:
        lines.append("#### Methods")
        lines.append("")
        lines.append(_render_methods_table(methods))
        lines.append("")
    elif methods:
        lines.append("#### Methods")
        lines.append("")
        lines.append(_render_methods_table(methods))
        lines.append("")

    return "\n".join(lines)


def _rst_to_mdx(text: str) -> str:
    """Convert RST-isms to Markdown/MDX equivalents."""
    # Convert RST double-backtick literals ``foo`` → `foo`
    text = re.sub(r"``(.*?)``", r"`\1`", text)
    # Convert :class:`Foo` and :exc:`Foo` → `Foo`
    text = re.sub(r":(class|exc|func|meth|attr):`([^`]+)`", r"`\2`", text)
    # Remove trailing `::` that RST uses for literal blocks
    text = re.sub(r"::\s*$", ":", text, flags=re.MULTILINE)
    return text


def generate() -> str:
    source = STUB_PATH.read_text()
    tree = ast.parse(source, filename=str(STUB_PATH))
    _collect_type_aliases(tree)
    classes, functions = _extract_classes_and_functions(tree)

    # Group classes
    exceptions: list[ast.ClassDef] = []
    data_classes: list[ast.ClassDef] = []
    main_classes: list[ast.ClassDef] = []
    for cls in classes:
        kind = _classify_class(cls)
        if kind == "exception":
            exceptions.append(cls)
        elif kind == "data":
            data_classes.append(cls)
        else:
            main_classes.append(cls)

    parts: list[str] = [FRONTMATTER, ""]

    # Module functions
    if functions:
        parts.append("---")
        parts.append("")
        parts.append("## Module Functions")
        parts.append("")
        for func in functions:
            parts.append(_render_function_section(func))
            parts.append("")

    # Main classes (Provider, Tree, Node, Locator)
    class_order = ["Provider", "Tree", "Node", "Locator"]
    ordered_main = sorted(
        main_classes, key=lambda c: class_order.index(c.name) if c.name in class_order else 99
    )

    if ordered_main:
        parts.append("---")
        parts.append("")
        parts.append("## Classes")
        parts.append("")
        for cls in ordered_main:
            parts.append(_render_class(cls))
            parts.append("---")
            parts.append("")

    # Data classes
    if data_classes:
        parts.append("## Data Classes")
        parts.append("")
        for cls in data_classes:
            parts.append(_render_class(cls))
            parts.append("---")
            parts.append("")

    # Exceptions
    if exceptions:
        parts.append("## Exceptions")
        parts.append("")
        parts.append("All xa11y exceptions inherit from `xa11y.XA11yError`.")
        parts.append("")
        parts.append("| Exception | Description |")
        parts.append("| --------- | ----------- |")
        for exc in exceptions:
            doc = _first_line_docstring(exc)
            parts.append(f"| `{exc.name}` | {doc} |")
        parts.append("")

    output = "\n".join(parts)
    return _rst_to_mdx(output)


def main() -> None:
    mdx = generate()
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_PATH.write_text(mdx)
    print(f"Generated {OUTPUT_PATH.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
