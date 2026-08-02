#!/usr/bin/env python3
"""Check internal links in the documentation site.

Scans two sources for internal links (markdown links and hrefs starting
with /) and validates that each resolves to either:
  - An existing content page (.mdx file)
  - A known build-time asset path (e.g. /api/python/reference/...)

The two sources are:

1. All .mdx files under docs/site/src/content/docs/.
2. The standalone Astro routes under docs/site/src/pages/. These are easy
   to forget: index.astro overrides the Starlight-rendered index.mdx at `/`,
   so it is the page every visitor actually lands on, and its links went
   stale through a docs reorganisation precisely because this checker only
   looked at .mdx.

Exit code 0 if all links are valid, 1 if any are broken.
"""

import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).parent / "site" / "src" / "content" / "docs"
PAGES_DIR = Path(__file__).parent / "site" / "src" / "pages"

# Everything under /api/ is generated: `generate_python_api.py` and
# `generate_js_api.py` write .mdx pages there, and Sphinx writes the
# /api/python/reference/ tree as raw HTML. None of it exists on a fresh
# checkout, so accept the prefix rather than making this check depend on
# whether the generators have run yet.
ASSET_PATH_PREFIXES = [
    "/api/python/",
    "/api/javascript/",
]

# Site-root paths that are neither content pages nor generated API assets:
# static files in `public/`, and the bare site root.
STATIC_PATHS = {"/"}

# Regex for markdown links: [text](/path/) and HTML href="/path/"
MARKDOWN_LINK = re.compile(r"\]\((/[^)]+)\)")
HTML_HREF = re.compile(r'href="(/[^"]+)"')


def slug_to_file(slug: str) -> Path | None:
    """Resolve a Starlight slug like /explanation/how-it-works/ to its source.

    Starlight renders both .mdx and .md, so try each. Returns None when
    neither exists.
    """
    slug = slug.strip("/") or "index"
    for suffix in (".mdx", ".md"):
        candidate = DOCS_DIR / f"{slug}{suffix}"
        if candidate.exists():
            return candidate
    return None


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

    # Resolve against the page path; in-page fragments aren't validated
    link = link.split("#", 1)[0]

    # Allow known asset paths
    for prefix in ASSET_PATH_PREFIXES:
        if link.startswith(prefix):
            return None

    if link in STATIC_PATHS:
        return None

    # Files served verbatim out of `public/` (hero.svg, .well-known/, ...).
    public = PAGES_DIR.parent.parent / "public" / link.lstrip("/")
    if public.exists():
        return None

    # Must resolve to an existing content page
    if slug_to_file(link) is None:
        stem = link.strip("/") or "index"
        return f"no content page at {stem}.mdx (or .md)"
    return None


def main() -> int:
    mdx_files = sorted(DOCS_DIR.rglob("*.mdx"))
    if not mdx_files:
        print(f"ERROR: no .mdx files found in {DOCS_DIR}", file=sys.stderr)
        return 1

    astro_files = sorted(PAGES_DIR.rglob("*.astro")) if PAGES_DIR.exists() else []
    files = mdx_files + astro_files

    all_errors: list[tuple[Path, int, str, str]] = []
    for filepath in files:
        for lineno, link, reason in check_file(filepath):
            all_errors.append((filepath, lineno, link, reason))

    if all_errors:
        print(f"Found {len(all_errors)} broken link(s):\n")
        for filepath, lineno, link, reason in all_errors:
            rel = filepath.relative_to(Path(__file__).parent / "site" / "src")
            print(f"  {rel}:{lineno}: {link}")
            print(f"    -> {reason}")
        print()
        return 1

    print(
        f"All links OK ({len(mdx_files)} content page(s), "
        f"{len(astro_files)} Astro route(s))"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
