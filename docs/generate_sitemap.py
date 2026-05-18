#!/usr/bin/env python3
"""Generate sitemap.xml by scanning the fully-built docs site.

Runs after the docs build pipeline has produced docs/site/dist/ containing:
  - Astro/Starlight HTML output (from `npm run build`)
  - Rust API reference at api/rust/reference/ (from `cargo doc`)
  - Python API reference at api/python/reference/ (from `sphinx-build`)

Walks dist/ for every relevant HTML file, filters out generated navigation
and source-view pages, and writes dist/sitemap.xml. Because URLs come from
the actual built output, adding a new MDX page, a new Rust item, or a new
Python class picks up automatically on the next build — nothing is
hardcoded.
"""

from __future__ import annotations

import datetime
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SITE_BASE = "https://xa11y.dev"
DIST = Path(__file__).resolve().parent / "site" / "dist"

# Directory prefixes (relative to dist) whose HTML files should not be
# advertised to search engines.
EXCLUDE_DIR_PREFIXES = (
    "api/rust/reference/src/",          # cargo doc source view
    "api/rust/reference/implementors/", # cargo doc trait-impl shims
    "api/python/reference/_static/",
    "api/python/reference/_sources/",
)

# Filenames that are navigation / build-tooling, not real content.
EXCLUDE_FILENAMES = {
    "all.html",            # cargo doc "all items"
    "help.html",           # cargo doc help overlay
    "settings.html",       # cargo doc settings
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


def main() -> int:
    if not DIST.exists():
        print(
            f"error: {DIST} does not exist; run the docs build first",
            file=sys.stderr,
        )
        return 1

    today = datetime.date.today().isoformat()
    urls = sorted(
        {url_for(p) for p in DIST.rglob("*.html") if should_include(p)}
    )

    ns = "http://www.sitemaps.org/schemas/sitemap/0.9"
    ET.register_namespace("", ns)
    root = ET.Element(f"{{{ns}}}urlset")
    for u in urls:
        el = ET.SubElement(root, f"{{{ns}}}url")
        ET.SubElement(el, f"{{{ns}}}loc").text = u
        ET.SubElement(el, f"{{{ns}}}lastmod").text = today

    tree = ET.ElementTree(root)
    ET.indent(tree, space="  ")
    out = DIST / "sitemap.xml"
    tree.write(out, encoding="utf-8", xml_declaration=True)
    print(f"wrote {len(urls)} URLs to {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
