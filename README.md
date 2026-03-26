# xa11y

Cross-platform accessibility library for reading and interacting with accessibility trees.

## Quick Start

```bash
cargo build --workspace
cargo xtask test              # unit tests
cargo xtask check             # all pre-PR checks (fmt, lint, test, python)
```

## Development Commands

All workflow commands are available via `cargo xtask`:

```bash
cargo xtask fmt               # format Rust + Python
cargo xtask lint              # clippy + ruff
cargo xtask test              # unit tests
cargo xtask test-python       # build + test Python bindings
cargo xtask test-integ        # integration tests (auto-detects OS)
cargo xtask test-integ-container              # Linux tests via Finch container
cargo xtask test-integ-container tree_has_buttons  # single test
cargo xtask fuzz              # provider fuzzer
cargo xtask coverage          # code coverage report
cargo xtask docs              # build documentation
cargo xtask check             # ALL pre-PR checks
```

## Project Structure

- `xa11y-core/` — Platform-independent types, traits, selector engine
- `xa11y-linux/` — AT-SPI2 backend via zbus
- `xa11y-macos/` — macOS backend (AXUIElement)
- `xa11y-windows/` — Windows backend (stub)
- `xa11y/` — Umbrella crate with unit + integration tests
- `xa11y-test-app/` — AccessKit + winit test application
- `xa11y-fuzz/` — Fuzz targets for xa11y-core

## License

All dependencies are permissively licensed (MIT, Apache-2.0, BSD, or similar). License compliance is enforced in CI via `cargo-deny`.
