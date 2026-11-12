//! macOS input-simulation backend using Quartz `CGEvent` APIs.
//!
//! Posts synthesised pointer and keyboard events to the HID event tap
//! (`kCGHIDEventTap`). Requires the host process to have the **Accessibility**
//! and **Input Monitoring** privacy grants — absent those, the OS silently
//! drops events (there is no API-level error signal). The backend itself only
//! surfaces failures from `CGEvent*` allocation.
//!
//! Drag events: `pointer_drag` is overridden to emit the distinct
//! `kCGEventLeftMouseDragged` / `…RightMouseDragged` / `…OtherMouseDragged`
//! types between the down and up — drag-and-drop source views filter on
//! those and ignore ordinary `kCGEventMouseMoved`.
//!
//! `type_text` uses `CGEventKeyboardSetUnicodeString` so case, IME, and
//! dead-key composition are produced by the OS text path rather than being
//! synthesised against the active keyboard layout.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

use core_foundation::base::{CFRelease, CFTypeRef};

use xa11y_core::input::{
    validate_pointer_regions, InputProvider, Key, MouseButton, Point, ScrollDelta,
};
use xa11y_core::{Error, Rect, Result};

// ── Quartz / CoreGraphics FFI ───────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

// CGEventTapLocation
const K_CG_HID_EVENT_TAP: u32 = 0;

// CGEventType
const K_CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const K_CG_EVENT_MOUSE_MOVED: u32 = 5;
const K_CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
const K_CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
const K_CG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;

// CGMouseButton
const K_CG_MOUSE_BUTTON_LEFT: u32 = 0;
const K_CG_MOUSE_BUTTON_RIGHT: u32 = 1;
const K_CG_MOUSE_BUTTON_CENTER: u32 = 2;

// CGScrollEventUnit
const K_CG_SCROLL_EVENT_UNIT_LINE: u32 = 1;

// CGEventField
const K_CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;

// CGEventSourceStateID
const K_CG_EVENT_SOURCE_STATE_PRIVATE: i32 = -1;
const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;

// CGEventFlags (modifier flags applied at post time).
const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
const MODIFIER_FLAGS: u64 = K_CG_EVENT_FLAG_MASK_SHIFT
    | K_CG_EVENT_FLAG_MASK_CONTROL
    | K_CG_EVENT_FLAG_MASK_ALTERNATE
    | K_CG_EVENT_FLAG_MASK_COMMAND;

// Flags that macOS puts on a key by its key code (arrows, Home, F-keys),
// not modifiers that a key-down holds.
const K_CG_EVENT_FLAG_MASK_NUMERIC_PAD: u64 = 0x0020_0000;
const K_CG_EVENT_FLAG_MASK_SECONDARY_FN: u64 = 0x0080_0000;
const KEY_CODE_FLAGS: u64 = K_CG_EVENT_FLAG_MASK_NUMERIC_PAD | K_CG_EVENT_FLAG_MASK_SECONDARY_FN;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;

    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        mouse_cursor_position: CGPoint,
        mouse_button: u32,
    ) -> CGEventRef;

    // `CGEventCreateScrollWheelEvent` is variadic (`wheel1, ...`). Declared
    // here with fixed parameters, wheel2 went where a variadic callee does
    // not look on arm64, and every scroll carried a garbage deltaX
    // (826560 in the WKWebView). The `2` variant takes all three wheels as
    // fixed parameters.
    fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;

    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;

    fn CGEventPost(tap: u32, event: CGEventRef);

    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);

    fn CGEventSetFlags(event: CGEventRef, flags: u64);

    fn CGEventGetFlags(event: CGEventRef) -> u64;

    fn CGEventGetType(event: CGEventRef) -> u32;

    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;

    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;

    fn CGEventSourceFlagsState(state_id: i32) -> u64;

    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, chars: *const u16);
}

// ── Provider ────────────────────────────────────────────────────────

/// CGEvent-backed [`InputProvider`].
///
/// All calls serialise through an internal mutex so interleaving threads can't
/// scramble the posted event stream (macOS delivers events globally to the
/// focused app, so ordering is session-wide).
#[derive(Default)]
pub struct MacOSInputProvider {
    lock: Mutex<()>,
    pointer_lock: Mutex<()>,
    held_buttons: AtomicU8,
    /// CGEventFlags for the modifier keys currently held via `key_down`.
    ///
    /// macOS stamps an event with modifier state **at creation time**, read
    /// from the window server's combined state — and a *synthetic* key-down
    /// does not update that state synchronously. Posting `key_down(Shift)`
    /// and a click back-to-back, as core's `with_keys_held` does for
    /// `Mouse::click_with`, therefore produced a click carrying no modifier
    /// at all: by the time the window server caught up, the key-up had
    /// already been posted.
    ///
    /// Tracking the held flags here and stamping them onto every event we
    /// post makes the modifier deterministic, with no sleep to tune. It also
    /// covers `drag_with(held: …)`, which reaches the same primitives.
    held_flags: AtomicU64,
    /// Modifier flags released through `key_up` that the session state may
    /// still report.
    ///
    /// An event from a NULL source copies the session's modifiers when it is
    /// created, and the window server applies a synthetic key-up a moment
    /// after the post. So after `chord("u", ["Meta"])`, the next "i" copied
    /// Cmd and arrived as Cmd+I (#681). `post` strips these flags from every
    /// event until the session state no longer reports them, then forgets
    /// them, so a modifier held on the physical keyboard after that is kept.
    released_flags: AtomicU64,
}

/// Quartz display bounds and CGEvent positions share the global logical
/// coordinate space, whose origin is the main display's upper-left corner.
fn display_regions() -> Result<Vec<Rect>> {
    let mut count = 0;
    // SAFETY: a null display buffer with capacity zero queries the count.
    let status = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) };
    if status != 0 {
        return Err(Error::Platform {
            code: i64::from(status),
            message: "CGGetActiveDisplayList(count) failed".into(),
        });
    }
    // One spare slot detects a growing list rather than accepting a truncated
    // geometry set. Do not bound the native set by the diagnosis's size limit.
    let capacity = count.checked_add(1).ok_or_else(|| Error::Platform {
        code: -1,
        message: "CGGetActiveDisplayList count overflow".into(),
    })?;
    let mut displays = Vec::new();
    displays
        .try_reserve_exact(capacity as usize)
        .map_err(|error| Error::Platform {
            code: -1,
            message: format!("allocating CGGetActiveDisplayList buffer: {error}"),
        })?;
    displays.resize(capacity as usize, 0);
    let mut written = 0;
    // SAFETY: the vector contains capacity writable display IDs.
    let status = unsafe { CGGetActiveDisplayList(capacity, displays.as_mut_ptr(), &mut written) };
    if status != 0 {
        return Err(Error::Platform {
            code: i64::from(status),
            message: "CGGetActiveDisplayList(list) failed".into(),
        });
    }
    if written >= capacity {
        return Err(Error::Platform {
            code: -1,
            message: "active display list changed while enumerating; geometry may be incomplete"
                .into(),
        });
    }
    displays.truncate(written as usize);
    displays
        .into_iter()
        .map(|display| {
            // SAFETY: each ID came from the active-display list.
            quartz_display_rect(unsafe { CGDisplayBounds(display) })
        })
        .collect()
}

fn quartz_display_rect(bounds: CGRect) -> Result<Rect> {
    // Point coordinates are integral. Ceil both half-open edges to preserve
    // exactly the integer points inside even a fractional native rectangle.
    let left = bounds.origin.x.ceil();
    let top = bounds.origin.y.ceil();
    let right = (bounds.origin.x + bounds.size.width).ceil();
    let bottom = (bounds.origin.y + bounds.size.height).ceil();
    let invalid_geometry = || Error::Platform {
        code: -1,
        message: format!(
            "CGDisplayBounds returned invalid or unrepresentable geometry: {bounds:?}"
        ),
    };
    if ![left, top, right, bottom]
        .iter()
        .all(|edge| edge.is_finite())
        || bounds.size.width < 0.0
        || bounds.size.height < 0.0
        || left < f64::from(i32::MIN)
        || top < f64::from(i32::MIN)
        || left > f64::from(i32::MAX)
        || top > f64::from(i32::MAX)
        || right - left < 0.0
        || bottom - top < 0.0
        || right - left > f64::from(u32::MAX)
        || bottom - top > f64::from(u32::MAX)
    {
        return Err(invalid_geometry());
    }
    // The finite, integral edges/spans above are exactly representable as
    // i64. Check the final signed origin and unsigned dimensions separately;
    // a valid Rect's far edge may exceed the range of an individual Point.
    Ok(Rect {
        x: i32::try_from(left as i64).map_err(|_| invalid_geometry())?,
        y: i32::try_from(top as i64).map_err(|_| invalid_geometry())?,
        width: u32::try_from((right - left) as i64).map_err(|_| invalid_geometry())?,
        height: u32::try_from((bottom - top) as i64).map_err(|_| invalid_geometry())?,
    })
}

fn button_mask(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
    }
}

impl MacOSInputProvider {
    // Callers hold pointer_lock through native events and capture-state updates.
    fn move_pointer(&self, to: Point) -> Result<()> {
        let held = self.held_buttons.load(Ordering::SeqCst);
        // Captured moves need the native dragged event, just like pointer_drag.
        // For an ordinary mouse-moved event the button argument is ignored.
        let (kind, button) = if held & button_mask(MouseButton::Left) != 0 {
            (K_CG_EVENT_LEFT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_LEFT)
        } else if held & button_mask(MouseButton::Right) != 0 {
            (K_CG_EVENT_RIGHT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_RIGHT)
        } else if held & button_mask(MouseButton::Middle) != 0 {
            (K_CG_EVENT_OTHER_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_CENTER)
        } else {
            (K_CG_EVENT_MOUSE_MOVED, K_CG_MOUSE_BUTTON_LEFT)
        };
        let ev = self.mouse_event(kind, to, button)?;
        self.post(ev)
    }

    fn press_pointer(&self, button: MouseButton) -> Result<()> {
        let ev = self.button_event(button, true)?;
        self.post(ev)?;
        self.held_buttons
            .fetch_or(button_mask(button), Ordering::SeqCst);
        Ok(())
    }

    fn release_pointer(&self, button: MouseButton) -> Result<()> {
        let ev = self.button_event(button, false)?;
        self.post(ev)?;
        self.held_buttons
            .fetch_and(!button_mask(button), Ordering::SeqCst);
        Ok(())
    }

    fn press_pointer_at(&self, button: MouseButton, at: Point) -> Result<()> {
        let event = self.mouse_event(cg_button_down(button), at, cg_button(button))?;
        self.post(event)?;
        self.held_buttons
            .fetch_or(button_mask(button), Ordering::SeqCst);
        Ok(())
    }

    fn release_pointer_at(&self, button: MouseButton, at: Point) -> Result<()> {
        let event = self.mouse_event(cg_button_up(button), at, cg_button(button))?;
        self.post(event)?;
        self.held_buttons
            .fetch_and(!button_mask(button), Ordering::SeqCst);
        Ok(())
    }

    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }

    fn post(&self, event: CGEventRef) -> Result<()> {
        if event.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventCreate* returned NULL".to_string(),
            });
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Stamp any modifiers held via `key_down` onto this event. See
        // `held_flags` for why the window server's own state can't be relied
        // on here. OR rather than assign, so flags the event already carries
        // (caps lock, a physically-held key) survive.
        //
        // An event from a NULL source also copies the session's Fn and
        // NumericPad flags, which the last arrow, Home or F-key left there,
        // so "a" after Home went out as Fn+A. Drop the ones that this event's
        // own key code does not carry, and leave other flags untouched.
        let held = self.held_flags.load(Ordering::SeqCst);
        // SAFETY: `event` is a valid CGEventRef we just created.
        let flags = unsafe { CGEventGetFlags(event) };
        // Modifiers that xa11y released and the session still reports. Once
        // the session no longer reports one, it is forgotten (see
        // `released_flags`).
        let released = self.released_flags.load(Ordering::SeqCst);
        let stale = if released == 0 {
            0
        } else {
            // SAFETY: CGEventSourceFlagsState takes a state ID and has no
            // other preconditions.
            let session =
                unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };
            self.released_flags
                .fetch_and(session | !released, Ordering::SeqCst);
            flags & released & !held
        };
        let borrowed = if flags & KEY_CODE_FLAGS == 0 {
            0
        } else {
            match key_code_flags(event) {
                Ok(own) => flags & KEY_CODE_FLAGS & !own,
                Err(error) => {
                    // SAFETY: `event` is a valid CGEventRef we own and do not post.
                    unsafe { CFRelease(event as CFTypeRef) };
                    return Err(error);
                }
            }
        };
        if held != 0 || borrowed != 0 || stale != 0 {
            // SAFETY: `event` is a valid CGEventRef we just created.
            unsafe { CGEventSetFlags(event, (flags & !borrowed & !stale) | held) };
        }
        // SAFETY: `event` is a valid CGEventRef we just created; CGEventPost
        // takes an owned +0 borrow and does not consume it.
        unsafe {
            CGEventPost(K_CG_HID_EVENT_TAP, event);
            CFRelease(event as CFTypeRef);
        }
        Ok(())
    }

    fn mouse_event(&self, ty: u32, at: Point, button: u32) -> Result<CGEventRef> {
        let pt = CGPoint {
            x: at.x as f64,
            y: at.y as f64,
        };
        // SAFETY: nil source is valid per the CGEventSource docs ("Use NULL to
        // create a combined state from the pasted-in event source defaults").
        let ev = unsafe { CGEventCreateMouseEvent(std::ptr::null_mut(), ty, pt, button) };
        if ev.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: format!("CGEventCreateMouseEvent failed (type={ty})"),
            });
        }
        Ok(ev)
    }

    fn button_event(&self, button: MouseButton, down: bool) -> Result<CGEventRef> {
        // Button-only primitives act at the current native pointer position.
        // In particular a captured release must not teleport to (0, 0), nor
        // depend on a display-geometry preflight succeeding.
        // SAFETY: a null source samples the combined native event state.
        let current = unsafe { CGEventCreate(std::ptr::null_mut()) };
        if current.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventCreate failed while reading the current pointer position".into(),
            });
        }
        // SAFETY: current is a live event; the location is copied before release.
        let at = unsafe { CGEventGetLocation(current) };
        unsafe { CFRelease(current as CFTypeRef) };
        let kind = if down {
            cg_button_down(button)
        } else {
            cg_button_up(button)
        };
        // SAFETY: at is in Quartz global coordinates, exactly as CGEvent expects.
        let event =
            unsafe { CGEventCreateMouseEvent(std::ptr::null_mut(), kind, at, cg_button(button)) };
        if event.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventCreateMouseEvent failed for a button-only event".into(),
            });
        }
        Ok(event)
    }

    /// Post one key-down/key-up pair carrying the character `c`.
    /// `CGEventKeyboardSetUnicodeString` replaces the synthetic keystroke
    /// the event would normally generate with the supplied UTF-16 string,
    /// so we get correct case/IME without synthesising shift chords.
    fn type_unicode_char(&self, c: char) -> Result<()> {
        let mut buf = [0u16; 2];
        let utf16 = c.encode_utf16(&mut buf);
        let ev = self.keyboard_event(0, true)?;
        // SAFETY: `utf16.as_ptr()` is valid for `utf16.len()` u16 reads
        // for the duration of the call.
        unsafe {
            CGEventKeyboardSetUnicodeString(ev, utf16.len(), utf16.as_ptr());
        }
        self.post(ev)?;
        let ev = self.keyboard_event(0, false)?;
        unsafe {
            CGEventKeyboardSetUnicodeString(ev, utf16.len(), utf16.as_ptr());
        }
        self.post(ev)
    }

    fn keyboard_event(&self, vk: u16, down: bool) -> Result<CGEventRef> {
        // SAFETY: nil source is allowed by CGEventSource contract.
        let ev = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), vk, down) };
        if ev.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventCreateKeyboardEvent failed".to_string(),
            });
        }
        Ok(ev)
    }
}

// ── Mapping helpers ─────────────────────────────────────────────────

/// The Fn and NumericPad flags that macOS gives the key of a key event, read
/// from a new event of a private source, which holds no session state.
fn key_code_flags(event: CGEventRef) -> Result<u64> {
    // SAFETY: `event` is a valid CGEventRef; the probe and its source are
    // released before return.
    unsafe {
        let ty = CGEventGetType(event);
        if ty != K_CG_EVENT_KEY_DOWN && ty != K_CG_EVENT_KEY_UP {
            return Ok(0);
        }
        let vk = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_PRIVATE);
        if source.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventSourceCreate(private) returned NULL".to_string(),
            });
        }
        let probe = CGEventCreateKeyboardEvent(source, vk, ty == K_CG_EVENT_KEY_DOWN);
        CFRelease(source as CFTypeRef);
        if probe.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: format!("CGEventCreateKeyboardEvent failed (key code {vk})"),
            });
        }
        let flags = CGEventGetFlags(probe) & KEY_CODE_FLAGS;
        CFRelease(probe as CFTypeRef);
        Ok(flags)
    }
}

fn cg_button(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => K_CG_MOUSE_BUTTON_LEFT,
        MouseButton::Right => K_CG_MOUSE_BUTTON_RIGHT,
        MouseButton::Middle => K_CG_MOUSE_BUTTON_CENTER,
    }
}

fn cg_button_down(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => K_CG_EVENT_LEFT_MOUSE_DOWN,
        MouseButton::Right => K_CG_EVENT_RIGHT_MOUSE_DOWN,
        MouseButton::Middle => K_CG_EVENT_OTHER_MOUSE_DOWN,
    }
}

fn cg_button_up(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => K_CG_EVENT_LEFT_MOUSE_UP,
        MouseButton::Right => K_CG_EVENT_RIGHT_MOUSE_UP,
        MouseButton::Middle => K_CG_EVENT_OTHER_MOUSE_UP,
    }
}

fn cg_button_dragged(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => K_CG_EVENT_LEFT_MOUSE_DRAGGED,
        MouseButton::Right => K_CG_EVENT_RIGHT_MOUSE_DRAGGED,
        MouseButton::Middle => K_CG_EVENT_OTHER_MOUSE_DRAGGED,
    }
}

/// Virtual key code for a [`Key`]. Codes are the standard HIToolbox
/// `Events.h` keycodes (USB HID usage page → physical key on a US layout
/// for letters/digits).
fn vk_for(key: &Key) -> Result<u16> {
    let vk = match key {
        Key::Shift => 0x38,
        Key::Ctrl => 0x3B,
        Key::Alt => 0x3A,
        Key::Meta => 0x37,
        Key::Enter => 0x24,
        Key::Escape => 0x35,
        Key::Backspace => 0x33,
        Key::Tab => 0x30,
        Key::Space => 0x31,
        Key::Delete => 0x75,
        // macOS has no "Insert" key. The "Help" key on older keyboards
        // occupies the same position and is the conventional mapping.
        Key::Insert => 0x72,
        Key::ArrowUp => 0x7E,
        Key::ArrowDown => 0x7D,
        Key::ArrowLeft => 0x7B,
        Key::ArrowRight => 0x7C,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,
        Key::F(n) => match n {
            1 => 0x7A,
            2 => 0x78,
            3 => 0x63,
            4 => 0x76,
            5 => 0x60,
            6 => 0x61,
            7 => 0x62,
            8 => 0x64,
            9 => 0x65,
            10 => 0x6D,
            11 => 0x67,
            12 => 0x6F,
            13 => 0x69,
            14 => 0x6B,
            15 => 0x71,
            16 => 0x6A,
            17 => 0x40,
            18 => 0x4F,
            19 => 0x50,
            20 => 0x5A,
            _ => {
                return Err(Error::invalid_action_data(format!(
                    "F{n} has no HIToolbox keycode on macOS"
                )));
            }
        },
        Key::Char(c) => vk_for_char(*c)?,
    };
    Ok(vk)
}

/// Virtual key code for `Key::Char`. The [`Key`] contract guarantees `c` is
/// not ASCII uppercase. Letters, digits, and common US-layout punctuation
/// are mapped here; anything else returns [`Error::Unsupported`] and the
/// caller is expected to use `Keyboard::type_text` instead.
fn vk_for_char(c: char) -> Result<u16> {
    let vk = match c {
        'a' => 0x00,
        'b' => 0x0B,
        'c' => 0x08,
        'd' => 0x02,
        'e' => 0x0E,
        'f' => 0x03,
        'g' => 0x05,
        'h' => 0x04,
        'i' => 0x22,
        'j' => 0x26,
        'k' => 0x28,
        'l' => 0x25,
        'm' => 0x2E,
        'n' => 0x2D,
        'o' => 0x1F,
        'p' => 0x23,
        'q' => 0x0C,
        'r' => 0x0F,
        's' => 0x01,
        't' => 0x11,
        'u' => 0x20,
        'v' => 0x09,
        'w' => 0x0D,
        'x' => 0x07,
        'y' => 0x10,
        'z' => 0x06,
        '0' => 0x1D,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1A,
        '8' => 0x1C,
        '9' => 0x19,
        ';' => 0x29,
        '/' => 0x2C,
        '`' => 0x32,
        '[' => 0x21,
        '\\' => 0x2A,
        ']' => 0x1E,
        '\'' => 0x27,
        ',' => 0x2B,
        '.' => 0x2F,
        '-' => 0x1B,
        '=' => 0x18,
        _ => {
            return Err(Error::Unsupported {
                feature: format!(
                    "Key::Char('{c}') has no HIToolbox virtual-key mapping; \
                     use Keyboard::type_text for arbitrary characters"
                ),
            });
        }
    };
    Ok(vk)
}

// ── InputProvider impl ──────────────────────────────────────────────

impl InputProvider for MacOSInputProvider {
    fn validate_pointer_target(&self, point: Point) -> Result<()> {
        validate_pointer_regions(
            point,
            point,
            "quartz global logical coordinates",
            &display_regions()?,
        )
    }

    fn pointer_buttons_down(&self) -> bool {
        self.held_buttons.load(Ordering::SeqCst) != 0
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
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if count == 0 {
            return Ok(());
        }
        self.validate_pointer_target(at)?;
        let clicked = (|| {
            self.move_pointer(at)?;
            // `kCGMouseEventClickState` tells the OS which click in a sequence this
            // is (1 = single, 2 = double, …). We emit N paired down/up events
            // with increasing click-state so AppKit/Cocoa recognises multi-clicks
            // regardless of the OS double-click timing window.
            for i in 1..=count {
                let down = self.mouse_event(cg_button_down(button), at, cg_button(button))?;
                // SAFETY: down is a valid CGEventRef.
                unsafe {
                    CGEventSetIntegerValueField(down, K_CG_MOUSE_EVENT_CLICK_STATE, i as i64);
                }
                self.post(down)?;
                self.held_buttons
                    .fetch_or(button_mask(button), Ordering::SeqCst);
                let up = self.mouse_event(cg_button_up(button), at, cg_button(button))?;
                unsafe {
                    CGEventSetIntegerValueField(up, K_CG_MOUSE_EVENT_CLICK_STATE, i as i64);
                }
                self.post(up)?;
                self.held_buttons
                    .fetch_and(!button_mask(button), Ordering::SeqCst);
            }
            Ok(())
        })();
        if clicked.is_err() && self.held_buttons.load(Ordering::SeqCst) & button_mask(button) != 0 {
            let released = self.release_pointer_at(button, at);
            clicked.and(released)
        } else {
            clicked
        }
    }

    fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let _guard = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Quartz: a positive wheel1 scrolls toward the top and a positive
        // wheel2 toward the left. Positive `dy` scrolls toward the end and
        // positive `dx` toward the right (#688), so both are negated. Done
        // before the first OS call: `i32::MIN` has no negation, and a refused
        // delta must not leave the pointer moved.
        let negate = |axis: &str, n: i32| {
            n.checked_neg().ok_or_else(|| {
                Error::invalid_action_data(format!(
                    "scroll delta {axis}={n} cannot be reversed for Quartz's wheel direction"
                ))
            })
        };
        let wheel1 = negate("dy", delta.dy)?;
        let wheel2 = negate("dx", delta.dx)?;
        self.validate_pointer_target(at)?;
        self.move_pointer(at)?;
        // SAFETY: nil source is valid.
        let ev = unsafe {
            CGEventCreateScrollWheelEvent2(
                std::ptr::null_mut(),
                K_CG_SCROLL_EVENT_UNIT_LINE,
                2,
                wheel1,
                wheel2,
                0,
            )
        };
        if ev.is_null() {
            return Err(Error::Platform {
                code: -1,
                message: "CGEventCreateScrollWheelEvent2 failed".to_string(),
            });
        }
        self.post(ev)
    }

    fn key_down(&self, key: &Key) -> Result<()> {
        let vk = vk_for(key)?;
        let ev = self.keyboard_event(vk, true)?;
        // Modifier keys posted as an ordinary key-down don't set the
        // event-flags bit Cocoa inspects for "is Shift held". Record the flag
        // *before* posting so this event carries it too, matching hardware —
        // a real Shift key-down already reports shift as set. `post` stamps
        // it onto this and every later event until `key_up` clears it.
        let flags = modifier_flag_for(key);
        let (was_held, was_released) = if flags != 0 {
            (
                self.held_flags.fetch_or(flags, Ordering::SeqCst) & flags,
                self.released_flags.fetch_and(!flags, Ordering::SeqCst) & flags,
            )
        } else {
            (0, 0)
        };
        let posted = self.post(ev);
        if posted.is_err() && flags != 0 {
            // The key-down never reached the window server, so the modifier
            // is not held: put both records back as they were, or every
            // later event would carry a modifier nobody pressed.
            self.held_flags
                .fetch_and(!(flags & !was_held), Ordering::SeqCst);
            self.released_flags.fetch_or(was_released, Ordering::SeqCst);
        }
        posted
    }

    fn key_up(&self, key: &Key) -> Result<()> {
        let vk = vk_for(key)?;
        let ev = self.keyboard_event(vk, false)?;
        // Clear before posting, again matching hardware: a real Shift key-up
        // reports the modifier as already released.
        let flags = modifier_flag_for(key);
        let (mut was_held, mut was_released) = (0, 0);
        if flags != 0 {
            let before = self.held_flags.fetch_and(!flags, Ordering::SeqCst);
            was_held = before & flags;
            let held = before & !flags;
            was_released = self.released_flags.fetch_or(flags, Ordering::SeqCst) & flags;
            // The event copied the session's modifier flags when it was
            // created, and the window server has not yet seen the key-ups
            // posted just before it. After Cmd+Ctrl+R, the Ctrl key-up still
            // carried Ctrl, and the window server kept Ctrl in the session
            // state: the next "a" arrived as Ctrl+A. A modifier key-up
            // therefore carries only the modifiers still held through
            // `key_down`. A modifier held on the physical keyboard is dropped
            // from this one event; the next event copies it back.
            // SAFETY: `ev` is a valid CGEventRef we just created.
            unsafe {
                let current = CGEventGetFlags(ev);
                CGEventSetFlags(ev, (current & !MODIFIER_FLAGS) | held);
            }
        }
        let posted = self.post(ev);
        if posted.is_err() && flags != 0 {
            // The key-up never reached the window server, so the modifier is
            // still down: restore both records, or the next event would go
            // out without a modifier the session still holds.
            self.held_flags.fetch_or(was_held, Ordering::SeqCst);
            self.released_flags
                .fetch_and(!(flags & !was_released), Ordering::SeqCst);
        }
        posted
    }

    fn type_text(&self, text: &str) -> Result<()> {
        for step in plan_type_text(text) {
            match step {
                TypeStep::Char(c) => self.type_unicode_char(c)?,
                // A line break is the Enter key on every platform (#498).
                // As Unicode text, AppKit text views insert "\r" or "\n"
                // inconsistently and controls that act on Return never see
                // the key.
                TypeStep::LineBreak => {
                    self.key_down(&Key::Enter)?;
                    self.key_up(&Key::Enter)?;
                }
                // A tab is the Tab key, as on the other platforms: as Unicode
                // text, a control that moves focus on Tab never sees the key.
                TypeStep::Tab => {
                    self.key_down(&Key::Tab)?;
                    self.key_up(&Key::Tab)?;
                }
            }
        }
        Ok(())
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
        // Drag-and-drop source views on macOS filter for
        // kCGEventLeftMouseDragged / …RightMouseDragged / …OtherMouseDragged
        // specifically — ordinary kCGEventMouseMoved between the down and up
        // is ignored by DnD. Synthesise the sequence ourselves using the
        // right dragged-event type instead of relying on the core trait's
        // default (which emits mouse-moved).
        const STEP: std::time::Duration = std::time::Duration::from_millis(16);
        self.move_pointer(from)?;
        // Use the gesture's explicit coordinates, rather than querying cursor
        // state that may not yet reflect the just-posted move.
        self.press_pointer_at(button, from)?;
        // AppKit drag sources start an NSDraggingSession only once the button
        // has been held briefly, and drop targets register the drop only if
        // the pointer rests on them before release. Without these holds a
        // synthesized drag moves the pointer but delivers no drop.
        std::thread::sleep(DRAG_HOLD_AFTER_DOWN);
        let step_ms = STEP.as_millis().max(1);
        let steps = (duration.as_millis() / step_ms).max(1) as i32;
        let dragged_ty = cg_button_dragged(button);
        let cg_btn = cg_button(button);
        let mut last_posted = from;
        let moved = (|| {
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let x =
                    (f64::from(from.x) + (f64::from(to.x) - f64::from(from.x)) * t).round() as i32;
                let y =
                    (f64::from(from.y) + (f64::from(to.y) - f64::from(from.y)) * t).round() as i32;
                let ev = self.mouse_event(dragged_ty, Point::new(x, y), cg_btn)?;
                self.post(ev)?;
                last_posted = Point::new(x, y);
                if i < steps {
                    std::thread::sleep(STEP);
                }
            }
            std::thread::sleep(DRAG_HOLD_BEFORE_UP);
            Ok(())
        })();
        let released = self.release_pointer_at(button, last_posted);
        moved.and(released)
    }
}

/// One step of `type_text`: a character posted as Unicode, a line break
/// posted as the Return key, or a tab posted as the Tab key.
///
/// Each character is its own key event. An app that reads key events, not
/// text input, takes only the first character of an event's string: a
/// 20-character event typed "7" of "7*6=" into Calculator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeStep {
    Char(char),
    LineBreak,
    Tab,
}

/// Split `text` into one [`TypeStep::Char`] per character, with each line
/// break (`"\r\n"`, `"\r"` or `"\n"`) as one [`TypeStep::LineBreak`] and
/// each `"\t"` as one [`TypeStep::Tab`].
/// Pairing is per call: a `"\r"` ending one call and a `"\n"` starting the
/// next are two line breaks.
fn plan_type_text(text: &str) -> Vec<TypeStep> {
    let mut steps = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                steps.push(TypeStep::LineBreak);
            }
            '\t' => steps.push(TypeStep::Tab),
            _ => steps.push(TypeStep::Char(c)),
        }
    }
    steps
}

/// Pause after the button goes down, before the first dragged event, so an
/// AppKit drag source has time to start its dragging session.
const DRAG_HOLD_AFTER_DOWN: std::time::Duration = std::time::Duration::from_millis(200);

/// Pause at the end point before the button comes up, so the drop target
/// under the pointer registers the drop.
const DRAG_HOLD_BEFORE_UP: std::time::Duration = std::time::Duration::from_millis(500);

fn modifier_flag_for(key: &Key) -> u64 {
    match key {
        Key::Shift => K_CG_EVENT_FLAG_MASK_SHIFT,
        Key::Ctrl => K_CG_EVENT_FLAG_MASK_CONTROL,
        Key::Alt => K_CG_EVENT_FLAG_MASK_ALTERNATE,
        Key::Meta => K_CG_EVENT_FLAG_MASK_COMMAND,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_regions_keep_negative_logical_origins() {
        let rect = quartz_display_rect(CGRect {
            origin: CGPoint {
                x: -1440.0,
                y: -900.0,
            },
            size: CGSize {
                width: 1440.0,
                height: 900.0,
            },
        })
        .expect("native logical display");
        assert_eq!(
            rect,
            Rect {
                x: -1440,
                y: -900,
                width: 1440,
                height: 900
            }
        );
    }

    #[test]
    fn fractional_display_edges_preserve_integer_point_membership() {
        let rect = quartz_display_rect(CGRect {
            origin: CGPoint { x: -9.5, y: -2.4 },
            size: CGSize {
                width: 9.75,
                height: 1.5,
            },
        })
        .expect("fractional logical display");
        assert_eq!(
            rect,
            Rect {
                x: -9,
                y: -2,
                width: 10,
                height: 2
            }
        );
        assert!(
            validate_pointer_regions(Point::new(0, -1), Point::new(0, -1), "test", &[rect]).is_ok()
        );
        assert!(
            validate_pointer_regions(Point::new(1, -1), Point::new(1, -1), "test", &[rect])
                .is_err()
        );
        assert!(validate_pointer_regions(
            Point::new(-10, -1),
            Point::new(-10, -1),
            "test",
            &[rect]
        )
        .is_err());
    }

    #[test]
    fn native_display_dimensions_keep_the_full_unsigned_range() {
        for origin in [i32::MIN, i32::MAX] {
            let rect = quartz_display_rect(CGRect {
                origin: CGPoint {
                    x: f64::from(origin),
                    y: f64::from(origin),
                },
                size: CGSize {
                    width: f64::from(u32::MAX),
                    height: f64::from(u32::MAX),
                },
            })
            .expect("a signed origin with unsigned dimensions is representable");
            assert_eq!(
                rect,
                Rect {
                    x: origin,
                    y: origin,
                    width: u32::MAX,
                    height: u32::MAX
                }
            );
            let point = Point::new(origin, origin);
            assert!(validate_pointer_regions(point, point, "test", &[rect]).is_ok());
        }
    }

    #[test]
    fn malformed_native_display_geometry_is_an_error() {
        for bounds in [
            CGRect {
                origin: CGPoint {
                    x: f64::INFINITY,
                    y: 0.0,
                },
                size: CGSize {
                    width: 100.0,
                    height: 100.0,
                },
            },
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: f64::NAN,
                    height: 100.0,
                },
            },
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: -1.0,
                    height: 100.0,
                },
            },
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: f64::from(u32::MAX) + 1.0,
                    height: 100.0,
                },
            },
        ] {
            assert!(matches!(
                quartz_display_rect(bounds),
                Err(Error::Platform { .. })
            ));
        }
    }

    fn chars(s: &str) -> Vec<TypeStep> {
        s.chars().map(TypeStep::Char).collect()
    }

    #[test]
    fn every_line_break_shape_is_one_return() {
        // "\r\n" is one line break, not two (#498).
        assert_eq!(
            plan_type_text("a\r\nb\rc\nd"),
            vec![
                TypeStep::Char('a'),
                TypeStep::LineBreak,
                TypeStep::Char('b'),
                TypeStep::LineBreak,
                TypeStep::Char('c'),
                TypeStep::LineBreak,
                TypeStep::Char('d'),
            ]
        );
    }

    #[test]
    fn consecutive_breaks_stay_separate() {
        assert_eq!(
            plan_type_text("\n\r\r\n\n"),
            vec![
                TypeStep::LineBreak,
                TypeStep::LineBreak,
                TypeStep::LineBreak,
                TypeStep::LineBreak
            ]
        );
        // "\n\r" is two breaks: only CR followed by LF pairs up.
        assert_eq!(
            plan_type_text("x\n\ry"),
            vec![
                TypeStep::Char('x'),
                TypeStep::LineBreak,
                TypeStep::LineBreak,
                TypeStep::Char('y')
            ]
        );
    }

    #[test]
    fn each_character_is_its_own_event() {
        assert_eq!(plan_type_text("7*6="), chars("7*6="));
        // A non-BMP emoji is one character: its surrogate pair is never
        // split across events.
        assert_eq!(plan_type_text("é😀\n"), {
            let mut steps = chars("é😀");
            steps.push(TypeStep::LineBreak);
            steps
        });
        assert_eq!(plan_type_text(""), vec![]);
    }

    #[test]
    fn tab_is_the_tab_key() {
        assert_eq!(
            plan_type_text("a\tb\t\r\n"),
            vec![
                TypeStep::Char('a'),
                TypeStep::Tab,
                TypeStep::Char('b'),
                TypeStep::Tab,
                TypeStep::LineBreak,
            ]
        );
    }
}
