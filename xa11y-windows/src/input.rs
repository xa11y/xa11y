//! Windows input-simulation backend using `SendInput`.
//!
//! Implements [`xa11y_core::InputProvider`] with the Win32 `SendInput` API.
//! Pointer motion uses absolute, virtual-desktop coordinates; text entry uses
//! `KEYEVENTF_UNICODE` so it is keyboard-layout-independent; individual keys
//! go through virtual-key codes so downstream apps see normal `WM_KEYDOWN`
//! messages (and modifiers compose with `chord`).

use std::sync::Mutex;

use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY, VK_0, VK_1, VK_2, VK_3,
    VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_A, VK_B, VK_BACK, VK_C, VK_CONTROL, VK_D, VK_DELETE,
    VK_DOWN, VK_E, VK_END, VK_ESCAPE, VK_F, VK_F1, VK_F10, VK_F11, VK_F12, VK_F13, VK_F14, VK_F15,
    VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24, VK_F3, VK_F4,
    VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_G, VK_H, VK_HOME, VK_I, VK_INSERT, VK_J, VK_K, VK_L,
    VK_LEFT, VK_LWIN, VK_M, VK_MENU, VK_N, VK_NEXT, VK_O, VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4,
    VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_P,
    VK_PRIOR, VK_Q, VK_R, VK_RETURN, VK_RIGHT, VK_S, VK_SHIFT, VK_SPACE, VK_T, VK_TAB, VK_U, VK_UP,
    VK_V, VK_W, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{Error, Result};

/// One notch of a physical mouse wheel. Matches Win32's `WHEEL_DELTA` macro.
const WHEEL_DELTA: i32 = 120;

/// `SendInput`-based [`InputProvider`] for Windows.
///
/// All calls serialize through an internal mutex so concurrent threads don't
/// interleave pointer moves or key up/down pairs (Win32 event ordering is
/// global to the desktop session).
#[derive(Default)]
pub struct WindowsInputProvider {
    lock: Mutex<()>,
}

impl WindowsInputProvider {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }

    fn send(&self, inputs: &[INPUT]) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let size = std::mem::size_of::<INPUT>() as i32;
        // SAFETY: `inputs` is a valid slice and `size` matches `INPUT`.
        let sent = unsafe { SendInput(inputs, size) };
        if sent as usize != inputs.len() {
            // SAFETY: GetLastError takes no args and is always safe to call.
            let code = unsafe { GetLastError().0 } as i64;
            return Err(Error::Platform {
                code,
                message: format!(
                    "SendInput sent {sent}/{} events (GetLastError={code})",
                    inputs.len()
                ),
            });
        }
        Ok(())
    }
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
fn to_absolute(x: i32, y: i32) -> (i32, i32) {
    // SAFETY: GetSystemMetrics takes a constant and returns i32; always safe.
    let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
    let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);
    // +vw/2 / vw biases toward the logical centre of each pixel (Microsoft's
    // documented formula for normalised absolute mouse coordinates).
    let ax = (((x - vx) as i64 * 65535 + (vw as i64 / 2)) / vw as i64) as i32;
    let ay = (((y - vy) as i64 * 65535 + (vh as i64 / 2)) / vh as i64) as i32;
    (ax, ay)
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

/// Map a `Key::Char` value to a virtual-key code assuming a US layout for
/// symbols. The [`Key`] contract guarantees `c` is not ASCII uppercase, so we
/// only need to handle lowercase letters, digits, and common OEM punctuation.
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
        ';' => VK_OEM_1,
        '/' => VK_OEM_2,
        '`' => VK_OEM_3,
        '[' => VK_OEM_4,
        '\\' => VK_OEM_5,
        ']' => VK_OEM_6,
        '\'' => VK_OEM_7,
        ',' => VK_OEM_COMMA,
        '.' => VK_OEM_PERIOD,
        '-' => VK_OEM_MINUS,
        '=' => VK_OEM_PLUS,
        _ => {
            return Err(Error::Unsupported {
                feature: format!(
                    "Key::Char('{c}') has no Win32 virtual-key mapping; \
                     use Keyboard::type_text for arbitrary characters"
                ),
            });
        }
    };
    Ok(vk)
}

fn button_down_up(button: MouseButton) -> (u32, u32) {
    match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN.0, MOUSEEVENTF_LEFTUP.0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN.0, MOUSEEVENTF_RIGHTUP.0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN.0, MOUSEEVENTF_MIDDLEUP.0),
    }
}

impl InputProvider for WindowsInputProvider {
    fn pointer_move(&self, to: Point) -> Result<()> {
        let (ax, ay) = to_absolute(to.x, to.y);
        let input = mouse_input(
            MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0,
            ax,
            ay,
            0,
        );
        self.send(&[input])
    }

    fn pointer_down(&self, button: MouseButton) -> Result<()> {
        let (down, _) = button_down_up(button);
        self.send(&[mouse_input(down, 0, 0, 0)])
    }

    fn pointer_up(&self, button: MouseButton) -> Result<()> {
        let (_, up) = button_down_up(button);
        self.send(&[mouse_input(up, 0, 0, 0)])
    }

    fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.pointer_move(at)?;
        let (down, up) = button_down_up(button);
        // Batch each press/release into one SendInput call so the system
        // bookkeeps the click-count (double/triple click) for us when the
        // calls arrive within the OS double-click time.
        for _ in 0..count {
            self.send(&[mouse_input(down, 0, 0, 0), mouse_input(up, 0, 0, 0)])?;
        }
        Ok(())
    }

    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        self.pointer_move(at)?;
        let step = WHEEL_DELTA;
        if delta.dy != 0 {
            // `Input.dy > 0` means scroll content down per module docs, which
            // maps to a *positive* wheel delta on Win32 (wheel rolled forward).
            self.send(&[mouse_input(MOUSEEVENTF_WHEEL.0, 0, 0, delta.dy * step)])?;
        }
        if delta.dx != 0 {
            self.send(&[mouse_input(MOUSEEVENTF_HWHEEL.0, 0, 0, delta.dx * step)])?;
        }
        Ok(())
    }

    fn key_down(&self, key: &Key) -> Result<()> {
        let (vk, extended) = vk_for(key)?;
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        self.send(&[key_input(vk, flags)])
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        let (vk, extended) = vk_for(key)?;
        let mut flags = KEYEVENTF_KEYUP;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        self.send(&[key_input(vk, flags)])
    }

    fn type_text(&self, text: &str) -> Result<()> {
        // Use KEYEVENTF_UNICODE so case/shift/IME behaviour is produced by
        // the OS text-input path rather than by synthesising shift chords
        // against whatever keyboard layout happens to be active.
        let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
        for code_unit in text.encode_utf16() {
            inputs.push(unicode_input(code_unit, false));
            inputs.push(unicode_input(code_unit, true));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        self.send(&inputs)
    }
}
