#!/usr/bin/env bash
# Seed the action's tool cache through the release-assets API. The public
# /releases/download route can return 504 while API asset downloads work.
# Keep sccache-action for its GHA cache credentials and post-run statistics.
set -euo pipefail

version=0.19.0
case "$RUNNER_ARCH" in
  X64) arch=x86_64; cache_arch=x64 ;;
  ARM64) arch=aarch64; cache_arch=arm64 ;;
  *) echo "Unsupported sccache runner architecture: $RUNNER_ARCH" >&2; exit 1 ;;
esac
case "$RUNNER_OS" in
  Linux) platform=unknown-linux-musl ;;
  macOS) platform=apple-darwin ;;
  Windows) platform=pc-windows-msvc ;;
  *) echo "Unsupported sccache runner OS: $RUNNER_OS" >&2; exit 1 ;;
esac

task_tools="$RUNNER_TOOL_CACHE"
task_temp="$RUNNER_TEMP"
if [[ "$RUNNER_OS" == Windows ]]; then
  task_tools=$(cygpath -u "$task_tools")
  task_temp=$(cygpath -u "$task_temp")
fi
task_cache="$task_tools/sccache/$version/$cache_arch"
if [[ -f "$task_cache.complete" ]]; then
  echo "Using cached sccache $version for $RUNNER_OS/$RUNNER_ARCH"
  exit 0
fi
task_download=$(mktemp -d "$task_temp/xa11y-sccache.XXXXXX")
archive="sccache-v$version-$arch-$platform.tar.gz"
gh release download "v$version" --repo mozilla/sccache \
  --pattern "$archive" --pattern "$archive.sha256" --dir "$task_download"

# Match sccache-action's checksum verification before populating its cache.
node -e '
  const fs = require("node:fs"), crypto = require("node:crypto");
  const file = process.argv[1];
  const actual = crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
  const expected = fs.readFileSync(file + ".sha256", "utf8").trim();
  if (actual !== expected) throw new Error("sccache release checksum mismatch");
' "$task_download/$archive"
tar -xzf "$task_download/$archive" -C "$task_download"
mkdir -p "$task_cache"
cp -R "$task_download/sccache-v$version-$arch-$platform/." "$task_cache/"
touch "$task_cache.complete"
