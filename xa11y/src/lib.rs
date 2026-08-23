//! xa11y — Cross-Platform Accessibility Client Library
//!
//! Provides a unified API for reading and interacting with accessibility trees
//! across desktop platforms (macOS, Windows, Linux).
//!
//! # Quick Start
//!
//! ```no_run
//! use std::time::Duration;
//! use xa11y::*;
//!
//! let app = App::by_name("Safari", Duration::from_secs(5)).expect("App not found");
//!
//! for child in app.children().unwrap() {
//!     println!("{}: {:?}", child.role, child.name);
//! }
//!
//! app.locator(r#"button[name="OK"]"#).press().expect("Failed to press");
//! ```

use std::sync::{Arc, OnceLock};

// Re-export public types.
pub use xa11y_core::{
    App, Diagnosis, Element, ElementData, ElementState, Error, Event, EventKind, Locator,
    RawPlatformData, Rect, Result, Role, ShellSurface, ShellSurfaceKind, StateFlag, StateSet,
    Subscription, SubscriptionIter, Toggled, TreeNode,
};

// `#[doc(hidden)]`: the provider-side construction contracts. Re-exported so
// an out-of-tree `Provider` implementation can reach them — without this the
// completeness guard stops at this repo's own backends. Not public API.
#[doc(hidden)]
pub use xa11y_core::{ElementParts, EventParts, StateParts};

// Re-export the process-wide default-timeout configuration (see
// `xa11y_core::config`): the default for every auto-wait / `wait_*` call
// that doesn't pass an explicit timeout. `set_default_timeout` overrides the
// `XA11Y_DEFAULT_TIMEOUT` environment variable, which overrides the built-in
// 5 seconds.
pub use xa11y_core::{default_timeout, set_default_timeout, DEFAULT_TIMEOUT_ENV_VAR};

// Re-export input simulation surface.
pub use xa11y_core::input;
pub use xa11y_core::{
    anchor_point, point_for, Anchor, ClickOptions, ClickTarget, DragOptions, InputProvider,
    InputSim, IntoPoint, Key, Keyboard, Mouse, MouseButton, Point, ScrollDelta,
};

// Re-export screenshot surface. The annotation *result* types live in
// `xa11y_core::screenshot::legend` — `screenshot_annotated` below is the only
// thing that builds one, but the data is core's, on the same terms as
// `Screenshot` itself.
pub use xa11y_core::screenshot;
pub use xa11y_core::screenshot::{Annotated, LegendEntry, Omission, OmissionReason};
pub use xa11y_core::{Screenshot, ScreenshotProvider};

// Re-export bidi text helpers (see `xa11y_core::text`). `name`, `value`, and
// `description` on `ElementData` are stripped of bidi format controls; these
// helpers let callers strip ad-hoc strings or check membership.
pub use xa11y_core::{is_bidi_control, strip_bidi, strip_bidi_opt};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider, RecvStatus, Selector, SelectorGroup};

/// Shared in-memory mock Provider — re-exported from `xa11y-core` when the
/// `test-support` feature is enabled. Used by language-binding tests so
/// Python and JS don't each carry their own copy of the fixture.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use xa11y_core::mock;

#[doc(hidden)]
#[cfg(feature = "cli")]
pub mod cli;

// The MCP stdio server behind `xa11y mcp`. Reached only through `cli::run`,
// so it rides the same feature.
#[cfg(feature = "cli")]
mod mcp;

// Re-export the extension traits so `use xa11y::*` enables `App::by_name(...)`
// and `ShellSurface::by_kind(...)`.
pub use app_ext::AppExt;
pub use shell_ext::ShellSurfaceExt;

// ── Internal singleton ──────────────────────────────────────────────────────

static PROVIDER: OnceLock<std::result::Result<&'static dyn Provider, String>> = OnceLock::new();

fn get_provider_ref() -> Result<&'static dyn Provider> {
    PROVIDER
        .get_or_init(|| {
            create_provider_boxed()
                .map(|b| &*Box::leak(b))
                .map_err(|e| format!("{e}"))
        })
        .as_ref()
        .copied()
        .map_err(|msg| Error::Platform {
            code: -1,
            message: msg.clone(),
        })
}

#[doc(hidden)]
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(get_provider_ref()?))
}

// ── Platform provider construction (internal) ───────────────────────────────

#[doc(hidden)]
#[cfg(feature = "testing")]
pub fn create_provider() -> Result<Arc<dyn Provider>> {
    create_provider_boxed().map(Arc::from)
}

/// Build an [`InputSim`] backed by the platform's native input-synthesis API
/// (CGEvent on macOS, SendInput on Windows, XTest on X11). Returns
/// [`Error::Unsupported`] on a Wayland-only Linux session and
/// [`Error::Platform`] on any other platform we don't ship a backend for.
///
/// `InputSim` is cheap to clone — construct one and share it.
pub fn input_sim() -> Result<InputSim> {
    #[cfg(target_os = "macos")]
    {
        let backend = xa11y_macos::MacOSInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(target_os = "windows")]
    {
        let backend = xa11y_windows::WindowsInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(target_os = "linux")]
    {
        let backend = xa11y_linux::LinuxInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!(
                "Input simulation not available on platform: {}",
                std::env::consts::OS
            ),
        })
    }
}

// ── Screenshot entry points ────────────────────────────────────────────
//
// Three bare functions instead of a factory + handle. The platform backend
// (ScreenCaptureKit on macOS, X11 `GetImage` or xdg-desktop-portal on Linux,
// GDI on Windows) is initialised lazily on first call and memoized in a
// `OnceLock`, so repeated captures reuse the same backend without paying
// construction cost per call.
//
// All three return:
// - [`Error::PermissionDenied`] on macOS if Screen Recording is not granted
//   (or on Linux if the Wayland portal denies consent).
// - [`Error::Unsupported`] on Linux if neither `DISPLAY` nor `WAYLAND_DISPLAY`
//   is set, and on older Windows contexts where `BitBlt` is unavailable.

static SCREENSHOT_BACKEND: OnceLock<std::result::Result<Arc<dyn ScreenshotProvider>, String>> =
    OnceLock::new();

fn screenshot_backend() -> Result<Arc<dyn ScreenshotProvider>> {
    SCREENSHOT_BACKEND
        .get_or_init(create_screenshot_backend)
        .as_ref()
        .cloned()
        .map_err(|msg| Error::Platform {
            code: -1,
            message: msg.clone(),
        })
}

fn create_screenshot_backend() -> std::result::Result<Arc<dyn ScreenshotProvider>, String> {
    #[cfg(target_os = "macos")]
    {
        xa11y_macos::MacOSScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(target_os = "windows")]
    {
        xa11y_windows::WindowsScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(target_os = "linux")]
    {
        xa11y_linux::LinuxScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(format!(
            "Screenshot not available on platform: {}",
            std::env::consts::OS
        ))
    }
}

/// Capture the full screen.
///
/// What "full" covers is the backend's own answer — the virtual desktop on
/// Windows, one `SCDisplay` on macOS, the X11 root window on Linux — and so is
/// where its pixel `(0, 0)` sits. [`screenshot_annotated`] needs that origin
/// and takes it from [`ScreenshotProvider::capture_full`]; a caller who only
/// wants pixels does not, so it is dropped here rather than widening the
/// return type of the oldest entry point in this module.
pub fn screenshot() -> Result<Screenshot> {
    screenshot_backend()?.capture_full().map(|(shot, _)| shot)
}

/// Capture an explicit sub-rectangle of the screen.
pub fn screenshot_region(rect: Rect) -> Result<Screenshot> {
    screenshot_backend()?.capture_region(rect)
}

/// Capture the pixels under an element's current bounds.
///
/// Returns [`Error::NoElementBounds`] if the element has no bounds. The target
/// window is **not** raised or activated — see the `screenshot` module docs.
pub fn screenshot_element(element: &Element) -> Result<Screenshot> {
    let rect = element.bounds.ok_or(Error::NoElementBounds)?;
    screenshot_backend()?.capture_region(rect)
}

// ── Annotated screenshots ───────────────────────────────────────────────
//
// The *resolution* half of the annotated-screenshot feature. `xa11y-core`'s
// `screenshot::annotate` module owns the pixels: it takes rectangles, tags and
// colours and writes RGBA bytes, and it knows nothing about selectors. This
// half owns the other direction — turning [`Locator`]s into those rectangles,
// and producing the legend that maps a drawn tag back to a selector the caller
// can act on.
//
// The split is deliberate: core's arithmetic is testable and fuzzable with no
// display, and this crate never learns how to set a pixel.
//
// The *result* types live in core too (`xa11y_core::screenshot::legend`), and
// are re-exported above so `xa11y::LegendEntry` keeps working. They are data,
// not resolution: leaving them here would have put four binding-facing types
// outside the reach of `cargo xtask check-bindings-parity`, which reads
// `xa11y-core`'s rustdoc JSON and nothing else.

/// Largest number of elements one annotated capture describes.
///
/// [`screenshot_annotated`] stops after this many elements across **all**
/// groups — counting both the ones it draws and the ones it reports in
/// [`Annotated::omitted`] — and reports how many it did not reach in
/// [`Annotated::truncated`]. A selector like `*` over a large tree would
/// otherwise put thousands of legend entries into an agent's context window.
///
/// Past the cap nothing is built: no selector is formatted and no entry is
/// allocated, so the cap bounds the describing as well as the result. What it
/// does not bound is the tree read — a provider hands back everything its
/// selector matched before the cap is consulted.
///
/// The cap is never silent: a caller that sees `truncated > 0` knows the
/// legend is a prefix of what matched, and narrows the selector.
pub const MAX_ANNOTATIONS: usize = 100;

/// Capture the screen with boxes drawn over the elements each locator in
/// `groups` matches, plus a legend mapping every box back to a selector.
///
/// `region` chooses what is captured — `None` for the full primary display,
/// `Some(rect)` for an explicit sub-rectangle — and `groups` chooses what is
/// drawn on it. The two are independent: a locator may match elements outside
/// the captured area, and those land in [`Annotated::omitted`] with
/// [`OmissionReason::OutsideCapture`] rather than being clamped to an edge.
///
/// Each locator is one **group**: it gets one palette colour
/// (`ANNOTATION_PALETTE[(group - 1) % 7]`, cycling past the seventh) and one
/// tag letter, so `groups[1]`'s third match is tagged `B3` and its legend
/// entry reads `group = 2, index = 3`.
///
/// Every group must be **scoped to an application** — `app.locator(..)`, or
/// anything derived from one. A rootless locator (`xa11y::locator("button")`)
/// is refused; see the `Errors` section for why.
///
/// Nothing is deduplicated. Two locators matching one element produce two
/// boxes and two legend entries, because which groups an element belongs to
/// is what the caller asked for.
///
/// # The selector round-trip
///
/// [`LegendEntry::selector`] is the group's selector plus `:nth(n)`, and it
/// resolves against the same scope the group's [`Locator`] had:
///
/// ```no_run
/// # use xa11y::*;
/// # use std::time::Duration;
/// let app = App::by_name("Calculator", Duration::from_secs(5))?;
/// let shot = screenshot_annotated(None, &[app.locator("button")])?;
///
/// // A model read "A7" off the PNG; act on the element it labels.
/// let entry = shot.legend.iter().find(|e| e.tag == "A7").expect("A7");
/// app.locator(&entry.selector).press()?;
/// # Ok::<(), xa11y::Error>(())
/// ```
///
/// # Groups that describe nothing
///
/// A group whose selector matches no element contributes no legend entry and
/// no omission, and the same is true of a locator whose `nth` is past the end
/// of its match set. That is deliberate, and it is the one asymmetry worth
/// naming: [`Annotated::omitted`] describes *elements that were found and
/// could not be drawn*, so it has a role, a name and a selector for each.
/// A selector that matched nothing has no element to describe — there is
/// nothing to put in an omission but the selector the caller already holds,
/// and the absence of any `A`-tagged entry says the same thing. Nothing is
/// substituted and nothing is retried, so tenet 1 is not in play.
///
/// # Bounds
///
/// At most [`MAX_ANNOTATIONS`] elements are described in total; anything
/// beyond that is counted in [`Annotated::truncated`], and no selector is
/// built for it.
///
/// # Errors
///
/// - [`Error::InvalidSelector`] if a group's locator is **rootless**. An
///   entry's selector round-trips as `<selector>:nth(n)`, and a rootless
///   locator resolves that `:nth` once per application, whereas the legend
///   numbers matches across all of them — so the two disagree as soon as two
///   applications match, and the entry names a different element than the box
///   it labels. Scope the group to an application: `app.locator(..)`.
/// - [`Error::InvalidSelector`] if a group's selector does not parse, or if it
///   is a comma-separated alternation (`"button, link"`). An entry's selector
///   must round-trip as `<selector>:nth(n)`, and appending `:nth(n)` to a
///   group would bind to its last clause alone. Pass one locator per clause —
///   which also gives each clause its own colour.
/// - Whatever resolving a locator surfaces (the application is gone, the
///   platform refused a tree read).
/// - [`Error::PermissionDenied`] / [`Error::Unsupported`] from the capture
///   itself, on the same terms as [`screenshot`] and [`screenshot_region`].
///
/// # Timing
///
/// Selectors are resolved *before* the capture, so a failing selector costs no
/// pixels. The tree read and the capture are therefore not simultaneous: an
/// element that moves between the two is boxed where it was, which is the same
/// race `screenshot_element` already has.
pub fn screenshot_annotated(region: Option<Rect>, groups: &[Locator]) -> Result<Annotated> {
    let (resolved, truncated) = resolve_groups(groups)?;
    let (annotations, mut legend, mut omitted) = plan_annotations(&resolved);

    // The origin is where the capture's pixel (0, 0) sits in logical screen
    // coordinates, and every box is placed relative to it. For a region that
    // is the rect the caller asked for, by `capture_region`'s contract. For a
    // full capture only the backend knows: Windows captures the virtual
    // desktop, whose top-left goes negative as soon as a monitor sits left of
    // or above the primary, and macOS captures a display that need not be at
    // the coordinate-space origin. Assuming (0, 0) drew every box a monitor's
    // width out of place and reported nothing, because the shifted rects still
    // landed inside the wider image.
    let (shot, origin) = match region {
        Some(rect) => (screenshot_region(rect)?, Point::new(rect.x, rect.y)),
        None => screenshot_backend()?.capture_full()?,
    };
    let drawn = draw_and_reconcile(&shot, origin, &annotations, &mut legend, &mut omitted)?;

    Ok(Annotated::for_capture(drawn, legend, omitted, truncated))
}

/// Draw `annotations` onto `shot`, and move the legend entries core could not
/// place into `omitted`.
///
/// `annotations` and `legend` are index-aligned, which is what lets the
/// `Vec<usize>` core returns be read as legend positions.
///
/// Core reports one `skipped` list for two different situations, and the
/// difference is invisible from inside it, so it is resolved here from the
/// capture's own scale. [`plan_annotations`] has already removed everything
/// with no *logical* rectangle, but a rectangle with logical area can still
/// come out empty in *physical* pixels: `Rect::to_physical` rounds, so a 1×1
/// logical box at `scale = 0.25` becomes 0×0 and can never overlap the image.
/// That is [`OmissionReason::ZeroArea`], the same reason a 0-width element
/// gets, and calling it `OutsideCapture` would tell the caller to look at a
/// different monitor for a box that has no size anywhere. Everything else that
/// was skipped genuinely fell outside the captured pixels.
///
/// Split out from [`screenshot_annotated`] so the reconciliation is testable
/// against a synthetic [`Screenshot`], with no display and no permissions.
fn draw_and_reconcile(
    shot: &Screenshot,
    origin: Point,
    annotations: &[screenshot::Annotation],
    legend: &mut Vec<LegendEntry>,
    omitted: &mut Vec<Omission>,
) -> Result<Screenshot> {
    let (drawn, skipped) = shot.annotate(annotations, origin)?;
    // The same clamp `Rect::to_physical` applies internally: a non-finite or
    // non-positive scale is identity, never garbage.
    let scale = if shot.scale.is_finite() && shot.scale > 0.0 {
        f64::from(shot.scale)
    } else {
        1.0
    };
    // Descending, so each removal cannot shift an index still to be removed.
    for i in skipped.iter().rev() {
        let entry = legend.remove(*i);
        let physical = entry.bounds.to_physical(scale);
        let reason = if physical.width == 0 || physical.height == 0 {
            OmissionReason::ZeroArea
        } else {
            OmissionReason::OutsideCapture
        };
        omitted.push(Omission::new(
            entry.selector,
            entry.role,
            entry.name,
            reason,
        ));
    }
    Ok(drawn)
}

/// One matched element, with everything both a legend entry and an omission
/// need. Produced by [`resolve_groups`]; consumed by [`plan_annotations`].
#[derive(Debug)]
struct Resolved {
    group: usize,
    index: usize,
    selector: String,
    role: String,
    name: Option<String>,
    bounds: Option<Rect>,
    color: [u8; 3],
}

/// Resolve every group, stopping at [`MAX_ANNOTATIONS`] and reporting how many
/// matches were left unresolved.
fn resolve_groups(groups: &[Locator]) -> Result<(Vec<Resolved>, usize)> {
    let mut out = Vec::new();
    let mut truncated = 0_usize;

    for (g, locator) in groups.iter().enumerate() {
        let group = g + 1;
        let color = screenshot::ANNOTATION_PALETTE[g % screenshot::ANNOTATION_PALETTE.len()];
        let base = locator.selector().trim();
        // Both of these run before the tree read, so a group that cannot
        // produce a round-tripping selector costs neither a walk nor pixels.
        require_scoped(locator, base)?;
        let scheme = numbering_scheme(base)?;
        let elements = locator.elements()?;

        // `elements()` ignores the locator's own `nth`, so a caller who wrote
        // `app.locator("button").first()` would otherwise get every button
        // boxed. Honour it: the selection is what the locator says it is.
        let selected: Vec<usize> = match locator.nth_index() {
            // Out of range is not an error here: `elements()` already returned
            // everything the selector matched, and a locator asking for a
            // match that does not exist simply annotates nothing. See
            // "Groups that describe nothing" on `screenshot_annotated`.
            Some(k) => (0..elements.len()).filter(|&i| i == k).collect(),
            None => (0..elements.len()).collect(),
        };

        for i in selected {
            if out.len() >= MAX_ANNOTATIONS {
                // Counted, not described — and deliberately before
                // `entry_at`, so the cap bounds the selector building too
                // rather than only the size of what is returned.
                truncated += 1;
                continue;
            }
            let element = &elements[i];
            let (index, selector) = entry_at(base, scheme, i);
            out.push(Resolved {
                group,
                index,
                selector,
                role: element.role.to_snake_case().to_string(),
                name: element.name.clone(),
                bounds: element.bounds,
                color,
            });
        }
    }

    Ok((out, truncated))
}

/// Refuse a group whose locator searches every application at once.
///
/// A legend entry's selector round-trips as `<selector>:nth(n)`, resolved
/// against the scope the group's locator had. A rootless locator has no such
/// scope: `Locator` runs the search **once per application** and concatenates
/// the results, so its `:nth(n)` counts within one application while the
/// legend numbers matches across all of them. With one button in the first
/// application and three in the second, the entry describing the second
/// application's first button carries `button:nth(2)` — which resolves to that
/// application's *second* button, silently, and `button:nth(1)` matches two
/// elements at once.
///
/// The alternative — numbering per application and adding the owning pid to
/// every entry — only helps a caller who reads the pid; `entry.selector` on
/// its own, which is the documented round trip and what every surface uses,
/// would stay wrong for anyone who does not. Refusing is total. It is also the
/// treatment the other non-round-tripping selector already gets, in
/// [`numbering_scheme`].
fn require_scoped(locator: &Locator, base: &str) -> Result<()> {
    if locator.root().is_some() {
        return Ok(());
    }
    Err(Error::InvalidSelector {
        selector: base.to_string(),
        message: format!(
            "annotation groups must be scoped to an application: a rootless locator is \
             resolved once per application and the results concatenated, so the `:nth(n)` \
             every legend entry carries would count within one application while the legend \
             counts across all of them — and `{base}:nth(n)` would then name a different \
             element than the box beside it. Scope the group, e.g. \
             `app.locator({base:?})`"
        ),
    })
}

/// Validate `base` as an annotation selector and return how its matches are
/// numbered: `Some(k)` when a trailing `:nth(k)` has already collapsed the
/// match set to one element, `None` when each match needs its own `:nth`.
///
/// Pure, and it runs before the tree read: an alternation costs no walk.
///
/// `base` is expected to be trimmed — `"button "` plus `":nth(1)"` would parse
/// as a *descendant* segment, which selects something else entirely.
fn numbering_scheme(base: &str) -> Result<Option<usize>> {
    let group = SelectorGroup::parse(base)?;
    if group.clauses.len() > 1 {
        return Err(Error::InvalidSelector {
            selector: base.to_string(),
            message: format!(
                "annotation selectors cannot be comma-separated alternations: every legend \
                 entry names one element as `<selector>:nth(n)`, and `{base}:nth(n)` would \
                 bind to the last clause alone. Pass one annotation group per clause — each \
                 then also gets its own colour"
            ),
        });
    }

    // A trailing `:nth(k)` has already collapsed the match set to that one
    // element, so the selector round-trips unchanged and `k` — not `1` — is
    // the element's `:nth` argument. Appending a second `:nth` would not even
    // parse ("expected combinator between selectors").
    Ok(group
        .clauses
        .first()
        .and_then(|clause| clause.segments.last())
        .and_then(|segment| segment.simple.nth))
}

/// The `:nth(n)` argument and the round-tripping selector for the `i`-th
/// (0-based) match of `base`, under the scheme [`numbering_scheme`] returned.
///
/// One match at a time on purpose: [`resolve_groups`] stops calling it at
/// [`MAX_ANNOTATIONS`], so a selector over a huge tree does not allocate a
/// String per match it will never describe.
fn entry_at(base: &str, scheme: Option<usize>, i: usize) -> (usize, String) {
    match scheme {
        Some(k) => (k, base.to_string()),
        None => (i + 1, format!("{base}:nth({})", i + 1)),
    }
}

/// The `:nth(n)` argument and the round-tripping selector for each of `count`
/// matches of `base`, in match order.
///
/// [`resolve_groups`] does not use this — it numbers one match at a time so
/// the cap bounds the work — but the round-trip guarantee is easiest to state
/// and to test over a whole match set at once.
#[cfg(test)]
fn entry_numbering(base: &str, count: usize) -> Result<Vec<(usize, String)>> {
    let scheme = numbering_scheme(base)?;
    Ok((0..count).map(|i| entry_at(base, scheme, i)).collect())
}

/// Split resolved matches into the boxes to draw (with their legend entries,
/// index-aligned) and the elements that have no drawable geometry.
///
/// Pure, and the only place [`OmissionReason::NoBounds`] and
/// [`OmissionReason::ZeroArea`] are decided: core reports both a zero-area
/// rect and an off-image rect through one `skipped` list, so they have to be
/// told apart here, before it is called.
fn plan_annotations(
    resolved: &[Resolved],
) -> (Vec<screenshot::Annotation>, Vec<LegendEntry>, Vec<Omission>) {
    let mut annotations = Vec::new();
    let mut legend = Vec::new();
    let mut omitted = Vec::new();

    for r in resolved {
        // One `match` decides drawable-or-why-not, so there is no second place
        // a case could be added and forgotten.
        let drawable = match r.bounds {
            Some(b) if b.width > 0 && b.height > 0 => Ok(b),
            Some(_) => Err(OmissionReason::ZeroArea),
            None => Err(OmissionReason::NoBounds),
        };
        match drawable {
            Ok(bounds) => {
                // The entry owns the tag — `LegendEntry::new` derives it from
                // the same group/index pair, so the box and its legend line
                // cannot disagree about what is drawn in it.
                let entry = LegendEntry::new(
                    r.group,
                    r.index,
                    r.selector.clone(),
                    r.role.clone(),
                    r.name.clone(),
                    bounds,
                    r.color,
                );
                annotations
                    .push(screenshot::Annotation::new(bounds, entry.tag.clone()).color(r.color));
                legend.push(entry);
            }
            Err(reason) => omitted.push(Omission::new(
                r.selector.clone(),
                r.role.clone(),
                r.name.clone(),
                reason,
            )),
        }
    }

    (annotations, legend, omitted)
}

fn create_provider_boxed() -> Result<Box<dyn Provider>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(xa11y_macos::MacOSProvider::new()?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(xa11y_windows::WindowsProvider::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(xa11y_linux::LinuxProvider::new()?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!("Unsupported platform: {}", std::env::consts::OS),
        })
    }
}

// ── AppExt extension trait ───────────────────────────────────────────────────

mod app_ext {
    use std::time::Duration;

    use super::{provider, App, ElementData, Result};

    /// Extension trait that adds singleton-based constructors to [`App`].
    ///
    /// Imported automatically via `use xa11y::*`.
    ///
    /// # Example
    /// ```no_run
    /// use std::time::Duration;
    /// use xa11y::*;
    ///
    /// let app = App::by_name("Safari", Duration::from_secs(5))?;
    /// # Ok::<(), xa11y::Error>(())
    /// ```
    pub trait AppExt: Sized {
        /// Find an application by exact name using the global singleton
        /// provider, polling until it appears or `timeout` elapses. Pass
        /// `Duration::ZERO` for a single attempt with no waiting. See
        /// [`App::by_name_with`] for retry semantics.
        fn by_name(name: &str, timeout: Duration) -> Result<Self>;
        /// Find an application by process ID using the global singleton
        /// provider, polling until it appears or `timeout` elapses.
        ///
        /// This is the supported way to wait for a freshly launched process
        /// to surface in the accessibility tree — the poll covers the window
        /// between process spawn and the platform bridge registering the
        /// app, so callers don't need a hand-rolled loop over
        /// [`list`](Self::list). See [`App::by_pid_with`] for the full
        /// contract and [`by_name`](Self::by_name) for retry semantics.
        fn by_pid(pid: u32, timeout: Duration) -> Result<Self>;
        /// Resolve the application that currently holds the system foreground,
        /// using the global singleton provider, polling until one exists or
        /// `timeout` elapses. Pass `Duration::ZERO` for a single attempt with
        /// no waiting. Unlike [`find`](Self::find) with a `|d| d.states.focused`
        /// predicate, this queries the platform foreground mechanism directly,
        /// so on Windows it returns the exact foreground window even when the
        /// process owns several top-level windows. See [`App::foreground_with`]
        /// for the full contract and [`by_name`](Self::by_name) for retry
        /// semantics.
        fn foreground(timeout: Duration) -> Result<Self>;
        /// List all running applications using the global singleton provider.
        fn list() -> Result<Vec<Self>>;
        /// Find an application matching `predicate` using the global
        /// singleton provider, polling until one appears or `timeout`
        /// elapses. `predicate` runs against each running app's
        /// [`ElementData`] on every poll. See [`App::find_with`] for
        /// match / timeout semantics.
        fn find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> bool;
        /// Like [`find`](Self::find), but with a fallible predicate:
        /// `Ok(false)` keeps polling while `Err(_)` aborts and propagates.
        /// See [`App::try_find_with`].
        fn try_find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> Result<bool>;
    }

    impl AppExt for App {
        fn by_name(name: &str, timeout: Duration) -> Result<Self> {
            App::by_name_with(provider()?, name, timeout)
        }

        fn by_pid(pid: u32, timeout: Duration) -> Result<Self> {
            App::by_pid_with(provider()?, pid, timeout)
        }

        fn foreground(timeout: Duration) -> Result<Self> {
            App::foreground_with(provider()?, timeout)
        }

        fn list() -> Result<Vec<Self>> {
            App::list_with(provider()?)
        }

        fn find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> bool,
        {
            App::find_with(provider()?, timeout, predicate)
        }

        fn try_find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> Result<bool>,
        {
            App::try_find_with(provider()?, timeout, predicate)
        }
    }
}

// ── ShellSurfaceExt extension trait ─────────────────────────────────────────

mod shell_ext {
    use std::time::Duration;

    use super::{provider, Result, ShellSurface, ShellSurfaceKind};

    /// Extension trait that adds singleton-based constructors to
    /// [`ShellSurface`].
    ///
    /// Imported automatically via `use xa11y::*`.
    ///
    /// # Example
    /// ```no_run
    /// use std::time::Duration;
    /// use xa11y::*;
    ///
    /// let taskbar = ShellSurface::by_kind(ShellSurfaceKind::Taskbar, Duration::ZERO)?;
    /// taskbar.locator("button[name='Show Hidden Icons']").press()?;
    /// # Ok::<(), xa11y::Error>(())
    /// ```
    pub trait ShellSurfaceExt: Sized {
        /// List the OS shell surfaces currently on screen using the global
        /// singleton provider. The listing is live and reads nothing into
        /// existence: transient surfaces appear only while open, and
        /// enumerating never opens or presses anything. See
        /// [`ShellSurface::list_with`].
        fn list() -> Result<Vec<Self>>;
        /// Wait for exactly one surface of `kind` using the global singleton
        /// provider, polling until it appears or `timeout` elapses. Pass
        /// `Duration::ZERO` for a single attempt with no waiting. Errors with
        /// a candidate diagnosis both when none and when several match — see
        /// [`ShellSurface::by_kind_with`] for the full contract.
        fn by_kind(kind: ShellSurfaceKind, timeout: Duration) -> Result<Self>;
    }

    impl ShellSurfaceExt for ShellSurface {
        fn list() -> Result<Vec<Self>> {
            ShellSurface::list_with(provider()?)
        }

        fn by_kind(kind: ShellSurfaceKind, timeout: Duration) -> Result<Self> {
            ShellSurface::by_kind_with(provider()?, kind, timeout)
        }
    }
}

// ── Annotated-screenshot unit tests ─────────────────────────────────────
//
// The whole resolution half is exercised here against `xa11y-core`'s shared
// mock Provider and a synthetic `Screenshot`: no display, no application, no
// capture permission. What is left for the integration tests is that a real
// backend's pixels and a real tree agree.

#[cfg(test)]
mod annotated_tests {
    use super::*;

    /// The mock tree's app root, plus the provider that vends it.
    fn mock_app() -> (Arc<dyn Provider>, ElementData) {
        let provider: Arc<dyn Provider> = xa11y_core::mock::build_provider();
        let apps = provider.list_apps().expect("the mock must list its app");
        let root = apps.into_iter().next().expect("the mock has one app");
        (provider, root)
    }

    fn locator(selector: &str) -> Locator {
        let (provider, root) = mock_app();
        Locator::new(provider, Some(root), selector)
    }

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn resolved(group: usize, index: usize, bounds: Option<Rect>) -> Resolved {
        Resolved {
            group,
            index,
            selector: format!("button:nth({index})"),
            role: "button".to_string(),
            name: Some("Back".to_string()),
            bounds,
            color: screenshot::ANNOTATION_PALETTE[0],
        }
    }

    /// A fully transparent capture, so `annotate` has somewhere real to draw.
    fn blank(width: u32, height: u32) -> Screenshot {
        blank_scaled(width, height, 1.0)
    }

    /// [`blank`] at a chosen physical-to-logical ratio, for the cases where
    /// the scale is what is under test.
    fn blank_scaled(width: u32, height: u32, scale: f32) -> Screenshot {
        Screenshot::new(
            width,
            height,
            vec![0; (width as usize) * (height as usize) * 4],
            scale,
        )
    }

    // ── A two-application desktop ────────────────────────────────────────
    //
    // The shared mock has exactly one application, which is why every
    // round-trip test above passes while a rootless group silently mislabels:
    // with one application, "numbered across the whole match set" and
    // "numbered within each application" are the same numbering. They stop
    // being the same at two.

    /// One node of [`TwoAppProvider`]'s tree: its data, and its children.
    struct TwoAppNode {
        data: ElementData,
        children: Vec<usize>,
    }

    /// Two applications, the first with one button and the second with three.
    ///
    /// Deliberately not in `xa11y_core::mock`: this fixture exists to make the
    /// *disagreement* between two numbering schemes observable, and it is only
    /// this module's tests that care.
    struct TwoAppProvider {
        nodes: Vec<TwoAppNode>,
        apps: Vec<usize>,
    }

    impl TwoAppProvider {
        fn build() -> Arc<dyn Provider> {
            let mut nodes: Vec<TwoAppNode> = Vec::new();
            let mut apps = Vec::new();

            // Buttons sit at distinct, non-overlapping rectangles so a legend
            // entry can be told apart by bounds as well as by name.
            for (app_i, (app_name, buttons)) in [
                ("App One", vec!["one-btn1"]),
                ("App Two", vec!["two-btn1", "two-btn2", "two-btn3"]),
            ]
            .into_iter()
            .enumerate()
            {
                let app_index = nodes.len();
                apps.push(app_index);
                let mut app = ElementData::for_role(Role::Application);
                app.name = Some(app_name.to_string());
                app.pid = Some(app_i as u32 + 1);
                app.handle = app_index as u64 + 1;
                nodes.push(TwoAppNode {
                    data: app,
                    children: Vec::new(),
                });

                for (b, button_name) in buttons.into_iter().enumerate() {
                    let index = nodes.len();
                    let mut button = ElementData::for_role(Role::Button);
                    button.name = Some(button_name.to_string());
                    button.pid = Some(app_i as u32 + 1);
                    button.handle = index as u64 + 1;
                    button.bounds = Some(rect(10 + 40 * b as i32, 10 + 60 * app_i as i32, 30, 20));
                    nodes.push(TwoAppNode {
                        data: button,
                        children: Vec::new(),
                    });
                    nodes[app_index].children.push(index);
                }
            }

            Arc::new(Self { nodes, apps })
        }

        fn index_of(&self, element: &ElementData) -> Option<usize> {
            self.nodes
                .iter()
                .position(|n| n.data.handle == element.handle)
        }
    }

    impl Provider for TwoAppProvider {
        fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
            let indices: Vec<usize> = match element {
                None => self.apps.clone(),
                Some(el) => match self.index_of(el) {
                    Some(i) => self.nodes[i].children.clone(),
                    None => Vec::new(),
                },
            };
            Ok(indices
                .into_iter()
                .map(|i| self.nodes[i].data.clone())
                .collect())
        }

        fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
            let Some(target) = self.index_of(element) else {
                return Ok(None);
            };
            Ok(self
                .nodes
                .iter()
                .find(|n| n.children.contains(&target))
                .map(|n| n.data.clone()))
        }

        fn list_apps(&self) -> Result<Vec<ElementData>> {
            Ok(self
                .apps
                .iter()
                .map(|&i| self.nodes[i].data.clone())
                .collect())
        }

        fn focused_app(&self) -> Result<ElementData> {
            Ok(self.nodes[self.apps[0]].data.clone())
        }

        fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
            Ok(Vec::new())
        }

        // This fixture is a tree reader, not an action target: every mutating
        // call is refused rather than quietly succeeding, so a test that
        // reached one would fail loudly instead of asserting on a no-op.
        fn press(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn focus(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn blur(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn toggle(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn select(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn expand(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn collapse(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn show_menu(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn increment(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn decrement(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
            Err(unsupported_in_fixture())
        }
        fn subscribe(&self, _: &ElementData) -> Result<xa11y_core::Subscription> {
            Err(unsupported_in_fixture())
        }
    }

    fn unsupported_in_fixture() -> Error {
        Error::Unsupported {
            feature: "TwoAppProvider is a read-only tree fixture".to_string(),
        }
    }

    fn names(elements: &[Element]) -> Vec<String> {
        elements
            .iter()
            .map(|e| e.name.clone().unwrap_or_default())
            .collect()
    }

    // ── entry_numbering: the selector round-trip ─────────────────────────

    #[test]
    fn entry_numbering_appends_a_one_based_nth_per_match() {
        let numbering = entry_numbering("button", 3).expect("a plain role selector must number");
        assert_eq!(
            numbering,
            vec![
                (1, "button:nth(1)".to_string()),
                (2, "button:nth(2)".to_string()),
                (3, "button:nth(3)".to_string()),
            ]
        );
    }

    #[test]
    fn entry_numbering_of_no_matches_is_empty() {
        assert!(entry_numbering("button", 0)
            .expect("zero matches is not an error")
            .is_empty());
    }

    #[test]
    fn entry_numbering_keeps_attribute_filters_and_combinators() {
        let numbering = entry_numbering(r#"toolbar > button[name="Back"]"#, 1).expect("numbering");
        assert_eq!(
            numbering[0].1, r#"toolbar > button[name="Back"]:nth(1)"#,
            "the nth must attach to the last segment, not replace the selector"
        );
    }

    #[test]
    fn entry_numbering_leaves_a_trailing_nth_alone() {
        // `button:nth(2)` has already collapsed the match set to one element,
        // and appending a second `:nth` would not even parse. The `:nth`
        // argument is 2, not 1 — the index in the tag has to be the one that
        // resolves the element.
        let numbering = entry_numbering("button:nth(2)", 1).expect("numbering");
        assert_eq!(numbering, vec![(2, "button:nth(2)".to_string())]);
    }

    #[test]
    fn entry_numbering_appends_after_an_nth_on_an_earlier_segment() {
        // Only the *last* segment's nth collapses the result set, so this one
        // still needs its own.
        let numbering = entry_numbering("toolbar:nth(1) > button", 2).expect("numbering");
        assert_eq!(numbering[1].1, "toolbar:nth(1) > button:nth(2)");
        assert_eq!(numbering[1].0, 2);
    }

    #[test]
    fn entry_numbering_refuses_a_comma_separated_group() {
        let err = entry_numbering("button, text_field", 2)
            .expect_err("an alternation cannot produce a round-tripping :nth");
        match err {
            Error::InvalidSelector { selector, message } => {
                assert_eq!(selector, "button, text_field");
                assert!(
                    message.contains("one annotation group per clause"),
                    "the message must name the fix, got: {message}"
                );
            }
            other => panic!("expected InvalidSelector, got {other:?}"),
        }
    }

    #[test]
    fn entry_numbering_rejects_an_unparsable_selector() {
        let err = entry_numbering("button[", 1).expect_err("a broken selector must be reported");
        assert!(matches!(err, Error::InvalidSelector { .. }), "got {err:?}");
    }

    /// The round trip the whole feature exists for, against the real matcher:
    /// the selector a legend entry carries must resolve to the element that
    /// entry describes.
    #[test]
    fn every_entry_selector_resolves_to_its_own_element() {
        let (provider, root) = mock_app();
        let all = Locator::new(Arc::clone(&provider), Some(root.clone()), "button")
            .elements()
            .expect("the mock has buttons");
        assert!(all.len() >= 2, "fixture must have several buttons");

        let numbering = entry_numbering("button", all.len()).expect("numbering");
        for (i, (index, selector)) in numbering.iter().enumerate() {
            assert_eq!(*index, i + 1);
            let resolved = Locator::new(Arc::clone(&provider), Some(root.clone()), selector)
                .element()
                .unwrap_or_else(|e| panic!("{selector} must resolve: {e}"));
            assert_eq!(
                resolved.name, all[i].name,
                "{selector} resolved to the wrong element"
            );
        }
    }

    // ── resolve_groups: numbering, colour, cap ───────────────────────────

    #[test]
    fn groups_and_indices_are_both_one_based() {
        let (out, truncated) = resolve_groups(&[locator("button"), locator("text_field")])
            .expect("the mock resolves both groups");
        assert_eq!(truncated, 0);

        let buttons: Vec<_> = out.iter().filter(|r| r.group == 1).collect();
        assert_eq!(buttons[0].index, 1);
        assert_eq!(buttons[0].selector, "button:nth(1)");
        assert_eq!(buttons[0].role, "button");
        assert_eq!(buttons[1].index, 2);
        assert_eq!(buttons[1].selector, "button:nth(2)");

        let fields: Vec<_> = out.iter().filter(|r| r.group == 2).collect();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].index, 1);
        assert_eq!(fields[0].selector, "text_field:nth(1)");
        assert_eq!(fields[0].name.as_deref(), Some("Search"));
    }

    #[test]
    fn each_group_takes_the_next_palette_colour_and_wraps_past_seven() {
        let groups: Vec<Locator> = (0..9).map(|_| locator("text_field")).collect();
        let (out, _) = resolve_groups(&groups).expect("resolve");
        assert_eq!(out.len(), 9, "one text_field per group");

        for (i, r) in out.iter().enumerate() {
            assert_eq!(r.group, i + 1);
            assert_eq!(
                r.color,
                screenshot::ANNOTATION_PALETTE[i % 7],
                "group {} colour",
                r.group
            );
        }
        // The wrap is the assertion that matters: group 8 reuses group 1's.
        assert_eq!(out[7].color, out[0].color);
        assert_eq!(out[8].color, out[1].color);
    }

    #[test]
    fn a_locator_with_its_own_nth_annotates_only_that_match() {
        let (out, truncated) = resolve_groups(&[locator("button").nth(2)]).expect("resolve");
        assert_eq!(truncated, 0);
        assert_eq!(out.len(), 1, "nth(2) selects one element, not all of them");
        assert_eq!(out[0].index, 2);
        assert_eq!(out[0].selector, "button:nth(2)");
        assert_eq!(out[0].name.as_deref(), Some("Forward"));
    }

    #[test]
    fn resolution_stops_at_the_cap_and_says_how_many_it_did_not_reach() {
        // Each group matches the mock's two buttons, so 60 groups ask for 120.
        let groups: Vec<Locator> = (0..60).map(|_| locator("button")).collect();
        let (out, truncated) = resolve_groups(&groups).expect("resolve");

        assert_eq!(out.len(), MAX_ANNOTATIONS);
        assert_eq!(truncated, 120 - MAX_ANNOTATIONS);
        assert_eq!(
            out.last().expect("a capped run still resolves some").group,
            50,
            "the cap must bite mid-run, not drop whole groups"
        );
    }

    #[test]
    fn a_group_whose_selector_matches_nothing_contributes_nothing() {
        let (out, truncated) =
            resolve_groups(&[locator("button"), locator("progress_bar")]).expect("resolve");
        assert_eq!(truncated, 0);
        assert!(
            out.iter().all(|r| r.group == 1),
            "an empty group is not an error and adds no entries"
        );
    }

    // ── Rootless groups: the multi-application regression ────────────────

    /// The defect this refusal exists for, demonstrated against the real
    /// matcher before the refusal is asserted.
    ///
    /// A rootless `Locator` runs its search once per application and
    /// concatenates, so `:nth(n)` counts *within* an application. A legend
    /// that numbered the concatenated list would hand out `button:nth(2)` for
    /// the second application's first button — which resolves to that
    /// application's second button. No error, a different control pressed.
    #[test]
    fn a_rootless_locators_nth_counts_per_application_not_across_the_match_set() {
        let provider = TwoAppProvider::build();
        let rootless = |selector: &str| Locator::new(Arc::clone(&provider), None, selector);

        let all = rootless("button").elements().expect("both apps resolve");
        assert_eq!(
            names(&all),
            ["one-btn1", "two-btn1", "two-btn2", "two-btn3"],
            "the match set is the concatenation, in application order"
        );

        // Entry 2 of a globally numbered legend would describe "two-btn1"...
        let second = rootless("button:nth(2)")
            .elements()
            .expect("nth(2) resolves");
        assert_eq!(
            names(&second),
            ["two-btn2"],
            "...but `button:nth(2)` names the wrong element"
        );

        // ...and entry 1's selector is not even unique.
        let first = rootless("button:nth(1)")
            .elements()
            .expect("nth(1) resolves");
        assert_eq!(
            names(&first),
            ["one-btn1", "two-btn1"],
            "one selector, two elements: `.element()` would silently take the first"
        );

        // Entry 4's selector would find nothing at all.
        assert!(
            rootless("button:nth(4)")
                .elements()
                .expect("nth(4) resolves")
                .is_empty(),
            "no application has a fourth button"
        );
    }

    #[test]
    fn a_rootless_group_is_refused_and_the_error_names_the_fix() {
        let provider = TwoAppProvider::build();
        let err = resolve_groups(&[Locator::new(provider, None, "button")])
            .expect_err("a rootless group cannot produce a round-tripping selector");
        match err {
            Error::InvalidSelector { selector, message } => {
                assert_eq!(selector, "button");
                assert!(
                    message.contains("scoped to an application"),
                    "the message must say what is wrong, got: {message}"
                );
                assert!(
                    message.contains("app.locator"),
                    "the message must name the fix, got: {message}"
                );
            }
            other => panic!("expected InvalidSelector, got {other:?}"),
        }
    }

    /// A rootless group whose selector already ends in `:nth(k)` is the same
    /// defect wearing a worse disguise: every match would get the *identical*
    /// tag and the identical selector, so two boxes in the picture would carry
    /// one label. Refused on the same terms.
    #[test]
    fn a_rootless_group_with_its_own_nth_is_refused_too() {
        let provider = TwoAppProvider::build();
        let err = resolve_groups(&[Locator::new(provider, None, "button:nth(1)")])
            .expect_err("a rootless group is refused whatever its selector");
        assert!(matches!(err, Error::InvalidSelector { .. }), "got {err:?}");
    }

    /// Scoping is the fix the error names, so it has to actually work on the
    /// desktop that broke the rootless case.
    #[test]
    fn scoped_groups_round_trip_on_a_two_application_desktop() {
        let provider = TwoAppProvider::build();
        let apps = provider.list_apps().expect("two apps");
        assert_eq!(apps.len(), 2);

        for app in apps {
            let scoped = Locator::new(Arc::clone(&provider), Some(app.clone()), "button");
            let expected = names(&scoped.elements().expect("buttons"));
            let (resolved, truncated) = resolve_groups(&[scoped]).expect("resolve");
            assert_eq!(truncated, 0);
            assert_eq!(resolved.len(), expected.len());

            for (r, want) in resolved.iter().zip(&expected) {
                assert_eq!(r.name.as_deref(), Some(want.as_str()));
                let back = Locator::new(Arc::clone(&provider), Some(app.clone()), &r.selector)
                    .elements()
                    .unwrap_or_else(|e| panic!("{} must resolve: {e}", r.selector));
                let back = names(&back);
                assert_eq!(
                    back.len(),
                    1,
                    "{} must name exactly one element, got {back:?}",
                    r.selector
                );
                assert_eq!(
                    &back[0], want,
                    "{} resolved to the wrong element",
                    r.selector
                );
            }
        }
    }

    #[test]
    fn a_locator_whose_nth_is_past_the_end_describes_nothing() {
        // Deliberate silence, documented under "Groups that describe nothing"
        // on `screenshot_annotated`: `omitted` describes elements that were
        // found, and this group found none.
        let (out, truncated) = resolve_groups(&[locator("button").nth(9)]).expect("resolve");
        assert!(out.is_empty());
        assert_eq!(truncated, 0);
    }

    #[test]
    fn an_invalid_group_selector_fails_the_whole_call() {
        let err = resolve_groups(&[locator("button"), locator("a, b")])
            .expect_err("an alternation must be refused before any capture");
        assert!(matches!(err, Error::InvalidSelector { .. }), "got {err:?}");
    }

    // ── plan_annotations: NoBounds / ZeroArea ────────────────────────────

    #[test]
    fn an_element_with_bounds_becomes_a_box_and_a_legend_entry() {
        let (annotations, legend, omitted) =
            plan_annotations(&[resolved(2, 7, Some(rect(10, 20, 30, 40)))]);

        assert!(omitted.is_empty());
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].rect, rect(10, 20, 30, 40));
        assert_eq!(annotations[0].tag, "B7");
        assert_eq!(annotations[0].color, screenshot::ANNOTATION_PALETTE[0]);

        assert_eq!(legend.len(), 1);
        assert_eq!(legend[0].tag, "B7");
        assert_eq!(legend[0].group, 2);
        assert_eq!(legend[0].index, 7);
        assert_eq!(legend[0].bounds, rect(10, 20, 30, 40));
    }

    #[test]
    fn an_element_with_no_bounds_is_omitted_as_no_bounds() {
        let (annotations, legend, omitted) = plan_annotations(&[resolved(1, 1, None)]);
        assert!(annotations.is_empty());
        assert!(legend.is_empty());
        assert_eq!(omitted.len(), 1);
        assert_eq!(omitted[0].reason, OmissionReason::NoBounds);
        assert_eq!(omitted[0].selector, "button:nth(1)");
    }

    #[test]
    fn zero_width_or_zero_height_bounds_are_omitted_as_zero_area() {
        let (annotations, _, omitted) = plan_annotations(&[
            resolved(1, 1, Some(rect(5, 5, 0, 40))),
            resolved(1, 2, Some(rect(5, 5, 30, 0))),
            resolved(1, 3, Some(rect(5, 5, 0, 0))),
        ]);
        assert!(annotations.is_empty(), "nothing to outline is not a box");
        assert_eq!(omitted.len(), 3);
        assert!(omitted.iter().all(|o| o.reason == OmissionReason::ZeroArea));
    }

    #[test]
    fn the_mock_tree_classifies_both_kinds_at_once() {
        // check_box "Agree" has no bounds in the fixture; the buttons do.
        let (out, _) = resolve_groups(&[locator("button"), locator("check_box")]).expect("resolve");
        let (annotations, legend, omitted) = plan_annotations(&out);

        assert_eq!(annotations.len(), 2);
        assert_eq!(legend.len(), 2);
        assert_eq!(omitted.len(), 1);
        assert_eq!(omitted[0].role, "check_box");
        assert_eq!(omitted[0].reason, OmissionReason::NoBounds);
        assert_eq!(omitted[0].selector, "check_box:nth(1)");
    }

    // ── draw_and_reconcile: OutsideCapture ───────────────────────────────

    #[test]
    fn a_box_outside_the_capture_moves_from_the_legend_to_omitted() {
        // The fixture's buttons sit at x≈110..220, y≈60..90.
        let (out, _) = resolve_groups(&[locator("button")]).expect("resolve");
        let (annotations, mut legend, mut omitted) = plan_annotations(&out);
        assert_eq!(legend.len(), 2);

        // A capture covering only the first button's column.
        let shot = blank(60, 60);
        let drawn = draw_and_reconcile(
            &shot,
            Point::new(100, 50),
            &annotations,
            &mut legend,
            &mut omitted,
        )
        .expect("a well-formed capture must annotate");

        assert_eq!(drawn.width, 60);
        assert_eq!(legend.len(), 1, "only the visible button keeps its entry");
        assert_eq!(legend[0].name.as_deref(), Some("Back"));
        assert_eq!(omitted.len(), 1);
        assert_eq!(omitted[0].name.as_deref(), Some("Forward"));
        assert_eq!(omitted[0].reason, OmissionReason::OutsideCapture);
        assert_eq!(
            omitted[0].selector, "button:nth(2)",
            "an omitted element still carries a usable selector"
        );
    }

    #[test]
    fn everything_inside_the_capture_stays_in_the_legend() {
        let (out, _) = resolve_groups(&[locator("button")]).expect("resolve");
        let (annotations, mut legend, mut omitted) = plan_annotations(&out);

        let shot = blank(400, 300);
        let drawn = draw_and_reconcile(
            &shot,
            Point::new(100, 50),
            &annotations,
            &mut legend,
            &mut omitted,
        )
        .expect("annotate");

        assert_eq!(legend.len(), 2);
        assert!(omitted.is_empty());
        assert_ne!(drawn.pixels, shot.pixels, "boxes must have been drawn");
    }

    #[test]
    fn a_box_that_rounds_away_at_a_sub_unit_scale_is_zero_area_not_outside_capture() {
        // 1x1 logical at scale 0.25 rounds to 0x0 physical, so it can never
        // overlap the image and comes back in core's one `skipped` list — the
        // same list an off-screen box comes back in. It is not off-screen; it
        // has no size anywhere, which is what `ZeroArea` means.
        let planned = [resolved(1, 1, Some(rect(2, 2, 1, 1)))];
        let (annotations, mut legend, mut omitted) = plan_annotations(&planned);
        assert_eq!(annotations.len(), 1, "1x1 logical is drawable on its face");

        let shot = blank_scaled(40, 40, 0.25);
        draw_and_reconcile(
            &shot,
            Point::new(0, 0),
            &annotations,
            &mut legend,
            &mut omitted,
        )
        .expect("annotate");

        assert!(legend.is_empty());
        assert_eq!(omitted.len(), 1);
        assert_eq!(omitted[0].reason, OmissionReason::ZeroArea);
    }

    #[test]
    fn reconciling_several_skips_keeps_the_remaining_entries_aligned() {
        // Three annotations, the outer two off-image: removing by descending
        // index is what keeps the middle one's entry intact.
        let planned = [
            resolved(1, 1, Some(rect(-500, -500, 10, 10))),
            resolved(1, 2, Some(rect(2, 2, 10, 10))),
            resolved(1, 3, Some(rect(900, 900, 10, 10))),
        ];
        let (annotations, mut legend, mut omitted) = plan_annotations(&planned);

        let shot = blank(40, 40);
        draw_and_reconcile(
            &shot,
            Point::new(0, 0),
            &annotations,
            &mut legend,
            &mut omitted,
        )
        .expect("annotate");

        assert_eq!(legend.len(), 1);
        assert_eq!(legend[0].index, 2);
        assert_eq!(omitted.len(), 2);
        assert!(omitted
            .iter()
            .all(|o| o.reason == OmissionReason::OutsideCapture));
    }

    // ── Serialization ────────────────────────────────────────────────────
    //
    // `OmissionReason`'s own spelling and serde round-trip moved to
    // `xa11y_core::screenshot::legend` with the type. What is still this
    // crate's to prove is that a legend entry built by `plan_annotations`
    // carries every field the legend promises.

    #[test]
    fn a_serialized_legend_entry_carries_every_field_the_legend_promises() {
        let (annotations, legend, _) = plan_annotations(&[resolved(1, 1, Some(rect(1, 2, 3, 4)))]);
        assert_eq!(annotations.len(), 1);

        let json: serde_json::Value =
            serde_json::to_value(&legend[0]).expect("a legend entry must serialize");
        for key in [
            "tag", "group", "index", "selector", "role", "name", "bounds", "color",
        ] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        assert_eq!(json["bounds"]["width"], 3);
        assert_eq!(json["color"], serde_json::json!([230, 159, 0]));
    }
}
