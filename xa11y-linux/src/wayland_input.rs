//! Wayland input-simulation backend, implemented via the kernel's
//! `/dev/uinput` device.
//!
//! This is the same mechanism `xdotool --using-uinput`, `ydotool`, `wtype`,
//! Steam Input, Wine, Sunshine, and most Linux automation tools use. The
//! kernel exposes our virtual device on the same `evdev → libinput →
//! compositor` pipeline as a USB keyboard/mouse, so events reach every
//! Wayland compositor (GNOME, KDE, sway, Hyprland, Cosmic, weston) without
//! any portal infrastructure. It also works on X11; we keep the XTest
//! backend in `input.rs` only because XTest needs no special privilege and
//! changing existing X11 users' setup would be a gratuitous regression.
//!
//! ## Privilege model
//!
//! Opening `/dev/uinput` for writing requires either root or membership
//! in the `input` group. Granting the user the `input` group also grants
//! global keystroke read access via `/dev/input/event*` — the same threat
//! surface as macOS "Input Monitoring" or any X11 client connecting to
//! `$DISPLAY`. xa11y surfaces an actionable [`Error::PermissionDenied`]
//! when the open fails with `EACCES`.
//!
//! ## Keymap
//!
//! Keysym → evdev-keycode translation goes through `xkbcommon` against
//! the system's default xkb keymap (rules=evdev, model=pc105, layout=us
//! when names are empty). We declare every keycode in the keymap as
//! emittable on the virtual device, so a future user-layout-aware path
//! can swap the keymap without restructuring the device.
//!
//! ## Coordinate space
//!
//! The virtual pointer is configured with absolute axes in a virtual
//! 0..=`COORD_MAX` range. xa11y's `pointer_move((x, y))` expects screen
//! pixels, so we translate via a screen size queried opportunistically
//! from `wl_output` at construction time, falling back to a 1920x1080
//! default. Compositors map our 0..=`COORD_MAX` range onto the active
//! virtual screen.

use std::sync::Mutex;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    PropType, RelativeAxisCode, UinputAbsSetup,
};
use xkbcommon::xkb;

use xa11y_core::input::{Key as XaKey, MouseButton, Point, ScrollDelta};
use xa11y_core::{Error, Result};

use crate::input::{char_keysym, key_to_keysym, XK_SHIFT_L};

/// Virtual coordinate range for the absolute pointer. Compositors scale
/// this onto the active virtual screen — same convention used by graphics
/// tablets and remote-desktop clients.
const COORD_MAX: i32 = 32767;

/// Default screen-pixel range when we can't query the actual display.
/// Used to map xa11y's screen-pixel inputs into the 0..=COORD_MAX space.
const DEFAULT_SCREEN_W: i32 = 1920;
const DEFAULT_SCREEN_H: i32 = 1080;

pub(crate) struct WaylandInputBackend {
    inner: Mutex<UinputState>,
    screen_w: i32,
    screen_h: i32,
}

struct UinputState {
    device: VirtualDevice,
    keymap: ReverseKeymap,
}

/// keysym → (evdev keycode, needs_shift). The X11 backend has its own
/// equivalent; this one builds against an xkb keymap so we honour the
/// user's actual layout.
struct ReverseKeymap {
    table: std::collections::HashMap<u32, (u16, bool)>,
}

impl ReverseKeymap {
    fn from_xkb(keymap: &xkb::Keymap) -> Self {
        let mut table = std::collections::HashMap::new();
        let min: u32 = keymap.min_keycode().into();
        let max: u32 = keymap.max_keycode().into();
        for kc_raw in min..=max {
            let kc_xkb = xkb::Keycode::from(kc_raw);
            // Levels 0 (unshifted) and 1 (shifted). Higher AltGr/level3
            // stuff isn't reachable from xa11y's `Key` today.
            for level in 0..2u32 {
                for sym in keymap.key_get_syms_by_level(kc_xkb, 0, level) {
                    let raw: u32 = (*sym).into();
                    if raw == 0 {
                        continue;
                    }
                    // evdev keycode = xkb keycode − 8 (Linux convention).
                    let evdev_kc = kc_raw.wrapping_sub(8);
                    if evdev_kc > u16::MAX as u32 {
                        continue;
                    }
                    table.entry(raw).or_insert((evdev_kc as u16, level == 1));
                }
            }
        }
        Self { table }
    }

    fn lookup(&self, keysym: u32) -> Option<(u16, bool)> {
        self.table.get(&keysym).copied()
    }
}

impl WaylandInputBackend {
    pub(crate) fn new() -> Result<Self> {
        let (screen_w, screen_h) = detect_screen_size();

        // Build an xkb keymap from system defaults and enumerate every
        // keycode that maps to a keysym we might emit. The kernel needs
        // an explicit set of `KEY_*` codes the device can produce.
        let xkb_ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &xkb_ctx,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| Error::Platform {
            code: -1,
            message: "xkbcommon: failed to compile default keymap".into(),
        })?;
        let reverse = ReverseKeymap::from_xkb(&keymap);

        let mut keys = AttributeSet::<KeyCode>::new();
        for &(evdev_kc, _shift) in reverse.table.values() {
            keys.insert(KeyCode(evdev_kc));
        }
        for btn in [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE] {
            keys.insert(btn);
        }

        let abs_x = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, 0, COORD_MAX, 0, 0, 1),
        );
        let abs_y = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_Y,
            AbsInfo::new(0, 0, COORD_MAX, 0, 0, 1),
        );

        let mut props = AttributeSet::<PropType>::new();
        props.insert(PropType::POINTER);

        let mut rels = AttributeSet::<RelativeAxisCode>::new();
        rels.insert(RelativeAxisCode::REL_WHEEL);
        rels.insert(RelativeAxisCode::REL_HWHEEL);

        let device = VirtualDevice::builder()
            .map_err(map_open_err)?
            .name("xa11y virtual input")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1209, 0xa11a, 1))
            .with_keys(&keys)
            .map_err(map_io)?
            .with_absolute_axis(&abs_x)
            .map_err(map_io)?
            .with_absolute_axis(&abs_y)
            .map_err(map_io)?
            .with_relative_axes(&rels)
            .map_err(map_io)?
            .with_properties(&props)
            .map_err(map_io)?
            .build()
            .map_err(map_io)?;

        Ok(Self {
            inner: Mutex::new(UinputState {
                device,
                keymap: reverse,
            }),
            screen_w,
            screen_h,
        })
    }

    /// Convert a screen-pixel coord into the device's virtual 0..=COORD_MAX
    /// range. Compositors then scale the virtual range onto whatever the
    /// active screen is.
    fn map_x(&self, x: i32) -> i32 {
        let w = if self.screen_w > 0 { self.screen_w } else { 1 };
        ((x as i64) * (COORD_MAX as i64) / (w as i64)).clamp(0, COORD_MAX as i64) as i32
    }

    fn map_y(&self, y: i32) -> i32 {
        let h = if self.screen_h > 0 { self.screen_h } else { 1 };
        ((y as i64) * (COORD_MAX as i64) / (h as i64)).clamp(0, COORD_MAX as i64) as i32
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, UinputState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn pointer_move(&self, to: Point) -> Result<()> {
        let x = self.map_x(to.x);
        let y = self.map_y(to.y);
        let mut s = self.lock();
        emit(
            &mut s.device,
            &[
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
            ],
        )
    }

    pub(crate) fn pointer_down(&self, button: MouseButton) -> Result<()> {
        let key = button_key(button);
        let mut s = self.lock();
        emit(
            &mut s.device,
            &[InputEvent::new(EventType::KEY.0, key.0, 1)],
        )
    }

    pub(crate) fn pointer_up(&self, button: MouseButton) -> Result<()> {
        let key = button_key(button);
        let mut s = self.lock();
        emit(
            &mut s.device,
            &[InputEvent::new(EventType::KEY.0, key.0, 0)],
        )
    }

    pub(crate) fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let x = self.map_x(at.x);
        let y = self.map_y(at.y);
        let key = button_key(button);
        let mut s = self.lock();
        // Move + click as one frame so the compositor sees the position
        // and the button-down on the same input report.
        let mut frame = vec![
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
        ];
        for _ in 0..count {
            frame.push(InputEvent::new(EventType::KEY.0, key.0, 1));
            frame.push(InputEvent::new(EventType::KEY.0, key.0, 0));
        }
        emit(&mut s.device, &frame)
    }

    pub(crate) fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let x = self.map_x(at.x);
        let y = self.map_y(at.y);
        let mut s = self.lock();
        // REL_WHEEL: negative = down, positive = up. ScrollDelta uses
        // "positive dy = content scrolls down" (wheel rolled toward the
        // user), so wheel value = -dy.
        let wheel = -delta.dy;
        let hwheel = delta.dx;
        let mut frame = vec![
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
        ];
        if wheel != 0 {
            frame.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_WHEEL.0,
                wheel,
            ));
        }
        if hwheel != 0 {
            frame.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_HWHEEL.0,
                hwheel,
            ));
        }
        emit(&mut s.device, &frame)
    }

    pub(crate) fn key_down(&self, key: &XaKey) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        let mut s = self.lock();
        let (kc, _shift) = s.keymap.lookup(keysym).ok_or_else(|| Error::Unsupported {
            feature: format!("keysym 0x{keysym:04x} not in xkb keymap"),
        })?;
        emit(&mut s.device, &[InputEvent::new(EventType::KEY.0, kc, 1)])
    }

    pub(crate) fn key_up(&self, key: &XaKey) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        let mut s = self.lock();
        let (kc, _shift) = s.keymap.lookup(keysym).ok_or_else(|| Error::Unsupported {
            feature: format!("keysym 0x{keysym:04x} not in xkb keymap"),
        })?;
        emit(&mut s.device, &[InputEvent::new(EventType::KEY.0, kc, 0)])
    }

    pub(crate) fn type_text(&self, text: &str) -> Result<()> {
        let mut s = self.lock();
        let shift_kc = s
            .keymap
            .lookup(XK_SHIFT_L)
            .ok_or_else(|| Error::Unsupported {
                feature: "no Shift_L in xkb keymap".into(),
            })?
            .0;

        for c in text.chars() {
            let keysym = char_keysym(c);
            let (kc, needs_shift) = s.keymap.lookup(keysym).ok_or_else(|| Error::Unsupported {
                feature: format!(
                    "character '{c}' (keysym 0x{keysym:04x}) has no keycode \
                     in the current xkb keyboard layout"
                ),
            })?;
            let mut frame = vec![];
            if needs_shift {
                frame.push(InputEvent::new(EventType::KEY.0, shift_kc, 1));
            }
            frame.push(InputEvent::new(EventType::KEY.0, kc, 1));
            frame.push(InputEvent::new(EventType::KEY.0, kc, 0));
            if needs_shift {
                frame.push(InputEvent::new(EventType::KEY.0, shift_kc, 0));
            }
            emit(&mut s.device, &frame)?;
        }
        Ok(())
    }
}

fn emit(dev: &mut VirtualDevice, events: &[InputEvent]) -> Result<()> {
    dev.emit(events).map_err(map_io)
}

fn button_key(b: MouseButton) -> KeyCode {
    match b {
        MouseButton::Left => KeyCode::BTN_LEFT,
        MouseButton::Right => KeyCode::BTN_RIGHT,
        MouseButton::Middle => KeyCode::BTN_MIDDLE,
    }
}

fn map_open_err(e: std::io::Error) -> Error {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => Error::PermissionDenied {
            instructions: "open /dev/uinput: permission denied — add the user to the `input` \
                           group (sudo usermod -aG input $USER) and re-login, then re-run."
                .into(),
        },
        ErrorKind::NotFound => Error::Unsupported {
            feature: "/dev/uinput not present — kernel uinput module not loaded \
                      (try `sudo modprobe uinput`)"
                .into(),
        },
        _ => Error::Platform {
            code: e.raw_os_error().unwrap_or(-1) as i64,
            message: format!("uinput open: {e}"),
        },
    }
}

fn map_io(e: std::io::Error) -> Error {
    Error::Platform {
        code: e.raw_os_error().unwrap_or(-1) as i64,
        message: format!("uinput: {e}"),
    }
}

/// Best-effort screen-size detection. Tries `XA11Y_SCREEN_WIDTH` /
/// `XA11Y_SCREEN_HEIGHT` env vars first (CI / explicit override), then
/// falls back to a 1920x1080 default. A future improvement: speak
/// `wl_output` directly to query the live display geometry.
fn detect_screen_size() -> (i32, i32) {
    let parse = |name: &str, default: i32| -> i32 {
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(default)
    };
    (
        parse("XA11Y_SCREEN_WIDTH", DEFAULT_SCREEN_W),
        parse("XA11Y_SCREEN_HEIGHT", DEFAULT_SCREEN_H),
    )
}

// ─── Unit tests ───────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use xa11y_core::input::Key as XaKey;

    fn default_keymap() -> xkb::Keymap {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .expect("US keymap should compile from xkbcommon defaults")
    }

    #[test]
    fn reverse_keymap_maps_lowercase_letter() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        let (_kc, needs_shift) = reverse.lookup(char_keysym('a')).expect("'a' in US keymap");
        assert!(!needs_shift, "'a' should be on the unshifted level");
    }

    #[test]
    fn reverse_keymap_maps_uppercase_letter_to_same_keycode_with_shift() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        let lower = reverse.lookup(char_keysym('a')).expect("'a'");
        let upper = reverse.lookup(char_keysym('A')).expect("'A'");
        assert_eq!(lower.0, upper.0, "'a' and 'A' must share a keycode");
        assert!(!lower.1);
        assert!(upper.1, "'A' must require shift");
    }

    #[test]
    fn reverse_keymap_resolves_named_keys() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        for key in [
            XaKey::Enter,
            XaKey::Escape,
            XaKey::Tab,
            XaKey::Backspace,
            XaKey::Space,
        ] {
            let sym = key_to_keysym(&key).expect("named keys map to keysyms");
            assert!(
                reverse.lookup(sym).is_some(),
                "{key:?} (keysym 0x{sym:04x}) missing from US keymap"
            );
        }
    }

    #[test]
    fn reverse_keymap_resolves_shift_l() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        assert!(
            reverse.lookup(XK_SHIFT_L).is_some(),
            "Shift_L must be in any keymap"
        );
    }

    #[test]
    fn button_key_uses_input_event_codes() {
        assert_eq!(button_key(MouseButton::Left), KeyCode::BTN_LEFT);
        assert_eq!(button_key(MouseButton::Right), KeyCode::BTN_RIGHT);
        assert_eq!(button_key(MouseButton::Middle), KeyCode::BTN_MIDDLE);
    }

    #[test]
    fn screen_size_respects_env_override() {
        // Use a Mutex over std::env to avoid racing with parallel tests.
        // For simplicity, run this test single-threadedly via #[ignore]
        // if needed — for now the env var is namespaced so collisions
        // are unlikely in `cargo test` runs.
        unsafe {
            std::env::set_var("XA11Y_SCREEN_WIDTH", "2560");
            std::env::set_var("XA11Y_SCREEN_HEIGHT", "1440");
        }
        let (w, h) = detect_screen_size();
        unsafe {
            std::env::remove_var("XA11Y_SCREEN_WIDTH");
            std::env::remove_var("XA11Y_SCREEN_HEIGHT");
        }
        assert_eq!(w, 2560);
        assert_eq!(h, 1440);
    }
}
