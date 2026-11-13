//! Input simulation: synthesised pointer and keyboard events.
//!
//! Input simulation is **separate from** the accessibility action layer
//! ([`crate::Provider`], [`crate::Element`], [`crate::Locator`]). The two
//! mechanisms are fundamentally different:
//!
//! - **Accessibility actions** (`element.press()`, `locator.toggle()`) call
//!   the platform's a11y API directly. They work without the target window
//!   being focused or visible, are deterministic, and are the preferred way
//!   to drive a UI.
//! - **Input simulation** ([`InputSim`]) generates OS-level pointer/keyboard
//!   events at the system event layer. Use it only for interactions that have
//!   no a11y equivalent (drag-and-drop, scroll wheels, complex shortcut
//!   sequences). Most platforms require the target window to be foregrounded
//!   and require additional permissions (Accessibility + Input Monitoring on
//!   macOS, Wayland portal grants on Linux, etc.).
//!
//! There is **no implicit bridge** between the two: an accessibility-action
//! failure never falls back to input simulation, and [`InputSim`] never
//! inspects or auto-resolves the a11y tree on behalf of the caller. If you
//! want to click an element, you compute its bounds (via the a11y API) and
//! pass them in — see [`IntoPoint`] and [`point_for`].
//!
//! # Layout
//!
//! [`InputSim`] exposes two sub-handles:
//!
//! - [`InputSim::mouse`] → [`Mouse`] for pointer operations (`click`, `drag`,
//!   `scroll`, `down`/`up`).
//! - [`InputSim::keyboard`] → [`Keyboard`] for key operations (`press`,
//!   `chord`, `down`/`up`, `type_text`).
//!
//! Modifier keys (`Shift`, `Ctrl`, `Alt`, `Meta`) are regular variants of
//! [`Key`] — there is no separate `Modifier` type. `Key::Char(c)` represents
//! the unshifted physical key; use [`Key::Shift`] explicitly for uppercase
//! or shifted symbols (see [`Key`] for the rationale).

use std::sync::Arc;
use std::time::Duration;

use crate::element::{Element, Rect};
use crate::error::{Error, Result};

// ── Geometry ────────────────────────────────────────────────────────

/// A 2D point in desktop coordinates: physical pixels on Windows and logical
/// points on macOS and Linux.
///
/// This is the same coordinate space as [`crate::element::Rect`] in
/// `Element::bounds`, so anchor points computed from an element's bounds are
/// already in the right space. Origin is top-left of the primary display;
/// negative values are valid on multi-monitor setups.
///
/// Each [`InputProvider`] uses the same units as its element bounds. Windows
/// and macOS pass these coordinates to native input without DPI conversion;
/// Linux converts logical points when its display scale is known. A point
/// that lands on an element's centre is
/// `anchor_point(&element.bounds, Anchor::Center)`.
#[allow(
    clippy::exhaustive_structs,
    reason = "Closed domain: a 2D screen point is an x and a y. Literal \
              construction is the point of the type."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Convert this **logical** point to **physical** device pixels by
    /// multiplying by `scale` (the physical-to-logical ratio). Used by input
    /// backends at the OS boundary. A non-finite or non-positive `scale`
    /// collapses to `1.0` (identity) — see [`crate::element::Rect::to_physical`].
    #[must_use]
    pub fn to_physical(self, scale: f64) -> Point {
        let s = crate::element::sane_scale(scale);
        Point {
            x: (f64::from(self.x) * s).round() as i32,
            y: (f64::from(self.y) * s).round() as i32,
        }
    }

    /// Convert this **physical** point to **logical** coordinates by dividing
    /// by `scale`. Inverse of [`Point::to_physical`].
    #[must_use]
    pub fn to_logical(self, scale: f64) -> Point {
        self.to_physical(1.0 / crate::element::sane_scale(scale))
    }
}

/// Where on an element to land a pointer event.
///
/// All anchors are computed against the element's [`Rect`] *at the time of the
/// input call*, not at element-fetch time — but only if the caller supplies a
/// fresh element. `InputSim` will not re-traverse the a11y tree on its own.
///
/// `#[non_exhaustive]`: the named anchors are the four corners and the centre,
/// which is five of the standard nine-point grid — `TopCenter`, `BottomCenter`,
/// `LeftCenter`, and `RightCenter` are the obvious next additions.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Anchor {
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Pixel offset from the element's top-left corner.
    Offset {
        dx: i32,
        dy: i32,
    },
}

/// Compute a [`Point`] inside a [`Rect`] using the given [`Anchor`].
pub fn anchor_point(rect: &Rect, anchor: Anchor) -> Point {
    let x = rect.x;
    let y = rect.y;
    // Rectangles are half-open. Named right/bottom anchors target the last
    // point inside the bounds, never the exclusive far edge (which can be on
    // a different display and is not part of the element).
    // A large unsigned extent can still end at a representable signed point
    // when its origin is negative. Narrow only the final coordinate.
    let add_extent = |origin: i32, offset: u32| {
        (i64::from(origin) + i64::from(offset)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    };
    let right = add_extent(x, rect.width.saturating_sub(1));
    let bottom = add_extent(y, rect.height.saturating_sub(1));
    match anchor {
        Anchor::Center => Point::new(
            add_extent(x, rect.width / 2),
            add_extent(y, rect.height / 2),
        ),
        Anchor::TopLeft => Point::new(x, y),
        Anchor::TopRight => Point::new(right, y),
        Anchor::BottomLeft => Point::new(x, bottom),
        Anchor::BottomRight => Point::new(right, bottom),
        Anchor::Offset { dx, dy } => Point::new(x.saturating_add(dx), y.saturating_add(dy)),
    }
}

/// Resolve an [`Element`]'s current bounds to a screen [`Point`] using `anchor`.
///
/// Reads `element.bounds`. Returns [`Error::NoElementBounds`] if the element
/// has no bounds (e.g. an off-screen or virtual node).
///
/// **Staleness:** `Element` is a snapshot — its bounds were captured when the
/// caller fetched it from the provider. If the UI may have moved since then,
/// re-fetch the element first (e.g. via [`crate::Locator`]).
pub fn point_for(element: &Element, anchor: Anchor) -> Result<Point> {
    let bounds = element.bounds.ok_or(Error::NoElementBounds)?;
    Ok(anchor_point(&bounds, anchor))
}

// ── Targets ─────────────────────────────────────────────────────────

/// A target that can be lowered to a screen [`Point`].
///
/// Implemented for:
/// - [`Point`] and `(i32, i32)` — used as-is.
/// - `&`[`Element`] — uses the element's `bounds` field at the call site, with
///   [`Anchor::Center`]. For a non-default anchor, call [`point_for`] yourself
///   and pass the resulting `Point`.
///
/// Not implemented for [`crate::Locator`]: the caller must explicitly resolve
/// the locator to an `Element` first. This keeps the cost of provider traffic
/// (and the failure mode) visible at the call site.
pub trait IntoPoint {
    fn into_point(self) -> Result<Point>;

    /// Original element geometry, retained for pointer failure diagnosis.
    #[doc(hidden)]
    fn element_bounds(&self) -> Option<Rect> {
        None
    }

    /// Whether this target's point comes from an element's accessibility
    /// bounds rather than being a literal screen point. [`Mouse`] refuses
    /// such targets on backends whose bounds are not screen coordinates
    /// (see [`InputProvider::element_bounds_are_screen_coordinates`]).
    #[doc(hidden)]
    fn is_element_target(&self) -> bool {
        false
    }
}

impl IntoPoint for Point {
    fn into_point(self) -> Result<Point> {
        Ok(self)
    }
}

impl IntoPoint for (i32, i32) {
    fn into_point(self) -> Result<Point> {
        Ok(Point::new(self.0, self.1))
    }
}

impl IntoPoint for &Element {
    fn into_point(self) -> Result<Point> {
        point_for(self, Anchor::Center)
    }

    fn is_element_target(&self) -> bool {
        true
    }

    fn element_bounds(&self) -> Option<Rect> {
        self.bounds
    }
}

// ── Pointer ─────────────────────────────────────────────────────────

/// A mouse button.
///
/// Deliberately **exhaustive**, for the same reason as [`Key`]: every variant
/// has to be mapped to an X11 button number, an evdev code, a CoreGraphics
/// event type, and a `MOUSEEVENTF_*` flag pair. Adding a side button (X1/X2)
/// is real work in four backends — Windows alone expresses them through
/// `MOUSEEVENTF_XDOWN` plus a `mouseData` field rather than a distinct flag —
/// and the build should say so.
#[allow(
    clippy::exhaustive_enums,
    reason = "Capability enum: each variant must be mapped by every input \
              backend, so adding one has to break their builds. See Key."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Direction and magnitude of a scroll event, in platform "ticks" (typically
/// one notch of a physical scroll wheel). Positive `dy` scrolls toward the end
/// of the content, so the view moves down the page, as when a wheel is turned
/// toward the user; positive `dx` scrolls toward the right. This is the sign
/// of a DOM `WheelEvent`'s `deltaY` / `deltaX` and of a WebDriver wheel
/// action, and it is the same on every platform.
#[allow(
    clippy::exhaustive_structs,
    reason = "Closed domain: scrolling has two axes. A third would need a \
              third scroll axis to exist."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScrollDelta {
    pub dx: i32,
    pub dy: i32,
}

impl ScrollDelta {
    pub const fn new(dx: i32, dy: i32) -> Self {
        Self { dx, dy }
    }

    pub const fn vertical(dy: i32) -> Self {
        Self { dx: 0, dy }
    }

    pub const fn horizontal(dx: i32) -> Self {
        Self { dx, dy: 0 }
    }
}

// ── Keyboard ────────────────────────────────────────────────────────

/// A keyboard key.
///
/// Modifier keys (`Shift`, `Ctrl`, `Alt`, `Meta`) are regular variants of this
/// enum — they are not a separate type. This matches the physical reality that
/// modifiers are keys like any other, and the convention of Playwright,
/// Puppeteer, Selenium, pyautogui, `SendInput`, and `XTest`.
///
/// # `Key::Char` semantics
///
/// `Key::Char(c)` represents **the physical key labeled with the unshifted
/// character `c`**. It does **not** auto-synthesise `Shift`. To produce an
/// uppercase letter or shifted symbol, hold [`Key::Shift`] explicitly:
///
/// ```ignore
/// // Cmd+A (select all):
/// keyboard.chord(Key::Char('a'), &[Key::Meta]);
///
/// // Uppercase 'A':
/// keyboard.chord(Key::Char('a'), &[Key::Shift]);
/// ```
///
/// For this reason, `Key::Char` **rejects ASCII uppercase letters at the API
/// boundary** ([`Error::InvalidActionData`]). This prevents the common
/// footgun where `chord(Key::Char('K'), &[Key::Meta])` is read as "Cmd+K"
/// but would silently mean "Cmd+Shift+K" under auto-shift semantics.
///
/// To type arbitrary text (with IME support and correct case handling), use
/// [`Keyboard::type_text`] — `Key` is for single-key presses.
///
/// # `Meta`
///
/// `Meta` is the platform's "command" modifier: Cmd on macOS, Win on Windows,
/// Super on Linux. Backends are responsible for the platform mapping.
///
/// # Extensibility
///
/// Deliberately **exhaustive**. This enum names platform capabilities: every
/// variant has to be translated to a keysym, a virtual-key code, and an evdev
/// keycode by four separate input backends. Growth here *should* be breaking,
/// because the compile error in each backend is the only thing that forces a
/// per-platform decision. `#[non_exhaustive]` would turn "nobody mapped this
/// key" into a runtime `Error::Unsupported` from a library whose type system
/// advertises the key — a worse outcome than a build failure at the point the
/// variant is added.
#[allow(
    clippy::exhaustive_enums,
    reason = "Capability enum: each variant must be mapped by every input \
              backend, so adding one has to break their builds. See the \
              Extensibility note above."
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character (lowercase, no shifted symbols). Backends
    /// translate this to the matching physical key. See the type-level docs
    /// for the rationale on rejecting uppercase letters.
    Char(char),

    // Modifiers (held-key form — combine with other keys via `chord`).
    Shift,
    Ctrl,
    Alt,
    Meta,

    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    Delete,
    Insert,

    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,

    Home,
    End,
    PageUp,
    PageDown,

    /// A function key. `n` is 1-based (`F(1)` = F1).
    F(u8),
}

impl Key {
    /// Validate a key for use at the API boundary.
    ///
    /// Returns [`Error::InvalidActionData`] for `Key::Char` with an ASCII
    /// uppercase letter — callers must lowercase and hold [`Key::Shift`]
    /// explicitly. See the type-level docs.
    pub(crate) fn validate(&self) -> Result<()> {
        if let Key::Char(c) = self {
            if c.is_ascii_uppercase() {
                return Err(Error::invalid_action_data(format!(
                    "Key::Char('{c}') is uppercase; use Key::Char('{}') \
                         with Key::Shift held to produce an uppercase letter",
                    c.to_ascii_lowercase()
                )));
            }
        }
        Ok(())
    }
}

// ── Click / drag option structs ─────────────────────────────────────

/// Options for [`Mouse::click_with`].
///
/// `#[non_exhaustive]`: an options struct exists to grow. Start from
/// [`ClickOptions::new`] and chain only what you need —
/// `ClickOptions::new().button(MouseButton::Right).count(2)`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClickOptions {
    pub button: MouseButton,
    /// Number of consecutive clicks (1 = single, 2 = double, …).
    pub count: u32,
    /// Keys held (pressed but not released) for the duration of the click —
    /// typically modifier keys like [`Key::Shift`] or [`Key::Meta`].
    pub held: Vec<Key>,
    /// Anchor used when the target is an [`Element`]. Ignored for raw points.
    pub anchor: Anchor,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            count: 1,
            held: Vec::new(),
            anchor: Anchor::Center,
        }
    }
}

impl ClickOptions {
    /// Default options: left button, single click, nothing held, centre anchor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the button to click with.
    #[must_use]
    pub fn button(mut self, button: MouseButton) -> Self {
        self.button = button;
        self
    }

    /// Set the number of consecutive clicks (`2` is a double-click).
    #[must_use]
    pub fn count(mut self, count: u32) -> Self {
        self.count = count;
        self
    }

    /// Set the keys held for the duration of the click.
    #[must_use]
    pub fn held(mut self, held: impl IntoIterator<Item = Key>) -> Self {
        self.held = held.into_iter().collect();
        self
    }

    /// Set the anchor used when the target is an [`Element`].
    #[must_use]
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }
}

/// Options for [`Mouse::drag_with`].
///
/// `#[non_exhaustive]`: see [`ClickOptions`]. Start from
/// [`DragOptions::new`] and chain what you need.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DragOptions {
    pub button: MouseButton,
    /// Keys held for the duration of the drag.
    pub held: Vec<Key>,
    /// Time over which the pointer moves from start to end. Backends
    /// interpolate pointer movement across this duration.
    ///
    /// On macOS the drag also holds still for 200 ms after the button goes
    /// down and for 500 ms at the end point before it comes up, outside this
    /// duration. AppKit drag sources need the first pause to start a
    /// dragging session, and drop targets need the second to register the
    /// drop.
    pub duration: Duration,
}

impl Default for DragOptions {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            held: Vec::new(),
            duration: Duration::from_millis(150),
        }
    }
}

impl DragOptions {
    /// Default options: left button, nothing held, 150ms.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the button held down for the drag.
    #[must_use]
    pub fn button(mut self, button: MouseButton) -> Self {
        self.button = button;
        self
    }

    /// Set the keys held for the duration of the drag.
    #[must_use]
    pub fn held(mut self, held: impl IntoIterator<Item = Key>) -> Self {
        self.held = held.into_iter().collect();
        self
    }

    /// Set the total time over which the drag is performed.
    #[must_use]
    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }
}

// ── Backend trait ───────────────────────────────────────────────────

/// Platform backend trait for synthesised user input.
///
/// Implementors generate OS-level pointer and keyboard events. Most methods
/// correspond to a single low-level operation; a few (marked "provided") are
/// synthesised by default but may be overridden when a platform has a
/// higher-fidelity primitive.
///
/// **This trait is intentionally separate from [`crate::Provider`].** A
/// backend that only knows how to read the accessibility tree should not
/// implement `InputProvider`, and vice versa. Crates may implement both for
/// the same platform but the two surfaces never call into each other.
///
/// # Errors
///
/// Implementations should return:
/// - [`Error::PermissionDenied`] when the OS denies the synthesis permission.
/// - [`Error::Unsupported`] when the operation has no platform implementation
///   (e.g. pointer warp on a session that disallows it). Do **not** silently
///   degrade — surface the missing capability per Tenet 1.
/// - [`Error::Platform`] for raw OS failures.
pub trait InputProvider: Send + Sync {
    // ── Capabilities (provided) ─────────────────────────────────────

    /// Whether an element's accessibility bounds are usable as screen
    /// coordinates for this backend's pointer events.
    ///
    /// `true` (the default) everywhere bounds and pointer share one global
    /// coordinate space. A backend returns `false` when they do not — on
    /// Wayland, clients do not know their global position, so AT-SPI bounds
    /// are not screen coordinates — and pointer actions targeted at an
    /// [`Element`] then fail with [`Error::Unsupported`] instead of clicking
    /// somewhere else. Raw-point pointer actions and keyboard input are
    /// unaffected.
    fn element_bounds_are_screen_coordinates(&self) -> bool {
        true
    }

    /// Preflight an explicit pointer target before any part of its gesture.
    ///
    /// Built-in providers reject targets outside their displays (or configured
    /// uinput extent). The default preserves the behavior of existing custom
    /// and virtual providers; those providers may override it to validate their
    /// own coordinate domain. Native query errors must propagate unchanged.
    fn validate_pointer_target(&self, _point: Point) -> Result<()> {
        Ok(())
    }

    /// Whether a mouse button is held through this provider. Such moves may
    /// belong to a captured drag, so display preflight must not interrupt them.
    /// This is tracked provider state, not a query of physical input devices.
    fn pointer_buttons_down(&self) -> bool {
        false
    }

    // ── Pointer (required) ──────────────────────────────────────────

    /// Move the pointer to `to` without pressing any buttons.
    fn pointer_move(&self, to: Point) -> Result<()>;

    /// Press `button` at the current pointer location (no release).
    fn pointer_down(&self, button: MouseButton) -> Result<()>;

    /// Release `button` at the current pointer location.
    fn pointer_up(&self, button: MouseButton) -> Result<()>;

    /// Click `button` at `at`, repeated `count` times. The backend is
    /// responsible for honouring the OS double-click interval when
    /// `count > 1` and for any platform-specific click-state bookkeeping
    /// (e.g. `kCGMouseEventClickState` on macOS).
    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()>;

    /// Scroll by `delta` ticks at `at`.
    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()>;

    // ── Keyboard (required) ─────────────────────────────────────────

    /// Press `key` (no release). Use [`key_up`](Self::key_up) to release.
    ///
    /// Modifiers are just keys: hold `Key::Shift` via `key_down(&Key::Shift)`.
    fn key_down(&self, key: &Key) -> Result<()>;

    /// Release `key`.
    fn key_up(&self, key: &Key) -> Result<()>;

    /// Type `text` as literal user input.
    ///
    /// Backends should prefer the OS's text-input path (with IME support)
    /// over synthesising individual key presses where possible.
    ///
    /// Line breaks are the exception, and part of the contract: `"\r\n"`,
    /// `"\r"` and `"\n"` each type one Return/Enter keypress, so in a
    /// single-line field this may activate the default button.
    fn type_text(&self, text: &str) -> Result<()>;

    // ── Pointer (provided, override for platform fidelity) ──────────

    /// Press `button` at `from`, interpolate to `to` over `duration`, release.
    ///
    /// The default synthesis posts `pointer_down` → a series of `pointer_move`
    /// calls (≈60 Hz cadence) → `pointer_up`. Backends **should override** to
    /// emit platform-specific drag events where they differ from move events
    /// — on macOS, drag-and-drop source apps filter for
    /// `kCGEventLeftMouseDragged`, which is distinct from
    /// `kCGEventMouseMoved`. On Windows and X11 the default synthesis is
    /// usually sufficient.
    fn pointer_drag(
        &self,
        from: Point,
        to: Point,
        button: MouseButton,
        duration: Duration,
    ) -> Result<()> {
        self.validate_pointer_target(from)?;
        self.validate_pointer_target(to)?;
        const STEP: Duration = Duration::from_millis(16);
        self.pointer_move(from)?;
        self.pointer_down(button)?;
        let steps = (duration.as_millis() / STEP.as_millis().max(1)).max(1) as i32;
        let moved = (|| {
            for i in 1..=steps {
                let t = f64::from(i) / f64::from(steps);
                // Widen before subtraction: distant desktop coordinates can
                // span the full signed range even though each endpoint fits.
                let x =
                    (f64::from(from.x) + (f64::from(to.x) - f64::from(from.x)) * t).round() as i32;
                let y =
                    (f64::from(from.y) + (f64::from(to.y) - f64::from(from.y)) * t).round() as i32;
                self.pointer_move(Point::new(x, y))?;
                if i < steps {
                    std::thread::sleep(STEP);
                }
            }
            Ok(())
        })();
        // Always release after a failed move; preserve the first failure if
        // release also fails, matching held-key and Windows drag cleanup.
        let released = self.pointer_up(button);
        moved.and(released)
    }
}

// ── Public façade ───────────────────────────────────────────────────

/// Validate against the complete native region set before bounding the error
/// snapshot. Backends supply points in the same space as their native regions.
#[doc(hidden)]
pub fn validate_pointer_regions(
    point: Point,
    checked_point: Point,
    coordinate_space: &str,
    displays: &[Rect],
) -> Result<()> {
    if displays.iter().any(|rect| {
        let x = i64::from(checked_point.x);
        let y = i64::from(checked_point.y);
        x >= i64::from(rect.x)
            && y >= i64::from(rect.y)
            && x < i64::from(rect.x) + i64::from(rect.width)
            && y < i64::from(rect.y) + i64::from(rect.height)
    }) {
        return Ok(());
    }
    Err(Error::invalid_action_data(format!(
        "pointer target ({}, {}) is not on any display",
        point.x, point.y
    ))
    .diagnose(
        crate::Diagnosis::new().pointer_target(crate::PointerTargetDiagnosis::new(
            point,
            checked_point,
            coordinate_space,
            displays,
        )),
    ))
}

/// Record where a backend's display regions came from on an off-display
/// refusal, when that is not obvious from the backend (a documented
/// fallback geometry source, or an explicit configuration override).
/// Errors without a pointer-target diagnosis pass through unchanged.
#[doc(hidden)]
pub fn note_display_source(error: Error, source: &str) -> Error {
    let Some(mut diagnosis) = error.diagnosis().cloned() else {
        return error;
    };
    let Some(target) = diagnosis.pointer_target.as_mut() else {
        return error;
    };
    target.display_source = Some(source.to_owned());
    error.diagnose(diagnosis)
}

/// Preserve an element's original geometry after a binding lowers it to a
/// point. This is pure error enrichment; native failures keep their identity.
#[doc(hidden)]
pub fn enrich_pointer_error(error: Error, bounds: Option<Rect>, anchor: Anchor) -> Error {
    let Some(mut diagnosis) = error.diagnosis().cloned() else {
        return error;
    };
    let Some(target) = diagnosis.pointer_target.as_mut() else {
        return error;
    };
    target.element_bounds = bounds;
    target.anchor = bounds.map(|_| anchor);
    error.diagnose(diagnosis)
}

fn after_pointer_prefix(error: Error, held: &[Key]) -> Error {
    // A native gesture can recheck geometry after the façade's preflight.
    // If modifiers were already posted, a late refusal cannot promise that
    // the whole action had no effect, even though cleanup releases them.
    if held.is_empty() {
        return error;
    }
    let Some(mut diagnosis) = error.diagnosis().cloned() else {
        return error;
    };
    if diagnosis.pointer_target.is_none() {
        return error;
    }
    diagnosis.delivery = Some(crate::Delivery::Unknown);
    error.diagnose(diagnosis)
}

fn enrich_drag_error(
    error: Error,
    from: Point,
    from_bounds: Option<Rect>,
    to: Point,
    to_bounds: Option<Rect>,
) -> Error {
    let requested = error
        .diagnosis()
        .and_then(|d| d.pointer_target.as_ref())
        .map(|d| d.point);
    if requested == Some(from) {
        enrich_pointer_error(error, from_bounds, Anchor::Center)
    } else if requested == Some(to) {
        enrich_pointer_error(error, to_bounds, Anchor::Center)
    } else {
        error
    }
}

#[cfg(test)]
mod pointer_validation_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct Checked {
        events: Mutex<Vec<String>>,
        held: AtomicBool,
        query_failure: AtomicBool,
        queries: AtomicUsize,
        fail_drag_move: bool,
        late_click_refusal: bool,
    }

    impl Checked {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
                held: AtomicBool::new(false),
                query_failure: AtomicBool::new(false),
                queries: AtomicUsize::new(0),
                fail_drag_move: false,
                late_click_refusal: false,
            })
        }
        fn record(&self, event: impl Into<String>) {
            self.events.lock().unwrap().push(event.into());
        }
    }

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    impl InputProvider for Checked {
        fn validate_pointer_target(&self, point: Point) -> Result<()> {
            self.queries.fetch_add(1, Ordering::Relaxed);
            if self.query_failure.load(Ordering::Relaxed) {
                return Err(Error::Platform {
                    code: 42,
                    message: "native display query failed".into(),
                });
            }
            validate_pointer_regions(point, point, "test pixels", &[rect(0, 0, 100, 100)])
        }
        fn pointer_buttons_down(&self) -> bool {
            self.held.load(Ordering::Relaxed)
        }
        fn pointer_move(&self, _: Point) -> Result<()> {
            if self.fail_drag_move && self.pointer_buttons_down() {
                self.record("move failed");
                return Err(Error::Platform {
                    code: 99,
                    message: "move failed".into(),
                });
            }
            self.record("move");
            Ok(())
        }
        fn pointer_down(&self, _: MouseButton) -> Result<()> {
            self.record("down");
            self.held.store(true, Ordering::Relaxed);
            Ok(())
        }
        fn pointer_up(&self, _: MouseButton) -> Result<()> {
            self.record("up");
            self.held.store(false, Ordering::Relaxed);
            Ok(())
        }
        fn pointer_click(&self, point: Point, _: MouseButton, _: u32) -> Result<()> {
            if self.late_click_refusal {
                return validate_pointer_regions(point, point, "test pixels", &[]);
            }
            self.record("click");
            Ok(())
        }
        fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
            self.record("scroll");
            Ok(())
        }
        fn key_down(&self, key: &Key) -> Result<()> {
            self.record(format!("key down {key:?}"));
            Ok(())
        }
        fn key_up(&self, key: &Key) -> Result<()> {
            self.record(format!("key up {key:?}"));
            Ok(())
        }
        fn type_text(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn element(bounds: Rect) -> Element {
        Element::new(
            crate::ElementData {
                bounds: Some(bounds),
                ..Default::default()
            },
            crate::mock::build_provider(),
        )
    }

    #[test]
    fn regions_preserve_negative_origins_gaps_and_half_open_edges() {
        let regions = [rect(-100, -100, 100, 100), rect(10, 0, 100, 100)];
        for point in [
            Point::new(-100, -100),
            Point::new(-1, -1),
            Point::new(109, 99),
        ] {
            validate_pointer_regions(point, point, "pixels", &regions).expect("on a display");
        }
        for point in [Point::new(0, 0), Point::new(110, 0), Point::new(-101, 0)] {
            let error = validate_pointer_regions(point, point, "pixels", &regions)
                .expect_err("off display");
            assert_eq!(error.delivery(), crate::Delivery::NotDelivered);
            assert_eq!(
                error
                    .diagnosis()
                    .unwrap()
                    .pointer_target
                    .as_ref()
                    .unwrap()
                    .point,
                point
            );
        }
    }

    #[test]
    fn off_display_target_is_invalid_action_data_not_a_selector_miss() {
        let point = Point::new(500, 7);
        let error = validate_pointer_regions(point, point, "pixels", &[rect(0, 0, 10, 10)])
            .expect_err("off display");
        assert!(
            matches!(error, Error::InvalidActionData { .. }),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("pointer target (500, 7) is not on any display"),
            "{message}"
        );
        assert!(message.contains("pointer target: Point"), "{message}");
        assert_eq!(error.delivery(), crate::Delivery::NotDelivered);
    }

    #[test]
    fn display_source_is_recorded_on_pointer_refusals_only() {
        let point = Point::new(500, 7);
        let error = note_display_source(
            validate_pointer_regions(point, point, "pixels", &[rect(0, 0, 10, 10)]).unwrap_err(),
            "root window",
        );
        let geometry = error.diagnosis().unwrap().pointer_target.as_ref().unwrap();
        assert_eq!(geometry.display_source.as_deref(), Some("root window"));
        assert!(error.to_string().contains("display source: root window"));

        let other = note_display_source(Error::invalid_action_data("bad"), "root window");
        assert!(other.diagnosis().is_none());
    }

    #[test]
    fn validation_checks_every_display_before_bounding_the_diagnosis() {
        let regions: Vec<_> = (0..40).map(|i| rect(i * 10, 0, 5, 5)).collect();
        validate_pointer_regions(Point::new(391, 1), Point::new(391, 1), "pixels", &regions)
            .unwrap();
        let error =
            validate_pointer_regions(Point::new(999, 1), Point::new(999, 1), "pixels", &regions)
                .unwrap_err();
        let geometry = error.diagnosis().unwrap().pointer_target.as_ref().unwrap();
        assert_eq!(geometry.displays.len(), 32);
        assert_eq!(geometry.omitted_displays, 8);
    }

    #[test]
    fn extreme_edges_do_not_overflow_and_empty_sets_refuse() {
        let point = Point::new(i32::MAX, i32::MIN);
        validate_pointer_regions(
            point,
            point,
            "pixels",
            &[rect(i32::MAX - 2, i32::MIN, 10, 5)],
        )
        .unwrap();
        let error = validate_pointer_regions(point, point, "pixels", &[]).unwrap_err();
        assert!(error
            .diagnosis()
            .unwrap()
            .pointer_target
            .as_ref()
            .unwrap()
            .displays
            .is_empty());
    }

    #[test]
    fn bad_element_anchor_refuses_before_pointer_or_modifier_events() {
        let backend = Checked::new();
        let sim = InputSim::new(backend.clone());
        let bounds = rect(90, 10, 5, 5);
        let error = sim
            .mouse()
            .click_with(
                ClickTarget::Element(&element(bounds)),
                ClickOptions::new()
                    .anchor(Anchor::Offset { dx: 20, dy: 2 })
                    .held(vec![Key::Ctrl]),
            )
            .unwrap_err();
        let geometry = error.diagnosis().unwrap().pointer_target.as_ref().unwrap();
        assert_eq!(geometry.point, Point::new(110, 12));
        assert_eq!(geometry.element_bounds, Some(bounds));
        assert_eq!(geometry.anchor, Some(Anchor::Offset { dx: 20, dy: 2 }));
        assert!(backend.events.lock().unwrap().is_empty());
        assert_eq!(error.delivery(), crate::Delivery::NotDelivered);
    }

    #[test]
    fn invalid_drag_end_keeps_end_geometry_and_posts_nothing() {
        let backend = Checked::new();
        let sim = InputSim::new(backend.clone());
        let from = element(rect(5, 5, 10, 10));
        let end_bounds = rect(200, 200, 10, 10);
        let to = element(end_bounds);
        let error = sim
            .mouse()
            .drag_with(&from, &to, DragOptions::new().held(vec![Key::Shift]))
            .unwrap_err();
        let geometry = error.diagnosis().unwrap().pointer_target.as_ref().unwrap();
        assert_eq!(geometry.element_bounds, Some(end_bounds));
        assert_eq!(geometry.point, Point::new(205, 205));
        assert_eq!(geometry.anchor, Some(Anchor::Center));
        assert!(backend.events.lock().unwrap().is_empty());
    }

    #[test]
    fn native_query_error_is_not_rewritten_as_a_coordinate_miss() {
        let backend = Checked::new();
        backend.query_failure.store(true, Ordering::Relaxed);
        let sim = InputSim::new(backend.clone());
        let error = sim.mouse().click((5, 5)).unwrap_err();
        assert!(
            matches!(error, Error::Platform { code: 42, ref message } if message == "native display query failed")
        );
        assert!(error.diagnosis().is_none());
        assert!(backend.events.lock().unwrap().is_empty());
    }

    #[test]
    fn zero_count_does_not_query_geometry_or_post_modifiers() {
        let backend = Checked::new();
        backend.query_failure.store(true, Ordering::Relaxed);
        let sim = InputSim::new(backend.clone());
        sim.mouse()
            .click_with(
                ClickTarget::Point(Point::new(i32::MAX, i32::MIN)),
                ClickOptions::new().count(0).held(vec![Key::Ctrl]),
            )
            .unwrap();
        assert_eq!(backend.queries.load(Ordering::Relaxed), 0);
        assert!(backend.events.lock().unwrap().is_empty());
    }

    #[test]
    fn captured_moves_and_releases_bypass_geometry_but_clicks_do_not() {
        let backend = Checked::new();
        let sim = InputSim::new(backend.clone());
        sim.mouse().down(MouseButton::Left).unwrap();
        sim.mouse().move_to((500, 500)).unwrap();
        sim.mouse().click((500, 500)).unwrap_err();
        backend.query_failure.store(true, Ordering::Relaxed);
        sim.mouse().up(MouseButton::Left).unwrap();
        assert_eq!(*backend.events.lock().unwrap(), ["down", "move", "up"]);
    }

    #[test]
    fn failed_drag_move_still_releases_button_and_modifiers() {
        let mut backend = Checked::new();
        Arc::get_mut(&mut backend).unwrap().fail_drag_move = true;
        let sim = InputSim::new(backend.clone());
        let error = sim
            .mouse()
            .drag_with(
                (5, 5),
                (10, 10),
                DragOptions::new().held(vec![Key::Ctrl, Key::Shift]),
            )
            .unwrap_err();
        assert!(matches!(error, Error::Platform { code: 99, .. }));
        assert_eq!(
            *backend.events.lock().unwrap(),
            [
                "key down Ctrl",
                "key down Shift",
                "move",
                "down",
                "move failed",
                "up",
                "key up Shift",
                "key up Ctrl"
            ]
        );
    }

    #[test]
    fn late_refusal_after_modifier_prefix_is_unknown_and_modifiers_release() {
        let mut backend = Checked::new();
        Arc::get_mut(&mut backend).unwrap().late_click_refusal = true;
        let sim = InputSim::new(backend.clone());
        let error = sim
            .mouse()
            .click_with(
                ClickTarget::Point(Point::new(5, 5)),
                ClickOptions::new().held(vec![Key::Ctrl]),
            )
            .unwrap_err();
        assert_eq!(error.delivery(), crate::Delivery::Unknown);
        assert_eq!(
            *backend.events.lock().unwrap(),
            ["key down Ctrl", "key up Ctrl"]
        );
    }

    #[test]
    fn every_literal_pointer_path_checks_before_dispatch() {
        let backend = Checked::new();
        let sim = InputSim::new(backend.clone());
        let bad = Point::new(100, 100);
        for error in [
            sim.mouse().click(bad).unwrap_err(),
            sim.mouse().double_click(bad).unwrap_err(),
            sim.mouse().right_click(bad).unwrap_err(),
            sim.mouse().move_to(bad).unwrap_err(),
            sim.mouse().scroll(bad, ScrollDelta::default()).unwrap_err(),
            sim.mouse().drag(bad, (10, 10)).unwrap_err(),
        ] {
            assert_eq!(error.delivery(), crate::Delivery::NotDelivered);
            assert_eq!(
                error
                    .diagnosis()
                    .unwrap()
                    .pointer_target
                    .as_ref()
                    .unwrap()
                    .point,
                bad
            );
        }
        assert!(backend.events.lock().unwrap().is_empty());
    }

    #[test]
    fn an_out_of_range_scroll_is_refused_before_any_event() {
        let backend = Checked::new();
        let sim = InputSim::new(backend.clone());
        let ok = Point::new(5, 5);
        for delta in [
            ScrollDelta::vertical(i32::MIN),
            ScrollDelta::vertical(10_001),
            ScrollDelta::horizontal(-10_001),
            ScrollDelta::new(0, i32::MAX),
        ] {
            let error = sim.mouse().scroll(ok, delta).unwrap_err();
            assert!(
                matches!(error, Error::InvalidActionData { .. }),
                "{delta:?}: {error:?}"
            );
            assert_eq!(error.delivery(), crate::Delivery::NotDelivered);
        }
        assert!(backend.events.lock().unwrap().is_empty());
        assert!(validate_scroll(ScrollDelta::new(-10_000, 10_000)).is_ok());
    }

    #[test]
    fn pointer_payload_serializes_as_plain_geometry() {
        let bounds = rect(100, 100, 10, 10);
        let point = Point::new(105, 105);
        let error =
            validate_pointer_regions(point, point, "physical pixels", &[rect(0, 0, 10, 10)])
                .unwrap_err();
        let error = enrich_pointer_error(error, Some(bounds), Anchor::Offset { dx: 5, dy: 5 });
        let payload =
            serde_json::to_value(error.diagnosis().unwrap().pointer_target.as_ref().unwrap())
                .unwrap();
        assert_eq!(payload["point"], serde_json::json!({"x":105,"y":105}));
        assert_eq!(
            payload["element_bounds"],
            serde_json::json!({"x":100,"y":100,"width":10,"height":10})
        );
        assert_eq!(
            payload["anchor"],
            serde_json::json!({"offset":{"dx":5,"dy":5}})
        );
        assert_eq!(payload["displays"][0]["width"], 10);
        assert!(error.to_string().contains("physical pixels"));
    }

    struct Legacy;
    // An existing external/virtual provider need not add either new hook.
    impl InputProvider for Legacy {
        fn pointer_move(&self, _: Point) -> Result<()> {
            Ok(())
        }
        fn pointer_down(&self, _: MouseButton) -> Result<()> {
            Ok(())
        }
        fn pointer_up(&self, _: MouseButton) -> Result<()> {
            Ok(())
        }
        fn pointer_click(&self, _: Point, _: MouseButton, _: u32) -> Result<()> {
            Ok(())
        }
        fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
            Ok(())
        }
        fn key_down(&self, _: &Key) -> Result<()> {
            Ok(())
        }
        fn key_up(&self, _: &Key) -> Result<()> {
            Ok(())
        }
        fn type_text(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn legacy_virtual_provider_keeps_its_own_coordinate_domain() {
        InputSim::new(Arc::new(Legacy))
            .mouse()
            .click((i32::MAX, i32::MIN))
            .unwrap();
    }
}

/// Synthesises OS-level pointer and keyboard events.
///
/// `InputSim` is a thin façade over an [`InputProvider`] backend. Methods are
/// organised by input device: [`InputSim::mouse`] returns a [`Mouse`] handle
/// with pointer operations, [`InputSim::keyboard`] returns a [`Keyboard`]
/// handle with key operations. This structure matches Playwright and
/// Puppeteer's `page.mouse.*` / `page.keyboard.*` layout and keeps the combo
/// verbs (`click`, `press`) unambiguous even though `Element::press` exists
/// at the a11y layer.
///
/// Use this only when the accessibility action layer cannot express the
/// interaction you need — see the [module docs](self) for the rationale.
///
/// `InputSim` is cheap to clone (it shares the backend via `Arc`).
///
/// Built-in backends check pointer targets against every active display before
/// sending input; Wayland uses its configured virtual input extent. This proves
/// screen membership, not visibility or hit testing. Both drag endpoints are
/// checked before the gesture. Captured moves while this provider holds a button,
/// and releases, remain possible. Custom providers retain their coordinate domain
/// unless they implement [`InputProvider::validate_pointer_target`].
///
/// # Example
///
/// ```ignore
/// # use xa11y_core::{input::*, Element};
/// # fn go(sim: InputSim, button: Element) -> xa11y_core::Result<()> {
/// sim.mouse().click(&button)?;
/// sim.keyboard().chord(Key::Char('a'), &[Key::Meta])?; // Cmd/Ctrl+A
/// sim.keyboard().type_text("hello")?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct InputSim {
    backend: Arc<dyn InputProvider>,
}

impl InputSim {
    /// Build an `InputSim` over an explicit input backend.
    ///
    /// Most callers should use the platform-detected singleton instead —
    /// `xa11y::input_sim()` in Rust, `input_sim()` / `inputSim()` in the
    /// bindings. This constructor exists for tests and for embedders that
    /// supply their own [`InputProvider`].
    pub fn new(backend: Arc<dyn InputProvider>) -> Self {
        Self { backend }
    }

    /// Get the backing provider for advanced or composite sequences.
    pub fn backend(&self) -> &Arc<dyn InputProvider> {
        &self.backend
    }

    /// Handle for pointer operations.
    pub fn mouse(&self) -> Mouse<'_> {
        Mouse {
            backend: &self.backend,
        }
    }

    /// Handle for keyboard operations.
    pub fn keyboard(&self) -> Keyboard<'_> {
        Keyboard {
            backend: &self.backend,
        }
    }

    /// Resolve an element's current bounds to a screen point using `anchor`.
    ///
    /// Like the free function [`point_for`], but refuses with
    /// [`Error::Unsupported`] when this backend's element bounds are not
    /// screen coordinates (see
    /// [`InputProvider::element_bounds_are_screen_coordinates`]).
    pub fn point_for(&self, element: &Element, anchor: Anchor) -> Result<Point> {
        self.mouse().point_for_bounds(element.bounds, anchor)
    }
}

/// The refusal for an element-targeted pointer action on a backend whose
/// accessibility bounds are not screen coordinates.
fn element_target_unsupported() -> Error {
    Error::Unsupported {
        feature: "pointer action targeted at an element: on this session (Wayland) \
                  applications do not know their global position, so accessibility \
                  bounds are not screen coordinates. Use a semantic action (press, \
                  focus, ...) or pass a raw screen point"
            .to_string(),
    }
}

/// Pointer operations. Obtain via [`InputSim::mouse`].
pub struct Mouse<'a> {
    backend: &'a Arc<dyn InputProvider>,
}

impl Mouse<'_> {
    fn validate_target(&self, point: Point, bounds: Option<Rect>, anchor: Anchor) -> Result<()> {
        self.backend
            .validate_pointer_target(point)
            .map_err(|error| enrich_pointer_error(error, bounds, anchor))
    }

    /// Turn an element's accessibility `bounds` into the screen point for a
    /// pointer action, refusing when the backend's bounds are not screen
    /// coordinates.
    ///
    /// The one place element targets are lowered to points: core's own
    /// `Element` targets and both language bindings (whose element types are
    /// not core `Element`s) route through it, so every surface refuses the
    /// same way. The capability is checked before the bounds, so a refused
    /// backend says why even for an element with no bounds.
    #[doc(hidden)]
    pub fn point_for_bounds(&self, bounds: Option<Rect>, anchor: Anchor) -> Result<Point> {
        if !self.backend.element_bounds_are_screen_coordinates() {
            return Err(element_target_unsupported());
        }
        let bounds = bounds.ok_or(Error::NoElementBounds)?;
        Ok(anchor_point(&bounds, anchor))
    }

    /// Lower an [`IntoPoint`] target, applying the element-target check.
    fn resolve(&self, target: impl IntoPoint) -> Result<Point> {
        if target.is_element_target() && !self.backend.element_bounds_are_screen_coordinates() {
            return Err(element_target_unsupported());
        }
        target.into_point()
    }

    /// Left-click `target` once at its [`Anchor::Center`] (for elements) or
    /// at the literal point.
    pub fn click(&self, target: impl IntoPoint) -> Result<()> {
        let bounds = target.element_bounds();
        let pt = self.resolve(target)?;
        self.validate_target(pt, bounds, Anchor::Center)?;
        self.backend
            .pointer_click(pt, MouseButton::Left, 1)
            .map_err(|error| enrich_pointer_error(error, bounds, Anchor::Center))
    }

    /// Click with explicit options (button, count, held keys, anchor).
    ///
    /// `opts.anchor` is used only when `target` is an [`Element`]; for raw
    /// points it is ignored.
    pub fn click_with(&self, target: ClickTarget<'_>, opts: ClickOptions) -> Result<()> {
        for k in &opts.held {
            k.validate()?;
        }
        let (pt, bounds) = match target {
            ClickTarget::Point(p) => (p, None),
            ClickTarget::Element(el) => (self.point_for_bounds(el.bounds, opts.anchor)?, el.bounds),
        };
        if opts.count == 0 {
            return Ok(());
        }
        self.validate_target(pt, bounds, opts.anchor)?;
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend.pointer_click(pt, opts.button, opts.count)
        })
        .map_err(|error| {
            enrich_pointer_error(after_pointer_prefix(error, &opts.held), bounds, opts.anchor)
        })
    }

    /// Convenience for a left double-click at `target`.
    pub fn double_click(&self, target: impl IntoPoint) -> Result<()> {
        let bounds = target.element_bounds();
        let pt = self.resolve(target)?;
        self.validate_target(pt, bounds, Anchor::Center)?;
        self.backend
            .pointer_click(pt, MouseButton::Left, 2)
            .map_err(|error| enrich_pointer_error(error, bounds, Anchor::Center))
    }

    /// Convenience for a right-click at `target`.
    pub fn right_click(&self, target: impl IntoPoint) -> Result<()> {
        let bounds = target.element_bounds();
        let pt = self.resolve(target)?;
        self.validate_target(pt, bounds, Anchor::Center)?;
        self.backend
            .pointer_click(pt, MouseButton::Right, 1)
            .map_err(|error| enrich_pointer_error(error, bounds, Anchor::Center))
    }

    /// Press `button` at the current pointer location (no release).
    pub fn down(&self, button: MouseButton) -> Result<()> {
        self.backend.pointer_down(button)
    }

    /// Release `button` at the current pointer location.
    pub fn up(&self, button: MouseButton) -> Result<()> {
        self.backend.pointer_up(button)
    }

    /// Move the pointer to `target` without pressing any buttons.
    pub fn move_to(&self, target: impl IntoPoint) -> Result<()> {
        let bounds = target.element_bounds();
        let pt = self.resolve(target)?;
        if !self.backend.pointer_buttons_down() {
            self.validate_target(pt, bounds, Anchor::Center)?;
        }
        self.backend
            .pointer_move(pt)
            .map_err(|error| enrich_pointer_error(error, bounds, Anchor::Center))
    }

    /// Press the left button at `from`, move to `to`, release. Default
    /// duration: 150 ms, plus macOS's hold pauses (see
    /// [`DragOptions::duration`]). Use [`drag_with`](Self::drag_with) to
    /// customise.
    pub fn drag(&self, from: impl IntoPoint, to: impl IntoPoint) -> Result<()> {
        let from_bounds = from.element_bounds();
        let to_bounds = to.element_bounds();
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        self.validate_target(from, from_bounds, Anchor::Center)?;
        self.validate_target(to, to_bounds, Anchor::Center)?;
        self.backend
            .pointer_drag(from, to, MouseButton::Left, Duration::from_millis(150))
            .map_err(|error| enrich_drag_error(error, from, from_bounds, to, to_bounds))
    }

    /// Drag with explicit options.
    pub fn drag_with(
        &self,
        from: impl IntoPoint,
        to: impl IntoPoint,
        opts: DragOptions,
    ) -> Result<()> {
        for k in &opts.held {
            k.validate()?;
        }
        let from_bounds = from.element_bounds();
        let to_bounds = to.element_bounds();
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        self.validate_target(from, from_bounds, Anchor::Center)?;
        self.validate_target(to, to_bounds, Anchor::Center)?;
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend
                .pointer_drag(from, to, opts.button, opts.duration)
        })
        .map_err(|error| {
            enrich_drag_error(
                after_pointer_prefix(error, &opts.held),
                from,
                from_bounds,
                to,
                to_bounds,
            )
        })
    }

    /// Scroll at `target` by `delta` ticks, at most
    /// 10,000 on each axis.
    pub fn scroll(&self, target: impl IntoPoint, delta: ScrollDelta) -> Result<()> {
        validate_scroll(delta)?;
        let bounds = target.element_bounds();
        let pt = self.resolve(target)?;
        self.validate_target(pt, bounds, Anchor::Center)?;
        self.backend
            .pointer_scroll(pt, delta)
            .map_err(|error| enrich_pointer_error(error, bounds, Anchor::Center))
    }
}

/// Most ticks one `scroll` sends on an axis. Each tick is an OS event on some
/// backends (an XTest button click on X11), and the backends scale a tick
/// into the platform's unit (`WHEEL_DELTA`, 120 per notch, on Windows), so an
/// unbounded count is both an event flood and an integer overflow.
const MAX_SCROLL_TICKS: i32 = 10_000;

fn validate_scroll(delta: ScrollDelta) -> Result<()> {
    for (axis, ticks) in [("dx", delta.dx), ("dy", delta.dy)] {
        if ticks.unsigned_abs() > MAX_SCROLL_TICKS.unsigned_abs() {
            return Err(Error::invalid_action_data(format!(
                "scroll {axis} of {ticks} ticks is out of range: at most \
                 {MAX_SCROLL_TICKS} ticks either way per call"
            )));
        }
    }
    Ok(())
}

/// Keyboard operations. Obtain via [`InputSim::keyboard`].
pub struct Keyboard<'a> {
    backend: &'a Arc<dyn InputProvider>,
}

impl Keyboard<'_> {
    /// Tap `key` (press + release) with no other keys held.
    pub fn press(&self, key: Key) -> Result<()> {
        key.validate()?;
        self.backend.key_down(&key)?;
        self.backend.key_up(&key)
    }

    /// Tap `key` while `held` are held down.
    ///
    /// Modifiers are ordinary keys in this API — pass `Key::Shift`,
    /// `Key::Ctrl`, `Key::Alt`, or `Key::Meta` via `held`.
    ///
    /// ```ignore
    /// // Cmd/Ctrl+A:
    /// keyboard.chord(Key::Char('a'), &[Key::Meta])?;
    /// ```
    pub fn chord(&self, key: Key, held: &[Key]) -> Result<()> {
        key.validate()?;
        for k in held {
            k.validate()?;
        }
        with_keys_held(self.backend.as_ref(), held, || {
            self.backend.key_down(&key)?;
            self.backend.key_up(&key)
        })
    }

    /// Press `key` without releasing. Pair with [`up`](Self::up).
    pub fn down(&self, key: Key) -> Result<()> {
        key.validate()?;
        self.backend.key_down(&key)
    }

    /// Release a previously pressed key.
    pub fn up(&self, key: Key) -> Result<()> {
        key.validate()?;
        self.backend.key_up(&key)
    }

    /// Type literal text into whichever element currently has keyboard focus.
    ///
    /// `Keyboard` does not focus the target for you — call the appropriate
    /// accessibility action (e.g. `Element::focus` via the provider) first.
    ///
    /// Unlike [`press`](Self::press), this accepts any text (including
    /// uppercase and shifted symbols); backends handle the case/shift synthesis.
    ///
    /// On Linux (X11 and Wayland) every character is resolved against the
    /// active keyboard layout before the first key event is sent: if any
    /// character cannot be typed, the call returns [`Error::Unsupported`]
    /// naming it and its index, and nothing is typed.
    ///
    /// `"\r\n"`, `"\r"` and `"\n"` each type one Return/Enter keypress, so
    /// in a single-line field this may activate the default button.
    /// `"\t"` types the Tab key on every platform, so it can move focus to
    /// the next control rather than insert a tab character. Both are key
    /// taps, so a modifier held with [`down`](Self::down) applies to them on
    /// every platform: with Shift held, `"\t"` is Shift+Tab.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.backend.type_text(text)
    }
}

/// Explicit target for [`Mouse::click_with`]: either a raw point or an
/// element to anchor against.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a click lands at a screen position, which the \
              caller either states outright or derives from an element's \
              bounds. Locator is deliberately not a third variant — see the \
              IntoPoint docs on why resolving stays explicit."
)]
pub enum ClickTarget<'a> {
    Point(Point),
    Element(&'a Element),
}

impl From<Point> for ClickTarget<'_> {
    fn from(p: Point) -> Self {
        Self::Point(p)
    }
}

impl From<(i32, i32)> for ClickTarget<'_> {
    fn from(t: (i32, i32)) -> Self {
        Self::Point(Point::new(t.0, t.1))
    }
}

impl<'a> From<&'a Element> for ClickTarget<'a> {
    fn from(el: &'a Element) -> Self {
        Self::Element(el)
    }
}

/// Run `body` with each key in `keys` held down, releasing them all (in
/// reverse order) before returning. Errors during release are returned only
/// when `body` succeeded — a body failure takes precedence.
///
/// A `key_down` that fails partway (a backend can refuse one, as Windows
/// does for input UIPI would drop) releases the keys already pressed before
/// returning its error, so a refused chord never leaves a modifier held.
fn with_keys_held<F>(backend: &dyn InputProvider, keys: &[Key], body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    for (pressed, k) in keys.iter().enumerate() {
        if let Err(e) = backend.key_down(k) {
            for held in keys[..pressed].iter().rev() {
                // The press error is the one to report; a release that also
                // fails cannot be retried from here and must not mask it.
                let _ = backend.key_up(held);
            }
            return Err(e);
        }
    }
    let result = body();
    let mut release_err: Option<Error> = None;
    for k in keys.iter().rev() {
        if let Err(e) = backend.key_up(k) {
            // Keep the first release error so we can surface it if the body
            // succeeded; if the body already failed, the body error wins.
            if release_err.is_none() {
                release_err = Some(e);
            }
        }
    }
    match (result, release_err) {
        (Err(e), _) => Err(e),
        (Ok(()), Some(e)) => Err(e),
        (Ok(()), None) => Ok(()),
    }
}

#[cfg(test)]
mod keys_held_tests {
    use super::*;
    use std::sync::Mutex;

    /// Records key events and refuses `key_down` for one key.
    struct Refusing {
        refuse: Key,
        log: Mutex<Vec<String>>,
    }

    impl InputProvider for Refusing {
        fn pointer_move(&self, _: Point) -> Result<()> {
            Ok(())
        }
        fn pointer_down(&self, _: MouseButton) -> Result<()> {
            Ok(())
        }
        fn pointer_up(&self, _: MouseButton) -> Result<()> {
            Ok(())
        }
        fn pointer_click(&self, _: Point, _: MouseButton, _: u32) -> Result<()> {
            Ok(())
        }
        fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
            Ok(())
        }
        fn key_down(&self, key: &Key) -> Result<()> {
            if *key == self.refuse {
                return Err(Error::PermissionDenied {
                    instructions: "refused".into(),
                });
            }
            self.log.lock().unwrap().push(format!("down {key:?}"));
            Ok(())
        }
        fn key_up(&self, key: &Key) -> Result<()> {
            self.log.lock().unwrap().push(format!("up {key:?}"));
            Ok(())
        }
        fn type_text(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_refused_key_down_releases_the_keys_already_held() {
        let backend = Refusing {
            refuse: Key::Alt,
            log: Mutex::new(Vec::new()),
        };
        let err = with_keys_held(&backend, &[Key::Ctrl, Key::Shift, Key::Alt], || {
            panic!("the body must not run when a key could not be pressed")
        })
        .expect_err("the refusal propagates");
        assert!(matches!(err, Error::PermissionDenied { .. }), "{err:?}");
        assert_eq!(
            *backend.log.lock().unwrap(),
            ["down Ctrl", "down Shift", "up Shift", "up Ctrl"]
        );
    }
}

#[cfg(test)]
mod point_scale_tests {
    use super::{anchor_point, Anchor, Point};
    use crate::element::Rect;

    #[test]
    fn scale_one_is_identity() {
        let p = Point::new(100, -50);
        assert_eq!(p.to_physical(1.0), p);
        assert_eq!(p.to_logical(1.0), p);
    }

    #[test]
    fn logical_to_physical_scales() {
        assert_eq!(Point::new(100, 200).to_physical(1.5), Point::new(150, 300));
        assert_eq!(Point::new(-40, 10).to_physical(2.0), Point::new(-80, 20));
    }

    #[test]
    fn physical_to_logical_is_inverse() {
        assert_eq!(Point::new(150, 300).to_logical(1.5), Point::new(100, 200));
    }

    #[test]
    fn bad_scale_degrades_to_identity() {
        let p = Point::new(7, 9);
        assert_eq!(p.to_physical(0.0), p);
        assert_eq!(p.to_physical(f64::NAN), p);
    }

    #[test]
    fn anchor_center_of_logical_bounds_scales_to_physical() {
        // A button whose logical bounds are 200x40 at (100, 100). Its centre
        // is (200, 120) logically; on a 2x display the physical pixel is
        // (400, 240) — what an input backend would feed to the OS.
        let bounds = Rect {
            x: 100,
            y: 100,
            width: 200,
            height: 40,
        };
        let center = anchor_point(&bounds, Anchor::Center);
        assert_eq!(center, Point::new(200, 120));
        assert_eq!(center.to_physical(2.0), Point::new(400, 240));
    }

    #[test]
    fn corner_anchors_stay_inside_half_open_bounds() {
        let bounds = Rect {
            x: 10,
            y: 20,
            width: 3,
            height: 2,
        };
        assert_eq!(anchor_point(&bounds, Anchor::TopRight), Point::new(12, 20));
        assert_eq!(
            anchor_point(&bounds, Anchor::BottomLeft),
            Point::new(10, 21)
        );
        assert_eq!(
            anchor_point(&bounds, Anchor::BottomRight),
            Point::new(12, 21)
        );
    }

    #[test]
    fn wide_negative_origin_anchors_reach_representable_far_edges() {
        let bounds = Rect {
            x: i32::MIN,
            y: i32::MIN,
            width: u32::MAX,
            height: u32::MAX,
        };
        let last = i32::MAX - 1;
        assert_eq!(
            anchor_point(&bounds, Anchor::TopRight),
            Point::new(last, i32::MIN)
        );
        assert_eq!(
            anchor_point(&bounds, Anchor::BottomLeft),
            Point::new(i32::MIN, last)
        );
        assert_eq!(
            anchor_point(&bounds, Anchor::BottomRight),
            Point::new(last, last)
        );
        assert_eq!(anchor_point(&bounds, Anchor::Center), Point::new(-1, -1));
    }

    #[test]
    fn far_anchors_saturate_only_after_adding_the_origin() {
        let bounds = Rect {
            x: -100,
            y: -200,
            width: u32::MAX,
            height: u32::MAX,
        };
        assert_eq!(
            anchor_point(&bounds, Anchor::BottomRight),
            Point::new(i32::MAX, i32::MAX)
        );
    }

    #[test]
    fn empty_bounds_and_explicit_offsets_preserve_their_anchor_rules() {
        let bounds = Rect {
            x: -20,
            y: 30,
            width: 0,
            height: 0,
        };
        for anchor in [
            Anchor::Center,
            Anchor::TopLeft,
            Anchor::TopRight,
            Anchor::BottomLeft,
            Anchor::BottomRight,
        ] {
            assert_eq!(anchor_point(&bounds, anchor), Point::new(-20, 30));
        }
        assert_eq!(
            anchor_point(
                &bounds,
                Anchor::Offset {
                    dx: i32::MIN,
                    dy: i32::MAX
                }
            ),
            Point::new(i32::MIN, i32::MAX)
        );
    }
}

#[cfg(test)]
mod element_target_tests {
    use super::*;
    use crate::element::ElementData;
    use crate::role::Role;
    use std::sync::Mutex;

    /// Records every input event, so a refusal can be shown to send none.
    struct Recording {
        bounds_are_screen: bool,
        events: Mutex<Vec<String>>,
    }

    impl Recording {
        fn new(bounds_are_screen: bool) -> Arc<Self> {
            Arc::new(Self {
                bounds_are_screen,
                events: Mutex::new(Vec::new()),
            })
        }
        fn log(&self, e: String) -> Result<()> {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(e);
            Ok(())
        }
        fn events(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    impl InputProvider for Recording {
        fn element_bounds_are_screen_coordinates(&self) -> bool {
            self.bounds_are_screen
        }
        fn pointer_move(&self, to: Point) -> Result<()> {
            self.log(format!("move {to:?}"))
        }
        fn pointer_down(&self, button: MouseButton) -> Result<()> {
            self.log(format!("down {button:?}"))
        }
        fn pointer_up(&self, button: MouseButton) -> Result<()> {
            self.log(format!("up {button:?}"))
        }
        fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
            self.log(format!("click {at:?} {button:?} x{count}"))
        }
        fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
            self.log(format!("scroll {at:?} {delta:?}"))
        }
        fn key_down(&self, key: &Key) -> Result<()> {
            self.log(format!("keydown {key:?}"))
        }
        fn key_up(&self, key: &Key) -> Result<()> {
            self.log(format!("keyup {key:?}"))
        }
        fn type_text(&self, text: &str) -> Result<()> {
            self.log(format!("type {text}"))
        }
    }

    fn element() -> Element {
        let mut data = ElementData::for_role(Role::Button);
        data.bounds = Some(Rect {
            x: 10,
            y: 20,
            width: 100,
            height: 40,
        });
        Element::new(data, crate::mock::build_provider())
    }

    fn assert_refused(r: Result<()>) {
        match r {
            Err(Error::Unsupported { feature }) => {
                assert!(feature.contains("not screen coordinates"), "{feature}")
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn element_targets_are_refused_when_bounds_are_not_screen_coordinates() {
        let backend = Recording::new(false);
        let sim = InputSim::new(backend.clone());
        let el = element();
        let mouse = sim.mouse();
        assert_refused(mouse.click(&el));
        assert_refused(mouse.double_click(&el));
        assert_refused(mouse.right_click(&el));
        assert_refused(mouse.move_to(&el));
        assert_refused(mouse.scroll(&el, ScrollDelta::vertical(1)));
        assert_refused(mouse.drag(&el, (5, 5)));
        assert_refused(mouse.drag((5, 5), &el));
        assert_refused(mouse.click_with(ClickTarget::Element(&el), ClickOptions::new()));
        assert!(matches!(
            sim.point_for(&el, Anchor::Center),
            Err(Error::Unsupported { .. })
        ));
        // The binding entry point refuses before even looking at the bounds.
        assert!(matches!(
            mouse.point_for_bounds(None, Anchor::Center),
            Err(Error::Unsupported { .. })
        ));
        assert!(
            backend.events().is_empty(),
            "a refused target must send no event: {:?}",
            backend.events()
        );

        // Raw points and the keyboard are unaffected.
        mouse.click((1, 2)).expect("raw point");
        mouse.move_to(Point::new(3, 4)).expect("raw point");
        sim.keyboard().type_text("ok").expect("keyboard");
        assert_eq!(backend.events().len(), 3);
    }

    #[test]
    fn element_targets_resolve_when_bounds_are_screen_coordinates() {
        let backend = Recording::new(true);
        let sim = InputSim::new(backend.clone());
        let el = element();
        sim.mouse().click(&el).expect("element click");
        assert_eq!(
            sim.mouse()
                .point_for_bounds(el.bounds, Anchor::TopLeft)
                .expect("bounds"),
            Point::new(10, 20)
        );
        assert!(matches!(
            sim.mouse().point_for_bounds(None, Anchor::Center),
            Err(Error::NoElementBounds)
        ));
        assert_eq!(backend.events().len(), 1);
        assert!(
            backend.events()[0].starts_with("click"),
            "{:?}",
            backend.events()
        );
    }
}
