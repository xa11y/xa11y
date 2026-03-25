#!/usr/bin/env bash
# Integration test harness for xa11y Python bindings on Linux.
#
# Sets up Xvfb, D-Bus session, AT-SPI2, launches the accesskit+winit test app,
# builds the Python package via maturin, then runs pytest integration tests.
#
# Usage: ./run_python_integ_tests.sh
#        ./run_python_integ_tests.sh test_toggle  # run a single test

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
    # Deactivate virtualenv if active
    if [ -n "${VIRTUAL_ENV:-}" ]; then
        deactivate 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "=== xa11y Python integration test harness ==="
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

# 2. Start AT-SPI2 bus launcher and registryd
echo "Starting AT-SPI2 infrastructure..."

export NO_AT_BRIDGE=0
export AT_SPI_CLIENT=true
export ACCESSIBILITY_ENABLED=1

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

# 2b. Enable accessibility on the AT-SPI bus
echo "Enabling AT-SPI accessibility..."
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled variant:boolean:true \
    2>/dev/null || true
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true \
    2>/dev/null || true

# 3. Build the Rust workspace (test app + Python bindings)
echo "Building workspace..."
cargo build --workspace 2>&1

# 4. Set up Python virtualenv and install xa11y
echo "Setting up Python environment..."
python3 -m venv .venv-integ 2>/dev/null || python -m venv .venv-integ
source .venv-integ/bin/activate
pip install --quiet maturin pytest

echo "Building xa11y Python package..."
(cd xa11y-python && maturin develop --release 2>&1)

# 5. Launch the test application
echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app --headless &
CLEANUP_PIDS+=($!)

echo "Waiting for test app to register with AT-SPI..."
sleep 3

# 6. Run Python integration tests
echo "Running Python integration tests..."
TEST_FILTER="${1:-}"
set +e
if [ -n "$TEST_FILTER" ]; then
    pytest xa11y-python/tests/test_integ.py -v -m integ -k "$TEST_FILTER" 2>&1
else
    pytest xa11y-python/tests/test_integ.py -v -m integ 2>&1
fi
TEST_EXIT=$?
set -e

echo "=== Python integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
