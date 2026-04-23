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
//! failure never falls back to input simulation, and `InputSim` never inspects
//! or auto-resolves the a11y tree on behalf of the caller. If you want to
//! click an element, you compute its bounds (via the a11y API) and pass them
//! in — see [`IntoPoint`] and [`InputSim::point_for`].

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

impl From<(i32, i32)> for Point {
    fn from((x, y): (i32, i32)) -> Self {
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
        Ok(self.into())
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

/// A keyboard modifier.
///
/// `Meta` is the platform's "command" modifier: Cmd on macOS, Win on Windows,
/// Super on Linux. Use this when you want a portable shortcut; backends are
/// responsible for the platform mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

/// A single key for a press / release.
///
/// For typing arbitrary text (with IME support and correct shift handling),
/// prefer [`InputSim::type_text`] over a sequence of `Key::Char` taps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character. Backends are responsible for any modifier
    /// synthesis required to produce it (e.g. shift for `'A'`).
    Char(char),

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

// ── Click / drag option structs ─────────────────────────────────────

/// Options for [`InputSim::click_with`].
#[derive(Debug, Clone)]
pub struct ClickOptions {
    pub button: MouseButton,
    /// Number of consecutive clicks (1 = single, 2 = double, …).
    pub count: u32,
    /// Modifiers held for the duration of the click.
    pub modifiers: Vec<Modifier>,
    /// Anchor used when the target is an [`Element`]. Ignored for raw points.
    pub anchor: Anchor,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            count: 1,
            modifiers: Vec::new(),
            anchor: Anchor::Center,
        }
    }
}

/// Options for [`InputSim::drag_with`].
#[derive(Debug, Clone)]
pub struct DragOptions {
    pub button: MouseButton,
    pub modifiers: Vec<Modifier>,
    /// Total time over which the drag is performed. Backends interpolate
    /// pointer movement across this duration.
    pub duration: Duration,
}

impl Default for DragOptions {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            modifiers: Vec::new(),
            duration: Duration::from_millis(150),
        }
    }
}

// ── Backend trait ───────────────────────────────────────────────────

/// Platform backend trait for synthesised user input.
///
/// Implementors generate OS-level pointer and keyboard events. Each method
/// corresponds to a single, low-level operation; the higher-level
/// [`InputSim`] composes these into ergonomic actions.
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
    // ── Pointer ─────────────────────────────────────────────────────

    /// Move the pointer to `to` without pressing any buttons.
    fn pointer_move(&self, to: Point) -> Result<()>;

    /// Press `button` at the current pointer location.
    fn pointer_press(&self, button: MouseButton) -> Result<()>;

    /// Release `button` at the current pointer location.
    fn pointer_release(&self, button: MouseButton) -> Result<()>;

    /// Click `button` at `at`, repeated `count` times. The backend is
    /// responsible for honouring the OS double-click interval when `count > 1`.
    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()>;

    /// Press `button` at `from`, move to `to` over `duration`, then release.
    fn pointer_drag(
        &self,
        from: Point,
        to: Point,
        button: MouseButton,
        duration: Duration,
    ) -> Result<()>;

    /// Scroll by `delta` ticks at `at`.
    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()>;

    // ── Keyboard ────────────────────────────────────────────────────

    /// Press `key` (no release).
    fn key_down(&self, key: &Key) -> Result<()>;

    /// Release `key`.
    fn key_up(&self, key: &Key) -> Result<()>;

    /// Press and release `key` while `modifiers` are held.
    fn key_tap(&self, key: &Key, modifiers: &[Modifier]) -> Result<()>;

    /// Type `text` as literal user input.
    ///
    /// Backends should prefer the OS's text-input path (with IME support)
    /// over synthesising individual key presses where possible.
    fn type_text(&self, text: &str) -> Result<()>;

    // ── Modifier holds ──────────────────────────────────────────────

    /// Press a modifier (without release). Pair with [`modifier_up`](Self::modifier_up).
    fn modifier_down(&self, modifier: Modifier) -> Result<()>;

    /// Release a previously pressed modifier.
    fn modifier_up(&self, modifier: Modifier) -> Result<()>;
}

// ── Public façade ───────────────────────────────────────────────────

/// Synthesises OS-level pointer and keyboard events.
///
/// `InputSim` is a thin façade over an [`InputProvider`] backend. Use it only
/// when the accessibility action layer cannot express the interaction you
/// need — see the [module docs](self) for the rationale.
///
/// `InputSim` is cheap to clone (it shares the backend via `Arc`).
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

    // ── Pointer ─────────────────────────────────────────────────────

    /// Left-click `target` once at its [`Anchor::Center`] (for elements) or
    /// at the literal point.
    pub fn click(&self, target: impl IntoPoint) -> Result<()> {
        let pt = target.into_point()?;
        self.backend.pointer_click(pt, MouseButton::Left, 1)
    }

    /// Click with explicit options (button, count, modifiers, anchor).
    ///
    /// `opts.anchor` is used only when `target` is an [`Element`]; for raw
    /// points it is ignored.
    pub fn click_with(&self, target: ClickTarget<'_>, opts: ClickOptions) -> Result<()> {
        let pt = match target {
            ClickTarget::Point(p) => p,
            ClickTarget::Element(el) => point_for(el, opts.anchor)?,
        };
        with_modifiers(self.backend.as_ref(), &opts.modifiers, || {
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
        let from = from.into_point()?;
        let to = to.into_point()?;
        with_modifiers(self.backend.as_ref(), &opts.modifiers, || {
            self.backend
                .pointer_drag(from, to, opts.button, opts.duration)
        })
    }

    /// Scroll at `target` by `delta` ticks.
    pub fn scroll(&self, target: impl IntoPoint, delta: ScrollDelta) -> Result<()> {
        let pt = target.into_point()?;
        self.backend.pointer_scroll(pt, delta)
    }

    // ── Keyboard ────────────────────────────────────────────────────

    /// Type literal text into whichever element currently has keyboard focus.
    ///
    /// `InputSim` does not focus the target for you — call the appropriate
    /// accessibility action (e.g. `Element::focus` via the provider) first.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.backend.type_text(text)
    }

    /// Tap `key` (press + release) with no modifiers.
    pub fn key(&self, key: Key) -> Result<()> {
        self.backend.key_tap(&key, &[])
    }

    /// Tap `key` while `modifiers` are held.
    ///
    /// Example: `chord(Key::Char('a'), &[Modifier::Meta])` for select-all.
    pub fn chord(&self, key: Key, modifiers: &[Modifier]) -> Result<()> {
        self.backend.key_tap(&key, modifiers)
    }

    /// Press `key` without releasing. Pair with [`key_up`](Self::key_up).
    pub fn key_down(&self, key: Key) -> Result<()> {
        self.backend.key_down(&key)
    }

    /// Release a previously pressed key.
    pub fn key_up(&self, key: Key) -> Result<()> {
        self.backend.key_up(&key)
    }

    // ── Geometry helpers ────────────────────────────────────────────

    /// Resolve an element's current bounds to a screen point using `anchor`.
    /// Equivalent to the free function [`point_for`].
    pub fn point_for(&self, element: &Element, anchor: Anchor) -> Result<Point> {
        point_for(element, anchor)
    }
}

/// Explicit target for [`InputSim::click_with`]: either a raw point or an
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
        Self::Point(t.into())
    }
}

impl<'a> From<&'a Element> for ClickTarget<'a> {
    fn from(el: &'a Element) -> Self {
        Self::Element(el)
    }
}

/// Run `body` with each modifier in `mods` held down, releasing them all
/// (in reverse order) before returning. Errors during release are returned
/// only when `body` succeeded — a body failure takes precedence.
fn with_modifiers<F>(backend: &dyn InputProvider, mods: &[Modifier], body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    for m in mods {
        backend.modifier_down(*m)?;
    }
    let result = body();
    let mut release_err: Option<Error> = None;
    for m in mods.iter().rev() {
        if let Err(e) = backend.modifier_up(*m) {
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
