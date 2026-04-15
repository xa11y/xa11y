#!/usr/bin/env bash
# Integration test harness for xa11y JS bindings.
#
# On Linux: sets up Xvfb + D-Bus + AT-SPI2 + launches the AccessKit test app,
# then runs node --test against the JS integration suite. On macOS and Windows
# the caller is responsible for providing an interactive session, and we
# assume the test app has already been built.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
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

echo "=== xa11y JS integration test harness ==="

# ── Platform-specific display setup ──────────────────────────────────

if [[ "$(uname)" == "Linux" ]]; then
    if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        echo "No D-Bus session found, re-launching under dbus-run-session..."
        exec dbus-run-session -- bash "$0" "$@"
    fi

    echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"

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
        string:org.a11y.Status string:IsEnabled variant:boolean:true \
        2>/dev/null || true
    dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
        org.freedesktop.DBus.Properties.Set \
        string:org.a11y.Status string:ScreenReaderEnabled variant:boolean:true \
        2>/dev/null || true
fi

# ── Build the AccessKit test app ─────────────────────────────────────

echo "Building xa11y-test-app..."
cd "$PROJECT_ROOT"
cargo build -p xa11y-test-app

# ── Build the JS bindings ────────────────────────────────────────────

echo "Installing JS dev dependencies..."
cd "$JS_DIR"
if [ ! -d node_modules ]; then
    npm ci
fi

echo "Building JS bindings (debug)..."
npx napi build --platform --js native.js --dts native.d.ts
node scripts/patch-native-dts.mjs

# ── Launch the test application ──────────────────────────────────────

echo "Launching xa11y-test-app..."
"$PROJECT_ROOT/target/debug/xa11y-test-app" --headless &
APP_PID=$!
CLEANUP_PIDS+=("$APP_PID")

echo "Waiting for test app (pid=$APP_PID) to register with the a11y bus..."
sleep 3

# ── Run tests ────────────────────────────────────────────────────────

echo "Running JS integration tests..."
set +e
# 90s overall budget: tree discovery and actions each have their own
# internal timeouts.
cd "$JS_DIR"
timeout 180 node --test --test-timeout=60000 '__test__/integ/**/*.test.js'
TEST_EXIT=$?
set -e

echo "=== JS integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
