# Rich Accessibility: Identity, Semantics, Capture and Events

Design proposal for the open children of #475:

| Issue | Topic | Section |
|---|---|---|
| #481 | Act on an inspected node, with explicit identity and lifetime | [1](#1-identity-and-lifetime-481) |
| #476 | Document semantics and relationships | [2](#2-document-semantics-and-relations-476) |
| #482 | Bounded provider-level capture | [3](#3-bounded-capture-482) |
| #480 | Text, selection and live-region events | [4](#4-events-480) |
| #483, #531 | IA2 backend selection, one deadline | [5](#5-cross-cutting-capabilities-deadline-ia2) |

Status: every section is implemented, with the gaps listed in each
section's "Implemented" notes. Platform facts are from the
current code (file references are to `main` at `3603341`) and from
[Core-AAM 1.2](https://www.w3.org/TR/core-aam-1.2/) §3.4–3.5.

## Order of work

The four issues depend on each other. Settle them in this order:

1. **#481 identity first.** Capture nodes, relation targets and event targets
   are all "an element you can act on later". If identity is not settled,
   each of the other three invents its own reference.
2. **#476 semantics.** Adds fields to `ElementParts` (one compile break per
   backend) and a relation method. Capture and events then carry them for free.
3. **#482 capture.** Consumes 1 and 2. Absorbs the bounding logic that today
   lives only in the MCP `tree` tool, and is where #531's partial results go.
4. **#480 events.** Mostly independent; uses #481 for actionable targets.

## Things the research found that are wrong today

These are bugs or doc drift, independent of the new design. Fix them first or
alongside.

- **Unbounded handle caches.** macOS (`ax.rs:3582`) and Linux
  (`atspi.rs:66`, plus `action_indices` at `:69`) keep every element ever
  built until the provider drops. A long-running MCP server or test session
  grows without limit. Windows already caps at 65,536 (`uia.rs:49`).
- **`ElementStale` is never produced for a destroyed element** on macOS
  (`kAXErrorInvalidUIElement` becomes `Error::Platform`, `ax.rs:806-826`) or
  Windows (`UIA_E_ELEMENTNOTAVAILABLE` stays `Error::Platform`, `uia.rs:1594`).
  Its `selector` field never holds a selector; it holds `"handle:N"`.
- **Announcement text is thrown away on all three platforms**, although all
  three deliver it: AX `AXAnnouncementRequested` userInfo (observer created
  without `WithInfo`), UIA Notification (registered through the generic
  automation handler, so `displayString` is never seen), AT-SPI
  `Object:Announcement` `any_data`. The doc comment justifies this by UIA
  `LiveRegionChanged` having no text, which is a different event.
- **Event channels are unbounded** on every backend (`mpsc::channel`, sends
  are `let _ =`). Only the MCP drainer bounds and counts drops.
- **Doc drift:** `event.rs:112,130` say macOS/Windows do not emit
  `TextSelectionChanged` (both do). `design/events.md:542,550` predates the
  separate kind. `platform-details.mdx` role table claims a Windows landmark
  mapping for `Heading`/`Navigation`; no Windows code path produces
  `Navigation`.

---

## 1. Identity and lifetime (#481)

### What exists

The good news: **`Element` already acts on exactly the node it captured.**
Actions call `provider.press(&self.data)` with the captured handle and never
re-resolve the selector (`element.rs:389-709`). `Locator` re-resolves on
every wait; `Element` never does. So "a stale snapshot selects a similarly
named replacement" cannot happen through `Element` today.

What is missing:

| Gap | Today |
|---|---|
| A destroyed node fails as a typed stale error | Only Linux, and only on the action path guarded by `ensure_attached` (#491) |
| The node is still the *same* node, not a reused id | No check. AX element tokens and AT-SPI object paths (Qt uses pointer-derived paths) can be reused |
| Handle registry is bounded | Windows only |
| Two `Element`s can be compared | No `__eq__`/`__hash__` in either binding |
| Refer to a node across calls in MCP | Impossible: every tool takes a selector |

### Proposal

**The `Element` is the reference.** No new reference type in Rust, Python or
JS. Add three things to it, and one to MCP.

#### 1a. Identity: `same_as`, `__eq__`, `__hash__`

New provider method:

```rust
/// Platform identity of the node behind `element`, for equality only.
fn identity(&self, element: &ElementData) -> Result<NodeIdentity>;
```

`NodeIdentity` is opaque (`Eq + Hash`, `#[non_exhaustive]`, not
serializable) and is made of a **process key** plus a **native key**:

| | Process key | Native key | Reuse risk |
|---|---|---|---|
| Windows | pid + process creation time (already used for synthetic apps, `uia.rs:2813`) | UIA `RuntimeId` (read today only for dedup, `uia.rs:1363`) | RuntimeId is unique among *live* elements; providers may reuse one after the element dies |
| macOS | pid + process start time | `CFHash` + `CFEqual` of the retained `AXUIElementRef` | AppKit tokens are object pointers; a freed object's address can be reused |
| Linux | D-Bus unique bus name (never reused within one bus lifetime; cache already cleared on bus restart, `atspi.rs:278`) | object path | GTK and Chromium paths are effectively unique; Qt paths are pointer-derived and can be reused |

The process key is what makes "app restarted" detectable everywhere. The
native-key reuse column is the honest limit, documented rather than hidden.

Python gets `Element.__eq__` / `__hash__` from this; JS gets
`element.sameAs(other)` (no operator overloading). Identity is computed
lazily and memoised on the binding object.

#### 1b. Liveness check before every action

Every action path (and the explicit `Element.check_alive()`) runs a cheap
native check *before* mutating, and maps every "it's gone" outcome to
`Error::ElementStale` with a reason:

| Platform | Check | Cost |
|---|---|---|
| Windows | `CurrentProcessId` + re-read `RuntimeId`, compare with captured. `UIA_E_ELEMENTNOTAVAILABLE` → stale | 1 cross-process call (2 properties via a tiny cache request) |
| macOS | `AXUIElementGetPid` + one attribute read (`AXRole`); `kAXErrorInvalidUIElement` → stale; process start time compared | 1–2 AX messages |
| Linux | Generalise `ensure_attached` (`atspi.rs:2020`) to every action, which is what #586 asks for anyway; `NameHasOwner` for the bus | 1–3 D-Bus calls |

Plus one **tighten-only guard** on all three: if the re-read role differs from
the captured role, the element is stale. This catches most native-key reuse
(a freed button's address reused by a text field). It can only refuse; it
never redirects an action, so it is not a fallback under tenet 1.

`ElementStale` gains a `reason` in its `Diagnosis` (the variant's fields stay
as they are; the diagnosis is the extension point):

```
removed            node no longer exists in the app
process_exited     owning process is gone
process_restarted  same pid or name, new process instance
identity_changed   native id now names a different node (role guard tripped)
evicted            the handle registry dropped it (see 1c)
```

and carries the captured `role` / `name` so the message reads
`element is stale (removed): button "Save" captured 4.2s ago`.

#### 1c. Bounded registry

Replace the unbounded macOS and Linux caches with the Windows
`BoundedHandleCache` (moved into core as a generic). Same cap, 65,536,
insertion-order eviction. An evicted handle fails with
`ElementStale(reason="evicted")`, never with a re-lookup.

*Alternative:* a refcounted lease in `ElementData.handle` (an `Arc` whose drop
releases the native ref). Exact lifetime, no eviction surprise, but
`ElementData` is a plain `Clone` value crossing every crate and it changes
`ElementParts` in all three backends. Recommend the bounded cache now, and
leases only if eviction shows up in practice.

#### 1d. MCP refs

MCP is the only surface where a reference must outlive one call. `tree`,
`find` (and `capture`, section 3) return a `ref` on every node:

```json
{ "ref": "c7:42", "role": "button", "name": "Save", ... }
```

- `c7` is the capture id, `42` the node index in it.
- The server keeps the ref tables of the **last 8 captures** (each already
  capped at 2,000 nodes). Older → `failure_kind: "ref_expired"`.
- `action`, `text`, `table` accept `ref` as an alternative to `selector`
  (exactly one of the two). A ref acts on that node or fails with
  `element_stale` + reason. It never falls back to a selector.
- `failure_kind` gains `element_stale` and `ref_expired`
  (`[[types.variant_coverage]]` will force the mapping).

**CLI: deliberately no refs.** Each invocation is a new process with a new
registry; a ref printed by one `xa11y tree` cannot be resolved by the next.
Record this as an intentional omission. Nothing is ever persisted to disk.

### Python

```python
import xa11y

app = xa11y.App.by_name("Editor")
saves = app.locator('button[name="Save"]').elements()   # two "Save" buttons
toolbar_save = saves[0]

# ... the app re-renders; the dialog's Save button is destroyed and rebuilt
try:
    toolbar_save.press()
except xa11y.ElementStaleError as e:
    print(e.reason)        # "removed"
    print(e.last_observed) # 'button "Save" captured 4.2s ago'
    # deliberately NO automatic re-match by name; re-resolve explicitly:
    app.locator('toolbar > button[name="Save"]').press()

# identity
focused = app.locator(":focus").element()
assert focused == toolbar_save          # same native node?
seen = {toolbar_save, focused}          # hashable

toolbar_save.check_alive()              # raises ElementStaleError, or returns None
```

### Implemented

Built as designed, with these specifics:

- `Provider::revalidate` runs before every `Element` action (so `Locator`,
  the bindings, the CLI and MCP all get it). Its default is no check; the
  Linux, macOS and Windows backends and the mock implement it. The IA2 source
  keeps its own retained-identity checks and the default.
- Role guard compares the *raw* platform role the snapshot stored
  (`atspi_role`, `ax_role`, `control_type_id`), so a mapping refinement cannot
  report a live node as replaced.
- Windows does not compare `ElementData::pid` with the element's process: for
  hosted windows (UWP frames) it is the owning app's pid, not the element's.
- macOS process key is the pid alone (no `libc`); `process_exited` is told
  from `removed` by asking the pid's application element whether it answers.
- `ElementStale` changed shape: `{ reason, element }` replaces `{ selector }`
  (breaking for code that matches the variant's fields). Python and JS raise
  `ElementStaleError`, a subclass of `SelectorNotMatchedError`.
- Known limit, documented in `xa11y_core::identity` and the integ test: a
  platform that reuses an id for a new node with the same role (AccessKit on
  Linux and Windows reuses a `NodeId` for a re-added item) passes the check.

### Decided

- Bounded cache now (65,536, insertion-order eviction, `evicted` reason).
  Leases only if eviction shows up in practice.
- The role guard is part of the contract on all three platforms: a re-read
  role that differs from the captured one is `ElementStale(identity_changed)`.

### Open questions

1. MCP ref table size (8 captures × 2,000 nodes ≈ 16k handles): fine?

---

## 2. Document semantics and relations (#476)

### What exists

None of the attributes in scope is read anywhere. `Role` has `Heading`,
`Link`, `Navigation` (the only landmark), `Status`, `Alert`, `WebArea`.
Windows already batches `AriaRole` and `Level`; macOS already batches
`AXDisclosureLevel`; Linux calls `GetAttributes` only for collection roles.

### Platform mapping (Core-AAM 1.2)

| Semantic | Windows UIA | macOS AX | Linux AT-SPI |
|---|---|---|---|
| heading level | `HeadingLevel` (Win10 1803+) | `AXValue` on `AXHeading` | object attribute `level` |
| landmark | `LandmarkType` (Main, Navigation, Search, Form, Custom) + `LocalizedLandmarkType` for banner / complementary / contentinfo / region | subrole `AXLandmarkMain` … `AXLandmarkRegion` | `ROLE_LANDMARK` + attribute `xml-roles` |
| web role | `AriaRole` | none as such (role + subrole) | attribute `xml-roles` |
| link target | no property; Chromium, Edge and Firefox put the URL in `ValuePattern` of a Hyperlink | `AXURL` | `Hyperlink.GetURI(0)` (extra call) |
| live politeness | `LiveSetting` | `AXARIALive` | attributes `live`, `container-live` |
| atomic / relevant | `AriaProperties` string (`atomic=…;relevant=…`) | `AXARIAAtomic`, `AXARIARelevant` | attributes `atomic`, `relevant` (`container-*`) |
| busy | already `StateSet.busy` | | |

Relations are where the platforms disagree most:

| Kind | UIA | AX | AT-SPI |
|---|---|---|---|
| `labelled_by` | `LabeledBy` (one element) | `AXTitleUIElement` (only if exactly one) | `LABELLED_BY` |
| `label_for` | — | `AXServesAsTitleForUIElements` | `LABEL_FOR` |
| `described_by` | — (`FullDescription` is text only) | — (`AXCustomContent` text) | `DESCRIBED_BY` |
| `details` | `DescribedBy` (yes: UIA maps aria-details here) | `AXDetailsElements` | `DETAILS` |
| `controls` | `ControllerFor` (also carries aria-errormessage) | `AXLinkedUIElements` (shared with flowto) | `CONTROLLER_FOR` |
| `flows_to` | `FlowsTo` | `AXLinkedUIElements` (shared) | `FLOWS_TO` |
| `flows_from` | `FlowsFrom` | — | `FLOWS_FROM` |
| `error_message` | `ControllerFor` (shared) | `AXErrorMessageElements` | `ERROR_MESSAGE` |
| `owns` | restructures the tree | `AXOwns` | restructures, or `NODE_PARENT_OF` |
| `member_of` | — | — | `MEMBER_OF` |

So the relation API cannot pretend the kinds are 1:1. **Report the kind the
platform actually reports.** macOS `AXLinkedUIElements` surfaces as
`linked`, not split into `controls` / `flows_to` by guesswork. UIA
`ControllerFor` surfaces as `controls` and the docs say it may include error
messages.

### Proposal

#### 2a. Fields on `ElementData` / `ElementParts`

Same shape as the `table` / `cell` / `collection` sub-structs from #479:

```rust
pub heading_level: Option<u32>,          // 1 = h1; a label, not an index
pub landmark_type: Option<Landmark>,     // non_exhaustive enum
pub web_role: Option<String>,            // raw ARIA role string where exposed
pub link: Option<LinkInfo>,              // { url: Option<String> }
pub live_region: Option<LiveRegionInfo>, // { politeness, atomic, relevant, container }
```

- `Landmark`: `Banner`, `Main`, `Navigation`, `Search`, `Region`,
  `Complementary`, `ContentInfo`, `Form`, `Other` (+ `Unknown` fallback).
  `#[non_exhaustive]` like `Role`: backends map platform → `Landmark`, never
  the reverse. The field is named `landmark_type` (UIA's name) so it reads
  cleanly next to the role, see 2a-bis.
- `LiveRegionInfo.politeness`: `off` / `polite` / `assertive`.
  `container: bool` says whether it was inherited from an ancestor region
  (AT-SPI `container-live`, which the others don't distinguish → `None`).

#### 2a-bis. Landmarks get one role: `Role::Landmark` (recommended)

Today the role is inconsistent across platforms:

| | What `navigation` matches today |
|---|---|
| Linux | **every** landmark: `ROLE_LANDMARK` → `Navigation` (`atspi.rs:4102`), so main, banner and search too |
| macOS | only `AXLandmarkNavigation`; other landmarks are `group` |
| Windows | nothing; no code path produces it |

So behaviour changes on at least one platform whatever we pick. Options:

| | Find all landmarks | Find main | Cost |
|---|---|---|---|
| **A. `Role::Landmark` for every landmark + `landmark_type`** | `landmark` | `landmark[landmark_type="main"]` or `[landmark_type="main"]` | `navigation` role goes away |
| B. Keep `group` (+ `Navigation`), add `landmark_type` | needs a new "attribute present" selector operator; the selector AST is exhaustive, so that is a change in every backend's fast path | `[landmark_type="main"]` | Linux bug still needs fixing; two ways to say "navigation" |
| C. One role per landmark (`Main`, `Banner`, …) | no single selector | `main` | eight new roles, nothing gained over A |

"Landmark" is the standard word on every layer: WAI-ARIA defines `landmark`
as the abstract superclass of `banner`, `complementary`, `contentinfo`,
`form`, `main`, `navigation`, `region` and `search`; WCAG and the HTML spec
call them landmark regions; AT-SPI's role is `ROLE_LANDMARK`; UIA's property
is `LandmarkType`; AX's subroles are `AXLandmarkMain` and friends. So
`landmark` / `landmark_type` invents no vocabulary.

**Recommend A.** AT-SPI already has a single landmark role; UIA and AX model
it as group plus a property, which is exactly role + `landmark_type`. No new
selector operator is needed. `Role::Navigation` is removed (not left as a
variant nothing emits, which would make `navigation` selectors silently match
nothing): the selector parser rejects `navigation` with an `InvalidSelector`
whose message gives the replacement, `landmark[landmark_type="navigation"]`.
Python code comparing `el.role == "navigation"` does break silently; that
goes in the changelog. Unnamed `region` and `form` are not landmarks
(Core-AAM), so they keep `group`.

Heading level selector: `heading[heading_level="2"]`.

**Cost per node.** UIA (`HeadingLevel`, `LandmarkType`,
`LocalizedLandmarkType`, `LiveSetting`, `AriaProperties`) and AX
(`AXARIALive`, `AXARIAAtomic`, `AXARIARelevant`, `AXURL`) join the existing
batch reads: near zero. Linux needs a `GetAttributes` round trip per node,
which it skips today. Gate it to roles that can carry these (heading,
landmark, link, section, document, web-area descendants, any role whose
`GetInterfaces` lists `Hyperlink`), run it inside the existing `rayon::join`
(and fix #650 while there), and measure before/after on the GTK and Qt
fixtures.

#### 2b. Relations as a method, not a field

Relation targets are other elements: putting them in `ElementData` would cost
IPC on every node and create cycles in serialized trees. So:

```rust
/// Relations of `element`, by kind. One platform call where the platform
/// has one (AT-SPI GetRelationSet); UIA and AX read only the kinds asked for.
fn relations(&self, element: &ElementData, kinds: &[RelationKind])
    -> Result<Vec<(RelationKind, Vec<ElementData>)>>;
```

Default `Error::Unsupported` like `table_headers`. Asking for a kind the
platform cannot report → `Error::Unsupported` naming the kind. A supported
kind with no targets → empty list. That is the "unsupported vs genuinely
empty" distinction the issue asks for, done the same way the text API does it.

#### 2c. Unknown vs absent vs unsupported

Three answers, three mechanisms:

| Answer | How it shows |
|---|---|
| Element has no such semantics (a button has no heading level) | field `None` |
| Platform reported "unknown" | an explicit value (`Count::Unknown` precedent; `Landmark::Unknown`) |
| Backend can never report it | `xa11y.capabilities()` (section 5), and `Unsupported` from methods |

A per-field tri-state (`Reported<T>`) on every property was considered and
rejected: it doubles the binding surface and every consumer pays for it. A
static capability table answers the question once per backend, which is
where the answer actually lives.

### Python

```python
doc = app.locator("web_area").element()

for h in doc.locator("heading").elements():   # or heading[heading_level="2"]
    print("  " * (h.heading_level - 1) + (h.name or ""))

main = doc.locator('landmark[landmark_type="main"]').element()
landmarks = doc.locator("landmark").elements()
print([l.landmark_type for l in landmarks])   # ["banner", "navigation", "main", "contentinfo"]

link = doc.locator('link[name="Docs"]').element()
print(link.link.url)                     # "https://example.com/docs" or None

status = doc.locator("status").element()
print(status.live_region)                # LiveRegionInfo(politeness='polite', atomic=True, ...)

field = doc.locator('text_field[name="Email"]').element()
print(field.labelled_by())               # [Element(label "Email address")]
print(field.relations())                 # {"labelled_by": [...], "details": [...]}
try:
    field.relations("described_by")      # Windows: no such relation in UIA
except xa11y.ActionNotSupportedError as e:
    print(e)                             # "relation 'described_by' is not exposed by UIA"
```

### Implemented

Built as designed, with these specifics:

- Fields: `heading_level`, `landmark_type`, `web_role`, `link_url` and
  `live_region` on `ElementData` / `ElementParts`, in `xa11y_core::semantics`
  with `Landmark`, `Politeness`, `LiveRegionInfo` and `RelationKind`.
  `Role::Navigation` is now `Role::Landmark`, and the selector `navigation`
  fails with `InvalidSelector` naming `landmark[landmark_type="navigation"]`.
- Selector attributes: `heading_level` (numeric, `=` only), `landmark_type`,
  `web_role`, `url` and `live` (the politeness).
- `Element::relations(kinds)` returns each requested kind once, in request
  order. An empty `kinds` means every kind the capability table does not mark
  unsupported, so the table and the call cannot disagree. `capabilities()`
  gained `property:*` and `relation:*` rows.
- Linux reads one `GetAttributes` per node, gated by role (headings,
  landmarks, links, live regions and the web roles that carry them), and
  shares it with the table read. Answer to open question 1: a role list.
- Windows marks a group as a landmark from its UIA `LandmarkType` or, when it
  has none, from a landmark `AriaRole`. AccessKit, and other frameworks
  without `LandmarkType`, set only the latter.
- Surfaces: Python and JS getters and `relations()`, the CLI's element line
  and `xa11y relations SELECTOR [KIND...]`, the MCP element JSON and a
  `relations` tool whose targets carry refs.
- The AccessKit adapters publish little of this (see
  `xa11y/tests/integ/semantics.rs`): no heading level anywhere, landmark kinds
  on macOS and Windows only, live regions on Windows only, and only
  `controls` among relations.

### Open questions

1. `web_role`: worth exposing raw, or is it a `raw` key? Exposed as a field
   for now.

---

## 3. Bounded capture (#482)

### What exists

- `Element.tree()` / `App.tree()`: only `role`, `name`, `value`; no node or
  time limit; any child error aborts the whole tree.
- CLI `xa11y tree`: no limit at all.
- MCP `tree`: the only bounded walk (depth 12, 2,000 nodes, per-node
  `subtree_truncated`, `children_error`), implemented in the CLI crate.
- Per-node cost: Windows one `FindAllBuildCache` per level; macOS one batch
  read per node, parallel; Linux 8–12+ D-Bus calls per node, parallel.

### Proposal

One core operation that every tree surface is built on:

```rust
pub struct CaptureOptions { max_depth, max_nodes, max_text_bytes, detail, timeout }
pub struct Capture {
    pub id: CaptureId,
    pub nodes: Vec<CapturedNode>,   // pre-order, flat
    pub truncated: Truncation,      // by_depth, by_nodes, by_text, by_time
    pub vanished: u32,              // nodes that disappeared mid-capture
    pub started: SystemTime, pub finished: SystemTime,
    pub backend: Backend, pub detail: Detail,
}
pub struct CapturedNode {
    pub index: u32, pub parent: Option<u32>, pub depth: u32,
    pub data: ElementData,                 // includes section 2 fields
    pub children_count: Option<u32>,       // real count, even when cut
    pub children_truncated: bool,
    pub error: Option<Error>,              // children read failed; walk continues
    pub text: Option<CapturedText>,        // detail "text"
    pub relations: Vec<(RelationKind, Vec<RelationTarget>)>, // detail "relations"
}
pub enum RelationTarget { Node(u32), Outside(ElementData) }
```

`Element::capture`, `App::capture`, `Locator::capture`, and a
`Provider::capture` with a default implementation over `get_children`.

**Detail is explicit, and recorded on the result.** `structure` (default:
today's `ElementData`), `text` (full text of text elements, bounded by
`max_text_bytes`, so non-actionable document text survives), `relations`
(resolved to node indices where the target is inside the capture),
`formatting` (runs, bounded per node). A cheap capture and a richer one are
two captures with different `detail`; nothing merges them.

**Budgets.** `max_depth`, `max_nodes`, `max_text_bytes`, and a time budget
that becomes #531's `Deadline`. Every limit that fires is reported in
`truncated`, and the cut node says so. The #531 decision put "partial results
on expiry" here: a capture that runs out of time returns what it has with
`truncated.by_time = true` rather than a bare `Timeout`.

**Order.** Read breadth-first, emit pre-order. When `max_nodes` cuts, the
top of the tree is complete instead of one deep branch.

**Consistency, stated honestly.** A capture is *not* an atomic snapshot on
any platform. Each node is read once, at some instant between `started` and
`finished`. A node that vanishes is dropped and counted in `vanished`; a
failed children read marks that node's `error` and the walk continues.
Cycles (seen in broken web trees) are detected by `NodeIdentity` (section 1)
and cut with an error on the repeating node.

**Backend overrides, to be measured not assumed:**

| | Candidate | Caveat |
|---|---|---|
| Windows | `FindAllBuildCache(TreeScope_Subtree)` with one `CacheRequest` | One call has no node cap; use it under a size guess, fall back *explicitly* (logged, documented) to per-level for large trees, or always per-level. Measure. |
| macOS | per-node batch read is already the floor; parallel BFS | `AXChildren` joins the batch |
| Linux | `org.a11y.atspi.Cache.GetItems` returns an app's whole cached tree in one call | Toolkit-dependent (atk-bridge implements it; check GTK 4 and Qt). Where absent, per-node reads |

Acceptance from the issue (calls, latency and payload on small / large /
dynamic fixtures, and no regression for callers not asking for rich detail)
becomes a `cargo xtask bench-capture` target run in the integ cells.

**Then rebuild the existing surfaces on it:** `tree()` / `dump()` become
`capture(...)` plus a printer (no breaking change to their dict shape); MCP
`tree` drops its private walker and gains `ref` (section 1d) and `detail`;
CLI `xa11y tree` gains `--max-nodes` with a default, ending its unbounded
walk. Compact or token-optimised output stays outside xa11y (#103).

*Alternatives considered:*
- Keep growing `tree()`'s dict with kwargs. Rejected: untyped, mixes capture
  with serialisation, nowhere to put truncation or provenance.
- A streaming iterator (`for node in el.walk(...)`) instead of a result
  object. Good for very large trees and could come later on the same
  `Provider::capture` internals; a materialised `Capture` is what tests,
  diffing and MCP refs need first.

### Python

```python
cap = app.capture(
    max_depth=20,
    max_nodes=5000,
    detail={"structure", "text", "relations"},
    timeout=3.0,
)

print(cap.truncated)      # Truncation(by_depth=False, by_nodes=True, by_time=False, by_text=False)
print(cap.vanished, cap.finished - cap.started)

for node in cap.nodes:
    if node.role == "heading":
        print(node.depth, node.heading_level, node.name)
    if node.children_truncated:
        print("cut at", node.role, node.name, "has", node.children_count, "children")
    if node.error:
        print("could not read children of", node.name, node.error)

# non-actionable document text is preserved
article = next(n for n in cap.nodes if n.landmark_type == "main")
print("".join(n.text.content for n in cap.subtree(article) if n.text))

# relations resolved inside the capture
email = cap.find('text_field[name="Email"]')[0]
for target in email.relations["labelled_by"]:
    print(target.name)    # a CapturedNode when inside the capture

# act on a captured node: exact node or ElementStaleError, never a re-match
cap.find('button[name="Save"]')[0].element().press()
```

### Implemented

Built as designed, with these specifics:

- `xa11y_core::capture`: `CaptureOptions` (chained setters), `Capture`,
  `CapturedNode`, `Truncation`, `CapturedText`, `CaptureError` (a `step` and
  the `Error`) and `RelationTarget`. `Element`, `App`, `Locator` and
  `ShellSurface` have `capture(&options)`. A captured node holds its
  `Element`, so acting on it reaches that node or fails stale.
- No `CaptureId` and no `backend` field: refs in MCP carry their own
  capture ids, and `capabilities()` already names the platform.
- Detail is `text` and `relations`. `formatting` (runs) is not built yet.
- The walk is core-only for now: there is no `Provider::capture` hook and no
  backend override, so nothing has been measured against
  `FindAllBuildCache(TreeScope_Subtree)` or `Cache.GetItems`, and there is
  no `bench-capture` target yet. Those are the next step if capture cost
  shows up.
- A node whose children read fails with `ElementStale` is dropped and
  counted in `vanished`; any other failure is a `CaptureError` on the node.
  A repeated identity is a `cycle` error on the parent.
- MCP `tree` is rebuilt on capture (breadth-first now, so a node cut keeps
  the top of the tree) and gains `detail`. CLI `xa11y tree` is rebuilt on
  it with `--max-nodes` (default 2,000) and `--max-depth`, and prints where
  and why it cut. `Element.tree()` / `dump()` are unchanged and unbounded,
  which answers open question 1: they stay frozen and `capture()` is the
  bounded form.
- Python and JS expose `capture(...)` with the options folded in (keywords,
  options object); nodes refer to each other by index, so Python holds no
  reference cycles.

### Open questions

Decided: default `max_nodes` is 2,000 on every surface, the same as MCP.
Decided: `tree()` stays as it is (see Implemented).

1. `Capture.find(selector)`: run the selector engine over captured data
   (cheap, offline), or leave querying to the caller?

---

## 4. Events (#480)

### What exists

`EventKind` has 15 kinds. Subscriptions are app-scoped, return after native
registration completes on all three backends, and clean up on drop. #648
added `TextSelectionChanged` on all three and `text_change` deltas on Linux.
Missing: bounded buffering outside MCP, any timestamp or sequence in the
bindings, subscribe-time filtering, announcement payloads, a live-region
kind, and actionable targets (Linux targets carry handle 0).

### Proposal

#### 4a. One `announcement` kind everywhere, with an `origin` and the text

Two facts arrive through announcement-like events. An explicit request says
"say this" and carries text. A live-region or alert change says "this region
changed, go read it" and carries none.

| | Windows | macOS | Linux |
|---|---|---|---|
| Explicit request (text + priority) | `AddNotificationEventHandler`: `displayString`, `NotificationKind`, `NotificationProcessing`, `activityId` | `AXAnnouncementRequested` userInfo: `AXAnnouncementKey`, `AXPriorityKey` (needs `AXObserverCreateWithInfoCallback`) | `Object:Announcement`: text in `any_data`, politeness in `detail1` (AT-SPI 2.46+; GTK 4.14 `gtk_accessible_announce`) |
| Live region changed (no text) | `UIA_LiveRegionChangedEventId` | `AXLiveRegionChanged` (not registered today) | **No native signal.** Live updates arrive as `text-changed` / `children-changed` inside a `container-live` subtree |
| Alert appeared (no text) | `UIA_SystemAlertEventId` | no separate event | no separate event |

**Recommended: keep one kind, `announcement`, on every platform, and say
which fact it is.** Today Windows already folds all three UIA events into
`announcement`; this keeps that, adds what is missing, and makes macOS match:

```rust
pub struct AnnouncementInfo {
    pub origin: AnnouncementOrigin,        // request | live_region | alert
    pub text: Option<String>,              // Some for a request; None for the others
    pub politeness: Option<Politeness>,    // polite | assertive, normalised
    pub platform_priority: Option<String>, // "High", "ImportantMostRecent", ...
    pub activity_id: Option<String>,       // UIA only
}
```

- A portable `wait_for(announcement)` works on all three, which is the
  consistency a test writer wants, and nothing that works today on Windows
  stops working.
- `origin` keeps the two facts apart for a consumer that cares, without
  making everyone else subscribe to two kinds.
- Normalisation: AX `High` and UIA `ImportantAll` / `ImportantMostRecent` →
  `assertive`; the rest `polite`. The raw value is preserved. No
  screen-reader policy (queueing, interruption) is implied.

**What still differs, and cannot be made the same:** Linux has no live-region
signal (decided: not derived), so on Linux an `aria-live` update raises an
announcement only when the toolkit sends one itself. AccessKit and GTK 4's
announce API do; Chromium and GTK 3 widgets send text and children changes
instead. `xa11y.capabilities()` carries this as its own row,
`event:announcement:live_region`, `unsupported` on Linux, so a test can ask
before it waits.

*Alternative: split `live_region_changed` into its own kind.* Each kind then
means one fact, but the same app produces different kinds on different
platforms (AccessKit: an announcement on macOS and Linux, a live-region
change on Windows), and code waiting for `announcement` after an `aria-live`
update on Windows starts timing out. Rejected for the consistency reason
above.

#### 4a-bis. What changes on Windows

Nothing that works today breaks. The three UIA events keep arriving as
`announcement`; each gains a payload:

| UIA event | Raised by | Today | After |
|---|---|---|---|
| Notification | an app calling `RaiseNotificationEvent` (WPF, WinForms `RaiseAutomationNotification`, UWP/WinUI, Windows Terminal) | `announcement`, no text | `announcement`, `origin: request`, with text, kind, processing, activity id |
| LiveRegionChanged | a region with `LiveSetting` changed (WPF, WinUI, browsers for `aria-live`, AccessKit live nodes) | `announcement` | `announcement`, `origin: live_region`, politeness from the target's `LiveSetting` |
| SystemAlert | an alert appeared (browsers for `role=alert`, Win32 alerts) | `announcement` | `announcement`, `origin: alert`, politeness `assertive` |

The one mechanical change is reading the text: Notification moves from the
generic automation handler to `AddNotificationEventHandler`, which needs
`IUIAutomation5` (Windows 10 1709+). The capability table records the version
(`event:announcement:text`, `since 10.0.16299`). Older systems receive no
request at all: keeping Notification on the generic handler there would be a
silent fallback, so asking for `announcement` by name fails as unsupported and
an all-kinds subscription goes without requests.

#### 4b. Event metadata

- `sequence: u64` per subscription (moved down from the MCP drainer).
- `timestamp`: exposed in the bindings as seconds since the subscription
  started (`Instant` has no wall-clock meaning; MCP already does `at_ms`).
- `target` becomes an actionable `Element` (registered in the bounded
  registry, section 1c) instead of a handle-0 snapshot on Linux. Acting on it
  later follows section 1 rules.

#### 4c. Subscribe-time filter, unsupported kinds fail loudly

```rust
App::subscribe_with(SubscribeOptions::new().kinds(&[..]).buffer(4096))
```

- Backends skip registering handlers nobody asked for (UIA per-window
  property handlers are the expensive ones).
- A requested kind the backend cannot deliver fails `subscribe` with
  `Unsupported` naming it (e.g. `menu_opened` on Linux,
  `window_deactivated` on Windows today). Today such a kind just never fires.
- The plain `subscribe()` keeps "everything this backend supports".

#### 4d. Bounded buffering in core

Replace the unbounded channel with a bounded ring in core (default 4,096,
drop-oldest), and expose `Subscription.dropped` (total) so loss is
observable. MCP then reuses it instead of re-implementing it. Drop-oldest
vs drop-newest: oldest, because a test waiting for the latest state cares
about recent events; MCP already chose this.

#### 4e. Follow-ups this unlocks (separate PRs)

- macOS `text_change` from `AXTextStateChangeType` / `AXTextChangeValues`
  in userInfo (same `WithInfo` switch as 4a).
- Windows `text_change` from `TextEdit_TextChanged` where the provider
  raises it.
- `WindowDeactivated` on macOS / Windows, `WindowActivated` on Windows.
- Tests for `StructureChanged` and `SelectionChanged`, which have none.

### Python

```python
from xa11y import App, EventType

app = App.by_name("Chat")

with app.subscribe(kinds=[EventType.ANNOUNCEMENT, EventType.TEXT_CHANGED],
                   buffer=4096) as sub:
    app.locator('button[name="Send"]').press()

    ev = sub.wait_for(lambda e: e.event_type == EventType.ANNOUNCEMENT, timeout=5)
    print(ev.announcement.origin)      # "request", "live_region" or "alert"
    print(ev.announcement.text)        # "Message sent" for a request, else None
    print(ev.announcement.politeness)  # "polite"
    print(ev.sequence, ev.timestamp)   # 17, 0.412

    if ev.announcement.text is None:   # a region changed: read it
        print(ev.target.name, ev.target.text().read_all())

    print(sub.dropped)                 # 0, or how many were lost to the bound

# Ask before waiting for something a platform never sends
caps = xa11y.capabilities()
if caps.support("event:announcement:live_region") == "unsupported":   # Linux
    ...  # watch text_changed on the region instead
```

### Implemented

Built as designed, with these specifics and gaps:

- `Event.announcement: Option<AnnouncementInfo>` (`origin`, `text`,
  `politeness`, `platform_priority`, `activity_id`) and `Event.sequence`.
  Linux reads `any_data` and `detail1` of `Object:Announcement`. macOS moved
  to `AXObserverCreateWithInfoCallback` for `AXAnnouncementKey` /
  `AXPriorityKey` and registers `AXLiveRegionChanged`. Windows reports the
  origin and politeness of its three events.
- Windows notification text: `AddNotificationEventHandler` (IUIAutomation5)
  is wired through per-window registration, reconciliation and cleanup, and
  `Provider::subscribe_kinds` lets the Windows backend register only the
  handlers the requested kinds need.
- 4d: `event_channel()` in core, a bounded ring (4,096, drop-oldest) that
  numbers events as they are queued, with `Subscription::dropped`. Every
  backend sends through it. `EventReceiver::new` over a plain channel
  remains for tests and adapters.
- 4c: `App::subscribe_with(SubscribeOptions { kinds, buffer })` checks each
  kind against the capability table and fails `Unsupported` before
  subscribing. The filter runs at the channel, so other kinds never take a
  buffer slot. Backends still register every handler; skipping
  registrations is a later optimisation. Bindings fold the options into
  `subscribe(kinds=, buffer=)` / `subscribe({ kinds, buffer })`, the CLI
  takes `--kinds`, and MCP `events_start` passes its `kinds` through.
- 4b: `sequence` is exposed in the bindings and MCP. **Not done:** a binding
  `timestamp` (still Rust-only) and actionable Linux event targets (still
  handle-0 snapshots).
- MCP keeps its own 1,024-event poll buffer on top of the core one.

### Open questions

Decided: no live-region event on Linux (not derived).
Decided: 4,096 events, drop-oldest; one `announcement` kind with `origin`.

---

## 5. Cross-cutting: capabilities, deadline, IA2

### `xa11y.capabilities()` (implemented)

Status: built on this branch for the features that exist today; the rows for
sections 2 and 4 arrive with those changes.

#### The problem it solves

Three questions had no answer except "read the docs and hope they are
current":

1. A field is `None`. Does this element lack the property, or can this
   backend never report it? (#476 requires these to be distinguishable.)
2. Is an event kind silent because nothing happened, or because this backend
   never sends it? (`window_deactivated` on Windows simply never arrives.)
3. Which relation kinds can I ask for here?

The docs answered these in prose and tables, and had drifted: the role table
claims a Windows landmark mapping no code produces, and Blur was documented
as unsupported on Linux and Windows while both implement it (fixed on this
branch).

#### What it is

`xa11y-core/src/capabilities.rs` holds one list of rows, each with a column
per platform, so a new feature cannot be added without answering for all
three, and a new platform is a compile error on every row:

```rust
#[non_exhaustive] pub enum Platform { Linux, Macos, Windows }
#[non_exhaustive] pub enum Support { Supported, Partial, Unsupported }

#[non_exhaustive]
pub struct Capability {
    pub name: &'static str,            // "event:menu_opened"
    pub description: &'static str,
    pub support: Support,
    pub note: Option<String>,          // how it is provided, or why not
    pub since: Option<&'static str>,   // first OS version that has it
}

impl Capabilities {
    pub fn current() -> Result<Self>;                                   // running platform + detected OS version
    pub fn for_platform(p: Platform, os_version: Option<&str>) -> Result<Self>;
    pub fn get(&self, name: &str) -> Result<&Capability>;               // unknown name: error with closest names
    pub fn support(&self, name: &str) -> Result<Support>;
    pub fn note(&self, name: &str) -> Result<Option<&str>>;
    pub fn has(&self, name: &str) -> bool;
    pub fn entries(&self) -> &[Capability];
    pub fn names() -> impl Iterator<Item = &'static str>;
}
pub fn capabilities() -> Result<Capabilities>;                          // xa11y crate
```

- **`supported`**: the backend reads it whenever the app exposes it. `None`
  on an element means *this element* has none.
- **`partial`**: read for some toolkits, apps or display servers only (Linux
  window verbs: X11 and Sway; collection positions: GTK yes, Qt no). The note
  says which.
- **`unsupported`**: never. Methods fail as unsupported, the event never
  arrives, the field is always `None`.

**Other platforms and versions.** `capabilities()` with no arguments
describes the running platform, with the OS version detected on macOS
(`kern.osproductversion`) and Windows (`ver`). Passing a platform describes
another one; passing an OS version narrows the answers, so a row with
`since = "10.0.17134"` reads `unsupported` for `10.0.16299`, with the reason
in the note. Linux has no single platform version (what it reports depends on
the toolkit and display server, which the notes name), so a version for Linux
is an error rather than ignored. No row is version-gated yet; the first will
be UIA `HeadingLevel` (Windows 10 1803) and the Notification text (1709).

**Sources.** The table includes an explicit Windows IA2 column. A backend
option selects it on every interface; the runtime default follows
`XA11Y_ACCESSIBILITY_SOURCE`. `Capabilities.backend` names the source.

Names are `<area>:<name>`: `event:`, `action:`, `app:`, `window:`, `state:`,
`table:`, `text:`. Today's table has 38 rows.

#### Surfaces

| | |
|---|---|
| Rust | `xa11y::capabilities()`, `Capabilities::for_platform(..)` |
| Python | `xa11y.capabilities(platform=None, os_version=None)` → `Capabilities` (`support`, `note`, `get`, `has`, `in`, `[]`, iteration) |
| JS | `capabilities({ platform?, osVersion? })` → `Capabilities` |
| CLI | `xa11y capabilities [NAME \| AREA: ...] [--platform P] [--os-version V] [--json]` |
| MCP | `capabilities` tool (`platform`, `os_version`, `names`) |

`current` / `for_platform` fold into the single `capabilities(...)` entry in
both bindings (one name per operation). Strings are the same in both
bindings: `linux` / `macos` / `windows`, `supported` / `partial` /
`unsupported`.

#### What keeps it true

1. **Docs are generated.** `cargo xtask sync-capabilities` renders the table
   into `reference/platform-details.mdx` between two markers; `--check` runs
   in `cargo xtask check` and in CI.
2. **An integration test holds it against the running backend.**
   `xa11y/tests/integ/capabilities.rs` walks every row in each platform cell
   and either probes it against the AccessKit app (unsupported → the call
   fails as unsupported or the state reads unknown; supported → it does not)
   or names it in an explicit `NOT_PROBED` list with the reason. A new row
   fails the test until it is placed. Passing on Linux here.
3. **Completeness guards.** A unit test fails when an `EventKind` has no
   `event:` row; `[[types.variant_coverage]]` covers `Platform` and `Support`
   in the Python and TypeScript literal types; every non-`supported` answer
   must carry a note.
4. **Code will consult it** once `subscribe(kinds=...)` and `relations(kind)`
   exist (sections 2 and 4): those decide "unsupported" by reading the table,
   so a wrong entry is a visible wrong error.

```python
import xa11y

caps = xa11y.capabilities()                     # this machine
caps.platform, caps.os_version                  # ("linux", None)
caps.support("event:menu_opened")               # "unsupported"
caps.note("event:menu_opened")                  # "the AT-SPI backend maps no menu event"
"window:maximize" in caps                       # True
[c.name for c in caps if c.support == "partial"]

win = xa11y.capabilities("windows", os_version="10.0.19045")
win.get("window:enter_fullscreen")
# Capability('window:enter_fullscreen', 'unsupported', note='UIA has no fullscreen API')

caps.get("event:menu_open")   # InvalidActionDataError: unknown capability "event:menu_open";
                              # names in "event": event:menu_opened, event:menu_closed, ...
```

### #531 Deadline

Implemented. Core opens a thread-local `Deadline` for every wait and
auto-waiting action, and backends ask `call_timeout` for the time left
before each native call:

- **Linux.** Each D-Bus call races a timer set to the time left; the
  connection's 5 s method timeout stays as the outer bound.
- **macOS.** `AXUIElementSetMessagingTimeout` is lowered on the element for
  one message, then released to the bound it had (the global 2 s, or a shell
  probe's 0.25 s).
- **Windows IA2.** The caller stops waiting at its deadline and the worker
  stops at its next native operation. Only the 5 s stall detector disables
  the worker.
- **Windows UIA.** UIA's timeouts belong to the shared `IUIAutomation`, so
  lowering them per call would cut other threads short. No provider call
  starts after the deadline; one in flight keeps UIA's own bounds.

A cut call is recorded on the deadline (shared across the threads a walk
fans out to), and the timeout names it in `Diagnosis.pending`. A read that
returned after a cut is not trusted as an observation, and an action whose
call was cut is a `Timeout` with unknown delivery. Partial results stay
out of scope.

### #483 IA2

Implemented through the shared contracts. Per-attachment backend selection
is available in Python/JS and CLI/MCP application operations; Rust constructs
`provider_for_backend("ia2")` and passes it to the existing `App::*_with`
factories. The environment variable remains the default. `native` stays UIA
on Windows; selecting IA2 elsewhere is `Unsupported`.

IA2 provides shared identity, semantics, relations, bounded capture, text,
formatting, tables and semantic actions. `Text.object_at(offset)` maps
hypertext to live elements, with explicit unsupported paths in UIA, AX and
AT-SPI. CLI/MCP return an object snapshot for this operation; MCP refs remain
issued by tree/find/relations. The generated capability matrix includes IA2.

Intentional omissions: IA2 event subscriptions and window-manager verbs,
Firefox/toolkit compatibility claims, and live Text2 evidence. Those paths
are documented as unsupported or unverified. The real Chrome fixture covers
two tabs, negative IDs, repeated identity, semantics/relations, embedded
controls, text/caret/selection/formatting, tables, removal and reparenting.
The owned Chrome fixture pauses its browser process with the Windows debug
API and proves that an actual nonresponding provider returns within the
five-second stall bound and disables further requests. COM cancellation is
best effort; it does not promise termination of the provider process.

---

## Decisions

Made:

- **#481** Bounded handle cache now, leases later if needed. A role change on
  re-read means stale, on all three platforms.
- **#480** `LiveRegionChanged` is unsupported on Linux.
- **#482** Default `max_nodes` is 2,000 everywhere.
- **All** `xa11y.capabilities()`, with a platform and OS-version option:
  implemented in #660 (section 5).
- **#476** `Role::Landmark` for every landmark plus `landmark_type`;
  `Role::Navigation` removed with a loud selector error (2a-bis).
- **#480** One `announcement` kind on every platform with `origin`
  (`request` / `live_region` / `alert`) and the text, instead of a split
  (4a). No Windows break.

Still open: Linux `GetAttributes` gating (2), `web_role` exposure (2), MCP
ref table size (1), event buffer default (4), `tree()` frozen or not (3).
