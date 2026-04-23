//! Cross-platform integration tests for xa11y.
//!
//! These tests require a running test application (xa11y-test-app) with an
//! accessibility provider. On Linux, this means Xvfb + D-Bus + AT-SPI2.
//!
//! Run with: cargo xtask test-integ
//!
//! All tests are `#[ignore]` — the harness script runs them with `--ignored`.
//!
//! The tests are split across submodules under `tests/integ/` so each
//! thematic group lives in its own file while still compiling into this
//! single `integ_test` binary (keeping compile cost flat).

mod integ;
