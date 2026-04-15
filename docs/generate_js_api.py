#!/usr/bin/env python3
"""Generate the JavaScript API reference MDX from xa11y-js/index.d.ts.

Usage:
    python docs/generate_js_api.py

Reads:  xa11y-js/index.d.ts
Writes: docs/site/src/content/docs/api/javascript.mdx

The hand-written TypeScript declaration file is the single source of truth.
We parse it with a tiny line-based state machine — the file has a regular
structure (JSDoc blocks followed by `export class` / `export function` /
`export interface`) that doesn't need the full TypeScript compiler.
"""

from __future__ import annotations

import re
import textwrap
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DTS_PATH = REPO_ROOT / "xa11y-js" / "index.d.ts"
OUTPUT_PATH = (
    REPO_ROOT / "docs" / "site" / "src" / "content" / "docs" / "api" / "javascript.mdx"
)

# ── Parse ───────────────────────────────────────────────────────────────────


@dataclass
class Member:
    kind: str  # "method" | "getter" | "property" | "static"
    name: str
    signature: str
    doc: str = ""


@dataclass
class ClassDef:
    kind: str  # "class" | "interface"
    name: str
    doc: str = ""
    members: list[Member] = field(default_factory=list)


@dataclass
class FunctionDef:
    name: str
    signature: str
    doc: str = ""


def _strip_jsdoc(lines: list[str]) -> str:
    """Turn a JSDoc block into clean MDX-safe markdown text."""
    text = "\n".join(lines)
    # Strip leading `/**` / trailing `*/` / per-line `*`
    text = re.sub(r"^\s*/\*\*", "", text)
    text = re.sub(r"\*/\s*$", "", text)
    cleaned = []
    for line in text.splitlines():
        cleaned.append(re.sub(r"^\s*\*\s?", "", line))
    text = textwrap.dedent("\n".join(cleaned)).strip()
    # MDX parses `{...}` as a JS expression. Convert `{@link Foo.bar | label}`
    # and `{@link Foo}` JSDoc references to inline `code` so they round-trip
    # safely through the MDX renderer.
    text = re.sub(r"\{@link\s+([^}|]+)\|\s*([^}]+)\}", r"`\2`", text)
    text = re.sub(r"\{@link\s+([^}]+)\}", r"`\1`", text)
    return text


def parse(source: str) -> tuple[list[ClassDef], list[FunctionDef]]:
    classes: list[ClassDef] = []
    functions: list[FunctionDef] = []

    lines = source.splitlines()
    i = 0
    pending_doc = ""

    while i < len(lines):
        line = lines[i]
        stripped = line.strip()

        # JSDoc block
        if stripped.startswith("/**"):
            block = []
            while i < len(lines):
                block.append(lines[i])
                if "*/" in lines[i]:
                    break
                i += 1
            pending_doc = _strip_jsdoc(block)
            i += 1
            continue

        # export class / export declare class / export interface
        m = re.match(r"export\s+(?:declare\s+)?(class|interface)\s+(\w+)", stripped)
        if m:
            cls = ClassDef(kind=m.group(1), name=m.group(2), doc=pending_doc)
            pending_doc = ""
            # Find body
            depth = stripped.count("{") - stripped.count("}")
            i += 1
            member_doc = ""
            while i < len(lines) and depth > 0:
                mline = lines[i]
                mstrip = mline.strip()

                if mstrip.startswith("/**"):
                    block = []
                    while i < len(lines):
                        block.append(lines[i])
                        if "*/" in lines[i]:
                            break
                        i += 1
                    member_doc = _strip_jsdoc(block)
                    i += 1
                    continue

                # Track braces for nesting
                depth += mline.count("{") - mline.count("}")
                if depth <= 0:
                    break

                # Parse member line
                member = _parse_member(mstrip, member_doc)
                if member is not None:
                    cls.members.append(member)
                    member_doc = ""

                i += 1
            classes.append(cls)
            i += 1
            continue

        # export function
        m = re.match(r"export\s+(?:declare\s+)?function\s+(\w+)\s*(\(.*\).*?);?\s*$", stripped)
        if m:
            functions.append(
                FunctionDef(name=m.group(1), signature=m.group(2).rstrip(";").strip(), doc=pending_doc)
            )
            pending_doc = ""
            i += 1
            continue

        # Reset pending doc if we hit a non-declaration line
        if stripped and not stripped.startswith("//") and not stripped.startswith("*"):
            # Keep pending_doc if followed by blank line, reset otherwise
            pass
        i += 1

    return classes, functions


def _parse_member(line: str, doc: str) -> Member | None:
    line = line.rstrip(";").strip()
    if not line or line.startswith("//"):
        return None

    # Static method: `static name(args): Ret` or `static readonly x: T`
    m = re.match(r"(?:public\s+)?static\s+(?:readonly\s+)?(\w+)\s*(\(.*\).*)", line)
    if m:
        return Member(
            kind="static", name=m.group(1), signature=m.group(2).strip(), doc=doc
        )

    # Getter (from hand-written .d.ts the shape is `readonly name: T`)
    m = re.match(r"readonly\s+(\w+)\s*:\s*(.+)$", line)
    if m:
        return Member(kind="getter", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    # Method: `name(args): Ret`
    m = re.match(r"(\w+)\s*(\(.*\).*)", line)
    if m:
        return Member(
            kind="method", name=m.group(1), signature=m.group(2).strip(), doc=doc
        )

    # Plain property
    m = re.match(r"(\w+)\s*:\s*(.+)$", line)
    if m:
        return Member(kind="property", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    return None


# ── Render ─────────────────────────────────────────────────────────────────


def render_member(m: Member) -> list[str]:
    lines: list[str] = []
    if m.kind == "getter":
        lines.append(f"#### `{m.name}`")
        lines.append("")
        lines.append(f"*Type:* `{m.signature}`")
    elif m.kind == "static":
        lines.append(f"#### `static {m.name}{m.signature}`")
    elif m.kind == "method":
        lines.append(f"#### `{m.name}{m.signature}`")
    else:
        lines.append(f"#### `{m.name}`")
        lines.append("")
        lines.append(f"*Type:* `{m.signature}`")
    if m.doc:
        lines.append("")
        lines.append(m.doc)
    lines.append("")
    return lines


def render_class(cls: ClassDef) -> list[str]:
    lines: list[str] = [f"### `{cls.name}`", ""]
    if cls.doc:
        lines.append(cls.doc)
        lines.append("")

    statics = [m for m in cls.members if m.kind == "static"]
    getters = [m for m in cls.members if m.kind in ("getter", "property")]
    methods = [m for m in cls.members if m.kind == "method"]

    if statics:
        lines.append("#### Static methods")
        lines.append("")
        for m in statics:
            lines.extend(render_member(m))
    if getters:
        lines.append("#### Properties")
        lines.append("")
        for m in getters:
            lines.extend(render_member(m))
    if methods:
        lines.append("#### Methods")
        lines.append("")
        for m in methods:
            lines.extend(render_member(m))

    return lines


def render_function(fn: FunctionDef) -> list[str]:
    lines: list[str] = [f"### `{fn.name}{fn.signature}`", ""]
    if fn.doc:
        lines.append(fn.doc)
        lines.append("")
    return lines


def render(classes: list[ClassDef], functions: list[FunctionDef]) -> str:
    out: list[str] = [
        "---",
        'title: "JavaScript API Reference"',
        'description: "API reference for the xa11y Node.js bindings (@xa11y/xa11y)."',
        "---",
        "",
        "{/* This page is auto-generated from xa11y-js/index.d.ts. */}",
        "{/* Do not edit by hand — run `cargo xtask docs` to regenerate. */}",
        "",
        "## Overview",
        "",
        "The `@xa11y/xa11y` package provides cross-platform accessibility queries",
        "and actions for Node.js. All methods that touch the accessibility tree",
        "are asynchronous — they run on the napi tokio worker pool so the Node",
        "event loop stays responsive.",
        "",
        "```js",
        "import { App, locator } from '@xa11y/xa11y';",
        "",
        "const app = await App.byName('Safari');",
        "await app.locator('button[name=\"OK\"]').press();",
        "```",
        "",
        "## Errors",
        "",
    ]

    error_classes = [c for c in classes if c.name.endswith("Error")]
    other_classes = [c for c in classes if not c.name.endswith("Error")]

    for c in error_classes:
        out.extend(render_class(c))

    out.extend(["## Classes", ""])
    for c in other_classes:
        out.extend(render_class(c))

    if functions:
        out.extend(["## Functions", ""])
        for f in functions:
            out.extend(render_function(f))

    return "\n".join(out) + "\n"


def main() -> None:
    source = DTS_PATH.read_text()
    classes, functions = parse(source)
    content = render(classes, functions)
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_PATH.write_text(content)
    print(f"Wrote {OUTPUT_PATH.relative_to(REPO_ROOT)}")
    print(f"  {len(classes)} classes, {len(functions)} functions")


if __name__ == "__main__":
    main()
