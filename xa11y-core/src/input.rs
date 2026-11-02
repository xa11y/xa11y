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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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
    let w = rect.width.saturating_sub(1).min(i32::MAX as u32) as i32;
    let h = rect.height.saturating_sub(1).min(i32::MAX as u32) as i32;
    match anchor {
        Anchor::Center => Point::new(
            x.saturating_add((rect.width / 2).min(i32::MAX as u32) as i32),
            y.saturating_add((rect.height / 2).min(i32::MAX as u32) as i32),
        ),
        Anchor::TopLeft => Point::new(x, y),
        Anchor::TopRight => Point::new(x.saturating_add(w), y),
        Anchor::BottomLeft => Point::new(x, y.saturating_add(h)),
        Anchor::BottomRight => Point::new(x.saturating_add(w), y.saturating_add(h)),
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
/// one notch of a physical scroll wheel). Positive `dy` scrolls content
/// downward (i.e. moves the viewport up); positive `dx` scrolls right.
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
                return Err(Error::InvalidActionData {
                    message: format!(
                        "Key::Char('{c}') is uppercase; use Key::Char('{}') \
                         with Key::Shift held to produce an uppercase letter",
                        c.to_ascii_lowercase()
                    ),
                });
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
    /// Total time over which the drag is performed. Backends interpolate
    /// pointer movement across this duration.
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
        const STEP: Duration = Duration::from_millis(16);
        self.pointer_move(from)?;
        self.pointer_down(button)?;
        let steps = (duration.as_millis() / STEP.as_millis().max(1)).max(1) as i32;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let x = from.x + ((to.x - from.x) as f64 * t).round() as i32;
            let y = from.y + ((to.y - from.y) as f64 * t).round() as i32;
            self.pointer_move(Point::new(x, y))?;
            if i < steps {
                std::thread::sleep(STEP);
            }
        }
        self.pointer_up(button)
    }
}

// ── Public façade ───────────────────────────────────────────────────

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
        let pt = self.resolve(target)?;
        self.backend.pointer_click(pt, MouseButton::Left, 1)
    }

    /// Click with explicit options (button, count, held keys, anchor).
    ///
    /// `opts.anchor` is used only when `target` is an [`Element`]; for raw
    /// points it is ignored.
    pub fn click_with(&self, target: ClickTarget<'_>, opts: ClickOptions) -> Result<()> {
        for k in &opts.held {
            k.validate()?;
        }
        let pt = match target {
            ClickTarget::Point(p) => p,
            ClickTarget::Element(el) => self.point_for_bounds(el.bounds, opts.anchor)?,
        };
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend.pointer_click(pt, opts.button, opts.count)
        })
    }

    /// Convenience for a left double-click at `target`.
    pub fn double_click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = self.resolve(target)?;
        self.backend.pointer_click(pt, MouseButton::Left, 2)
    }

    /// Convenience for a right-click at `target`.
    pub fn right_click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = self.resolve(target)?;
        self.backend.pointer_click(pt, MouseButton::Right, 1)
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
        let pt = self.resolve(target)?;
        self.backend.pointer_move(pt)
    }

    /// Press the left button at `from`, move to `to`, release. Default
    /// duration: 150 ms. Use [`drag_with`](Self::drag_with) to customise.
    pub fn drag(&self, from: impl IntoPoint, to: impl IntoPoint) -> Result<()> {
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        self.backend
            .pointer_drag(from, to, MouseButton::Left, Duration::from_millis(150))
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
        let from = self.resolve(from)?;
        let to = self.resolve(to)?;
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend
                .pointer_drag(from, to, opts.button, opts.duration)
        })
    }

    /// Scroll at `target` by `delta` ticks.
    pub fn scroll(&self, target: impl IntoPoint, delta: ScrollDelta) -> Result<()> {
        let pt = self.resolve(target)?;
        self.backend.pointer_scroll(pt, delta)
    }
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
