//! Linux input-simulation backend.
//!
//! Two implementations live behind a single [`LinuxInputProvider`] facade,
//! chosen at construction time based on the session environment:
//!
//! - **X11** — drives the XTest extension over `x11rb`.
//!   Zero setup required; matches what existing X11 users already have.
//! - **Wayland / uinput** (anything else) — opens the kernel's
//!   `/dev/uinput` device and registers a virtual evdev keyboard+pointer.
//!   Compositor-agnostic — works on every Linux desktop (GNOME, KDE,
//!   sway, Hyprland, Cosmic, weston) plus headless setups. Same mechanism
//!   `xdotool --using-uinput`, `ydotool`, `wtype`, Steam, and Wine use.
//!   Requires `input` group membership; surfaces an actionable
//!   `Error::PermissionDenied` if the user isn't in the group. See
//!   [`crate::wayland_input`].
//!
//! Routing recognizes that a Wayland desktop normally exports `DISPLAY` for
//! XWayland. Native Wayland wins in a mixed session; use
//! `XA11Y_LINUX_INPUT_BACKEND=x11|wayland` to override it. There is no
//! compile-time feature flag; both backends are always built on Linux.
//!
//! Key mapping on X11 goes keysym → keycode via `GetKeyboardMapping`, queried
//! once at connect time. `Key::Char` for printable ASCII uses the codepoint
//! as its keysym; [`Keyboard::type_text`](xa11y_core::input::Keyboard::type_text)
//! looks up each character in the same keymap table and holds Shift when it
//! appears in the shifted column. The Wayland backend does the same dance
//! against an xkb keymap built from system defaults.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, RwLock};

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ConnectionExt as _, GetKeyboardMappingReply, Keycode, Screen, Window, BUTTON_PRESS_EVENT,
    BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use xa11y_core::input::{
    validate_pointer_regions, InputProvider, Key, MouseButton, Point, ScrollDelta,
};
use xa11y_core::{Error, Rect, Result};

use crate::session::{select_backend, DesktopBackend};
use crate::wayland_input::WaylandInputBackend;

// X11 keysyms — lifted from /usr/include/X11/keysymdef.h. Only the keysyms
// named by [`Key`] plus the modifiers we need to hold for shifted text.
// Shared with the Wayland backend via `key_to_keysym`.
pub(crate) const XK_SHIFT_L: u32 = 0xffe1;
pub(crate) const XK_CONTROL_L: u32 = 0xffe3;
pub(crate) const XK_ALT_L: u32 = 0xffe9;
pub(crate) const XK_SUPER_L: u32 = 0xffeb;
const XK_RETURN: u32 = 0xff0d;
const XK_ESCAPE: u32 = 0xff1b;
const XK_BACKSPACE: u32 = 0xff08;
const XK_TAB: u32 = 0xff09;
const XK_DELETE: u32 = 0xffff;
const XK_INSERT: u32 = 0xff63;
const XK_UP: u32 = 0xff52;
const XK_DOWN: u32 = 0xff54;
const XK_LEFT: u32 = 0xff51;
const XK_RIGHT: u32 = 0xff53;
const XK_HOME: u32 = 0xff50;
const XK_END: u32 = 0xff57;
const XK_PAGE_UP: u32 = 0xff55;
const XK_PAGE_DOWN: u32 = 0xff56;
const XK_F1: u32 = 0xffbe;
// XK_F24 = 0xffd5; 1..=24 is contiguous starting at XK_F1.

/// Resolve a [`Key`] to its X11 keysym.
///
/// Both the X11 and Wayland backends translate `Key` → keysym via this
/// function. The Wayland path then looks the keysym up in an xkb keymap to
/// get an evdev keycode; the X11 path looks it up in `GetKeyboardMapping`.
pub(crate) fn key_to_keysym(key: &Key) -> Result<u32> {
    let keysym = match key {
        Key::Shift => XK_SHIFT_L,
        Key::Ctrl => XK_CONTROL_L,
        Key::Alt => XK_ALT_L,
        Key::Meta => XK_SUPER_L,
        Key::Enter => XK_RETURN,
        Key::Escape => XK_ESCAPE,
        Key::Backspace => XK_BACKSPACE,
        Key::Tab => XK_TAB,
        Key::Space => 0x0020,
        Key::Delete => XK_DELETE,
        Key::Insert => XK_INSERT,
        Key::ArrowUp => XK_UP,
        Key::ArrowDown => XK_DOWN,
        Key::ArrowLeft => XK_LEFT,
        Key::ArrowRight => XK_RIGHT,
        Key::Home => XK_HOME,
        Key::End => XK_END,
        Key::PageUp => XK_PAGE_UP,
        Key::PageDown => XK_PAGE_DOWN,
        Key::F(n) => {
            if *n < 1 || *n > 24 {
                return Err(Error::InvalidActionData {
                    message: format!("F{n} is out of range (1..=24)"),
                });
            }
            XK_F1 + (*n as u32 - 1)
        }
        Key::Char(c) => char_keysym(*c),
    };
    Ok(keysym)
}

/// Convert a character to its X11 keysym. ASCII 0x20..=0x7e maps directly;
/// everything above uses the Unicode plane (`0x01000000 | codepoint`).
pub(crate) fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) {
        cp
    } else {
        0x0100_0000 | cp
    }
}

/// One planned key press for `type_text`: the keycode to tap, and the Shift
/// keycode to hold around it when the character is on the shifted level.
pub(crate) type Keystroke<K> = (K, Option<K>);

/// Resolve every character of `text` to a keystroke **before** any event is
/// sent, so a character the layout cannot produce fails the whole call with
/// nothing typed (issue #494). Typing the prefix and then failing would leave
/// the caller with half-entered text and no way to know how much landed.
///
/// `lookup` maps a keysym to `(keycode, needs_shift)` in the backend's
/// keymap; `layout` names that keymap in the error. The error names the
/// character, its code point and its index (in characters) within `text`.
pub(crate) fn plan_keystrokes<K: Copy>(
    text: &str,
    layout: &str,
    lookup: impl Fn(u32) -> Option<(K, bool)>,
) -> Result<Vec<Keystroke<K>>> {
    let mut shift: Option<K> = None;
    let mut plan = Vec::with_capacity(text.len());
    let mut chars = text.chars().enumerate().peekable();
    while let Some((index, c)) = chars.next() {
        // A line break is the Enter key on every platform (issue #498):
        // "\r\n", "\r" and "\n" each type one Return press. The Unicode
        // keysyms for U+000A / U+000D are in no layout, so without this a
        // multi-line string could not be typed at all.
        let keysym = match c {
            '\r' => {
                if matches!(chars.peek(), Some((_, '\n'))) {
                    chars.next();
                }
                XK_RETURN
            }
            '\n' => XK_RETURN,
            _ => char_keysym(c),
        };
        let (keycode, needs_shift) = lookup(keysym).ok_or_else(|| Error::Unsupported {
            feature: format!(
                "typing character {c:?} (U+{:04X}, keysym 0x{keysym:04x}) at index {index} of \
                 the text: it has no keycode in the current {layout}; nothing was typed",
                c as u32
            ),
        })?;
        let held = if needs_shift {
            let shift_kc = match shift {
                Some(kc) => kc,
                None => {
                    let (kc, _) = lookup(XK_SHIFT_L).ok_or_else(|| Error::Unsupported {
                        feature: format!(
                            "typing character {c:?} at index {index} of the text needs Shift, \
                             but Shift_L has no keycode in the current {layout}; nothing was \
                             typed"
                        ),
                    })?;
                    shift = Some(kc);
                    kc
                }
            };
            Some(shift_kc)
        } else {
            None
        };
        plan.push((keycode, held));
    }
    Ok(plan)
}

/// Keymap snapshot built from `GetKeyboardMapping`. For each keysym we care
/// about we store the keycode plus whether it lives in the shifted column
/// (column 1) rather than the unshifted column (column 0).
struct Keymap {
    min_keycode: u8,
    syms_per_code: u8,
    syms: Vec<u32>,
}

impl Keymap {
    fn from_reply(reply: GetKeyboardMappingReply, min_keycode: u8) -> Self {
        Self {
            min_keycode,
            syms_per_code: reply.keysyms_per_keycode,
            syms: reply.keysyms,
        }
    }

    /// Locate a keysym in the map. Returns `(keycode, needs_shift)` — the
    /// caller must hold Shift iff `needs_shift` is true.
    fn lookup(&self, keysym: u32) -> Option<(Keycode, bool)> {
        let per = self.syms_per_code as usize;
        if per == 0 {
            return None;
        }
        for (code_index, chunk) in self.syms.chunks(per).enumerate() {
            // Column 0 is the unshifted level, column 1 the shifted level.
            // Some keys repeat the column-0 keysym into column 1 when there
            // is no shifted binding; that's fine — either lookup succeeds.
            if chunk.first() == Some(&keysym) {
                return Some((self.min_keycode + code_index as u8, false));
            }
            if per >= 2 && chunk.get(1) == Some(&keysym) {
                return Some((self.min_keycode + code_index as u8, true));
            }
        }
        None
    }
}

/// XTest-backed backend. Holds the X server connection and a keymap snapshot.
pub(crate) struct X11InputBackend {
    /// Connection is `!Sync`; guard all server traffic with the mutex.
    conn: Mutex<RustConnection>,
    root: Window,
    keymap: RwLock<Keymap>,
}

impl X11InputBackend {
    fn display_regions(&self) -> Result<Vec<Rect>> {
        self.with_conn(|conn| {
            let version = conn
                .randr_query_version(1, 5)
                .map_err(platform)?
                .reply()
                .map_err(platform)?;
            if version.major_version < 1
                || (version.major_version == 1 && version.minor_version < 5)
            {
                return Err(Error::Unsupported {
                    feature: "X11 pointer validation requires RandR 1.5 monitor geometry".into(),
                });
            }
            let reply = conn
                .randr_get_monitors(self.root, true)
                .map_err(platform)?
                .reply()
                .map_err(platform)?;
            Ok(reply
                .monitors
                .into_iter()
                .map(|monitor| Rect {
                    x: i32::from(monitor.x),
                    y: i32::from(monitor.y),
                    width: u32::from(monitor.width),
                    height: u32::from(monitor.height),
                })
                .collect())
        })
    }

    fn new() -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None).map_err(platform)?;
        let setup = conn.setup().clone();
        let screen: &Screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| platform_msg("X server reported no screens"))?;
        let root = screen.root;

        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        if max_keycode < min_keycode {
            return Err(platform_msg("X server reported an empty keycode range"));
        }
        let count = max_keycode - min_keycode + 1;
        let reply = conn
            .get_keyboard_mapping(min_keycode, count)
            .map_err(platform)?
            .reply()
            .map_err(platform)?;
        let keymap = Keymap::from_reply(reply, min_keycode);

        Ok(Self {
            conn: Mutex::new(conn),
            root,
            keymap: RwLock::new(keymap),
        })
    }

    fn with_conn<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&RustConnection) -> Result<R>,
    {
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    fn send(&self, type_: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.with_conn(|conn| {
            conn.xtest_fake_input(type_, detail, 0, self.root, x, y, 0)
                .map_err(platform)?
                .check()
                .map_err(platform)?;
            conn.flush().map_err(platform)?;
            Ok(())
        })
    }

    fn key_event(&self, keysym: u32, press: bool) -> Result<()> {
        let (keycode, _shift) = {
            let map = self.keymap.read().unwrap_or_else(|e| e.into_inner());
            map.lookup(keysym).ok_or_else(|| Error::Unsupported {
                feature: format!("keysym 0x{keysym:04x} has no keycode in the current X layout"),
            })?
        };
        let type_ = if press {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        self.send(type_, keycode, 0, 0)
    }

    fn button_event(&self, button: u8, press: bool) -> Result<()> {
        let type_ = if press {
            BUTTON_PRESS_EVENT
        } else {
            BUTTON_RELEASE_EVENT
        };
        self.send(type_, button, 0, 0)
    }

    fn pointer_move(&self, to: Point) -> Result<()> {
        // detail=0 on MOTION_NOTIFY means absolute coordinates relative to root.
        self.send(MOTION_NOTIFY_EVENT, 0, clamp_coord(to.x), clamp_coord(to.y))
    }

    fn pointer_down(&self, button: MouseButton) -> Result<()> {
        self.button_event(button_number(button), true)
    }

    fn pointer_up(&self, button: MouseButton) -> Result<()> {
        self.button_event(button_number(button), false)
    }

    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.pointer_move(at)?;
        let btn = button_number(button);
        for _ in 0..count {
            self.button_event(btn, true)?;
            self.button_event(btn, false)?;
        }
        Ok(())
    }

    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        self.pointer_move(at)?;
        // X11 scroll-wheel convention: button 4 = up, 5 = down, 6 = left, 7 = right.
        // The input-sim contract: positive dy scrolls content down (viewport
        // up), which is the "wheel rolled toward the user" direction — that's
        // button 5 on X11.
        for _ in 0..delta.dy.abs() {
            let btn = if delta.dy > 0 { 5 } else { 4 };
            self.button_event(btn, true)?;
            self.button_event(btn, false)?;
        }
        for _ in 0..delta.dx.abs() {
            let btn = if delta.dx > 0 { 7 } else { 6 };
            self.button_event(btn, true)?;
            self.button_event(btn, false)?;
        }
        Ok(())
    }

    fn key_down(&self, key: &Key) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        self.key_event(keysym, true)
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        self.key_event(keysym, false)
    }

    fn type_text(&self, text: &str) -> Result<()> {
        // Every character is resolved against the keymap before the first
        // event goes out; see `plan_keystrokes`. A character in the shifted
        // column is typed with Shift held. This is the X11 analogue of the
        // KEYEVENTF_UNICODE path on Windows — both aim to be robust against
        // the active keyboard layout.
        let plan = {
            let map = self.keymap.read().unwrap_or_else(|e| e.into_inner());
            plan_keystrokes(text, "X keyboard layout", |keysym| map.lookup(keysym))?
        };
        for (keycode, shift) in plan {
            if let Some(shift) = shift {
                self.send(KEY_PRESS_EVENT, shift, 0, 0)?;
            }
            self.send(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.send(KEY_RELEASE_EVENT, keycode, 0, 0)?;
            if let Some(shift) = shift {
                self.send(KEY_RELEASE_EVENT, shift, 0, 0)?;
            }
        }
        Ok(())
    }
}

/// Public input provider for Linux. Dispatches to the X11 or Wayland backend
/// chosen at construction.
pub struct LinuxInputProvider {
    backend: InputBackend,
    pointer_lock: Mutex<()>,
    held_buttons: AtomicU8,
}

enum InputBackend {
    X11(Box<X11InputBackend>),
    Wayland(Box<WaylandInputBackend>),
}

impl LinuxInputProvider {
    /// Choose a backend based on the session environment.
    ///
    /// - X11 session → X11 (XTest).
    /// - Wayland or mixed session → uinput.
    /// - `XA11Y_LINUX_INPUT_BACKEND=x11|wayland` overrides auto detection.
    ///   May return [`Error::PermissionDenied`] if the user isn't in the
    ///   `input` group, or [`Error::Unsupported`] if the kernel `uinput`
    ///   module isn't loaded.
    pub fn new() -> Result<Self> {
        match select_backend("XA11Y_LINUX_INPUT_BACKEND", "input simulation")? {
            DesktopBackend::X11 => {
                let x11 = X11InputBackend::new()?;
                Ok(Self {
                    backend: InputBackend::X11(Box::new(x11)),
                    pointer_lock: Mutex::new(()),
                    held_buttons: AtomicU8::new(0),
                })
            }
            DesktopBackend::Wayland => {
                let wl = WaylandInputBackend::new()?;
                Ok(Self {
                    backend: InputBackend::Wayland(Box::new(wl)),
                    pointer_lock: Mutex::new(()),
                    held_buttons: AtomicU8::new(0),
                })
            }
        }
    }
}

impl LinuxInputProvider {
    /// Convert a logical target point (the cross-platform contract, same space
    /// as `Element::bounds`) to the physical device pixels both backends
    /// expect: XTest warps to physical root coordinates, and the uinput
    /// virtual range is mapped against the physical screen size. No-op at
    /// scale 1.0. See `crate::scale`.
    fn to_physical(&self, p: Point) -> Point {
        p.to_physical(crate::scale::coordinate_scale())
    }

    fn move_pointer(&self, point: Point) -> Result<()> {
        let point = self.to_physical(point);
        match &self.backend {
            InputBackend::X11(x) => x.pointer_move(point),
            InputBackend::Wayland(w) => w.pointer_move(point),
        }
    }

    // The caller holds pointer_lock through both native delivery and the
    // state update, so concurrent down/up cannot reorder the capture mask.
    fn press_pointer(&self, button: MouseButton) -> Result<()> {
        match &self.backend {
            InputBackend::X11(x) => x.pointer_down(button),
            InputBackend::Wayland(w) => w.pointer_down(button),
        }?;
        self.held_buttons
            .fetch_or(button_mask(button), Ordering::SeqCst);
        Ok(())
    }

    fn release_pointer(&self, button: MouseButton) -> Result<()> {
        match &self.backend {
            InputBackend::X11(x) => x.pointer_up(button),
            InputBackend::Wayland(w) => w.pointer_up(button),
        }?;
        self.held_buttons
            .fetch_and(!button_mask(button), Ordering::SeqCst);
        Ok(())
    }
}

impl InputProvider for LinuxInputProvider {
    fn validate_pointer_target(&self, point: Point) -> Result<()> {
        let checked = self.to_physical(point);
        match &self.backend {
            InputBackend::X11(x) => {
                let displays = x.display_regions()?;
                validate_pointer_regions(point, checked, "x11 physical root pixels", &displays)?;
                // XTest's wire coordinates are signed 16-bit. Do not silently
                // clamp an on-display point to a different target.
                if i16::try_from(checked.x).is_err() || i16::try_from(checked.y).is_err() {
                    return Err(Error::Unsupported {
                        feature:
                            "XTest cannot address this display point with signed 16-bit coordinates"
                                .into(),
                    });
                }
                Ok(())
            }
            InputBackend::Wayland(w) => w.validate_pointer_target(point, checked),
        }
    }

    fn pointer_buttons_down(&self) -> bool {
        self.held_buttons.load(Ordering::SeqCst) != 0
    }

    /// X11 shares one global coordinate space between AT-SPI extents and
    /// XTest. On Wayland, clients do not know their global position, so the
    /// bounds AT-SPI reports are not screen coordinates and an
    /// element-targeted click would land somewhere else (issue #495).
    fn element_bounds_are_screen_coordinates(&self) -> bool {
        match self.backend {
            InputBackend::X11(_) => true,
            InputBackend::Wayland(_) => false,
        }
    }

    fn pointer_move(&self, to: Point) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.pointer_buttons_down() {
            self.validate_pointer_target(to)?;
        }
        self.move_pointer(to)
    }

    fn pointer_down(&self, button: MouseButton) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.press_pointer(button)
    }

    fn pointer_up(&self, button: MouseButton) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.release_pointer(button)
    }

    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.validate_pointer_target(at)?;
        let at = self.to_physical(at);
        let clicked = match &self.backend {
            InputBackend::X11(x) => x.pointer_click(at, button, count),
            InputBackend::Wayland(w) => w.pointer_click(at, button, count),
        };
        if clicked.is_err() {
            // A native failure can follow a partial button-down. Always try
            // release, retaining capture state if release itself fails.
            self.held_buttons
                .fetch_or(button_mask(button), Ordering::SeqCst);
            let released = self.release_pointer(button);
            clicked.and(released)
        } else {
            self.held_buttons
                .fetch_and(!button_mask(button), Ordering::SeqCst);
            clicked
        }
    }

    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.validate_pointer_target(at)?;
        let at = self.to_physical(at);
        match &self.backend {
            InputBackend::X11(x) => x.pointer_scroll(at, delta),
            InputBackend::Wayland(w) => w.pointer_scroll(at, delta),
        }
    }

    fn pointer_drag(
        &self,
        from: Point,
        to: Point,
        button: MouseButton,
        duration: std::time::Duration,
    ) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.validate_pointer_target(from)?;
        self.validate_pointer_target(to)?;
        // Keep the whole gesture under pointer_lock. A concurrent button-up
        // must not turn a later captured move into a new geometry preflight.
        self.move_pointer(from)?;
        self.press_pointer(button)?;
        const STEP: std::time::Duration = std::time::Duration::from_millis(16);
        let steps = (duration.as_millis() / STEP.as_millis()).max(1) as i32;
        let moved = (|| {
            for i in 1..=steps {
                let fraction = f64::from(i) / f64::from(steps);
                let x = (f64::from(from.x) + (f64::from(to.x) - f64::from(from.x)) * fraction)
                    .round() as i32;
                let y = (f64::from(from.y) + (f64::from(to.y) - f64::from(from.y)) * fraction)
                    .round() as i32;
                self.move_pointer(Point::new(x, y))?;
                if i < steps {
                    std::thread::sleep(STEP);
                }
            }
            Ok(())
        })();
        let released = self.release_pointer(button);
        moved.and(released)
    }

    fn key_down(&self, key: &Key) -> Result<()> {
        match &self.backend {
            InputBackend::X11(x) => x.key_down(key),
            InputBackend::Wayland(w) => w.key_down(key),
        }
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        match &self.backend {
            InputBackend::X11(x) => x.key_up(key),
            InputBackend::Wayland(w) => w.key_up(key),
        }
    }

    fn type_text(&self, text: &str) -> Result<()> {
        match &self.backend {
            InputBackend::X11(x) => x.type_text(text),
            InputBackend::Wayland(w) => w.type_text(text),
        }
    }
}

fn button_mask(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
    }
}

pub(crate) fn platform<E: std::fmt::Display>(e: E) -> Error {
    Error::Platform {
        code: -1,
        message: e.to_string(),
    }
}

pub(crate) fn platform_msg(msg: &str) -> Error {
    Error::Platform {
        code: -1,
        message: msg.to_string(),
    }
}

/// Clamp an `i32` screen coordinate into the `i16` range that XTest takes.
/// X11 coordinates are 16-bit; this matches how libX11 itself narrows them.
fn clamp_coord(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn button_number(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-column X keymap starting at keycode 8:
    /// `a/A`, `b/B`, `Shift_L`.
    fn keymap(with_shift: bool) -> Keymap {
        let shift = if with_shift { XK_SHIFT_L } else { 0 };
        Keymap {
            min_keycode: 8,
            syms_per_code: 2,
            syms: vec![
                'a' as u32, 'A' as u32, //
                'b' as u32, 'B' as u32, //
                shift, 0,
            ],
        }
    }

    #[test]
    fn plan_resolves_every_character_with_shift_where_needed() {
        let map = keymap(true);
        let plan = plan_keystrokes("aBa", "X keyboard layout", |k| map.lookup(k))
            .expect("all characters map");
        assert_eq!(plan, vec![(8, None), (9, Some(10)), (8, None)]);
    }

    #[test]
    fn unmapped_character_mid_string_plans_nothing() {
        let map = keymap(true);
        let err = plan_keystrokes("ab\u{65e5}ba", "X keyboard layout", |k| map.lookup(k))
            .expect_err("an unmapped character must fail the whole plan");
        let Error::Unsupported { feature } = err else {
            panic!("expected Unsupported, got {err:?}");
        };
        assert!(feature.contains('\u{65e5}'), "{feature}");
        assert!(feature.contains("U+65E5"), "{feature}");
        assert!(feature.contains("index 2"), "{feature}");
        assert!(feature.contains("nothing was typed"), "{feature}");
    }

    #[test]
    fn line_breaks_type_one_return_each() {
        let mut map = keymap(true);
        // Keycode 11: Return.
        map.syms.extend([XK_RETURN, 0]);
        let plan = |text: &str| {
            plan_keystrokes(text, "X keyboard layout", |k| map.lookup(k)).expect("plans")
        };
        let ret = (11, None);
        assert_eq!(plan("a\nb"), vec![(8, None), ret, (9, None)]);
        assert_eq!(plan("a\rb"), vec![(8, None), ret, (9, None)]);
        // CRLF is one line break, not two.
        assert_eq!(plan("a\r\nb"), vec![(8, None), ret, (9, None)]);
        // …but LF CR is two, as are consecutive breaks.
        assert_eq!(plan("\n\r"), vec![ret, ret]);
        assert_eq!(plan("\r\n\r\n"), vec![ret, ret]);
    }

    #[test]
    fn line_break_without_return_key_fails_before_typing() {
        let map = keymap(true);
        let err = plan_keystrokes("a\nb", "X keyboard layout", |k| map.lookup(k))
            .expect_err("no Return in this keymap");
        assert!(err.to_string().contains("index 1"), "{err}");
    }

    #[test]
    fn missing_shift_fails_before_any_keystroke() {
        let map = keymap(false);
        // `a` alone needs no Shift and plans fine…
        assert!(plan_keystrokes("a", "X keyboard layout", |k| map.lookup(k)).is_ok());
        // …but a later capital fails the plan, so the `a` is never sent.
        let err = plan_keystrokes("aB", "X keyboard layout", |k| map.lookup(k))
            .expect_err("Shift_L is missing");
        assert!(err.to_string().contains("Shift_L"), "{err}");
    }
}
