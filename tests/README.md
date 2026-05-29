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

The JS integ suite additionally has smoke tests for input_sim and screenshot against the
AccessKit app; these complement (not duplicate) the Tauri Python tests.

### Per-app compat tests verify a11y API surface compatibility

These tests (find, tree navigation, roles, widget discovery, actions, events) confirm that
each framework's accessibility API works correctly end-to-end with xa11y. They are written
in Python (primary) and JS (partial coverage). CLI integration tests do not yet exist —
see Known Gaps.

---

## Coverage Matrix

`✅` = covered | `❌` = not covered (gap) | `⚠️` = partial or platform-limited

| App        | Platform(s)           | Python compat | Python actions | Python events | Python input_sim | Python screenshot | JS compat | JS actions | JS input_sim | JS screenshot | CLI |
|------------|-----------------------|:-------------:|:--------------:|:-------------:|:----------------:|:-----------------:|:---------:|:----------:|:------------:|:-------------:|:---:|
| accesskit  | linux, macos, windows | ✅¹           | ✅¹            | ⚠️¹           | —                | —                 | ✅        | ✅         | ✅²          | ✅²           | ❌  |
| qt         | linux, macos, windows | ✅            | ✅             | ✅            | —                | —                 | ❌        | ❌         | —            | —             | ❌  |
| gtk        | linux                 | ✅            | ✅             | ❌³           | —                | —                 | ❌        | ❌         | —            | —             | ❌  |
| cocoa      | macos                 | ✅            | ✅             | ✅            | —                | —                 | ❌        | ❌         | —            | —             | ❌  |
| tauri      | linux, macos, windows | ✅            | ✅             | ✅            | ✅               | ✅                | ❌        | ❌         | —            | —             | ❌  |
| electron   | linux                 | ❌            | ❌             | ❌            | —                | —                 | ✅        | ✅         | ✅²          | ✅²           | ❌  |

**Notes:**
1. The accesskit Python compat/actions suites run **on Linux only** (the harness gates this on `sys.platform`). Linux is where AccessKit's AT-SPI bridge — and its `"click"`-not-`"toggle"` action naming, which the toggle()-via-press fallback in `xa11y-linux/src/atspi.rs` depends on — is exercised. The events module also runs but its assertions are `xfail`-guarded (⚠️). On macOS/Windows the Rust integ suite in `xa11y/tests/integ/` stays the canonical AccessKit coverage, so Python is not duplicated there.
2. JS input_sim and screenshot run against the AccessKit app for the `integ` suite, and against Electron for the `integ-electron` suite.
3. GTK has no event subscription tests. Only widget/compat and actions are covered.

---

## Known Gaps

| ID              | Description                                                                                  | Severity | Workaround                                               |
|-----------------|----------------------------------------------------------------------------------------------|:--------:|----------------------------------------------------------|
| `cli_integ`     | No CLI integration tests exist for any app. The `xa11y` binary is not exercised end-to-end against a live app in CI. | **high** | Unit tests in `xa11y-python/tests/test_cli.py` cover CLI error paths only. |
| `js_app_coverage` | JS integ tests cover only the AccessKit app and Electron. No JS tests for Qt, GTK, Cocoa, or Tauri. | medium | Python suites cover those apps.                   |
| `gtk_events`    | No event subscription tests for GTK. Only compat and actions are covered.                    | low      | GTK event subscription is exercised in the Rust integ suite via AT-SPI2. |

---

## How to Run

```bash
# All Python integration test suites (all apps)
cargo xtask test-apps

# Rust core suite (AccessKit app, fast-path)
cargo xtask test-integ

# Per-framework Python suites
cargo xtask test-qt          # Qt/PySide6
cargo xtask test-gtk         # GTK4
cargo xtask test-cocoa       # Cocoa/AppKit (macOS only)
cargo xtask test-tauri       # Tauri

# JS integration tests (AccessKit app)
cd xa11y-js && node --test __test__/integ/

# JS Electron integration tests
cd xa11y-js && node --test __test__/integ-electron/

# Linux integration tests via container
cargo xtask test-integ-container

# All pre-PR checks (fmt, lint, unit tests, Python bindings)
cargo xtask check
```

CI configuration: see `.github/workflows/ci.yml`.

---

## Test Layout

```
tests/
  README.md            <- this file
  matrix.yaml          <- machine-readable coverage matrix
  matrix_check.py      <- CI validator (prints coverage summary; gaps must be documented)
  helpers.py           <- shared Python launch helpers (launch_test_app fixture)
  qt/                  <- Qt/PySide6 Python integ tests
    conftest.py
    test_01_widgets.py     <- compat + actions
    test_02_events.py      <- event subscription
    test_03_a11y_tree.py   <- tree structure
    test_04_widgets_extra.py
  gtk/                 <- GTK4 Python integ tests
    conftest.py
    test_widgets.py        <- compat + actions (no events yet)
  cocoa/               <- Cocoa/AppKit Python integ tests (macOS only)
    conftest.py
    test_01_widgets.py     <- compat + actions
    test_02_events.py      <- event subscription
    test_03_widgets_extra.py
  tauri/               <- Tauri Python integ tests (all platforms)
    conftest.py
    test_01_widgets.py     <- compat + actions
    test_02_events.py      <- event subscription
    test_03_widgets_extra.py
    test_input_sim.py      <- input simulation (one-per-platform)
    test_screenshot.py     <- screenshot capture (one-per-platform)

xa11y/tests/integ/     <- Rust core suite (AccessKit app, fast-path)
  mod.rs               <- shared helpers (app_tree, one, named, act)
  tree.rs              <- tree traversal + find
  actions.rs           <- press, toggle, focus, expand/collapse
  events_linux.rs      <- AT-SPI2 event subscription
  events_macos.rs      <- AX notification event subscription
  events_windows.rs    <- UIA event subscription
  screenshot.rs        <- screenshot capture

xa11y-js/__test__/
  integ/               <- JS integ tests (AccessKit app)
    01_tree.test.js    <- compat
    02_actions.test.js <- actions
    03_input_sim.test.js
    04_screenshot.test.js
  integ-electron/      <- JS Electron integ tests
    electron_a11y.test.js  <- compat + AccessibilityNotEnabled detection
  unit/                <- JS unit tests (no live app)

xa11y-python/tests/    <- Python unit tests + CLI unit tests
  test_cli.py          <- CLI error-path unit tests (no live app)
  test_element.py
  test_locator.py
  ...
```
