#!/usr/bin/env bash
# Integration test harness for the xa11y Electron tests on Linux.
#
# Sets up Xvfb + D-Bus + AT-SPI2, installs Electron in test-apps/electron, builds
# the JS bindings, then runs the JS Electron integration suite via `node --test`.
#
# Usage: ./scripts/run_electron_tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ELECTRON_APP_DIR="$PROJECT_ROOT/test-apps/electron"
JS_DIR="$PROJECT_ROOT/xa11y-js"

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y Electron integration test harness ==="

if [[ "$(uname)" != "Linux" ]]; then
    echo "Electron a11y tests only run on Linux (the bridge bug is Linux-specific)."
    exit 0
fi

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    echo "No D-Bus session found, re-launching under dbus-run-session..."
    exec dbus-run-session -- bash "$0" "$@"
fi

echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"

# ── Display ──────────────────────────────────────────────────────────
if [ -z "${DISPLAY:-}" ]; then
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
fi
echo "DISPLAY=$DISPLAY"

# ── AT-SPI2 ──────────────────────────────────────────────────────────
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
fi
sleep 1

if command -v /usr/libexec/at-spi2-registryd &>/dev/null; then
    /usr/libexec/at-spi2-registryd &
    CLEANUP_PIDS+=($!)
elif command -v at-spi2-registryd &>/dev/null; then
    at-spi2-registryd &
    CLEANUP_PIDS+=($!)
fi
sleep 1

dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled variant:boolean:true 2>/dev/null || true
dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
    org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true 2>/dev/null || true

# ── Electron install ────────────────────────────────────────────────
if [ ! -x "$ELECTRON_APP_DIR/node_modules/electron/dist/electron" ]; then
    echo "Installing Electron in $ELECTRON_APP_DIR..."
    (cd "$ELECTRON_APP_DIR" && npm install --no-audit --no-fund --silent)
fi

# ── Build the JS bindings ────────────────────────────────────────────
echo "Installing JS dev dependencies..."
cd "$JS_DIR"
if [ ! -d node_modules ]; then
    npm ci
fi

echo "Building JS bindings (debug)..."
npx napi build --platform --js native.js --dts native.d.ts
node scripts/patch-native-dts.mjs

# ── Run tests ────────────────────────────────────────────────────────
cd "$PROJECT_ROOT"
OVERALL_EXIT=0

echo "Running Electron JS integration tests..."
set +e
XA11Y_TEST_APP=electron timeout 300 node --test --test-timeout=120000 --test-reporter=spec \
    'tests/suites/js/**/*.test.js'
JS_EXIT=$?
set -e
[ $JS_EXIT -ne 0 ] && OVERALL_EXIT=$JS_EXIT
echo "--- JS suite finished (exit code: $JS_EXIT) ---"

echo "Running Electron Python integration tests..."
set +e
XA11Y_TEST_APP=electron timeout 300 python3 -m pytest tests/suites/python/ \
    -v -s --timeout=60 --rootdir=.
PYTHON_EXIT=$?
set -e
[ $PYTHON_EXIT -ne 0 ] && OVERALL_EXIT=$PYTHON_EXIT
echo "--- Python suite finished (exit code: $PYTHON_EXIT) ---"

echo "Running Electron CLI integration tests..."
set +e
XA11Y_TEST_APP=electron timeout 300 python3 -m pytest tests/suites/cli/ \
    -v -s --timeout=60 --rootdir=.
CLI_EXIT=$?
set -e
[ $CLI_EXIT -ne 0 ] && OVERALL_EXIT=$CLI_EXIT
echo "--- CLI suite finished (exit code: $CLI_EXIT) ---"

echo "=== Electron integration tests finished (overall exit code: $OVERALL_EXIT) ==="
exit $OVERALL_EXIT
