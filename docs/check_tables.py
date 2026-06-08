#!/usr/bin/env python3
"""Check that GitHub-Flavored-Markdown tables in the docs are well-formed
*and* that they actually render to <table> elements in the built site.

Two layers of checking, because table bugs come in two flavours:

1. **Source well-formedness** (default). Scans all .mdx files under
   docs/site/src/content/docs/ and validates each table against the
   rules Starlight's GFM renderer relies on:

     - A blank line must precede the table, otherwise the header row is
       folded into the preceding paragraph and the table never renders.
     - The header row, the delimiter row, and every body row must all
       have the same number of cells. A row with too few cells silently
       drops a column (this is the bug that motivated the check); a row
       with too many cells spills into an extra column.

   A "table" is detected as a header row starting with `|` immediately
   followed by a delimiter row (pipes, dashes, optional colons). Pipes
   inside inline code spans or escaped as `\\|` are not treated as cell
   separators. Fenced code blocks (```) are skipped so example tables in
   documentation aren't validated as real ones.

2. **Rendered output** (`--rendered <dist>`). Even a perfectly-formed
   source table renders as a raw paragraph if the GFM remark plugin is
   not wired into the MDX pipeline — exactly the regression in issue
   #247, where an Astro upgrade stopped `@astrojs/mdx` from enabling
   `remark-gfm`. Static source checks can't see that, so this mode maps
   every .mdx source to its built HTML page and asserts the number of
   `<table>` elements matches the number of source tables. It catches
   config/toolchain regressions that drop *all* tables at once.

Exit code 0 if everything checks out, 1 if any problems are found.
"""

import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).parent / "site" / "src" / "content" / "docs"

# Matches the opening tag of a rendered HTML table, e.g. `<table>` or
# `<table class="...">`. Used by the rendered-output check.
RENDERED_TABLE = re.compile(r"<table[\s/>]")

# A delimiter row: only pipes, dashes, colons and whitespace, with at
# least one dash and at least one pipe.
DELIMITER_ROW = re.compile(r"^\s*\|?[\s:|-]*-[\s:|-]*\|[\s:|-]*$")


def split_cells(line: str) -> list[str]:
    """Split a table row into cells, honouring code spans and escapes.

    Pipes inside `inline code` or written as `\\|` do not separate cells.
    Leading and trailing table-edge pipes are stripped first.
    """
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|") and not s.endswith("\\|"):
        s = s[:-1]

    cells: list[str] = []
    buf: list[str] = []
    in_code = False
    i = 0
    while i < len(s):
        ch = s[i]
        if ch == "\\" and i + 1 < len(s):
            buf.append(s[i : i + 2])
            i += 2
            continue
        if ch == "`":
            in_code = not in_code
            buf.append(ch)
        elif ch == "|" and not in_code:
            cells.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
        i += 1
    cells.append("".join(buf))
    return cells


def iter_tables(lines: list[str]):
    """Yield the 0-based header line index of every table in `lines`.

    A table is a header row starting with `|` immediately followed by a
    delimiter row. Fenced code blocks are skipped so example tables in
    documentation aren't counted. Shared by the source and rendered
    checks so both agree on what "a table" is.
    """
    n = len(lines)
    in_fence = False
    i = 0
    while i < n:
        line = lines[i]
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            i += 1
            continue
        is_header = (
            not in_fence
            and line.lstrip().startswith("|")
            and i + 1 < n
            and DELIMITER_ROW.match(lines[i + 1])
        )
        if not is_header:
            i += 1
            continue
        yield i
        # Skip past the delimiter row and the contiguous body rows.
        j = i + 2
        while (
            j < n
            and lines[j].lstrip().startswith("|")
            and not lines[j].lstrip().startswith("```")
        ):
            j += 1
        i = j


def count_tables(filepath: Path) -> int:
    """Number of real (non-fenced) tables in an .mdx source file."""
    return sum(1 for _ in iter_tables(filepath.read_text().splitlines()))


def rendered_html_path(filepath: Path, dist: Path) -> Path:
    """Map an .mdx source path to its built HTML page under `dist`.

    Astro/Starlight emits `<slug>/index.html` for each doc, where the
    slug is the source path relative to the content root without its
    extension. `index.mdx` collapses onto its parent directory.
    """
    slug = filepath.relative_to(DOCS_DIR).with_suffix("")
    if slug.name == "index":
        slug = slug.parent
    return dist / slug / "index.html"


def check_rendered(filepath: Path, dist: Path) -> list[str]:
    """Return reasons the built page doesn't match the source's tables."""
    n_source = count_tables(filepath)
    if n_source == 0:
        return []

    html_path = rendered_html_path(filepath, dist)
    if not html_path.exists():
        return [
            f"has {n_source} table(s) but built page is missing: "
            f"{html_path.relative_to(dist)}"
        ]

    n_rendered = len(RENDERED_TABLE.findall(html_path.read_text()))
    if n_rendered != n_source:
        return [
            f"has {n_source} source table(s) but built page renders "
            f"{n_rendered} <table> element(s) — GFM tables may not be "
            f"reaching the HTML (see issue #247)"
        ]
    return []


def check_file(filepath: Path) -> list[tuple[int, str]]:
    """Return a list of (line_number, reason) for malformed tables."""
    errors: list[tuple[int, str]] = []
    lines = filepath.read_text().splitlines()
    n = len(lines)
    in_fence = False
    i = 0
    while i < n:
        line = lines[i]
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            i += 1
            continue
        is_header = (
            not in_fence
            and line.lstrip().startswith("|")
            and i + 1 < n
            and DELIMITER_ROW.match(lines[i + 1])
        )
        if not is_header:
            i += 1
            continue

        ncols = len(split_cells(line))
        if i == 0 or lines[i - 1].strip() != "":
            errors.append((i + 1, "no blank line before table"))

        sep_cols = len(split_cells(lines[i + 1]))
        if sep_cols != ncols:
            errors.append(
                (i + 2, f"delimiter row has {sep_cols} cells, header has {ncols}")
            )

        j = i + 2
        while (
            j < n
            and lines[j].lstrip().startswith("|")
            and not lines[j].lstrip().startswith("```")
        ):
            row_cols = len(split_cells(lines[j]))
            if row_cols != ncols:
                errors.append((j + 1, f"row has {row_cols} cells, header has {ncols}"))
            j += 1
        i = j
    return errors


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv

    dist: Path | None = None
    if "--rendered" in argv:
        idx = argv.index("--rendered")
        try:
            dist = Path(argv[idx + 1])
        except IndexError:
            print("ERROR: --rendered requires a path to the built dist/ dir")
            return 1
        if not dist.is_dir():
            print(f"ERROR: dist directory not found: {dist}", file=sys.stderr)
            return 1

    mdx_files = sorted(DOCS_DIR.rglob("*.mdx"))
    if not mdx_files:
        print(f"ERROR: no .mdx files found in {DOCS_DIR}", file=sys.stderr)
        return 1

    all_errors: list[tuple[Path, str]] = []
    for filepath in mdx_files:
        for lineno, reason in check_file(filepath):
            all_errors.append((filepath, f"{lineno}: {reason}"))

    if dist is not None:
        for filepath in mdx_files:
            for reason in check_rendered(filepath, dist):
                all_errors.append((filepath, reason))

    if all_errors:
        print(f"Found {len(all_errors)} table issue(s):\n")
        for filepath, reason in all_errors:
            rel = filepath.relative_to(DOCS_DIR)
            print(f"  {rel}:{reason}")
        print()
        return 1

    where = "source + rendered" if dist is not None else "source"
    print(f"All tables OK ({len(mdx_files)} files checked, {where})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
