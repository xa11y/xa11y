#!/usr/bin/env bash
# macOS platform fuzzer harness for xa11y.
#
# Launches xa11y-test-app, then runs the macos-provider-fuzz binary to
# exercise all code paths in xa11y-macos with random operations.
#
# Usage:
#   ./run_fuzz_macos.sh                          # random seed, 10k iterations
#   ./run_fuzz_macos.sh --seed 42 -n 50000       # reproducible, 50k iterations
#   FUZZ_SEED=42 FUZZ_ITERATIONS=5000 ./run_fuzz_macos.sh
#
# To run with coverage:
#   cargo llvm-cov run --bin macos-provider-fuzz -- --seed 42 -n 10000

set -euo pipefail

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y macOS Platform Fuzzer ==="

# 1. Build everything
echo "Building workspace..."
cargo build --workspace 2>&1

# 2. Launch the test application
echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app &
CLEANUP_PIDS+=($!)

# Wait for accessibility registration
echo "Waiting for test app to register..."
sleep 2

# 3. Build fuzzer args
FUZZ_ARGS=""
if [ -n "${FUZZ_SEED:-}" ]; then
    FUZZ_ARGS="$FUZZ_ARGS --seed $FUZZ_SEED"
fi
if [ -n "${FUZZ_ITERATIONS:-}" ]; then
    FUZZ_ARGS="$FUZZ_ARGS --iterations $FUZZ_ITERATIONS"
fi

# Pass through any CLI args
FUZZ_ARGS="$FUZZ_ARGS $*"

# 4. Run the fuzzer
echo "Running fuzzer..."
set +e
./target/debug/macos-provider-fuzz $FUZZ_ARGS
FUZZ_EXIT=$?
set -e

echo "=== Fuzzer finished (exit code: $FUZZ_EXIT) ==="
exit $FUZZ_EXIT
