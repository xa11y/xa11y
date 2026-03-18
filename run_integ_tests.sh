#!/usr/bin/env bash
# Integration test harness for xa11y on Linux.
#
# Sets up Xvfb, D-Bus session, AT-SPI2, launches the GTK test app,
# then runs the integration tests with --ignored.
#
# Usage: ./run_integ_tests.sh

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

echo "=== xa11y integration test harness ==="

# 1. Find a free display number
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

# 2. Start a D-Bus session bus
echo "Starting D-Bus session bus..."
eval "$(dbus-launch --sh-syntax)"
echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"
CLEANUP_PIDS+=("$DBUS_SESSION_BUS_PID")

# 3. Start AT-SPI2 bus launcher and registryd
echo "Starting AT-SPI2 infrastructure..."

# Enable AT-SPI
export NO_AT_BRIDGE=0
export GTK_MODULES=atk-bridge
export AT_SPI_CLIENT=true

# The AT-SPI bus launcher creates a separate accessibility bus
if command -v /usr/libexec/at-spi-bus-launcher &>/dev/null; then
    /usr/libexec/at-spi-bus-launcher --launch-immediately &
    CLEANUP_PIDS+=($!)
elif command -v at-spi-bus-launcher &>/dev/null; then
    at-spi-bus-launcher --launch-immediately &
    CLEANUP_PIDS+=($!)
else
    echo "WARNING: at-spi-bus-launcher not found, AT-SPI2 may not work"
fi

sleep 1

# Start the registry daemon
if command -v /usr/libexec/at-spi2-registryd &>/dev/null; then
    /usr/libexec/at-spi2-registryd &
    CLEANUP_PIDS+=($!)
elif command -v at-spi2-registryd &>/dev/null; then
    at-spi2-registryd &
    CLEANUP_PIDS+=($!)
else
    echo "WARNING: at-spi2-registryd not found"
fi

sleep 1

# 4. Build everything
echo "Building workspace..."
cargo build --workspace 2>&1

# 5. Launch the test application
echo "Launching xa11y-test-app..."
cargo run -p xa11y-test-app -- --headless &
CLEANUP_PIDS+=($!)

# Wait for the app to start and register with AT-SPI
echo "Waiting for test app to register with AT-SPI..."
sleep 3

# 6. Run integration tests
echo "Running integration tests..."
set +e
cargo test -p xa11y --test integ_test -- --ignored --test-threads=1 2>&1
TEST_EXIT=$?
set -e

echo "=== Integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
