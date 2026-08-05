#!/usr/bin/env python3
"""Bump the version in pytest-xa11y/pyproject.toml.

pytest-xa11y versions independently of xa11y, so it is deliberately outside
cargo-release's shared-version scheme. This script is the whole mechanism:
read the current version, apply the requested level, write it back, print the
new version.

Usage:
    python .github/scripts/bump_pytest_xa11y.py --level patch
    python .github/scripts/bump_pytest_xa11y.py --show
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

PYPROJECT = Path(__file__).resolve().parents[2] / "pytest-xa11y" / "pyproject.toml"

# Matches the version line inside [project]. Deliberately anchored to the
# line start so a version string elsewhere in the file cannot be picked up.
VERSION_RE = re.compile(r'^version = "(\d+)\.(\d+)\.(\d+)"$', re.MULTILINE)


def read_version(text: str) -> tuple[int, int, int]:
    matches = VERSION_RE.findall(text)
    if len(matches) != 1:
        raise SystemExit(
            f"Expected exactly one `version = \"X.Y.Z\"` line in {PYPROJECT}, "
            f"found {len(matches)}."
        )
    major, minor, patch = matches[0]
    return int(major), int(minor), int(patch)


def bump(version: tuple[int, int, int], level: str) -> tuple[int, int, int]:
    major, minor, patch = version
    if level == "major":
        return major + 1, 0, 0
    if level == "minor":
        return major, minor + 1, 0
    if level == "patch":
        return major, minor, patch + 1
    raise SystemExit(f"Unknown level {level!r}; expected major, minor or patch.")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--level", choices=["major", "minor", "patch"])
    parser.add_argument(
        "--show",
        action="store_true",
        help="Print the current version without changing it.",
    )
    args = parser.parse_args(argv)

    text = PYPROJECT.read_text(encoding="utf-8")
    current = read_version(text)

    if args.show or args.level is None:
        print(".".join(str(part) for part in current))
        return 0

    new = bump(current, args.level)
    new_str = ".".join(str(part) for part in new)
    PYPROJECT.write_text(
        VERSION_RE.sub(f'version = "{new_str}"', text, count=1), encoding="utf-8"
    )
    print(new_str)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
