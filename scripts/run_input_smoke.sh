#!/usr/bin/env bash
# Run the xa11y-linux input-smoke tests (XTest-driven) under Xvfb inside the
# Linux integ container. Verifies the X11 input backend end-to-end.
set -euo pipefail

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    exec dbus-run-session -- bash "$0" "$@"
fi

CLEANUP_PIDS=()
cleanup() {
    for pid in "${CLEANUP_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

Xvfb :99 -screen 0 1280x1024x24 -ac &
CLEANUP_PIDS+=($!)
sleep 1
export DISPLAY=:99

cd /xa11y
cargo test -p xa11y-linux --test input_smoke -- --ignored --test-threads=1 --nocapture 2>&1
