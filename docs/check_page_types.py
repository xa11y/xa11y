#!/usr/bin/env python3
"""Check that every hand-written docs page declares its Diátaxis page type.

The docs site follows Diátaxis (https://diataxis.fr): every page is a
tutorial, a how-to guide, reference, or explanation, and mixing modes on one
page is the failure this check exists to prevent. See `docs/PAGE_TYPES.md`
for the contract this enforces.

Each `.mdx` page under docs/site/src/content/docs/ must carry:

  1. A `pageType` key in its frontmatter, one of the types in PAGE_TYPES.
  2. A `{/* DIATAXIS: <type> — <canonical text> */}` banner comment
     immediately after the frontmatter, whose type matches the frontmatter
     and whose text matches this file's canonical wording for that type.
  3. A location matching its type: a `reference` page lives under
     `reference/`, a `how-to` under `guides/`, and so on.

Rule 1 is what tooling reads and is enforced a second time by the Astro
content schema (docs/site/src/content.config.ts), so the site build fails on
a missing key too. Rule 2 is what a human or an agent editing the file sees,
which is the moment the constraint needs to be in front of them; pinning the
wording is what stops the banner and the frontmatter from drifting apart.
Rule 3 keeps the sidebar architecture and the on-disk layout in agreement.

Generated API pages under `api/` are excluded: they are built by
generate_python_api.py / generate_js_api.py, not hand-written.

Exit code 0 if every page checks out, 1 if any problems are found.
"""

import re
import sys
from pathlib import Path

DOCS_DIR = Path(__file__).parent / "site" / "src" / "content" / "docs"

# Generated rather than hand-written, as a path prefix relative to DOCS_DIR.
# A prefix (rather than "any component named api") so a hand-written page at
# `guides/api/…` would still be checked.
EXCLUDED_PREFIXES = ("api/",)

# Starlight renders both; checking only .mdx would let a .md page skip the
# pageType and directory rules. The banner is exempt for .md, because
# `{/* … */}` is MDX comment syntax and a plain .md page would render it as
# visible body text.
PAGE_SUFFIXES = ("*.mdx", "*.md")

# The canonical banner text for each page type. The banner is compared after
# whitespace normalisation, so the wrapping in a page's source is free but
# the words are not.
CANONICAL_TEXT = {
    "tutorial": (
        "a guided lesson the reader follows start to finish and is guaranteed "
        "to finish successfully. Keep momentum: no options, no alternatives, "
        "no exhaustive lists. Those belong in guides/, reference/, "
        "explanation/."
    ),
    "how-to": (
        "a goal-oriented recipe for a reader who already knows what they want. "
        "Steps and decisions only. Concepts go to explanation/, exhaustive "
        "option lists go to reference/."
    ),
    "reference": (
        "a factual description of the machinery, structured for lookup rather "
        "than for reading through. Neutral and complete. No task narratives, "
        "no rationale, no teaching."
    ),
    "explanation": (
        "background and rationale: why the design is what it is, and how the "
        "pieces relate. No step-by-step instructions, no exhaustive tables."
    ),
    "evaluation": (
        "pre-adoption material for someone deciding whether to use xa11y. "
        "Deliberately outside the four Diátaxis modes; see docs/PAGE_TYPES.md "
        "before adding another one."
    ),
    "landing": (
        "the site entry point. Navigational only: every claim here is a "
        "summary of a page it links to."
    ),
}

# Kept in sync by hand with the PAGE_TYPES literal in
# docs/site/src/content.config.ts, which enforces the same set at build time.
# Adding a type here without adding it there makes the site build reject a page
# this check accepts, which is a loud failure rather than a silent one.
PAGE_TYPES = tuple(CANONICAL_TEXT)

# The directory each page type must live in, relative to DOCS_DIR, compared
# exactly rather than as a prefix: a `reference` page belongs in `reference/`
# itself, not in a subdirectory of it. An empty string means the site root.
REQUIRED_DIR = {
    "tutorial": "tutorials",
    "how-to": "guides",
    "reference": "reference",
    "explanation": "explanation",
    "evaluation": "",
    "landing": "",
}

FRONTMATTER = re.compile(r"\A---\n(.*?)\n---\n", re.DOTALL)
PAGE_TYPE_KEY = re.compile(r"^pageType:\s*(\S+)\s*$", re.MULTILINE)
# The banner: `{/* DIATAXIS: <type> — <text> */}`. Both an em dash and a
# plain hyphen are accepted as the separator so the file is easy to type.
BANNER = re.compile(
    r"\{/\*\s*DIATAXIS:\s*([a-z-]+)\s*(?:—|--?)\s*(.*?)\*/\}", re.DOTALL
)


def normalize(text: str) -> str:
    """Collapse all whitespace runs, so banner line wrapping is free."""
    return " ".join(text.split())


def check_directory(declared: str, rel: Path) -> list[str]:
    """Return a problem if the page is not in the directory its type maps to."""
    required = REQUIRED_DIR[declared]
    actual = rel.parent.as_posix()
    actual = "" if actual == "." else actual
    if actual == required:
        return []
    where = f"`{required}/`" if required else "the site root"
    found = f"`{actual}/`" if actual else "the site root"
    return [f"a `{declared}` page must live in {where}, but this one is in {found}"]


def check_file(path: Path) -> list[str]:
    """Return a list of problem descriptions for one page (empty if fine)."""
    problems = []
    text = path.read_text()
    rel = path.relative_to(DOCS_DIR)

    fm_match = FRONTMATTER.match(text)
    if not fm_match:
        return ["no frontmatter block"]

    key_match = PAGE_TYPE_KEY.search(fm_match.group(1))
    if not key_match:
        return [
            "no `pageType` in frontmatter; expected one of: " + ", ".join(PAGE_TYPES)
        ]

    declared = key_match.group(1)
    if declared not in PAGE_TYPES:
        return [
            f"unknown pageType `{declared}`; expected one of: " + ", ".join(PAGE_TYPES)
        ]

    # The banner must be the first thing after the frontmatter, ahead of any
    # prose or imports, so an editor sees it without scrolling. It is MDX
    # comment syntax, so a plain .md page is exempt: Starlight would render
    # `{/* … */}` there as visible body text.
    body = text[fm_match.end() :]
    banner_match = BANNER.search(body)
    if path.suffix == ".md":
        problems.extend(check_directory(declared, rel))
        return problems
    if not banner_match:
        problems.append(
            "no DIATAXIS banner after the frontmatter. Paste this:\n"
            f"      {{/* DIATAXIS: {declared} — {CANONICAL_TEXT[declared]} */}}"
        )
    else:
        if body[: banner_match.start()].strip():
            problems.append(
                "the DIATAXIS banner must come first, before imports and prose"
            )
        banner_type = banner_match.group(1)
        if banner_type != declared:
            problems.append(
                f"banner says `{banner_type}` but frontmatter says `{declared}`"
            )
        elif normalize(banner_match.group(2)) != normalize(CANONICAL_TEXT[declared]):
            problems.append(
                f"banner text does not match the canonical wording for "
                f"`{declared}`; expected:\n      {CANONICAL_TEXT[declared]}"
            )

    problems.extend(check_directory(declared, rel))
    return problems


def main() -> int:
    pages = sorted(
        p
        for suffix in PAGE_SUFFIXES
        for p in DOCS_DIR.rglob(suffix)
        if not p.relative_to(DOCS_DIR).as_posix().startswith(EXCLUDED_PREFIXES)
    )
    if not pages:
        print(f"ERROR: no page files found in {DOCS_DIR}", file=sys.stderr)
        return 1

    failures = 0
    for page in pages:
        problems = check_file(page)
        if problems:
            failures += 1
            rel = page.relative_to(DOCS_DIR)
            print(f"{rel}:", file=sys.stderr)
            for problem in problems:
                print(f"    {problem}", file=sys.stderr)

    if failures:
        print(
            f"\n{failures} of {len(pages)} page(s) failed the page-type check. "
            "See docs/PAGE_TYPES.md.",
            file=sys.stderr,
        )
        return 1

    print(f"All pages declare a Diátaxis page type ({len(pages)} checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
