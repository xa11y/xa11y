//! Native Linux window management.
//!
//! Accessibility remains the source of the element tree. This module owns
//! the separate window-manager channel and resolves an AT-SPI top-level to a
//! native window before it advertises or performs a mutation. A snapshot binds
//! to one native ID. Later mutations validate that binding instead of matching
//! the snapshot's old title and geometry again.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection as WaylandConnection, Dispatch, QueueHandle};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask, MapState,
    Window, CLIENT_MESSAGE_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use xa11y_core::{ElementData, Error, Point, Rect, Result};

use crate::session::{select_backend, DesktopBackend};

const SOURCE_APPLICATION: u32 = 1;
const ICONIC_STATE: u32 = 3;
const STATE_REMOVE: u32 = 0;
const STATE_ADD: u32 = 1;
/// ICCCM `CurrentTime`. A window manager forwarding `WM_DELETE_WINDOW` stamps
/// it with the triggering event's time; xa11y has no triggering event.
const CURRENT_TIME: u32 = 0;

/// Snapshot-side native window facts. A missing action is not advertised;
/// direct calls still return the backend's precise unsupported or identity
/// error so callers are never sent through a silent substitute.
#[derive(Debug, Default)]
pub(crate) struct WindowFacts {
    pub(crate) actions: Vec<&'static str>,
    pub(crate) bounds: Option<Rect>,
    pub(crate) minimized: Option<bool>,
    pub(crate) maximized: Option<bool>,
    pub(crate) fullscreen: Option<bool>,
    pub(crate) backend: Option<&'static str>,
    pub(crate) protocol_version: Option<u32>,
}

pub(crate) struct WindowManager {
    backend: WindowBackend,
}

enum WindowBackend {
    X11(Box<X11WindowBackend>),
    Sway(Box<SwayWindowBackend>),
    Unavailable(String),
}

impl WindowManager {
    pub(crate) fn new() -> Self {
        let backend = match select_backend("XA11Y_LINUX_WINDOW_BACKEND", "native window management")
        {
            Ok(DesktopBackend::X11) => match X11WindowBackend::new() {
                Ok(backend) => WindowBackend::X11(Box::new(backend)),
                Err(error) => WindowBackend::Unavailable(format!(
                    "X11 window-manager connection failed: {error}"
                )),
            },
            Ok(DesktopBackend::Wayland) => match SwayWindowBackend::new() {
                Ok(backend) => WindowBackend::Sway(Box::new(backend)),
                Err(error) => WindowBackend::Unavailable(error.to_string()),
            },
            Err(error) => WindowBackend::Unavailable(error.to_string()),
        };
        Self { backend }
    }

    pub(crate) fn facts(&self, element: &ElementData) -> WindowFacts {
        match &self.backend {
            WindowBackend::X11(backend) => backend.facts(element).unwrap_or_default(),
            WindowBackend::Sway(backend) => backend.facts(element).unwrap_or_default(),
            WindowBackend::Unavailable(_) => WindowFacts::default(),
        }
    }

    /// The root-window origin (physical pixels) of the X11 client window
    /// that is `pid`'s top-level titled `title`, for toolkits whose
    /// AT-SPI screen extents ignore the window position (GTK 4 on X11,
    /// issue #487).
    ///
    /// The window is found by pid and title only, never by geometry: the
    /// geometry is what is being computed. `Ok(None)` when the Wayland
    /// backend is selected (a client has no global origin) or when zero or
    /// several windows match, so the caller reports no bounds rather than
    /// wrong ones; an error when no window backend could be set up. The
    /// origin is cached per window and dropped when the window is
    /// configured, reparented, destroyed or renamed, or the set of
    /// top-levels changes.
    pub(crate) fn x11_client_origin(&self, pid: u32, title: &str) -> Result<Option<(i32, i32)>> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.client_origin(pid, title),
            WindowBackend::Sway(_) => Ok(None),
            WindowBackend::Unavailable(reason) => Err(Error::Unsupported {
                feature: format!("X11 client origin: {reason}"),
            }),
        }
    }

    /// The pid of the window that the window manager reports as active
    /// (`_NET_ACTIVE_WINDOW`). `Ok(None)` when the backend cannot say: no
    /// X11 window manager with `_NET_ACTIVE_WINDOW`, no active window, an
    /// active window without `_NET_WM_PID`, or the Wayland backend.
    pub(crate) fn active_window_pid(&self) -> Result<Option<u32>> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.active_window_pid(),
            WindowBackend::Sway(_) | WindowBackend::Unavailable(_) => Ok(None),
        }
    }

    pub(crate) fn uses_native(&self) -> bool {
        matches!(
            &self.backend,
            WindowBackend::X11(_) | WindowBackend::Sway(_)
        )
    }

    pub(crate) fn activate(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.activate(element),
            WindowBackend::Sway(backend) => backend.activate(element),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn minimize(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.minimize(element),
            WindowBackend::Sway(_) => Err(Error::Unsupported {
                feature: "minimize: Sway has no minimized window state".to_string(),
            }),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn maximize(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.maximize(element),
            WindowBackend::Sway(_) => Err(Error::Unsupported {
                feature: "maximize: Sway has no maximized state distinct from fullscreen"
                    .to_string(),
            }),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.enter_fullscreen(element),
            WindowBackend::Sway(backend) => backend.enter_fullscreen(element),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn restore(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.restore(element),
            WindowBackend::Sway(backend) => backend.restore(element),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn close(&self, element: &ElementData) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.close(element),
            WindowBackend::Sway(backend) => backend.close(element),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.move_to(element, x, y),
            WindowBackend::Sway(backend) => backend.move_to(element, x, y),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }

    pub(crate) fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        match &self.backend {
            WindowBackend::X11(backend) => backend.resize_to(element, width, height),
            WindowBackend::Sway(backend) => backend.resize_to(element, width, height),
            WindowBackend::Unavailable(reason) => Err(window_unavailable(reason)),
        }
    }
}

fn window_unavailable(reason: &str) -> Error {
    Error::Unsupported {
        feature: format!("native Linux window management: {reason}"),
    }
}

const SWAY_IPC_MAGIC: &[u8; 6] = b"i3-ipc";
const SWAY_IPC_COMMAND: u32 = 0;
const SWAY_IPC_GET_TREE: u32 = 4;
const MAX_SWAY_REPLY: usize = 32 * 1024 * 1024;

/// Sway is the first native Wayland backend. Capability detection requires
/// both the advertised wlr foreign-toplevel protocol and Sway's IPC socket.
/// The wlr protocol deliberately exposes no PID/accessibility identifier, so
/// IPC supplies the identity proof and carries the native compositor request;
/// xa11y never guesses a handle from a title-only foreign-toplevel event.
struct SwayWindowBackend {
    socket: PathBuf,
    protocol_version: u32,
    bindings: Mutex<HashMap<u64, NativeBinding<u64>>>,
}

#[derive(Clone, Copy, Debug)]
struct NativeBinding<Id> {
    id: Id,
    pid: u32,
    process_start: u64,
    server_identity: ServerIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerIdentity {
    X11(Window),
    Sway { pid: u32, process_start: u64 },
}

fn sway_server_identity(stream: &UnixStream) -> Result<ServerIdentity> {
    let credentials = getsockopt(stream, PeerCredentials).map_err(|error| Error::Platform {
        code: error as i32 as i64,
        message: format!("read Sway IPC peer credentials: {error}"),
    })?;
    let pid = u32::try_from(credentials.pid()).map_err(|_| Error::Platform {
        code: -1,
        message: "Sway IPC peer has an invalid process id".to_string(),
    })?;
    Ok(ServerIdentity::Sway {
        pid,
        process_start: process_start(pid)?,
    })
}

fn process_start(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
        Error::Unsupported {
            feature: format!(
                "native window identity: process {pid} is no longer available: {error}"
            ),
        }
    })?;
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(|| Error::Platform {
        code: -1,
        message: format!("native window identity: invalid /proc/{pid}/stat"),
    })?;
    fields
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::Platform {
            code: -1,
            message: format!("native window identity: missing start time for process {pid}"),
        })
}

fn lost_identity(backend: &str, handle: u64) -> Error {
    Error::Unsupported {
        feature: format!(
            "native {backend} window identity was lost for accessibility handle {handle}"
        ),
    }
}

#[derive(Debug, Clone)]
struct SwayCandidate {
    id: u64,
    pid: u32,
    title: Option<String>,
    bounds: Option<Rect>,
    floating: bool,
    fullscreen: bool,
}

impl SwayWindowBackend {
    fn new() -> Result<Self> {
        let protocol_version =
            advertised_wlr_foreign_toplevel_version()?.ok_or_else(|| Error::Unsupported {
                feature: "native Wayland window management: compositor does not advertise \
                          zwlr_foreign_toplevel_manager_v1"
                    .to_string(),
            })?;
        let socket = std::env::var_os("SWAYSOCK")
            .map(PathBuf::from)
            .ok_or_else(|| Error::Unsupported {
                feature: "native Wayland window management: the initial supported compositor is \
                          Sway and SWAYSOCK is not set"
                    .to_string(),
            })?;
        if !socket.exists() {
            return Err(Error::Unsupported {
                feature: format!(
                    "native Wayland window management: SWAYSOCK {} does not exist",
                    socket.display()
                ),
            });
        }
        Ok(Self {
            socket,
            protocol_version,
            bindings: Mutex::new(HashMap::new()),
        })
    }

    fn facts(&self, element: &ElementData) -> Result<WindowFacts> {
        if self.protocol_version == 0 {
            return Err(Error::Unsupported {
                feature: "native Wayland window management: invalid wlr foreign-toplevel protocol version 0"
                    .to_string(),
            });
        }
        let target = self.bind(element)?;
        let mut actions = vec!["activate", "enter_fullscreen", "restore", "close"];
        if target.floating {
            actions.push("move_to");
            actions.push("resize_to");
        }
        Ok(WindowFacts {
            actions,
            bounds: target.bounds,
            // Sway does not implement minimize or a maximized state distinct
            // from fullscreen. Unknown is different from false.
            minimized: None,
            maximized: None,
            fullscreen: Some(target.fullscreen),
            backend: Some("wayland-sway-wlr-foreign-toplevel"),
            protocol_version: Some(self.protocol_version),
        })
    }

    fn activate(&self, element: &ElementData) -> Result<()> {
        self.command_for(element, "focus")
    }

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        self.command_for(element, "fullscreen enable")
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        self.command_for(element, "fullscreen disable")
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        self.command_for(element, "kill")
    }

    fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        let target = self.resolve(element)?;
        if !target.floating {
            return Err(Error::Unsupported {
                feature: "move_to: Sway only positions floating windows".to_string(),
            });
        }
        self.command(element, target.id, &format!("move position {x} {y}"))
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        let target = self.resolve(element)?;
        if !target.floating {
            return Err(Error::Unsupported {
                feature: "resize_to: Sway only sizes floating windows".to_string(),
            });
        }
        self.command(
            element,
            target.id,
            &format!("resize set width {width} px height {height} px"),
        )
    }

    fn command_for(&self, element: &ElementData, command: &str) -> Result<()> {
        let target = self.resolve(element)?;
        self.command(element, target.id, command)
    }

    fn command(&self, element: &ElementData, id: u64, command: &str) -> Result<()> {
        let binding = *self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&element.handle)
            .ok_or_else(|| lost_identity("Wayland", element.handle))?;
        if binding.id != id {
            return Err(lost_identity("Wayland", element.handle));
        }
        let payload = format!("[con_id={id}] {command}");
        let (reply, _) = self.request(
            SWAY_IPC_COMMAND,
            payload.as_bytes(),
            Some(binding.server_identity),
        )?;
        let results = reply.as_array().ok_or_else(|| Error::Platform {
            code: -1,
            message: "Sway command reply was not an array".to_string(),
        })?;
        if results.is_empty() {
            return Err(Error::Platform {
                code: -1,
                message: "Sway command reply was empty".to_string(),
            });
        }
        if let Some(failure) = results
            .iter()
            .find(|result| result.get("success").and_then(serde_json::Value::as_bool) != Some(true))
        {
            let diagnosis = failure
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("compositor refused the request");
            return Err(Error::Platform {
                code: -1,
                message: format!("Sway rejected {command:?} for con_id {id}: {diagnosis}"),
            });
        }
        Ok(())
    }

    fn bind(&self, element: &ElementData) -> Result<SwayCandidate> {
        if self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&element.handle)
        {
            return self.resolve(element);
        }
        let pid = element.pid.ok_or_else(|| Error::Unsupported {
            feature: "native Wayland window identity: AT-SPI target has no process id".to_string(),
        })?;
        let (tree, server_identity) = self.request(SWAY_IPC_GET_TREE, &[], None)?;
        let mut candidates = Vec::new();
        collect_sway_candidates(&tree, &mut candidates);
        candidates.retain(|candidate| candidate.pid == pid);
        let target =
            choose_sway_candidate(pid, element.name.as_deref(), element.bounds, &candidates)?;
        let binding = NativeBinding {
            id: target.id,
            pid,
            process_start: process_start(pid)?,
            server_identity,
        };
        let mut bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        if let std::collections::hash_map::Entry::Vacant(entry) = bindings.entry(element.handle) {
            entry.insert(binding);
        } else {
            drop(bindings);
            return self.resolve(element);
        }
        Ok(target)
    }

    fn resolve(&self, element: &ElementData) -> Result<SwayCandidate> {
        let binding = *self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&element.handle)
            .ok_or_else(|| lost_identity("Wayland", element.handle))?;
        if element.pid != Some(binding.pid) || process_start(binding.pid)? != binding.process_start
        {
            return Err(lost_identity("Wayland", element.handle));
        }
        // Sway's node_init assigns IDs from a monotonic process-local counter.
        // The connected peer's PID and start time reject a restarted
        // compositor, where that counter could start over.
        let (tree, _) = self.request(SWAY_IPC_GET_TREE, &[], Some(binding.server_identity))?;
        let mut candidates = Vec::new();
        collect_sway_candidates(&tree, &mut candidates);
        bound_sway_candidate(binding, element.handle, &candidates)
    }

    fn request(
        &self,
        message_type: u32,
        payload: &[u8],
        expected: Option<ServerIdentity>,
    ) -> Result<(serde_json::Value, ServerIdentity)> {
        let mut stream = UnixStream::connect(&self.socket).map_err(|error| Error::Platform {
            code: -1,
            message: format!("connect Sway IPC {}: {error}", self.socket.display()),
        })?;
        let server_identity = sway_server_identity(&stream)?;
        if expected.is_some_and(|identity| identity != server_identity) {
            return Err(Error::Unsupported {
                feature: "native Wayland window identity: Sway compositor session changed"
                    .to_string(),
            });
        }
        let length = u32::try_from(payload.len())
            .map_err(|_| Error::invalid_action_data("Sway IPC request is too large"))?;
        let mut header = Vec::with_capacity(14);
        header.extend_from_slice(SWAY_IPC_MAGIC);
        header.extend_from_slice(&length.to_le_bytes());
        header.extend_from_slice(&message_type.to_le_bytes());
        stream.write_all(&header).map_err(sway_io)?;
        stream.write_all(payload).map_err(sway_io)?;

        let mut response_header = [0_u8; 14];
        stream.read_exact(&mut response_header).map_err(sway_io)?;
        if &response_header[..6] != SWAY_IPC_MAGIC {
            return Err(Error::Platform {
                code: -1,
                message: "Sway IPC reply had an invalid magic header".to_string(),
            });
        }
        let response_length = u32::from_le_bytes(
            response_header[6..10]
                .try_into()
                .map_err(|_| sway_protocol("invalid reply length field"))?,
        ) as usize;
        let response_type = u32::from_le_bytes(
            response_header[10..14]
                .try_into()
                .map_err(|_| sway_protocol("invalid reply type field"))?,
        );
        if response_type != message_type {
            return Err(sway_protocol(&format!(
                "reply type {response_type} did not match request type {message_type}"
            )));
        }
        if response_length > MAX_SWAY_REPLY {
            return Err(sway_protocol(&format!(
                "reply length {response_length} exceeds {MAX_SWAY_REPLY} byte limit"
            )));
        }
        let mut response = vec![0_u8; response_length];
        stream.read_exact(&mut response).map_err(sway_io)?;
        let value = serde_json::from_slice(&response).map_err(|error| Error::Platform {
            code: -1,
            message: format!("decode Sway IPC JSON: {error}"),
        })?;
        Ok((value, server_identity))
    }
}

fn collect_sway_candidates(node: &serde_json::Value, output: &mut Vec<SwayCandidate>) {
    let id = node.get("id").and_then(serde_json::Value::as_u64);
    let pid = node
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let is_container = node.get("type").and_then(serde_json::Value::as_str) == Some("con");
    let is_toplevel = node.get("app_id").is_some_and(|value| !value.is_null())
        || node.get("window").is_some_and(|value| !value.is_null())
        || node.get("shell").is_some_and(|value| !value.is_null());
    if let (Some(id), Some(pid), true, true) = (id, pid, is_container, is_toplevel) {
        let bounds = node.get("rect").and_then(sway_rect);
        let floating = node
            .get("floating")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.ends_with("_on"));
        let fullscreen = node
            .get("fullscreen_mode")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|value| value != 0);
        output.push(SwayCandidate {
            id,
            pid,
            title: node
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            bounds,
            floating,
            fullscreen,
        });
    }
    for field in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(field).and_then(serde_json::Value::as_array) {
            for child in children {
                collect_sway_candidates(child, output);
            }
        }
    }
}

fn sway_rect(value: &serde_json::Value) -> Option<Rect> {
    Some(Rect {
        x: i32::try_from(value.get("x")?.as_i64()?).ok()?,
        y: i32::try_from(value.get("y")?.as_i64()?).ok()?,
        width: u32::try_from(value.get("width")?.as_u64()?).ok()?,
        height: u32::try_from(value.get("height")?.as_u64()?).ok()?,
    })
}

fn choose_sway_candidate(
    pid: u32,
    title: Option<&str>,
    bounds: Option<Rect>,
    candidates: &[SwayCandidate],
) -> Result<SwayCandidate> {
    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }
    if candidates.is_empty() {
        return Err(Error::Unsupported {
            feature: format!(
                "native Wayland window identity: no Sway toplevel has process id {pid}"
            ),
        });
    }
    if let Some(expected) = bounds {
        let by_bounds: Vec<&SwayCandidate> = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .bounds
                    .is_some_and(|actual| rect_near(actual, expected, 2))
            })
            .collect();
        if by_bounds.len() == 1 {
            if title.is_some_and(|title| by_bounds[0].title.as_deref() != Some(title)) {
                return Err(Error::Unsupported { feature: format!("native Wayland window identity is ambiguous: process {pid} has conflicting title and geometry") });
            }
            return Ok(by_bounds[0].clone());
        }
        if let Some(title) = title {
            let by_title: Vec<&&SwayCandidate> = by_bounds
                .iter()
                .filter(|candidate| candidate.title.as_deref() == Some(title))
                .collect();
            if by_title.len() == 1 {
                return Ok((*by_title[0]).clone());
            }
        }
    }
    if let Some(title) = title {
        let by_title: Vec<&SwayCandidate> = candidates
            .iter()
            .filter(|candidate| candidate.title.as_deref() == Some(title))
            .collect();
        if by_title.len() == 1 {
            return Ok(by_title[0].clone());
        }
    }
    Err(Error::Unsupported {
        feature: format!(
            "native Wayland window identity is ambiguous: process {pid} owns {} Sway toplevels",
            candidates.len()
        ),
    })
}

fn bound_sway_candidate(
    binding: NativeBinding<u64>,
    handle: u64,
    candidates: &[SwayCandidate],
) -> Result<SwayCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.id == binding.id && candidate.pid == binding.pid)
        .cloned()
        .ok_or_else(|| lost_identity("Wayland", handle))
}

#[derive(Default)]
struct WaylandGlobals {
    wlr_foreign_toplevel_version: Option<u32>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandGlobals {
    fn event(
        state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &WaylandConnection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            interface, version, ..
        } = event
        {
            if interface == "zwlr_foreign_toplevel_manager_v1" {
                state.wlr_foreign_toplevel_version = Some(version);
            }
        }
    }
}

fn advertised_wlr_foreign_toplevel_version() -> Result<Option<u32>> {
    let connection = WaylandConnection::connect_to_env().map_err(|error| Error::Platform {
        code: -1,
        message: format!("connect Wayland compositor for window capability detection: {error}"),
    })?;
    let display = connection.display();
    let mut queue = connection.new_event_queue::<WaylandGlobals>();
    display.get_registry(&queue.handle(), ());
    let mut globals = WaylandGlobals::default();
    queue
        .roundtrip(&mut globals)
        .map_err(|error| Error::Platform {
            code: -1,
            message: format!("query Wayland registry for window capabilities: {error}"),
        })?;
    Ok(globals.wlr_foreign_toplevel_version)
}

fn sway_io(error: std::io::Error) -> Error {
    Error::Platform {
        code: error.raw_os_error().unwrap_or(-1) as i64,
        message: format!("Sway IPC: {error}"),
    }
}

fn sway_protocol(message: &str) -> Error {
    Error::Platform {
        code: -1,
        message: format!("Sway IPC: {message}"),
    }
}

#[derive(Clone, Copy)]
struct X11Atoms {
    net_client_list: Atom,
    net_supported: Atom,
    net_supporting_wm_check: Atom,
    net_wm_pid: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_frame_extents: Atom,
    gtk_frame_extents: Atom,
    net_wm_state: Atom,
    net_wm_state_hidden: Atom,
    net_wm_state_max_horz: Atom,
    net_wm_state_max_vert: Atom,
    net_wm_state_fullscreen: Atom,
    net_active_window: Atom,
    net_close_window: Atom,
    net_moveresize_window: Atom,
    wm_change_state: Atom,
    wm_protocols: Atom,
    wm_delete_window: Atom,
}

impl X11Atoms {
    fn new(conn: &RustConnection) -> Result<Self> {
        fn atom(conn: &RustConnection, name: &[u8]) -> Result<Atom> {
            conn.intern_atom(false, name)
                .map_err(platform)?
                .reply()
                .map(|reply| reply.atom)
                .map_err(platform)
        }
        Ok(Self {
            net_client_list: atom(conn, b"_NET_CLIENT_LIST")?,
            net_supported: atom(conn, b"_NET_SUPPORTED")?,
            net_supporting_wm_check: atom(conn, b"_NET_SUPPORTING_WM_CHECK")?,
            net_wm_pid: atom(conn, b"_NET_WM_PID")?,
            net_wm_name: atom(conn, b"_NET_WM_NAME")?,
            utf8_string: atom(conn, b"UTF8_STRING")?,
            net_frame_extents: atom(conn, b"_NET_FRAME_EXTENTS")?,
            gtk_frame_extents: atom(conn, b"_GTK_FRAME_EXTENTS")?,
            net_wm_state: atom(conn, b"_NET_WM_STATE")?,
            net_wm_state_hidden: atom(conn, b"_NET_WM_STATE_HIDDEN")?,
            net_wm_state_max_horz: atom(conn, b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
            net_wm_state_max_vert: atom(conn, b"_NET_WM_STATE_MAXIMIZED_VERT")?,
            net_wm_state_fullscreen: atom(conn, b"_NET_WM_STATE_FULLSCREEN")?,
            net_active_window: atom(conn, b"_NET_ACTIVE_WINDOW")?,
            net_close_window: atom(conn, b"_NET_CLOSE_WINDOW")?,
            net_moveresize_window: atom(conn, b"_NET_MOVERESIZE_WINDOW")?,
            wm_change_state: atom(conn, b"WM_CHANGE_STATE")?,
            wm_protocols: atom(conn, b"WM_PROTOCOLS")?,
            wm_delete_window: atom(conn, b"WM_DELETE_WINDOW")?,
        })
    }
}

/// A top-level window's identity for [`X11WindowBackend::client_origin`]:
/// owning pid and title.
type OriginKey = (u32, String);

/// A cached client window and the root origin of its content (physical pixels).
type CachedOrigin = (Window, (i32, i32));

struct X11WindowBackend {
    conn: Mutex<RustConnection>,
    /// [`Self::client_origin`] results keyed by (pid, title): the window and
    /// its root origin. See [`Self::forget_origins`] for what drops an entry.
    origins: Mutex<HashMap<OriginKey, CachedOrigin>>,
    bindings: Mutex<HashMap<u64, NativeBinding<Window>>>,
    destroyed_handles: Mutex<HashSet<u64>>,
    root: Window,
    atoms: X11Atoms,
    supported: HashSet<Atom>,
    has_window_manager: bool,
    /// Whether [`Self::client_origin`] has run, and so selected the root
    /// mask and property changes on top-levels. Until then this connection
    /// watches only what `bind` needs, so a client that never reads GTK 4
    /// bounds (the MCP server, most scripts) queues no root or title events.
    origins_watched: AtomicBool,
}

#[derive(Debug, Clone)]
struct X11Candidate {
    window: Window,
    pid: u32,
    title: Option<String>,
    bounds: Option<Rect>,
}

impl X11WindowBackend {
    fn new() -> Result<Self> {
        Self::connect(None)
    }

    /// Connect to `display` (`None` reads `$DISPLAY`) and read the window
    /// manager's capabilities once. The isolated-Xvfb tests pass their own
    /// display so they exercise the same detection as production.
    fn connect(display: Option<&str>) -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(display).map_err(platform)?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| platform("X server reported no screens"))?
            .root;
        let atoms = X11Atoms::new(&conn)?;
        let supported = property_u32(&conn, root, atoms.net_supported, AtomEnum::ATOM.into())?
            .into_iter()
            .collect();
        let has_window_manager = !property_u32(
            &conn,
            root,
            atoms.net_supporting_wm_check,
            AtomEnum::WINDOW.into(),
        )?
        .is_empty();
        Ok(Self {
            conn: Mutex::new(conn),
            origins: Mutex::new(HashMap::new()),
            bindings: Mutex::new(HashMap::new()),
            destroyed_handles: Mutex::new(HashSet::new()),
            root,
            atoms,
            supported,
            has_window_manager,
            origins_watched: AtomicBool::new(false),
        })
    }

    fn facts(&self, element: &ElementData) -> Result<WindowFacts> {
        self.with_server_grab(|conn| self.facts_with_connection(conn, element))
    }

    fn facts_with_connection(
        &self,
        conn: &RustConnection,
        element: &ElementData,
    ) -> Result<WindowFacts> {
        let window = self.bind(conn, element)?;
        let state: HashSet<Atom> =
            property_u32(conn, window, self.atoms.net_wm_state, AtomEnum::ATOM.into())?
                .into_iter()
                .collect();

        let mut actions = Vec::new();
        if self.supports(self.atoms.net_active_window) {
            actions.push("activate");
        }
        if self.has_window_manager {
            actions.push("minimize");
        }
        if self.supports_state(self.atoms.net_wm_state_max_horz)
            && self.supports_state(self.atoms.net_wm_state_max_vert)
        {
            actions.push("maximize");
        }
        if self.supports_state(self.atoms.net_wm_state_fullscreen) {
            actions.push("enter_fullscreen");
        }
        if self.has_window_manager || self.supports(self.atoms.net_wm_state) {
            actions.push("restore");
        }
        // Advertised exactly when `close` would pick a mechanism, so the
        // capability and the verb cannot disagree.
        if self.close_mechanism(conn, window)?.is_some() {
            actions.push("close");
        }
        if self.supports(self.atoms.net_moveresize_window) {
            actions.push("move_to");
            actions.push("resize_to");
        }

        // Without a window manager nobody maintains _NET_WM_STATE, so its
        // absence is not an observed "not minimized": the states are unknown.
        let observed = |value: bool| self.has_window_manager.then_some(value);
        Ok(WindowFacts {
            actions,
            bounds: self.window_bounds(conn, window).ok(),
            minimized: observed(state.contains(&self.atoms.net_wm_state_hidden)),
            maximized: observed(
                state.contains(&self.atoms.net_wm_state_max_horz)
                    && state.contains(&self.atoms.net_wm_state_max_vert),
            ),
            fullscreen: observed(state.contains(&self.atoms.net_wm_state_fullscreen)),
            backend: Some(if self.has_window_manager {
                "x11-ewmh"
            } else {
                "x11-icccm"
            }),
            protocol_version: None,
        })
    }

    fn with_server_grab<T>(&self, action: impl FnOnce(&RustConnection) -> Result<T>) -> Result<T> {
        // The first scale lookup may open a separate X connection. Complete it
        // before the grab; a different client cannot answer while grabbed.
        crate::scale::coordinate_scale();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.grab_server()
            .map_err(platform)?
            .check()
            .map_err(platform)?;
        let result = action(&conn);
        let release = conn
            .ungrab_server()
            .map_err(platform)
            .and_then(|cookie| cookie.check().map_err(platform))
            .and_then(|()| conn.flush().map_err(platform));
        match (result, release) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(action_error), Err(release_error)) => Err(Error::Platform {
                code: -1,
                message: format!("native X11 action failed: {action_error}; releasing server grab failed: {release_error}"),
            }),
        }
    }

    fn supports(&self, atom: Atom) -> bool {
        self.supported.contains(&atom)
    }

    fn supports_state(&self, state_atom: Atom) -> bool {
        self.supports(self.atoms.net_wm_state) && self.supports(state_atom)
    }

    fn require(&self, atom: Atom, feature: &str) -> Result<()> {
        if self.supports(atom) {
            Ok(())
        } else {
            Err(Error::Unsupported {
                feature: format!("{feature}: the X11 window manager does not advertise it"),
            })
        }
    }

    fn bind(&self, conn: &RustConnection, element: &ElementData) -> Result<Window> {
        if self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&element.handle)
        {
            return self.resolve(conn, element);
        }
        self.drain_destroy_events(conn)?;
        let pid = element.pid.ok_or_else(|| Error::Unsupported {
            feature: "native X11 window identity: AT-SPI target has no process id".to_string(),
        })?;
        let clients = self.top_level_windows(conn)?;
        if clients.is_empty() {
            return Err(Error::Unsupported {
                feature: if self.has_window_manager {
                    "native X11 window identity: the window manager exposes no _NET_CLIENT_LIST"
                        .to_string()
                } else {
                    "native X11 window identity: no window manager is running and the root \
                     window has no mapped top-level children"
                        .to_string()
                },
            });
        }
        // Subscribe before reading candidate properties. A destruction after
        // this point is observable even when the XID is immediately reused.
        self.watch_clients(conn, &clients)?;
        let destroyed_before_read = self.drain_destroy_events(conn)?;
        let mut candidates = Vec::new();
        for window in clients {
            if destroyed_before_read.contains(&window) {
                continue;
            }
            if let Some(candidate) = self.candidate(conn, window)? {
                if candidate.pid == pid {
                    candidates.push(candidate);
                }
            }
        }
        let window = choose_candidate(pid, element.name.as_deref(), element.bounds, &candidates)?;
        if self.drain_destroy_events(conn)?.contains(&window) {
            return Err(lost_identity("X11", element.handle));
        }
        let binding = NativeBinding {
            id: window,
            pid,
            process_start: process_start(pid)?,
            server_identity: ServerIdentity::X11(self.root),
        };
        let mut bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        if let std::collections::hash_map::Entry::Vacant(entry) = bindings.entry(element.handle) {
            entry.insert(binding);
        } else {
            drop(bindings);
            return self.resolve(conn, element);
        }
        Ok(window)
    }

    fn resolve(&self, conn: &RustConnection, element: &ElementData) -> Result<Window> {
        let binding = *self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&element.handle)
            .ok_or_else(|| lost_identity("X11", element.handle))?;
        if element.pid != Some(binding.pid)
            || process_start(binding.pid)? != binding.process_start
            || binding.server_identity != ServerIdentity::X11(self.root)
        {
            return Err(lost_identity("X11", element.handle));
        }
        self.drain_destroy_events(conn)?;
        if self
            .destroyed_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&element.handle)
        {
            return Err(lost_identity("X11", element.handle));
        }
        let clients = self.top_level_windows(conn)?;
        if !clients.contains(&binding.id) {
            return Err(lost_identity("X11", element.handle));
        }
        let actual_pid = property_u32(
            conn,
            binding.id,
            self.atoms.net_wm_pid,
            AtomEnum::CARDINAL.into(),
        )?;
        if actual_pid.first().copied() != Some(binding.pid) {
            return Err(lost_identity("X11", element.handle));
        }
        Ok(binding.id)
    }

    /// The top-level client windows identity is resolved against.
    ///
    /// The source is fixed by whether a window manager runs, which is read
    /// once at connection time; it is not a second attempt after the first
    /// fails. With one, `_NET_CLIENT_LIST` is authoritative: a reparenting
    /// manager makes the root's children its frames, not the clients. With
    /// none, nothing maintains `_NET_CLIENT_LIST`, and the ICCCM top-levels
    /// are the mapped, non-override-redirect children of the root. Either way
    /// the candidates then go through the same `_NET_WM_PID`, process-start,
    /// and geometry/title proof.
    ///
    /// Callers hold the server grab, so no window can vanish between the
    /// tree query and the attribute reads.
    fn top_level_windows(&self, conn: &RustConnection) -> Result<Vec<Window>> {
        if self.has_window_manager {
            return property_u32(
                conn,
                self.root,
                self.atoms.net_client_list,
                AtomEnum::WINDOW.into(),
            );
        }
        let children = conn
            .query_tree(self.root)
            .map_err(platform)?
            .reply()
            .map_err(platform)?
            .children;
        let mut top_levels = Vec::new();
        for window in children {
            let attributes = conn
                .get_window_attributes(window)
                .map_err(platform)?
                .reply()
                .map_err(platform)?;
            if !attributes.override_redirect && attributes.map_state == MapState::VIEWABLE {
                top_levels.push(window);
            }
        }
        Ok(top_levels)
    }

    /// Choose how `close` reaches `window`, before anything is sent. `None`
    /// means neither mechanism applies; [`Self::close_unsupported`] says why.
    fn close_mechanism(
        &self,
        conn: &RustConnection,
        window: Window,
    ) -> Result<Option<CloseMechanism>> {
        let wm_advertises_close = self.supports(self.atoms.net_close_window);
        // Read WM_PROTOCOLS only when it decides the outcome.
        let client_accepts_delete = !wm_advertises_close
            && property_u32(conn, window, self.atoms.wm_protocols, AtomEnum::ATOM.into())?
                .contains(&self.atoms.wm_delete_window);
        Ok(select_close_mechanism(
            wm_advertises_close,
            client_accepts_delete,
        ))
    }

    fn close_unsupported(&self, window: Window) -> Error {
        let manager = if self.has_window_manager {
            "the running window manager does not list _NET_CLOSE_WINDOW in _NET_SUPPORTED"
        } else {
            "no window manager is running, so nothing provides _NET_CLOSE_WINDOW"
        };
        Error::Unsupported {
            feature: format!(
                "close: no X11 close mechanism is available: {manager}, and client window \
                 {window:#x} does not list WM_DELETE_WINDOW in WM_PROTOCOLS (xa11y does not \
                 force-close with XKillClient)"
            ),
        }
    }

    /// Select [`client_event_mask`] on each of `windows`. A selection
    /// replaces this connection's previous mask on that window, so every
    /// caller goes through here and none can narrow another's.
    fn watch_clients(&self, conn: &RustConnection, windows: &[Window]) -> Result<()> {
        let mask = client_event_mask(self.origins_watched.load(Ordering::Acquire));
        for &window in windows {
            conn.change_window_attributes(
                window,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new().event_mask(mask),
            )
            .map_err(platform)?
            .check()
            .map_err(platform)?;
        }
        Ok(())
    }

    /// Start watching what invalidates a cached [`Self::client_origin`], on
    /// its first call: the set of top-levels on the root. With a window
    /// manager the set is `_NET_CLIENT_LIST`, a root property; without one
    /// it is the root's mapped children. The caller holds the server grab
    /// and enumerates after this, so no window can appear unobserved.
    fn watch_origins(&self, conn: &RustConnection) -> Result<()> {
        if self.origins_watched.load(Ordering::Acquire) {
            return Ok(());
        }
        let root_mask = if self.has_window_manager {
            EventMask::PROPERTY_CHANGE
        } else {
            EventMask::PROPERTY_CHANGE | EventMask::SUBSTRUCTURE_NOTIFY
        };
        conn.change_window_attributes(
            self.root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new().event_mask(root_mask),
        )
        .map_err(platform)?
        .check()
        .map_err(platform)?;
        self.origins_watched.store(true, Ordering::Release);
        Ok(())
    }

    /// Apply every event the server has sent up to now: drop stale
    /// [`Self::client_origin`] entries, tombstone bindings whose window was
    /// destroyed, and return the destroyed windows.
    fn drain_destroy_events(&self, conn: &RustConnection) -> Result<HashSet<Window>> {
        // A round trip orders every notification the server generated before
        // this call ahead of the check, so none is still in flight.
        conn.get_input_focus()
            .map_err(platform)?
            .reply()
            .map_err(platform)?;
        let mut destroyed_windows = HashSet::new();
        let mut renamed = HashSet::new();
        while let Some(event) = conn.poll_for_event().map_err(platform)? {
            self.forget_origins(&event, &mut renamed);
            if let Event::DestroyNotify(event) = event {
                destroyed_windows.insert(event.window);
                let bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
                let mut destroyed = self
                    .destroyed_handles
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                for (&handle, binding) in bindings.iter() {
                    if binding.id == event.window {
                        destroyed.insert(handle);
                    }
                }
            }
        }
        self.forget_renamed(conn, &renamed)?;
        Ok(destroyed_windows)
    }

    /// Drop the cached [`Self::client_origin`] entries `event` may have made
    /// wrong:
    /// - the window moved or went away (`ConfigureNotify`, `ReparentNotify`,
    ///   `DestroyNotify`). A reparenting window manager moves the frame, not
    ///   the client, and tells the client with a synthetic `ConfigureNotify`
    ///   (ICCCM 4.1.5), which reaches us through the same event mask;
    /// - a watched window's `_GTK_FRAME_EXTENTS` changed: the shadow size
    ///   is part of the cached origin;
    /// - a watched window's `_NET_WM_NAME` or `_NET_WM_PID` changed: its own
    ///   entries go now, and the window joins `renamed` for
    ///   [`Self::forget_renamed`], which reads each new identity once after
    ///   the whole batch;
    /// - the set of top-levels changed (`_NET_CLIENT_LIST`, or a root child
    ///   mapped, unmapped or created without a window manager). A new window
    ///   may share a cached title, and would not be watched yet.
    fn forget_origins(&self, event: &Event, renamed: &mut HashSet<Window>) {
        let mut origins = self.origins.lock().unwrap_or_else(|e| e.into_inner());
        if origins.is_empty() {
            return;
        }
        match event {
            Event::PropertyNotify(e)
                if e.window != self.root
                    && (e.atom == self.atoms.net_wm_name || e.atom == self.atoms.net_wm_pid) =>
            {
                origins.retain(|_, (cached, _)| *cached != e.window);
                renamed.insert(e.window);
            }
            Event::PropertyNotify(e)
                if e.window != self.root && e.atom == self.atoms.gtk_frame_extents =>
            {
                origins.retain(|_, (cached, _)| *cached != e.window)
            }
            Event::PropertyNotify(e)
                if e.window == self.root && e.atom == self.atoms.net_client_list =>
            {
                origins.clear()
            }
            Event::ConfigureNotify(e) => origins.retain(|_, (cached, _)| *cached != e.window),
            Event::ReparentNotify(e) => origins.retain(|_, (cached, _)| *cached != e.window),
            Event::DestroyNotify(e) => origins.retain(|_, (cached, _)| *cached != e.window),
            Event::MapNotify(_) | Event::UnmapNotify(_) | Event::CreateNotify(_) => origins.clear(),
            _ => {}
        }
    }

    /// Drop the entry for each `renamed` window's new (pid, title): a window
    /// renamed *to* a cached title makes that match ambiguous. The reads are
    /// pipelined, so a batch of renames costs one round trip.
    ///
    /// A window that is gone by the time it is read (`BadWindow`, the
    /// expected race with its destruction) clears every entry, which is never
    /// wrong, only slower. Any other failure is returned.
    fn forget_renamed(&self, conn: &RustConnection, renamed: &HashSet<Window>) -> Result<()> {
        if renamed.is_empty()
            || self
                .origins
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        {
            return Ok(());
        }
        let mut cookies = Vec::with_capacity(renamed.len());
        for &window in renamed {
            let pid = conn
                .get_property(
                    false,
                    window,
                    self.atoms.net_wm_pid,
                    AtomEnum::CARDINAL,
                    0,
                    1,
                )
                .map_err(platform)?;
            let title = conn
                .get_property(
                    false,
                    window,
                    self.atoms.net_wm_name,
                    self.atoms.utf8_string,
                    0,
                    u32::MAX,
                )
                .map_err(platform)?;
            cookies.push((window, pid, title));
        }
        // Read every reply before taking the cache lock: a round trip under
        // it would hold up a concurrent cache hit for nothing.
        let mut keys = Vec::with_capacity(cookies.len());
        let mut window_gone = None;
        for (window, pid, title) in cookies {
            let read = pid.reply().and_then(|pid| Ok((pid, title.reply()?)));
            match read {
                Ok((pid, title)) => {
                    let pid = pid.value32().and_then(|mut values| values.next());
                    let title = String::from_utf8(title.value)
                        .ok()
                        .filter(|title| !title.is_empty());
                    // No pid or no title: it cannot match any cached key.
                    if let (Some(pid), Some(title)) = (pid, title) {
                        keys.push((pid, title));
                    }
                }
                Err(x11rb::errors::ReplyError::X11Error(error))
                    if error.error_kind == x11rb::protocol::ErrorKind::Window =>
                {
                    window_gone = Some(window);
                }
                Err(error) => return Err(platform(error)),
            }
        }
        let mut origins = self.origins.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(window) = window_gone {
            log::debug!(
                "X11 client origin cache cleared: window {window:#x} changed its identity \
                 and was gone before it could be read"
            );
            origins.clear();
            return Ok(());
        }
        for key in keys {
            origins.remove(&key);
        }
        Ok(())
    }

    fn client_origin(&self, pid: u32, title: &str) -> Result<Option<(i32, i32)>> {
        let key = (pid, title.to_owned());
        {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            // One round trip, so an invalidation the server has already sent
            // is applied before the cache is trusted.
            self.drain_destroy_events(&conn)?;
            if let Some((_, origin)) = self
                .origins
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
            {
                return Ok(Some(*origin));
            }
        }
        // A miss enumerates under the server grab, as `bind` does: no window
        // can vanish between listing the top-levels and reading them.
        self.with_server_grab(|conn| {
            self.watch_origins(conn)?;
            self.drain_destroy_events(conn)?;
            let clients = self.top_level_windows(conn)?;
            // Watch before reading: a rename or move after the read
            // invalidates the entry instead of racing past it.
            self.watch_clients(conn, &clients)?;
            let mut candidates = Vec::with_capacity(clients.len());
            for window in clients {
                if let Some(candidate) = self.identity(conn, window)? {
                    candidates.push(candidate);
                }
            }
            let Some(window) = select_client_by_pid_title(&candidates, pid, title) else {
                log::debug!(
                    "X11 client origin for pid {pid} titled {title:?}: {} of {} top-levels \
                     match, so no origin (need exactly one)",
                    clients_by_pid_title(&candidates, pid, title).len(),
                    candidates.len(),
                );
                return Ok(None);
            };
            let translated = conn
                .translate_coordinates(window, self.root, 0, 0)
                .map_err(platform)?
                .reply()
                .map_err(platform)?;
            let (shadow_x, shadow_y) = self.gtk_shadow(conn, window)?;
            let origin = (
                i32::from(translated.dst_x) + shadow_x,
                i32::from(translated.dst_y) + shadow_y,
            );
            self.origins
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (window, origin));
            Ok(Some(origin))
        })
    }

    /// The left and top size of the shadow that GTK draws inside `window`,
    /// from `_GTK_FRAME_EXTENTS`. GTK 4 window-relative extents start at the
    /// visible frame, inside the shadow. A window without the property has no
    /// shadow; a malformed value is an error.
    fn gtk_shadow(&self, conn: &RustConnection, window: Window) -> Result<(i32, i32)> {
        let extents = property_u32(
            conn,
            window,
            self.atoms.gtk_frame_extents,
            AtomEnum::CARDINAL.into(),
        )?;
        match extents.as_slice() {
            [] => Ok((0, 0)),
            &[left, _, top, _] => Ok((
                i32::try_from(left).map_err(platform)?,
                i32::try_from(top).map_err(platform)?,
            )),
            other => Err(Error::Platform {
                code: -1,
                message: format!(
                    "_GTK_FRAME_EXTENTS of window {window:#x} has {} values, not 4",
                    other.len()
                ),
            }),
        }
    }

    /// A top-level's owning pid and title, without its geometry. `None`
    /// when it has no `_NET_WM_PID`.
    fn identity(&self, conn: &RustConnection, window: Window) -> Result<Option<X11Candidate>> {
        let Some(pid) = property_u32(
            conn,
            window,
            self.atoms.net_wm_pid,
            AtomEnum::CARDINAL.into(),
        )?
        .first()
        .copied() else {
            return Ok(None);
        };
        // Callers hold the server grab, so the window cannot vanish between
        // the pid read above and this one: a failure here is a real one and
        // is returned, never read as "untitled". An absent property is an
        // empty value, not an error.
        let title = String::from_utf8(property_bytes(
            conn,
            window,
            self.atoms.net_wm_name,
            self.atoms.utf8_string,
        )?)
        .ok()
        .filter(|title| !title.is_empty());
        Ok(Some(X11Candidate {
            window,
            pid,
            title,
            bounds: None,
        }))
    }

    fn candidate(&self, conn: &RustConnection, window: Window) -> Result<Option<X11Candidate>> {
        let Some(mut candidate) = self.identity(conn, window)? else {
            return Ok(None);
        };
        // Under the server grab, as for `identity`: a geometry read that
        // fails is returned rather than leaving the candidate without bounds,
        // which would quietly change how `choose_candidate` ranks it.
        candidate.bounds = Some(self.window_bounds(conn, window)?);
        Ok(Some(candidate))
    }

    fn window_bounds(&self, conn: &RustConnection, window: Window) -> Result<Rect> {
        let geometry = conn
            .get_geometry(window)
            .map_err(platform)?
            .reply()
            .map_err(platform)?;
        let translated = conn
            .translate_coordinates(window, self.root, 0, 0)
            .map_err(platform)?
            .reply()
            .map_err(platform)?;
        let extents = property_u32(
            conn,
            window,
            self.atoms.net_frame_extents,
            AtomEnum::CARDINAL.into(),
        )?;
        let (left, right, top, bottom) = match extents.as_slice() {
            [left, right, top, bottom, ..] => (*left, *right, *top, *bottom),
            _ => (0, 0, 0, 0),
        };
        Ok(Rect {
            x: i32::from(translated.dst_x) - i32::try_from(left).unwrap_or(i32::MAX),
            y: i32::from(translated.dst_y) - i32::try_from(top).unwrap_or(i32::MAX),
            width: u32::from(geometry.width)
                .saturating_add(left)
                .saturating_add(right),
            height: u32::from(geometry.height)
                .saturating_add(top)
                .saturating_add(bottom),
        }
        .to_logical(crate::scale::coordinate_scale()))
    }

    fn active_window_pid(&self) -> Result<Option<u32>> {
        if !self.supports(self.atoms.net_active_window) {
            return Ok(None);
        }
        // Under the grab, so the active window cannot go away between the
        // two reads.
        self.with_server_grab(|conn| {
            let active = property_u32(
                conn,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW.into(),
            )?;
            let Some(&window) = active.first().filter(|&&window| window != 0) else {
                return Ok(None);
            };
            Ok(property_u32(
                conn,
                window,
                self.atoms.net_wm_pid,
                AtomEnum::CARDINAL.into(),
            )?
            .first()
            .copied())
        })
    }

    fn activate(&self, element: &ElementData) -> Result<()> {
        self.require(self.atoms.net_active_window, "activate")?;
        self.send_for(
            element,
            self.atoms.net_active_window,
            [SOURCE_APPLICATION, 0, 0, 0, 0],
        )
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        if !self.has_window_manager {
            return Err(Error::Unsupported {
                feature: "minimize: no ICCCM/EWMH window manager is active".to_string(),
            });
        }
        // Keep the entry verbs distinct and absolute. Some window managers
        // preserve EWMH fullscreen/maximized atoms while iconifying, which
        // would otherwise make a later restore or screen-fill operation start
        // from two active states.
        // Several messages go out in turn. Once one has been sent, a later
        // failure must not read as "nothing happened" (see `Sent`).
        let mut sent = Sent::default();
        if self.supports_state(self.atoms.net_wm_state_fullscreen) {
            sent.step(self.change_state(
                element,
                STATE_REMOVE,
                self.atoms.net_wm_state_fullscreen,
                0,
            ))?;
        }
        if self.supports_state(self.atoms.net_wm_state_max_horz)
            && self.supports_state(self.atoms.net_wm_state_max_vert)
        {
            sent.step(self.change_state(
                element,
                STATE_REMOVE,
                self.atoms.net_wm_state_max_horz,
                self.atoms.net_wm_state_max_vert,
            ))?;
        }
        sent.step(self.send_for(
            element,
            self.atoms.wm_change_state,
            [ICONIC_STATE, 0, 0, 0, 0],
        ))
    }

    fn maximize(&self, element: &ElementData) -> Result<()> {
        self.require(self.atoms.net_wm_state, "maximize")?;
        if !self.supports_state(self.atoms.net_wm_state_max_horz)
            || !self.supports_state(self.atoms.net_wm_state_max_vert)
        {
            return Err(Error::Unsupported {
                feature:
                    "maximize: the X11 window manager does not advertise both maximized states"
                        .to_string(),
            });
        }
        let mut sent = Sent::default();
        sent.step(self.change_state(element, STATE_REMOVE, self.atoms.net_wm_state_fullscreen, 0))?;
        sent.step(self.change_state(
            element,
            STATE_ADD,
            self.atoms.net_wm_state_max_horz,
            self.atoms.net_wm_state_max_vert,
        ))
    }

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        if !self.supports_state(self.atoms.net_wm_state_fullscreen) {
            return Err(Error::Unsupported {
                feature: "enter_fullscreen: the X11 window manager does not advertise fullscreen"
                    .to_string(),
            });
        }
        let mut sent = Sent::default();
        sent.step(self.change_state(
            element,
            STATE_REMOVE,
            self.atoms.net_wm_state_max_horz,
            self.atoms.net_wm_state_max_vert,
        ))?;
        sent.step(self.change_state(element, STATE_ADD, self.atoms.net_wm_state_fullscreen, 0))
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        self.with_server_grab(|conn| {
            let window = self.resolve(conn, element)?;
            // De-iconification is the ICCCM MapWindow request. EWMH state removal
            // is separate and explicit: fullscreen and the two maximize atoms are
            // distinct states, and restore clears all of them.
            conn.map_window(window)
                .map_err(platform)?
                .check()
                .map_err(platform)?;
            if self.supports(self.atoms.net_wm_state) {
                send_client_message(
                    conn,
                    self.root,
                    window,
                    self.atoms.net_wm_state,
                    [
                        STATE_REMOVE,
                        self.atoms.net_wm_state_fullscreen,
                        0,
                        SOURCE_APPLICATION,
                        0,
                    ],
                )?;
                send_client_message(
                    conn,
                    self.root,
                    window,
                    self.atoms.net_wm_state,
                    [
                        STATE_REMOVE,
                        self.atoms.net_wm_state_max_horz,
                        self.atoms.net_wm_state_max_vert,
                        SOURCE_APPLICATION,
                        0,
                    ],
                )?;
            }
            conn.flush().map_err(platform)
        })
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        self.with_server_grab(|conn| {
            let window = self.resolve(conn, element)?;
            // The mechanism is chosen from what the window manager and the
            // client advertise, before anything is sent; a failed send is
            // returned, never retried through the other mechanism.
            match self.close_mechanism(conn, window)? {
                Some(CloseMechanism::EwmhCloseWindow) => {
                    log::debug!("close {window:#x}: EWMH _NET_CLOSE_WINDOW via the window manager");
                    send_client_message(
                        conn,
                        self.root,
                        window,
                        self.atoms.net_close_window,
                        [CURRENT_TIME, SOURCE_APPLICATION, 0, 0, 0],
                    )?;
                }
                Some(CloseMechanism::IcccmDeleteWindow) => {
                    log::debug!(
                        "close {window:#x}: ICCCM WM_DELETE_WINDOW sent to the client, since \
                         _NET_CLOSE_WINDOW is not advertised (window manager running: {})",
                        self.has_window_manager
                    );
                    send_wm_protocol(
                        conn,
                        window,
                        self.atoms.wm_protocols,
                        self.atoms.wm_delete_window,
                    )?;
                }
                None => return Err(self.close_unsupported(window)),
            }
            conn.flush().map_err(platform)
        })
    }

    fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        self.require(self.atoms.net_moveresize_window, "move_to")?;
        let point = Point::new(x, y).to_physical(crate::scale::coordinate_scale());
        let flags = (1_u32 << 8) | (1_u32 << 9) | (SOURCE_APPLICATION << 12);
        self.send_for(
            element,
            self.atoms.net_moveresize_window,
            [flags, point.x as u32, point.y as u32, 0, 0],
        )
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        self.require(self.atoms.net_moveresize_window, "resize_to")?;
        let physical = Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
        .to_physical(crate::scale::coordinate_scale());
        self.with_server_grab(|conn| {
            let window = self.resolve(conn, element)?;
            // EWMH's moveresize width and height describe the client window,
            // while xa11y's bounds contract reports the decorated outer frame.
            // Remove the current frame extents so a requested outer size reads
            // back as that same size after the window manager applies it.
            let extents = property_u32(
                conn,
                window,
                self.atoms.net_frame_extents,
                AtomEnum::CARDINAL.into(),
            )?;
            let (horizontal, vertical) = match extents.as_slice() {
                [left, right, top, bottom, ..] => {
                    (left.saturating_add(*right), top.saturating_add(*bottom))
                }
                _ => (0, 0),
            };
            let client_width = physical.width.saturating_sub(horizontal).max(1);
            let client_height = physical.height.saturating_sub(vertical).max(1);
            let flags = (1_u32 << 10) | (1_u32 << 11) | (SOURCE_APPLICATION << 12);
            send_client_message(
                conn,
                self.root,
                window,
                self.atoms.net_moveresize_window,
                [flags, 0, 0, client_width, client_height],
            )?;
            conn.flush().map_err(platform)
        })
    }

    fn change_state(
        &self,
        element: &ElementData,
        action: u32,
        first: Atom,
        second: Atom,
    ) -> Result<()> {
        self.send_for(
            element,
            self.atoms.net_wm_state,
            [action, first, second, SOURCE_APPLICATION, 0],
        )
    }

    fn send_for(&self, element: &ElementData, type_: Atom, data: [u32; 5]) -> Result<()> {
        self.with_server_grab(|conn| {
            let window = self.resolve(conn, element)?;
            send_client_message(conn, self.root, window, type_, data)?;
            conn.flush().map_err(platform)
        })
    }
}

/// How `close` asks an X11 client to close. Both are polite requests the
/// client may answer with a save prompt or a refusal; neither kills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseMechanism {
    /// EWMH `_NET_CLOSE_WINDOW` to the root window. The window manager
    /// carries it out, usually by sending the client `WM_DELETE_WINDOW`.
    EwmhCloseWindow,
    /// ICCCM `WM_PROTOCOLS` / `WM_DELETE_WINDOW` sent straight to the client:
    /// the message a window manager would forward, for a client that lists
    /// the protocol. This is how a bare X server (no window manager, as under
    /// plain Xvfb) closes a window at all.
    IcccmDeleteWindow,
}

/// The close mechanism for a window, decided from advertised capabilities
/// alone. The window manager's request comes first whenever it is advertised:
/// it is the EWMH contract, and the manager may do more than the client
/// message (end a session-managed client, update its own lists). The ICCCM
/// message is chosen only when the manager does not offer one. Choosing up
/// front is what keeps this from being a fallback chain (tenet 1): a failed
/// send is an error, not a cue to try the other mechanism.
fn select_close_mechanism(
    wm_advertises_close_window: bool,
    client_accepts_delete_window: bool,
) -> Option<CloseMechanism> {
    if wm_advertises_close_window {
        Some(CloseMechanism::EwmhCloseWindow)
    } else if client_accepts_delete_window {
        Some(CloseMechanism::IcccmDeleteWindow)
    } else {
        None
    }
}

fn choose_candidate(
    pid: u32,
    title: Option<&str>,
    bounds: Option<Rect>,
    candidates: &[X11Candidate],
) -> Result<Window> {
    if let [only] = candidates {
        // The PID match is sufficient unless the lone window contradicts the
        // target. A same-process window that is merely the last one left (a
        // hidden dialog whose X window is unmapped, beside a still-mapped main
        // window) must not stand in for it: closing that "dialog" would close
        // the main window. The accessible name may legitimately differ from
        // the X11 title, so agreeing geometry still proves identity.
        let title_conflicts =
            title.is_some_and(|title| only.title.as_deref().is_some_and(|actual| actual != title));
        let bounds_agree = bounds
            .zip(only.bounds)
            .is_some_and(|(expected, actual)| rect_near(actual, expected, 2));
        if title_conflicts && !bounds_agree {
            return Err(Error::Unsupported {
                feature: format!(
                    "native X11 window identity: the only top-level window of process {pid} \
                     is {:#x} titled {:?}, which matches neither the title {:?} nor the \
                     geometry of the target (the target's own window may be unmapped)",
                    only.window,
                    only.title.as_deref().unwrap_or_default(),
                    title.unwrap_or_default(),
                ),
            });
        }
        return Ok(only.window);
    }
    if candidates.is_empty() {
        return Err(Error::Unsupported {
            feature: format!(
                "native X11 window identity: no top-level client window has process id {pid}"
            ),
        });
    }

    if let Some(expected) = bounds {
        let by_bounds: Vec<&X11Candidate> = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .bounds
                    .is_some_and(|actual| rect_near(actual, expected, 2))
            })
            .collect();
        if by_bounds.len() == 1 {
            if title.is_some_and(|title| by_bounds[0].title.as_deref() != Some(title)) {
                return Err(Error::Unsupported { feature: format!("native X11 window identity is ambiguous: process {pid} has conflicting title and geometry") });
            }
            return Ok(by_bounds[0].window);
        }
        if by_bounds.len() > 1 {
            if let Some(title) = title {
                let by_title: Vec<&&X11Candidate> = by_bounds
                    .iter()
                    .filter(|candidate| candidate.title.as_deref() == Some(title))
                    .collect();
                if by_title.len() == 1 {
                    return Ok(by_title[0].window);
                }
            }
        }
    }

    // Title may disambiguate only after the authoritative PID filter. It is
    // never accepted as identity on its own.
    if let Some(title) = title {
        let by_title: Vec<&X11Candidate> = candidates
            .iter()
            .filter(|candidate| candidate.title.as_deref() == Some(title))
            .collect();
        if by_title.len() == 1 {
            return Ok(by_title[0].window);
        }
    }

    Err(Error::Unsupported {
        feature: format!(
            "native X11 window identity is ambiguous: process {pid} owns {} candidate windows",
            candidates.len()
        ),
    })
}

fn rect_near(actual: Rect, expected: Rect, tolerance: i32) -> bool {
    (actual.x - expected.x).abs() <= tolerance
        && (actual.y - expected.y).abs() <= tolerance
        && i64::from(actual.width).abs_diff(i64::from(expected.width)) <= tolerance as u64
        && i64::from(actual.height).abs_diff(i64::from(expected.height)) <= tolerance as u64
}

/// Tracks whether an earlier message of a multi-message window verb went out.
///
/// `minimize`, `maximize` and `enter_fullscreen` send two or three EWMH
/// requests. A failure on the first leaves the window untouched, so its error
/// keeps whatever disposition it has. A failure on a later one (the window's
/// identity lost between sends, a rejected request) follows a request the
/// window manager may already have carried out, so it is reported as
/// `Unknown` even when its variant alone would say `NotDelivered`.
#[derive(Default)]
struct Sent(bool);

impl Sent {
    fn step(&mut self, result: Result<()>) -> Result<()> {
        let already_sent = self.0;
        if result.is_ok() {
            self.0 = true;
        }
        if already_sent {
            result.map_err(|e| {
                crate::atspi::unknown_after_mutation(e, "an earlier window-manager request")
            })
        } else {
            result
        }
    }
}

fn send_client_message(
    conn: &RustConnection,
    root: Window,
    window: Window,
    type_: Atom,
    data: [u32; 5],
) -> Result<()> {
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_,
        data: ClientMessageData::from(data),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(platform)?
    .check()
    .map_err(platform)
}

/// Send an ICCCM `WM_PROTOCOLS` message to the client that owns `window`.
/// ICCCM §4.2.8 addresses it to the client window itself with an empty event
/// mask, which delivers it to the window's creator rather than to whoever
/// selects substructure redirection on the root.
fn send_wm_protocol(
    conn: &RustConnection,
    window: Window,
    wm_protocols: Atom,
    protocol: Atom,
) -> Result<()> {
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: wm_protocols,
        data: ClientMessageData::from([protocol, CURRENT_TIME, 0, 0, 0]),
    };
    conn.send_event(false, window, EventMask::NO_EVENT, event)
        .map_err(platform)?
        .check()
        .map_err(platform)
}

/// The event mask kept on every X11 top-level xa11y inspects: structure
/// changes (moves, reparenting, destruction) for bindings and cached origins,
/// plus, once [`X11WindowBackend::client_origin`] has run, property changes
/// so a rename invalidates a cached origin.
fn client_event_mask(origins_watched: bool) -> EventMask {
    if origins_watched {
        EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE
    } else {
        EventMask::STRUCTURE_NOTIFY
    }
}

/// The candidates owned by `pid` whose title is exactly `title`.
fn clients_by_pid_title(candidates: &[X11Candidate], pid: u32, title: &str) -> Vec<Window> {
    candidates
        .iter()
        .filter(|c| c.pid == pid && c.title.as_deref() == Some(title))
        .map(|c| c.window)
        .collect()
}

/// The one top-level owned by `pid` and titled exactly `title`, or `None`
/// when there is none or more than one. Geometry plays no part: it is what
/// the caller is computing.
fn select_client_by_pid_title(
    candidates: &[X11Candidate],
    pid: u32,
    title: &str,
) -> Option<Window> {
    match clients_by_pid_title(candidates, pid, title).as_slice() {
        [window] => Some(*window),
        _ => None,
    }
}

fn property_u32(
    conn: &RustConnection,
    window: Window,
    property: Atom,
    type_: Atom,
) -> Result<Vec<u32>> {
    let reply = conn
        .get_property(false, window, property, type_, 0, u32::MAX)
        .map_err(platform)?
        .reply()
        .map_err(platform)?;
    if reply.format == 0 {
        return Ok(Vec::new());
    }
    reply
        .value32()
        .map(|values| values.collect())
        .ok_or_else(|| Error::Platform {
            code: -1,
            message: format!("X11 property {property} did not contain 32-bit values"),
        })
}

fn property_bytes(
    conn: &RustConnection,
    window: Window,
    property: Atom,
    type_: Atom,
) -> Result<Vec<u8>> {
    conn.get_property(false, window, property, type_, 0, u32::MAX)
        .map_err(platform)?
        .reply()
        .map(|reply| reply.value)
        .map_err(platform)
}

fn platform(error: impl std::fmt::Display) -> Error {
    Error::Platform {
        code: -1,
        message: format!("X11 window management: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        choose_candidate, choose_sway_candidate, collect_sway_candidates,
        select_client_by_pid_title, select_close_mechanism, CloseMechanism, X11Candidate,
    };
    use xa11y_core::{Delivery, Error, Rect};

    #[test]
    fn a_failure_after_an_earlier_window_message_is_unknown() {
        let lost = || super::lost_identity("X11", 1);
        // The first message failed: nothing was sent, the variant decides.
        let mut sent = super::Sent::default();
        assert_eq!(
            sent.step(Err(lost())).unwrap_err().delivery(),
            Delivery::NotDelivered
        );
        // A later message failed after one went out: the window may have moved.
        let mut sent = super::Sent::default();
        sent.step(Ok(())).expect("first message");
        let err = sent.step(Err(lost())).unwrap_err();
        assert!(matches!(err, Error::Platform { .. }), "{err:?}");
        assert_eq!(err.delivery(), Delivery::Unknown);
    }

    #[test]
    fn close_prefers_the_window_managers_request_when_advertised() {
        // Whatever the client lists, an advertised _NET_CLOSE_WINDOW wins:
        // the ICCCM message is never a second attempt behind it.
        for client_accepts_delete in [false, true] {
            assert_eq!(
                select_close_mechanism(true, client_accepts_delete),
                Some(CloseMechanism::EwmhCloseWindow)
            );
        }
    }

    #[test]
    fn close_without_net_close_window_uses_icccm_only_for_willing_clients() {
        assert_eq!(
            select_close_mechanism(false, true),
            Some(CloseMechanism::IcccmDeleteWindow)
        );
        assert_eq!(select_close_mechanism(false, false), None);
    }

    fn candidate(window: u32, title: &str, bounds: Rect) -> X11Candidate {
        X11Candidate {
            window,
            pid: 7,
            title: Some(title.to_string()),
            bounds: Some(bounds),
        }
    }

    fn identity(window: u32, pid: u32, title: Option<&str>) -> X11Candidate {
        X11Candidate {
            window,
            pid,
            title: title.map(str::to_string),
            bounds: None,
        }
    }

    #[test]
    fn client_origin_needs_exactly_one_window_with_the_pid_and_title() {
        let one = [identity(1, 7, Some("Foo")), identity(2, 7, Some("Bar"))];
        assert_eq!(select_client_by_pid_title(&one, 7, "Foo"), Some(1));
        assert_eq!(
            select_client_by_pid_title(&[], 7, "Foo"),
            None,
            "no windows"
        );
        assert_eq!(
            select_client_by_pid_title(&one, 7, "Baz"),
            None,
            "no window has the title"
        );
        let twins = [identity(1, 7, Some("Foo")), identity(2, 7, Some("Foo"))];
        assert_eq!(
            select_client_by_pid_title(&twins, 7, "Foo"),
            None,
            "two same-titled windows of one process are ambiguous"
        );
    }

    #[test]
    fn client_origin_ignores_other_processes_and_untitled_windows() {
        // Another process's same-titled window neither matches nor makes
        // this one ambiguous.
        let candidates = [identity(1, 8, Some("Foo")), identity(2, 7, Some("Foo"))];
        assert_eq!(select_client_by_pid_title(&candidates, 7, "Foo"), Some(2));
        assert_eq!(
            select_client_by_pid_title(&[identity(1, 8, Some("Foo"))], 7, "Foo"),
            None
        );
        // A window with no readable title is never a match.
        let candidates = [identity(1, 7, None), identity(2, 7, Some("Foo"))];
        assert_eq!(select_client_by_pid_title(&candidates, 7, "Foo"), Some(2));
        assert_eq!(
            select_client_by_pid_title(&[identity(1, 7, None)], 7, ""),
            None
        );
    }

    #[test]
    fn client_origin_titles_match_exactly() {
        let candidates = [identity(1, 7, Some("Foo Bar"))];
        for title in ["Foo", "foo bar", "Foo Bar ", "FOO BAR"] {
            assert_eq!(
                select_client_by_pid_title(&candidates, 7, title),
                None,
                "{title:?} must not match \"Foo Bar\""
            );
        }
        assert_eq!(
            select_client_by_pid_title(&candidates, 7, "Foo Bar"),
            Some(1)
        );
    }

    #[test]
    fn one_pid_match_is_sufficient() {
        let candidates = [candidate(
            10,
            "Window",
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        )];
        assert_eq!(choose_candidate(7, None, None, &candidates).unwrap(), 10);
    }

    #[test]
    fn a_lone_candidate_that_contradicts_the_title_is_not_identity() {
        // A hidden Qt dialog keeps its AT-SPI node but has no mapped X window.
        // The process's only remaining top-level is the main window, and
        // binding the dialog to it made `close` on the hidden dialog quit the
        // whole app (PR #467's Qt CI failure).
        let main = Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let dialog = Rect {
            x: 336,
            y: 238,
            width: 108,
            height: 45,
        };
        let candidates = [candidate(10, "xa11y-qt-test-app", main)];
        let error = choose_candidate(7, Some("Sample Dialog"), Some(dialog), &candidates)
            .expect_err("a contradicting lone candidate must not bind");
        assert!(
            error.to_string().contains("Sample Dialog"),
            "the error names what was being resolved: {error}"
        );
        // Unknown geometry does not rescue a contradicting title.
        assert!(choose_candidate(7, Some("Sample Dialog"), None, &candidates).is_err());
        // Agreeing geometry does: the accessible name may legitimately differ
        // from the X11 title.
        assert_eq!(
            choose_candidate(7, Some("Renamed"), Some(main), &candidates).unwrap(),
            10
        );
        // A matching title, or no title at all, keeps the PID match sufficient.
        assert_eq!(
            choose_candidate(7, Some("xa11y-qt-test-app"), Some(dialog), &candidates).unwrap(),
            10
        );
        assert_eq!(
            choose_candidate(7, None, Some(dialog), &candidates).unwrap(),
            10
        );
    }

    #[test]
    fn same_process_windows_use_geometry_before_title() {
        let candidates = [
            candidate(
                10,
                "Same title",
                Rect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            ),
            candidate(
                11,
                "Same title",
                Rect {
                    x: 200,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            ),
        ];
        assert_eq!(
            choose_candidate(
                7,
                Some("Same title"),
                Some(Rect {
                    x: 201,
                    y: 0,
                    width: 100,
                    height: 100,
                }),
                &candidates,
            )
            .unwrap(),
            11
        );
    }

    #[test]
    fn title_is_never_used_without_pid_candidates() {
        let error = choose_candidate(7, Some("Unique"), None, &[]).unwrap_err();
        assert!(error.to_string().contains("process id 7"));
    }

    #[test]
    fn ambiguous_identity_fails_closed() {
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let candidates = [candidate(10, "Same", bounds), candidate(11, "Same", bounds)];
        let error = choose_candidate(7, Some("Same"), Some(bounds), &candidates).unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn conflicting_title_and_geometry_cannot_bind_a_different_window() {
        let a = candidate(
            10,
            "A",
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        );
        let b = candidate(
            11,
            "B",
            Rect {
                x: 200,
                y: 0,
                width: 100,
                height: 100,
            },
        );
        let error = choose_candidate(7, Some("A"), b.bounds, &[a, b]).unwrap_err();
        assert!(error.to_string().contains("conflicting title and geometry"));
    }

    #[test]
    fn sway_tree_covers_native_and_xwayland_toplevels() {
        let tree = serde_json::json!({
            "id": 1,
            "type": "root",
            "nodes": [{
                "id": 2,
                "type": "workspace",
                "nodes": [
                    {
                        "id": 20,
                        "type": "con",
                        "pid": 42,
                        "name": "Native",
                        "app_id": null,
                        "window": null,
                        "shell": "xdg_shell",
                        "floating": "auto_off",
                        "fullscreen_mode": 1,
                        "rect": {"x": 0, "y": 0, "width": 800, "height": 600},
                        "nodes": [],
                        "floating_nodes": []
                    },
                    {
                        "id": 21,
                        "type": "con",
                        "pid": 43,
                        "name": "XWayland",
                        "app_id": null,
                        "window": 1234,
                        "floating": "user_on",
                        "fullscreen_mode": 0,
                        "rect": {"x": 100, "y": 100, "width": 640, "height": 480},
                        "nodes": [],
                        "floating_nodes": []
                    }
                ],
                "floating_nodes": []
            }],
            "floating_nodes": []
        });
        let mut candidates = Vec::new();
        collect_sway_candidates(&tree, &mut candidates);
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].fullscreen);
        assert!(candidates[1].floating);
        assert_eq!(
            choose_sway_candidate(43, Some("XWayland"), None, &candidates[1..])
                .unwrap()
                .id,
            21
        );
    }

    #[test]
    fn sway_same_process_identity_fails_closed_when_indistinguishable() {
        let bounds = Rect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
        };
        let candidates = [
            super::SwayCandidate {
                id: 20,
                pid: 42,
                title: Some("Same".to_string()),
                bounds: Some(bounds),
                floating: false,
                fullscreen: false,
            },
            super::SwayCandidate {
                id: 21,
                pid: 42,
                title: Some("Same".to_string()),
                bounds: Some(bounds),
                floating: false,
                fullscreen: false,
            },
        ];
        let error = choose_sway_candidate(42, Some("Same"), Some(bounds), &candidates)
            .expect_err("indistinguishable Sway windows must not be guessed");
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn retained_sway_identity_survives_movement_and_rename_but_not_replacement() {
        let original = super::SwayCandidate {
            id: 20,
            pid: 42,
            title: Some("A".to_string()),
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 300,
                height: 200,
            }),
            floating: true,
            fullscreen: false,
        };
        let mut other = original.clone();
        other.id = 21;
        other.title = Some("B".to_string());
        other.bounds = Some(Rect {
            x: 400,
            ..original.bounds.expect("bounds")
        });
        let binding = super::NativeBinding {
            id: original.id,
            pid: original.pid,
            process_start: 1,
            server_identity: super::ServerIdentity::Sway {
                pid: 1,
                process_start: 1,
            },
        };
        let mut moved = original.clone();
        moved.title = Some("Renamed".to_string());
        moved.bounds = other.bounds;
        other.bounds = original.bounds;
        other.title = moved.title.clone();
        assert_eq!(
            super::bound_sway_candidate(binding, 1, &[other.clone(), moved])
                .unwrap()
                .id,
            20
        );
        assert!(super::bound_sway_candidate(binding, 1, &[other.clone()]).is_err());
        let mut replacement = other;
        replacement.id = 22;
        assert!(super::bound_sway_candidate(binding, 1, &[replacement]).is_err());
    }

    #[test]
    fn sway_action_uses_bound_id_after_windows_swap_positions() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let socket = std::env::temp_dir().join(format!(
            "xa11y-sway-identity-{}-{}.sock",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let listener = UnixListener::bind(&socket).expect("bind fake Sway IPC socket");
        let pid = std::process::id();
        let tree = move |swapped: bool| {
            let positions = if swapped { [400, 0] } else { [0, 400] };
            serde_json::json!({
                "id": 1, "type": "root", "nodes": [
                    {"id": 20, "type": "con", "pid": pid, "name": "Same",
                     "shell": "xdg_shell", "floating": "user_on", "fullscreen_mode": 0,
                     "rect": {"x": positions[0], "y": 0, "width": 300, "height": 200}},
                    {"id": 21, "type": "con", "pid": pid, "name": "Same",
                     "shell": "xdg_shell", "floating": "user_on", "fullscreen_mode": 0,
                     "rect": {"x": positions[1], "y": 0, "width": 300, "height": 200}}
                ]
            })
        };
        let server = std::thread::spawn(move || {
            for (index, expected_type) in [4_u32, 4, 0].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().expect("Sway IPC request");
                let mut header = [0_u8; 14];
                stream.read_exact(&mut header).expect("request header");
                assert_eq!(&header[..6], b"i3-ipc");
                let length = u32::from_le_bytes(header[6..10].try_into().expect("length"));
                let message_type = u32::from_le_bytes(header[10..14].try_into().expect("type"));
                assert_eq!(message_type, expected_type);
                let mut payload = vec![0_u8; length as usize];
                stream.read_exact(&mut payload).expect("request payload");
                let response = if index == 2 {
                    assert_eq!(payload, b"[con_id=20] focus");
                    serde_json::json!([{"success": true}])
                } else {
                    tree(index == 1)
                };
                let bytes = serde_json::to_vec(&response).expect("response JSON");
                let mut reply = Vec::from(&b"i3-ipc"[..]);
                reply.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                reply.extend_from_slice(&message_type.to_le_bytes());
                reply.extend_from_slice(&bytes);
                stream.write_all(&reply).expect("Sway IPC reply");
            }
        });
        let backend = super::SwayWindowBackend {
            socket: socket.clone(),
            protocol_version: 1,
            bindings: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let mut element = xa11y_core::ElementData::for_role(xa11y_core::Role::Window);
        element.handle = 71;
        element.pid = Some(pid);
        element.name = Some("Same".to_string());
        element.bounds = Some(Rect {
            x: 0,
            y: 0,
            width: 300,
            height: 200,
        });
        backend.bind(&element).expect("bind first Sway window");
        backend
            .activate(&element)
            .expect("activate retained window");
        server.join().expect("fake Sway IPC server");
        std::fs::remove_file(socket).expect("remove fake socket");
    }

    #[test]
    fn sway_rejects_changed_session_before_sending_action() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let socket =
            std::env::temp_dir().join(format!("xa11y-sway-session-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket).expect("bind fake Sway IPC socket");
        let pid = std::process::id();
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().expect("Sway IPC connection");
                if index == 1 {
                    let mut first = [0_u8; 1];
                    assert_eq!(
                        stream
                            .read(&mut first)
                            .expect("read after identity rejection"),
                        0,
                        "no command may be sent to a different compositor session"
                    );
                    break;
                }
                let mut header = [0_u8; 14];
                stream.read_exact(&mut header).expect("tree request header");
                let tree = serde_json::json!({"id": 1, "type": "root", "nodes": [
                    {"id": 20, "type": "con", "pid": pid, "name": "A", "shell": "xdg_shell",
                     "rect": {"x": 0, "y": 0, "width": 300, "height": 200}}
                ]});
                let bytes = serde_json::to_vec(&tree).expect("tree JSON");
                let mut reply = Vec::from(&b"i3-ipc"[..]);
                reply.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                reply.extend_from_slice(&4_u32.to_le_bytes());
                reply.extend_from_slice(&bytes);
                stream.write_all(&reply).expect("tree reply");
            }
        });
        let backend = super::SwayWindowBackend {
            socket: socket.clone(),
            protocol_version: 1,
            bindings: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let mut element = xa11y_core::ElementData::for_role(xa11y_core::Role::Window);
        element.handle = 72;
        element.pid = Some(pid);
        element.name = Some("A".to_string());
        backend.bind(&element).expect("bind window");
        let binding = backend.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let mut wrong = binding
            .get(&element.handle)
            .copied()
            .expect("stored binding");
        drop(binding);
        wrong.server_identity = super::ServerIdentity::Sway {
            pid: 1,
            process_start: 1,
        };
        backend
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(element.handle, wrong);
        let error = backend
            .activate(&element)
            .expect_err("different session must be rejected");
        assert!(error.to_string().contains("session changed"));
        server.join().expect("fake Sway IPC server");
        std::fs::remove_file(socket).expect("remove fake socket");
    }

    /// `active_window_pid` reads the pid of `_NET_ACTIVE_WINDOW`, and says
    /// nothing when there is no active window or the WM does not support it.
    #[test]
    #[ignore = "requires Xvfb; run through scripts/run_integ_tests.sh"]
    fn x11_active_window_pid_reads_the_ewmh_active_window() {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
        };
        use x11rb::wrapper::ConnectionExt as _;

        let (_xvfb, display) = isolated_xvfb();
        let (observer, screen) = x11rb::rust_connection::RustConnection::connect(Some(&display))
            .expect("connect isolated Xvfb");
        let root = observer.setup().roots[screen].root;
        let connect = || {
            x11rb::rust_connection::RustConnection::connect(Some(&display))
                .expect("connect backend to isolated Xvfb")
                .0
        };
        let backend_conn = connect();
        let atoms = super::X11Atoms::new(&backend_conn).expect("X11 atoms");
        let backend = |conn, supported: &[u32]| super::X11WindowBackend {
            conn: std::sync::Mutex::new(conn),
            origins: std::sync::Mutex::new(std::collections::HashMap::new()),
            bindings: std::sync::Mutex::new(std::collections::HashMap::new()),
            destroyed_handles: std::sync::Mutex::new(std::collections::HashSet::new()),
            root,
            atoms,
            supported: supported.iter().copied().collect(),
            has_window_manager: true,
            origins_watched: std::sync::atomic::AtomicBool::new(false),
        };
        let ewmh = backend(backend_conn, &[atoms.net_active_window]);
        let set_active = |window: u32| {
            observer
                .change_property32(
                    PropMode::REPLACE,
                    root,
                    atoms.net_active_window,
                    AtomEnum::WINDOW,
                    &[window],
                )
                .expect("set _NET_ACTIVE_WINDOW")
                .check()
                .expect("_NET_ACTIVE_WINDOW stored");
        };
        let window = observer.generate_id().expect("XID");
        observer
            .create_window(
                0,
                window,
                root,
                0,
                0,
                300,
                200,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .expect("create window")
            .check()
            .expect("created window");
        observer
            .change_property32(
                PropMode::REPLACE,
                window,
                atoms.net_wm_pid,
                AtomEnum::CARDINAL,
                &[4242],
            )
            .expect("set PID")
            .check()
            .expect("PID stored");

        assert_eq!(
            ewmh.active_window_pid().unwrap(),
            None,
            "no _NET_ACTIVE_WINDOW yet"
        );
        set_active(window);
        assert_eq!(ewmh.active_window_pid().unwrap(), Some(4242));
        set_active(0);
        assert_eq!(
            ewmh.active_window_pid().unwrap(),
            None,
            "window 0 means no active window"
        );
        set_active(window);
        let no_ewmh = backend(connect(), &[]);
        assert_eq!(
            no_ewmh.active_window_pid().unwrap(),
            None,
            "a WM without _NET_ACTIVE_WINDOW support is not asked"
        );
    }

    #[test]
    #[ignore = "requires Xvfb; run through scripts/run_integ_tests.sh"]
    fn x11_retained_action_survives_swap_and_rejects_reused_xid() {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::ConnectionExt as _;
        use x11rb::protocol::xproto::{
            ChangeWindowAttributesAux, ConfigureWindowAux, CreateWindowAux, WindowClass,
        };
        use x11rb::wrapper::ConnectionExt as _;

        let (_xvfb, display) = isolated_xvfb();
        let (observer, screen) = x11rb::rust_connection::RustConnection::connect(Some(&display))
            .expect("connect isolated Xvfb");
        let (backend_conn, backend_screen) =
            x11rb::rust_connection::RustConnection::connect(Some(&display))
                .expect("connect backend to isolated Xvfb");
        assert_eq!(screen, backend_screen);
        let root = observer.setup().roots[screen].root;
        let atoms = super::X11Atoms::new(&backend_conn).expect("X11 atoms");
        let pid = std::process::id();
        let a = observer.generate_id().expect("XID A");
        let b = observer.generate_id().expect("XID B");
        let create = |window, x| {
            observer
                .create_window(
                    0,
                    window,
                    root,
                    x,
                    0,
                    300,
                    200,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new(),
                )
                .expect("create window")
                .check()
                .expect("created window");
            observer
                .change_property32(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    window,
                    atoms.net_wm_pid,
                    x11rb::protocol::xproto::AtomEnum::CARDINAL,
                    &[pid],
                )
                .expect("set PID")
                .check()
                .expect("PID stored");
            observer
                .change_property8(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    window,
                    atoms.net_wm_name,
                    atoms.utf8_string,
                    b"Same",
                )
                .expect("set title")
                .check()
                .expect("title stored");
        };
        create(a, 0);
        create(b, 400);
        observer
            .change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                root,
                atoms.net_client_list,
                x11rb::protocol::xproto::AtomEnum::WINDOW,
                &[a, b],
            )
            .expect("client list")
            .check()
            .expect("client list stored");
        observer
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new()
                    .event_mask(x11rb::protocol::xproto::EventMask::SUBSTRUCTURE_NOTIFY),
            )
            .expect("watch root")
            .check()
            .expect("root watched");
        observer.flush().expect("flush setup");
        let backend = super::X11WindowBackend {
            conn: std::sync::Mutex::new(backend_conn),
            origins: std::sync::Mutex::new(std::collections::HashMap::new()),
            bindings: std::sync::Mutex::new(std::collections::HashMap::new()),
            destroyed_handles: std::sync::Mutex::new(std::collections::HashSet::new()),
            root,
            atoms,
            supported: [atoms.net_active_window].into_iter().collect(),
            has_window_manager: true,
            origins_watched: std::sync::atomic::AtomicBool::new(false),
        };
        let element = |handle, x| {
            let mut data = xa11y_core::ElementData::for_role(xa11y_core::Role::Window);
            data.handle = handle;
            data.pid = Some(pid);
            data.name = Some("Same".to_string());
            data.bounds = Some(Rect {
                x,
                y: 0,
                width: 300,
                height: 200,
            });
            data
        };
        let retained_a = element(101, 0);
        let retained_b = element(102, 400);
        backend.facts(&retained_a).expect("bind A");
        backend.facts(&retained_b).expect("bind B");
        observer
            .configure_window(a, &ConfigureWindowAux::new().x(400))
            .expect("move A")
            .check()
            .expect("A moved");
        observer
            .configure_window(b, &ConfigureWindowAux::new().x(0))
            .expect("move B")
            .check()
            .expect("B moved");
        observer.flush().expect("flush movement");
        backend
            .activate(&retained_a)
            .expect("activate original A after swap");
        let target = loop {
            if let x11rb::protocol::Event::ClientMessage(event) =
                observer.wait_for_event().expect("native event")
            {
                break event.window;
            }
        };
        assert_eq!(
            target, a,
            "retained A must not activate B at A's old position"
        );
        observer
            .destroy_window(a)
            .expect("destroy A")
            .check()
            .expect("A destroyed");
        create(a, 400);
        observer.flush().expect("flush XID replacement");
        let error = backend
            .activate(&retained_a)
            .expect_err("reused XID must be tombstoned");
        assert!(error.to_string().contains("identity was lost"));
        // A fresh accessibility snapshot may bind the replacement separately.
        backend
            .facts(&element(103, 400))
            .expect("bind replacement from a fresh handle");
    }

    /// An Xvfb private to one test, killed on drop. It runs no window
    /// manager, so it is exactly the "bare X server" case of issue #464.
    struct Xvfb(std::process::Child);
    impl Drop for Xvfb {
        fn drop(&mut self) {
            // Teardown of a test fixture: a failed kill means it already exited.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn isolated_xvfb() -> (Xvfb, String) {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut xvfb = Xvfb(
            Command::new("Xvfb")
                .args([
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "1024x768x24",
                    "-nolisten",
                    "tcp",
                    "-ac",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("start isolated Xvfb"),
        );
        let mut display_number = String::new();
        BufReader::new(xvfb.0.stdout.take().expect("Xvfb displayfd"))
            .read_line(&mut display_number)
            .expect("read Xvfb display number");
        // The local socket, as the harness's own Xvfb uses: TCP needs a loopback
        // listener that sandboxed runners do not always permit.
        let display = format!(":{}", display_number.trim());
        (xvfb, display)
    }

    fn intern(conn: &x11rb::rust_connection::RustConnection, name: &[u8]) -> u32 {
        use x11rb::protocol::xproto::ConnectionExt as _;
        conn.intern_atom(false, name)
            .expect("intern atom")
            .reply()
            .expect("atom reply")
            .atom
    }

    #[test]
    #[ignore = "requires Xvfb; run through scripts/run_integ_tests.sh"]
    fn x11_close_without_window_manager_sends_icccm_delete_window() {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
        };
        use x11rb::wrapper::ConnectionExt as _;

        let (_xvfb, display) = isolated_xvfb();
        let (observer, screen) = x11rb::rust_connection::RustConnection::connect(Some(&display))
            .expect("connect isolated Xvfb");
        let root = observer.setup().roots[screen].root;
        let backend =
            super::X11WindowBackend::connect(Some(&display)).expect("X11 backend on bare Xvfb");
        assert!(
            !backend.has_window_manager && backend.supported.is_empty(),
            "fixture: the isolated Xvfb must run no window manager"
        );
        let net_wm_pid = intern(&observer, b"_NET_WM_PID");
        let net_wm_name = intern(&observer, b"_NET_WM_NAME");
        let utf8_string = intern(&observer, b"UTF8_STRING");
        let wm_protocols = intern(&observer, b"WM_PROTOCOLS");
        let wm_delete_window = intern(&observer, b"WM_DELETE_WINDOW");
        let wm_take_focus = intern(&observer, b"WM_TAKE_FOCUS");
        let pid = std::process::id();

        // Two mapped top-levels of this process, told apart by geometry. Only
        // the first participates in WM_DELETE_WINDOW.
        let create = |x: i16, title: &[u8], protocols: &[u32]| {
            let window = observer.generate_id().expect("XID");
            observer
                .create_window(
                    0,
                    window,
                    root,
                    x,
                    0,
                    300,
                    200,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new(),
                )
                .expect("create window")
                .check()
                .expect("created window");
            observer
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    net_wm_pid,
                    AtomEnum::CARDINAL,
                    &[pid],
                )
                .expect("set PID")
                .check()
                .expect("PID stored");
            observer
                .change_property8(PropMode::REPLACE, window, net_wm_name, utf8_string, title)
                .expect("set title")
                .check()
                .expect("title stored");
            if !protocols.is_empty() {
                observer
                    .change_property32(
                        PropMode::REPLACE,
                        window,
                        wm_protocols,
                        AtomEnum::ATOM,
                        protocols,
                    )
                    .expect("set WM_PROTOCOLS")
                    .check()
                    .expect("WM_PROTOCOLS stored");
            }
            observer
                .map_window(window)
                .expect("map window")
                .check()
                .expect("window mapped");
            window
        };
        let deletable = create(0, b"Deletable", &[wm_take_focus, wm_delete_window]);
        let undeletable = create(400, b"Undeletable", &[wm_take_focus]);
        observer.flush().expect("flush setup");

        let element = |handle, x, title: &str| {
            let mut data = xa11y_core::ElementData::for_role(xa11y_core::Role::Window);
            data.handle = handle;
            data.pid = Some(pid);
            data.name = Some(title.to_string());
            data.bounds = Some(Rect {
                x,
                y: 0,
                width: 300,
                height: 200,
            });
            data
        };
        let deletable_element = element(201, 0, "Deletable");
        let undeletable_element = element(202, 400, "Undeletable");

        // Capability reporting follows the mechanism: advertised only where
        // the client accepts WM_DELETE_WINDOW.
        let facts = backend
            .facts(&deletable_element)
            .expect("bind a top-level without a window manager");
        assert!(
            facts.actions.contains(&"close"),
            "close must be advertised for a WM_DELETE_WINDOW client: {:?}",
            facts.actions
        );
        assert_eq!(
            facts.actions,
            vec!["close"],
            "nothing else works without a WM"
        );
        assert_eq!(facts.backend, Some("x11-icccm"));
        let facts = backend
            .facts(&undeletable_element)
            .expect("bind the second top-level");
        assert!(
            !facts.actions.contains(&"close"),
            "close must not be advertised without WM_DELETE_WINDOW: {:?}",
            facts.actions
        );

        backend
            .close(&deletable_element)
            .expect("close must use ICCCM WM_DELETE_WINDOW without a window manager");
        let message = loop {
            if let x11rb::protocol::Event::ClientMessage(event) =
                observer.wait_for_event().expect("native event")
            {
                break event;
            }
        };
        assert_eq!(message.window, deletable, "sent to the bound window");
        assert_eq!(message.type_, wm_protocols, "an ICCCM WM_PROTOCOLS message");
        assert_eq!(message.format, 32);
        assert_eq!(message.data.as_data32()[0], wm_delete_window);

        let error = backend
            .close(&undeletable_element)
            .expect_err("no mechanism is available for this client");
        assert!(
            matches!(error, xa11y_core::Error::Unsupported { .. }),
            "got {error:?}"
        );
        let text = error.to_string();
        for needle in ["_NET_CLOSE_WINDOW", "no window manager", "WM_DELETE_WINDOW"] {
            assert!(text.contains(needle), "{needle:?} missing from: {text}");
        }
        // Nothing reached the undeletable client: no substitute was tried.
        observer
            .get_input_focus()
            .expect("round trip")
            .reply()
            .expect("round trip reply");
        while let Some(event) = observer.poll_for_event().expect("poll") {
            if let x11rb::protocol::Event::ClientMessage(event) = event {
                panic!(
                    "unexpected ClientMessage to {:#x} (undeletable is {undeletable:#x})",
                    event.window
                );
            }
        }
    }

    /// `client_origin` skips the shadow GTK draws inside its client window,
    /// and follows a change of `_GTK_FRAME_EXTENTS`.
    #[test]
    #[ignore = "requires Xvfb; run through scripts/run_integ_tests.sh"]
    fn x11_client_origin_adds_the_gtk_shadow() {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
        };
        use x11rb::wrapper::ConnectionExt as _;

        let (_xvfb, display) = isolated_xvfb();
        let (observer, screen) = x11rb::rust_connection::RustConnection::connect(Some(&display))
            .expect("connect isolated Xvfb");
        let root = observer.setup().roots[screen].root;
        let backend =
            super::X11WindowBackend::connect(Some(&display)).expect("X11 backend on bare Xvfb");
        let net_wm_pid = intern(&observer, b"_NET_WM_PID");
        let net_wm_name = intern(&observer, b"_NET_WM_NAME");
        let utf8_string = intern(&observer, b"UTF8_STRING");
        let gtk_frame_extents = intern(&observer, b"_GTK_FRAME_EXTENTS");
        let pid = std::process::id();
        let sync = || {
            observer
                .get_input_focus()
                .expect("round trip")
                .reply()
                .expect("round trip reply");
        };
        let window = observer.generate_id().expect("XID");
        observer
            .create_window(
                0,
                window,
                root,
                100,
                60,
                300,
                200,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .expect("create window")
            .check()
            .expect("created window");
        observer
            .change_property32(
                PropMode::REPLACE,
                window,
                net_wm_pid,
                AtomEnum::CARDINAL,
                &[pid],
            )
            .expect("set PID")
            .check()
            .expect("PID stored");
        observer
            .change_property8(PropMode::REPLACE, window, net_wm_name, utf8_string, b"Calc")
            .expect("set title")
            .check()
            .expect("title stored");
        let shadow = |extents: &[u32]| {
            observer
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    gtk_frame_extents,
                    AtomEnum::CARDINAL,
                    extents,
                )
                .expect("set _GTK_FRAME_EXTENTS")
                .check()
                .expect("_GTK_FRAME_EXTENTS stored");
            sync();
        };
        observer
            .map_window(window)
            .expect("map window")
            .check()
            .expect("window mapped");
        sync();

        assert_eq!(
            backend.client_origin(pid, "Calc").unwrap(),
            Some((100, 60)),
            "a window without _GTK_FRAME_EXTENTS has no shadow"
        );
        // gnome-calculator's shadow, then gtk4-widget-factory's.
        shadow(&[61, 61, 55, 67]);
        assert_eq!(
            backend.client_origin(pid, "Calc").unwrap(),
            Some((161, 115)),
            "the origin must start inside the left and top shadow"
        );
        shadow(&[14, 14, 12, 17]);
        assert_eq!(
            backend.client_origin(pid, "Calc").unwrap(),
            Some((114, 72)),
            "a shadow change must drop the cached origin"
        );
        shadow(&[1, 2, 3]);
        assert!(
            backend.client_origin(pid, "Calc").is_err(),
            "a malformed _GTK_FRAME_EXTENTS is an error, not a zero shadow"
        );
    }

    /// `client_origin` against a real X server: the origin follows a move,
    /// two same-titled windows are ambiguous, and neither a rename nor a new
    /// window can leave a cached origin answering for the wrong window.
    #[test]
    #[ignore = "requires Xvfb; run through scripts/run_integ_tests.sh"]
    fn x11_client_origin_is_unique_and_invalidated() {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{
            AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, PropMode,
            WindowClass,
        };
        use x11rb::wrapper::ConnectionExt as _;

        let (_xvfb, display) = isolated_xvfb();
        let (observer, screen) = x11rb::rust_connection::RustConnection::connect(Some(&display))
            .expect("connect isolated Xvfb");
        let root = observer.setup().roots[screen].root;
        let backend =
            super::X11WindowBackend::connect(Some(&display)).expect("X11 backend on bare Xvfb");
        assert!(
            !backend.has_window_manager,
            "fixture: the isolated Xvfb must run no window manager"
        );
        let net_wm_pid = intern(&observer, b"_NET_WM_PID");
        let net_wm_name = intern(&observer, b"_NET_WM_NAME");
        let utf8_string = intern(&observer, b"UTF8_STRING");
        let pid = std::process::id();
        let sync = || {
            observer
                .get_input_focus()
                .expect("round trip")
                .reply()
                .expect("round trip reply");
        };
        let rename = |window: u32, title: &[u8]| {
            observer
                .change_property8(PropMode::REPLACE, window, net_wm_name, utf8_string, title)
                .expect("set title")
                .check()
                .expect("title stored");
        };
        let create = |x: i16, y: i16, title: &[u8]| {
            let window = observer.generate_id().expect("XID");
            observer
                .create_window(
                    0,
                    window,
                    root,
                    x,
                    y,
                    300,
                    200,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new(),
                )
                .expect("create window")
                .check()
                .expect("created window");
            observer
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    net_wm_pid,
                    AtomEnum::CARDINAL,
                    &[pid],
                )
                .expect("set PID")
                .check()
                .expect("PID stored");
            rename(window, title);
            observer
                .map_window(window)
                .expect("map window")
                .check()
                .expect("window mapped");
            sync();
            window
        };
        let moved = |window: u32, x: i32, y: i32| {
            observer
                .configure_window(window, &ConfigureWindowAux::new().x(x).y(y))
                .expect("move")
                .check()
                .expect("moved");
            sync();
        };

        // The root mask is selected lazily: a backend that never looks up a
        // client origin queues no root events.
        let root_mask = || {
            let conn = backend.conn.lock().unwrap_or_else(|e| e.into_inner());
            conn.get_window_attributes(root)
                .expect("root attributes")
                .reply()
                .expect("root attributes reply")
                .your_event_mask
        };
        assert_eq!(
            root_mask(),
            x11rb::protocol::xproto::EventMask::NO_EVENT,
            "connect must not select a root mask"
        );

        let a = create(10, 20, b"Foo");
        let client_mask = |window: u32| {
            let conn = backend.conn.lock().unwrap_or_else(|e| e.into_inner());
            conn.get_window_attributes(window)
                .expect("client attributes")
                .reply()
                .expect("client attributes reply")
                .your_event_mask
        };
        // `bind` watches a window before any origin lookup: structure only.
        {
            let conn = backend.conn.lock().unwrap_or_else(|e| e.into_inner());
            backend.watch_clients(&conn, &[a]).expect("watch A");
        }
        assert_eq!(
            client_mask(a),
            x11rb::protocol::xproto::EventMask::STRUCTURE_NOTIFY,
            "before any lookup a watched client gets no property events"
        );
        assert_eq!(backend.client_origin(pid, "Foo").unwrap(), Some((10, 20)));
        assert!(
            client_mask(a).contains(x11rb::protocol::xproto::EventMask::PROPERTY_CHANGE),
            "the first lookup must upgrade an already-watched client's mask"
        );
        assert_eq!(
            root_mask(),
            x11rb::protocol::xproto::EventMask::PROPERTY_CHANGE
                | x11rb::protocol::xproto::EventMask::SUBSTRUCTURE_NOTIFY,
            "the first lookup must select the root mask"
        );
        assert_eq!(
            backend.client_origin(pid + 1, "Foo").unwrap(),
            None,
            "another pid's lookup must not see this window"
        );

        // A move invalidates the cached origin.
        moved(a, 50, 60);
        assert_eq!(
            backend.client_origin(pid, "Foo").unwrap(),
            Some((50, 60)),
            "the origin must follow a move"
        );

        // A second window with the same title makes "Foo" ambiguous, even
        // though "Foo" was cached before it appeared.
        let b = create(400, 300, b"Foo");
        assert_eq!(
            backend.client_origin(pid, "Foo").unwrap(),
            None,
            "two same-titled windows must give no origin"
        );

        // Renaming B away resolves it again, to A.
        rename(b, b"Other");
        sync();
        assert_eq!(backend.client_origin(pid, "Foo").unwrap(), Some((50, 60)));
        assert_eq!(
            backend.client_origin(pid, "Other").unwrap(),
            Some((400, 300))
        );

        // A rename that touches neither a cached window nor a cached title
        // leaves the other entries alone.
        rename(b, b"Unrelated");
        sync();
        // Looking up B's new title drains the rename before the checks.
        assert_eq!(
            backend.client_origin(pid, "Unrelated").unwrap(),
            Some((400, 300))
        );
        assert!(
            backend
                .origins
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&(pid, "Foo".to_owned())),
            "an unrelated rename must keep A's cached origin"
        );
        assert!(
            !backend
                .origins
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&(pid, "Other".to_owned())),
            "the renamed window's own entry must go"
        );
        rename(b, b"Other");
        sync();

        // A is cached as "Foo". Renaming A away and B to "Foo" must not
        // leave A's origin answering for "Foo".
        rename(a, b"Renamed");
        rename(b, b"Foo");
        sync();
        assert_eq!(
            backend.client_origin(pid, "Foo").unwrap(),
            Some((400, 300)),
            "a rename must invalidate the cached origin"
        );

        // A cached window renamed so that another window shares its title,
        // without any change to the cached window itself.
        assert_eq!(
            backend.client_origin(pid, "Renamed").unwrap(),
            Some((50, 60))
        );
        rename(b, b"Renamed");
        sync();
        assert_eq!(
            backend.client_origin(pid, "Renamed").unwrap(),
            None,
            "a window renamed to a cached title makes it ambiguous"
        );

        // A destroyed window's origin is gone with it.
        observer
            .destroy_window(b)
            .expect("destroy B")
            .check()
            .expect("B destroyed");
        sync();
        assert_eq!(
            backend.client_origin(pid, "Renamed").unwrap(),
            Some((50, 60))
        );
        observer
            .destroy_window(a)
            .expect("destroy A")
            .check()
            .expect("A destroyed");
        sync();
        assert_eq!(backend.client_origin(pid, "Renamed").unwrap(), None);

        // A _NET_WM_PID change onto a cached (pid, title) makes it ambiguous,
        // with no change to the cached window itself.
        let set_pid = |window: u32, value: u32| {
            observer
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    net_wm_pid,
                    AtomEnum::CARDINAL,
                    &[value],
                )
                .expect("set PID")
                .check()
                .expect("PID stored");
            sync();
        };
        let _c = create(30, 40, b"Pid");
        let d = create(500, 400, b"Pid");
        set_pid(d, pid + 1);
        assert_eq!(backend.client_origin(pid, "Pid").unwrap(), Some((30, 40)));
        set_pid(d, pid);
        assert_eq!(
            backend.client_origin(pid, "Pid").unwrap(),
            None,
            "a window whose pid changes onto a cached key makes it ambiguous"
        );
    }

    /// The real thing: the AccessKit + winit test app on a bare X server, with
    /// no window manager to forward a close. winit lists WM_DELETE_WINDOW and
    /// turns it into `CloseRequested`, on which the test app exits.
    #[test]
    #[ignore = "requires Xvfb and a built xa11y-test-app; run through scripts/run_integ_tests.sh"]
    fn x11_close_without_window_manager_closes_winit_test_app() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let app_bin = std::env::var_os("XA11Y_TEST_APP_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../target/debug/xa11y-test-app")
            });
        assert!(
            app_bin.exists(),
            "{} not found: build the workspace first (or set XA11Y_TEST_APP_BIN)",
            app_bin.display()
        );
        let (_xvfb, display) = isolated_xvfb();
        struct App(std::process::Child);
        impl Drop for App {
            fn drop(&mut self) {
                // Teardown: the app normally exited already; kill is a no-op then.
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut app = App(Command::new(&app_bin)
            .arg("--headless")
            .env("DISPLAY", &display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn xa11y-test-app"));
        let backend =
            super::X11WindowBackend::connect(Some(&display)).expect("X11 backend on bare Xvfb");
        assert!(
            !backend.has_window_manager,
            "fixture: the isolated Xvfb must run no window manager"
        );
        let mut element = xa11y_core::ElementData::for_role(xa11y_core::Role::Window);
        element.handle = 301;
        element.pid = Some(app.0.id());

        let deadline = Instant::now() + Duration::from_secs(15);
        let facts = loop {
            match backend.facts(&element) {
                Ok(facts) => break facts,
                // The window maps asynchronously after spawn; until then no
                // top-level has the app's pid.
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => panic!("test app window never bound: {error}"),
            }
        };
        assert!(
            facts.actions.contains(&"close"),
            "winit lists WM_DELETE_WINDOW, so close must be advertised: {:?}",
            facts.actions
        );
        backend
            .close(&element)
            .expect("close must be accepted without a window manager");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = app.0.try_wait().expect("poll test app") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "test app still running 10s after close()"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(status.success(), "test app exited with {status}");
    }
}
