# xa11y Library Review Report

## 1. Public Interface Overview

The library has a clean, layered architecture:

- **`create_provider() -> Box<dyn Provider>`** — single entry point, returns platform-appropriate implementation
- **`Provider` trait** — `get_app_tree()`, `get_all_apps()`, `perform_action()`, `check_permissions()`, `list_apps()`
- **`EventProvider` trait** — `subscribe()`, `wait_for_event()`, `wait_for()`
- **`Tree`** — flattened DFS-ordered snapshot with selector queries
- **`Node`** — 13 fields (id, role, name, value, description, bounds, bounds_normalized, actions, states, children, parent, depth, app_name, raw)
- **38 Roles**, **11 Actions**, **13 EventKinds**, **9 StateSet fields**
- **CSS-like selector engine** — `button[name="Submit"]`, `toolbar > text_field`, `:nth(2)`

---

## 2. Comparison to Similar Libraries

### Role Coverage

| Library | Roles | Notes |
|---|---|---|
| **xa11y** | 38 | Lean, covers common UI elements |
| **AccessKit** | 182 | Exhaustive — ARIA, DPUB, graphics, PDF, input variants |
| **AT-SPI** | 131 | Broad Linux/GNOME coverage |
| **UIA (Windows)** | ~40 ControlTypes | Similar count to xa11y |
| **macOS AX** | ~50 roles + subroles | Moderate, uses subroles for specialization |

**Assessment:** 38 roles is reasonable for a cross-platform abstraction — the "common denominator" approach. Missing roles that may matter: `SpinButton`/`Switch` (distinct from checkbox), `Tooltip`, `Status`/`Log` (live regions), input type variants (`SearchInput`, `PasswordInput`), and document structure roles (`Heading` levels, `Article`, `Navigation`, `Banner`). The biggest gap vs AccessKit is **live region roles** and **document semantics** — if xa11y is meant to support web content testing, these will matter.

### Action Coverage

| Library | Actions | Model |
|---|---|---|
| **xa11y** | 11 enum variants | Flat enum + optional ActionData |
| **AccessKit** | 24 variants | Flat enum + ActionData |
| **AT-SPI** | Per-interface methods | Interface-based (Action, EditableText, Value, Selection) |
| **UIA** | Control Patterns | Pattern objects with multiple methods each |
| **macOS AX** | 9 named strings | `AXPress`, `AXIncrement`, etc. |
| **Playwright** | ~15 locator methods | Auto-waiting, high-level |

**Assessment:** The 11 actions cover the essentials well. Gaps vs AccessKit: `Blur`, `ReplaceSelectedText`, `SetTextSelection`, `ScrollToPoint`, `SetScrollOffset`, `ShowTooltip`/`HideTooltip`, `CustomAction`. The most impactful missing one is probably **`SetTextSelection`** — for text editing automation, being able to select a range is important. `Blur` is also useful for triggering validation.

### State Coverage

| Library | States | Model |
|---|---|---|
| **xa11y** | 9 fields in StateSet | Typed struct with `Option<Toggled>` for tri-state |
| **AT-SPI** | 44 bit flags | Bitmask |
| **UIA** | Pattern-specific | Scattered across patterns |
| **AccessKit** | ~20 bool methods + enums | Individual getters |

**Assessment:** The `StateSet` is clean and practical. The `Option<Toggled>` for `checked` and `Option<bool>` for `expanded` are well-designed — they distinguish "not applicable" from "false", which AT-SPI's bitmask doesn't. Missing states that could matter: `focusable` (vs `focused`), `modal`, `read_only` (vs `editable`), `pressed` (for toggle buttons, distinct from checked), `has_popup`.

### Event Coverage

| Library | Events | Subscription Model |
|---|---|---|
| **xa11y** | 13 EventKinds | `subscribe()` → `Subscription` with RAII cleanup |
| **AT-SPI** | ~50+ event types | D-Bus signals via `Registry.registerEventListener()` |
| **UIA** | 4 categories, many subtypes | `AddAutomationEventHandler()` with TreeScope |
| **macOS AX** | ~30 notifications | `AXObserverAddNotification()` + RunLoop |
| **Playwright** | Page-level only | `page.on()` — no element-level a11y events |
| **Selenium/WebDriver** | None | Polling only |

**Assessment:** The 13 event kinds are a solid normalized subset. The `EventFilter` with selector support is a nice touch — AT-SPI and UIA don't have built-in selector-based filtering. Missing vs platform APIs: `TextChanged` (text insertion/deletion with ranges — important for editors), `BoundsChanged`/`Moved`/`Resized`, `ActiveDescendantChanged` (for virtual lists/grids). The text events are the biggest gap for text editing scenarios.

---

## 3. Comparison to AccessKit

AccessKit and xa11y are complementary:

| Aspect | AccessKit (provider-side) | xa11y (consumer-side) |
|---|---|---|
| **Purpose** | Make apps accessible TO ATs | Read accessibility FROM apps |
| **Data flow** | App → platform → AT | AT ← platform ← app |
| **Tree model** | Push-based `TreeUpdate` (incremental) | Pull-based snapshots |
| **Node identity** | App-assigned `u64`, stable across updates | Sequential `u32`, snapshot-local |
| **Action handling** | Receives actions via `ActionHandler` trait | Dispatches actions via `perform_action()` |

**Data model alignment:** The `Node` is simpler than AccessKit's `Node` (which has ~100+ properties), but that's appropriate — xa11y is reading a normalized view, not authoring a complete accessibility description. The `Role` enum should ideally be a superset of what can actually be encountered from any platform, while AccessKit's needs to cover what apps can declare. Consider aligning the `Role` enum more closely with AccessKit's for interoperability — but only for roles that platform APIs actually expose to consumers.

**Identity gap:** AccessKit uses `NodeId(u64)` that the app assigns and keeps stable across tree updates. xa11y's `NodeId = u32` is reassigned each snapshot. If xa11y ever needs to correlate nodes across snapshots (e.g., for diffing), a richer identity model is needed (see section 7).

---

## 4. Data Format Sensibility

**Strengths:**
- **Flattened `Vec<Node>` in DFS order** — excellent for serialization, FFI, and deterministic traversal. Much better than pointer-based trees for cross-language bindings.
- **`Option<>` for inapplicable states** (`checked: Option<Toggled>`, `expanded: Option<bool>`) — correctly models "not applicable" vs "off".
- **`NormalizedRect`** alongside pixel `Rect` — great for resolution-independent comparisons and ML/vision model integration.
- **`RawPlatformData`** opt-in — clean separation between normalized and platform-specific data.
- **All types derive `Serialize`/`Deserialize`** — ready for FFI and IPC.

**Concerns:**
- **`NodeId = u32`** is sequential DFS index, which means it's fragile: inserting one element shifts all subsequent IDs. This is fine within a snapshot but makes cross-snapshot correlation impossible by ID alone. Consider adding an optional `platform_id` or `stable_id` field that carries the platform's native stable identifier (macOS `AXIdentifier`, Windows `AutomationId`, AT-SPI `AccessibleId`).
- **`app_name` on every Node** feels redundant — it's already on the `Tree`. Consider removing it from `Node` or making it only present on the root.
- **No `text_value` vs `numeric_value` distinction on Node** — `value` is `Option<String>`, but sliders have numeric values. The `ActionData` has `Value(String)` and `NumericValue(f64)` for setting, but reading always returns a string. Consider `numeric_value: Option<f64>` on Node for symmetry.
- **No `min_value`/`max_value`** for range controls (sliders, progress bars, spinners). AccessKit, AT-SPI, UIA, and macOS AX all expose these. If someone's automating a slider, they need to know the range.

---

## 5. Cross-OS Abstraction Quality

**Well done:**
- The `Provider` trait is minimal and clean — 5 methods cover the entire surface area.
- `AppTarget` with `ByName`/`ByPid`/`ByWindow` covers the practical ways users identify apps.
- `WindowHandle` correctly wraps platform-specific handle types.
- Role mapping is consistent — each platform maps its native roles to the shared enum.
- `PermissionStatus::Denied { instructions }` is a nice UX touch.

**Risks:**
- **`ScrollIntoView` is a no-op on macOS** — this asymmetry should be documented or the action shouldn't be reported as available on macOS nodes.
- **`Toggle` and `Press` map to the same platform action** on macOS (`AXPress`). If they're semantically identical on the platform, should the node report both? Or should the library normalize this so `Toggle` only appears on checkboxes/switches?
- **`include_raw: true` is required for action dispatch on macOS** — this is a surprising requirement that should probably be internalized (always capture the raw handles needed for action dispatch, independently of the user-facing `include_raw` flag).

---

## 6. Action Completeness

The 11 actions cover the core UI interactions well. What's missing for real automation scenarios:

| Missing Action | Why It Matters |
|---|---|
| `SetTextSelection(start, end)` | Text editing automation, copy/paste workflows |
| `Blur` / `ClearFocus` | Trigger validation, move focus away |
| `TypeText(string)` | Character-by-character input (vs `SetValue` which replaces) |
| `DragTo(target)` | Drag-and-drop workflows |
| `ScrollTo(direction, amount)` | `ScrollAmount` ActionData exists but there's no `Scroll` action |
| `CustomAction(id)` | Apps can define custom actions (e.g., "Reply", "Archive") |

The `ActionData` enum includes `ScrollAmount` and `Point`, but it's not clear which `Action` variant they pair with. `ScrollIntoView` is the only scroll action, but `ScrollAmount` implies a generic scroll. This should be clarified or a `Scroll` action added.

---

## 7. Element Targeting Between Snapshots

This is the **staleness problem** — one of the hardest problems in accessibility automation.

### How other libraries handle it:

| Library | Approach | Staleness Risk |
|---|---|---|
| **Playwright** | **Lazy locators** — re-query on every action | **None** — the locator describes how to find, not what was found |
| **Selenium** | Live element references | **StaleElementReferenceException** — must re-find |
| **UIA** | `RuntimeId` + `AutomationId` for re-finding | Moderate — AutomationId is stable if developers set it |
| **AT-SPI** | `(bus_name, object_path)` — live D-Bus refs | Elements go `DEFUNCT` |
| **macOS AX** | `AXUIElementRef` — live refs | Ref invalidates on element destruction |

### How xa11y handles it today:

xa11y uses **snapshot-based** access. `NodeId` is a DFS index — it's **not stable across snapshots**. To re-find an element:

1. Take a new snapshot
2. Re-query using selectors: `tree.query("button[name=\"Submit\"]")`
3. Or use `find_by_role()` / `find_by_name()`

**This is partially Playwright-like** — selectors serve a similar purpose to Playwright's locators. But there's a key difference: **Playwright locators auto-resolve on every action**, while xa11y requires the user to manually re-snapshot and re-query.

### Is it possible and straightforward today?

**Possible:** Yes — selectors + re-snapshotting work.

**Straightforward:** Not quite. The workflow is:
```rust
// Find element
let tree = provider.get_app_tree(&target, &opts)?;
let btn = tree.query("button[name=\"Save\"]")?[0];

// Perform action
provider.perform_action(&tree, btn.id, Action::Press, None)?;

// Element might have changed — must re-snapshot
let tree2 = provider.get_app_tree(&target, &opts)?;
let btn2 = tree2.query("button[name=\"Save\"]")?; // might be gone now
```

This is correct but verbose. Compare to Playwright:
```js
const btn = page.getByRole('button', { name: 'Save' });
await btn.click(); // auto-resolves, auto-waits
await btn.click(); // still works, re-resolves
```

### Is solving it in scope?

**Yes, and the library is already halfway there.** The `wait_for()` method on `EventProvider` is exactly the Playwright `waitFor` pattern. What would complete the picture is a **`Locator` type** that wraps a selector + provider reference and auto-resolves:

```rust
// Hypothetical API
let save_btn = provider.locator(&target, "button[name=\"Save\"]");
save_btn.click()?;          // internally: snapshot → query → perform_action
save_btn.wait_visible()?;   // internally: poll/subscribe until visible
save_btn.click()?;          // re-resolves against fresh snapshot
```

This would be a thin layer over existing primitives. The `Locator` would hold `(AppTarget, String /*selector*/, QueryOptions)` and resolve lazily on each operation. The selector engine, action dispatch, and event system are all the building blocks.

**The `include_raw: true` requirement for action dispatch is a blocker for this pattern** — the Locator would need to always include raw data internally, even if the user didn't ask for it.

---

## 8. Events vs Playwright / WebDriver

### Playwright's event model:
- **Page-level events** only (`page.on('load')`, `page.on('dialog')`, etc.)
- **No element-level accessibility events**
- Instead uses **auto-waiting assertions**: `expect(locator).toBeVisible()` polls until true or timeout
- **ARIA snapshots**: `locator.ariaSnapshot()` returns YAML tree representation for assertions

### WebDriver's event model:
- **No event subscription at all**
- Uses **polling** via `WebDriverWait` + `ExpectedConditions`

### xa11y's event model:
- **Element-level accessibility events** — much richer than both Playwright and WebDriver
- `subscribe()` with `EventFilter` (by kind, selector, state flag) — more powerful than any web tool
- `wait_for_event()` — single event with timeout (like Playwright's `waitForEvent`)
- `wait_for()` — wait for element state (like Playwright's `expect(locator).toBeVisible()`)
- Events carry a **`target: Option<Node>`** snapshot of the triggering element

### Comparison:

| Feature | xa11y | Playwright | WebDriver | AT-SPI | UIA |
|---|---|---|---|---|---|
| Element-level events | Yes (13 kinds) | No | No | Yes (~50 types) | Yes (4 categories) |
| Selector-filtered events | Yes (`EventFilter.selector`) | N/A | N/A | No | TreeScope only |
| Wait-for-state | Yes (`wait_for()`) | Yes (`expect().toBeVisible()`) | Yes (`ExpectedConditions`) | Manual | Manual |
| Event carries element snapshot | Yes (`target: Option<Node>`) | N/A | N/A | Yes (`source` Accessible) | Yes (AutomationElement) |
| RAII subscription cleanup | Yes (drop `Subscription`) | N/A | N/A | Manual unregister | Manual remove handler |

**Assessment:** The event model is **significantly better than Playwright/WebDriver** for desktop accessibility automation. The selector-filtered subscription is a unique strength. The `wait_for()` method correctly emulates Playwright's assertion-waiting pattern.

**Gaps vs platform-native APIs:**
- No `TextChanged` events (AT-SPI has `object:text-changed:inserted/removed` with position and content)
- No `BoundsChanged` / `Moved` / `Resized` events
- No `ActiveDescendantChanged` (important for virtual scrolling lists)
- `target: Option<Node>` — why optional? If the target element is destroyed before the event is delivered, this could be `None`, but it should be documented

---

## 9. Summary of Recommendations

### High Priority
1. ~~**Add `stable_id: Option<String>`** to `Node`~~ — **DONE** (included in §10 refactor)
2. ~~**Decouple `include_raw` from action dispatch**~~ — **DONE** (included in §10 refactor)
3. **Add a `Locator` abstraction** — thin wrapper that auto-resolves selectors against fresh snapshots on each action, completing the Playwright pattern

### Medium Priority
4. ~~**Add `TextChanged` event kind**~~ — **DONE** (EventKind::TextChanged + TextChangeData with change_type and position)
5. ~~**Add `SetTextSelection` action**~~ — **DONE** (with ActionData::TextSelection { start, end })
6. ~~**Add `numeric_value: Option<f64>`, `min_value: Option<f64>`, `max_value: Option<f64>`** to Node~~ — **DONE** (populated from platform range APIs for sliders/progress bars/spinners)
7. ~~**Add `Scroll` action**~~ — **DONE** (pairs with existing ScrollAmount ActionData)
8. ~~**Add a few more roles**: `Switch`, `SpinButton`, `Tooltip`, `Status`, `Navigation`~~ — **DONE** (Heading already existed; all mapped to platform equivalents)

### Low Priority
9. ~~Add `focusable` and `modal` to StateSet~~ — **DONE**
10. ~~Add `Blur` action~~ — **DONE** (stub; returns ActionNotSupported on all platforms for now)
11. ~~Remove `app_name` from `Node`~~ — **DONE** (still on `Tree`)
12. ~~Document platform behavior asymmetries~~ — **DONE** (`ScrollIntoView` doc comment notes macOS no-op)

### Additional (automation foundation)
13. ~~**Add `TypeText` action**~~ — **DONE** (stub; input simulation, accepts ActionData::Value)
14. ~~**Add `DragTo` action**~~ — **DONE** (stub; accepts ActionData::Point for drop destination)
15. ~~**Remove `Selector` from public API**~~ — **DONE** (internals remain pub(crate))
16. **Implement Locator** — design finalized, implementation pending

