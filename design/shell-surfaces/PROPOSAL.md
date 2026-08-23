# Shell surfaces — API proposal for xa11y#374

**Status:** shipped in [#377](https://github.com/xa11y/xa11y/pull/377); kept as
the design record for the feature. The `§` citations below refer to the field
report that preceded it, which was measurement gathered on the
`report/shell-surfaces-374` branch and is not part of this tree; this document
is the design those measurements pointed at.

**The request** ([#374](https://github.com/xa11y/xa11y/issues/374)): reach
OS-owned UI — the macOS menu bar and status items, the Windows taskbar and
tray (visible row and overflow), shell flyouts — through the same selector /
tree / dump / action machinery as app trees, with honest failure modes, on
all consumer surfaces (Rust, Python, JS, CLI, MCP).

**The shape in one paragraph.** One new discovery primitive,
`list_shell_surfaces()`, sitting exactly where `list_apps()` sits, returning
kind-tagged *real platform roots* — no synthetic nodes. One new handle type,
`ShellSurface`, shaped like `App` (kind + name + pid + a root, with
`locator()` / `tree()` / `dump()`). One new enum, `ShellSurfaceKind`. Nothing
else is new: selectors, locators, auto-wait, actions, `tree`/`dump`,
`Element`, `Diagnosis`, and every error variant are reused unchanged. The CLI
gains one command (`xa11y shell`) and one flag (`--shell KIND`); MCP gains one
tool (`shell`) and one parameter on `tree`/`find`/`action`. Nothing existing
changes behaviour: `App::list`, `xa11y apps`, rootless locators, and app
trees all stay exactly as they are.

---

## 1. Why not the issue's `system_menu_bar() -> Element`

The issue proposed a single provider primitive returning one synthetic
`MenuBar` root. The author flagged it as unverified and suggested measuring
first; the report did, and the measurement argues against it (§6.1):

- **No platform has one such object.** Windows shell UI is ≥6 unrelated
  top-level windows (§2.1). Linux is N `window-type:dock` frames across N
  panel processes (§3.1). macOS — the platform the name came from — is the
  strongest counterexample: the menu bar is *per application* (22 of 67
  processes vend one, §4.7), status items fan out over N processes (§4.1),
  and the Dock, desktop, and Control Center are three more surfaces in three
  more processes.
- **A synthetic root invents a parent that exists nowhere** and would have to
  merge unrelated windows under it — a fabricated element in a library whose
  tenet 2 is "only expose what accessibility APIs support". Every surface is
  already an ordinary element in the ordinary platform tree (§1); the only
  thing missing is a way to *find* them, because per-platform root filters
  drop them.
- **One root can't carry the per-surface contracts.** Whether enumeration
  mutates the screen is a property of the individual surface, not the
  platform (§2.3, §4.4): Windows overflow icons don't exist until the chevron
  is pressed, macOS NSMenu status items are fully readable closed, Control
  Center is not. A single tree with one contract would be wrong somewhere on
  every platform.

So the feature is discovery plus naming, not a new tree shape. That is also
the smallest possible feature, which suits the constraints: the entire query
and action pipeline already works on these elements once you can reach them.

## 2. Design principles

1. **Real roots only.** Every `ShellSurface` wraps an element the platform
   itself vends (a UIA pane, an `AXMenuBar`, an AT-SPI frame). xa11y adds a
   tag, never a node.
2. **Reuse the `App` pattern.** Discovery primitive on `Provider`, handle
   type in core, `Ext` trait on the umbrella crate, `--app`-style targeting
   in the CLI and MCP. A new binding user who knows `App` should be able to
   guess the whole surface.
3. **Additive and inert by default.** No existing enumeration, selector, or
   tree changes. Shell surfaces are visible only to code that asks for them.
4. **Mutation is always the caller's explicit act.** Enumerating surfaces and
   dumping their trees never opens, closes, focuses, or presses anything.
   Where content only exists after a press (tray overflow, Control Center),
   the caller performs that press on a real, advertised element and then
   re-enumerates. The API has no verb that presses on the caller's behalf.
5. **Honest per-surface, not per-platform.** Capability differences (pid
   attribution, stable ids, read-only enumeration) are documented per kind
   and surfaced through existing fields; failures use existing error
   variants with a `Diagnosis`. No new error variants are needed.

## 3. Core API

### 3.1 `ShellSurfaceKind`

```rust
/// What kind of OS shell surface a `ShellSurface` is.
///
/// `#[non_exhaustive]`: backends map platform → kind, never the reverse
/// (the same direction as `Role`), and the set grows as more shell UI is
/// classified — Start menu, jump lists, secondary taskbars, widget boards.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
         strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ShellSurfaceKind {
    /// The system menu bar: the frontmost application's menus, Apple menu
    /// included. macOS only — Windows and Linux have no equivalent object.
    MenuBar,
    /// One process's status items (`AXExtrasMenuBar`). One surface per
    /// owning process; macOS only. The Windows tray has no per-app hosting,
    /// so tray icons live inside the `Taskbar` surface instead.
    StatusItems,
    /// The Windows taskbar (`Shell_TrayWnd`), including the task band, the
    /// visible tray row, and the overflow chevron.
    Taskbar,
    /// A Linux desktop panel or dock: an AT-SPI frame carrying the
    /// `window-type:dock` attribute. One surface per frame.
    Panel,
    /// The macOS Dock.
    Dock,
    /// The desktop icon surface: `Progman`'s list view on Windows, Finder's
    /// desktop scroll area on macOS.
    Desktop,
    /// A transient shell window that exists only while open: the tray
    /// overflow flyout, Quick Settings, Notification Center, a shell
    /// context-menu popup, an opened Control Center panel.
    Flyout,
    /// A shell-owned window the backend could not classify. The documented
    /// fallback, like `Role::Unknown` — present so a new OS surface degrades
    /// to "reachable but untagged" rather than invisible.
    Unknown,
}
```

Following the `Role` precedent exactly: the enum crosses every binding as its
`snake_case` string (`"menu_bar"`, `"status_items"`, …), identical spelling
in Python and JS per the Binding Shape Conventions, converted mechanically
via `to_snake_case` — so, like `Role` and unlike `Error`/`EventKind`, it
needs **no** `[[types.variant_coverage]]` entry (**amended post-review, see
§12.4**). Downstream matches carry a
`_` arm; per the extensibility table's test, a new variant requires no work
in another crate (backends produce kinds, nothing must consume them
exhaustively), so `#[non_exhaustive]` is the right choice, not exhaustive.

### 3.2 `Provider::list_shell_surfaces`

```rust
/// Enumerate OS shell surfaces — taskbars, panels, docks, menu bars,
/// status items, the desktop, and any transient shell flyout currently
/// open. One entry per surface, each a real platform element usable as a
/// search root.
///
/// The listing is live: transient surfaces (`Flyout`) appear only while
/// they are on screen, and enumerating NEVER opens, closes, or presses
/// anything. Required — no default impl, for the same reason `list_apps`
/// has none: enumeration is platform-specific, and a silent empty default
/// would hide an unimplemented backend (tenet 1).
fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>>;
```

Notes:

- **Typed pair, not a kind smuggled through `raw`.** The kind is part of the
  primitive's contract, so it belongs in the signature where the compiler
  sees it. Core (`ShellSurface::list_with`) additionally stamps
  `raw["shell_kind"] = "<snake_case>"` on the surface root in one place, so
  the kind shows up in `tree`/`dump` output and is matchable through the
  existing raw-attribute selector fallback (`[shell_kind='taskbar']`) with
  zero selector-engine changes and no per-backend drift.
  **Amended post-review — see §12.1:** the stamp lands, but the selector
  claim does not hold for a locator rooted at the surface.
- **This is a new required trait method**, which breaks out-of-tree
  `Provider` implementations. That is the same accepted cost as a new
  `ElementParts` field — the trait is `#[doc(hidden)]` on the umbrella crate
  and intra-workspace deps are `=`-pinned precisely so this class of change
  is a loud compile error, not drift.
- The mock provider implements it (returning fixtures / empty), which is what
  lets the CLI and MCP surfaces be unit-tested off a real desktop, exactly as
  `apps`/`find`/`action` are today.

### 3.3 `ShellSurface`

Shaped on `App` — public data fields, private provider, the same query
methods, no `Element` inheritance:

```rust
/// A shell surface: one OS-owned top-level accessibility root, tagged with
/// what it is. The entry point for shell queries, as `App` is for
/// application queries.
pub struct ShellSurface {
    /// What this surface is.
    pub kind: ShellSurfaceKind,
    /// Human-readable name: the owning app for per-app surfaces ("Safari"
    /// menu bar, "Arq" status items), the platform's own name otherwise
    /// ("Taskbar", "Dock").
    pub name: String,
    /// Owning process where the platform reports one honestly. On macOS
    /// this is always the true owner (§4.3). On Windows it is the *host*
    /// (explorer.exe / ShellHost.exe) because UIA carries no per-icon
    /// owner (§2.2) — documented as the host, never faked. On Linux it is
    /// the panel process.
    pub pid: Option<u32>,
    /// The surface's root element data.
    pub data: ElementData,
    provider: Arc<dyn Provider>,
}

impl ShellSurface {
    pub fn list_with(provider: Arc<dyn Provider>) -> Result<Vec<Self>>;
    pub fn by_kind_with(provider: Arc<dyn Provider>,
                        kind: ShellSurfaceKind,
                        timeout: Duration) -> Result<Self>;

    pub fn locator(&self, selector: &str) -> Locator;   // rooted at the surface
    pub fn children(&self) -> Result<Vec<Element>>;
    pub fn tree(&self, max_depth: Option<usize>) -> Result<TreeNode>;
    pub fn dump(&self, max_depth: Option<usize>) -> Result<String>;
    pub fn as_element(&self) -> Element;
}
```

- `locator()` is `Locator::new(provider, Some(self.data), selector)` — the
  identical one-liner `App::locator` is. Everything downstream (auto-wait,
  `wait_*`, actions, nth, alternation, diagnosis) is inherited, not
  reimplemented.
- `by_kind_with` polls with the standard `poll_lookup` loop
  (`SelectorNotMatched` = "not yet", `Duration::ZERO` = single attempt), and
  its terminal diagnosis lists the surfaces that *were* found, mirroring
  `running_apps_diagnosis`. Its main job is the flyout workflow: press the
  chevron, then `by_kind(Flyout, timeout)` waits for the overflow window to
  materialise (§2.3) without a hand-rolled loop.
- **Ambiguity is refused, not first-matched.** If more than one surface of
  the kind exists (several `StatusItems` processes, several `Panel` frames,
  a second `Flyout`), `by_kind_with` returns `SelectorNotMatched` with a
  diagnosis listing every candidate (kind, name, pid) and the instruction to
  disambiguate via `list()` + pid. `App::by_name` takes first-match for
  historical reasons; for a surface whose consumers are mostly agents, a
  refusal that names the candidates is worth the inconsistency — it is the
  same call the MCP `action` tool already made with `ambiguous_selector`.
- **`ShellSurface` is not `App`.** Reusing `App` looked cheaper (zero new
  types) but was rejected: `App` is documented and typed as "a running
  application", `App::by_name`/`find`/`foreground` semantics don't transfer,
  a `kind` field on `App` would be `None` for every real app, and shell
  entries leaking into code that iterates `App::list()` is exactly the
  breaking change this design avoids.

### 3.4 Umbrella crate

Mirroring `AppExt`:

```rust
pub trait ShellSurfaceExt: Sized {
    /// List shell surfaces using the global singleton provider.
    fn list() -> Result<Vec<Self>>;
    /// Wait for exactly one surface of `kind`. `Duration::ZERO` = one
    /// attempt. Errors with a candidate diagnosis when several match.
    fn by_kind(kind: ShellSurfaceKind, timeout: Duration) -> Result<Self>;
}
```

```rust
// The Windows overflow workflow, end to end, with no new verbs:
let taskbar = ShellSurface::by_kind(ShellSurfaceKind::Taskbar, Duration::ZERO)?;
taskbar.locator("button[name='Show Hidden Icons']").press()?;   // real Invoke (§2.3)
let flyout = ShellSurface::by_kind(ShellSurfaceKind::Flyout, Duration::from_secs(3))?;
flyout.locator("button[name*='Tailscale']").press()?;
```

## 4. Kind semantics per platform

| Kind | Windows | macOS | Linux |
|---|---|---|---|
| `menu_bar` | — | frontmost app's `AXMenuBar`; name/pid = owning app | — |
| `status_items` | — | one per process with a live `AXExtrasMenuBar`; name/pid = owner | — |
| `taskbar` | `Shell_TrayWnd` pane: task band + visible tray + chevron; pid = explorer (host) | — | — |
| `panel` | — | — | each AT-SPI frame with `window-type:dock`; pid = panel process |
| `dock` | — | the Dock process's application element | — |
| `desktop` | `Progman` → `SysListView32` (amended post-review, §12.2) | Finder's `AXScrollArea` desc="desktop" (already in `AXChildren`, §1) | — |
| `flyout` | tray overflow, Quick Settings, Notification Center, `PopupHost` shell menus — while open | shell processes' `AXSystemDialog` windows (opened Control Center / NC panels) — while open | — |

Absence from a column is honest scope, not failure: `list()` on Linux simply
returns no `taskbar`, and asking `by_kind(Taskbar, _)` there fails with
`SelectorNotMatched` naming what was found. `Error::Unsupported` is reserved
for the genuine protocol gap (§3.2 of the report): Linux StatusNotifierItem
*menus* live on `com.canonical.dbusmenu`, a different subsystem from AT-SPI,
and stay out of scope with a documented pointer to a future issue — exactly
as #374 anticipated, narrowed to the one thing actually unavailable (panels
and XEmbed icons are reachable today; SNI icons are pressable today).

### Per-platform implementation notes (sketch, from the measurements)

**Windows.** Classify UIA desktop-root children by class name:
`Shell_TrayWnd` → `taskbar`, `Progman` → `desktop`;
`TopLevelWindowForOverflowXamlIsland`, `ControlCenterWindow` (content
non-offscreen only, §2.5), `Windows.UI.Core.CoreWindow` (ShellExperienceHost),
`Microsoft.UI.Content.PopupWindowSiteBridge` → `flyout`. No dual-discovery
region walk is needed under this shape: tray icons are found by walking the
taskbar surface's UIA children, which works identically for the Win11 XAML
tree and a Win10 layout — the Win7–10 named regions are just where the walk
finds the icons on older builds, and the empty-husk regions of Win11 (§2.2)
are simply empty containers in a tree whose icons are elsewhere, not a
special case to detect. `visible` vs `overflow` needs no new field: it falls
out of *which surface* an icon is under, corroborated by `stable_id`
(`SystemTrayIcon` vs `NotifyItemIcon`, §2.3).

Action fidelity on shell chrome follows the report's §6.4: macOS measured
zero false successes, so stripping verbs globally would punish two platforms
for one. On Windows, `show_menu` is advertised only where the platform
implements it (Win32 shell UI: desktop list items — measured working) and
omitted on XAML shell chrome, where `ShowContextMenu` returns `S_OK` and
no-ops (§2.4c) — an advertised verb that silently does nothing is the exact
tenet-3 violation. The tray-icon context menu therefore remains an explicit,
documented `InputSim` right-click composition, never a fake `show_menu`.
Driving Start / Search / Task View is out of scope for v1: their only
reachable verb is a `Toggle` that measurably lies (§2.4b), and they need
foreground activation that headless sessions deny.

**macOS.** `menu_bar`: resolve the frontmost app (existing `focused_app`)
and return its `AXMenuBar` attribute value. `status_items`: fan out over
`NSWorkspace.runningApplications`, probe `AXExtrasMenuBar` with
`AXUIElementSetMessagingTimeout` set **on the queried element** to ~250 ms
(§4.8.6 — the default is ~1.5 s *per attribute*, and four wedged WebContent
processes cost 6 s of a 6 s scan), and distinguish nil-because-absent from
nil-on-timeout by error code (§4.8.6). `dock`/`desktop`: fixed well-known
processes. This deliberately does **not** touch `list_gui_apps()`: the scan
is its own walk, so the §4.8.5 app-list union question stays a separate,
optional follow-up, and `App::list()` is byte-for-byte unchanged.

**Linux.** Filter the existing AT-SPI application enumeration to frames
carrying `window-type:dock` (§3.1) — one surface per frame, so xfce4-panel's
panel and dock rows become two `panel` surfaces with honest pids.

## 5. The macOS `AXMenuBar` child filter stays

§4.8 measured what lifting `ax.rs:1759` would do: safe for role-scoped
selectors (zero same-scope collisions across nine apps, §4.8.4), eager and
read-only (§4.8.3), but a median 3.7× `tree()` growth, 72 new cross-role
name collisions for unscoped selectors, and unbounded user-data exposure
(2244 nodes of one user's bookmarks — 52% of Safari's tree, §4.8.2).

This proposal keeps the filter. The `menu_bar` and `status_items` surfaces
reach the same elements through a root the caller asked for by name, which
delivers everything #374 requested with **zero** change to existing app
trees, dumps, selector behaviour, or their size. Lifting the filter (and the
possible `App::menu_bar()` accessor for a *background* app's menus) remains
a separate decision the report has already de-risked — it composes cleanly
with this design later, and nothing here presumes it.

## 6. CLI

```
xa11y shell                      # list surfaces: KIND\tPID\tNAME
xa11y tree   --shell KIND [--pid PID]
xa11y find   SELECTOR --shell KIND [--pid PID]
xa11y action ACTION SELECTOR --shell KIND [--pid PID]
```

`--shell` joins `--app`/`--pid` in the target-resolution options and is
mutually exclusive with `--app`; `--pid` alongside `--shell` disambiguates
same-kind surfaces (usage error with the candidate list otherwise, exit code
2 per the existing contract — **amended post-review, see §12.3**). `xa11y
shell` mirrors `xa11y apps`' tab-column
contract. One implementation in `cli.rs`, reached by all three launchers, per
One CLI Three Launchers; the resolution helper is value-producing so MCP can
share it (stdout-is-the-wire rule).

## 7. MCP

One new tool and one new parameter, with contracts in the descriptions per
the "a tool description is a contract the handler keeps" rule:

- **`shell`** — lists surfaces as `{kind, name, pid}` rows. The description
  states: the listing is live; `flyout` surfaces exist only while open;
  enumeration never opens or presses anything; on Windows, hidden tray icons
  exist only inside the overflow flyout — press the taskbar's "Show Hidden
  Icons" button, then call `shell` again and target the flyout. Spelling
  those steps out is the point: an agent should not have to discover the
  mutation model by experiment.
- **`tree` / `find` / `action`** gain an optional `shell` parameter (the
  kind string), mutually exclusive with `app`, combinable with `pid`.
  Ambiguity refuses with an `ambiguous_shell_surface` failure kind carrying
  the candidate list — the same behaviour `action` already has for
  `ambiguous_selector`.
- Results stay bounded by the existing depth/node/count limits, which
  matters more here than anywhere: a menu-bar tree is user-data-sized
  (§4.8.2), so the truncation-and-say-so machinery is load-bearing, and
  already built.
- No new `Error` variants means no `failure_kind` additions and no parity
  churn in `bindings/parity_allowlist.toml`'s variant coverage for `Error`.

## 8. Bindings

Python:

```python
surfaces = xa11y.ShellSurface.list()
bar = xa11y.ShellSurface.by_kind("menu_bar", timeout=2.0)   # seconds
bar.locator("menu_item[name='Save']").press()
bar.kind, bar.name, bar.pid                                  # "menu_bar", "Safari", 4021
```

JS:

```js
const bar = await ShellSurface.byKind('menu_bar', { timeout: 2000 })  // ms
await bar.locator("menu_item[name='Save']").press()
```

- `kind` crosses as the identically-spelled snake_case string in both
  bindings (value-enum convention; it is a value a user writes as a literal,
  not a destructured payload).
- Timeouts follow each language's unit convention (seconds / ms).
- Parity: `ShellSurface` classified `mirrored`; `ShellSurfaceKind` `opaque`
  (crosses as a string, like `Role`); `list_with`/`by_kind_with` get
  `rust_only` entries naming the singleton-provider fold, exactly as the
  `App` constructors do. The rustdoc-driven check makes forgetting any of
  this a build failure, which is the system working.
- JS: `ShellSurface` is declared by hand in `index.d.ts` (the shadowing
  rule), the `kind` parameter/property narrowed to a `ShellSurfaceKindName`
  literal union via a guarded `patch-native-dts.mjs` entry, and the union
  exported from `index.d.ts`.
- `strands-xa11y` / `pytest-xa11y` need nothing: no exception class,
  diagnosis attribute, or existing method changes, so
  `check_real_surface.py` stays green by construction.

## 9. Testing

- **Unit / mock:** the mock provider ships fixture surfaces; CLI + MCP
  targeting, ambiguity refusal, and the `shell` tool run per-launcher in
  `tests/suites/cli/test_mcp.py`, plus both MCP suites (SDK and raw-JSON)
  per the dual-suite rule.
- **Linux integ:** the report's container (xfce4-panel + dual-protocol tray
  probe) becomes a fixture: assert two `panel` surfaces, the clock button
  found by locator, `press` on the SNI icon, and the XEmbed icon's honest
  `actions=[]`. Runs headless in the existing container job.
- **Windows integ:** the report's disconnected-session control group (§0)
  says what CI can assert: `taskbar` and `desktop` surfaces exist, the
  chevron press opens a `flyout` that `by_kind` finds, Quick Settings
  content, `NotifyItemIcon` vs `SystemTrayIcon` stable ids. Start-menu
  surfaces are exactly what a headless session cannot open — excluded from
  scope above, so not asserted.
- **macOS integ:** assert only the Apple-stable facts (§6.8): a `dock`
  surface with `AXApplicationDockItem` subroles, a `menu_bar` surface whose
  first item is the Apple menu, `desktop` present, `com.apple.menuextra.*`
  stable ids. Third-party status items are the runner's software inventory
  — never asserted. One caveat the report makes unmissable: a locked-screen
  runner corrupts window trees while leaving menu bars intact (§4.8.7), so
  the macOS suite should assert unlocked-ness first, and the cycle guard
  §4.8.7 calls for in `get_children` should land with (or before) this
  feature.
- All integration tests `#[ignore]`d and run via `cargo xtask test-integ`,
  per the standing policy.

## 10. Explicitly out of scope (with pointers)

| Deferred | Why | Where it lives |
|---|---|---|
| Lifting the macOS `AXMenuBar` child filter from app trees | separate, de-risked decision; §5 above | report §4.8.8 |
| `list_gui_apps()` union (naming accessory apps in `App::list`) | not needed by the surfaces path; additive follow-up | report §4.8.5 |
| Linux SNI menus | `com.canonical.dbusmenu`, not AT-SPI; future issue as #374 suggested | report §3.2 |
| Driving Start / Search / Task View | only reachable verb measurably lies; needs foreground activation | report §2.4b |
| Post-verified `show_menu` on XAML shell chrome | v1 takes "don't advertise" (report option *a*); revisit with option *b* (verify a popup appeared, application-scoped on macOS) if omission proves too coarse | report §6.4, §4.5 |
| Events (`subscribe`) on surfaces | works per-process on macOS today via `App`; a surface-level story needs its own design | — |
| Jump lists, widgets, lock screen, multi-monitor taskbars, GNOME/KDE panels | `#[non_exhaustive]` kind enum absorbs them later | report §2.5, §3 |

## 11. Open questions for review

1. **`by_kind` in v1, or `list()` only?** `list()` alone is smaller;
   `by_kind` earns its place through the flyout wait and the ambiguity
   refusal. Proposal includes it; cutting it loses nothing structural.
2. **Kind granularity of `flyout`.** One catch-all transient kind is minimal
   and honest, but `notifications` / `quick_settings` may deserve their own
   tags once agents target them regularly. `#[non_exhaustive]` makes the
   split non-breaking later; starting split is also cheap. Default: start
   with `flyout`, split on demand.
3. **Naming.** `ShellSurface` / `shell` follows the report's vocabulary and
   avoids overloading "app". Alternatives considered and dropped:
   `SystemSurface` (vague), reusing `App` (§3.3).

## 12. Amendments (post-review)

This proposal was approved as written. Implementation review then found three
places where the approved text and the landed behaviour disagree, and in each
the implementation is the one that is right. The sections above are left
standing with a pointer here, so the record shows what changed and why rather
than reading as if it had always said this.

### 12.1 `raw["shell_kind"]` is readable, not selector-matchable (amends §3.2)

§3.2 claims the stamped kind "is matchable through the existing raw-attribute
selector fallback (`[shell_kind='taskbar']`)". The stamp itself is real and
landed as described: `ShellSurface::list_with` writes the snake_case kind onto
the surface root's raw map in one place, and it appears in `tree` and `dump`
output.

The selector half does not follow. A locator rooted at a surface searches that
surface's descendants; the root is not among its own candidates, so
`surface.locator("[shell_kind='taskbar']")` cannot match the surface it is
rooted at. Every surface root carries the attribute and no descendant does,
which leaves the raw key useful for reading and inspection and useless as a
filter from the one root it is stamped on. The attribute is therefore
documented as a read — `surface.as_element().raw["shell_kind"]` — with no
selector promise attached.

### 12.2 The Windows `desktop` surface root is the list view (amends §4)

§4's Windows `desktop` cell reads `Progman` → `SysListView32`, which names the
walk without saying which element becomes the surface root; §3's
per-platform sketch says "`Progman` → `desktop`", which reads as the root
being `Progman` itself.

The landed behaviour resolves it to the list view. The backend walks
`Progman` → `SHELLDLL_DefView` → `SysListView32` and uses the `SysListView32`
element as the surface root, because that is the element that actually holds
the desktop icons; `Progman` is a container two levels above them. When that
chain is absent — a shell configuration with no desktop list view — the
backend contributes no `desktop` surface at all rather than falling back to
`Progman`, which would hand callers a root with nothing in it (tenet 1).

### 12.3 Ambiguity is an operation failure, exit code 1 (amends §6)

§6 specifies exit code 2, a usage error, when `--shell KIND` matches several
surfaces. The landed behaviour is exit code 1, an operation failure, and it
should be.

An argument error is one the caller can fix by reading their own command line.
`--shell flyout` is a well-formed invocation; whether it matches zero, one, or
four surfaces is a fact about the desktop at that moment, and it can differ
between two identical invocations a second apart. This is the case the CLI
already had in `CliError::Ambiguous` for a selector matching several elements
where one was required, and that is exit code 1. Splitting the two would have
meant a script treating "your flags are wrong" and "the desktop had two
panels" as the same class of failure, or as different classes depending on
which of the two ambiguities it hit.

The exit codes for `--shell` therefore read:

| Condition | `CliError` variant | Exit |
|---|---|---|
| several surfaces of the kind | `AmbiguousShellSurface` | 1 |
| no surface of the kind | `Xa11y(SelectorNotMatched)` | 1 |
| `--shell` together with `--app` | `Usage` | 2 |
| kind string is not a known kind | `Usage` | 2 |

Both exit-1 cases carry a `Diagnosis` listing the surfaces that were
enumerated, so the disambiguating pid — or the absence of any candidate — is
readable off the failure (tenet 6).

One consequence is worth stating, because §6 implies otherwise by offering
`--pid` as the general remedy: `--pid` is the only lever, and it does not
always separate the candidates. One Linux panel process can own several
`panel` frames, which are then several surfaces with one pid. The ambiguity
message distinguishes the two situations — "N surfaces are present; `xa11y
shell` lists their pids" when no pid was given, and "N surfaces share pid P;
this operation cannot pick between them" when one was and did not narrow it —
rather than repeating advice the caller has already taken.

### 12.4 `ShellSurfaceKind` does get variant coverage (amends §3.1)

§3.1 argued the enum needs no `[[types.variant_coverage]]` entry "like
`Role`". Review showed the analogy fails: `Role` has no hand-maintained
closed list anywhere, while the kind strings ended up spelled out in the
`.pyi` and `patch-native-dts.mjs` literal unions. The implementation
therefore single-sources every derivable list from a new
`ShellSurfaceKind::ALL` (the CLI/MCP enum and both bindings' parse errors),
guards `ALL` itself with an exhaustive-match test, and covers the two
remaining hand-written unions with a `[[types.variant_coverage]]` entry — so
adding a variant without updating them fails the parity check instead of
shipping a kind that TypeScript and schema-validating MCP clients reject.
