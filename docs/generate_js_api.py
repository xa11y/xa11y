#!/usr/bin/env python3
"""Generate the JavaScript API reference MDX for xa11y.

Usage:
    python docs/generate_js_api.py

Reads:
  - xa11y-js/native.d.ts  (auto-generated from Rust by napi-rs, then
    post-processed by xa11y-js/scripts/patch-native-dts.mjs)
  - xa11y-js/index.d.ts   (hand-written; adds the error classes, the
    EventEmitter-based Subscription, and App augmentations)

Writes:
  docs/site/src/content/docs/api/javascript.mdx

The split means:
  * every class / method / type that mirrors the Rust API comes from
    native.d.ts (one source of truth -- the Rust source itself)
  * only JS-only symbols (error classes, Subscription, App interface
    augmentation) are read from index.d.ts

Before calling this script, make sure `xa11y-js/native.d.ts` exists and is
up to date:

    cd xa11y-js && npx napi build --platform --js native.js --dts native.d.ts \
        && node scripts/patch-native-dts.mjs
"""

from __future__ import annotations

import re
import sys
import textwrap
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
NATIVE_DTS = REPO_ROOT / "xa11y-js" / "native.d.ts"
INDEX_DTS = REPO_ROOT / "xa11y-js" / "index.d.ts"
OUTPUT_PATH = (
    REPO_ROOT / "docs" / "site" / "src" / "content" / "docs" / "api" / "javascript.mdx"
)

# Skip these when walking index.d.ts -- everything else in index.d.ts is
# either an error class or the EventEmitter-based Subscription, which we want.
SKIP_FROM_INDEX: set[str] = set()


# ── Data model ──────────────────────────────────────────────────────────────


@dataclass
class Member:
    kind: str  # "method" | "getter" | "static" | "property"
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


@dataclass
class TypeAlias:
    name: str
    definition: str
    doc: str = ""


# ── Parsing ─────────────────────────────────────────────────────────────────


def _strip_jsdoc(lines: list[str]) -> str:
    """Turn a JSDoc block into clean MDX-safe markdown text."""
    text = "\n".join(lines)
    text = re.sub(r"^\s*/\*\*", "", text)
    text = re.sub(r"\*/\s*$", "", text)
    cleaned = []
    for line in text.splitlines():
        cleaned.append(re.sub(r"^\s*\*\s?", "", line))
    text = textwrap.dedent("\n".join(cleaned)).strip()
    # MDX parses `{...}` as a JS expression, so convert JSDoc `{@link}`
    # references to inline code.
    text = re.sub(r"\{@link\s+([^}|]+)\|\s*([^}]+)\}", r"`\2`", text)
    text = re.sub(r"\{@link\s+([^}]+)\}", r"`\1`", text)
    return text


def _parse_member(line: str, doc: str) -> Member | None:
    line = line.rstrip(";").strip()
    if not line or line.startswith("//"):
        return None

    m = re.match(r"(?:public\s+)?static\s+(?:readonly\s+)?(\w+)\s*(\(.*\).*)", line)
    if m:
        return Member(kind="static", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    m = re.match(r"get\s+(\w+)\s*\(\s*\)\s*:\s*(.+)$", line)
    if m:
        return Member(kind="getter", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    m = re.match(r"readonly\s+(\w+)\s*:\s*(.+)$", line)
    if m:
        return Member(kind="getter", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    m = re.match(r"(\[Symbol\.\w+\])\s*(\(.*\).*)", line)
    if m:
        return Member(kind="method", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    m = re.match(r"(\w+)\s*(\(.*\).*)", line)
    if m:
        return Member(kind="method", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    m = re.match(r"(\w+)\s*:\s*(.+)$", line)
    if m:
        return Member(kind="property", name=m.group(1), signature=m.group(2).strip(), doc=doc)

    return None


def parse_dts(source: str) -> tuple[list[ClassDef], list[FunctionDef], list[TypeAlias]]:
    classes: list[ClassDef] = []
    functions: list[FunctionDef] = []
    aliases: list[TypeAlias] = []

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

        # Type alias `export type Name = ...` (possibly multi-line)
        m = re.match(r"export\s+type\s+(\w+)\s*=\s*(.*)$", stripped)
        if m:
            name = m.group(1)
            definition = m.group(2).strip()
            # Continue until we hit a `;`
            while not definition.rstrip().endswith(";"):
                i += 1
                if i >= len(lines):
                    break
                definition += "\n" + lines[i].rstrip()
            definition = definition.rstrip(";").strip()
            aliases.append(TypeAlias(name=name, definition=definition, doc=pending_doc))
            pending_doc = ""
            i += 1
            continue

        # Class / interface
        m = re.match(r"export\s+(?:declare\s+)?(class|interface)\s+(\w+)", stripped)
        if m:
            cls = ClassDef(kind=m.group(1), name=m.group(2), doc=pending_doc)
            pending_doc = ""
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

                depth += mline.count("{") - mline.count("}")
                if depth <= 0:
                    break

                member = _parse_member(mstrip, member_doc)
                if member is not None:
                    cls.members.append(member)
                    member_doc = ""

                i += 1
            classes.append(cls)
            i += 1
            continue

        # Function
        m = re.match(
            r"export\s+(?:declare\s+)?function\s+(\w+)\s*(\(.*\).*?);?\s*$", stripped
        )
        if m:
            functions.append(
                FunctionDef(
                    name=m.group(1),
                    signature=m.group(2).rstrip(";").strip(),
                    doc=pending_doc,
                )
            )
            pending_doc = ""
            i += 1
            continue

        i += 1

    return classes, functions, aliases


# ── Rendering ──────────────────────────────────────────────────────────────


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


def render_alias(alias: TypeAlias) -> list[str]:
    lines = [f"### `type {alias.name}`", ""]
    if alias.doc:
        lines.append(alias.doc)
        lines.append("")
    lines.append("```ts")
    lines.append(f"type {alias.name} = {alias.definition};")
    lines.append("```")
    lines.append("")
    return lines


# ── Main ───────────────────────────────────────────────────────────────────


def main() -> None:
    if not NATIVE_DTS.exists():
        sys.exit(
            f"error: {NATIVE_DTS.relative_to(REPO_ROOT)} not found.\n"
            "Run `cd xa11y-js && npx napi build --platform --js native.js "
            "--dts native.d.ts && node scripts/patch-native-dts.mjs` first."
        )

    native_classes, native_fns, native_aliases = parse_dts(NATIVE_DTS.read_text())
    index_classes, _, _ = parse_dts(INDEX_DTS.read_text())

    # Keep the error hierarchy from index.d.ts (they don't exist on the Rust side)
    error_classes = [
        c for c in index_classes if c.name.endswith("Error") and c.name not in SKIP_FROM_INDEX
    ]

    # Filter out private symbols
    native_classes = [c for c in native_classes if not c.name.startswith("_")]
    native_fns = [f for f in native_fns if not f.name.startswith("_")]

    out: list[str] = [
        "---",
        'title: "JavaScript API Reference"',
        'description: "API reference for the xa11y Node.js bindings (@crowecawcaw/xa11y)."',
        "---",
        "",
        "{/* This page is auto-generated. Do not edit by hand. */}",
        "{/* Source: xa11y-js/native.d.ts (Rust → napi-rs → patch-native-dts.mjs) */}",
        "{/* Regenerate with: python docs/generate_js_api.py */}",
        "",
        "## Overview",
        "",
        "The `@crowecawcaw/xa11y` package provides cross-platform accessibility queries",
        "and actions for Node.js. All methods that touch the accessibility tree",
        "are asynchronous — they run on the napi tokio worker pool so the Node",
        "event loop stays responsive.",
        "",
        "```js",
        "import { App } from '@crowecawcaw/xa11y';",
        "",
        "const app = await App.byName('Safari');",
        "await app.locator('button[name=\"OK\"]').press();",
        "```",
        "",
    ]

    if native_aliases:
        out.extend(["## Types", ""])
        for a in native_aliases:
            out.extend(render_alias(a))

    out.extend(["## Errors", ""])
    out.append(
        "All operations throw subclasses of `XA11yError`. Catch a specific "
        "subclass with `instanceof` and let the rest propagate."
    )
    out.append("")
    for c in error_classes:
        out.extend(render_class(c))

    out.extend(["## Classes", ""])
    for c in native_classes:
        out.extend(render_class(c))

    if native_fns:
        out.extend(["## Functions", ""])
        for f in native_fns:
            out.extend(render_function(f))

    content = "\n".join(out) + "\n"
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_PATH.write_text(content)
    print(f"Wrote {OUTPUT_PATH.relative_to(REPO_ROOT)}")
    print(
        f"  {len(native_classes)} classes, {len(native_fns)} functions, "
        f"{len(native_aliases)} type aliases, {len(error_classes)} error classes"
    )


if __name__ == "__main__":
    main()
