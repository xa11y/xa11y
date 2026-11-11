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
//! Opening `/dev/uinput` for writing requires root, membership in the
//! `input` group, or a udev rule that grants the user access. Granting the
//! user the `input` group also grants global keystroke read access via
//! `/dev/input/event*` — the same threat surface as macOS "Input
//! Monitoring" or any X11 client connecting to `$DISPLAY`. xa11y surfaces
//! an actionable [`Error::PermissionDenied`] when the open fails with
//! `EACCES`.
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
//! 0..=`COORD_MAX` range, which compositors (wlroots, KWin, Mutter) map
//! onto the bounding box of their output layout in logical coordinates.
//! xa11y's `pointer_move((x, y))` takes a point in that same logical layout,
//! so it is translated through the layout's bounding box, and a target is
//! validated against the individual outputs.
//!
//! The layout is read from the compositor on every validated gesture: each
//! output's `xdg-output` logical position and size. `XA11Y_SCREEN_WIDTH` /
//! `XA11Y_SCREEN_HEIGHT` are an explicit override, for a session with no
//! compositor to ask (a bare console, or `XA11Y_LINUX_INPUT_BACKEND=wayland`
//! on X11) or one whose absolute mapping differs; they describe a single
//! `width x height` screen at the origin. An override that is set but is not
//! a positive integer is an [`Error::InvalidConfig`], and a session with no
//! override and no usable compositor answer is an error that says to set
//! them. There is no assumed default size.

use std::sync::Mutex;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    PropType, RelativeAxisCode, UinputAbsSetup,
};
use xkbcommon::xkb;

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use xa11y_core::input::{
    note_display_source, validate_pointer_regions, Key as XaKey, MouseButton, Point, ScrollDelta,
};
use xa11y_core::{Error, Rect, Result};

use crate::input::{key_to_keysym, plan_keystrokes, MULTI_CLICK_GAP};

/// Virtual coordinate range for the absolute pointer. Compositors scale
/// this onto the active virtual screen — same convention used by graphics
/// tablets and remote-desktop clients.
const COORD_MAX: i32 = 32767;

/// The override variables for the pointer extent.
const SCREEN_WIDTH_VAR: &str = "XA11Y_SCREEN_WIDTH";
const SCREEN_HEIGHT_VAR: &str = "XA11Y_SCREEN_HEIGHT";

pub(crate) struct WaylandInputBackend {
    inner: Mutex<UinputState>,
    /// The layout of the latest validated gesture. Moves inside a gesture
    /// (a drag's interpolation, a captured move with a button held) map
    /// through it rather than re-reading the compositor at 60 Hz.
    extent: Mutex<Option<PointerExtent>>,
}

/// The logical output layout the absolute pointer range spans.
#[derive(Debug, Clone, PartialEq)]
struct PointerExtent {
    /// Bounding box of every output: what 0..=`COORD_MAX` is mapped onto.
    bounds: Rect,
    /// The outputs themselves: a target must lie on one of them.
    outputs: Vec<Rect>,
    /// Where the layout came from, for a refusal's diagnosis.
    source: String,
}

impl PointerExtent {
    fn new(outputs: Vec<Rect>, source: String) -> Result<Self> {
        let bounds = bounding_box(&outputs).ok_or_else(|| Error::Platform {
            code: -1,
            message: format!(
                "{source} describes no output, so the uinput pointer has no extent; \
                 set {SCREEN_WIDTH_VAR} and {SCREEN_HEIGHT_VAR}"
            ),
        })?;
        Ok(Self {
            bounds,
            outputs,
            source,
        })
    }

    /// Map a logical layout coordinate onto the device's 0..=`COORD_MAX`
    /// range along one axis.
    fn map_axis(value: i32, origin: i32, size: u32) -> i32 {
        let size = i64::from(size.max(1));
        let offset = i64::from(value) - i64::from(origin);
        (offset * i64::from(COORD_MAX) / size).clamp(0, i64::from(COORD_MAX)) as i32
    }

    fn map(&self, p: Point) -> (i32, i32) {
        (
            Self::map_axis(p.x, self.bounds.x, self.bounds.width),
            Self::map_axis(p.y, self.bounds.y, self.bounds.height),
        )
    }
}

/// The smallest rectangle holding every output; `None` for no outputs.
fn bounding_box(outputs: &[Rect]) -> Option<Rect> {
    let first = outputs.first()?;
    let (mut left, mut top) = (i64::from(first.x), i64::from(first.y));
    let (mut right, mut bottom) = (left, top);
    for r in outputs {
        left = left.min(i64::from(r.x));
        top = top.min(i64::from(r.y));
        right = right.max(i64::from(r.x) + i64::from(r.width));
        bottom = bottom.max(i64::from(r.y) + i64::from(r.height));
    }
    Some(Rect {
        x: i32::try_from(left).ok()?,
        y: i32::try_from(top).ok()?,
        width: u32::try_from(right - left).ok()?,
        height: u32::try_from(bottom - top).ok()?,
    })
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
    /// Re-read the layout and check `checked` against its outputs. Called at
    /// the start of every pointer gesture, so a hotplugged or rearranged
    /// output is seen by the next one.
    pub(crate) fn validate_pointer_target(&self, requested: Point, checked: Point) -> Result<()> {
        let extent = detect_extent()?;
        let result = validate_extent(requested, checked, &extent);
        *self.extent.lock().unwrap_or_else(|e| e.into_inner()) = Some(extent);
        result
    }

    /// Map a point through the latest validated layout, reading it when no
    /// gesture has validated one yet.
    fn map(&self, p: Point) -> Result<(i32, i32)> {
        let mut extent = self.extent.lock().unwrap_or_else(|e| e.into_inner());
        if extent.is_none() {
            *extent = Some(detect_extent()?);
        }
        match extent.as_ref() {
            Some(extent) => Ok(extent.map(p)),
            // Unreachable: populated just above.
            None => Err(Error::Platform {
                code: -1,
                message: "uinput pointer extent missing after detection".into(),
            }),
        }
    }

    pub(crate) fn new() -> Result<Self> {
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
            extent: Mutex::new(None),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, UinputState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn pointer_move(&self, to: Point) -> Result<()> {
        let (x, y) = self.map(to)?;
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
        let (x, y) = self.map(at)?;
        let key = button_key(button);
        let mut s = self.lock();
        let press = InputEvent::new(EventType::KEY.0, key.0, 1);
        let release = InputEvent::new(EventType::KEY.0, key.0, 0);
        // Move + first click as one frame so the compositor sees the
        // position and the button-down on the same input report.
        emit(
            &mut s.device,
            &[
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
                press,
                release,
            ],
        )?;
        // Each further click is its own frame, after the same gap as on X11.
        // In one frame, every press and release shares one timestamp, and
        // the compositor sees one report of the button's final state.
        for _ in 1..count {
            std::thread::sleep(MULTI_CLICK_GAP);
            emit(&mut s.device, &[press, release])?;
        }
        Ok(())
    }

    pub(crate) fn pointer_scroll(&self, at: Point, delta: ScrollDelta) -> Result<()> {
        let (x, y) = self.map(at)?;
        let mut s = self.lock();
        // REL_WHEEL: negative = down, positive = up. ScrollDelta uses
        // "positive dy = scroll toward the end" (wheel rolled toward the
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
        // Resolve the whole string before emitting anything, so an
        // untypeable character fails the call with nothing typed.
        let frames = typing_frames(&s.keymap, text)?;
        for frame in frames {
            emit(&mut s.device, &frame)?;
        }
        Ok(())
    }
}

/// The evdev frames that type `text`, one per character (Shift press, key
/// press, key release, Shift release). Fails without producing any frame if
/// one character is missing from the keymap; see
/// [`crate::input::plan_keystrokes`].
fn typing_frames(keymap: &ReverseKeymap, text: &str) -> Result<Vec<Vec<InputEvent>>> {
    let plan = plan_keystrokes(text, "xkb keyboard layout", |keysym| keymap.lookup(keysym))?;
    Ok(plan
        .into_iter()
        .map(|(kc, shift)| {
            let mut frame = Vec::with_capacity(4);
            if let Some(shift) = shift {
                frame.push(InputEvent::new(EventType::KEY.0, shift, 1));
            }
            frame.push(InputEvent::new(EventType::KEY.0, kc, 1));
            frame.push(InputEvent::new(EventType::KEY.0, kc, 0));
            if let Some(shift) = shift {
                frame.push(InputEvent::new(EventType::KEY.0, shift, 0));
            }
            frame
        })
        .collect())
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
            instructions: "open /dev/uinput: permission denied. Wayland input simulation \
                           writes to /dev/uinput, which needs either membership in the `input` \
                           group (sudo usermod -aG input $USER, then log in again) or a udev \
                           rule granting the user access, e.g. \
                           KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\", \
                           OPTIONS+=\"static_node=uinput\"."
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

fn validate_extent(requested: Point, checked: Point, extent: &PointerExtent) -> Result<()> {
    validate_pointer_regions(
        requested,
        checked,
        "wayland logical output layout",
        &extent.outputs,
    )
    .map_err(|error| note_display_source(error, &extent.source))
}

/// One override variable: `None` when unset, an error naming the variable
/// and its value when set to anything but a positive integer.
fn screen_dimension(name: &str, value: Option<std::ffi::OsString>) -> Result<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.to_string_lossy();
    match text.trim().parse::<u32>() {
        Ok(n) if n > 0 && i32::try_from(n).is_ok() => Ok(Some(n)),
        _ => Err(Error::InvalidConfig {
            message: format!(
                "{name}={text:?} is not a positive integer number of logical pixels; \
                 unset it to read the screen size from the compositor"
            ),
        }),
    }
}

/// The override, when both variables are set; an error when only one is.
fn extent_override(
    width: Option<std::ffi::OsString>,
    height: Option<std::ffi::OsString>,
) -> Result<Option<PointerExtent>> {
    match (
        screen_dimension(SCREEN_WIDTH_VAR, width)?,
        screen_dimension(SCREEN_HEIGHT_VAR, height)?,
    ) {
        (None, None) => Ok(None),
        (Some(width), Some(height)) => PointerExtent::new(
            vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
            format!("{SCREEN_WIDTH_VAR}/{SCREEN_HEIGHT_VAR} override ({width}x{height})"),
        )
        .map(Some),
        (Some(_), None) | (None, Some(_)) => Err(Error::InvalidConfig {
            message: format!(
                "{SCREEN_WIDTH_VAR} and {SCREEN_HEIGHT_VAR} override the screen size together; \
                 set both or neither"
            ),
        }),
    }
}

/// The pointer extent: the override when set, otherwise the compositor's
/// output layout. Never an assumed size.
fn detect_extent() -> Result<PointerExtent> {
    if let Some(extent) = extent_override(
        std::env::var_os(SCREEN_WIDTH_VAR),
        std::env::var_os(SCREEN_HEIGHT_VAR),
    )? {
        return Ok(extent);
    }
    let conn = Connection::connect_to_env().map_err(|e| Error::Platform {
        code: -1,
        message: format!(
            "the uinput pointer needs the screen size, and there is no Wayland compositor \
             to read it from ({e}); set {SCREEN_WIDTH_VAR} and {SCREEN_HEIGHT_VAR}"
        ),
    })?;
    compositor_extent(conn)
}

/// Output geometry gathered from `xdg-output`, one slot per `wl_output`.
#[derive(Default)]
struct LayoutQuery {
    outputs: Vec<wl_output::WlOutput>,
    xdg_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    xdg_outputs: Vec<zxdg_output_v1::ZxdgOutputV1>,
    positions: Vec<Option<(i32, i32)>>,
    sizes: Vec<Option<(i32, i32)>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for LayoutQuery {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" => {
                    let output = registry.bind::<wl_output::WlOutput, (), Self>(
                        name,
                        version.min(2),
                        qh,
                        (),
                    );
                    state.outputs.push(output);
                }
                "zxdg_output_manager_v1" => {
                    state.xdg_manager = Some(
                        registry.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, (), Self>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        ),
                    );
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for LayoutQuery {
    fn event(
        _state: &mut Self,
        _output: &wl_output::WlOutput,
        _event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Geometry comes from xdg-output, which reports it in the logical
        // space the compositor maps absolute pointer motion through.
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for LayoutQuery {
    fn event(
        _state: &mut Self,
        _mgr: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _event: zxdg_output_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // zxdg_output_manager_v1 has no events.
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for LayoutQuery {
    fn event(
        state: &mut Self,
        _xdg_output: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                state.positions[*index] = Some((x, y));
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                state.sizes[*index] = Some((width, height));
            }
            _ => {}
        }
    }
}

/// Read every output's logical rectangle from the compositor. Any output
/// the compositor does not fully describe fails the read rather than being
/// skipped, since a missing output would refuse targets on it.
fn compositor_extent(conn: Connection) -> Result<PointerExtent> {
    let fail = |what: String| Error::Platform {
        code: -1,
        message: format!(
            "{what}, so the uinput pointer extent is unknown; set {SCREEN_WIDTH_VAR} and \
             {SCREEN_HEIGHT_VAR}"
        ),
    };
    let display = conn.display();
    let mut queue = conn.new_event_queue::<LayoutQuery>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut state = LayoutQuery::default();
    queue
        .roundtrip(&mut state)
        .map_err(|e| fail(format!("reading the Wayland registry failed ({e})")))?;
    let Some(mgr) = state.xdg_manager.clone() else {
        return Err(fail(
            "the Wayland compositor does not offer xdg-output (zxdg_output_manager_v1)".into(),
        ));
    };
    if state.outputs.is_empty() {
        return Err(fail("the Wayland compositor reports no outputs".into()));
    }
    state.positions = vec![None; state.outputs.len()];
    state.sizes = vec![None; state.outputs.len()];
    for (index, output) in state.outputs.clone().into_iter().enumerate() {
        let xdg_output = mgr.get_xdg_output(&output, &qh, index);
        state.xdg_outputs.push(xdg_output);
    }
    queue
        .roundtrip(&mut state)
        .map_err(|e| fail(format!("reading xdg-output geometry failed ({e})")))?;
    let mut rects = Vec::with_capacity(state.outputs.len());
    for (index, (position, size)) in state.positions.iter().zip(&state.sizes).enumerate() {
        match (position, size) {
            (Some((x, y)), Some((width, height))) if *width > 0 && *height > 0 => {
                rects.push(Rect {
                    x: *x,
                    y: *y,
                    width: width.unsigned_abs(),
                    height: height.unsigned_abs(),
                });
            }
            _ => {
                return Err(fail(format!(
                    "xdg-output gave output {index} no usable logical geometry \
                     (position {position:?}, size {size:?})"
                )));
            }
        }
    }
    PointerExtent::new(
        rects,
        format!(
            "Wayland compositor outputs (xdg-output logical layout, {} output(s))",
            state.outputs.len()
        ),
    )
}

// ─── Unit tests ───────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{char_keysym, XK_SHIFT_L};
    use xa11y_core::input::Key as XaKey;

    fn os(value: &str) -> Option<std::ffi::OsString> {
        Some(value.into())
    }

    fn override_extent(width: &str, height: &str) -> PointerExtent {
        extent_override(os(width), os(height))
            .expect("valid override")
            .expect("override set")
    }

    #[test]
    fn override_extent_accepts_the_last_pixel_without_clamping() {
        let point = Point::new(9, 4);
        assert!(validate_extent(point, point, &override_extent("10", "5")).is_ok());
    }

    #[test]
    fn malformed_or_nonpositive_override_names_the_variable_and_value() {
        for (width, height, bad) in [
            ("0", "5", "XA11Y_SCREEN_WIDTH=\"0\""),
            ("-1", "5", "XA11Y_SCREEN_WIDTH=\"-1\""),
            ("10", "abc", "XA11Y_SCREEN_HEIGHT=\"abc\""),
            ("10", "", "XA11Y_SCREEN_HEIGHT=\"\""),
            ("1.5", "5", "XA11Y_SCREEN_WIDTH=\"1.5\""),
            ("3000000000", "5", "XA11Y_SCREEN_WIDTH=\"3000000000\""),
        ] {
            let error = extent_override(os(width), os(height)).expect_err("bad override");
            assert!(matches!(error, Error::InvalidConfig { .. }), "{error:?}");
            assert!(error.to_string().contains(bad), "{error}");
        }
    }

    #[test]
    fn half_an_override_is_an_error_and_none_defers_to_the_compositor() {
        let error = extent_override(os("10"), None).expect_err("width only");
        assert!(matches!(error, Error::InvalidConfig { .. }), "{error:?}");
        assert!(extent_override(None, None).unwrap().is_none());
    }

    #[test]
    fn override_extent_rejects_negative_and_exclusive_edges_with_geometry() {
        let extent = override_extent("10", "5");
        for point in [Point::new(-1, 4), Point::new(10, 4), Point::new(9, 5)] {
            let error = validate_extent(point, point, &extent).expect_err("outside extent");
            assert!(
                matches!(error, Error::InvalidActionData { .. }),
                "{error:?}"
            );
            assert_eq!(error.delivery(), xa11y_core::Delivery::NotDelivered);
            let target = error
                .diagnosis()
                .expect("diagnosis")
                .pointer_target
                .as_ref()
                .expect("geometry");
            assert_eq!(target.point, point);
            assert_eq!(target.checked_point, point);
            assert_eq!(target.coordinate_space, "wayland logical output layout");
            assert_eq!(
                target.display_source.as_deref(),
                Some("XA11Y_SCREEN_WIDTH/XA11Y_SCREEN_HEIGHT override (10x5)")
            );
            assert_eq!(
                target.displays,
                [Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 5
                }]
            );
        }
    }

    #[test]
    fn layout_maps_through_its_bounding_box_and_checks_each_output() {
        // A 1920x1080 output at the origin and a 1280x1024 one to its left,
        // 100 px lower: a gap above the left output is not on a display.
        let extent = PointerExtent::new(
            vec![
                Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                Rect {
                    x: -1280,
                    y: 100,
                    width: 1280,
                    height: 1024,
                },
            ],
            "test".into(),
        )
        .unwrap();
        assert_eq!(
            extent.bounds,
            Rect {
                x: -1280,
                y: 0,
                width: 3200,
                height: 1124,
            }
        );
        assert_eq!(extent.map(Point::new(-1280, 0)), (0, 0));
        assert_eq!(
            extent.map(Point::new(320, 562)),
            (1600 * COORD_MAX / 3200, 562 * COORD_MAX / 1124)
        );
        let gap = Point::new(-10, 50);
        assert!(validate_extent(gap, gap, &extent).is_err());
        let left = Point::new(-10, 150);
        assert!(validate_extent(left, left, &extent).is_ok());
    }

    #[test]
    fn an_empty_layout_is_a_platform_error_that_names_the_override() {
        let error = PointerExtent::new(Vec::new(), "compositor".into()).unwrap_err();
        assert!(matches!(error, Error::Platform { .. }), "{error:?}");
        assert!(error.to_string().contains("XA11Y_SCREEN_WIDTH"), "{error}");
    }

    fn default_keymap() -> xkb::Keymap {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .expect("US keymap should compile from xkbcommon defaults")
    }

    #[test]
    fn typing_frames_reject_an_unmapped_character_with_no_frames() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        assert_eq!(
            typing_frames(&reverse, "aB")
                .expect("US letters type")
                .len(),
            2
        );
        let err = typing_frames(&reverse, "ab\u{65e5}c")
            .expect_err("a CJK character is not in the US layout");
        let msg = err.to_string();
        assert!(
            msg.contains("index 2") && msg.contains("nothing was typed"),
            "{msg}"
        );
    }

    #[test]
    fn line_breaks_type_key_enter() {
        let reverse = ReverseKeymap::from_xkb(&default_keymap());
        let enter = KeyCode::KEY_ENTER.code();
        for text in ["\n", "\r", "\r\n"] {
            let frames = typing_frames(&reverse, text).expect("a line break types");
            assert_eq!(frames.len(), 1, "{text:?} is one key press");
            let codes: Vec<u16> = frames[0].iter().map(|e| e.code()).collect();
            assert_eq!(codes, vec![enter, enter], "{text:?}");
        }
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
    fn override_describes_one_screen_at_the_origin() {
        let extent = override_extent("2560", "1440");
        assert_eq!(
            extent.outputs,
            [Rect {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440
            }]
        );
        assert_eq!(extent.bounds, extent.outputs[0]);
        assert_eq!(extent.map(Point::new(1280, 720)), (16383, 16383));
    }
}

/// `compositor_extent` against a hermetic mock compositor (no real Wayland
/// session): the real registry walk, binds, `get_xdg_output` and roundtrips.
#[cfg(test)]
mod compositor_mock_tests {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use wayland_client::Connection;
    use wayland_protocols::xdg::xdg_output::zv1::server::{
        zxdg_output_manager_v1::{self, ZxdgOutputManagerV1},
        zxdg_output_v1::{self, ZxdgOutputV1},
    };
    use wayland_server::protocol::wl_output::{self, WlOutput};
    use wayland_server::{
        backend::ClientData, Dispatch, Display, DisplayHandle, GlobalDispatch, Resource,
    };
    use xa11y_core::{Error, Rect};

    /// One output: logical position, and size (`None` sends none).
    type MockOutput = (i32, i32, Option<(i32, i32)>);

    /// Each output global's index selects its logical rectangle; `None`
    /// sends no logical size for that output.
    struct MockServer {
        layout: Vec<MockOutput>,
    }
    struct ClientState;
    impl ClientData for ClientState {}

    impl GlobalDispatch<WlOutput, usize> for MockServer {
        fn bind(
            _state: &mut Self,
            _handle: &DisplayHandle,
            _client: &wayland_server::Client,
            resource: wayland_server::New<WlOutput>,
            index: &usize,
            data_init: &mut wayland_server::DataInit<'_, Self>,
        ) {
            data_init.init(resource, *index);
        }
    }
    impl Dispatch<WlOutput, usize> for MockServer {
        fn request(
            _state: &mut Self,
            _client: &wayland_server::Client,
            _resource: &WlOutput,
            _request: wl_output::Request,
            _data: &usize,
            _dh: &DisplayHandle,
            _init: &mut wayland_server::DataInit<'_, Self>,
        ) {
        }
    }
    impl GlobalDispatch<ZxdgOutputManagerV1, ()> for MockServer {
        fn bind(
            _state: &mut Self,
            _handle: &DisplayHandle,
            _client: &wayland_server::Client,
            resource: wayland_server::New<ZxdgOutputManagerV1>,
            _global_data: &(),
            data_init: &mut wayland_server::DataInit<'_, Self>,
        ) {
            data_init.init(resource, ());
        }
    }
    impl Dispatch<ZxdgOutputManagerV1, ()> for MockServer {
        fn request(
            state: &mut Self,
            _client: &wayland_server::Client,
            _resource: &ZxdgOutputManagerV1,
            request: zxdg_output_manager_v1::Request,
            _data: &(),
            _dh: &DisplayHandle,
            data_init: &mut wayland_server::DataInit<'_, Self>,
        ) {
            if let zxdg_output_manager_v1::Request::GetXdgOutput { id, output } = request {
                let index = *output.data::<usize>().expect("output index");
                let xdg = data_init.init(id, ());
                let (x, y, size) = state.layout[index];
                xdg.logical_position(x, y);
                if let Some((width, height)) = size {
                    xdg.logical_size(width, height);
                }
                xdg.done();
            }
        }
    }
    impl Dispatch<ZxdgOutputV1, ()> for MockServer {
        fn request(
            _state: &mut Self,
            _client: &wayland_server::Client,
            _resource: &ZxdgOutputV1,
            _request: zxdg_output_v1::Request,
            _data: &(),
            _dh: &DisplayHandle,
            _init: &mut wayland_server::DataInit<'_, Self>,
        ) {
        }
    }

    fn query(
        layout: Vec<MockOutput>,
        xdg_output: bool,
    ) -> xa11y_core::Result<super::PointerExtent> {
        let sock_path = std::env::temp_dir().join(format!(
            "xa11y-mock-wl-extent-{}-{}.sock",
            std::process::id(),
            layout.len() * 2 + usize::from(xdg_output)
        ));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind mock socket");
        listener.set_nonblocking(true).expect("nonblocking");

        let mut display: Display<MockServer> = Display::new().expect("server display");
        let mut dh = display.handle();
        for index in 0..layout.len() {
            dh.create_global::<MockServer, WlOutput, usize>(2, index);
        }
        if xdg_output {
            dh.create_global::<MockServer, ZxdgOutputManagerV1, ()>(3, ());
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown);
        let server = thread::spawn(move || {
            let mut state = MockServer { layout };
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        dh.insert_client(stream, Arc::new(ClientState))
                            .expect("insert client");
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("mock accept: {e}"),
                }
                display.dispatch_clients(&mut state).expect("dispatch");
                display.flush_clients().expect("flush");
                if server_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        let stream = UnixStream::connect(&sock_path).expect("connect mock socket");
        let conn = Connection::from_socket(stream).expect("client connection");
        let extent = super::compositor_extent(conn);

        shutdown.store(true, Ordering::Relaxed);
        server.join().expect("join server");
        let _ = std::fs::remove_file(&sock_path);
        extent
    }

    #[test]
    fn reads_every_output_logical_rectangle_from_xdg_output() {
        let extent = query(
            vec![(0, 0, Some((1280, 720))), (1280, -200, Some((1920, 1080)))],
            true,
        )
        .expect("two described outputs");
        assert_eq!(
            extent.outputs,
            [
                Rect {
                    x: 0,
                    y: 0,
                    width: 1280,
                    height: 720
                },
                Rect {
                    x: 1280,
                    y: -200,
                    width: 1920,
                    height: 1080
                },
            ]
        );
        assert_eq!(
            extent.bounds,
            Rect {
                x: 0,
                y: -200,
                width: 3200,
                height: 1080
            }
        );
        assert!(extent.source.contains("xdg-output"), "{}", extent.source);
    }

    #[test]
    fn an_undescribed_output_or_missing_xdg_output_asks_for_the_override() {
        for (layout, xdg_output) in [
            (vec![(0, 0, Some((1280, 720))), (1280, 0, None)], true),
            (vec![(0, 0, Some((1280, 720)))], false),
            (vec![], true),
        ] {
            let error = query(layout, xdg_output).expect_err("unusable layout");
            assert!(matches!(error, Error::Platform { .. }), "{error:?}");
            let message = error.to_string();
            assert!(
                message.contains("XA11Y_SCREEN_WIDTH") && message.contains("XA11Y_SCREEN_HEIGHT"),
                "{message}"
            );
        }
    }
}
