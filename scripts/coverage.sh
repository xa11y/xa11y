#!/usr/bin/env bash
# Generate code coverage report for xa11y workspace.
#
# Requires: cargo-llvm-cov (installed automatically if missing)
# Output: coverage/index.html
#
# Usage: ./coverage.sh

set -euo pipefail

if ! command -v cargo-llvm-cov &>/dev/null; then
    echo "Installing cargo-llvm-cov..."
    cargo install cargo-llvm-cov
fi

echo "=== Generating coverage report ==="
cargo llvm-cov --workspace --html --branch --output-dir coverage/

echo ""
echo "Report: coverage/index.html"
echo "Open with: open coverage/index.html  (macOS)"
echo "           xdg-open coverage/index.html  (Linux)"
