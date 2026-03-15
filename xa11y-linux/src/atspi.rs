use zbus::zvariant::OwnedObjectPath;

/// Reference to an AT-SPI2 accessible element on D-Bus.
#[derive(Debug, Clone)]
pub struct ElementRef {
    pub bus_name: String,
    pub path: OwnedObjectPath,
}

// ─── D-Bus Proxy Definitions ────────────────────────────────────

#[zbus::proxy(
    interface = "org.a11y.Bus",
    default_service = "org.a11y.Bus",
    default_path = "/org/a11y/bus"
)]
pub trait A11yBus {
    fn get_address(&self) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Accessible")]
pub trait Accessible {
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn description(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn child_count(&self) -> zbus::Result<i32>;

    fn get_child_at_index(&self, index: i32) -> zbus::Result<(String, OwnedObjectPath)>;

    fn get_children(&self) -> zbus::Result<Vec<(String, OwnedObjectPath)>>;

    fn get_role(&self) -> zbus::Result<u32>;

    fn get_state(&self) -> zbus::Result<Vec<u32>>;

    fn get_interfaces(&self) -> zbus::Result<Vec<String>>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Application")]
pub trait Application {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Component")]
pub trait Component {
    fn get_extents(&self, coord_type: u32) -> zbus::Result<(i32, i32, i32, i32)>;
    fn grab_focus(&self) -> zbus::Result<bool>;
    fn scroll_to(&self, scroll_type: u32) -> zbus::Result<bool>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Action")]
pub trait AtspiAction {
    fn do_action(&self, index: i32) -> zbus::Result<bool>;
    fn get_n_actions(&self) -> zbus::Result<i32>;
    fn get_name(&self, index: i32) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Value")]
pub trait Value {
    #[zbus(property)]
    fn current_value(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn set_current_value(&self, value: f64) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.a11y.atspi.Text")]
pub trait Text {
    fn get_text(&self, start_offset: i32, end_offset: i32) -> zbus::Result<String>;
    fn get_character_count(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(interface = "org.a11y.atspi.EditableText")]
pub trait EditableText {
    fn set_text_contents(&self, new_contents: &str) -> zbus::Result<bool>;
}

// ─── Connection ─────────────────────────────────────────────────

pub async fn connect() -> xa11y_core::Result<zbus::Connection> {
    let session = zbus::Connection::session()
        .await
        .map_err(|e| xa11y_core::Error::Platform(format!("D-Bus session bus unavailable: {e}")))?;

    if let Ok(proxy) = A11yBusProxy::new(&session).await {
        if let Ok(addr) = proxy.get_address().await {
            if !addr.is_empty() {
                if let Ok(builder) = zbus::connection::Builder::address(addr.as_str()) {
                    if let Ok(conn) = builder.build().await {
                        return Ok(conn);
                    }
                }
            }
        }
    }

    Ok(session)
}

// ─── Proxy Constructors ─────────────────────────────────────────

/// Build a proxy for the given element. Takes owned bus_name/path to avoid lifetime issues.
macro_rules! make_proxy_fn {
    ($fn_name:ident, $proxy_type:ident) => {
        pub async fn $fn_name<'a>(
            conn: &'a zbus::Connection,
            bus_name: &str,
            path: &OwnedObjectPath,
        ) -> zbus::Result<$proxy_type<'a>> {
            $proxy_type::builder(conn)
                .destination(bus_name.to_owned())?
                .path(path.clone())?
                .build()
                .await
        }
    };
}

make_proxy_fn!(accessible_proxy, AccessibleProxy);
make_proxy_fn!(component_proxy, ComponentProxy);
make_proxy_fn!(action_proxy, AtspiActionProxy);
make_proxy_fn!(value_proxy, ValueProxy);
make_proxy_fn!(text_proxy, TextProxy);
make_proxy_fn!(editable_text_proxy, EditableTextProxy);
make_proxy_fn!(application_proxy, ApplicationProxy);
