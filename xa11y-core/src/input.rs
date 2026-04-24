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

/// A 2D point in screen coordinates.
///
/// Coordinates are integer screen pixels in the platform's native coordinate
/// space. On macOS this is points (the OS handles HiDPI scaling for input
/// events); on Windows and Linux this is physical pixels. Origin is top-left
/// of the primary display; negative values are valid on multi-monitor setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Where on an element to land a pointer event.
///
/// All anchors are computed against the element's [`Rect`] *at the time of the
/// input call*, not at element-fetch time — but only if the caller supplies a
/// fresh element. `InputSim` will not re-traverse the a11y tree on its own.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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
    let (x, y, w, h) = (rect.x, rect.y, rect.width as i32, rect.height as i32);
    match anchor {
        Anchor::Center => Point::new(x + w / 2, y + h / 2),
        Anchor::TopLeft => Point::new(x, y),
        Anchor::TopRight => Point::new(x + w, y),
        Anchor::BottomLeft => Point::new(x, y + h),
        Anchor::BottomRight => Point::new(x + w, y + h),
        Anchor::Offset { dx, dy } => Point::new(x + dx, y + dy),
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
}

// ── Pointer ─────────────────────────────────────────────────────────

/// A mouse button.
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
#[derive(Debug, Clone)]
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

/// Options for [`Mouse::drag_with`].
#[derive(Debug, Clone)]
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
    /// Equivalent to the free function [`point_for`].
    pub fn point_for(&self, element: &Element, anchor: Anchor) -> Result<Point> {
        point_for(element, anchor)
    }
}

/// Pointer operations. Obtain via [`InputSim::mouse`].
pub struct Mouse<'a> {
    backend: &'a Arc<dyn InputProvider>,
}

impl Mouse<'_> {
    /// Left-click `target` once at its [`Anchor::Center`] (for elements) or
    /// at the literal point.
    pub fn click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = target.into_point()?;
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
            ClickTarget::Element(el) => point_for(el, opts.anchor)?,
        };
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend.pointer_click(pt, opts.button, opts.count)
        })
    }

    /// Convenience for a left double-click at `target`.
    pub fn double_click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = target.into_point()?;
        self.backend.pointer_click(pt, MouseButton::Left, 2)
    }

    /// Convenience for a right-click at `target`.
    pub fn right_click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = target.into_point()?;
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
        let pt = target.into_point()?;
        self.backend.pointer_move(pt)
    }

    /// Press the left button at `from`, move to `to`, release. Default
    /// duration: 150 ms. Use [`drag_with`](Self::drag_with) to customise.
    pub fn drag(&self, from: impl IntoPoint, to: impl IntoPoint) -> Result<()> {
        let from = from.into_point()?;
        let to = to.into_point()?;
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
        let from = from.into_point()?;
        let to = to.into_point()?;
        with_keys_held(self.backend.as_ref(), &opts.held, || {
            self.backend
                .pointer_drag(from, to, opts.button, opts.duration)
        })
    }

    /// Scroll at `target` by `delta` ticks.
    pub fn scroll(&self, target: impl IntoPoint, delta: ScrollDelta) -> Result<()> {
        let pt = target.into_point()?;
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
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.backend.type_text(text)
    }
}

/// Explicit target for [`Mouse::click_with`]: either a raw point or an
/// element to anchor against.
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
fn with_keys_held<F>(backend: &dyn InputProvider, keys: &[Key], body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    for k in keys {
        backend.key_down(k)?;
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
