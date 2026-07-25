# xa11y Test Organization

What is tested where, how to run it, and how to read the coverage matrix.

---

## Key Design Decisions

### Rust/AccessKit tests stay in `xa11y/tests/integ/`

These are the fast-path core validation suite. They test the xa11y library itself against
the AccessKit test app (a purpose-built winit + AccessKit app). They run with
`cargo xtask test-integ` and are entirely separate from the per-app compatibility matrix
below. They cover: tree traversal (`tree.rs`), actions (`actions.rs`), events
(`events_linux.rs` / `events_macos.rs` / `events_windows.rs`), and screenshot capture
(`screenshot.rs`).

### Input sim + screenshot are tested once per platform, not per-app

Input simulation and screenshot test the platform's input/screenshot APIs, not
accessibility-framework compatibility. Testing them against every app would be redundant.
The vehicle is the Tauri app (it runs on Linux, macOS, and Windows), which also has a
dedicated event-log page for verifying synthesised pointer and keyboard events end-to-end.

The JS integ suite additionally has smoke tests for input_sim and screenshot; they gate
themselves to the Tauri and Electron apps (see the top of
`tests/suites/js/03_input_sim.test.js` and `04_screenshot.test.js`) and complement — not
duplicate — the Tauri Python tests.

### Per-app compat tests verify a11y API surface compatibility

These tests (find, tree navigation, roles, widget discovery, actions, events) confirm that
each framework's accessibility API works correctly end-to-end with xa11y. They are written
in Python (primary), JS, and CLI (`tests/suites/cli/`, which drives the `xa11y`
binary against the live app).

---

## Coverage Matrix

The coverage index lives in [`matrix.yaml`](matrix.yaml) and is rendered — and
validated — by:

```bash
python tests/matrix_check.py     # or: cargo xtask test-matrix-check
```

That command prints the per-app × per-language matrix, the documented gaps, and
the platform exclusions, then checks two things CI enforces: that every empty
cell has a matching documented gap, and that every claimed `(app, language,
feature)` maps to a test file that actually exists.

This README deliberately does **not** duplicate the table. It used to, and the
copy rotted — it was still listing a `cli_integ` gap that no longer existed and
omitting three test apps that had since landed.

> **A green matrix cell is not proof a suite ran.** `matrix_check.py` validates
> claims against *test files on disk*, not against execution. What guarantees
> execution is the harness: `tests/harness/launch.py` fails the run when a
> requested suite cannot start, and prints a per-suite ledger at the end of
> every cell saying whether each suite ran, was deliberately skipped
> (`declared_suite_skips`), or did not run. See issue #327 for the four Windows
> cells that claimed CLI coverage they had never once executed.

---

## How to Run

```bash
# All Python integration test suites (all apps)
cargo xtask test-apps

# Rust core suite (AccessKit app, fast-path)
cargo xtask test-integ

# Per-app suites. Each launches the app once and runs the python, js and cli
# suites against it — the same tests/harness/launch.py entry point CI uses.
# Pass an explicit suite list to narrow it, e.g. `cargo xtask test-qt python`.
cargo xtask test-qt          # Qt/PySide6
cargo xtask test-gtk         # GTK4
cargo xtask test-cocoa       # Cocoa/AppKit (macOS only)
cargo xtask test-tauri       # Tauri
cargo xtask test-electron    # Electron (Linux only)
cargo xtask test-egui        # egui/eframe
cargo xtask test-winforms    # WinForms (Windows only)
cargo xtask test-wpf         # WPF (Windows only)

# Unit-test the harness itself (no app launched)
cargo xtask test-harness

# Linux integration tests via container
cargo xtask test-integ-container

# All pre-PR checks (fmt, lint, unit tests, Python bindings, harness)
cargo xtask check
```

The `cli` suite needs the `xa11y` binary (`cargo build -p xa11y`) and the `js`
suite needs the built bindings; `scripts/run_app_suite.sh` builds both on
demand. If the CLI binary is missing the harness **fails** rather than skipping
— see the note under Coverage Matrix.

CI configuration: see `.github/workflows/ci.yml`.

---

## Test Layout

The per-app suites are app-agnostic: one set of tests per language, parameterised
by `XA11Y_TEST_APP`, rather than a directory per framework.

```
tests/
  README.md            <- this file
  matrix.yaml          <- machine-readable coverage matrix
  matrix_check.py      <- CI validator (prints coverage summary; gaps must be documented)
  helpers.py           <- shared Python launch helpers (launch_test_app fixture)
  harness/
    launch.py          <- THE entry point: launches an app once, runs the
                          requested suites against it, audits what actually ran
    test_launch.py     <- unit tests for the harness (cargo xtask test-harness)
  suites/
    python/            <- Python integ suite (compat, actions, events, errors,
                          input_sim, screenshot, foreground)
    js/                <- JS integ suite (numbered NN_<feature>.test.js)
    cli/               <- CLI integ suite — drives the `xa11y` binary
                          (tree, find, actions, input_sim, screenshot)

xa11y/tests/integ/     <- Rust core suite (AccessKit app, fast-path)
  mod.rs               <- shared helpers (app_tree, one, named, act)
  tree.rs              <- tree traversal + find
  actions.rs           <- press, toggle, focus, expand/collapse
  errors.rs            <- error paths
  multi_window.rs      <- multi-window traversal
  events_linux.rs      <- AT-SPI2 event subscription
  events_macos.rs      <- AX notification event subscription
  events_windows.rs    <- UIA event subscription
  screenshot.rs        <- screenshot capture

xa11y-js/__test__/
  unit/                <- JS unit tests (no live app)
  types/               <- TypeScript type tests

xa11y-python/tests/    <- Python unit tests + CLI unit tests
  test_cli.py          <- CLI error-path unit tests (no live app)
  test_element.py
  test_locator.py
  ...
```
