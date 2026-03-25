# Agent Guidelines

## Integration Test Coverage

Before adding or modifying integration tests, read [`xa11y/tests/INTEG_COVERAGE.md`](xa11y/tests/INTEG_COVERAGE.md) for the current coverage analysis. The goal is **full coverage** of the public API surface through real integration tests.

When adding new tests:

1. Check the coverage file to identify gaps — prioritize uncovered areas.
2. If the test app (`xa11y-test-app`) lacks a widget needed for a test, add it to the test app first. The test app uses AccessKit + winit and is defined in `xa11y-test-app/src/main.rs`.
3. All integration tests must be `#[ignore]` and run via `./run_integ_tests.sh` (Linux) or `./run_integ_tests_macos.sh` (macOS).
4. After adding tests, update `INTEG_COVERAGE.md` to reflect the new coverage.
5. Run `./run_integ_tests.sh` to verify tests pass before committing.

### Test helpers

Integration tests use shared helpers from `xa11y/tests/integ/mod.rs`:
- `h::provider()` — create platform provider
- `h::app_tree(p)` — get test app tree with retries
- `h::raw_tree(p)` — get tree with `include_raw: true`
- `h::named(tree, "Submit")` — find node by name substring
- `h::act(p, tree, id, action)` — perform action, wait, re-read tree

### Key coverage gaps to address

- **EventProvider** — no tests at all (not yet implemented for any provider)
- **macOS integration tests** — blocked on `xa11y-macos` provider implementation

## Design Tenets

1. **No silent fallbacks.** If an operation fails, return the error — don't silently try a different mechanism. Fallbacks hide bugs and make behavior unpredictable for consumers. Surface failures clearly so callers can handle them.

2. **Only expose what accessibility APIs support.** If a platform has no accessibility interface for an operation, don't implement it with input simulation — leave it out.

## Pre-Commit / Pre-PR Checklist

CI runs with `RUSTFLAGS: -Dwarnings`, so all warnings are errors. Before committing or opening a PR, verify:

1. **Formatting** — `cargo fmt --all` (CI runs `cargo fmt --all -- --check`)
2. **No warnings** — `RUSTFLAGS="-Dwarnings" cargo check --workspace` (catches unused imports, dead code, etc.)
3. **Unit tests pass** — `cargo test --workspace`
4. **Integration tests pass** (if touching provider/test-app code):
   - Linux: `./run_integ_tests.sh`
   - macOS: `./run_integ_tests_macos.sh`
5. **No new `#[allow(...)]` without justification** — if you must suppress a warning, add a comment explaining why
6. **Python bindings** (if touching `xa11y-python/`):
   - `xa11y-python` is excluded from the Cargo workspace — `cargo check --workspace` does **not** cover it
   - Build: `cd xa11y-python && pip install -e .`
   - Check: `cd xa11y-python && cargo check` (compile the Rust extension)
   - Format: `cd xa11y-python && cargo fmt -- --check`
   - Tests: `cd xa11y-python && python -m pytest tests/ -v`

Common CI failures:
- `unused import` / `dead_code` — remove the unused code or add `#[allow(dead_code)]` with a reason
- Formatting diffs — run `cargo fmt`
- Platform stubs (`xa11y-macos` on Linux, `xa11y-linux` on macOS) — make sure stub modules compile cleanly on all platforms
- Python binding failures — `xa11y-python` is **not** in the Cargo workspace, so workspace-wide commands skip it. You must build and test it separately (`cd xa11y-python`)

## Running Tests

```bash
# Unit tests (no infrastructure needed)
cargo test --workspace

# Python binding tests (xa11y-python is excluded from the workspace)
cd xa11y-python && pip install -e . && python -m pytest tests/ -v

# Integration tests (Linux — needs Xvfb + D-Bus + AT-SPI2)
./run_integ_tests.sh

# Integration tests (Linux from macOS — via Finch container)
./run_integ_container.sh                      # all tests
./run_integ_container.sh tree_has_buttons      # single test
./run_integ_container.sh --shell               # interactive debug

# Integration tests (macOS — needs xa11y-macos provider)
./run_integ_tests_macos.sh

# Platform provider fuzzer (needs test app running)
./run_provider_fuzz.sh                    # random seed, 10k iterations
./run_provider_fuzz.sh --seed 42 -n 5000  # reproducible

# Core fuzz tests (requires nightly)
cd xa11y-fuzz/fuzz && cargo +nightly fuzz run tree_ops -- -max_total_time=60

# Coverage report
./coverage.sh
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
- `docs/DESIGN.md` — Full design specification
