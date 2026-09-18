//! Native Linux window management.
//!
//! Accessibility remains the source of the element tree. This module owns
//! the separate window-manager channel and resolves an AT-SPI top-level to a
//! native window before it advertises or performs a mutation. Identity is
//! never inferred from a title alone: X11 first requires an exact process id,
//! then disambiguates same-process windows with live geometry and title.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection as WaylandConnection, Dispatch, QueueHandle};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask, Window,
    CLIENT_MESSAGE_EVENT,
};
use x11rb::rust_connection::RustConnection;
use xa11y_core::{ElementData, Error, Point, Rect, Result};

use crate::session::{select_backend, DesktopBackend};

const SOURCE_APPLICATION: u32 = 1;
const ICONIC_STATE: u32 = 3;
const STATE_REMOVE: u32 = 0;
const STATE_ADD: u32 = 1;

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
        })
    }

    fn facts(&self, element: &ElementData) -> Result<WindowFacts> {
        if self.protocol_version == 0 {
            return Err(Error::Unsupported {
                feature: "native Wayland window management: invalid wlr foreign-toplevel protocol version 0"
                    .to_string(),
            });
        }
        let target = self.resolve(element)?;
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
        self.command(target.id, &format!("move position {x} {y}"))
    }

    fn resize_to(&self, element: &ElementData, width: u32, height: u32) -> Result<()> {
        let target = self.resolve(element)?;
        if !target.floating {
            return Err(Error::Unsupported {
                feature: "resize_to: Sway only sizes floating windows".to_string(),
            });
        }
        self.command(
            target.id,
            &format!("resize set width {width} px height {height} px"),
        )
    }

    fn command_for(&self, element: &ElementData, command: &str) -> Result<()> {
        let target = self.resolve(element)?;
        self.command(target.id, command)
    }

    fn command(&self, id: u64, command: &str) -> Result<()> {
        let payload = format!("[con_id={id}] {command}");
        let reply = self.request(SWAY_IPC_COMMAND, payload.as_bytes())?;
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

    fn resolve(&self, element: &ElementData) -> Result<SwayCandidate> {
        let pid = element.pid.ok_or_else(|| Error::Unsupported {
            feature: "native Wayland window identity: AT-SPI target has no process id".to_string(),
        })?;
        let tree = self.request(SWAY_IPC_GET_TREE, &[])?;
        let mut candidates = Vec::new();
        collect_sway_candidates(&tree, &mut candidates);
        candidates.retain(|candidate| candidate.pid == pid);
        choose_sway_candidate(pid, element.name.as_deref(), element.bounds, &candidates)
    }

    fn request(&self, message_type: u32, payload: &[u8]) -> Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket).map_err(|error| Error::Platform {
            code: -1,
            message: format!("connect Sway IPC {}: {error}", self.socket.display()),
        })?;
        let length = u32::try_from(payload.len()).map_err(|_| Error::InvalidActionData {
            message: "Sway IPC request is too large".to_string(),
        })?;
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
        serde_json::from_slice(&response).map_err(|error| Error::Platform {
            code: -1,
            message: format!("decode Sway IPC JSON: {error}"),
        })
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
    net_wm_state: Atom,
    net_wm_state_hidden: Atom,
    net_wm_state_max_horz: Atom,
    net_wm_state_max_vert: Atom,
    net_wm_state_fullscreen: Atom,
    net_active_window: Atom,
    net_close_window: Atom,
    net_moveresize_window: Atom,
    wm_change_state: Atom,
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
            net_wm_state: atom(conn, b"_NET_WM_STATE")?,
            net_wm_state_hidden: atom(conn, b"_NET_WM_STATE_HIDDEN")?,
            net_wm_state_max_horz: atom(conn, b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
            net_wm_state_max_vert: atom(conn, b"_NET_WM_STATE_MAXIMIZED_VERT")?,
            net_wm_state_fullscreen: atom(conn, b"_NET_WM_STATE_FULLSCREEN")?,
            net_active_window: atom(conn, b"_NET_ACTIVE_WINDOW")?,
            net_close_window: atom(conn, b"_NET_CLOSE_WINDOW")?,
            net_moveresize_window: atom(conn, b"_NET_MOVERESIZE_WINDOW")?,
            wm_change_state: atom(conn, b"WM_CHANGE_STATE")?,
        })
    }
}

struct X11WindowBackend {
    conn: Mutex<RustConnection>,
    root: Window,
    atoms: X11Atoms,
    supported: HashSet<Atom>,
    has_window_manager: bool,
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
        let (conn, screen_num) = RustConnection::connect(None).map_err(platform)?;
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
            root,
            atoms,
            supported,
            has_window_manager,
        })
    }

    fn facts(&self, element: &ElementData) -> Result<WindowFacts> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.resolve(&conn, element)?;
        let state: HashSet<Atom> = property_u32(
            &conn,
            window,
            self.atoms.net_wm_state,
            AtomEnum::ATOM.into(),
        )?
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
        if self.supports(self.atoms.net_close_window) {
            actions.push("close");
        }
        if self.supports(self.atoms.net_moveresize_window) {
            actions.push("move_to");
            actions.push("resize_to");
        }

        Ok(WindowFacts {
            actions,
            bounds: self.window_bounds(&conn, window).ok(),
            minimized: Some(state.contains(&self.atoms.net_wm_state_hidden)),
            maximized: Some(
                state.contains(&self.atoms.net_wm_state_max_horz)
                    && state.contains(&self.atoms.net_wm_state_max_vert),
            ),
            fullscreen: Some(state.contains(&self.atoms.net_wm_state_fullscreen)),
            backend: Some("x11-ewmh"),
            protocol_version: None,
        })
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

    fn resolve(&self, conn: &RustConnection, element: &ElementData) -> Result<Window> {
        let pid = element.pid.ok_or_else(|| Error::Unsupported {
            feature: "native X11 window identity: AT-SPI target has no process id".to_string(),
        })?;
        let clients = property_u32(
            conn,
            self.root,
            self.atoms.net_client_list,
            AtomEnum::WINDOW.into(),
        )?;
        if clients.is_empty() {
            return Err(Error::Unsupported {
                feature:
                    "native X11 window identity: the window manager exposes no _NET_CLIENT_LIST"
                        .to_string(),
            });
        }
        let mut candidates = Vec::new();
        for window in clients {
            if let Some(candidate) = self.candidate(conn, window)? {
                if candidate.pid == pid {
                    candidates.push(candidate);
                }
            }
        }
        choose_candidate(pid, element.name.as_deref(), element.bounds, &candidates)
    }

    fn candidate(&self, conn: &RustConnection, window: Window) -> Result<Option<X11Candidate>> {
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
        let title = property_bytes(conn, window, self.atoms.net_wm_name, self.atoms.utf8_string)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|title| !title.is_empty());
        let bounds = self.window_bounds(conn, window).ok();
        Ok(Some(X11Candidate {
            window,
            pid,
            title,
            bounds,
        }))
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
        if self.supports_state(self.atoms.net_wm_state_fullscreen) {
            self.change_state(element, STATE_REMOVE, self.atoms.net_wm_state_fullscreen, 0)?;
        }
        if self.supports_state(self.atoms.net_wm_state_max_horz)
            && self.supports_state(self.atoms.net_wm_state_max_vert)
        {
            self.change_state(
                element,
                STATE_REMOVE,
                self.atoms.net_wm_state_max_horz,
                self.atoms.net_wm_state_max_vert,
            )?;
        }
        self.send_for(
            element,
            self.atoms.wm_change_state,
            [ICONIC_STATE, 0, 0, 0, 0],
        )
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
        self.change_state(element, STATE_REMOVE, self.atoms.net_wm_state_fullscreen, 0)?;
        self.change_state(
            element,
            STATE_ADD,
            self.atoms.net_wm_state_max_horz,
            self.atoms.net_wm_state_max_vert,
        )
    }

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        if !self.supports_state(self.atoms.net_wm_state_fullscreen) {
            return Err(Error::Unsupported {
                feature: "enter_fullscreen: the X11 window manager does not advertise fullscreen"
                    .to_string(),
            });
        }
        self.change_state(
            element,
            STATE_REMOVE,
            self.atoms.net_wm_state_max_horz,
            self.atoms.net_wm_state_max_vert,
        )?;
        self.change_state(element, STATE_ADD, self.atoms.net_wm_state_fullscreen, 0)
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.resolve(&conn, element)?;
        // De-iconification is the ICCCM MapWindow request. EWMH state removal
        // is separate and explicit: fullscreen and the two maximize atoms are
        // distinct states, and restore clears all of them.
        conn.map_window(window)
            .map_err(platform)?
            .check()
            .map_err(platform)?;
        if self.supports(self.atoms.net_wm_state) {
            send_client_message(
                &conn,
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
                &conn,
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
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        self.require(self.atoms.net_close_window, "close")?;
        self.send_for(
            element,
            self.atoms.net_close_window,
            [0, SOURCE_APPLICATION, 0, 0, 0],
        )
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.resolve(&conn, element)?;
        // EWMH's moveresize width and height describe the client window,
        // while xa11y's bounds contract reports the decorated outer frame.
        // Remove the current frame extents so a requested outer size reads
        // back as that same size after the window manager applies it.
        let extents = property_u32(
            &conn,
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
            &conn,
            self.root,
            window,
            self.atoms.net_moveresize_window,
            [flags, 0, 0, client_width, client_height],
        )?;
        conn.flush().map_err(platform)
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.resolve(&conn, element)?;
        send_client_message(&conn, self.root, window, type_, data)?;
        conn.flush().map_err(platform)
    }
}

fn choose_candidate(
    pid: u32,
    title: Option<&str>,
    bounds: Option<Rect>,
    candidates: &[X11Candidate],
) -> Result<Window> {
    if candidates.len() == 1 {
        return Ok(candidates[0].window);
    }
    if candidates.is_empty() {
        return Err(Error::Unsupported {
            feature: format!("native X11 window identity: no EWMH client has process id {pid}"),
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
    use super::{choose_candidate, choose_sway_candidate, collect_sway_candidates, X11Candidate};
    use xa11y_core::Rect;

    fn candidate(window: u32, title: &str, bounds: Rect) -> X11Candidate {
        X11Candidate {
            window,
            pid: 7,
            title: Some(title.to_string()),
            bounds: Some(bounds),
        }
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
}
