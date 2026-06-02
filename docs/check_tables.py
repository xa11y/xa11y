#!/usr/bin/env python3
"""Check that GitHub-Flavored-Markdown tables in the docs are well-formed.

Scans all .mdx files under docs/site/src/content/docs/ for tables and
validates each one against the rules Starlight's GFM renderer relies on:

  - A blank line must precede the table, otherwise the header row is
    folded into the preceding paragraph and the table never renders.
  - The header row, the delimiter row, and every body row must all have
    the same number of cells. A row with too few cells silently drops a
    column (this is the bug that motivated the check); a row with too
    many cells spills into an extra column.

A "table" is detected as a header row starting with `|` immediately
followed by a delimiter row (pipes, dashes, optional colons). Pipes
inside inline code spans or escaped as `\\|` are not treated as cell
separators. Fenced code blocks (```) are skipped so example tables in
documentation aren't validated as real ones.

Exit code 0 if all tables are well-formed, 1 if any problems are found.
"""

import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).parent / "site" / "src" / "content" / "docs"

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


def main() -> int:
    mdx_files = sorted(DOCS_DIR.rglob("*.mdx"))
    if not mdx_files:
        print(f"ERROR: no .mdx files found in {DOCS_DIR}", file=sys.stderr)
        return 1

    all_errors: list[tuple[Path, int, str]] = []
    for filepath in mdx_files:
        for lineno, reason in check_file(filepath):
            all_errors.append((filepath, lineno, reason))

    if all_errors:
        print(f"Found {len(all_errors)} malformed table issue(s):\n")
        for filepath, lineno, reason in all_errors:
            rel = filepath.relative_to(DOCS_DIR)
            print(f"  {rel}:{lineno}: {reason}")
        print()
        return 1

    print(f"All tables OK ({len(mdx_files)} files checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
