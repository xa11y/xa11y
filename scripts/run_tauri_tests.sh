#!/usr/bin/env bash
# Integration test harness for xa11y Tauri tests.
#
# On Linux: sets up Xvfb, D-Bus, AT-SPI2.
# On macOS/Windows: assumes a display is available.
#
# Usage: ./scripts/run_tauri_tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y Tauri integration test harness ==="

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

# ── Build Tauri app ───────────────────────────────────────────────────

echo "Building Tauri test app..."
cd "$PROJECT_ROOT"
cargo build -p xa11y-tauri-test-app

# ── Set up Python venv ───────────────────────────────────────────────

VENV_DIR="$PROJECT_ROOT/.venv-tauri-test"

if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtualenv at $VENV_DIR..."
    python3 -m venv "$VENV_DIR"
fi

PIP="$VENV_DIR/bin/pip"
PYTEST="$VENV_DIR/bin/pytest"

echo "Installing dependencies..."
"$PIP" install --quiet maturin
"$PIP" install --quiet -r "$PROJECT_ROOT/tests/requirements.txt"

# Generate README for xa11y-python (it's in .gitignore, maturin needs it)
echo "Generating xa11y-python README..."
cd "$PROJECT_ROOT"
cargo xtask sync-readmes 2>&1

# Build and install xa11y Python bindings
echo "Building xa11y Python bindings..."
cd "$PROJECT_ROOT/xa11y-python"
"$PIP" install --quiet -e .

cd "$PROJECT_ROOT"

# ── Run tests ────────────────────────────────────────────────────────

echo "Running Tauri integration tests..."
set +e
timeout 300 "$PYTEST" "$PROJECT_ROOT/tests/tauri/" -v -s --timeout=60 --rootdir="$PROJECT_ROOT" 2>&1
TEST_EXIT=$?
set -e

echo "=== Tauri integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
