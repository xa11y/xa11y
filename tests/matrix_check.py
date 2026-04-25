#!/usr/bin/env python3
"""matrix_check.py — print the xa11y test coverage summary and document all gaps.

Run from the repo root:
    python tests/matrix_check.py

Exits 0 if all empty coverage cells have a matching documented gap entry.
Exits 1 if any empty cell is undocumented.

# TODO: add --strict flag to fail on empty cells without a gap entry, so
#       this can be used as a CI gate to prevent silent coverage regressions.
"""

from __future__ import annotations

import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML is required. Install with: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

MATRIX_PATH = Path(__file__).parent / "matrix.yaml"
LANGUAGES = ["python", "js", "cli"]
SYMBOLS = {True: "✅", False: "❌"}


def load_matrix() -> dict:
    with MATRIX_PATH.open() as f:
        return yaml.safe_load(f)


def gap_ids_for(gaps: list[dict], app: str, lang: str) -> list[str]:
    """Return IDs of gap entries that cover the given (app, language) pair."""
    matched = []
    for gap in gaps:
        affects = gap.get("affects", {})
        apps = affects.get("apps", [])
        language = affects.get("language", None)
        langs = [language] if isinstance(language, str) else (language or [])
        if app in apps and (language is None or lang in langs):
            matched.append(gap["id"])
    return matched


def main() -> int:
    data = load_matrix()
    coverage: dict[str, dict[str, list]] = data["coverage"]
    gaps: list[dict] = data.get("gaps", [])
    apps_meta: dict = data.get("apps", {})

    col_width = max(len(lang) for lang in LANGUAGES) + 2
    app_width = max(len(app) for app in coverage) + 2

    # Print header
    print("xa11y Test Coverage Matrix")
    print("=" * 60)
    header = f"{'App':<{app_width}}" + "".join(f"{lang:<{col_width}}" for lang in LANGUAGES)
    print(header)
    print("-" * len(header))

    undocumented: list[tuple[str, str]] = []

    for app, lang_map in coverage.items():
        row = f"{app:<{app_width}}"
        for lang in LANGUAGES:
            features: list = lang_map.get(lang, [])
            if features:
                cell = SYMBOLS[True]
            else:
                cell = SYMBOLS[False]
            row += f"{cell:<{col_width}}"

            # Check whether an empty cell has a documented gap
            if not features:
                gap_ids = gap_ids_for(gaps, app, lang)
                if not gap_ids:
                    undocumented.append((app, lang))

        print(row)

    print()

    # Print gaps section
    print("Documented Gaps")
    print("=" * 60)
    for gap in gaps:
        severity = gap.get("severity", "unknown").upper()
        print(f"[{severity}] {gap['id']}: {gap['description'].strip()}")
        if "workaround" in gap:
            print(f"         Workaround: {gap['workaround']}")
        print()

    # Print unsupported platforms
    print("Platform Exclusions")
    print("=" * 60)
    for app, meta in apps_meta.items():
        unsupported = meta.get("unsupported", {})
        for platform, info in unsupported.items():
            reason = info.get("reason", "")
            print(f"  {app} / {platform}: {reason}")
    print()

    # Result
    if undocumented:
        print("FAIL: undocumented empty coverage cells (add a gap entry in matrix.yaml):")
        for app, lang in undocumented:
            print(f"  coverage.{app}.{lang}")
        return 1

    print("OK: all empty coverage cells have a documented gap entry.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
