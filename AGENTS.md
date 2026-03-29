# Agent Guidelines

## Integration Test Coverage

The goal is **full coverage** of the public API surface through real integration tests.

When adding new tests:

1. If the test app (`xa11y-test-app`) lacks a widget needed for a test, add it to the test app first. The test app uses AccessKit + winit and is defined in `xa11y-test-app/src/main.rs`.
2. All integration tests must be `#[ignore]` and run via `cargo xtask test-integ`.
3. Run `cargo xtask test-integ` to verify tests pass before committing.

### Test helpers

Integration tests use shared helpers from `xa11y/tests/integ/mod.rs`:
- `h::app_tree()` — get test app root Node with retries
- `h::one(root, "selector")` — find exactly one node by selector
- `h::named(root, "Submit")` — find node by name substring
- `h::act(node, action)` — perform action, wait, re-read tree, return new root

### Key coverage gaps to address

- **EventProvider** — no tests at all (not yet implemented for any provider)
- **macOS integration tests** — blocked on `xa11y-macos` provider implementation

## Design Tenets

1. **No silent fallbacks.** If an operation fails, return the error — don't silently try a different mechanism. Fallbacks hide bugs and make behavior unpredictable for consumers. Surface failures clearly so callers can handle them.

2. **Only expose what accessibility APIs support.** If a platform has no accessibility interface for an operation, don't implement it with input simulation — leave it out.

## Pre-Commit / Pre-PR Checklist

Run `cargo xtask check` to run all pre-PR checks in one command. It covers formatting, linting, unit tests, and Python bindings.

CI runs with `RUSTFLAGS: -Dwarnings`, so all warnings are errors. Individual checks:

1. **Formatting** — `cargo xtask fmt` (use `cargo xtask fmt --check` to verify without modifying)
2. **Lint** — `cargo xtask lint` (clippy + ruff check + Python Rust check)
3. **Unit tests** — `cargo xtask test`
4. **Integration tests** (if touching provider/test-app code) — `cargo xtask test-integ`
5. **Python bindings** — `cargo xtask test-python`
6. **No new `#[allow(...)]` without justification** — if you must suppress a warning, add a comment explaining why

Common CI failures:
- `unused import` / `dead_code` — remove the unused code or add `#[allow(dead_code)]` with a reason
- Formatting diffs — run `cargo xtask fmt`
- Platform stubs (`xa11y-macos` on Linux, `xa11y-linux` on macOS) — make sure stub modules compile cleanly on all platforms
- Python binding failures — `xa11y-python` is **not** in the Cargo workspace, so workspace-wide commands skip it. `cargo xtask lint` and `cargo xtask test-python` handle this automatically.

## Running Tests

```bash
# All pre-PR checks (fmt, lint, test, test-python)
cargo xtask check

# Individual commands
cargo xtask fmt                               # format Rust + Python
cargo xtask fmt --check                       # check without modifying
cargo xtask lint                              # clippy + ruff + Python Rust check
cargo xtask test                              # unit tests
cargo xtask test-python                       # build + test Python bindings
cargo xtask test-integ                        # integration tests (auto-detects OS)
cargo xtask test-integ-container              # Linux integration tests via Finch
cargo xtask test-integ-container tree_has_buttons  # single test in container
cargo xtask fuzz                              # provider fuzzer
cargo xtask fuzz --seed 42 -n 5000            # reproducible fuzz run
cargo xtask coverage                          # code coverage report
cargo xtask docs                              # build documentation

# Core fuzz tests (requires nightly)
cd xa11y-fuzz/fuzz && cargo +nightly fuzz run tree_ops -- -max_total_time=60
```

## Project Structure

- `xa11y-core/` — Platform-independent types, traits, selector engine
- `xa11y-linux/` — AT-SPI2 backend via zbus
- `xa11y-macos/` — macOS backend (AXUIElement, with ObjC exception safety)
- `xa11y-windows/` — Windows backend (stub)
- `xa11y/` — Umbrella crate, unit tests, integration tests
- `xa11y-test-app/` — AccessKit + winit app used as target for integration tests
- `xa11y-python/` — Python bindings via PyO3/maturin (excluded from Cargo workspace)
- `xa11y-fuzz/` — Fuzz targets for xa11y-core (tree, selector, serde) and macOS platform fuzzer
- `xtask/` — Development workflow commands (`cargo xtask <command>`)
- `scripts/` — Shell scripts for integration tests, fuzzing, coverage
- `docs/` — Documentation site and generation scripts
