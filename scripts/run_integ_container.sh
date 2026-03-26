#!/usr/bin/env bash
# Run Linux integration tests in a finch container with bind-mounted source.
#
# First time: builds base image (~2min) and creates cargo cache volume.
# Subsequent runs: incremental build + test (~10-30s).
#
# Usage:
#   ./run_integ_container.sh                    # run all integ tests
#   ./run_integ_container.sh tree_has_buttons    # run specific test(s)
#   ./run_integ_container.sh --build-only        # just build, don't test
#   ./run_integ_container.sh --shell             # drop into shell

set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="xa11y-base"
VOLUME="xa11y-cargo-cache"

# Build base image if it doesn't exist
if ! finch image inspect "$IMAGE" &>/dev/null; then
    echo "Building base image (one-time)..."
    finch build -t "$IMAGE" -f Containerfile.base .
fi

# Create cargo cache volume if it doesn't exist
finch volume inspect "$VOLUME" &>/dev/null 2>&1 || finch volume create "$VOLUME" >/dev/null

# Handle --shell mode
if [ "${1:-}" = "--shell" ]; then
    exec finch run --rm -it \
        -v "$(pwd):/xa11y" \
        -v "$VOLUME:/xa11y/target" \
        "$IMAGE" bash
fi

# Determine env vars
BUILD_ONLY=0
TEST_FILTER=""
if [ "${1:-}" = "--build-only" ]; then
    BUILD_ONLY=1
elif [ -n "${1:-}" ]; then
    TEST_FILTER="$*"
fi

finch run --rm \
    -v "$(pwd):/xa11y" \
    -v "$VOLUME:/xa11y/target" \
    -e "BUILD_ONLY=$BUILD_ONLY" \
    -e "TEST_FILTER=$TEST_FILTER" \
    "$IMAGE" bash /xa11y/scripts/run_integ_tests.sh
