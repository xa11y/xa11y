//! D-Bus calls bounded by the caller's deadline (#531).
//!
//! zbus sets one method timeout per connection ([`ATSPI_METHOD_TIMEOUT`]) and
//! has no per-call override, so a call made late in a 1 s wait could still
//! block for 5 s. [`Proxy`] wraps the blocking proxy and races each call
//! against a timer set to the time left on the open
//! [`Deadline`](xa11y_core::Deadline). With no deadline open, or one further
//! away than the connection's own bound, the call is made exactly as before.
//!
//! A call cut short this way fails the same way a connection timeout does,
//! as an I/O error of kind `TimedOut`, so every call site's existing timeout
//! handling still applies. It is also reported to the deadline, which is how
//! core names it as pending whatever error the call site builds from it.

use std::future::Future;

use zbus::message::Message;
use zbus::names::MemberName;

use super::{is_timeout, ATSPI_METHOD_TIMEOUT};

/// An AT-SPI proxy whose calls end by the open deadline.
///
/// Only the calls this backend makes are exposed, so a new kind of call
/// cannot bypass the bound by reaching the inner proxy.
pub(crate) struct Proxy<'a>(zbus::blocking::Proxy<'a>);

impl<'a> From<zbus::blocking::Proxy<'a>> for Proxy<'a> {
    fn from(inner: zbus::blocking::Proxy<'a>) -> Self {
        Self(inner)
    }
}

impl Proxy<'_> {
    /// `zbus::blocking::Proxy::call_method`, bounded by the deadline.
    pub(crate) fn call_method<B>(&self, method: &str, body: &B) -> zbus::Result<Message>
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        let member = MemberName::try_from(method)?;
        self.bounded(method, self.0.inner().call_method(member, body))
    }

    /// `zbus::blocking::Proxy::get_property`, bounded by the deadline.
    pub(crate) fn get_property<T>(&self, property: &str) -> zbus::Result<T>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        self.bounded(property, self.0.inner().get_property(property))
    }

    /// `zbus::blocking::Proxy::set_property`, bounded by the deadline.
    pub(crate) fn set_property<T>(&self, property: &str, value: T) -> zbus::Result<()>
    where
        T: 'static + zbus::zvariant::DynamicType + Into<zbus::zvariant::Value<'static>>,
    {
        let inner = self.0.inner();
        self.bounded(property, async move {
            inner
                .set_property(property, value)
                .await
                .map_err(|e| zbus::Error::FDO(Box::new(e)))
        })
    }

    /// The call as the timeout diagnosis names it: interface member, object
    /// and the application's bus name.
    fn describe(&self, member: &str) -> String {
        let interface = self.0.interface().as_str();
        let interface = interface
            .strip_prefix("org.a11y.atspi.")
            .unwrap_or(interface);
        format!(
            "{interface}.{member} on {} ({})",
            self.0.path().as_str(),
            self.0.destination().as_str()
        )
    }

    fn bounded<T>(
        &self,
        member: &str,
        call: impl Future<Output = zbus::Result<T>>,
    ) -> zbus::Result<T> {
        let timeout = match xa11y_core::call_timeout(ATSPI_METHOD_TIMEOUT, || self.describe(member))
        {
            Ok(t) => t,
            // The deadline passed before the call was made, and recorded
            // it. Fail as a timeout so the call site reports it as one.
            Err(_) => return Err(timed_out()),
        };
        if !timeout.is_deadline() {
            // The connection's own method timeout is the nearer bound.
            return async_io::block_on(call);
        }
        let timer = async {
            async_io::Timer::after(timeout.duration).await;
            Err(timed_out())
        };
        let result = async_io::block_on(futures_lite::future::or(call, timer));
        if let Err(e) = &result {
            if is_timeout(e) {
                // Recording the cut is the point of this call; the error it
                // returns is built again by core with the wait's own context.
                let _ = timeout.expired(self.describe(member));
            }
        }
        result
    }
}

/// The error zbus returns when a method timeout expires, so a cut call and a
/// connection timeout are handled alike.
fn timed_out() -> zbus::Error {
    zbus::Error::InputOutput(
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the caller's deadline passed before the application replied",
        )
        .into(),
    )
}
