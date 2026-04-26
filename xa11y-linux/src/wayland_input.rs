//! Wayland input-simulation backend.
//!
//! Bridges xa11y's [`InputProvider`](xa11y_core::input::InputProvider) trait
//! onto libei via the [`reis`] crate. The EI socket FD is supplied by
//! `org.freedesktop.portal.RemoteDesktop.ConnectToEIS` after a portal
//! handshake; tests can also bypass the portal and pass an FD directly via
//! [`WaylandInputBackend::from_eis_fd`].
//!
//! ## Flow on construction
//!
//! 1. Connect to the session bus (zbus).
//! 2. `RemoteDesktop.CreateSession` → wait for `Request.Response` →
//!    extract `session_handle`.
//! 3. `RemoteDesktop.SelectDevices(types = KEYBOARD|POINTER, persist_mode =
//!    PERMANENT)` → wait for response. PERMANENT avoids re-prompting on
//!    subsequent runs.
//! 4. `RemoteDesktop.Start(session, "", {})` → wait for response. First-run
//!    consent UI surfaces here.
//! 5. `RemoteDesktop.ConnectToEIS(session, {})` → returns a `UnixFd`.
//! 6. Wrap that FD in [`reis::ei::Context`], run `handshake_blocking`, and
//!    pump events until the seat has bound `pointer_absolute`, `button`,
//!    `keyboard`, and `scroll` capabilities and the corresponding devices
//!    are `Resumed`. Capture the keymap announced for the keyboard device.
//!
//! ## Threading
//!
//! `reis::ei::Context` is `Clone` but its internal buffer state isn't
//! `Sync`. We guard the active context + device handles with a single
//! `Mutex<EiState>` and serialise emission. Input simulation is sequential
//! by nature so this is fine.
//!
//! ## Keymap
//!
//! libei semantics: the EIS server tells the client which keymap is in
//! force. We build an `xkbcommon::xkb::Keymap` from the bytes the server
//! sends, then build a reverse-lookup table mapping keysym → (keycode,
//! level) so the X11-style `keysym_for(Key)` translation can re-use the
//! same Key abstraction across both backends. evdev keycode = xkb keycode
//! − 8, the conventional offset baked into all xkb-on-Linux keymaps.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reis::ei::handshake::ContextType;
use reis::ei::{self, button::ButtonState, keyboard::KeyState};
use reis::event::{self, DeviceCapability, EiEvent, EiEventConverter};
use reis::PendingRequestResult;
use xkbcommon::xkb;
use zbus::blocking::{Connection as ZbusConnection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use xa11y_core::input::{Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{Error, Result};

use crate::input::{char_keysym, key_to_keysym, platform, platform_msg, XK_SHIFT_L};

// Linux evdev button codes — see <linux/input-event-codes.h>.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

// xkb keycodes are evdev-codes + 8, by ancient convention. The X11 layer
// also follows this; the constant is the inverse direction (xkb → evdev).
const XKB_TO_EVDEV_OFFSET: u32 = 8;

// RemoteDesktop device-type bitmask for SelectDevices.
const RD_TYPE_KEYBOARD: u32 = 1;
const RD_TYPE_POINTER: u32 = 2;

// SelectDevices.persist_mode: 0=NEVER, 1=TRANSIENT, 2=PERMANENT.
const RD_PERSIST_PERMANENT: u32 = 2;

/// Wayland input backend. Holds the open portal session, the EI context,
/// the device handles, and a reverse keymap built from the EIS-supplied
/// xkb keymap.
pub(crate) struct WaylandInputBackend {
    /// Kept alive so the portal session isn't torn down. `None` for the
    /// bypass-portal `from_eis_fd` test path.
    _zbus: Option<ZbusConnection>,
    /// Portal session handle path, used for cleanup on drop.
    _session: Option<OwnedObjectPath>,
    inner: Mutex<EiState>,
}

struct EiState {
    ctx: ei::Context,
    /// EI top-level Connection wrapper (from the handshake response).
    /// Held for the lifetime of the backend so `disconnect`-style
    /// teardown remains possible.
    _conn: ei::Connection,
    /// Request-side handles bound during construction. All four
    /// capabilities are required; any missing capability surfaces as
    /// `Error::Unsupported` from `new`.
    pointer: ei::Device,
    pointer_abs: ei::PointerAbsolute,
    keyboard: ei::Device,
    keyboard_iface: ei::Keyboard,
    button_iface: ei::Button,
    scroll_iface: ei::Scroll,
    /// Reverse keymap: keysym → (xkb keycode, needs_shift).
    keymap: ReverseKeymap,
    /// Monotonically-increasing serial used for `start_emulating` and
    /// `frame` calls. Each `frame` should bump.
    serial: u32,
    /// Per-device sequence numbers passed to `start_emulating`. Bumped on
    /// each emission; libei treats this as a transactional cookie.
    sequence: u32,
}

/// keysym → (xkb_keycode, needs_shift). Mirrors the X11 [`Keymap::lookup`]
/// shape so [`WaylandInputBackend::type_text`] can use the same shift logic.
struct ReverseKeymap {
    by_keysym: HashMap<u32, (u32, bool)>,
}

impl ReverseKeymap {
    /// Build by enumerating every keycode × layout × level in the xkb
    /// keymap and recording the keysym at each cell. Layout 0 only —
    /// multi-layout sessions still emit the user's primary group.
    fn from_xkb(keymap: &xkb::Keymap) -> Self {
        let mut by_keysym: HashMap<u32, (u32, bool)> = HashMap::new();
        let min: u32 = keymap.min_keycode().into();
        let max: u32 = keymap.max_keycode().into();
        for kc_raw in min..=max {
            let kc = xkb::Keycode::from(kc_raw);
            // Levels 0 (unshifted) and 1 (shifted). Higher levels (mode-
            // shifted, AltGr) aren't reached by xa11y today.
            for level in 0..2u32 {
                let syms = keymap.key_get_syms_by_level(kc, 0, level);
                for sym in syms {
                    let raw: u32 = (*sym).into();
                    if raw == 0 {
                        continue;
                    }
                    by_keysym.entry(raw).or_insert((kc_raw, level == 1));
                }
            }
        }
        Self { by_keysym }
    }

    fn lookup(&self, keysym: u32) -> Option<(u32, bool)> {
        self.by_keysym.get(&keysym).copied()
    }
}

impl WaylandInputBackend {
    /// Open a portal RemoteDesktop session and hand the EI socket to libei.
    pub(crate) fn new() -> Result<Self> {
        let zbus = ZbusConnection::session().map_err(|e| Error::Platform {
            code: -1,
            message: format!("session bus connect: {e}"),
        })?;

        let proxy = Proxy::new(
            &zbus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.RemoteDesktop",
        )
        .map_err(|e| platform_msg(&format!("portal RemoteDesktop proxy: {e}")))?;

        // 1. CreateSession
        let session_handle = create_session(&zbus, &proxy)?;

        // 2. SelectDevices
        select_devices(&zbus, &proxy, &session_handle)?;

        // 3. Start (consent prompt the first time per persist_mode)
        start_session(&zbus, &proxy, &session_handle)?;

        // 4. ConnectToEIS — returns a UnixFd.
        let fd = connect_to_eis(&proxy, &session_handle)?;

        let stream = UnixStream::from(fd);
        let inner = init_ei(stream)?;

        Ok(Self {
            _zbus: Some(zbus),
            _session: Some(session_handle),
            inner: Mutex::new(inner),
        })
    }

    /// Test entry point: skip the portal entirely and run the EI handshake
    /// against a caller-supplied socket. Pair with a `reis::eis` server
    /// for in-process roundtrip tests.
    #[allow(dead_code)]
    pub(crate) fn from_eis_fd(fd: OwnedFd) -> Result<Self> {
        let stream = UnixStream::from(fd);
        let inner = init_ei(stream)?;
        Ok(Self {
            _zbus: None,
            _session: None,
            inner: Mutex::new(inner),
        })
    }

    pub(crate) fn pointer_move(&self, to: Point) -> Result<()> {
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.pointer_abs.motion_absolute(to.x as f32, to.y as f32);
            Ok(())
        })
    }

    pub(crate) fn pointer_down(&self, button: MouseButton) -> Result<()> {
        let code = button_evdev(button);
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.button_iface.button(code, ButtonState::Press);
            Ok(())
        })
    }

    pub(crate) fn pointer_up(&self, button: MouseButton) -> Result<()> {
        let code = button_evdev(button);
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.button_iface.button(code, ButtonState::Released);
            Ok(())
        })
    }

    pub(crate) fn pointer_click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let code = button_evdev(button);
        let mut s = self.lock();
        // Move first, then `count` press/release pairs. One frame per event
        // keeps the wire trace easy to reason about; libei only requires
        // at-most-one button request *for the same button* per frame.
        emit_frame(&mut s, |s, _ts| {
            s.pointer_abs.motion_absolute(at.x as f32, at.y as f32);
            Ok(())
        })?;
        for _ in 0..count {
            emit_frame(&mut s, |s, _ts| {
                s.button_iface.button(code, ButtonState::Press);
                Ok(())
            })?;
            emit_frame(&mut s, |s, _ts| {
                s.button_iface.button(code, ButtonState::Released);
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(crate) fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.pointer_abs.motion_absolute(at.x as f32, at.y as f32);
            Ok(())
        })?;
        // libei `scroll_discrete` units are 120 per detent, matching the
        // wire protocol for high-resolution wheels. ScrollDelta is in
        // ticks (one wheel notch each), so multiply.
        if delta.dx != 0 || delta.dy != 0 {
            emit_frame(&mut s, |s, _ts| {
                s.scroll_iface
                    .scroll_discrete(delta.dx * 120, delta.dy * 120);
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(crate) fn key_down(&self, key: &Key) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        let (xkb_kc, _shift) = self.lookup_keysym(keysym)?;
        let evdev = xkb_kc
            .checked_sub(XKB_TO_EVDEV_OFFSET)
            .ok_or_else(|| Error::Unsupported {
                feature: format!("xkb keycode {xkb_kc} below evdev range"),
            })?;
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.keyboard_iface.key(evdev, KeyState::Press);
            Ok(())
        })
    }

    pub(crate) fn key_up(&self, key: &Key) -> Result<()> {
        let keysym = key_to_keysym(key)?;
        let (xkb_kc, _shift) = self.lookup_keysym(keysym)?;
        let evdev = xkb_kc
            .checked_sub(XKB_TO_EVDEV_OFFSET)
            .ok_or_else(|| Error::Unsupported {
                feature: format!("xkb keycode {xkb_kc} below evdev range"),
            })?;
        let mut s = self.lock();
        emit_frame(&mut s, |s, _ts| {
            s.keyboard_iface.key(evdev, KeyState::Released);
            Ok(())
        })
    }

    pub(crate) fn type_text(&self, text: &str) -> Result<()> {
        // Mirror the X11 logic: per-char keysym → (keycode, needs_shift),
        // hold Shift for shifted-column keysyms.
        let shift_kc = {
            let s = self.lock();
            let (kc, _) = s
                .keymap
                .lookup(XK_SHIFT_L)
                .ok_or_else(|| Error::Unsupported {
                    feature: "no Shift_L in current xkb layout".into(),
                })?;
            kc.checked_sub(XKB_TO_EVDEV_OFFSET)
                .ok_or_else(|| Error::Unsupported {
                    feature: "xkb Shift_L keycode below evdev range".into(),
                })?
        };

        for c in text.chars() {
            let keysym = char_keysym(c);
            let (xkb_kc, needs_shift) =
                self.lookup_keysym(keysym).map_err(|_| Error::Unsupported {
                    feature: format!(
                        "character '{c}' (keysym 0x{keysym:04x}) has no keycode \
                         in the current xkb layout"
                    ),
                })?;
            let evdev =
                xkb_kc
                    .checked_sub(XKB_TO_EVDEV_OFFSET)
                    .ok_or_else(|| Error::Unsupported {
                        feature: format!("xkb keycode {xkb_kc} below evdev range"),
                    })?;
            let mut s = self.lock();
            if needs_shift {
                emit_frame(&mut s, |s, _ts| {
                    s.keyboard_iface.key(shift_kc, KeyState::Press);
                    Ok(())
                })?;
            }
            emit_frame(&mut s, |s, _ts| {
                s.keyboard_iface.key(evdev, KeyState::Press);
                Ok(())
            })?;
            emit_frame(&mut s, |s, _ts| {
                s.keyboard_iface.key(evdev, KeyState::Released);
                Ok(())
            })?;
            if needs_shift {
                emit_frame(&mut s, |s, _ts| {
                    s.keyboard_iface.key(shift_kc, KeyState::Released);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, EiState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lookup_keysym(&self, keysym: u32) -> Result<(u32, bool)> {
        let s = self.lock();
        s.keymap.lookup(keysym).ok_or_else(|| Error::Unsupported {
            feature: format!("keysym 0x{keysym:04x} not in current xkb keymap"),
        })
    }
}

/// Run an emission as a single libei frame: `start_emulating` →
/// `(emit caller's events)` → `frame` → `stop_emulating` → flush.
///
/// The state's `serial` and `sequence` are bumped each call, matching the
/// "transactional cookie" semantics of libei.
fn emit_frame<F>(s: &mut EiState, f: F) -> Result<()>
where
    F: FnOnce(&mut EiState, u64) -> Result<()>,
{
    s.sequence = s.sequence.wrapping_add(1);
    let ts = monotonic_ts();
    s.pointer.start_emulating(s.serial, s.sequence);
    s.keyboard.start_emulating(s.serial, s.sequence);
    f(s, ts)?;
    s.pointer.frame(s.serial, ts);
    s.keyboard.frame(s.serial, ts);
    s.pointer.stop_emulating(s.serial);
    s.keyboard.stop_emulating(s.serial);
    s.serial = s.serial.wrapping_add(1);
    s.ctx
        .flush()
        .map_err(|e| platform_msg(&format!("ei flush: {e}")))?;
    Ok(())
}

/// Read the keymap announced by the EIS server. The fd refers to a memfd
/// or shm file; the bytes are an XKB keymap in `KEYMAP_FORMAT_TEXT_V1`
/// (the only format libei supports today). Strip a trailing NUL — both
/// libxkbcommon and libei accept null-terminated keymaps but the Rust
/// `xkb::Keymap::new_from_string` rejects embedded NULs.
fn read_keymap(km: &event::Keymap) -> Result<String> {
    let fd = km
        .fd
        .try_clone()
        .map_err(|e| platform_msg(&format!("dup keymap fd: {e}")))?;
    let mut file = std::fs::File::from(fd);
    let mut buf = vec![0u8; km.size as usize];
    file.read_exact(&mut buf)
        .map_err(|e| platform_msg(&format!("read keymap fd: {e}")))?;
    if buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|e| platform_msg(&format!("keymap is not UTF-8: {e}")))
}

fn monotonic_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn button_evdev(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Middle => BTN_MIDDLE,
    }
}

// ─── EI handshake + device discovery ───────────────────────────────────

/// Run the EI handshake, bind capabilities on the first seat, drive
/// device-added events until we have keyboard, pointer-absolute, button,
/// and scroll devices in the Resumed state, and capture the keyboard's
/// keymap.
fn init_ei(stream: UnixStream) -> Result<EiState> {
    let ctx =
        ei::Context::new(stream).map_err(|e| platform_msg(&format!("ei::Context::new: {e}")))?;
    let resp = reis::handshake::ei_handshake_blocking(&ctx, "xa11y", ContextType::Sender)
        .map_err(|e| platform_msg(&format!("ei handshake: {e}")))?;
    let conn = resp.connection.clone();
    // EiEventConverter takes ownership of the HandshakeResp so it can
    // wire up object identities for subsequent typed events.
    let mut converter = EiEventConverter::new(&ctx, resp);
    ctx.flush()
        .map_err(|e| platform_msg(&format!("ei flush after handshake: {e}")))?;

    let mut pointer: Option<ei::Device> = None;
    let mut pointer_abs: Option<ei::PointerAbsolute> = None;
    let mut keyboard: Option<ei::Device> = None;
    let mut keyboard_iface: Option<ei::Keyboard> = None;
    let mut button_iface: Option<ei::Button> = None;
    let mut scroll_iface: Option<ei::Scroll> = None;
    let mut keymap_str: Option<String> = None;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if pointer.is_some()
            && pointer_abs.is_some()
            && keyboard.is_some()
            && keyboard_iface.is_some()
            && button_iface.is_some()
            && scroll_iface.is_some()
            && keymap_str.is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(platform_msg(
                "timed out waiting for EIS to advertise pointer + keyboard devices",
            ));
        }

        ctx.read()
            .map_err(|e| platform_msg(&format!("ei read: {e}")))?;
        while let Some(pending) = ctx.pending_event() {
            let raw = match pending {
                PendingRequestResult::Request(r) => r,
                PendingRequestResult::ParseError(e) => {
                    return Err(platform_msg(&format!("ei parse error: {e:?}")));
                }
                PendingRequestResult::InvalidObject(_id) => continue,
            };
            // EiEventConverter consumes the wire-level event and emits
            // zero or more high-level `EiEvent`s for us to match on.
            converter
                .handle_event(raw)
                .map_err(|e| platform_msg(&format!("ei event convert: {e:?}")))?;
        }
        while let Some(ev) = converter.next_event() {
            match ev {
                EiEvent::SeatAdded(s) => {
                    s.seat.bind_capabilities(&[
                        DeviceCapability::PointerAbsolute,
                        DeviceCapability::Keyboard,
                        DeviceCapability::Button,
                        DeviceCapability::Scroll,
                    ]);
                    ctx.flush()
                        .map_err(|e| platform_msg(&format!("ei flush: {e}")))?;
                }
                EiEvent::DeviceAdded(d) => {
                    let dev = &d.device;
                    if dev.has_capability(DeviceCapability::PointerAbsolute) {
                        pointer = Some(dev.device().clone());
                        if let Some(pa) = dev.interface::<ei::PointerAbsolute>() {
                            pointer_abs = Some(pa);
                        }
                    }
                    if dev.has_capability(DeviceCapability::Keyboard) {
                        keyboard = Some(dev.device().clone());
                        if let Some(k) = dev.interface::<ei::Keyboard>() {
                            keyboard_iface = Some(k);
                        }
                        if let Some(km) = dev.keymap() {
                            keymap_str = Some(read_keymap(km)?);
                        }
                    }
                    if dev.has_capability(DeviceCapability::Button) {
                        if let Some(b) = dev.interface::<ei::Button>() {
                            button_iface = Some(b);
                        }
                    }
                    if dev.has_capability(DeviceCapability::Scroll) {
                        if let Some(sc) = dev.interface::<ei::Scroll>() {
                            scroll_iface = Some(sc);
                        }
                    }
                }
                EiEvent::DeviceResumed(_) | EiEvent::DevicePaused(_) => {
                    // Nothing to do — we don't track per-device suspend
                    // state today. emit_frame's start_emulating /
                    // stop_emulating bracket each frame.
                }
                EiEvent::Disconnected(reason) => {
                    return Err(platform_msg(&format!(
                        "EIS disconnected during setup: {reason:?}"
                    )));
                }
                _ => {}
            }
        }
    }

    let keymap_str = keymap_str
        .ok_or_else(|| platform_msg("EIS did not announce a keymap for the keyboard device"))?;
    let xkb_ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &xkb_ctx,
        keymap_str,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| platform_msg("xkb_keymap_new_from_string returned None"))?;
    let reverse = ReverseKeymap::from_xkb(&keymap);

    Ok(EiState {
        ctx,
        _conn: conn,
        pointer: pointer.unwrap(),
        pointer_abs: pointer_abs.unwrap(),
        keyboard: keyboard.unwrap(),
        keyboard_iface: keyboard_iface.unwrap(),
        button_iface: button_iface.unwrap(),
        scroll_iface: scroll_iface.unwrap(),
        keymap: reverse,
        serial: 1,
        sequence: 0,
    })
}

// ─── Portal RPC helpers ────────────────────────────────────────────────

/// Random handle-token suffix; the portal docs require this to be a unique
/// alphanumeric string per request so the Request object path is
/// predictable. We use a process-counter so concurrent xa11y processes on
/// the same bus don't collide.
fn handle_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("xa11y_{}_{n}", std::process::id())
}

fn await_response(
    zbus: &ZbusConnection,
    request_path: &OwnedObjectPath,
) -> Result<HashMap<String, OwnedValue>> {
    let request = Proxy::new(
        zbus,
        "org.freedesktop.portal.Desktop",
        request_path,
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| platform_msg(&format!("portal Request proxy: {e}")))?;

    let mut signals = request
        .receive_signal("Response")
        .map_err(|e| platform_msg(&format!("portal receive_signal: {e}")))?;
    let msg = signals
        .next()
        .ok_or_else(|| platform_msg("portal Response signal channel closed before reply"))?;
    let (response, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| platform_msg(&format!("portal Response deserialize: {e}")))?;
    if response != 0 {
        return Err(Error::PermissionDenied {
            instructions: format!("xdg-desktop-portal RemoteDesktop denied (response={response})"),
        });
    }
    Ok(results)
}

fn create_session(zbus: &ZbusConnection, proxy: &Proxy<'_>) -> Result<OwnedObjectPath> {
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("session_handle_token", Value::from(handle_token()));
    options.insert("handle_token", Value::from(handle_token()));
    let request_path: OwnedObjectPath = proxy
        .call("CreateSession", &(options,))
        .map_err(|e| platform_msg(&format!("CreateSession call: {e}")))?;
    let results = await_response(zbus, &request_path)?;
    let session_handle: String = results
        .get("session_handle")
        .ok_or_else(|| platform_msg("CreateSession response missing session_handle"))?
        .downcast_ref::<String>()
        .map_err(|_| platform_msg("session_handle not a string"))?;
    OwnedObjectPath::try_from(session_handle.as_str())
        .map_err(|e| platform_msg(&format!("session_handle not a path: {e}")))
}

fn select_devices(
    zbus: &ZbusConnection,
    proxy: &Proxy<'_>,
    session: &OwnedObjectPath,
) -> Result<()> {
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(handle_token()));
    options.insert("types", Value::from(RD_TYPE_KEYBOARD | RD_TYPE_POINTER));
    options.insert("persist_mode", Value::from(RD_PERSIST_PERMANENT));
    let request_path: OwnedObjectPath = proxy
        .call("SelectDevices", &(session, options))
        .map_err(|e| platform_msg(&format!("SelectDevices call: {e}")))?;
    let _ = await_response(zbus, &request_path)?;
    Ok(())
}

fn start_session(
    zbus: &ZbusConnection,
    proxy: &Proxy<'_>,
    session: &OwnedObjectPath,
) -> Result<()> {
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(handle_token()));
    let parent_window = ""; // headless / test use
    let request_path: OwnedObjectPath = proxy
        .call("Start", &(session, parent_window, options))
        .map_err(|e| platform_msg(&format!("Start call: {e}")))?;
    let _ = await_response(zbus, &request_path)?;
    Ok(())
}

fn connect_to_eis(proxy: &Proxy<'_>, session: &OwnedObjectPath) -> Result<OwnedFd> {
    let options: HashMap<&str, Value> = HashMap::new();
    let fd: zbus::zvariant::OwnedFd = proxy
        .call("ConnectToEIS", &(session, options))
        .map_err(|e| platform_msg(&format!("ConnectToEIS call: {e}")))?;
    // SAFETY: zbus::OwnedFd owns the descriptor; converting to std OwnedFd
    // is a transfer of that ownership, no double-close.
    let raw = fd.as_raw_fd();
    let dup = unsafe { libc_dup(raw)? };
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// Duplicate the FD so the zbus owner can drop without closing the EI
/// socket. We don't pull in `libc` for this; just call the syscall via
/// `nix`-free std (UnixStream::try_clone after a from_raw_fd round-trip
/// doesn't apply here because we want a raw FD for ei::Context::new).
unsafe fn libc_dup(fd: i32) -> Result<i32> {
    // Use the dup syscall via std's File. Wrap fd as ManuallyDrop to keep
    // ownership with zbus::OwnedFd, then dup explicitly.
    use std::os::fd::BorrowedFd;
    let borrowed = BorrowedFd::borrow_raw(fd);
    let owned = borrowed
        .try_clone_to_owned()
        .map_err(|e| platform_msg(&format!("dup EI fd: {e}")))?;
    let raw = owned.as_raw_fd();
    // Forget the OwnedFd so the caller's `from_raw_fd` is the sole owner.
    std::mem::forget(owned);
    Ok(raw)
}

/// Convenience for the upper input.rs to use without importing `platform`.
#[allow(dead_code)]
fn _platform_used_by_upper(e: impl std::fmt::Display) -> Error {
    platform(e)
}

// ─── In-process unit tests ─────────────────────────────────────────────
//
// Lives inside the source file so we can exercise crate-private items
// (`button_evdev`, `ReverseKeymap`, `handle_token`) without widening their
// visibility. The exhaustive end-to-end libei test is in
// `tests/wayland_input_e2e.rs`, gated `#[ignore]` to require a real GNOME
// portal stack — run via `scripts/run_wayland_libei.sh`.
#[cfg(test)]
mod tests {
    use super::*;
    use xa11y_core::input::Key;

    /// Build a default US keymap (rules=evdev, model=pc105, layout=us)
    /// without going through libei. xkbcommon resolves the empty strings
    /// to its compile-time defaults.
    fn default_keymap() -> xkb::Keymap {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .expect("US keymap should compile from xkbcommon defaults")
    }

    #[test]
    fn reverse_keymap_maps_lowercase_letter() {
        let keymap = default_keymap();
        let reverse = ReverseKeymap::from_xkb(&keymap);
        // 'a' is keysym 0x61, US-layout AC01, should be unshifted.
        let (_kc, needs_shift) = reverse
            .lookup(char_keysym('a'))
            .expect("'a' must be in a US keymap");
        assert!(!needs_shift, "'a' should be on the unshifted level");
    }

    #[test]
    fn reverse_keymap_maps_uppercase_letter_to_same_keycode_with_shift() {
        let keymap = default_keymap();
        let reverse = ReverseKeymap::from_xkb(&keymap);
        let lower = reverse.lookup(char_keysym('a')).expect("'a'");
        let upper = reverse.lookup(char_keysym('A')).expect("'A'");
        assert_eq!(
            lower.0, upper.0,
            "'a' and 'A' must share a keycode in a US keymap"
        );
        assert!(!lower.1);
        assert!(upper.1, "'A' must require shift");
    }

    #[test]
    fn reverse_keymap_resolves_named_keys() {
        let keymap = default_keymap();
        let reverse = ReverseKeymap::from_xkb(&keymap);
        // Each keysym we resolve via `key_to_keysym(&Key::…)` should be in
        // the keymap. If any of these miss, the type_text and key_down
        // paths would fail at runtime with Unsupported.
        for key in [
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Backspace,
            Key::Space,
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
        let keymap = default_keymap();
        let reverse = ReverseKeymap::from_xkb(&keymap);
        let (kc, _) = reverse
            .lookup(XK_SHIFT_L)
            .expect("Shift_L must be in any keymap we'd encounter");
        assert!(kc >= XKB_TO_EVDEV_OFFSET, "Shift_L keycode is in xkb range");
    }

    #[test]
    fn button_evdev_uses_input_event_codes() {
        assert_eq!(button_evdev(MouseButton::Left), 0x110);
        assert_eq!(button_evdev(MouseButton::Right), 0x111);
        assert_eq!(button_evdev(MouseButton::Middle), 0x112);
    }

    #[test]
    fn handle_token_is_unique_per_call() {
        let a = handle_token();
        let b = handle_token();
        assert_ne!(a, b, "handle_token must increment per call");
        // Token must be alphanumeric + underscore (xdg-desktop-portal
        // requires the handle_token suffix to fit a D-Bus object path).
        for c in a.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '_',
                "handle_token must be path-safe, got {a:?}"
            );
        }
    }
}
