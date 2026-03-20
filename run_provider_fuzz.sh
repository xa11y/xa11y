#!/usr/bin/env bash
# Cross-platform provider fuzzer harness for xa11y.
#
# Launches xa11y-test-app with platform-appropriate infrastructure,
# then runs the provider-fuzz binary to exercise all code paths
# with random operations.
#
# Works on macOS and Linux. On Linux, sets up Xvfb + D-Bus + AT-SPI2.
#
# Usage:
#   ./run_provider_fuzz.sh                          # random seed, 10k iterations
#   ./run_provider_fuzz.sh --seed 42 -n 50000       # reproducible, 50k iterations
#   FUZZ_SEED=42 FUZZ_ITERATIONS=5000 ./run_provider_fuzz.sh

set -euo pipefail

OS="$(uname -s)"
CLEANUP_PIDS=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "=== xa11y Provider Fuzzer ($OS) ==="

# ── Linux: set up D-Bus + Xvfb + AT-SPI2 ─────────────────────────────────────

if [ "$OS" = "Linux" ]; then
    # Re-exec under dbus-run-session if needed
    if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        echo "No D-Bus session found, re-launching under dbus-run-session..."
        exec dbus-run-session -- bash "$0" "$@"
    fi

    # Start Xvfb
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

    # AT-SPI2 infrastructure
    export NO_AT_BRIDGE=0
    export AT_SPI_CLIENT=true
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
fi

# ── Build ─────────────────────────────────────────────────────────────────────

echo "Building workspace..."
cargo build --workspace 2>&1

# ── Launch test app ───────────────────────────────────────────────────────────

echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app &
CLEANUP_PIDS+=($!)

WAIT_SECS=2
[ "$OS" = "Linux" ] && WAIT_SECS=3
echo "Waiting ${WAIT_SECS}s for test app to register..."
sleep $WAIT_SECS

# ── Build fuzzer args ─────────────────────────────────────────────────────────

FUZZ_ARGS=""
if [ -n "${FUZZ_SEED:-}" ]; then
    FUZZ_ARGS="$FUZZ_ARGS --seed $FUZZ_SEED"
fi
if [ -n "${FUZZ_ITERATIONS:-}" ]; then
    FUZZ_ARGS="$FUZZ_ARGS --iterations $FUZZ_ITERATIONS"
fi
FUZZ_ARGS="$FUZZ_ARGS $*"

# ── Run fuzzer ────────────────────────────────────────────────────────────────

echo "Running fuzzer..."
set +e
./target/debug/provider-fuzz $FUZZ_ARGS
FUZZ_EXIT=$?
set -e

echo "=== Fuzzer finished (exit code: $FUZZ_EXIT) ==="
exit $FUZZ_EXIT
