//! Registry reads against a real `dbus-daemon`. A p2p fake cannot show what
//! the bus answers when an application leaves with a call to it pending.

use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use super::*;

const ROOT: &str = "/org/a11y/atspi/accessible/root";

struct TestBus {
    daemon: Child,
    dir: PathBuf,
    address: String,
}

impl TestBus {
    fn start(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("xa11y-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("bus directory");
        let config = dir.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
                dir.join("socket").display()
            ),
        )
        .expect("bus config");
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("these tests need dbus-daemon on PATH");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().expect("daemon stdout"))
            .read_line(&mut address)
            .expect("daemon address");
        Self {
            daemon,
            dir,
            address: address.trim().to_string(),
        }
    }

    fn connect(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .expect("bus address")
            .method_timeout(ATSPI_METHOD_TIMEOUT)
            .build()
            .expect("bus connection")
    }

    /// A connection plus a second handle on its socket, so a test can close
    /// it the way a process exit does.
    fn connect_closable(&self) -> (Connection, UnixStream) {
        let socket = UnixStream::connect(self.dir.join("socket")).expect("bus socket");
        let handle = socket.try_clone().expect("socket handle");
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(socket)
            .build()
            .expect("bus connection");
        (connection, handle)
    }

    fn provider(&self) -> LinuxProvider {
        LinuxProvider {
            a11y_bus: Mutex::new(self.connect()),
            handle_cache: Arc::new(Mutex::new(HandleTable::new(MAX_CACHED_ELEMENTS))),
            action_indices: Arc::new(Mutex::new(BoundedHandleCache::new(MAX_CACHED_ELEMENTS))),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        }
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        // Teardown of a test fixture: a daemon that already exited is fine.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Registry {
    apps: Vec<String>,
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Registry {
    fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
        self.apps
            .iter()
            .map(|app| {
                (
                    app.clone(),
                    zbus::zvariant::OwnedObjectPath::try_from(ROOT).expect("root path"),
                )
            })
            .collect()
    }
}

struct App;

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl App {
    #[zbus(property)]
    fn name(&self) -> &str {
        "Healthy"
    }
}

/// Closes its own connection when asked for its name, so the call is
/// pending when the application leaves the bus.
struct Exiting {
    socket: UnixStream,
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Exiting {
    #[zbus(property)]
    fn name(&self) -> String {
        self.socket
            .shutdown(std::net::Shutdown::Both)
            .expect("close the application's connection");
        "never delivered".into()
    }
}

/// Stays on the bus and answers its name read with `NoReply`.
struct Refusing;

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Refusing {
    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String> {
        Err(zbus::fdo::Error::NoReply("still on the bus".into()))
    }
}

/// Closes its own connection when asked for its children.
struct ExitingWindows {
    socket: UnixStream,
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl ExitingWindows {
    fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
        self.socket
            .shutdown(std::net::Shutdown::Both)
            .expect("close the application's connection");
        Vec::new()
    }
}

/// An application with one `ACTIVE` top-level window.
struct ActiveApp {
    bus_name: String,
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl ActiveApp {
    fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
        vec![(
            self.bus_name.clone(),
            zbus::zvariant::OwnedObjectPath::try_from("/window").expect("window path"),
        )]
    }

    #[zbus(property)]
    fn name(&self) -> &str {
        "Focused"
    }
}

struct ActiveWindow;

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl ActiveWindow {
    fn get_state(&self) -> Vec<u32> {
        // AtspiStateType::ACTIVE = 1.
        vec![1 << 1, 0]
    }
}

fn unique_name(connection: &Connection) -> String {
    connection
        .unique_name()
        .expect("bus connections have a unique name")
        .to_string()
}

fn serve_registry(bus: &TestBus, apps: Vec<String>) -> Connection {
    let registry = bus.connect();
    registry
        .object_server()
        .at(ROOT, Registry { apps })
        .expect("registry root");
    registry
        .request_name("org.a11y.atspi.Registry")
        .expect("registry name");
    registry
}

#[test]
fn list_apps_skips_an_app_that_exits_during_its_name_read() {
    let bus = TestBus::start("exiting-app");
    let healthy = bus.connect();
    healthy.object_server().at(ROOT, App).expect("healthy app");
    let (exiting, socket) = bus.connect_closable();
    exiting
        .object_server()
        .at(ROOT, Exiting { socket })
        .expect("exiting app");
    let _registry = serve_registry(&bus, vec![unique_name(&exiting), unique_name(&healthy)]);

    let apps = bus
        .provider()
        .list_apps()
        .expect("an app that left the bus is not a failed listing");

    let names: Vec<_> = apps.iter().filter_map(|a| a.name.as_deref()).collect();
    assert_eq!(names, ["Healthy"]);
}

#[test]
fn list_apps_fails_on_a_no_reply_from_an_app_still_on_the_bus() {
    let bus = TestBus::start("refusing-app");
    let refusing = bus.connect();
    refusing
        .object_server()
        .at(ROOT, Refusing)
        .expect("refusing app");
    let _registry = serve_registry(&bus, vec![unique_name(&refusing)]);

    let err = bus
        .provider()
        .list_apps()
        .expect_err("an app that is still on the bus did not leave");

    assert!(
        matches!(&err, Error::Platform { message, .. }
            if message.contains("NoReply") && message.contains(&unique_name(&refusing))),
        "{err:?}"
    );
}

#[test]
fn focused_app_skips_an_app_that_exits_while_its_windows_are_read() {
    let bus = TestBus::start("exiting-windows");
    let (exiting, socket) = bus.connect_closable();
    exiting
        .object_server()
        .at(ROOT, ExitingWindows { socket })
        .expect("exiting app");
    let focused = bus.connect();
    focused
        .object_server()
        .at(
            ROOT,
            ActiveApp {
                bus_name: unique_name(&focused),
            },
        )
        .expect("focused app");
    focused
        .object_server()
        .at("/window", ActiveWindow)
        .expect("active window");
    let _registry = serve_registry(&bus, vec![unique_name(&exiting), unique_name(&focused)]);

    let app = bus
        .provider()
        .focused_app()
        .expect("an app that left the bus is not a failed lookup");

    assert_eq!(app.name.as_deref(), Some("Focused"));
}
