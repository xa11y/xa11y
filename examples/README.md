# Examples

Complete, copy-pasteable end-to-end examples for each binding. Each example
launches the in-repo AccessKit test app (`test-apps/accesskit`), discovers
elements with a tree dump, drives the UI through the public API, and tears
the subprocess down. Same flow in every language so they can be read
side-by-side.

| Language   | File                                    | Run command                                                   |
|------------|-----------------------------------------|---------------------------------------------------------------|
| Rust       | [`rust/src/main.rs`](rust/src/main.rs)  | `cargo run -p xa11y-example-end-to-end`                       |
| Python     | [`python/end_to_end.py`](python/end_to_end.py) | `python examples/python/end_to_end.py`                  |
| JavaScript | [`js/end_to_end.mjs`](js/end_to_end.mjs) | `node examples/js/end_to_end.mjs`                            |
| CLI        | [`cli/end_to_end.sh`](cli/end_to_end.sh) | `bash examples/cli/end_to_end.sh`                            |

## Prerequisites

All examples need the AccessKit test app built:

```bash
cargo build -p xa11y-test-app
```

The CLI example additionally needs the `xa11y` CLI binary:

```bash
cargo build -p xa11y
```

The Python example needs the bindings installed (`pip install -e xa11y-python`),
the JS example needs the bindings built (`cd xa11y-js && npm install && npm run
build:debug`).

Platform setup (the examples assume these are already done by the operator):

- **macOS**: the interpreter binary (Python / Node / Cargo) needs
  *Accessibility* permission granted under System Settings → Privacy & Security.
- **Linux**: an X (or Xvfb) display and an AT-SPI registry must be running.
- **Windows**: no extra setup; UIA is always available to a desktop session.

## Coverage

Each example exercises:

1. `App::by_pid` (or `App.byPid`) with a poll loop until the OS registers the
   subprocess.
2. `App::dump` to discover element roles and names — selectors come from
   reading this output, not guessing.
3. `Locator` with selector strings + auto-waiting actions.
4. `wait_visible` and `wait_until` (the alternatives to `sleep`).
5. Reading `Element` fields: `role`, `name`, `actions`, `enabled`, `checked`.
6. Actions: `press`, `set_value`, `press`-as-toggle.
7. Iteration over multiple matches via `.elements()`.
8. Event subscription with `Subscription.wait_for`.
9. Subprocess teardown via a panic/exception-safe guard.

CI runs all four examples against the AccessKit test app on Linux, macOS, and
Windows (see the `examples` job in `.github/workflows/ci.yml`) so they
cannot rot.
