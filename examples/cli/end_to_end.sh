#!/usr/bin/env bash
# End-to-end xa11y CLI example: drive the AccessKit test app from the shell.
#
# This script is a complete, copy-pasteable starting point for using the
# `xa11y` CLI as a debugging or scripting tool. It targets the AccessKit
# test app shipped with this repo (`test-apps/accesskit`) so it runs
# identically on Linux, macOS, and Windows (Git Bash).
#
# What it demonstrates:
#
#   * Listing accessible apps with `xa11y apps`.
#   * Dumping the tree with `xa11y tree` to discover selectors.
#   * Finding elements with `xa11y find` (pretty / bounds / center output).
#   * Dispatching actions with `xa11y action` (press, set_value).
#
# Run from the repo root, after building:
#
#   cargo build -p xa11y-test-app -p xa11y
#   bash examples/cli/end_to_end.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Locate the test-app binary (handle Windows .exe).
if [ -x "$REPO_ROOT/target/debug/xa11y-test-app" ]; then
    APP_BIN="$REPO_ROOT/target/debug/xa11y-test-app"
elif [ -x "$REPO_ROOT/target/debug/xa11y-test-app.exe" ]; then
    APP_BIN="$REPO_ROOT/target/debug/xa11y-test-app.exe"
else
    echo "Build the test app first: cargo build -p xa11y-test-app" >&2
    exit 1
fi

# Locate the xa11y CLI binary.
if [ -x "$REPO_ROOT/target/debug/xa11y" ]; then
    CLI="$REPO_ROOT/target/debug/xa11y"
elif [ -x "$REPO_ROOT/target/debug/xa11y.exe" ]; then
    CLI="$REPO_ROOT/target/debug/xa11y.exe"
else
    echo "Build the CLI first: cargo build -p xa11y" >&2
    exit 1
fi

# 1. Launch the test app in the background. We capture $! as a best-effort
#    cleanup target, but on Git Bash for Windows that's the subshell pid, not
#    the .exe's pid — so we re-discover the real pid via `xa11y apps` below.
"$APP_BIN" >/dev/null 2>&1 &
LAUNCHER_PID=$!
APP_PID=""
cleanup() {
    [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null || true
    kill "$LAUNCHER_PID" 2>/dev/null || true
    wait "$LAUNCHER_PID" 2>/dev/null || true
}
trap cleanup EXIT

# 2. Poll `xa11y apps` for the test app by name. The Linux/macOS process name
#    is "xa11y-test-app"; the Windows UIA window title is "xa11y Test App".
DEADLINE=$(( $(date +%s) + 30 ))
while :; do
    APP_PID=$("$CLI" apps 2>/dev/null | awk -F'\t' '
        $2 == "xa11y-test-app" || $2 == "xa11y Test App" { print $1; exit }
    ')
    if [ -n "$APP_PID" ]; then
        break
    fi
    if [ "$(date +%s)" -ge "$DEADLINE" ]; then
        echo "Test app did not register within 30s" >&2
        echo "--- xa11y apps ---" >&2
        "$CLI" apps >&2 || true
        exit 1
    fi
    sleep 0.2
done

echo "App registered (pid=$APP_PID)"

# 3. Dump the tree once for discovery. In a real session you'd inspect it
#    visually to find the role and name of the element you want to drive.
echo
echo "--- xa11y tree --pid $APP_PID (first 30 lines) ---"
"$CLI" tree --pid "$APP_PID" | sed -n '1,30p'

# 4. Find: list every button. -o pretty (default) prints role/name; -o bounds
#    prints `X,Y,W,H` rectangles ready to pass to `xa11y click --at`.
echo
echo "--- xa11y find 'button' --pid $APP_PID ---"
"$CLI" find 'button' --pid "$APP_PID"

# 5. Action: press the Submit button. Asserts a clean exit (the test app
#    rebuilds its tree on action — the action result lands in subsequent
#    queries).
"$CLI" action press 'button[name="Submit"]' --pid "$APP_PID"
echo "press OK"

# 6. Action: set the value of a text field. --value supplies the new content.
"$CLI" action set_value 'text_field[name="Name"]' --pid "$APP_PID" --value "Ada Lovelace"
echo "set_value OK"

# 7. Action: toggle the checkbox via the `press` semantic verb. (Toggle on
#    web/AT-SPI checkboxes is exposed as press on this widget.)
"$CLI" action press 'check_box[name="I agree to terms"]' --pid "$APP_PID"
echo "toggle OK"

# 8. Find again — Submit's bounds in screen pixels, in the format the input
#    simulation subcommands (`xa11y click`) accept.
echo
echo "--- bounds of Submit ---"
"$CLI" find 'button[name="Submit"]' --pid "$APP_PID" -o bounds

echo
echo "OK — example completed successfully."
