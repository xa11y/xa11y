#!/usr/bin/env bash
# Integration test harness for xa11y Cocoa/AppKit tests (macOS only).
#
# Usage: ./scripts/run_cocoa_tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COCOA_APP_DIR="$PROJECT_ROOT/test-apps/cocoa"

if [[ "$(uname)" != "Darwin" ]]; then
    echo "Cocoa tests are macOS-only — skipping on $(uname)."
    exit 0
fi

echo "=== xa11y Cocoa/AppKit integration test harness ==="

# ── Build the Swift app if needed ────────────────────────────────────

BINARY="$COCOA_APP_DIR/xa11y-cocoa-test-app"
if [ ! -f "$BINARY" ]; then
    echo "Building Cocoa test app..."
    make -C "$COCOA_APP_DIR" build
fi

# ── Set up Python venv ───────────────────────────────────────────────

VENV_DIR="$PROJECT_ROOT/.venv-cocoa-test"

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

echo "Running Cocoa/AppKit integration tests..."
set +e
XA11Y_TEST_APP=cocoa "$PYTEST" "$PROJECT_ROOT/tests/suites/python/" -v -s --timeout=60 --rootdir="$PROJECT_ROOT" 2>&1
TEST_EXIT=$?
set -e

echo "=== Cocoa/AppKit integration tests finished (exit code: $TEST_EXIT) ==="
exit $TEST_EXIT
