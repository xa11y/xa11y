# Annotated screenshots

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

**Not a vision fallback.** The annotations come from the accessibility tree. An app
with no tree gets no annotations, and the feature degrades to a plain screenshot. If
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
| **annotation** | one drawn box + tag, for one element |
| `--annotate SELECTOR` | the CLI flag; each occurrence is one **group** |
| **tag** | the short text drawn in the box (`A7`) |
| **legend** | the out-of-band list mapping tag → element |

"highlight" implies transient on-screen chrome, which this is not.
"segmented" (macapptree's word) describes image segmentation, which this is
not either. `legend` is a straight borrow from maps and charts and needs no
defence.

#### The collision with accessibility's own "annotation"

"Annotation" is a live term inside accessibility. The concrete one is Windows
UIA's **`AnnotationPattern`** (`UIA_AnnotationPatternId`, `UIA_AnnotationControlTypeId`,
`AnnotationType_Comment` / `_SpellingError` / `_TrackChanges`), which Word and
Excel use for comments and tracked changes. ARIA has an annotations module too
(`role="comment"`, `role="suggestion"`, `role="mark"` — note it also owns
"mark", so that name was no safer).

Taking the word is **worth it**, for three reasons.

**The layers do not share a namespace.** Everything this feature names lives on
the screenshot side: `--annotate`, `xa11y_core::screenshot::Annotation`,
`Screenshot::annotate`, `Annotated`, `ANNOTATION_PALETTE`. A future a11y
annotation would live on `Element` and in `Role`. `Role::Annotation` is a
variant, not a type, so it cannot collide with a struct at all.

**The noun a consumer touches is `legend`, not `annotations`.** This is
structural, not luck. `annotate` is a *verb* (a flag, a kwarg, a method) and the
data it produces is a *legend*. Verbs and nouns collide far less than two nouns
would. Had the output field been `shot.annotations`, the answer here would be
different.

**A Windows-only pattern probably never gets normalized anyway.** Tenet 1 of
`design/README.md` is "abstract where platforms agree"; `AnnotationPattern` has
no AT-SPI equivalent and no macOS one (`NSAccessibilityCustomContent` is
"custom content", a different shape). By this project's own rules that data
belongs in `element.raw["patterns"]` and the UIA escape hatch, not in a
normalized name.

**The escape, reserved now so nobody has to invent it under pressure.** If a
future release does normalize it: the a11y side takes the qualified name
(`TextAnnotation`, or `element.comments` if the scope is really comments), and
the screenshot side keeps the bare one. What must *not* happen is a bare
`Element::annotations` field appearing later — that is the one shape that makes
the two genuinely ambiguous, and it is cheap to rule out today.

### Tag format

The request reaches for `2.15` — selector 2, element 15. Two problems with a
numeric group *and* a numeric index in one tag, whatever separates them:

1. **Separator loss collides two distinct tags.** `1-12` and `11-2` both render
   to `112` if the hyphen is lost to compression, downscaling, or a model
   simply not attending to a 3px horizontal bar. A dot is worse: `2.15` also
   reads as a decimal, so `2.150` is an equally plausible transcription and
   nothing type-checks it.
2. **The indices in the request are 0-based; every index in xa11y is 1-based.**
   `:nth(n)` and `Locator.nth(n)` are 1-based. A tag reading `2.15` invites
   `:nth(15)` for what is actually `:nth(16)`.

**Decision: a letter for the group, a 1-based number within it** — `A1`, `B7`,
`C12`.

The reason is (1), and it is the whole argument: a letter followed by digits is
**self-delimiting**. There is no separator, so there is nothing to lose, and no
two distinct tags can ever render to the same glyph sequence. `A12` and `AB2`
stay distinct under any amount of downscaling. That property is not available
to any all-numeric scheme.

Everything else is a bonus: the number is exactly the `:nth(n)` argument; the
letter is the text alternative to the box colour (WCAG 1.4.1, which an
accessibility library should apply to its own output unprompted); and `A12` is
three glyphs where `1-12` is four, which matters on a 20px toolbar icon.

Groups past `Z` continue `AA`, `AB`. A caller passing 27 selectors has a
different problem.

#### "Letters don't correspond to the locator index — is that an issue?"

Almost never, because **the tag is not the machine-readable channel.** Every
legend entry carries `group` as a 1-based `int` alongside the tag:

```python
e.tag      # "B7"
e.group    # 2     ← the annotate= index, no letter arithmetic
e.index    # 7
e.selector # "text_field:nth(7)"
```

So code filtering by group compares ints, and the model acting on a box uses
`e.selector` and never decodes the tag at all. The letter exists for one job:
being unambiguous *in the image*, where there is no structured field to fall
back on.

The one place the correspondence is exercised by a human is reading the CLI
legend, and the group header spells it out rather than making anyone count:

```
A  button       #0072B2   7 annotated
B  text_field   #D55E00   2 annotated
```

That leaves exactly one scenario where letters cost anything: someone holding
the image with the legend lost, wanting to know which `--annotate` flag produced
a box. A→1 is a 26-entry mapping everyone already knows from spreadsheet
columns. Against that, `1-12` costs a collision class that no lookup recovers.

#### The one variant worth considering

With a single `--annotate`, the group prefix carries no information, so tags
could be bare `1`, `2`, `3` and only grow a letter once a second group exists.
Fewer glyphs in the common case.

Recommend against: it makes the tag format conditional on an argument count, so
nothing can learn or parse one rule, and a script reading tags breaks the day
someone adds a second `--annotate`. A uniform format is worth three pixels.

`tag_for(group, index)` in `xa11y-core/src/screenshot/annotate.rs` is the only
place this is decided; the tests that assert it are the only other place a
change lands.

## Layering

Two layers, and the split is the load-bearing part of this design.

```
xa11y-core::screenshot   pure pixels: Rect + text + colour → new Screenshot
        ▲                 no Provider, no selectors, no platform
        │
xa11y (umbrella)         selectors → Locator::elements() → Vec<Annotation>
        ▲                 owns the target resolution and the legend
        │
   cli / mcp / bindings
```

Core never learns what a selector is; the umbrella crate never learns how to
set a pixel. Core's half is testable and fuzzable with no display, no app, and
no permissions, which is where essentially all the arithmetic risk lives.

## Core: the drawing half

New module `xa11y-core/src/screenshot/annotate.rs` (today `screenshot.rs` is a flat
file; it becomes a directory).

```rust
/// One box to draw: where, what to write in it, and in what colour.
///
/// `rect` is in **logical** screen coordinates, the same space as
/// `Element::bounds`. `Screenshot::annotate` converts to physical pixels.
#[non_exhaustive]
pub struct Annotation {
    pub rect: Rect,
    pub tag: String,
    pub color: [u8; 3],
}

impl Annotation {
    pub fn new(rect: Rect, tag: impl Into<String>) -> Self;   // palette[0]
    pub fn color(mut self, rgb: [u8; 3]) -> Self;            // chained setter
}

/// Colour-blind-safe qualitative palette (Okabe–Ito, minus black).
pub const ANNOTATION_PALETTE: [[u8; 3]; 7] = [ /* … */ ];

impl Screenshot {
    /// Draw `annotations` onto a copy of this capture.
    ///
    /// `origin` is the logical top-left of what this capture covers — the
    /// region passed to `capture_region`, or whatever
    /// `ScreenshotProvider::capture_full` reported alongside the pixels.
    /// Annotations are translated by it and scaled by `self.scale`.
    ///
    /// Annotations whose rect does not intersect the image are **skipped, not
    /// clamped**, and reported in the returned `Vec<usize>` of skipped
    /// indices. A box clamped to the edge would claim the wrong pixels.
    pub fn annotate(&self, annotations: &[Annotation], origin: Point) -> Result<(Screenshot, Vec<usize>)>;
}
```

`[u8; 3]` rather than an `Rgb` newtype, deliberately: a new public type costs a
`[types]` classification in `bindings/parity_allowlist.toml` and a binding
decision, and buys nothing over three bytes.

`Annotation` is `#[non_exhaustive]` with a constructor and a chained setter, matching
`ClickOptions` — it is built in `xa11y`, another crate, so it owes callers a
way to construct one (AGENTS.md, "Public API Extensibility").

### Drawing, without a new dependency

- **Boxes.** A `stroke` px outline in the annotation colour, `stroke = clamp(round(scale), 1, 4)`.
  Written straight into the RGBA buffer; no blending, no alpha.
- **Tags.** A filled badge in the annotation colour, outlined in its own
  foreground colour, sitting **outside** the box — by default resting on the
  box's top edge, left-aligned. The tag is drawn in whichever of black/white
  has more contrast against the badge colour (relative luminance, the WCAG
  formula). At 7 palette colours this is a checkable property, so the unit test
  asserts every palette entry clears 4.5:1 against its chosen foreground.

  Outside, not inside, and this was learned the expensive way: the first
  implementation put the badge at the box's inner top-left, which on a toolbar
  button is exactly where the button's own label is. The `A1` badge covered the
  word "New". An annotation that destroys the content it points at defeats the
  feature, and no unit test was ever going to notice — it took running the
  thing against a real Qt application and looking at the picture.

- **Glyphs.** An embedded 5×7 bitmap font covering `0-9` and `A-Z` — 36 glyphs
  × 7 bytes, a `const [[u8; 7]; 36]`. Scaled by `2 × stroke`, so a tag is 10×14
  px at 1× and keeps its apparent size on a HiDPI capture rather than shrinking.

  The alternative is `ab_glyph`/`fontdue` plus an embedded TTF: a dependency,
  a few hundred KB in every binary and both wheels, and a rasteriser, for two
  character classes. The bitmap table is about sixty lines and never changes.
  `image`/`imageproc` is heavier still.

- **Badge placement.** Ten candidate spots in preference order — above the box
  (left- then right-aligned), below it, beside it, and the four inner corners
  as a last resort. Each is scored on whether it is visible at all, whether it
  covers a badge already placed, and whether it fits wholly on the image; the
  first candidate holding the best score wins. Annotations are drawn
  largest-area-first so small elements land on top. Greedy, bounded, and good
  enough; a layout solver is not warranted.

  Two things this deliberately does not solve. A badge can still cover a
  *neighbouring* element's pixels — in a dense form there is nowhere to put a
  badge that covers nothing, and the guarantee that matters is that an
  annotated element stays readable. And a box filling the whole capture has no
  on-image outside spot, so it falls through to an inner corner and covers its
  own content; that is the one case where the old failure mode survives, and it
  is the case where the box is a window rather than a control.

- **Duplicates are not deduplicated.** Two selectors matching one element get
  two annotations and two legend entries. Merging them would silently drop a group's
  membership, which is information the caller asked for.

### Overflow

`Rect` is `i32`, `scale` is `f32`, and the products index a `Vec<u8>`. Every
coordinate goes through `Rect::to_physical` (which already sanitises a
non-finite or non-positive scale) and then a checked conversion to a pixel
index. A new `cargo-fuzz` target — `xa11y/fuzz/fuzz_targets/annotate_ops.rs` —
drives `Screenshot::annotate` with arbitrary rects, scales, and image dimensions
and asserts it neither panics nor writes out of bounds.

## Umbrella: the resolution half

```rust
pub fn screenshot_annotated(
    region: Option<Rect>,
    groups: &[Locator],
) -> Result<Annotated>;

/// A capture plus the legend describing what was drawn on it.
#[non_exhaustive]
pub struct Annotated {
    pub screenshot: Screenshot,
    pub legend: Vec<LegendEntry>,
    pub omitted: Vec<Omission>,
}

#[non_exhaustive]
pub struct LegendEntry {
    pub tag: String,          // "B7"
    pub group: usize,         // 1-based, matches the --annotate order
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
or one outside the captured pixels is dropped from the image — dropping it
*silently* would leave a legend that disagrees with the picture and no way to
find out why. See "Multi-monitor" below for what a full capture does and does
not cover.

### Why `Locator` and not `&str`

A selector alone has no scope. Taking a `Locator` means the caller has already
said what tree it searches, the chained forms (`app.locator("toolbar").child("button")`)
work unchanged, and the umbrella crate does not grow a second app-resolution
path next to the one `cli::resolve_app` already owns.

A `Locator` can still be rootless, and that is refused — see the Python section.
The scope is not decoration: it is what each entry's `<selector>:nth(n)` is
resolved against, and a group with no scope has no numbering the entry can
round-trip through.

The CLI still takes strings, because a command line has no other option; it
resolves `--app`/`--pid`/`--shell` once and builds one `Locator` per `--annotate`.

## Surfaces

### `Screenshot` grows a legend, rather than a second function

`Screenshot` is `#[non_exhaustive]`, so adding a field is not breaking for
readers, and `Screenshot::new` keeps its signature (backends set an empty
legend). The bindings then expose **one** `screenshot()` whose return type does
not depend on its arguments — AGENTS.md, "Options structs fold into the primary
verb": two names for one operation is worse than one name with options.

Rust keeps `Annotated` as a distinct type because Rust callers can destructure it;
the bindings flatten it onto `Screenshot` (`shot.legend`, `shot.omitted`, both
empty for an unannotated capture) and declare the flatten in the parity allowlist.

### CLI

```
xa11y screenshot [--region X,Y,W,H] --out PATH
                 [--app NAME | --pid PID | --shell KIND]
                 [--annotate SELECTOR]...
                 [--legend text|json|none]
```

`--annotate` is repeatable and is the opt-in: with none, behaviour is byte-identical
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

Text legend, one group header plus one line per annotation:

```
A  button       #0072B2  7 annotated
B  text_field   #D55E00  2 annotated

A1  button      "7"          bounds=104,318,48,44   button:nth(1)
A2  button      "8"          bounds=156,318,48,44   button:nth(2)
…
B1  text_field  "Display"    bounds=100,60,320,52   text_field:nth(1)

omitted: 1 element (outside_capture: button "Paste")
```

### MCP

`screenshot` gains `annotate: string[]` and the shared `app`/`pid`/`shell` target
properties. The result keeps its image content and gains `legend`, `omitted`,
and `truncated` in the JSON summary.

Three things the tool description must state, because a model that discovers
them by experiment spends calls doing it:

- Annotations come from the accessibility tree, so an app without one gets no annotations.
- The tag format, and that the number is the `:nth(n)` argument.
- The legend cap (100 entries) and that `truncated` reports when it bit.

The cap is the "Results are bounded" rule from AGENTS.md. A `--annotate div`-style
selector over a large tree would otherwise put a thousand entries in a context
window. Annotations past the cap are neither drawn nor listed, and `truncated` says
how many.

### Python

`annotate=` is a keyword-only parameter on the existing `screenshot()`. Absent, the
call is exactly what it is today.

```python
import xa11y

app = xa11y.App.by_name("Calculator")

shot = xa11y.screenshot(
    element=app.locator("window").element(),
    annotate=[app.locator("button"), app.locator("text_field")],
)
shot.save_png("calc.png")
```

`annotate` accepts `Locator | str`, and every group must be **scoped to an
application**. A bare string builds a rootless locator, the same as
`xa11y.locator(s)`, and those are refused with `InvalidSelectorError`: a
rootless search runs once per application and concatenates the results, so its
`:nth(n)` counts within one application while the legend numbers matches across
all of them. With one button in the first application and three in the second,
the entry for the second application's first button would carry
`button:nth(2)` — which resolves to that application's *second* button, with no
error. Numbering per application and adding the owning pid to each entry would
only help a caller who reads the pid; `entry.selector` on its own, which is the
documented round trip, would stay wrong. The error names the fix.

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
| `group` | `int` | 1-based, matching the `annotate=` order |
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

Both `legend` and `omitted` are `[]` on an unannotated capture, so consumers need
no version check.

#### Scoping and cropping compose

The `element=` / `region=` argument crops the image; `annotate=` chooses what to
draw on it. They are independent, and either can be omitted:

```python
# whole display, annotations from one app
xa11y.screenshot(annotate=[app.locator("button")])

# one window cropped, annotations scoped to that window
win = app.locator("window[name='Preferences']")
xa11y.screenshot(element=win.element(), annotate=[win.descendant("button")])

# a fixed region, annotations from one app's menu bar items
xa11y.screenshot(region=(0, 0, 1440, 90), annotate=[app.locator("menu_item")])
```

Annotations outside the crop land in `omitted`; they are not clamped to the edge.

#### The GIL

Selector resolution and pixel work both happen inside `py.allow_threads`. The
`annotate=` arguments are parsed and the locators cloned before the block, since
that needs the GIL (tenet 5, and the same shape `screenshot(element=...)`
already has).

#### Typing

`_native.pyi` gains `LegendEntry` and `Omission` classes and the `annotate`
parameter, checked against the compiled module by
`test_stub_method_signatures_match_runtime`. `OmissionReasonName` joins
`MouseButtonName` and `AnchorName` as a `Literal` union — identically spelled in
the JS binding, since it is a value a user compares against as a literal.

### JS

`screenshot({ annotate: [app.locator('button')] })` → `Screenshot` with
`.legend` / `.omitted`. `annotate` accepts `Locator | string` and refuses
rootless groups, matching Python.
`index.d.ts` needs the `Screenshot` class members added by hand — the napi
declaration in `native.d.ts` is shadowed and reaches nobody (AGENTS.md, "Type
Declarations").

## Known limits, stated rather than papered over

**Multi-monitor.** What `ScreenshotProvider::capture_full` covers is the
backend's own answer, and it is not "the primary display" everywhere: Windows
captures the whole **virtual desktop**, Linux/X11 the root window, macOS one
`SCDisplay`. So an element on a second monitor lands in `omitted` with
`OutsideCapture` on macOS, and is genuinely *in the image* on Windows and X11.

What is uniform is that the capture reports **where** it is.
`capture_full` returns the logical coordinate its pixel `(0, 0)` sits at
alongside the pixels, and `screenshot_annotated` translates every box by it.
That origin is not `(0, 0)` on Windows — the virtual desktop's top-left goes
negative as soon as a monitor is arranged left of or above the primary — nor on
a Mac whose `displays[0]` is not at the coordinate-space origin. Assuming
`(0, 0)` there did not put anything in `omitted`: the shifted rectangles still
landed inside the (wider) image, so every box was drawn one monitor's width out
of place and the legend said nothing was wrong.

The residue is `Screenshot::scale`, which is a single scalar for a capture that
may span monitors at different DPI. Windows reports the scale of the monitor at
the virtual origin: exact on a uniform-DPI desktop, and off by the DPI ratio for
boxes on a monitor that scales differently. Capturing per-monitor is the fix and
it changes the capture contract, so it stays stated here rather than papered
over. The Wayland note below is the same limitation from the other side.

**Occlusion.** The accessibility tree carries no z-order. An element behind
another window has bounds, so it gets a box drawn over whatever is actually
on screen there. The caller narrows with the selector (`button[visible]`)
because only the caller knows what it meant. Documented in the guide.

**Fractional scaling.** `Rect::to_physical` rounds position and size
independently, so a box can sit 1px off its true edge at 1.5×. Already
documented on `to_physical`; a 1px stroke offset is not worth a second
rounding mode.

**Mixed-DPI Wayland.** `Screenshot::scale` is a single scalar and cannot
represent per-monitor scales (see `xa11y-linux/src/scale.rs`). Annotations on the
non-dominant output are misplaced. Same caveat the capture path already
carries; this feature makes it visible rather than introducing it.

## Test plan

| Layer | Where | What |
|---|---|---|
| Core unit | `xa11y-core/src/screenshot/annotate.rs` | synthetic `Screenshot`, exact pixel assertions on stroke position and colour; clipping; `origin` translation; `scale` transform; tag glyph rendering; palette contrast ≥ 4.5:1; badge placement preference order and clipping; `Vec<usize>` of skipped annotations |
| Core fuzz | `xa11y/fuzz/fuzz_targets/annotate_ops.rs` | arbitrary rects × scales × dims, no panic, no OOB write |
| Umbrella unit | `xa11y/src/lib.rs` | legend construction against the core `MockProvider`: group/index numbering, `:nth(n)` round-trip, `omitted` classification |
| Integ | `xa11y/tests/integ/screenshot.rs` | annotation the AccessKit test app's buttons; legend matches `h::named`; PNG decodes; existing headless/`Unsupported` skips reused |
| CLI | `tests/suites/cli/test_screenshot.py` | `--annotate` × launchers, `--legend json` shape, `--out -` + legend refused with exit 2 |
| MCP raw | `tests/suites/cli/test_mcp.py` | argument validation, truncation flag |
| MCP SDK | `tests/mcp_client/test_interop.py` | the real client's view of the new schema — both suites, per AGENTS.md |
| Python | `xa11y-python/tests/`, `test_typing.py` | stub signature vs runtime; `test_gil_release.py` unaffected (annotating is CPU work inside `allow_threads`) |
| JS | `xa11y-js/__test__/unit/typing.test.js` | `index.d.ts` members exist on the runtime object |
| Parity | `bindings/parity_allowlist.toml` | `Annotation`, `Annotated`, `LegendEntry`, `Omission`, `OmissionReason` classified; `Annotated` flattened into `Screenshot` |
| Docs | `reference/cli.mdx`, `guides/mcp.mdx`, new guide page | Diátaxis banner + `pageType`; `cargo xtask lint-docs` |

`OmissionReason` is a new `#[non_exhaustive]` enum that the bindings map by
hand to strings, so it needs a `[[types.variant_coverage]]` entry naming
`xa11y-python/src/lib.rs`, `xa11y-js/src/types.rs`, and `xa11y/src/cli.rs`.

## Delivery

Four PRs, each independently green.

1. **Core drawing.** `Annotation`, `ANNOTATION_PALETTE`, the bitmap font, `Screenshot::annotate`,
   unit tests, fuzz target. No user-visible surface; nothing downstream changes.
2. **Umbrella + CLI.** `Annotated`, `LegendEntry`, `Omission`, `screenshot_annotated`,
   `--annotate`/`--legend`, help text, `reference/cli.mdx`, CLI + integ tests.
3. **MCP.** Tool schema, handler, description, both interop suites, `guides/mcp.mdx`.
4. **Bindings.** Python + JS, parity allowlist, typing tests, the guide page,
   and `strands-xa11y/tests/check_real_surface.py` if the `use_desktop` tool
   should surface annotations (worth a separate decision — see below).

Order matters only between 1 and the rest.

## Open questions

- **Should `strands-xa11y`'s `use_desktop` tool expose annotations?** It is the
  clearest consumer of the feature and the reason the package exists in this
  repo. Out of scope for the four PRs above; worth its own issue.
- **Should an annotation group be able to carry a caller-chosen colour?** The
  CLI would need `--annotate 'button#0072B2'` or a parallel `--annotate-color`
  flag, both ugly. Deferring: the palette is colour-blind-safe and deterministic
  by group order, which is what a legend needs.

Tag format is **settled** (`A1` / `B7`, see above), as is the vocabulary
(annotation / tag / legend).
