#!/usr/bin/env python3
"""matrix_check.py — print the xa11y test coverage summary and document all gaps.

Run from the repo root:
    python tests/matrix_check.py

Three independent checks run, and any one failing exits non-zero:

  1. Internal consistency — every empty coverage cell has a matching
     documented gap entry.
  2. Reality — every app named in the matrix is one the shared harness knows
     about, and every (language, feature) the matrix claims maps to a test
     file that actually exists on disk. This keeps matrix.yaml from drifting
     into claiming coverage that no longer ships.
  3. Execution — every app's `platforms:` list matches the OS × app cells of
     the `integ` matrix in .github/workflows/ci.yml. Checks 1 and 2 both
     validate claims against *files*, which is how `platforms:` claimed a
     windows-latest Tauri cell that had never existed and took the Windows
     input-simulation gap with it (issue #348, and #327 before it).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML is required. Install with: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

TESTS_DIR = Path(__file__).parent
MATRIX_PATH = TESTS_DIR / "matrix.yaml"
HARNESS_PATH = TESTS_DIR / "harness" / "launch.py"
WORKFLOW_PATH = TESTS_DIR.parent / ".github" / "workflows" / "ci.yml"
LANGUAGES = ["python", "js", "cli"]
SYMBOLS = {True: "✅", False: "❌"}

# GitHub runner labels used by the `integ` matrix, mapped to the platform names
# matrix.yaml writes in `platforms:`. An unknown label is an error rather than
# a silently ignored cell — a runner rename must be noticed here.
RUNNER_PLATFORMS = {
    "ubuntu-latest": "linux",
    "macos-latest": "macos",
    "windows-latest": "windows",
}

# For each language, map a coverage *feature* to the test file(s) that would
# exercise it. A claimed feature is considered real if at least one of its
# mapped files exists in that language's suite directory. Features with no
# mapping for a language are themselves an error (the matrix is claiming a
# kind of test that language doesn't have).
SUITE_DIRS = {
    "python": TESTS_DIR / "suites" / "python",
    "js": TESTS_DIR / "suites" / "js",
    "cli": TESTS_DIR / "suites" / "cli",
}
FEATURE_FILES: dict[str, dict[str, list[str]]] = {
    "python": {
        "compat": ["test_compat.py"],
        "actions": ["test_actions.py"],
        "events": ["test_events.py"],
        "errors": ["test_errors.py"],
        "input_sim": ["test_input_sim.py"],
        "screenshot": ["test_screenshot.py"],
    },
    "cli": {
        # CLI "compat" is split across the tree-dump and find commands.
        "compat": ["test_tree.py", "test_find.py"],
        "actions": ["test_actions.py"],
        "input_sim": ["test_input_sim.py"],
        "screenshot": ["test_screenshot.py"],
        # The MCP suite lives in the cli suite because it drives the same
        # binary — parametrized over all three launchers rather than just the
        # one `cli_bin` resolves.
        "mcp": ["test_mcp.py"],
    },
    "js": {
        # JS suite files are numbered (e.g. 01_compat.test.js); match on the
        # feature substring.
        "compat": ["compat"],
        "actions": ["actions"],
        "input_sim": ["input_sim"],
        "screenshot": ["screenshot"],
    },
}


def harness_apps() -> set[str]:
    """Apps the shared harness recognises, parsed from launch.py's VALID_APPS."""
    text = HARNESS_PATH.read_text()
    m = re.search(r"VALID_APPS\s*=\s*\(([^)]*)\)", text)
    if not m:
        return set()
    return set(re.findall(r"[\"']([\w-]+)[\"']", m.group(1)))


def _feature_has_test(lang: str, feature: str) -> bool:
    """True if a test file backing (lang, feature) exists on disk."""
    patterns = FEATURE_FILES.get(lang, {}).get(feature)
    if not patterns:
        return False
    suite_dir = SUITE_DIRS[lang]
    if not suite_dir.is_dir():
        return False
    names = [p.name for p in suite_dir.iterdir() if p.is_file()]
    return any(any(pat in name for name in names) for pat in patterns)


def verify_against_tests(data: dict) -> list[str]:
    """Return a list of problems where the matrix doesn't match reality."""
    problems: list[str] = []
    coverage: dict[str, dict[str, list]] = data["coverage"]
    known_apps = harness_apps()

    # tested_on apps (for per-platform features) should also be real apps.
    referenced_apps = set(coverage)
    for feat, meta in data.get("features", {}).items():
        for app in meta.get("tested_on", []) or []:
            referenced_apps.add(app)

    if known_apps:
        for app in sorted(referenced_apps):
            if app not in known_apps:
                problems.append(
                    f"app '{app}' is in matrix.yaml but not in the harness "
                    f"VALID_APPS ({HARNESS_PATH.relative_to(TESTS_DIR.parent)})"
                )

    for app, lang_map in coverage.items():
        for lang, features in lang_map.items():
            for feature in features or []:
                if feature not in FEATURE_FILES.get(lang, {}):
                    problems.append(
                        f"coverage.{app}.{lang} claims feature '{feature}', "
                        f"which has no known {lang} test mapping"
                    )
                elif not _feature_has_test(lang, feature):
                    problems.append(
                        f"coverage.{app}.{lang} claims feature '{feature}', "
                        f"but no backing test file exists in {SUITE_DIRS[lang]}"
                    )
    return problems


def integ_cells() -> dict[str, set[str]]:
    """Parse the `integ` job's OS × app matrix out of the CI workflow.

    Returns {app: {platform, ...}} using matrix.yaml's platform names.

    Raises RuntimeError if the workflow no longer has the shape this reads —
    a silently-empty result would turn the whole platform check into a no-op,
    which is exactly the failure mode this check exists to prevent.
    """
    with WORKFLOW_PATH.open() as f:
        workflow = yaml.safe_load(f)

    try:
        include = workflow["jobs"]["integ"]["strategy"]["matrix"]["include"]
    except (KeyError, TypeError) as exc:
        raise RuntimeError(
            f"could not find jobs.integ.strategy.matrix.include in "
            f"{WORKFLOW_PATH}; the platform check cannot run against a "
            f"workflow it can't read"
        ) from exc

    cells: dict[str, set[str]] = {}
    for entry in include:
        os_label, app = entry.get("os"), entry.get("app")
        if os_label is None or app is None:
            raise RuntimeError(f"integ matrix entry is missing os/app: {entry!r}")
        if os_label not in RUNNER_PLATFORMS:
            raise RuntimeError(
                f"integ matrix entry uses runner {os_label!r}, which "
                f"matrix_check.py doesn't map to a platform (known: "
                f"{', '.join(sorted(RUNNER_PLATFORMS))})"
            )
        cells.setdefault(app, set()).add(RUNNER_PLATFORMS[os_label])
    return cells


def verify_platforms(data: dict) -> list[str]:
    """Return problems where `platforms:` disagrees with the CI integ matrix.

    `platforms:` used to be unvalidated prose that read as authoritative. Three
    ways it can now be wrong, all fatal:

    * a platform is claimed but has no `integ` cell and no
      `covered_outside_integ` entry naming the vehicle that covers it instead;
    * a cell exists for a platform the app doesn't claim;
    * a `covered_outside_integ` entry is stale — it names a platform the app no
      longer claims, or one that *does* have a cell now, so the entry excuses
      nothing while still reading as a live design decision.
    """
    problems: list[str] = []
    apps_meta: dict = data.get("apps", {})
    cells = integ_cells()

    for app in sorted(set(cells) - set(apps_meta)):
        problems.append(
            f"the integ matrix has cells for app '{app}' "
            f"({', '.join(sorted(cells[app]))}), which matrix.yaml doesn't describe"
        )

    for app, meta in apps_meta.items():
        claimed = set(meta.get("platforms", []) or [])
        actual = cells.get(app, set())
        elsewhere = meta.get("covered_outside_integ", {}) or {}

        for platform in sorted(claimed - actual - set(elsewhere)):
            problems.append(
                f"apps.{app}.platforms claims '{platform}', but the integ "
                f"matrix has no {{os: *-latest, app: {app}}} cell there. Add "
                f"the cell, drop the claim, or record the covering vehicle "
                f"under apps.{app}.covered_outside_integ.{platform}"
            )
        for platform in sorted(actual - claimed):
            problems.append(
                f"the integ matrix runs {app} on {platform}, but "
                f"apps.{app}.platforms doesn't list it"
            )
        for platform, reason in sorted(elsewhere.items()):
            if platform not in claimed:
                problems.append(
                    f"apps.{app}.covered_outside_integ names '{platform}', "
                    f"which apps.{app}.platforms no longer claims (stale entry)"
                )
            elif platform in actual:
                problems.append(
                    f"apps.{app}.covered_outside_integ names '{platform}', "
                    f"which now has an integ cell (stale entry)"
                )
            elif not str(reason).strip():
                problems.append(
                    f"apps.{app}.covered_outside_integ.{platform} has no "
                    f"reason; it must name the vehicle that covers the platform"
                )
    return problems


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

    # Reality check: does the matrix match the tests/harness on disk?
    drift = verify_against_tests(data)

    print("Reality Check (matrix vs. tests on disk)")
    print("=" * 60)
    if drift:
        for problem in drift:
            print(f"  ✗ {problem}")
    else:
        print("  OK: every claimed (app, language, feature) maps to a real test.")
    print()

    # Execution check: does `platforms:` match the cells CI actually runs?
    try:
        platform_drift = verify_platforms(data)
    except RuntimeError as exc:
        platform_drift = [str(exc)]

    print("Execution Check (platforms: vs. the CI integ matrix)")
    print("=" * 60)
    if platform_drift:
        for problem in platform_drift:
            print(f"  ✗ {problem}")
    else:
        print("  OK: every declared platform has a cell that runs it.")
    print()

    # Result
    failed = False
    if undocumented:
        print("FAIL: undocumented empty coverage cells (add a gap entry in matrix.yaml):")
        for app, lang in undocumented:
            print(f"  coverage.{app}.{lang}")
        failed = True

    if drift:
        print("FAIL: matrix.yaml claims coverage that doesn't exist on disk (see above).")
        failed = True

    if platform_drift:
        print("FAIL: matrix.yaml's platforms: disagree with the CI integ matrix (see above).")
        failed = True

    if failed:
        return 1

    print(
        "OK: matrix is internally consistent, matches the tests on disk, "
        "and matches the platforms CI runs."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
