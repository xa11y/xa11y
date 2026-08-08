# Agent Guidelines

## Integration Test Coverage

The goal is **full coverage** of the public API surface through real integration tests.

When adding new tests:

1. If the AccessKit test app lacks a widget needed for a test, add it to the test app first. The test app uses AccessKit + winit and is defined in `test-apps/accesskit/src/main.rs`.
2. All integration tests must be `#[ignore]` and run via `cargo xtask test-integ`.
3. Run `cargo xtask test-integ` to verify tests pass before committing.

### Test helpers

Integration tests use shared helpers from `xa11y/tests/integ/mod.rs`:
- `h::app_tree()` — get test app root Element with retries
- `h::one(root, "selector")` — find exactly one element by selector
- `h::named(root, "Submit")` — find element by name substring
- `h::act(element, action)` — perform action, wait, re-read tree, return new root

### Key coverage gaps to address

- _(none currently)_ — the `ControlType.Custom` + `TableItem` cell branch of `map_uia_role` in `xa11y-windows/src/uia.rs` is now exercised end-to-end by `test-apps/wpf` (`windows-latest × wpf` in the `integ` matrix), alongside the DataItem cell shape `test-apps/winforms` covers.

## Design Tenets

1. **No silent fallbacks.** If an operation fails, return the error — don't silently try a different mechanism. Fallbacks hide bugs and make behavior unpredictable for consumers. Surface failures clearly so callers can handle them.

   **Anti-patterns that violate this tenet:**
   - `let _ = some_call();` — if the call's result matters, propagate it; if it genuinely doesn't, leave a one-line comment explaining why.
   - `some_call().ok()` used to coerce `Result → Option` and discard the error reason.
   - `if let Ok(x) = some_call() { ... } // else fall through` — this treats a real error as "no match". Match on the specific expected variant (e.g. `Err(Error::SelectorNotMatched)`) and propagate the rest.
   - Fallback chains: try A, on failure try B, on failure try C. Each step hides the original failure and changes effective behavior. If multiple mechanisms genuinely need to be tried, do it explicitly with logged reasoning, not silent fall-through.

2. **Only expose what accessibility APIs support.** If a platform has no accessibility interface for an operation, don't implement it with input simulation — leave it out.

3. **Action fidelity.** If an element reports an action name in its `actions` list, calling that action must invoke the original platform action — not a substitute or alias.

   `press`, `toggle`, `focus`, `select`, `expand`, `collapse` are *semantic verbs* — cross-platform concepts. Tenet 3 applies to the semantic verb, not a specific platform API name. For example: `press` on Windows legitimately dispatches to Invoke, Toggle, SelectionItem.Select, or ExpandCollapse based on the element's primary-activation pattern — this is the Windows canonical implementation of "activate this element," matching AXPress on macOS and AT-SPI `DoAction("click")` on Linux. A violation would be advertising `press` in actions but calling a platform API that doesn't implement the semantic (e.g. input simulation, or an unrelated pattern).

4. **Fail surfaceably, not fatally.** Prefer `Result` over `.unwrap()` / `.expect()` in provider and binding code.
   - **Locks**: `.lock().unwrap()` on caches or memoized state should be `.lock().unwrap_or_else(|e| e.into_inner())` — poisoning in a cache is recoverable. Only panic on locks that guard a genuine invariant.
   - **Platform FFI returns**: never `.unwrap()` a CF / AX / UIA / AT-SPI2 return. Propagate as `Error::Platform`.
   - **Tests** may use `.expect("...")` with a descriptive message when failure would indicate a broken test fixture.
   - If you add a new `.unwrap()`, a reviewer should be able to point at an invariant one line above that proves it can't panic.

5. **Blocking calls release the host runtime's lock.** In language bindings, any call that can block, sleep, or poll — waits, auto-waiting actions, attach/discovery loops, event receives — must release the host runtime's global lock (Python's GIL, or the platform equivalent) for the duration of the block. A wait that holds the GIL freezes every other thread in the consumer's process for up to the full timeout, and forces consumers into architectural workarounds (e.g. moving an in-process mock server into a separate process).

   **Anti-patterns that violate this tenet:**
   - A binding method that calls a core wait/poll loop directly instead of inside `py.allow_threads` (or the platform equivalent).
   - Holding the lock across a whole poll loop because one step needs it. The only legitimate reason to hold the lock is calling back into the host language (e.g. a Python predicate in `wait_until` / `App.find`) — reacquire it per callback, never for the loop.
   - Treating this as an optimization. A missing `allow_threads` on a blocking path is a correctness bug, not a style choice.

   Enforced for Python by `xa11y-python/tests/test_gil_release.py`, which asserts that a background thread keeps making progress while a native wait blocks.

6. **Errors carry their own diagnosis.** An error must contain enough context to understand the failure without re-running it under extra logging. If a consumer would need to wrap a call in `try/except` just to print surrounding state — which selector, what condition, what the tree looked like — that state belongs in the error itself. The structured carrier for this context is `Diagnosis` in `xa11y-core/src/error.rs`; new failure paths attach one at the *terminal* failure site (see the module docs there for the pattern).

   **Anti-patterns that violate this tenet:**
   - A timeout that reports only the duration. `Timeout after 60.0s` is a bug; it must say what it was waiting for (selector + condition) and what it last observed (e.g. "matched but visible=false" vs. "never matched").
   - "Not found"-class errors that echo only the input. They should describe where they searched and what they *did* find (near-miss candidates, enumeration counts, a bounded snapshot of the search scope).
   - Rich context that exists only in the message string. Bindings expose it as structured fields (exception attributes in Python, error properties in JS) so harnesses can act on it programmatically, not parse prose.
   - Unbounded diagnostics. Context is collected on the failure path only and is size-bounded (depth-limited tree snapshots, truncated candidate lists) — rich errors must never slow the success path or emit megabyte messages. In particular, never attach an expensive `Diagnosis` to an error that poll loops use as a retry signal; enrich at the point where the error actually reaches the user.

### Breaking a tenet

These tenets are firm defaults, not absolutes. If a situation genuinely requires breaking one:

1. **Get human approval first.** Do not land a tenet-breaking change without an explicit human sign-off on that specific break. Agents must pause and ask.
2. **Document it at the call site.** Add a comment immediately above the break, prefixed `// TENET-BREAK(<N>):` where `<N>` is the tenet number, explaining *why* the break is justified here (platform limitation, known upstream bug, etc.) and what the alternative would cost.
3. **Make the break discoverable.** These comments should be greppable (`rg 'TENET-BREAK'`) so the full set of exceptions stays visible and reviewable.

## Platform notes

### macOS: ObjC exception safety

All raw CoreFoundation / AX FFI calls in `xa11y-macos/src/ax.rs` must go through the wrappers in `xa11y-macos/src/exception_safe.m`. That file wraps calls like `CFRetain`, `CFRelease`, `CFGetTypeID`, `CFNumberGetValue`, `CFBooleanGetValue`, `CFArrayGetCount`, `CFArrayGetValueAtIndex`, and `CFDictionaryGetValue` in `@try`/`@catch`. A misbehaving AX value's `-release` / `-getTypeID` can throw an `NSException` that unwinds through `extern "C"` → process abort. When adding a new CF or AX interop call, go through the `safe_*` wrapper; if one doesn't exist, add it to `exception_safe.m` first. Enforced by `cargo xtask check-macos-ffi` (run automatically as part of `cargo xtask check`), which fails the build if any raw CF/AX symbol is referenced outside a `//` comment in `ax.rs`.

## Bindings Parity

`cargo xtask check-bindings-parity` verifies that the Python and JS bindings mirror `xa11y-core`'s public API. It reads core's real public surface from **rustdoc JSON**, so new public API is discovered automatically rather than needing to be added to a hardcoded list.

**Requires a nightly toolchain** (`--output-format json` is still unstable):

```bash
rustup toolchain install nightly --profile minimal
```

The check enforces three rules, all configured in `bindings/parity_allowlist.toml`:

1. **Every public type in `xa11y-core` must be classified** under `[types]` as one of:
   - `mirrored` — the bindings expose this type and each of its members.
   - `opaque` — it crosses the boundary as a primitive, plain object, or exception class (`Role` as a string, `Error` as exception classes). Members are not compared.
   - `internal` — not part of the binding surface (provider traits, selector-engine internals).

   A new public type that nobody classified **fails the check**. This is the point: you cannot add public API and silently forget the bindings.

2. **For `mirrored` types, members must match.** Every public core member must exist in both bindings, and every binding member must map to a core member — unless listed in `[python.rust_only]` / `[python.python_only]` (and the `[js.*]` equivalents) **with a reason**.

   Those per-member entries are themselves checked for staleness: an entry naming a member that core (or the binding) no longer has excuses nothing while still reading as a live design decision, so it fails the check rather than accumulating.

3. **Hand-mapped `#[non_exhaustive]` enums must have every variant covered.**
   See [Variant coverage](#variant-coverage-replaces-the-compiler-for-hand-mapped-enums)
   under Public API Extensibility — `[[types.variant_coverage]]` is what
   replaced the bindings' exhaustive `match` as the guard against an unmapped
   `Error` / `EventKind` / `StateFlag` variant.

### Flattening

Some core types have no binding class of their own; their members surface on another type. Declare that with `[[types.flatten]]` rather than writing one allowlist entry per member:

```toml
[[types.flatten]]
into = "Element"
from = ["ElementData", "StateSet"]
reason = "Element derefs to ElementData; both bindings expose StateSet's booleans as getters on Element."
```

Flattened members become **required** on the target binding type.

Flattening merges by name, so two sources exposing the same member name would collapse into one required entry — quietly excusing the bindings from exposing one of them. The check **fails** on any such collision; disambiguate with `rename`:

```toml
[[types.flatten]]
into = "InputSim"
from = ["Keyboard", "Mouse"]
reason = "Both bindings flatten the keyboard and mouse sub-APIs onto InputSim."
rename = [
    { member = "Keyboard::down", to = "key_down" },
    { member = "Mouse::down", to = "mouse_down" },
]
```

A `rename` naming a member no longer present in its source is reported as stale, same as a stale `[types]` entry.

## Public API Extensibility

Every public struct and enum in `xa11y-core` must make an explicit choice about
future growth. Two clippy restriction lints, enabled in `xa11y-core/Cargo.toml`,
enforce that the choice is made:

```toml
[lints.clippy]
exhaustive_enums = "warn"
exhaustive_structs = "warn"
```

CI runs with `-Dwarnings`, so a new public type is a build failure until it
either carries `#[non_exhaustive]` or an `#[allow(..., reason = "...")]` naming
the closed domain that makes growth impossible. This is the same discipline the
parity allowlist applies at the bindings boundary, one level down.

**Default to `#[non_exhaustive]`.** Take the `allow` when the type is a closed
domain in the mathematical sense — `Rect` (an origin and a size), `Point`,
`ScrollDelta`, `Toggled` (off/on/indeterminate), `RecvStatus`
(value/timeout/disconnected). "I can't think of a new field right now" is not a
closed domain.

**Capability enums are the second reason to stay exhaustive, and it is not
about closedness.** If a backend must translate every variant into an OS
primitive, growth *should* break the build: the compile error in each backend
is the only thing that forces a per-platform decision. `#[non_exhaustive]`
converts that into a `_` arm and a runtime `Error::Unsupported` from a library
whose type system advertises the capability — strictly worse than a build
failure at the point the variant is added.

The test is "does a new variant require work in another crate?"

| Enum | | Why |
|---|---|---|
| `Key`, `MouseButton` | exhaustive | Four input backends map each to a keysym / VK / evdev code / `MOUSEEVENTF_*` flag. |
| `Combinator`, `RoleMatch`, and the rest of the selector AST | exhaustive | Provider fast-paths match on it — `RoleMatch` in the Linux and macOS matchers, `Combinator` in the Windows push-down — and a `_` arm silently returns the wrong match set. |
| `Role` | `#[non_exhaustive]` | Backends map platform → `Role`, never the reverse, and `Unknown` is the documented fallback. |
| `Anchor` | `#[non_exhaustive]` | `anchor_point` resolves it in core; no backend sees it. |
| `Error`, `EventKind`, `StateFlag` | `#[non_exhaustive]` | Only the bindings map them, and `[[types.variant_coverage]]` guards that. |
| `ElementState` | `#[non_exhaustive]` | Resolved in core by `ElementState::is_met`; no binding or backend maps it, so nothing needs a coverage entry. |

`#[non_exhaustive]` forbids struct expressions from *other crates*, including
functional-update syntax — `Diagnosis { .., ..Default::default() }` does not
compile from `xa11y-linux`. So a non-exhaustive struct owes callers a way to
build one:

- **Required fields, few of them** — a plain constructor: `Screenshot::new`,
  `Event::new`, `TreeNode::new`.
- **Many optional fields, partial by nature** — a constructor plus public
  field assignment: `ElementData::for_role(role)` then `data.name = ...`. An
  event target or a test fixture genuinely does not have every field.
- **Options structs** — chained setters returning `Self`:
  `ClickOptions::new().button(..).count(..)`, `Diagnosis::new().condition(..)`.

### When a writer needs the opposite guarantee

`#[non_exhaustive]` protects *readers*: adding a field does not break them.
For a type whose writers must be complete, it removes something valuable —
the struct literal that used to fail their build until they populated the new
field. `ElementData` is the case: three provider crates translate a platform
node into one, and a new property silently arriving as `None` on every
platform is a bug, not a default.

The fix is to give the two roles separate types rather than to pick one
guarantee over the other. `reader_writer_pair!` in `xa11y-core/src/element.rs`
declares both from **one field list**, so they cannot drift:

```rust
reader_writer_pair! {
    /// Readers: growth is not breaking.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ElementData;

    /// Writers: growth IS breaking, on purpose.
    #[allow(clippy::exhaustive_structs, reason = "This type IS the completeness guard...")]
    #[derive(Debug, Clone)]
    pub struct ElementParts;

    fields { /// Element role
             pub role: Role, ... }
}
```

The reader gets the docs, the serde attributes, and `#[non_exhaustive]`; the
writer gets bare fields and stays exhaustive; the `From` impl is generated.
Providers write `ElementParts { .. }.into()`, so adding a field is a compile
error in every backend and a no-op for consumers.

**The single list is the point.** With two hand-written structs, adding a field
to `ElementData` alone leaves exactly one place to write a default — the `From`
impl — and the compiler points you at it. That is the cheapest fix and it
defeats the whole design. `StateSet` / `StateParts` uses the same macro for the
same reason: a silently-defaulted state is worse than it looks, because the
parity check then requires a binding getter for a state no platform populates.

Because a new field here is a deliberate compile error across a crate
boundary, the workspace pins intra-workspace deps with `=` rather than a caret
(`Cargo.toml`), so cargo cannot pair a newer `xa11y-core` with an older
provider in a downstream tree. `cargo xtask check` verifies the pins survive
cargo-release's `dependent-version = "upgrade"` rewrite.

Reach for this only where all three hold: the type is `#[non_exhaustive]`, it
has complete-construction sites in *other* crates, and a defaulted new field
would be a bug. Two of those are easy to get wrong:

- **Construction sites inside `xa11y-core` need nothing.** A crate can always
  struct-literal its own `#[non_exhaustive]` type, and that literal is still
  exhaustiveness-checked. `TreeNode` is built only in core, so it is covered.
- **An all-required-args constructor is already the guard.** `Screenshot::new`
  takes every field, so a new one changes the signature and breaks all callers.
  A `Parts` type would add nothing.

`Diagnosis`, `ClickOptions`, and `DragOptions` are the counter-case: every
call site sets one or two fields on purpose, so completeness is meaningless
and `Default` is the whole point.

One trap: a constructor named `new` on a type that is **flattened** into
another (see [Flattening](#flattening)) collides with the target's own `new`,
and both would then be satisfied by a single allowlist entry. That is why
`ElementData` has `for_role` rather than `new` — `Element::new` already exists.
The parity check reports this shadowing rather than merging silently.

### Variant coverage replaces the compiler for hand-mapped enums

`#[non_exhaustive]` on an enum forces downstream `match` arms to carry a `_`
fallback. For `Error`, `EventKind`, and `StateFlag` that fallback *removes a
guard the project relied on*: the bindings' exhaustive matches were what failed
the build when a variant was added without being mapped.

`[[types.variant_coverage]]` in `bindings/parity_allowlist.toml` is the
replacement. Each entry names an enum and the files that must mention every one
of its variants as `Type::Variant`:

```toml
[[types.variant_coverage]]
type = "EventKind"
reason = "Each variant maps to a distinct event-type string in both bindings and in the CLI."
files = [
    "xa11y-python/src/lib.rs",
    "xa11y-js/src/types.rs",
    "xa11y/src/cli.rs",
]
```

A file entry is either a bare path — the variant is written as `Type::Variant`
in Rust code — or a table naming how that file spells it:

```toml
{ path = "xa11y-js/index.js", spelling = "screaming_snake", prefix = "XA11Y_" }
```

`snake`, `camel`, and `screaming_snake` cover the generated `.pyi` constants,
the `EventTypeName` union, and the JS error-code table. Those are hand-
maintained alongside the Rust arms and are just as much a place a new variant
must be handled.

Rust files are parsed with `syn` and scanned as tokens, so a mention only
counts if it is real code — `// TODO: map Error::NewVariant`, a doc comment, a
string literal, a `use` import, and anything under `#[cfg(test)]` all fail the
check, and the lexer handles nested block comments and raw strings for free.
The `.pyi` / `.mjs` / `.js` files have no parser, so they get a line-comment
strip and a quoted-token match instead. Entries are checked for staleness (a
type that left core, or that is not an enum) the same way `[types]` entries
are.

`Anchor`, `MouseButton`, and `Toggled` are covered for the *binding* side
only. They cross as snake_case strings through hand-written parsers whose
catch-all arm would otherwise swallow a new variant; `MouseButton` and
`Toggled` stay exhaustive in core, so the backends are still guarded by the
compiler. A variant with a payload rather than a name (`Anchor::Offset`) has
no string spelling, so a file entry can `exclude` it — and a stale exclude is
reported like any other.

`Role` and `Key` are deliberately **not** covered: `Role` converts
mechanically via `to_snake_case`, and `Key` is parsed from a string by a
binding-side parser that already rejects unknown names loudly.

## Binding Shape Conventions

The parity check enforces that a core member is *present* in both bindings. It says nothing about what shape it takes. These are the conventions for that shape — follow them so a new binding method doesn't need its own debate.

### Options structs fold into the primary verb

Core's `*_with` variants (`Mouse::click_with`, `Mouse::drag_with`) do **not** get a second binding method. The option struct's fields become optional parameters of the primary verb:

- **Python** — keyword-only: `click(target, *, button="left", count=1, held=None, anchor=None)`
- **JS** — a trailing options object: `click(target, options?)`

The primary verb then routes through the `_with` variant **unconditionally**, even when every option is defaulted. One code path means the plain call cannot drift from the optioned one, and `ClickOptions::default()` is by construction what the plain call used to pass. The `_with` method gets a `rust_only` entry naming the fold, and the option struct is classified `internal` with a matching reason.

Two names for one operation is worse than one name with options — and a binding that exposes only the plain verb is the gap this convention exists to prevent.

### Value enums cross as identically-spelled snake_case strings

`Role` → `text_field`, `MouseButton` → `left`, `Anchor` → `top_left`. The **same spelling in both bindings** — do not camelCase them for JS. `Element.role` already returns `to_snake_case()`, and selectors are the same string in both languages.

The exception is an enum *payload* flattened onto a struct at the boundary: `EventKind` surfaces as `event_type = "focus_changed"` in Python and `type = 'focusChanged'` in JS. Those follow the naming of the properties around them, not the value convention. If you are adding a value that a user writes as a literal, it is snake_case everywhere; if you are destructuring an enum into fields, it follows the host language.

### Timing arguments use each language's own unit

**Seconds in Python, milliseconds in JS** — `duration=0.15` vs `duration: 150`, matching `set_default_timeout(seconds)` and `timeout: 5000`. This is a deliberate split of the same kind as `event_type` / `type`: someone porting a script is already renaming, and a Python API in milliseconds would be the odd one out in its own binding. Say so in the docstring, as `InputSim.drag` does.

### Polymorphic parameters beat method proliferation

A parameter may accept more than one shape when the alternative is two methods: `target` takes a tuple/array **or** an `Element`; `anchor` takes a name **or** a `(dx, dy)` offset. Parse the union in the binding and hand core a single concrete type.

### Parse arguments before the first OS call

Every parse (`parse_key`, `parse_button`, `parse_anchor`, `parse_duration`) runs before any event is posted, so a bad argument can never leave a half-delivered gesture behind — no key pressed without a release. It also makes validation testable without an input backend, which is the only coverage available for `InputSim` off a real display.

`parse_duration` is the pattern for numeric conversion: reject non-finite and negative values explicitly rather than letting `Duration::from_secs_f64` panic (tenet 4).

### Primitives keep core's shape

`mouse_down()` takes no target because `Mouse::down` takes none — the OS primitive presses wherever the pointer already is. Do not add convenience parameters a core primitive doesn't have; that is tenet 3 at the binding layer. Callers compose (`move_to` then `mouse_down`), exactly as they would in Rust.

### Anchors resolve binding-side

Neither binding's `Element` is a core `Element` (Python holds `ElementData`, JS holds its own struct), so `ClickTarget::Element` is not constructible without a provider round-trip. Call `xa11y::anchor_point(&rect, anchor)` in the binding and pass `ClickTarget::Point`. `ClickOptions.anchor` is therefore always inert by the time core sees it — that is expected, not a bug.

### Python blocking calls release the GIL

Any binding method that reaches an OS call goes inside `py.allow_threads`, with argument parsing done first (it needs the GIL). This is tenet 5, and it is not optional for input simulation: `drag(duration=...)` makes the block caller-controlled.

## Type Declarations

### `index.d.ts` shadows `native.d.ts`

`package.json` points `types` at `index.d.ts`, so where it declares a class of its own — `App`, the EventEmitter `Subscription`, the error hierarchy — that declaration is **what consumers get**, and the napi class of the same name in `native.d.ts` is invisible to them. Adding a method to the Rust `App` therefore requires adding it to `index.d.ts` by hand; the generated declaration alone reaches nobody. (`App.asElement`, `tree` and `dump` shipped this way for several releases.)

Interface declarations are the other case: `declare module './native.js' { interface Locator { ... } }` augments the generated class rather than shadowing it, and does merge.

`xa11y-js/__test__/unit/typing.test.js` enforces both directions of this against the real runtime objects, and is the JS counterpart of `xa11y-python/tests/test_typing.py`. It scans the `.d.ts` rather than using the TypeScript compiler API, because `typescript` 7 is the native port and no longer ships one.

### New string-valued parameters need a narrowing

`native.d.ts` is generated, so a `MouseButton` parameter arrives as plain `string`. Add an entry to `xa11y-js/scripts/patch-native-dts.mjs` narrowing it to a literal union (`MouseButtonName`, `AnchorName`, …), and export the alias from `index.d.ts`. Each entry is a guarded substitution — `all: N` asserts the occurrence count, so a new call site that quietly picks up the wide type fails the build instead of shipping.

### Signatures are checked on the Python side only

The parity check compares member **names**. Signature drift — an added parameter, a renamed keyword, a changed default — is caught by `test_stub_method_signatures_match_runtime` in `xa11y-python/tests/test_typing.py`.

It works because PyO3 emits a `__text_signature__` for every method, so `inspect.signature` reports the compiled module's real parameter names, keyword-only split, and defaults. The stub is then checkable against the thing it claims to describe, with no Rust parsing and no extra dependency. Dunders are compared by arity only: the interpreter invokes them positionally, and PyO3 owns the rendered signature of slot-backed ones (`Rect.__eq__` reports its parameter as `value` whatever the Rust source calls it).

Two things it deliberately does not do:

- **Types are not compared.** PyO3 attaches no annotations, so `param: str` in the stub has no runtime counterpart. Catching a wrong *type* still needs the Rust→PyO3 mapping table.
- **There is no JS equivalent.** napi-generated prototype methods report `Function.length === 0`, so there is no arity to compare — and `native.d.ts` is generated from the Rust anyway, so only the hand-written `index.d.ts` wrapper layer could drift. `__test__/unit/typing.test.js` covers that at name level.

Generating `_native.pyi` instead (with `pyo3-stub-gen`) is blocked: every version fails to compile against a PyO3 built with `abi3-py39`, which is what lets one wheel serve every Python version. See the notes on issue #331.

### Notes

- `#[doc(hidden)]` items are not public API and never need an allowlist entry — rustdoc excludes them.
- Public **methods** on `mirrored` types must carry a doc comment; the binding stubs and the docs site are generated from them.
- When rustdoc bumps its JSON format, the check fails loudly with the new version number. Verify the field reads in `xtask/src/rustdoc_api.rs` still hold, then add the version to `SUPPORTED_FORMAT_VERSIONS`.

## Pre-Commit / Pre-PR Checklist

Run `cargo xtask check` to run all pre-PR checks in one command. It covers formatting, linting, unit tests, both bindings, the integ harness self-tests, and the pytest plugin.

CI runs with `RUSTFLAGS: -Dwarnings`, so all warnings are errors. Individual checks:

1. **Formatting** — `cargo xtask fmt` (use `cargo xtask fmt --check` to verify without modifying)
2. **Lint** — `cargo xtask lint` (clippy + ruff check + Python Rust check)
3. **Unit tests** — `cargo xtask test`
4. **Integration tests** (if touching provider/test-app code) — `cargo xtask test-integ`
5. **Python bindings** — `cargo xtask test-python`
6. **pytest plugin** (if touching `pytest-xa11y/`) — `cargo xtask test-pytest-plugin`
7. **Docs prose** (if touching `README.md` or `docs/site/src/content/docs/`) — `cargo xtask lint-docs`
8. **Docs site** (if touching `docs/site/src/content/docs/`) — `python docs/check_page_types.py`, `docs/check_tables.py`, `docs/check_links.py`
9. **No new `#[allow(...)]` without justification** — if you must suppress a warning, add a comment explaining why. The `clippy::exhaustive_*` allows in `xa11y-core` use the `reason = "..."` form; see [Public API Extensibility](#public-api-extensibility).

Common CI failures:
- `unused import` / `dead_code` — remove the unused code or add `#[allow(dead_code)]` with a reason
- Formatting diffs — run `cargo xtask fmt`
- Platform stubs (`xa11y-macos` on Linux, `xa11y-linux` on macOS) — make sure stub modules compile cleanly on all platforms
- Python binding failures — `xa11y-python` is **not** in the Cargo workspace, so workspace-wide commands skip it. `cargo xtask lint` and `cargo xtask test-python` handle this automatically.

## Running Tests

```bash
# All pre-PR checks (fmt, lint, test, test-python, test-js, test-harness,
# test-pytest-plugin, plus the macOS FFI and bindings-parity checks)
cargo xtask check

# Individual commands
cargo xtask fmt                               # format Rust + Python
cargo xtask fmt --check                       # check without modifying
cargo xtask lint                              # clippy + ruff + Python Rust check
cargo xtask test                              # unit tests
cargo xtask test-python                       # build + test Python bindings
cargo xtask test-pytest-plugin                # install + test pytest-xa11y
cargo xtask test-integ                        # integration tests (auto-detects OS)
cargo xtask test-integ-container              # Linux integration tests via Finch
cargo xtask test-integ-container tree_has_buttons  # single test in container
cargo xtask test-qt                           # Qt (PySide6) integration tests
cargo xtask test-gtk                          # GTK4 integration tests
cargo xtask test-cocoa                        # Cocoa/AppKit integration tests (macOS only)
cargo xtask test-tauri                        # Tauri integration tests
cargo xtask test-winforms                     # WinForms integration tests (Windows only)
cargo xtask test-wpf                          # WPF integration tests (Windows only)
cargo xtask test-apps                         # all Python integration test suites
cargo xtask fuzz                              # provider fuzzer

# Wire-level input-backend tests (no app launched; both are #[ignore]d)
cargo test -p xa11y-linux   --test wayland_input_e2e -- --ignored --test-threads=1
cargo test -p xa11y-windows --test send_input_wire   -- --ignored --test-threads=1

cargo xtask fuzz --seed 42 -n 5000            # reproducible fuzz run
cargo xtask coverage                          # code coverage report
cargo xtask docs                              # build documentation
cargo xtask lint-docs                         # Vale prose lint for README + docs site

# Core fuzz tests (requires nightly)
cd xa11y/fuzz && cargo +nightly fuzz run tree_ops -- -max_total_time=60
```

## Docs Structure (Diátaxis)

The hand-written pages under `docs/site/src/content/docs/` follow
[Diátaxis](https://diataxis.fr). Every page is exactly one of **tutorial**,
**how-to guide**, **reference**, or **explanation**, and the directory layout,
the sidebar groups, and the page itself all say which.

`docs/PAGE_TYPES.md` is the contract. Read it before adding or restructuring a
page. The short version:

- Frontmatter carries `pageType: <type>`, and the first thing after the
  frontmatter is a `{/* DIATAXIS: <type> — … */}` banner with fixed wording.
- The page lives in the directory its type maps to: `tutorials/`, `guides/`,
  `reference/`, `explanation/`, or the site root for `evaluation` / `landing`.
- `python docs/check_page_types.py` enforces all three, and runs in
  `cargo xtask docs` and the `docs` CI job. The Astro content schema in
  `docs/site/src/content.config.ts` enforces the frontmatter key again, so
  `npm run build` fails on a page that omits it.

The rule that matters most in practice: **a reference table has exactly one
home.** If a second page needs it, that page links. The docs previously carried
selector syntax on two pages, a CLI manual on two pages, and the error variant
table on a page that wasn't about errors; all three had drifted apart by the
time they were merged. When a page seems to need two modes, write two pages.

## Docs Prose Linting

`README.md` and the hand-written pages under `docs/site/src/content/docs/` are
linted with [Vale](https://vale.sh). `cargo xtask lint-docs` runs it locally;
the `Docs Lint` workflow runs it in CI whenever one of those files, `.vale.ini`,
or the project vocabulary changes.

Two style sets are configured in `.vale.ini`:

- **`Vale`** — the built-in style that ships with the binary (spelling,
  repetition).
- **[`ai-tells`](https://github.com/tbhb/vale-ai-tells)** — flags the prose
  patterns that read as machine-written: em-dash asides, three-item verb
  series, "not X but Y" contrasts, hedges, filler adverbs. The release is
  pinned in `.vale.ini` so an upstream rule addition cannot turn CI red
  without a commit here.

`vale sync` downloads the style package into `.github/styles/`, which is
gitignored apart from `config/vocabularies/xa11y/accept.txt`. That file is the
project's term list: everything Vale's dictionary doesn't know but this
codebase writes in prose (`AXUIElement`, `uinput`, `pywinauto`, `subrole`, and
so on). Add a term there rather than rewording around the spell checker.

Two things worth knowing before adding pages:

- **Frontmatter is linted**, minus the keys themselves. `description` is the
  page's meta description, so it gets the same treatment as body prose. A
  frontmatter key not listed in the `TokenIgnores` line shows up as a colon
  alert, which is the cue to add it.
- **A fenced code block indented inside a JSX element** (every `<TabItem>` in
  these guides) is not recognised as a fence by Vale's Markdown parser, so
  `.vale.ini` skips those blocks explicitly. Top-level fences are handled by
  the parser.

## READMEs

`README.md` at the repo root is the source. `cargo xtask sync-readmes` renders
it into `xa11y/README.md` (crates.io) and `xa11y-python/README.md` (PyPI), and
CI runs `--check` and fails when either has drifted. Edit the root and
re-render; never edit a package README directly. `xa11y-js/README.md` is
hand-written and sits outside this pipeline.

Language-specific content is marked, in one of two forms:

- `<!-- python-only -->` ... `<!-- /python-only -->` renders in the root README
  *and* in that language's package README. Every visible block shows on the
  root page, which is why the root lists all three install steps while each
  package README lists only its own.
- `<!-- rust-only-hidden` ... `-->` puts the content inside the comment, so the
  root README renders nothing while the package README still gets it. The Rust
  quick example ships this way: the root page leads with Python, and crates.io
  still gets a Rust snippet.

The recognised language names are `rust`, `python`, and `js` (`README_LANGS` in
`xtask/src/main.rs`). A typo'd name fails `sync-readmes` instead of shipping a
literal marker to a package registry. Hidden content must not contain `-->`,
which would end the comment early and leak the rest into the root page.
