#!/usr/bin/env bash
# Integration test harness for xa11y on Linux.
#
# Sets up Xvfb, D-Bus session, AT-SPI2, launches the accesskit+winit test app,
# then runs the integration tests with --ignored.
#
# Usage: ./run_integ_tests.sh
#
# Compatible with Ubuntu 22.04+ and 24.04+ (uses dbus-run-session).

set -euo pipefail

# If we're not already inside a D-Bus session, re-exec under dbus-run-session.
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    echo "No D-Bus session found, re-launching under dbus-run-session..."
    exec dbus-run-session -- bash "$0" "$@"
fi

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y integration test harness ==="
echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"

# 1. Find a free display number and start Xvfb
XVFB_DISPLAY=":99"
for d in 99 98 97 96 95; do
    if [ ! -e "/tmp/.X${d}-lock" ]; then
        XVFB_DISPLAY=":${d}"
        break
    fi
done

echo "Starting Xvfb on $XVFB_DISPLAY..."
Xvfb "$XVFB_DISPLAY" -screen 0 1280x1024x24 -ac &
CLEANUP_PIDS+=($!)
sleep 1

export DISPLAY="$XVFB_DISPLAY"
echo "DISPLAY=$DISPLAY"

# 2. Start AT-SPI2 (bus launcher + registryd) and flip the Status flags.
#    Single source of truth — shared with the per-app harness and the
#    setup-a11y CI action. The daemons it backgrounds live for the rest of
#    this dbus-run-session, which exits when the script does.
# shellcheck source=setup_linux_a11y.sh
source "$(cd "$(dirname "$0")" && pwd)/setup_linux_a11y.sh"

# 3. Build everything
echo "Building workspace..."
cargo build --workspace --features xa11y/strict-roles 2>&1

# Support BUILD_ONLY mode (for pre-warming the build cache)
if [ "${BUILD_ONLY:-}" = "1" ]; then
    echo "=== Build complete (build-only mode) ==="
    exit 0
fi

# 4. Launch the test application (run binary directly, not via cargo run,
#    because cargo run changes the process owner name in AT-SPI)
echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app --headless &
CLEANUP_PIDS+=($!)

# Wait for the app to start and register with AT-SPI
echo "Waiting for test app to register with AT-SPI..."
sleep 3

# 5. Run integration tests
echo "Running integration tests..."
TEST_FILTER="${TEST_FILTER:-}"
set +e
NOCAPTURE_ARG=""
if [ "${INTEG_NOCAPTURE:-0}" = "1" ]; then
    NOCAPTURE_ARG="--nocapture"
fi
if [ -n "$TEST_FILTER" ]; then
    cargo test -p xa11y --features strict-roles --test integ_test -- --ignored --test-threads=1 $NOCAPTURE_ARG $TEST_FILTER 2>&1
else
    cargo test -p xa11y --features strict-roles --test integ_test -- --ignored --test-threads=1 $NOCAPTURE_ARG 2>&1
fi
TEST_EXIT=$?
set -e

echo "=== Integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
