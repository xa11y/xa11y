//! Windows input-simulation backend using `SendInput`.
//!
//! Implements [`xa11y_core::InputProvider`] with the Win32 `SendInput` API.
//! Pointer motion uses absolute, virtual-desktop coordinates; text entry uses
//! `KEYEVENTF_UNICODE` so it is keyboard-layout-independent; individual keys
//! go through virtual-key codes so downstream apps see normal `WM_KEYDOWN`
//! messages (and modifiers compose with `chord`).

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::{GetLastError, HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, VkKeyScanExW, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY, VK_0,
    VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_A, VK_B, VK_BACK, VK_C, VK_CONTROL,
    VK_D, VK_DELETE, VK_DOWN, VK_E, VK_END, VK_ESCAPE, VK_F, VK_F1, VK_F10, VK_F11, VK_F12, VK_F13,
    VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24,
    VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_G, VK_H, VK_HOME, VK_I, VK_INSERT, VK_J,
    VK_K, VK_L, VK_LEFT, VK_LWIN, VK_M, VK_MENU, VK_N, VK_NEXT, VK_O, VK_P, VK_PRIOR, VK_Q, VK_R,
    VK_RETURN, VK_RIGHT, VK_S, VK_SHIFT, VK_SPACE, VK_T, VK_TAB, VK_U, VK_UP, VK_V, VK_W, VK_X,
    VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{Error, Result};

/// Where a batch of input will land, for the UIPI pre-flight in
/// [`crate::uipi::check_target`].
#[derive(Clone, Copy)]
enum Target {
    /// Keyboard input goes to the foreground window.
    Keyboard,
    /// Mouse input goes to the window under this desktop point.
    PointerAt(Point),
    /// Mouse input at wherever the pointer already is.
    PointerHere,
    /// No pre-flight. For release events (`pointer_up`, `key_up`), which must
    /// never be refused: a refused release would leave a button or key held.
    /// Also for moves while a button is down, when mouse capture, not the
    /// window under the point, decides where the input goes.
    Unchecked,
}

/// One notch of a physical mouse wheel. Matches Win32's `WHEEL_DELTA` macro.
const WHEEL_DELTA: i32 = 120;

/// Separate Unicode characters so WPF can translate each VK_PACKET before
/// another character is queued (#620). Per-tap SendInput calls without pacing
/// still corrupted text in the native WPF stress test.
const TYPE_TEXT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// `SendInput`-based [`InputProvider`] for Windows.
///
/// All calls serialize through an internal mutex so concurrent threads don't
/// interleave pointer moves or key up/down pairs (Win32 event ordering is
/// global to the desktop session).
#[derive(Default)]
pub struct WindowsInputProvider {
    lock: Mutex<()>,
    /// The virtual key each held `Key::Char` was pressed as.
    ///
    /// A character's key depends on the keyboard layout, which can change
    /// between `key_down` and `key_up` (the user switches layout, or focus
    /// moves to a window with another one). Releasing whatever the new layout
    /// says would leave the original key stuck down, so `key_up` releases the
    /// key `key_down` actually pressed.
    held_chars: Mutex<HashMap<char, VIRTUAL_KEY>>,
    /// Mouse buttons currently held through this provider. While any is
    /// down, moves are not pre-flighted (see [`Target::Unchecked`]).
    buttons_down: std::sync::atomic::AtomicU32,
}

impl WindowsInputProvider {
    pub fn new() -> Result<Self> {
        // Match the provider/screenshot backends' DPI awareness so the
        // physical desktop coordinates in `pointer_move` and the virtual-screen
        // metrics in `to_absolute` agree, even if input is the first subsystem
        // constructed. Shared once-only init — see `crate::dpi`.
        crate::dpi::ensure_process_dpi_aware();
        Ok(Self::default())
    }

    fn validate_point(&self, dpi: &crate::dpi::DpiScope, point: Point) -> Result<()> {
        let displays = crate::coords::monitor_rects(dpi)?
            .into_iter()
            .map(|rect| {
                let width = i64::from(rect.right) - i64::from(rect.left);
                let height = i64::from(rect.bottom) - i64::from(rect.top);
                if width <= 0 || height <= 0 {
                    return Err(Error::Platform {
                        code: -1,
                        message: format!(
                            "EnumDisplayMonitors returned an invalid rectangle: {rect:?}"
                        ),
                    });
                }
                Ok(xa11y_core::Rect {
                    x: rect.left,
                    y: rect.top,
                    width: width as u32,
                    height: height as u32,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        xa11y_core::input::validate_pointer_regions(
            point,
            point,
            "physical desktop pixels",
            &displays,
        )
    }

    // These helpers require `lock` for the entire native event/state
    // transaction. Geometry is checked by the gesture entry point, before
    // its first event; never reject a later packet as "not delivered".
    fn move_locked(&self, dpi: &crate::dpi::DpiScope, to: Point) -> Result<()> {
        let (ax, ay) = to_absolute(dpi, to.x, to.y);
        let input = mouse_input(
            MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0,
            ax,
            ay,
            0,
        );
        let target = if self.pointer_buttons_down() {
            Target::Unchecked
        } else {
            Target::PointerAt(to)
        };
        self.send_locked(dpi, &[input], target)
    }

    fn down_locked(&self, dpi: &crate::dpi::DpiScope, button: MouseButton) -> Result<()> {
        let (down, _) = button_down_up(button);
        self.send_locked(dpi, &[mouse_input(down, 0, 0, 0)], Target::PointerHere)?;
        self.buttons_down
            .fetch_or(button_bit(button), std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }

    fn up_locked(&self, dpi: &crate::dpi::DpiScope, button: MouseButton) -> Result<()> {
        let (_, up) = button_down_up(button);
        let sent = self.send_locked(dpi, &[mouse_input(up, 0, 0, 0)], Target::Unchecked);
        if sent.is_ok() {
            self.buttons_down
                .fetch_and(!button_bit(button), std::sync::atomic::Ordering::AcqRel);
        }
        sent
    }

    fn finish_click_batch(
        &self,
        button: MouseButton,
        outcome: std::result::Result<(), (Error, u32)>,
        release: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        match outcome {
            Ok(()) => {
                self.buttons_down
                    .fetch_and(!button_bit(button), std::sync::atomic::Ordering::AcqRel);
                Ok(())
            }
            Err((error, sent)) => {
                if sent > 0 {
                    // A short paired batch may have posted only the down.
                    // Mark ownership before cleanup, and retain it if up
                    // fails. Never replace the original native failure.
                    self.buttons_down
                        .fetch_or(button_bit(button), std::sync::atomic::Ordering::AcqRel);
                    if release().is_ok() {
                        self.buttons_down
                            .fetch_and(!button_bit(button), std::sync::atomic::Ordering::AcqRel);
                    }
                }
                Err(error)
            }
        }
    }

    fn send(&self, dpi: &crate::dpi::DpiScope, inputs: &[INPUT], target: Target) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.send_locked(dpi, inputs, target)
    }

    /// Caller holds `lock` for the entire gesture, including multi-batch typing.
    fn send_locked(
        &self,
        dpi: &crate::dpi::DpiScope,
        inputs: &[INPUT],
        target: Target,
    ) -> Result<()> {
        self.preflight(dpi, target)?;
        send_packet(dpi, inputs).map_err(|(error, _)| error)
    }

    /// UIPI drops input to a higher-integrity window while SendInput still
    /// counts every event as sent, so check where it will land before posting
    /// (issue #503).
    fn preflight(&self, dpi: &crate::dpi::DpiScope, target: Target) -> Result<()> {
        let (hwnd, what) = match target {
            Target::Unchecked => (HWND::default(), "input"),
            // SAFETY: no arguments.
            Target::Keyboard => (unsafe { GetForegroundWindow() }, "keyboard input"),
            Target::PointerAt(p) => (
                crate::coords::window_from_point(dpi, POINT { x: p.x, y: p.y }),
                "mouse input",
            ),
            Target::PointerHere => {
                let p = crate::coords::cursor_pos(dpi).map_err(|e| Error::Platform {
                    code: e.code().0 as i64,
                    message: format!("GetCursorPos failed while checking the mouse target: {e}"),
                })?;
                (crate::coords::window_from_point(dpi, p), "mouse input")
            }
        };
        crate::uipi::check_target(hwnd, what)
    }
}

fn send_packet(
    dpi: &crate::dpi::DpiScope,
    inputs: &[INPUT],
) -> std::result::Result<(), (Error, u32)> {
    let sent = crate::coords::send_input(dpi, inputs);
    if sent as usize != inputs.len() {
        // Capture the native code before any cleanup call can overwrite it.
        // SAFETY: GetLastError has no arguments.
        let code = unsafe { GetLastError().0 } as i64;
        return Err((
            Error::Platform {
                code,
                message: format!(
                    "SendInput sent {sent}/{} events (GetLastError={code})",
                    inputs.len()
                ),
            },
            sent,
        ));
    }
    Ok(())
}

fn mouse_input(flags: u32, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// One tap `type_text` sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typed {
    /// One Unicode scalar, with its UTF-16 units submitted together.
    Character(char),
    /// A line break, as a `VK_RETURN` tap.
    LineBreak,
}

/// What `type_text` sends for `text`.
///
/// Line breaks are the Enter key, not Unicode CR / LF taps. Two reasons:
/// a Win32 / WinForms edit control turns a `WM_CHAR` CR *and* a `WM_CHAR` LF
/// into a line break each, so `"\r\n"` sent unit by unit typed two of them
/// (issue #498); and WPF's TextBox drops a Unicode CR or LF altogether and
/// starts a new line only on the Enter key. Enter is how a person types a
/// line break, so every control handles it.
///
/// A `"\r\n"` pair is one break, and a lone CR or a lone LF is one break
/// each, so `"\n\r"` and `"\r\r"` are two. The pairing is per call: a CR
/// ending one `type_text` call and an LF starting the next are two breaks.
fn typed_events(text: &str) -> Vec<Typed> {
    const CR: char = '\r';
    const LF: char = '\n';
    let mut out = Vec::with_capacity(text.len());
    let mut prev = None;
    for character in text.chars() {
        match character {
            LF if prev == Some(CR) => {}
            CR | LF => out.push(Typed::LineBreak),
            _ => out.push(Typed::Character(character)),
        }
        prev = Some(character);
    }
    out
}

fn unicode_input(code_unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: code_unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Normalize a physical screen pixel to the 0..=65535 absolute coordinate
/// range expected by `SendInput` with `MOUSEEVENTF_VIRTUALDESK`.
fn to_absolute(dpi: &crate::dpi::DpiScope, x: i32, y: i32) -> (i32, i32) {
    let (vx, vy, vw, vh) = crate::coords::virtual_screen(dpi);
    (absolute_axis(x, vx, vw), absolute_axis(y, vy, vh))
}

fn absolute_axis(value: i32, origin: i32, extent: i32) -> i32 {
    let extent = i64::from(extent.max(1));
    let normalized = ((i64::from(value) - i64::from(origin)) * 65535 + extent / 2) / extent;
    // Explicit targets have passed display preflight. Captured movement may
    // leave that region: Win32 absolute input still addresses only this range.
    // Clamp in i64 before narrowing so an extreme captured point cannot wrap.
    normalized.clamp(0, 65535) as i32
}

/// Map a named [`Key`] to its Win32 virtual-key code. Returns the VK plus a
/// bool indicating whether the key should be marked extended (arrow keys,
/// navigation cluster, Right Alt/Ctrl, etc.).
fn vk_for(key: &Key) -> Result<(VIRTUAL_KEY, bool)> {
    let v = match key {
        Key::Shift => (VK_SHIFT, false),
        Key::Ctrl => (VK_CONTROL, false),
        Key::Alt => (VK_MENU, false),
        Key::Meta => (VK_LWIN, true),
        Key::Enter => (VK_RETURN, false),
        Key::Escape => (VK_ESCAPE, false),
        Key::Backspace => (VK_BACK, false),
        Key::Tab => (VK_TAB, false),
        Key::Space => (VK_SPACE, false),
        Key::Delete => (VK_DELETE, true),
        Key::Insert => (VK_INSERT, true),
        Key::ArrowUp => (VK_UP, true),
        Key::ArrowDown => (VK_DOWN, true),
        Key::ArrowLeft => (VK_LEFT, true),
        Key::ArrowRight => (VK_RIGHT, true),
        Key::Home => (VK_HOME, true),
        Key::End => (VK_END, true),
        Key::PageUp => (VK_PRIOR, true),
        Key::PageDown => (VK_NEXT, true),
        Key::F(n) => {
            let vk = match n {
                1 => VK_F1,
                2 => VK_F2,
                3 => VK_F3,
                4 => VK_F4,
                5 => VK_F5,
                6 => VK_F6,
                7 => VK_F7,
                8 => VK_F8,
                9 => VK_F9,
                10 => VK_F10,
                11 => VK_F11,
                12 => VK_F12,
                13 => VK_F13,
                14 => VK_F14,
                15 => VK_F15,
                16 => VK_F16,
                17 => VK_F17,
                18 => VK_F18,
                19 => VK_F19,
                20 => VK_F20,
                21 => VK_F21,
                22 => VK_F22,
                23 => VK_F23,
                24 => VK_F24,
                _ => {
                    return Err(Error::InvalidActionData {
                        message: format!("F{n} is out of range (1..=24)"),
                    });
                }
            };
            (vk, false)
        }
        Key::Char(c) => (vk_for_char(*c)?, false),
    };
    Ok(v)
}

/// Map a `Key::Char` value to a virtual-key code. The [`Key`] contract
/// guarantees `c` is not ASCII uppercase. Letters and digits use their fixed
/// `VK_A`..`VK_Z` / `VK_0`..`VK_9` codes, which Windows defines per layout
/// already (shortcuts such as Ctrl+1 depend on that). Everything else is
/// resolved on the active layout by [`vk_on_active_layout`].
fn vk_for_char(c: char) -> Result<VIRTUAL_KEY> {
    let vk = match c {
        'a' => VK_A,
        'b' => VK_B,
        'c' => VK_C,
        'd' => VK_D,
        'e' => VK_E,
        'f' => VK_F,
        'g' => VK_G,
        'h' => VK_H,
        'i' => VK_I,
        'j' => VK_J,
        'k' => VK_K,
        'l' => VK_L,
        'm' => VK_M,
        'n' => VK_N,
        'o' => VK_O,
        'p' => VK_P,
        'q' => VK_Q,
        'r' => VK_R,
        's' => VK_S,
        't' => VK_T,
        'u' => VK_U,
        'v' => VK_V,
        'w' => VK_W,
        'x' => VK_X,
        'y' => VK_Y,
        'z' => VK_Z,
        '0' => VK_0,
        '1' => VK_1,
        '2' => VK_2,
        '3' => VK_3,
        '4' => VK_4,
        '5' => VK_5,
        '6' => VK_6,
        '7' => VK_7,
        '8' => VK_8,
        '9' => VK_9,
        // Everything else (punctuation, and letters outside a-z such as
        // `ß` or `é`) lives on a different key per layout, so it is looked up
        // on the layout that will receive the key, never assumed to be US
        // (issue #508).
        _ => return vk_on_active_layout(c),
    };
    Ok(vk)
}

/// Resolve `c` to a virtual key on the keyboard layout of the foreground
/// window's thread, which is the layout that will interpret the key press.
///
/// The foreground window's thread is not always the thread that owns
/// keyboard focus (`GetGUIThreadInfo` would name that one), but within one
/// window the two share a layout in practice, and the foreground thread is
/// the one Windows switches layouts for.
fn vk_on_active_layout(c: char) -> Result<VIRTUAL_KEY> {
    let mut units = [0u16; 2];
    let encoded = c.encode_utf16(&mut units);
    if encoded.len() != 1 {
        return Err(Error::Unsupported {
            feature: format!(
                "Key::Char('{c}') is outside the Basic Multilingual Plane, so no key on a \
                 Windows layout produces it; use Keyboard::type_text"
            ),
        });
    }
    // SAFETY: plain Win32 calls; a null foreground window yields thread id 0,
    // which GetKeyboardLayout reads as the calling thread. With no foreground
    // window there is no other layout the key could be read under.
    let layout = unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        GetKeyboardLayout(thread)
    };
    // SAFETY: plain Win32 call on a value code unit and layout handle.
    let scan = unsafe { VkKeyScanExW(encoded[0], layout) };
    decode_vk_scan(c, scan, layout.0 as usize)
}

/// Interpret a `VkKeyScanExW` result: the low byte is the virtual key, the
/// high byte the modifiers the layout needs to produce `c`, and `-1` means no
/// key produces it.
///
/// A character that needs modifiers is refused rather than chorded:
/// `Key::Char` names a key, never a shifted symbol (that is why it rejects
/// ASCII uppercase), so `"/"` on a German layout, where it is Shift+7, has no
/// key of its own.
fn decode_vk_scan(c: char, scan: i16, layout: usize) -> Result<VIRTUAL_KEY> {
    if scan == -1 {
        return Err(Error::Unsupported {
            feature: format!(
                "Key::Char('{c}') is on no key of the active keyboard layout \
                 (HKL {layout:#x}); use Keyboard::type_text for arbitrary characters"
            ),
        });
    }
    let [vk, shift_state] = (scan as u16).to_le_bytes();
    if shift_state != 0 {
        let mut needs = Vec::new();
        if shift_state & 1 != 0 {
            needs.push("Shift");
        }
        if shift_state & 2 != 0 {
            needs.push("Ctrl");
        }
        if shift_state & 4 != 0 {
            needs.push("Alt");
        }
        if needs.is_empty() {
            needs.push("a layout-specific modifier");
        }
        return Err(Error::Unsupported {
            feature: format!(
                "Key::Char('{c}') needs {} on the active keyboard layout (HKL {layout:#x}), \
                 so it is not a key of its own there; chord the base key with those \
                 modifiers, or use Keyboard::type_text",
                needs.join("+")
            ),
        });
    }
    Ok(VIRTUAL_KEY(u16::from(vk)))
}

fn button_down_up(button: MouseButton) -> (u32, u32) {
    match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN.0, MOUSEEVENTF_LEFTUP.0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN.0, MOUSEEVENTF_RIGHTUP.0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN.0, MOUSEEVENTF_MIDDLEUP.0),
    }
}

fn button_bit(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
    }
}

#[cfg(test)]
mod partial_click_tests {
    use super::*;
    use std::cell::Cell;

    fn short_send(sent: u32) -> std::result::Result<(), (Error, u32)> {
        Err((
            Error::Platform {
                code: 123,
                message: "original short send".into(),
            },
            sent,
        ))
    }

    #[test]
    fn partial_down_attempts_release_before_returning_original_error() {
        let provider = WindowsInputProvider::default();
        let _guard = provider.lock.lock().unwrap();
        let released = Cell::new(false);
        let error = provider
            .finish_click_batch(MouseButton::Left, short_send(1), || {
                assert!(provider.pointer_buttons_down());
                released.set(true);
                Ok(())
            })
            .unwrap_err();
        assert!(released.get());
        assert!(!provider.pointer_buttons_down());
        assert!(
            matches!(error, Error::Platform { code: 123, ref message } if message == "original short send")
        );
    }

    #[test]
    fn failed_release_keeps_capture_and_first_error() {
        let provider = WindowsInputProvider::default();
        let _guard = provider.lock.lock().unwrap();
        let error = provider
            .finish_click_batch(MouseButton::Right, short_send(1), || {
                assert!(provider.pointer_buttons_down());
                Err(Error::Platform {
                    code: 456,
                    message: "release failed".into(),
                })
            })
            .unwrap_err();
        assert!(provider.pointer_buttons_down());
        assert!(matches!(error, Error::Platform { code: 123, .. }));
    }

    #[test]
    fn zero_inserted_events_preserve_prior_capture_without_false_ownership() {
        let provider = WindowsInputProvider::default();
        let _guard = provider.lock.lock().unwrap();
        provider
            .finish_click_batch(MouseButton::Left, short_send(0), || {
                panic!("no down was sent")
            })
            .unwrap_err();
        assert!(!provider.pointer_buttons_down());
        provider.buttons_down.store(
            button_bit(MouseButton::Right),
            std::sync::atomic::Ordering::Release,
        );
        provider
            .finish_click_batch(MouseButton::Left, short_send(0), || {
                panic!("no down was sent")
            })
            .unwrap_err();
        assert!(provider.pointer_buttons_down());
    }
}

// Every method enters a `DpiScope`: they all read or produce coordinates
// (bounds in snapshots, window geometry, input points), which Windows answers
// in the calling thread's DPI space (issue #506).
impl InputProvider for WindowsInputProvider {
    fn validate_pointer_target(&self, point: Point) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        self.validate_point(&dpi, point)
    }

    fn pointer_buttons_down(&self) -> bool {
        self.buttons_down.load(std::sync::atomic::Ordering::Acquire) != 0
    }

    fn pointer_move(&self, to: Point) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.pointer_buttons_down() {
            self.validate_point(&dpi, to)?;
        }
        self.move_locked(&dpi, to)
    }

    fn pointer_down(&self, button: MouseButton) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let point = crate::coords::cursor_pos(&dpi).map_err(|error| Error::Platform {
            code: error.code().0 as i64,
            message: format!("GetCursorPos failed while validating the mouse target: {error}"),
        })?;
        self.validate_point(&dpi, Point::new(point.x, point.y))?;
        self.down_locked(&dpi, button)
    }

    fn pointer_up(&self, button: MouseButton) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Releases must not depend on geometry, capture ownership, or UIPI.
        self.up_locked(&dpi, button)
    }

    fn pointer_drag(
        &self,
        from: Point,
        to: Point,
        button: MouseButton,
        duration: std::time::Duration,
    ) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_point(&dpi, from)?;
        self.validate_point(&dpi, to)?;
        self.preflight(&dpi, Target::PointerAt(from))?;
        self.preflight(&dpi, Target::PointerAt(to))?;
        self.move_locked(&dpi, from)?;
        self.down_locked(&dpi, button)?;
        const STEP: std::time::Duration = std::time::Duration::from_millis(16);
        let steps = (duration.as_millis() / STEP.as_millis().max(1)).max(1) as i32;
        let moved = (|| {
            for i in 1..=steps {
                let t = f64::from(i) / f64::from(steps);
                let x =
                    (f64::from(from.x) + (f64::from(to.x) - f64::from(from.x)) * t).round() as i32;
                let y =
                    (f64::from(from.y) + (f64::from(to.y) - f64::from(from.y)) * t).round() as i32;
                // Capture routes intermediate moves, including monitor gaps.
                self.move_locked(&dpi, Point::new(x, y))?;
                if i < steps {
                    std::thread::sleep(STEP);
                }
            }
            Ok(())
        })();
        let released = self.up_locked(&dpi, button);
        moved.and(released)
    }

    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_point(&dpi, at)?;
        self.move_locked(&dpi, at)?;
        let (down, up) = button_down_up(button);
        for _ in 0..count {
            // A preflight refusal posts nothing and must not alter held state.
            self.preflight(&dpi, Target::PointerAt(at))?;
            let outcome = send_packet(
                &dpi,
                &[mouse_input(down, 0, 0, 0), mouse_input(up, 0, 0, 0)],
            );
            self.finish_click_batch(button, outcome, || {
                self.send_locked(&dpi, &[mouse_input(up, 0, 0, 0)], Target::Unchecked)
            })?;
        }
        Ok(())
    }

    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_point(&dpi, at)?;
        self.move_locked(&dpi, at)?;
        if delta.dy != 0 {
            self.send_locked(
                &dpi,
                &[mouse_input(
                    MOUSEEVENTF_WHEEL.0,
                    0,
                    0,
                    delta.dy * WHEEL_DELTA,
                )],
                Target::PointerAt(at),
            )?;
        }
        if delta.dx != 0 {
            self.send_locked(
                &dpi,
                &[mouse_input(
                    MOUSEEVENTF_HWHEEL.0,
                    0,
                    0,
                    delta.dx * WHEEL_DELTA,
                )],
                Target::PointerAt(at),
            )?;
        }
        Ok(())
    }
    fn key_down(&self, key: &Key) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        let (vk, extended) = vk_for(key)?;
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        self.send(&dpi, &[key_input(vk, flags)], Target::Keyboard)?;
        if let Key::Char(c) = key {
            self.held_chars
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(*c, vk);
        }
        Ok(())
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        // Entering the scope can fail before anything is sent, leaving the
        // button or key held. That is acceptable: the result is deterministic
        // per thread (the thread's awareness and whether it may switch do not
        // change between calls, unless the host changes this thread's
        // awareness in between), so the matching down on this thread would
        // already have failed the same way and pressed nothing.
        let dpi = crate::dpi::DpiScope::enter()?;
        // Release the key `key_down` pressed, not what the layout says now.
        let held = match key {
            Key::Char(c) => self
                .held_chars
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(c)
                .copied(),
            _ => None,
        };
        let (vk, extended) = match held {
            Some(vk) => (vk, false),
            None => vk_for(key)?,
        };
        let mut flags = KEYEVENTF_KEYUP;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        // Never refused: a release must always reach the wire.
        self.send(&dpi, &[key_input(vk, flags)], Target::Unchecked)?;
        if let Key::Char(c) = key {
            self.held_chars
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(c);
        }
        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<()> {
        let dpi = crate::dpi::DpiScope::enter()?;
        // Use KEYEVENTF_UNICODE so case/shift/IME behaviour is produced by
        // the OS text-input path rather than by synthesising shift chords
        // against whatever keyboard layout happens to be active.
        let events = typed_events(text);
        // Keep the whole call serialized across submissions and pacing.
        // WPF can translate an earlier VK_PACKET as a later character when
        // distinct characters are queued too quickly, even in separate
        // SendInput calls (#620). Keep surrogate pairs in one submission.
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        for event in events {
            let mut inputs = Vec::with_capacity(4);
            match event {
                Typed::Character(character) => {
                    let mut units = [0u16; 2];
                    for &unit in character.encode_utf16(&mut units).iter() {
                        inputs.push(unicode_input(unit, false));
                        inputs.push(unicode_input(unit, true));
                    }
                }
                Typed::LineBreak => {
                    inputs.push(key_input(VK_RETURN, KEYBD_EVENT_FLAGS(0)));
                    inputs.push(key_input(VK_RETURN, KEYEVENTF_KEYUP));
                }
            }
            self.send_locked(&dpi, &inputs, Target::Keyboard)?;
            // Include the final scalar: the next type_text call may start
            // immediately, including a call that types just one character.
            std::thread::sleep(TYPE_TEXT_INTERVAL);
        }
        Ok(())
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_OEM_1, VK_OEM_2};

    #[test]
    fn unshifted_key_resolves_to_its_vk() {
        // US ';' is VK_OEM_1 with no modifiers.
        assert_eq!(
            decode_vk_scan(';', VK_OEM_1.0 as i16, 0x409).unwrap(),
            VK_OEM_1
        );
    }

    #[test]
    fn shifted_symbol_is_refused_naming_the_modifier() {
        // German '/' is Shift+7: VK_7 (0x37) with shift state 1.
        let err = decode_vk_scan('/', 0x0137, 0x407).expect_err("needs Shift");
        let msg = err.to_string();
        assert!(msg.contains("Shift"), "{msg}");
        assert!(msg.contains("0x407"), "{msg}");
        // AltGr (Ctrl+Alt) symbols, e.g. German '{'.
        let err = decode_vk_scan('{', 0x0637, 0x407).expect_err("needs AltGr");
        assert!(err.to_string().contains("Ctrl+Alt"), "{err}");
    }

    #[test]
    fn character_off_the_layout_is_refused() {
        let err = decode_vk_scan('\u{00e6}', -1, 0x409).expect_err("not on US");
        assert!(err.to_string().contains("no key"), "{err}");
    }

    #[test]
    fn us_slash_still_resolves() {
        assert_eq!(
            decode_vk_scan('/', VK_OEM_2.0 as i16, 0x409).unwrap(),
            VK_OEM_2
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{absolute_axis, typed_events, Typed};

    #[test]
    fn absolute_coordinates_widen_before_subtraction_and_clamp_before_narrowing() {
        assert_eq!(absolute_axis(0, 0, 100), 0);
        assert_eq!(absolute_axis(50, 0, 100), 32768);
        assert_eq!(absolute_axis(-50, -100, 100), 32768);
        assert_eq!(absolute_axis(i32::MAX, i32::MIN, 1), 65535);
        assert_eq!(absolute_axis(i32::MIN, i32::MAX, 1), 0);
        assert_eq!(absolute_axis(100, 0, 100), 65535);
    }

    /// Render the events as text, with `⏎` for each Enter tap.
    fn typed(text: &str) -> String {
        typed_events(text)
            .into_iter()
            .map(|e| match e {
                Typed::Character(character) => character.to_string(),
                Typed::LineBreak => "\u{23CE}".into(),
            })
            .collect()
    }

    #[test]
    fn crlf_is_one_enter() {
        assert_eq!(typed("line1\r\nline2"), "line1\u{23CE}line2");
        assert_eq!(typed("a\r\nb\r\nc"), "a\u{23CE}b\u{23CE}c");
    }

    #[test]
    fn lone_breaks_are_one_enter_each() {
        assert_eq!(typed("a\nb"), "a\u{23CE}b");
        assert_eq!(typed("a\rb"), "a\u{23CE}b");
        assert_eq!(typed("\n"), "\u{23CE}");
        assert_eq!(typed("\r"), "\u{23CE}");
    }

    #[test]
    fn only_the_cr_lf_pair_collapses() {
        // LF then CR is two breaks, as is CR CR; CR CR LF is CR + CRLF.
        assert_eq!(typed("\n\r"), "\u{23CE}\u{23CE}");
        assert_eq!(typed("\r\r"), "\u{23CE}\u{23CE}");
        assert_eq!(typed("\r\r\n"), "\u{23CE}\u{23CE}");
        // The LF after a collapsed pair is a break of its own.
        assert_eq!(typed("\r\n\n"), "\u{23CE}\u{23CE}");
    }

    #[test]
    fn surrogate_pairs_stay_in_one_typed_character() {
        let events = typed_events("x\u{1F600}");
        assert_eq!(
            events,
            [Typed::Character('x'), Typed::Character('\u{1F600}')]
        );
        assert!(typed_events("").is_empty());
    }
}
