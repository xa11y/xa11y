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

## Running Tests

```bash
# Unit tests (no infrastructure needed)
cargo test --workspace

# Integration tests (Linux — needs Xvfb + D-Bus + AT-SPI2)
./run_integ_tests.sh

# Integration tests (macOS — needs xa11y-macos provider)
./run_integ_tests_macos.sh

# Fuzz tests (requires nightly)
cd xa11y-fuzz/fuzz && cargo +nightly fuzz run tree_ops -- -max_total_time=60

# Coverage report
./coverage.sh
```

## Project Structure

- `xa11y-core/` — Platform-independent types, traits, selector engine
- `xa11y-linux/` — AT-SPI2 backend via zbus
- `xa11y-macos/` — macOS backend (stub)
- `xa11y-windows/` — Windows backend (stub)
- `xa11y/` — Umbrella crate, unit tests, integration tests
- `xa11y-test-app/` — AccessKit + winit app used as target for integration tests
- `xa11y-fuzz/` — Fuzz targets for xa11y-core (tree, selector, serde)
- `docs/DESIGN.md` — Full design specification
