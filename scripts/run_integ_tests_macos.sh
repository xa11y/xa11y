#!/usr/bin/env bash
# Integration test harness for xa11y on macOS.
#
# Launches the accesskit+winit test app and runs integration tests.
# Requires macOS accessibility permissions and a working xa11y-macos provider.
#
# Usage: ./run_integ_tests_macos.sh

set -euo pipefail

TEST_PROFILE="${XA11Y_TEST_PROFILE:-debug}"
CARGO_PROFILE_ARG=""
case "$TEST_PROFILE" in
    debug) ;;
    release) CARGO_PROFILE_ARG="--release" ;;
    *) echo "error: XA11Y_TEST_PROFILE must be debug or release" >&2; exit 1 ;;
esac
TARGET_BIN_DIR="${CARGO_TARGET_DIR:-target}/$TEST_PROFILE"

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y macOS integration test harness ==="

# 1. Build everything
echo "Building workspace..."
cargo build ${CARGO_PROFILE_ARG:+"$CARGO_PROFILE_ARG"} --workspace --features xa11y/strict-roles 2>&1

# 2. Launch the test application (run binary directly, not via cargo run,
#    because cargo run changes the process owner name in CGWindowListCopyWindowInfo)
echo "Launching xa11y-test-app..."
"$TARGET_BIN_DIR/xa11y-test-app" --headless &
TEST_APP_PID=$!
CLEANUP_PIDS+=($TEST_APP_PID)

# Wait for accessibility registration
echo "Waiting for test app to register..."
sleep 2

# 3. Run integration tests
echo "Running integration tests..."
set +e
cargo test ${CARGO_PROFILE_ARG:+"$CARGO_PROFILE_ARG"} -p xa11y --features strict-roles --test integ_test -- --ignored --test-threads=1 2>&1
TEST_EXIT=$?

# 4. Restart the test application for the AX call count regression tests.
#    The integ phase above drives fullscreen and minimize transitions, whose
#    transient shell windows change the app's tree; the counts below are a
#    regression guard for a known tree, so measure a freshly launched app.
echo "Restarting xa11y-test-app for the AX call count tests..."
kill "$TEST_APP_PID" 2>/dev/null || true
wait "$TEST_APP_PID" 2>/dev/null || true
"$TARGET_BIN_DIR/xa11y-test-app" --headless &
TEST_APP_PID=$!
CLEANUP_PIDS+=($TEST_APP_PID)
sleep 2

# 5. Run macOS provider AX call count regression tests
echo "Running AX call count regression tests..."
cargo test ${CARGO_PROFILE_ARG:+"$CARGO_PROFILE_ARG"} -p xa11y-macos -- --ignored --test-threads=1 2>&1
MACOS_EXIT=$?
set -e

if [ $TEST_EXIT -ne 0 ] || [ $MACOS_EXIT -ne 0 ]; then
    FINAL_EXIT=1
else
    FINAL_EXIT=0
fi

echo "=== Integration tests finished (exit code: $FINAL_EXIT) ==="
exit $FINAL_EXIT
