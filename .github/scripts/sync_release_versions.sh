#!/usr/bin/env bash
# Bring every version field cargo-release does not own into line with the
# workspace version, and refresh the lockfiles that record them.
#
#   .github/scripts/sync_release_versions.sh [VERSION]
#
# VERSION defaults to [workspace.package].version in the root Cargo.toml,
# which is what `cargo release version <level>` has just written.
#
# cargo-release bumps the workspace manifests and the root Cargo.lock. It
# does not see:
#   * xa11y-python/ and xa11y-js/, which the workspace excludes, so their
#     Cargo.toml, package.json and package-lock.json;
#   * the lockfiles of those two crates and of xa11y/fuzz/, each of which
#     records every xa11y path crate's version. A stale entry there is what
#     makes `cargo build --locked` fail from a release tag (issue #463).
#
# The publish workflow runs this between bumping and committing, so the
# release commit, and the tag on it, carry every field. Afterwards
# `cargo xtask check-versions --expect "$VERSION"` must pass; the workflow
# runs it before tagging.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

version="${1:-}"
if [ -z "$version" ]; then
  version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
fi
version="${version#v}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]]; then
  echo "error: '$version' is not a semver version" >&2
  exit 2
fi
echo "Syncing release version $version"

# [package].version is the first `version = ` line at column 0 in both
# manifests; dependency tables spell it inline (`foo = { version = ... }`).
# awk rather than `sed 0,/re/`, which is GNU-only and this also runs on macOS.
for manifest in xa11y-python/Cargo.toml xa11y-js/Cargo.toml; do
  awk -v v="$version" '
    !done && /^version = "/ { print "version = \"" v "\""; done = 1; next }
    { print }
  ' "$manifest" > "$manifest.tmp"
  mv "$manifest.tmp" "$manifest"
  echo "  $manifest"
done

# Updates package.json and both version fields of package-lock.json.
(cd xa11y-js && npm version "$version" --no-git-tag-version --allow-same-version >/dev/null)
echo "  xa11y-js/package.json, xa11y-js/package-lock.json"

# Every tracked lockfile that records the xa11y path crates. Any cargo command
# that resolves rewrites a path package's stale version and leaves registry
# pins alone, so this changes nothing else in the lock.
git ls-files -z -- 'Cargo.lock' '*/Cargo.lock' | while IFS= read -r -d '' lock; do
  grep -qx 'name = "xa11y-core"' "$lock" || continue
  cargo metadata --format-version 1 --manifest-path "$(dirname "$lock")/Cargo.toml" >/dev/null
  echo "  $lock"
done
