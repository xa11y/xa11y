# xa11y Code Review — Outstanding Items

Snapshot after PR #129. Resolved items are in `git log main`. This is the live list of what's still open.

---

## Provider tenet work still pending

### Linux: fast-path selector matcher gap
`xa11y-linux/src/atspi.rs::matches_ref` only answers `role` / `name` / `value` / `description`. Selectors like `[enabled="true"]`, `[checked="on"]`, `[focused="true"]` silently return empty on Linux because the fast path filters elements out before a full `ElementData` is built. The delegation fix landed in PR #129 and was reverted as a side effect of the GTK debugging — see #132 for the related Linux refactor puzzle. Needs a proper fix: either extend `matches_ref` to answer state attrs directly from AT-SPI state flags, or re-land the fallthrough-to-full-build approach once the GTK issue is understood.

---

## Umbrella crate

### `StaticProviderRef` delegation boilerplate
`xa11y/src/lib.rs` — 70 lines of hand-written `Provider`-trait delegation for `StaticProviderRef`. Three naive refactors (blanket impl on `&T`, `Arc::from(Box)`, `Arc::new(Concrete)`) all broke Linux GTK CI in ways we couldn't pin down across two PRs (#129 + #130). Tracked in **[#132](https://github.com/xa11y/xa11y/issues/132)**. Needs a Linux dev loop, not CI ping-pong.

---

## Tests / CI

### Per-framework Python suite depth
AccessKit integ: ~122 tests. Cocoa: ~54. Tauri: ~49. Qt: ~76. GTK: ~28 (unchanged from before PR #129 — the expansion was reverted during GTK CI debugging). Still below AccessKit depth for the non-AccessKit apps.

### GTK test expansion reverted
PR #129 added event + widget tests for GTK, but they were removed during Linux CI debugging (same class of unexplained failure as #132). Re-adding requires first understanding why the GTK fixture flaked on that branch — likely tangled with the `StaticProviderRef` investigation.

### macOS AX-call-count regression tests not in CI
`scripts/run_integ_tests_macos.sh` runs the `ax_calls_*` tests. Bounds were relaxed from exact equality to `<=` in PR #129, but CI doesn't run these — kTCCServiceAccessibility grants on cargo-hashed test-binary paths are brittle under macos-latest runners (hash changes across builds, tccd reloads race). Developer-only today. See the comment in `.github/workflows/ci.yml` added in PR #129.
