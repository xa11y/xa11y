#!/usr/bin/env bash
# Run the xa11y-linux Wayland smoke tests (session-detection + portal branch
# selection) inside the container. The detection tests just flip env vars;
# the full portal path requires weston + xdg-desktop-portal which this script
# does NOT start — that coverage lives in run_wayland_portal.sh.
set -euo pipefail

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    exec dbus-run-session -- bash "$0" "$@"
fi

cd /xa11y
cargo test -p xa11y-linux --test wayland_smoke -- --ignored --test-threads=1 --nocapture 2>&1
