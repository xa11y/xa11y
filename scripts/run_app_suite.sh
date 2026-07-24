#!/usr/bin/env bash
# Unified local integration-test runner for one test app.
#
# Brings up the Linux accessibility environment (Xvfb + D-Bus + AT-SPI, and a
# window manager where the toolkit needs focus), builds the requested test app
# and bindings, sets up the shared Python venv, then hands off to the shared
# harness `tests/harness/launch.py` — the SAME entry point CI uses. This keeps
# `cargo xtask test-<app>` and CI on a single code path instead of the old
# per-app run_<app>_tests.sh scripts that ran pytest directly.
#
# Usage:
#   scripts/run_app_suite.sh <app> [suite ...]
#
#   <app>    qt | gtk | cocoa | tauri | electron | accesskit | egui | winforms
#   [suite]  python | js | cli   (default: python js cli, matching CI)
#
# Notes:
#   * Running the js suite needs Node + the xa11y-js bindings; the cli suite
#     needs the xa11y CLI binary. Both are built on demand below. Pass an
#     explicit suite list (e.g. `... tauri python`) to skip toolchains you
#     don't have locally.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

APP="${1:-}"
if [ -z "$APP" ]; then
    echo "usage: $0 <app> [suite ...]" >&2
    exit 2
fi
shift || true

SUITES=("$@")
if [ ${#SUITES[@]} -eq 0 ]; then
    SUITES=(python js cli)
fi

case "$APP" in
    qt|gtk|cocoa|tauri|electron|accesskit|egui|winforms) ;;
    *) echo "Unknown app: $APP (qt|gtk|cocoa|tauri|electron|accesskit|egui|winforms)" >&2; exit 2 ;;
esac

has_suite() {
    local want="$1"
    for s in "${SUITES[@]}"; do [ "$s" = "$want" ] && return 0; done
    return 1
}

echo "=== xa11y integration harness: app=$APP suites=${SUITES[*]} ==="

# ── Linux: display + window manager + AT-SPI ──────────────────────────
# cocoa is macOS-only; electron is Linux-only; winforms is Windows-only.
# Everything else is cross-OS.
if [ "$APP" = "cocoa" ] && [ "$(uname)" != "Darwin" ]; then
    echo "Cocoa tests are macOS-only — skipping on $(uname)."
    exit 0
fi
# uname reports MINGW*/MSYS*/CYGWIN* under the Git-Bash shells this script runs
# in on Windows.
case "$(uname)" in
    MINGW*|MSYS*|CYGWIN*) IS_WINDOWS=1 ;;
    *) IS_WINDOWS=0 ;;
esac
if [ "$APP" = "winforms" ] && [ "$IS_WINDOWS" = "0" ]; then
    echo "WinForms tests are Windows-only — skipping on $(uname)."
    exit 0
fi
if [ "$APP" = "electron" ] && [ "$(uname)" != "Linux" ]; then
    echo "Electron a11y tests only run on Linux — skipping on $(uname)."
    exit 0
fi

CLEANUP_PIDS=()
cleanup() {
    for pid in "${CLEANUP_PIDS[@]:-}"; do
        [ -n "$pid" ] || continue
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

if [ "$(uname)" = "Linux" ]; then
    # Re-exec under a D-Bus session if we don't have one yet.
    if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        echo "No D-Bus session found, re-launching under dbus-run-session..."
        exec dbus-run-session -- bash "$0" "$APP" "${SUITES[@]}"
    fi

    # Start Xvfb if there's no display.
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
        CLEANUP_PIDS+=("$!")
        sleep 1
        export DISPLAY="$XVFB_DISPLAY"
    fi
    echo "DISPLAY=$DISPLAY"

    # egui and Tauri need a window manager: egui's AccessKit bridge only
    # publishes its tree once the window is focused, and Tauri's input-sim
    # tests need focus routing so synthesised events reach the webview.
    if [ "$APP" = "egui" ] || [ "$APP" = "tauri" ]; then
        if command -v fluxbox >/dev/null 2>&1; then
            echo "Starting fluxbox (focus routing under Xvfb)..."
            fluxbox >/dev/null 2>&1 &
            CLEANUP_PIDS+=("$!")
            sleep 1
        else
            echo "WARNING: fluxbox not installed; $APP tree/input-sim may not work."
            echo "         Install with: sudo apt-get install -y fluxbox"
            [ "$APP" = "tauri" ] && export XA11Y_SKIP_INPUT_SIM=1
        fi
    fi

    # AT-SPI bring-up (shared single source of truth).
    # shellcheck source=setup_linux_a11y.sh
    source "$SCRIPT_DIR/setup_linux_a11y.sh"
fi

# ── Build the test app ────────────────────────────────────────────────
cd "$PROJECT_ROOT"
case "$APP" in
    tauri)
        echo "Building Tauri test app..."
        cargo build --manifest-path test-apps/tauri/Cargo.toml
        ;;
    egui)
        echo "Building egui test app..."
        cargo build --manifest-path test-apps/egui/Cargo.toml
        ;;
    accesskit)
        echo "Building AccessKit test app..."
        cargo build -p xa11y-test-app
        ;;
    cocoa)
        if [ ! -f test-apps/cocoa/xa11y-cocoa-test-app ]; then
            echo "Building Cocoa test app..."
            make -C test-apps/cocoa build
        fi
        ;;
    winforms)
        echo "Building WinForms test app..."
        dotnet build test-apps/winforms
        ;;
    electron)
        if [ ! -x test-apps/electron/node_modules/.bin/electron ]; then
            echo "Installing Electron..."
            (cd test-apps/electron && npm install --no-audit --no-fund --silent)
        fi
        ;;
    qt|gtk)
        : # Python apps — dependencies handled by the venv below.
        ;;
esac

# ── CLI binary (cli suite) ────────────────────────────────────────────
if has_suite cli; then
    echo "Building xa11y CLI..."
    cargo build -p xa11y
fi

# ── xa11y-js bindings (js suite) ──────────────────────────────────────
if has_suite js; then
    echo "Building xa11y-js bindings..."
    (
        cd xa11y-js
        [ -d node_modules ] || npm ci
        npx napi build --platform --js native.js --dts native.d.ts
        node scripts/patch-native-dts.mjs
    )
fi

# ── Python venv + bindings (python/cli suites use pytest) ─────────────
# setup_python_integ_env.sh creates .venv-test, installs maturin + shared
# test deps + any per-app requirements, and builds xa11y-python once.
APP_REQS=()
case "$APP" in
    qt)  APP_REQS=("$PROJECT_ROOT/test-apps/qt/requirements.txt") ;;
    gtk) APP_REQS=("$PROJECT_ROOT/test-apps/gtk/requirements.txt") ;;
esac
# shellcheck source=setup_python_integ_env.sh
source "$SCRIPT_DIR/setup_python_integ_env.sh" "${APP_REQS[@]:-}"

# ── Hand off to the shared harness ────────────────────────────────────
# Don't exec — let the EXIT trap tear down Xvfb/fluxbox afterwards.
cd "$PROJECT_ROOT"
echo "Launching shared harness for $APP..."
set +e
"$PYTHON" tests/harness/launch.py "$APP" "${SUITES[@]}"
HARNESS_EXIT=$?
set -e
echo "=== $APP harness finished (exit code: $HARNESS_EXIT) ==="
exit $HARNESS_EXIT
