# Agent Guidelines

## Integration Test Coverage

Before adding or modifying integration tests, read [`xa11y/tests/INTEG_COVERAGE.md`](xa11y/tests/INTEG_COVERAGE.md) for the current coverage analysis. The goal is **full coverage** of the public API surface through real AT-SPI2 integration tests.

When adding new tests:

1. Check the coverage file to identify gaps — prioritize uncovered areas.
2. If the test app (`xa11y-test-app`) lacks a widget needed for a test (e.g., expandable tree view, menu bar, combo box), add it to the test app first.
3. All integration tests must be `#[ignore]` and run via `./run_integ_tests.sh`.
4. After adding tests, update `INTEG_COVERAGE.md` to reflect the new coverage.
5. Run `./run_integ_tests.sh` to verify tests pass before committing.

### Key coverage gaps to address

- **EventProvider** — no tests at all
- **Action variants** — only `Press` is tested; `Focus`, `SetValue`, `Toggle`, `Increment`, `Decrement`, etc. all need tests (may require adding widgets like spinners or expandable rows to the test app)
- **StateSet fields** — `visible`, `focused`, `selected`, `expanded`, `editable`, `required`, `busy` are untested
- **Selector combinators** — descendant (` `) and child (`>`) combinators not tested against real trees
- **QueryOptions** — `visible_only` and `roles` filter not tested
- **Node fields** — `description`, `bounds`, `bounds_normalized`, `children`, `parent`, `depth` never verified
- **AppTarget::ByPid** — not tested
- **Error paths** — no tests for `AppNotFound`, `NodeNotFound`, `InvalidSelector`, etc.

## Running Tests

```bash
# Unit tests (no infrastructure needed)
cargo test --workspace

# Integration tests (Linux only, needs Xvfb + D-Bus + AT-SPI2)
./run_integ_tests.sh
```

## Project Structure

- `xa11y-core/` — Platform-independent types, traits, selector engine
- `xa11y-linux/` — AT-SPI2 backend via zbus
- `xa11y-macos/` — macOS backend (stub)
- `xa11y-windows/` — Windows backend (stub)
- `xa11y/` — Umbrella crate, unit tests, integration tests
- `xa11y-test-app/` — GTK3 app used as target for integration tests
- `docs/DESIGN.md` — Full design specification
