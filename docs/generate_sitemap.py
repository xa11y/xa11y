#!/usr/bin/env python3
"""Generate sitemap.xml by scanning the fully-built docs site.

Runs after the docs build pipeline has produced docs/site/dist/ containing:
  - Astro/Starlight HTML output (from `npm run build`)
  - Python API reference at api/python/reference/ (from `sphinx-build`)

Walks dist/ for every relevant HTML file, filters out generated navigation
and source-view pages, and writes dist/sitemap.xml. Because URLs come from
the actual built output, adding a new MDX page or a new Python class picks
up automatically on the next build — nothing is hardcoded.

Rust API reference is hosted on docs.rs and intentionally excluded from
this sitemap; docs.rs publishes its own sitemap.
"""

from __future__ import annotations

import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SITE_BASE = "https://xa11y.dev"
REPO_ROOT = Path(__file__).resolve().parents[1]
DIST = REPO_ROOT / "docs" / "site" / "dist"
CONTENT_ROOT = REPO_ROOT / "docs" / "site" / "src" / "content" / "docs"
LANDING_SOURCE = REPO_ROOT / "docs" / "site" / "src" / "pages" / "index.astro"

# Directory prefixes (relative to dist) whose HTML files should not be
# advertised to search engines.
EXCLUDE_DIR_PREFIXES = (
    "api/python/reference/_static/",
    "api/python/reference/_sources/",
)

# Filenames that are navigation / build-tooling, not real content.
EXCLUDE_FILENAMES = {
    "search.html",         # sphinx search UI
    "genindex.html",       # sphinx generated index
    "py-modindex.html",    # sphinx module index
    "404.html",
}


def url_for(path: Path) -> str:
    rel = path.relative_to(DIST).as_posix()
    if rel == "index.html":
        return f"{SITE_BASE}/"
    if rel.endswith("/index.html"):
        return f"{SITE_BASE}/{rel[: -len('index.html')]}"
    return f"{SITE_BASE}/{rel}"


def should_include(path: Path) -> bool:
    rel = path.relative_to(DIST).as_posix()
    if any(rel.startswith(p) for p in EXCLUDE_DIR_PREFIXES):
        return False
    if path.name in EXCLUDE_FILENAMES:
        return False
    return True


def source_for(dist_path: Path) -> Path | None:
    """Map a built HTML file back to its hand-written source, if any.

    Returns the source path for the custom landing and Starlight MDX/MD pages.
    Returns None for generated API reference pages (cargo doc, Sphinx) where
    no clean per-page source mapping exists — those pages omit lastmod rather
    than emit a misleading value.
    """
    rel = dist_path.relative_to(DIST).as_posix()
    if rel == "index.html":
        return LANDING_SOURCE if LANDING_SOURCE.exists() else None
    if rel.startswith("api/"):
        return None
    if rel.endswith("/index.html"):
        slug = rel[: -len("/index.html")]
        for ext in (".mdx", ".md"):
            candidate = CONTENT_ROOT / f"{slug}{ext}"
            if candidate.exists():
                return candidate
    return None


def git_mtime(source: Path) -> str | None:
    """Return ISO date (YYYY-MM-DD) of the last commit touching `source`."""
    try:
        result = subprocess.run(
            ["git", "log", "-1", "--format=%cs", "--", str(source)],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    out = result.stdout.strip()
    return out or None


def main() -> int:
    if not DIST.exists():
        print(
            f"error: {DIST} does not exist; run the docs build first",
            file=sys.stderr,
        )
        return 1

    seen: dict[str, Path] = {}
    for p in DIST.rglob("*.html"):
        if not should_include(p):
            continue
        u = url_for(p)
        seen.setdefault(u, p)
    pages = sorted(seen.items())

    ns = "http://www.sitemaps.org/schemas/sitemap/0.9"
    ET.register_namespace("", ns)
    root = ET.Element(f"{{{ns}}}urlset")
    dated = 0
    for u, p in pages:
        el = ET.SubElement(root, f"{{{ns}}}url")
        ET.SubElement(el, f"{{{ns}}}loc").text = u
        src = source_for(p)
        mtime = git_mtime(src) if src else None
        if mtime:
            ET.SubElement(el, f"{{{ns}}}lastmod").text = mtime
            dated += 1

    tree = ET.ElementTree(root)
    ET.indent(tree, space="  ")
    out = DIST / "sitemap.xml"
    tree.write(out, encoding="utf-8", xml_declaration=True)
    print(f"wrote {len(pages)} URLs to {out} ({dated} with lastmod)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
