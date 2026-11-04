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
# and short-circuits the xa11y-python build when the installed package was
# built from the current source (see the stamp below). On a fresh runner the
# first caller pays the full build; the second and third callers are no-ops.
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

# Skip the bindings rebuild only when the module that is installed was built
# from the source that is checked out now. "Importable" alone is not enough:
# after a rebase, a branch switch or an edit, `.venv-test` would keep serving
# the extension built first, and the suites would report the old code's
# results as the current tree's (#595).
#
# The stamp is a hash of the build's inputs by *content*, never by mtime — a
# rebase or checkout can leave mtimes where they were. The inputs are every
# path-dependency crate of xa11y-python (from `cargo metadata`, so a new
# backend crate is picked up without editing this list), the root
# Cargo.toml its members inherit from, and `.cargo/`. Each crate's `tests/`
# and the bindings' pure-Python sources are left out: neither is compiled
# into `_native`, and a test edit should not cost a rebuild.
#
# Within a single CI job the source doesn't change between suites, so the
# second and third callers still skip the build.
BINDINGS_STAMP_FILE="$VENV_DIR/.xa11y-python-build-stamp"

# Prints the stamp, or nothing (with the reason on stderr) when it cannot be
# computed — in which case the caller rebuilds rather than trusting the
# installed module.
xa11y_bindings_stamp() {
    local crate_dirs
    if ! crate_dirs="$(cd "$PROJECT_ROOT" && cargo metadata --format-version 1 \
            --manifest-path xa11y-python/Cargo.toml 2>/dev/null \
        | "$PYTHON" -c '
import json, os, sys
m = json.load(sys.stdin)
root = os.path.dirname(m["workspace_root"])
for p in m["packages"]:
    if p["source"] is None:
        d = os.path.relpath(os.path.dirname(p["manifest_path"]), root)
        print(d.replace(os.sep, "/"))
')" || [ -z "$crate_dirs" ]; then
        echo "Cannot list xa11y-python's path dependencies (cargo metadata failed)." >&2
        return 0
    fi

    # A read loop rather than `mapfile`: macOS ships bash 3.2.
    local -a pathspec=()
    local d
    while IFS= read -r d; do
        pathspec+=("$d")
    done <<<"$crate_dirs"
    pathspec+=(Cargo.toml .cargo
        ':(exclude,glob)**/tests/**'
        ':(exclude)xa11y-python/python')

    # Index blob ids (tracked + staged), the unstaged diff on top of them, and
    # the content of untracked, non-ignored files. Together these name the
    # working-tree content exactly, independent of HEAD and of mtimes.
    local state
    if ! state="$(cd "$PROJECT_ROOT" && {
            git ls-files -s -- "${pathspec[@]}" &&
            git diff --no-ext-diff --binary -- "${pathspec[@]}" &&
            git ls-files -o --exclude-standard -- "${pathspec[@]}" \
                | while IFS= read -r f; do
                    printf '%s %s\n' "$(git hash-object -- "$f")" "$f"
                done
        })"; then
        echo "Cannot read the source tree's state from git." >&2
        return 0
    fi
    # `git hash-object --stdin` is the one hash available on every runner
    # (macOS has no sha256sum, Git Bash has no shasum guarantee).
    printf '%s' "$state" | git hash-object --stdin
}

BINDINGS_STAMP="$(xa11y_bindings_stamp)"
INSTALLED_STAMP=""
if [ -f "$BINDINGS_STAMP_FILE" ]; then
    INSTALLED_STAMP="$(cat "$BINDINGS_STAMP_FILE")"
fi

# We check `xa11y._native` specifically (not just `xa11y`) because the repo
# root contains a `xa11y/` Rust crate directory that gets picked up by Python
# as an implicit namespace package — a bare `import xa11y` succeeds against it
# even when the PyO3 extension isn't installed.
if [ -n "$BINDINGS_STAMP" ] && [ "$BINDINGS_STAMP" = "$INSTALLED_STAMP" ] \
        && "$PYTHON" -c "import xa11y._native" 2>/dev/null; then
    echo "xa11y-python in $VENV_DIR is current for this source tree; skipping build."
else
    if [ -z "$BINDINGS_STAMP" ]; then
        echo "Cannot tell whether the installed xa11y-python is current; rebuilding."
    elif [ -n "$INSTALLED_STAMP" ]; then
        echo "Rust sources changed since xa11y-python was last built; rebuilding."
    fi
    echo "Building xa11y-python and installing into $VENV_DIR..."
    rm -f "$BINDINGS_STAMP_FILE"
    # A plain command, not an `if` condition: the caller's `set -e` must
    # abort on a failed build, or the suites would run against whatever
    # `_native` the failed build left behind.
    (cd "$PROJECT_ROOT/xa11y-python" && "$PIP" install --quiet -e .)
    if [ -n "$BINDINGS_STAMP" ]; then
        printf '%s\n' "$BINDINGS_STAMP" > "$BINDINGS_STAMP_FILE"
    fi
fi

# pytest-xa11y supplies the app fixtures the python and cli suites use. It is
# pure Python and installs in a second, so it is installed unconditionally
# rather than behind an importability check like the bindings above — that
# check exists to skip a Rust rebuild, and there is no rebuild to skip here.
echo "Installing pytest-xa11y into $VENV_DIR..."
"$PIP" install --quiet -e "$PROJECT_ROOT/pytest-xa11y"
