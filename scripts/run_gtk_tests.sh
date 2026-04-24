#!/usr/bin/env bash
# Integration test harness for xa11y GTK4 tests.
#
# On Linux: sets up Xvfb, D-Bus, AT-SPI2.
# On macOS: assumes a display is available (requires gtk4 + pygobject via Homebrew).
#
# Usage: ./scripts/run_gtk_tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GTK_APP_DIR="$PROJECT_ROOT/test-apps/gtk"

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y GTK4 integration test harness ==="

# ── Platform-specific display setup ──────────────────────────────────

if [[ "$(uname)" == "Linux" ]]; then
    # Re-exec under D-Bus session if needed
    if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        echo "No D-Bus session found, re-launching under dbus-run-session..."
        exec dbus-run-session -- bash "$0" "$@"
    fi

    echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"

    # Start Xvfb if no display
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

    # Start AT-SPI2
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

# ── Set up shared Python integ venv ──────────────────────────────────
# shellcheck source=setup_python_integ_env.sh
source "$SCRIPT_DIR/setup_python_integ_env.sh" "$GTK_APP_DIR/requirements.txt"

cd "$PROJECT_ROOT"

# ── Run tests ────────────────────────────────────────────────────────

echo "Running GTK4 integration tests..."
set +e
XA11Y_TEST_APP=gtk timeout 300 "$PYTEST" "$PROJECT_ROOT/tests/suites/python/" -v -s --timeout=60 --rootdir="$PROJECT_ROOT" 2>&1
TEST_EXIT=$?
set -e

echo "=== GTK4 integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
