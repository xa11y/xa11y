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

export DISPLAY="$XVFB_DISPLAY"

# Poll until X is ready (up to 5s)
for i in $(seq 1 50); do
    if xdpyinfo -display "$DISPLAY" &>/dev/null 2>&1; then break; fi
    sleep 0.1
done
echo "DISPLAY=$DISPLAY"

# 2. Start AT-SPI2 bus launcher and registryd
echo "Starting AT-SPI2 infrastructure..."

# Enable AT-SPI
export NO_AT_BRIDGE=0
export AT_SPI_CLIENT=true
export ACCESSIBILITY_ENABLED=1

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

# Poll until AT-SPI bus is reachable (up to 5s)
for i in $(seq 1 50); do
    if dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
        org.freedesktop.DBus.Peer.Ping &>/dev/null 2>&1; then break; fi
    sleep 0.1
done

# 2b. Enable accessibility on the AT-SPI bus (required for apps to register)
echo "Enabling AT-SPI accessibility..."
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled variant:boolean:true \
    2>/dev/null || true
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true \
    2>/dev/null || true

# 3. Build the test app (tests are compiled by cargo test below)
echo "Building test app..."
cargo build -p xa11y-test-app 2>&1

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

# Wait for the app to register with AT-SPI (poll up to 10s)
echo "Waiting for test app to register with AT-SPI..."
for i in $(seq 1 100); do
    if dbus-send --session --print-reply --dest=org.a11y.atspi.Registry \
        /org/a11y/atspi/accessible/root org.a11y.atspi.Accessible.GetChildren \
        &>/dev/null 2>&1; then break; fi
    sleep 0.1
done

# 5. Run integration tests
echo "Running integration tests..."
TEST_FILTER="${TEST_FILTER:-}"
set +e
if [ -n "$TEST_FILTER" ]; then
    cargo test -p xa11y --test integ_test -- --ignored --test-threads=1 $TEST_FILTER 2>&1
else
    cargo test -p xa11y --test integ_test -- --ignored --test-threads=1 2>&1
fi
TEST_EXIT=$?
set -e

echo "=== Integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
