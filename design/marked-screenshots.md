# Marked screenshots

Design for [#376](https://github.com/xa11y/xa11y/issues/376) — a screenshot with
bounding boxes drawn over selected elements, plus a machine-readable legend
mapping each box to the element it came from.

## Motivation

An agent driving an app through the accessibility tree falls back to a
screenshot when the tree is thin: elements with no name, a generic role, a
custom-drawn widget. The screenshot tells it what the app *looks* like, but not
which tree node is which pixel. Today the only way to correlate the two is
`screenshot(element=...)` per candidate, one round trip each.

Drawing the correlation into the image collapses that into one call. The image
carries the boxes; the legend carries the selector that acts on each box.

## What this is not

**Not a vision fallback.** The marks come from the accessibility tree. An app
with no tree gets no marks, and the feature degrades to a plain screenshot. If
the tree is *missing*, this does not help; it helps when the tree is *present
but uninformative*, which is the case the issue describes.

**Not a rendered legend panel.** macapptree composites a legend into the image.
We keep the legend out of band (stdout text, MCP JSON, a Python list). Rendered
text costs image area, changes the output dimensions, and is strictly worse for
a model than the same data as JSON. Boxes and tags go in the image; everything
else stays structured.

## Vocabulary

The word "label" is taken. In accessibility it means the accessible name, or
the element that supplies it (`aria-label`, "labelled by"). An accessibility
library that calls a drawn number a label owes every reader a disambiguation
forever. So:

| Term | Means |
|---|---|
| **mark** | one drawn box + tag, for one element |
| `--mark SELECTOR` | the CLI flag; each occurrence is a **mark group** |
| **tag** | the short text drawn in the box (`A7`) |
| **legend** | the out-of-band list mapping tag → element |

`legend` is a straight borrow from maps and charts and needs no defence.
"annotate" was the runner-up and is fine, but it is longer and it collides
softly with `AXCustomContent`-style annotations. "highlight" implies transient
on-screen chrome, which this is not. "segmented" (macapptree's word) describes
image segmentation, which this is not either.

### Tag format

The issue thread and the feature request both reach for `2.15` — selector 2,
element 15. Three problems:

1. **It reads as a decimal.** `2.15`, `2.150`, and `2.1.5` are the same number
   of characters of model attention, and only one of them is a valid tag. A
   model transcribing a tag back into a tool call has no type to check it
   against.
2. **The indices are 0-based; every other index in xa11y is 1-based.**
   `:nth(n)` and `Locator.nth(n)` are 1-based. A tag reading `2.15` invites
   `:nth(15)` when the element is `:nth(16)`.
3. **Colour is the only redundant channel.** Group identity is carried by the
   colour of the box and by the leading digit, but the leading digit is easy to
   lose against the separator at small sizes.

Recommended instead: **a letter for the group, a 1-based number for the element
within it** — `A1`, `B7`, `C12`.

- Unambiguous as text; nothing parses it as a number.
- The number is the `:nth(n)` argument, exactly.
- The letter is the text alternative to the colour. This is WCAG 1.4.1 applied
  to our own output, which is a thing an accessibility library should get right
  without being asked.
- Two glyphs instead of four at 12px, and no punctuation to render.

Groups past `Z` continue `AA`, `AB`. In practice a caller passing 27 selectors
has a different problem.

The format lives in one function (`tag_for(group, index)` in
`xa11y-core/src/screenshot/mark.rs`), so switching back to `2.15` is a
one-function change plus the tests that assert it. If you want to keep the
numeric form, that is where it goes.

## Layering

Two layers, and the split is the load-bearing part of this design.

```
xa11y-core::screenshot   pure pixels: Rect + text + colour → new Screenshot
        ▲                 no Provider, no selectors, no platform
        │
xa11y (umbrella)         selectors → Locator::elements() → Vec<Mark>
        ▲                 owns the target resolution and the legend
        │
   cli / mcp / bindings
```

Core never learns what a selector is; the umbrella crate never learns how to
set a pixel. Core's half is testable and fuzzable with no display, no app, and
no permissions, which is where essentially all the arithmetic risk lives.

## Core: the drawing half

New module `xa11y-core/src/screenshot/mark.rs` (today `screenshot.rs` is a flat
file; it becomes a directory).

```rust
/// One box to draw: where, what to write in it, and in what colour.
///
/// `rect` is in **logical** screen coordinates, the same space as
/// `Element::bounds`. `Screenshot::mark` converts to physical pixels.
#[non_exhaustive]
pub struct Mark {
    pub rect: Rect,
    pub tag: String,
    pub color: [u8; 3],
}

impl Mark {
    pub fn new(rect: Rect, tag: impl Into<String>) -> Self;   // palette[0]
    pub fn color(mut self, rgb: [u8; 3]) -> Self;            // chained setter
}

/// Colour-blind-safe qualitative palette (Okabe–Ito, minus black).
pub const MARK_PALETTE: [[u8; 3]; 7] = [ /* … */ ];

impl Screenshot {
    /// Draw `marks` onto a copy of this capture.
    ///
    /// `origin` is the logical top-left of what this capture covers — the
    /// region passed to `capture_region`, or `(0, 0)` for a full-display
    /// capture. Marks are translated by it and scaled by `self.scale`.
    ///
    /// Marks whose rect does not intersect the image are **skipped, not
    /// clamped**, and reported in the returned `Vec<usize>` of skipped
    /// indices. A box clamped to the edge would claim the wrong pixels.
    pub fn mark(&self, marks: &[Mark], origin: Point) -> Result<(Screenshot, Vec<usize>)>;
}
```

`[u8; 3]` rather than an `Rgb` newtype, deliberately: a new public type costs a
`[types]` classification in `bindings/parity_allowlist.toml` and a binding
decision, and buys nothing over three bytes.

`Mark` is `#[non_exhaustive]` with a constructor and a chained setter, matching
`ClickOptions` — it is built in `xa11y`, another crate, so it owes callers a
way to construct one (AGENTS.md, "Public API Extensibility").

### Drawing, without a new dependency

- **Boxes.** A `stroke` px outline in the mark colour, `stroke = clamp(round(scale), 1, 4)`.
  Written straight into the RGBA buffer; no blending, no alpha.
- **Tags.** A filled badge in the mark colour at the box's inner top-left, with
  the tag drawn on top in whichever of black/white has more contrast against
  that colour (relative luminance, the WCAG formula). At 7 palette colours this
  is a compile-time-checkable property, so the unit test asserts every palette
  entry clears 4.5:1 against its chosen foreground.
- **Glyphs.** An embedded 5×7 bitmap font covering `0-9` and `A-Z` — 36 glyphs
  × 7 bytes, a `const [[u8; 7]; 36]`. Integer-scaled by `clamp(round(scale), 1, 4)`.

  The alternative is `ab_glyph`/`fontdue` plus an embedded TTF: a dependency,
  a few hundred KB in every binary and both wheels, and a rasteriser, for two
  character classes. The bitmap table is about sixty lines and never changes.
  `image`/`imageproc` is heavier still.

- **Badge collisions.** Nested elements (window ⊃ group ⊃ button) put badges on
  top of each other. Marks are drawn largest-area-first so small elements land
  on top, and a badge that would overlap one already placed tries the box's
  other three inner corners before accepting the overlap. Greedy, bounded, and
  good enough; a layout solver is not warranted.

- **Duplicates are not deduplicated.** Two selectors matching one element get
  two marks and two legend entries. Merging them would silently drop a group's
  membership, which is information the caller asked for.

### Overflow

`Rect` is `i32`, `scale` is `f32`, and the products index a `Vec<u8>`. Every
coordinate goes through `Rect::to_physical` (which already sanitises a
non-finite or non-positive scale) and then a checked conversion to a pixel
index. A new `cargo-fuzz` target — `xa11y/fuzz/fuzz_targets/mark_ops.rs` —
drives `Screenshot::mark` with arbitrary rects, scales, and image dimensions
and asserts it neither panics nor writes out of bounds.

## Umbrella: the resolution half

```rust
/// What to capture, and what to mark on it.
#[non_exhaustive]
pub struct MarkedCapture { /* built with chained setters */ }

pub fn screenshot_marked(
    region: Option<Rect>,
    groups: &[Locator],
) -> Result<Marked>;

/// A capture plus the legend describing what was drawn on it.
#[non_exhaustive]
pub struct Marked {
    pub screenshot: Screenshot,
    pub legend: Vec<LegendEntry>,
    pub omitted: Vec<Omission>,
}

#[non_exhaustive]
pub struct LegendEntry {
    pub tag: String,          // "B7"
    pub group: usize,         // 1-based, matches the --mark order
    pub index: usize,         // 1-based, the :nth(n) argument
    pub selector: String,     // "button:nth(7)" — usable as-is
    pub role: String,
    pub name: Option<String>,
    pub bounds: Rect,         // logical
    pub color: [u8; 3],
}

#[non_exhaustive]
pub struct Omission {
    pub selector: String,
    pub role: String,
    pub name: Option<String>,
    pub reason: OmissionReason,   // NoBounds | ZeroArea | OutsideCapture
}
```

`selector` on the entry is the point of the whole feature. The issue asks for
"a printed selector (`xa11y find …`)"; this is it, and it round-trips:
`app.locator(entry.selector).press()`.

`omitted` is tenet 1 and tenet 6. An element with no bounds, a zero-sized one,
or one on a second monitor is dropped from the image — dropping it *silently*
would leave a legend that disagrees with the picture and no way to find out
why. See "Multi-monitor" below; that case is common enough to matter.

### Why `Locator` and not `&str`

A selector alone has no scope. Taking a `Locator` means the caller has already
said what tree it searches, the chained forms (`app.locator("toolbar").child("button")`)
work unchanged, and the umbrella crate does not grow a second app-resolution
path next to the one `cli::resolve_app` already owns.

The CLI still takes strings, because a command line has no other option; it
resolves `--app`/`--pid`/`--shell` once and builds one `Locator` per `--mark`.

## Surfaces

### `Screenshot` grows a legend, rather than a second function

`Screenshot` is `#[non_exhaustive]`, so adding a field is not breaking for
readers, and `Screenshot::new` keeps its signature (backends set an empty
legend). The bindings then expose **one** `screenshot()` whose return type does
not depend on its arguments — AGENTS.md, "Options structs fold into the primary
verb": two names for one operation is worse than one name with options.

Rust keeps `Marked` as a distinct type because Rust callers can destructure it;
the bindings flatten it onto `Screenshot` (`shot.legend`, `shot.omitted`, both
empty for an unmarked capture) and declare the flatten in the parity allowlist.

### CLI

```
xa11y screenshot [--region X,Y,W,H] --out PATH
                 [--app NAME | --pid PID | --shell KIND]
                 [--mark SELECTOR]...
                 [--legend text|json|none]
```

`--mark` is repeatable and is the opt-in: with none, behaviour is byte-identical
to today.

This is the one real cost of the design. `xa11y screenshot`'s help text
currently reads "regions only — no selectors, no a11y", and that stops being
true: the command gains a target, and gains the failure modes that come with
one (`app not found`, `no elements matched`). Both the help text and
`docs/site/src/content/docs/reference/cli.mdx` say so.

**`--out -` plus a legend is a usage error.** PNG bytes and legend text cannot
share stdout. Rather than quietly moving the legend to stderr, the command
refuses and names the two fixes (`--out FILE`, or `--legend none`). Tenet 1 —
the alternative is a caller piping a PNG somewhere and never learning that the
legend they asked for went to a different stream.

Text legend, one group header plus one line per mark:

```
A  button       #0072B2  7 marked
B  text_field   #D55E00  2 marked

A1  button      "7"          bounds=104,318,48,44   button:nth(1)
A2  button      "8"          bounds=156,318,48,44   button:nth(2)
…
B1  text_field  "Display"    bounds=100,60,320,52   text_field:nth(1)

omitted: 1 element (outside_capture: button "Paste")
```

### MCP

`screenshot` gains `marks: string[]` and the shared `app`/`pid`/`shell` target
properties. The result keeps its image content and gains `legend`, `omitted`,
and `truncated` in the JSON summary.

Three things the tool description must state, because a model that discovers
them by experiment spends calls doing it:

- Marks come from the accessibility tree, so an app without one gets no marks.
- The tag format, and that the number is the `:nth(n)` argument.
- The legend cap (100 entries) and that `truncated` reports when it bit.

The cap is the "Results are bounded" rule from AGENTS.md. A `--mark div`-style
selector over a large tree would otherwise put a thousand entries in a context
window. Marks past the cap are neither drawn nor listed, and `truncated` says
how many.

### Python

`marks=` is a keyword-only parameter on the existing `screenshot()`. Absent, the
call is exactly what it is today.

```python
import xa11y

app = xa11y.App.by_name("Calculator")

shot = xa11y.screenshot(
    element=app.locator("window").element(),
    marks=[app.locator("button"), app.locator("text_field")],
)
shot.save_png("calc.png")
```

`marks` accepts `Locator | str`. A `Locator` brings its own scope; a bare string
is resolved against the system root, the same as `xa11y.locator(s)` — so
`marks=["button"]` means *every* button on screen, which is occasionally what
you want and never what you want by accident.

The legend is a list of entries, in draw order:

```python
for e in shot.legend:
    print(f"{e.tag:>4}  {e.role:<12} {e.name!r:<16} {e.selector}")

#   A1  button       '7'              button:nth(1)
#   A2  button       '8'              button:nth(2)
#   B1  text_field   'Display'        text_field:nth(1)
```

Each entry:

| Attribute | Type | |
|---|---|---|
| `tag` | `str` | what is drawn in the box — `"B7"` |
| `group` | `int` | 1-based, matching the `marks=` order |
| `index` | `int` | 1-based, and exactly the `:nth(n)` argument |
| `selector` | `str` | `"button:nth(7)"` — usable against the same scope |
| `role` | `str` | snake_case, as everywhere else |
| `name` | `str \| None` | |
| `bounds` | `Rect` | logical coordinates |
| `color` | `tuple[int, int, int]` | the box colour, for correlating by eye |

The round trip the whole feature exists for — model reads a tag off the image,
script acts on the element:

```python
tag = "B1"                                    # ← model read this off the PNG
entry = next(e for e in shot.legend if e.tag == tag)
app.locator(entry.selector).set_value("42")
```

Anything the tree knew about but the image could not show is reported, never
dropped in silence:

```python
for o in shot.omitted:
    print(o.reason, o.role, o.name)

#   outside_capture  button  'Paste'
#   no_bounds        menu_item  'About'
```

`reason` is one of `"no_bounds"`, `"zero_area"`, `"outside_capture"` — a
snake_case string, like every other enum that crosses the binding boundary.

Both `legend` and `omitted` are `[]` on an unmarked capture, so consumers need
no version check.

#### Scoping and cropping compose

The `element=` / `region=` argument crops the image; `marks=` chooses what to
draw on it. They are independent, and either can be omitted:

```python
# whole display, marks from one app
xa11y.screenshot(marks=[app.locator("button")])

# one window cropped, marks scoped to that window
win = app.locator("window[name='Preferences']")
xa11y.screenshot(element=win.element(), marks=[win.descendant("button")])

# a fixed region, marks from the whole system
xa11y.screenshot(region=(0, 0, 1440, 90), marks=["button"])
```

Marks outside the crop land in `omitted`; they are not clamped to the edge.

#### The GIL

Selector resolution and pixel work both happen inside `py.allow_threads`. The
`marks=` arguments are parsed and the locators cloned before the block, since
that needs the GIL (tenet 5, and the same shape `screenshot(element=...)`
already has).

#### Typing

`_native.pyi` gains `LegendEntry` and `Omission` classes and the `marks`
parameter, checked against the compiled module by
`test_stub_method_signatures_match_runtime`. `MarkOmissionReason` joins
`MouseButtonName` and `AnchorName` as a `Literal` union — identically spelled in
the JS binding, since it is a value a user compares against as a literal.

### JS

`screenshot({ marks: ['button'], app: 'Safari' })` → `Screenshot` with
`.legend` / `.omitted`. `marks` accepts `Locator | string`, matching Python.
`index.d.ts` needs the `Screenshot` class members added by hand — the napi
declaration in `native.d.ts` is shadowed and reaches nobody (AGENTS.md, "Type
Declarations").

## Known limits, stated rather than papered over

**Multi-monitor.** `ScreenshotProvider::capture_full` captures the *primary*
display. An element on a second monitor has valid bounds that are outside the
capture, and lands in `omitted` with `OutsideCapture`. Not a bug to fix here;
a documented consequence of the existing capture contract.

**Occlusion.** The accessibility tree carries no z-order. An element behind
another window has bounds, so it gets a box drawn over whatever is actually
on screen there. The caller narrows with the selector (`button[visible]`)
because only the caller knows what it meant. Documented in the guide.

**Fractional scaling.** `Rect::to_physical` rounds position and size
independently, so a box can sit 1px off its true edge at 1.5×. Already
documented on `to_physical`; a 1px stroke offset is not worth a second
rounding mode.

**Mixed-DPI Wayland.** `Screenshot::scale` is a single scalar and cannot
represent per-monitor scales (see `xa11y-linux/src/scale.rs`). Marks on the
non-dominant output are misplaced. Same caveat the capture path already
carries; this feature makes it visible rather than introducing it.

## Test plan

| Layer | Where | What |
|---|---|---|
| Core unit | `xa11y-core/src/screenshot/mark.rs` | synthetic `Screenshot`, exact pixel assertions on stroke position and colour; clipping; `origin` translation; `scale` transform; tag glyph rendering; palette contrast ≥ 4.5:1; badge collision nudge; `Vec<usize>` of skipped marks |
| Core fuzz | `xa11y/fuzz/fuzz_targets/mark_ops.rs` | arbitrary rects × scales × dims, no panic, no OOB write |
| Umbrella unit | `xa11y/src/lib.rs` | legend construction against the core `MockProvider`: group/index numbering, `:nth(n)` round-trip, `omitted` classification |
| Integ | `xa11y/tests/integ/screenshot.rs` | mark the AccessKit test app's buttons; legend matches `h::named`; PNG decodes; existing headless/`Unsupported` skips reused |
| CLI | `tests/suites/cli/test_screenshot.py` | `--mark` × launchers, `--legend json` shape, `--out -` + legend refused with exit 2 |
| MCP raw | `tests/suites/cli/test_mcp.py` | argument validation, truncation flag |
| MCP SDK | `tests/mcp_client/test_interop.py` | the real client's view of the new schema — both suites, per AGENTS.md |
| Python | `xa11y-python/tests/`, `test_typing.py` | stub signature vs runtime; `test_gil_release.py` unaffected (marking is CPU work inside `allow_threads`) |
| JS | `xa11y-js/__test__/unit/typing.test.js` | `index.d.ts` members exist on the runtime object |
| Parity | `bindings/parity_allowlist.toml` | `Mark`, `Marked`, `LegendEntry`, `Omission`, `OmissionReason` classified; `Marked` flattened into `Screenshot` |
| Docs | `reference/cli.mdx`, `guides/mcp.mdx`, new guide page | Diátaxis banner + `pageType`; `cargo xtask lint-docs` |

`OmissionReason` is a new `#[non_exhaustive]` enum that the bindings map by
hand to strings, so it needs a `[[types.variant_coverage]]` entry naming
`xa11y-python/src/lib.rs`, `xa11y-js/src/types.rs`, and `xa11y/src/cli.rs`.

## Delivery

Four PRs, each independently green.

1. **Core drawing.** `Mark`, `MARK_PALETTE`, the bitmap font, `Screenshot::mark`,
   unit tests, fuzz target. No user-visible surface; nothing downstream changes.
2. **Umbrella + CLI.** `Marked`, `LegendEntry`, `Omission`, `screenshot_marked`,
   `--mark`/`--legend`, help text, `reference/cli.mdx`, CLI + integ tests.
3. **MCP.** Tool schema, handler, description, both interop suites, `guides/mcp.mdx`.
4. **Bindings.** Python + JS, parity allowlist, typing tests, the guide page,
   and `strands-xa11y/tests/check_real_surface.py` if the `use_desktop` tool
   should surface marks (worth a separate decision — see below).

Order matters only between 1 and the rest.

## Open questions

- **Should `strands-xa11y`'s `use_desktop` tool expose marks?** It is the
  clearest consumer of the feature and the reason the package exists in this
  repo. Out of scope for the four PRs above; worth its own issue.
- **Should a mark group be able to carry a caller-chosen colour?** The CLI would
  need `--mark 'button#0072B2'` or a parallel `--mark-color` flag, both ugly.
  Deferring: the palette is colour-blind-safe and deterministic by group order,
  which is what a legend needs.
- **Tag format.** Recorded above as a recommendation, not a decision. The
  requester asked for `2.15`; this doc argues for `B7` and isolates the choice
  in one function either way.
