#!/usr/bin/env python3
"""Check internal links in documentation .mdx files.

Scans all .mdx files under docs/site/src/content/docs/ for internal links
(markdown links starting with /) and validates that they resolve to either:
  - An existing content page (.mdx file)
  - A known build-time asset path (e.g. /api/python/reference/...)

Exit code 0 if all links are valid, 1 if any are broken.
"""

import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).parent / "site" / "src" / "content" / "docs"

# Paths served by build-time assets (sphinx), not content pages.
ASSET_PATH_PREFIXES = [
    "/api/python/reference/",
]

# Regex for markdown links: [text](/path/) and HTML href="/path/"
MARKDOWN_LINK = re.compile(r'\]\((/[^)]+)\)')
HTML_HREF = re.compile(r'href="(/[^"]+)"')


def slug_to_file(slug: str) -> Path:
    """Convert a Starlight content slug like /guides/overview/ to a file path."""
    slug = slug.strip("/")
    return DOCS_DIR / f"{slug}.mdx"


def check_file(filepath: Path) -> list[tuple[int, str, str]]:
    """Return list of (line_number, link, reason) for broken links in a file."""
    errors = []
    text = filepath.read_text()
    for i, line in enumerate(text.splitlines(), start=1):
        for match in MARKDOWN_LINK.finditer(line):
            link = match.group(1)
            err = validate_link(link)
            if err:
                errors.append((i, link, err))
        for match in HTML_HREF.finditer(line):
            link = match.group(1)
            err = validate_link(link)
            if err:
                errors.append((i, link, err))
    return errors


def validate_link(link: str) -> str | None:
    """Return an error message if the link is broken, or None if valid."""
    # Allow anchor-only links
    if link.startswith("#"):
        return None

    # Allow known asset paths
    for prefix in ASSET_PATH_PREFIXES:
        if link.startswith(prefix):
            return None

    # Must resolve to an existing content page
    target = slug_to_file(link)
    if not target.exists():
        return f"no content page at {target.relative_to(DOCS_DIR)}"
    return None


def main() -> int:
    mdx_files = sorted(DOCS_DIR.rglob("*.mdx"))
    if not mdx_files:
        print(f"ERROR: no .mdx files found in {DOCS_DIR}", file=sys.stderr)
        return 1

    all_errors: list[tuple[Path, int, str, str]] = []
    for filepath in mdx_files:
        for lineno, link, reason in check_file(filepath):
            all_errors.append((filepath, lineno, link, reason))

    if all_errors:
        print(f"Found {len(all_errors)} broken link(s):\n")
        for filepath, lineno, link, reason in all_errors:
            rel = filepath.relative_to(DOCS_DIR)
            print(f"  {rel}:{lineno}: {link}")
            print(f"    -> {reason}")
        print()
        return 1

    print(f"All links OK ({len(mdx_files)} files checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
