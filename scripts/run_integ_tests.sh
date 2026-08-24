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
sleep 1

export DISPLAY="$XVFB_DISPLAY"
echo "DISPLAY=$DISPLAY"

# 2. Start AT-SPI2 (bus launcher + registryd) and flip the Status flags.
#    Single source of truth — shared with the per-app harness and the
#    setup-a11y CI action. The daemons it backgrounds live for the rest of
#    this dbus-run-session, which exits when the script does.
# shellcheck source=setup_linux_a11y.sh
source "$(cd "$(dirname "$0")" && pwd)/setup_linux_a11y.sh"

# 3. Build everything
echo "Building workspace..."
cargo build --workspace --features xa11y/strict-roles 2>&1

# Support BUILD_ONLY mode (for pre-warming the build cache)
if [ "${BUILD_ONLY:-}" = "1" ]; then
    echo "=== Build complete (build-only mode) ==="
    exit 0
fi

# 4. Launch the desktop panel fixture.
#
#    The Linux shell-surface classifier matches an AT-SPI frame carrying
#    `window-type:dock`, and a bare Xvfb display vends none — no desktop
#    environment runs here. Without this fixture the shell tests in
#    xa11y/tests/integ/shell.rs could only skip on an empty enumeration,
#    which is the coverage hole issue #383 is about, so the panel is a hard
#    requirement rather than a best-effort extra.
#
#    It needs a Python with the GTK 3 typelib. That is deliberately not
#    whichever `python3` is first on PATH: CI installs actions/setup-python,
#    whose interpreter has no PyGObject, while the system one does.
#
#    Its output goes to a log rather than the terminal: GTK 3's ATK bridge
#    emits a CRITICAL for every Text/Value probe against a widget that
#    implements neither, so a tree read from the panel would interleave a
#    dozen of them into the test output.
PANEL_PY=""
for candidate in "${XA11Y_PANEL_PYTHON:-}" /usr/bin/python3 /usr/bin/python3.12 python3; do
    [ -n "$candidate" ] || continue
    command -v "$candidate" >/dev/null 2>&1 || continue
    if "$candidate" -c 'import gi; gi.require_version("Gtk", "3.0"); from gi.repository import Gtk' \
        >/dev/null 2>&1; then
        PANEL_PY="$candidate"
        break
    fi
done
if [ -z "$PANEL_PY" ]; then
    echo "error: no Python with GTK 3 (PyGObject) found, so the desktop panel" >&2
    echo "       fixture cannot start and the shell-surface tests would have" >&2
    echo "       nothing to find." >&2
    echo "       Install it:  sudo apt-get install -y python3-gi gir1.2-gtk-3.0" >&2
    echo "       Or point at an interpreter that has it: XA11Y_PANEL_PYTHON=..." >&2
    echo "       In the container flow, an image built before the panel landed" >&2
    echo "       predates that package: rebuild it (<runtime> image rm xa11y-base)." >&2
    exit 1
fi
PANEL_LOG="target/xa11y-test-panel.log"
mkdir -p target
echo "Launching xa11y test panel ($PANEL_PY, log: $PANEL_LOG)..."
"$PANEL_PY" test-apps/panel/panel.py > "$PANEL_LOG" 2>&1 &
CLEANUP_PIDS+=($!)

# Verify the fixture is actually up before running anything against it. A
# panel that failed to start is a broken harness, and saying so here beats six
# shell tests failing with "no shell surfaces at all" and a log nobody prints:
# the first time this fixture ran on CI it died on a PyGObject version
# conflict, and the traceback was sitting in $PANEL_LOG unread.
echo "Waiting for the panel to register as a shell surface..."
PANEL_READY=0
for _ in $(seq 1 20); do
    if ./target/debug/xa11y shell 2>/dev/null | grep -q "^panel"; then
        PANEL_READY=1
        break
    fi
    sleep 1
done
if [ "$PANEL_READY" != "1" ]; then
    echo "error: the panel fixture never appeared as a shell surface." >&2
    echo "--- $PANEL_LOG ---" >&2
    cat "$PANEL_LOG" >&2 || true
    echo "--- xa11y shell ---" >&2
    ./target/debug/xa11y shell >&2 || true
    echo "--- xa11y apps ---" >&2
    ./target/debug/xa11y apps >&2 || true
    exit 1
fi
./target/debug/xa11y shell

# 5. Launch the test application (run binary directly, not via cargo run,
#    because cargo run changes the process owner name in AT-SPI)
echo "Launching xa11y-test-app..."
./target/debug/xa11y-test-app --headless &
CLEANUP_PIDS+=($!)

# Wait for the app to start and register with AT-SPI
echo "Waiting for test app to register with AT-SPI..."
sleep 3

# 6. Run integration tests
echo "Running integration tests..."
TEST_FILTER="${TEST_FILTER:-}"
set +e
NOCAPTURE_ARG=""
if [ "${INTEG_NOCAPTURE:-0}" = "1" ]; then
    NOCAPTURE_ARG="--nocapture"
fi
if [ -n "$TEST_FILTER" ]; then
    cargo test -p xa11y --features strict-roles --test integ_test -- --ignored --test-threads=1 $NOCAPTURE_ARG $TEST_FILTER 2>&1
else
    cargo test -p xa11y --features strict-roles --test integ_test -- --ignored --test-threads=1 $NOCAPTURE_ARG 2>&1
fi
TEST_EXIT=$?
set -e

echo "=== Integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
