//! Linux input-simulation backend using X11 + XTEST.
//!
//! Connects to the X server on construction and drives the XTest extension
//! (`fake_input`) for pointer motion, button events, scroll, and keyboard.
//!
//! **Wayland is not supported** at this layer. If `WAYLAND_DISPLAY` is set and
//! `DISPLAY` is not, [`LinuxInputProvider::new`] returns [`Error::Unsupported`]
//! — per Tenet 1 we refuse to fall back silently. A future backend based on
//! `libei` / `org.freedesktop.portal.RemoteDesktop` can be added behind a
//! feature flag.
//!
//! Key mapping goes keysym → keycode via `GetKeyboardMapping`, which we query
//! once at connect time and re-query when the server notifies us of a layout
//! change. `Key::Char` for printable ASCII uses the codepoint as its keysym;
//! [`Keyboard::type_text`](xa11y_core::input::Keyboard::type_text) looks up
//! each character in the same keymap table and holds Shift when it appears in
//! the shifted column.

use std::sync::{Mutex, RwLock};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, GetKeyboardMappingReply, Keycode, Screen, Window, BUTTON_PRESS_EVENT,
    BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{Error, Result};

// X11 keysyms — lifted from /usr/include/X11/keysymdef.h. Only the keysyms
// named by [`Key`] plus the modifiers we need to hold for shifted text.
const XK_SHIFT_L: u32 = 0xffe1;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_ALT_L: u32 = 0xffe9;
const XK_SUPER_L: u32 = 0xffeb;
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

/// XTest-backed [`InputProvider`].
pub struct LinuxInputProvider {
    /// Connection is `!Sync`; guard all server traffic with the mutex.
    conn: Mutex<RustConnection>,
    root: Window,
    keymap: RwLock<Keymap>,
}

impl LinuxInputProvider {
    /// Connect to `$DISPLAY` and initialise XTest state.
    ///
    /// Returns [`Error::Unsupported`] if the session is Wayland-only
    /// (`WAYLAND_DISPLAY` is set and `DISPLAY` is not). Returns
    /// [`Error::Platform`] for any X protocol / connection failure.
    pub fn new() -> Result<Self> {
        let display_set = std::env::var_os("DISPLAY").is_some();
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        if !display_set {
            let feature = if wayland {
                "input simulation on Wayland (X11 DISPLAY not set)".to_string()
            } else {
                "input simulation (no X11 DISPLAY)".to_string()
            };
            return Err(Error::Unsupported { feature });
        }

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

    /// Resolve a [`Key`] to its X11 keysym, returning
    /// [`Error::InvalidActionData`] for out-of-range `Key::F(n)`.
    fn keysym_for(&self, key: &Key) -> Result<u32> {
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
}

/// Convert a character to its X11 keysym. ASCII 0x20..=0x7e maps directly;
/// everything above uses the Unicode plane (`0x01000000 | codepoint`).
fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) {
        cp
    } else {
        0x0100_0000 | cp
    }
}

fn platform<E: std::fmt::Display>(e: E) -> Error {
    Error::Platform {
        code: -1,
        message: e.to_string(),
    }
}

fn platform_msg(msg: &str) -> Error {
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

impl InputProvider for LinuxInputProvider {
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
        // button 5 on X11. Matches Windows' positive-delta / scroll-down
        // mapping implicitly the other way around, but "positive dy moves
        // content down" is the doc'd invariant.
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
        let keysym = self.keysym_for(key)?;
        self.key_event(keysym, true)
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        let keysym = self.keysym_for(key)?;
        self.key_event(keysym, false)
    }

    fn type_text(&self, text: &str) -> Result<()> {
        // For each character, look up its keysym; if the keymap has it in the
        // shifted column, hold Shift for that press. This is the X11 analogue
        // of the KEYEVENTF_UNICODE path on Windows — both aim to be robust
        // against the active keyboard layout.
        for c in text.chars() {
            let keysym = char_keysym(c);
            let (keycode, needs_shift) = {
                let map = self.keymap.read().unwrap_or_else(|e| e.into_inner());
                match map.lookup(keysym) {
                    Some(v) => v,
                    None => {
                        return Err(Error::Unsupported {
                            feature: format!(
                                "character '{c}' (keysym 0x{keysym:04x}) has no keycode \
                                 in the current X keyboard layout"
                            ),
                        });
                    }
                }
            };
            if needs_shift {
                self.key_event(XK_SHIFT_L, true)?;
            }
            self.send(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.send(KEY_RELEASE_EVENT, keycode, 0, 0)?;
            if needs_shift {
                self.key_event(XK_SHIFT_L, false)?;
            }
        }
        Ok(())
    }
}

fn button_number(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
    }
}
