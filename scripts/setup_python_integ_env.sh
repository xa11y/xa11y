#!/usr/bin/env bash
# Shared Python venv bootstrap for the Linux integration-test runners.
#
# The Qt / GTK / Tauri suites used to each create their own `.venv-{qt,gtk,tauri}-test`
# and `pip install -e ./xa11y-python` separately — the first one paid a ~60s
# maturin build, the other two re-linked against the warm cargo cache for
# ~10s each. Worse, every invocation ran `pip install -e .` even when nothing
# had changed, re-invoking maturin through pip each time.
#
# This helper consolidates all three suites onto a single `.venv-test` venv
# and short-circuits the xa11y-python build when the package is already
# importable. On a fresh runner the first caller pays the full build; the
# second and third callers are no-ops.
#
# Usage: source scripts/setup_python_integ_env.sh [EXTRA_REQUIREMENTS_FILE ...]
#
# Sets PROJECT_ROOT, VENV_DIR, PIP, PYTHON, PYTEST on success (never exits —
# meant to be sourced into the caller's env).

SETUP_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SETUP_SCRIPT_DIR/.." && pwd)"
VENV_DIR="$PROJECT_ROOT/.venv-test"

if [ ! -d "$VENV_DIR" ]; then
    echo "Creating shared integ venv at $VENV_DIR..."
    python3 -m venv "$VENV_DIR"
fi

PIP="$VENV_DIR/bin/pip"
PYTHON="$VENV_DIR/bin/python"
PYTEST="$VENV_DIR/bin/pytest"

if [[ "$(uname)" == MINGW* ]] || [[ "$(uname)" == MSYS* ]] || [[ "$OSTYPE" == "msys" ]]; then
    PIP="$VENV_DIR/Scripts/pip.exe"
    PYTHON="$VENV_DIR/Scripts/python.exe"
    PYTEST="$VENV_DIR/Scripts/pytest.exe"
fi

# Core requirements: maturin (builds the PyO3 bindings) + shared test deps.
"$PIP" install --quiet maturin
"$PIP" install --quiet -r "$PROJECT_ROOT/tests/requirements.txt"

# Per-suite extras (e.g. PySide6, PyGObject) are passed as arguments.
for req in "$@"; do
    if [ -n "$req" ] && [ -f "$req" ]; then
        "$PIP" install --quiet -r "$req"
    fi
done

# xa11y-python is built with maturin via `pip install -e`. maturin reads the
# README referenced in pyproject.toml, and that README is gitignored (generated
# from the root by `cargo xtask sync-readmes`). Generate it unconditionally —
# it's cheap.
echo "Generating xa11y-python README..."
(cd "$PROJECT_ROOT" && cargo xtask sync-readmes >/dev/null 2>&1 || true)

# Skip the bindings rebuild if the compiled extension module is already
# importable. We check `xa11y._native` specifically (not just `xa11y`)
# because the repo root contains a `xa11y/` Rust crate directory that gets
# picked up by Python as an implicit namespace package — a bare
# `import xa11y` succeeds against it even when the PyO3 extension isn't
# installed.
#
# Within a single CI job, source doesn't change between Qt → GTK → Tauri
# steps, so the second and third callers get this for free.
if "$PYTHON" -c "import xa11y._native" 2>/dev/null; then
    echo "xa11y-python already installed in $VENV_DIR; skipping build."
else
    echo "Building xa11y-python and installing into $VENV_DIR..."
    (cd "$PROJECT_ROOT/xa11y-python" && "$PIP" install --quiet -e .)
fi
