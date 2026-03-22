# xa11y

Cross-platform accessibility library for reading and interacting with accessibility trees.

## Quick Start

```bash
cargo build --workspace
cargo test --workspace        # unit tests
```

## Running Linux Integration Tests (from macOS)

Requires [Finch](https://github.com/runfinch/finch) (or any OCI-compatible container runtime).

```bash
# First run builds a base image (~2min), then compiles + tests (~1min)
./run_integ_container.sh

# Subsequent runs reuse the build cache (~45s, or ~10s for a single test)
./run_integ_container.sh tree_has_buttons   # run one test
./run_integ_container.sh --build-only       # compile without testing
./run_integ_container.sh --shell            # interactive shell in container
```

On native Linux, run directly:

```bash
./run_integ_tests.sh
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
