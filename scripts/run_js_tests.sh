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

APP_LOG="$JS_DIR/.xa11y-test-app.log"
rm -f "$APP_LOG"
echo "Launching xa11y-test-app (log: $APP_LOG)..."
# Set RUST_BACKTRACE so if the test-app panics we get a useful trace,
# and force it to flush stdout.
RUST_BACKTRACE=1 \
    "$PROJECT_ROOT/target/debug/xa11y-test-app" --headless \
    >"$APP_LOG" 2>&1 &
APP_PID=$!
CLEANUP_PIDS+=("$APP_PID")

echo "Waiting for test app (pid=$APP_PID) to register with the a11y bus..."

# Wait up to 30s for the test app to become visible through the bindings
# before handing off to node --test. This gives a much clearer failure
# signal than letting the node test runner time out internally.
cd "$JS_DIR"
export XA11Y_TEST_APP_NAME=""
set +e
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        echo "!! xa11y-test-app (pid=$APP_PID) exited before registering with the a11y bus"
        break
    fi
    FOUND=$(node -e "
        const { App } = require('./index.js');
        App.list()
          .then((apps) => {
              const names = apps.map((a) => a.name).filter(Boolean);
              const hit = names.find((n) => n.toLowerCase().includes('xa11y'));
              if (hit) { console.log('FOUND:' + hit); process.exit(0); }
              console.error('visible apps: ' + JSON.stringify(names));
              process.exit(1);
          })
          .catch((err) => { console.error('App.list() threw: ' + err.message); process.exit(2); });
    " 2>&1)
    if echo "$FOUND" | grep -q '^FOUND:'; then
        XA11Y_TEST_APP_NAME="${FOUND#FOUND:}"
        echo "Test app registered: $XA11Y_TEST_APP_NAME"
        break
    fi
    echo "attempt $attempt/15: $FOUND"
    sleep 2
done
set -e

dump_diagnostics() {
    echo "=============================================================="
    echo "DIAGNOSTIC DUMP"
    echo "=============================================================="
    echo "--- xa11y-test-app process status ---"
    if kill -0 "$APP_PID" 2>/dev/null; then
        echo "pid=$APP_PID is still alive"
    else
        echo "pid=$APP_PID is NOT alive (exited)"
    fi
    echo ""
    echo "--- xa11y-test-app stdout/stderr ($APP_LOG) ---"
    if [ -f "$APP_LOG" ]; then
        cat "$APP_LOG" || true
    else
        echo "(no log file)"
    fi
    echo ""
    echo "--- ps (xa11y / at-spi) ---"
    ps -ef | grep -E "xa11y|at-spi" | grep -v grep || echo "(no matching processes)"
    echo ""
    echo "--- environment ---"
    echo "DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-}"
    echo "DISPLAY=${DISPLAY:-}"
    echo ""
    echo "--- DBus names on session bus ---"
    dbus-send --session --dest=org.freedesktop.DBus \
        --type=method_call --print-reply \
        /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1 \
        | grep -iE "a11y|xa11y|accessibility" || echo "(no a11y services visible)"
    echo ""
    echo "=============================================================="
}

if [ -z "$XA11Y_TEST_APP_NAME" ]; then
    echo "!! Test app never became visible through xa11y."
    DIAG_FILE="$JS_DIR/.failure-diagnostics.log"
    dump_diagnostics | tee "$DIAG_FILE"

    echo "::error title=Test app not discoverable::xa11y-test-app (pid=$APP_PID) did not register with AT-SPI within 30s. See the step for diagnostics."
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        {
            echo "## JS integration (Linux) — test app not discoverable"
            echo ""
            echo '```'
            cat "$DIAG_FILE"
            echo '```'
        } >>"$GITHUB_STEP_SUMMARY"
    fi
fi

# ── Run tests ────────────────────────────────────────────────────────

echo "Running JS integration tests..."
set +e
# Per-file timeout of 60s gives getApp() up to 30s of retry budget plus
# actual test work; overall 180s bounds the whole suite so CI never hangs.
# Capture the output so we can also surface it in the GitHub step summary
# when the raw logs aren't easily reachable.
NODE_TEST_LOG="$JS_DIR/.node-test-output.log"
timeout 180 node --test --test-timeout=60000 --test-reporter=spec \
    '__test__/integ/**/*.test.js' 2>&1 | tee "$NODE_TEST_LOG"
TEST_EXIT=${PIPESTATUS[0]}
set -e

if [ "$TEST_EXIT" -ne 0 ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "## JS integration (Linux) — test run failed (exit $TEST_EXIT)"
        echo ""
        echo '```'
        tail -200 "$NODE_TEST_LOG"
        echo '```'
    } >>"$GITHUB_STEP_SUMMARY"
    echo "::error title=JS integration tests failed::See the Run JS integration tests step (exit code $TEST_EXIT) for details."
fi

echo "=== JS integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
