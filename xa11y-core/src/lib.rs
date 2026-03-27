/// Internal action types used by platform backends.
/// Not part of the public API — use convenience methods on [`Locator`] instead.
#[doc(hidden)]
pub mod action;
pub mod error;
pub mod event;
pub mod event_provider;
pub mod locator;
pub mod node;
pub mod provider;
pub mod role;
pub mod selector;
pub mod tree;

// Re-export primary types at the crate root for convenience.
pub use error::{Error, Result};
pub use event::{
    ElementState, Event, EventFilter, EventKind, StateFlag, TextChangeData, TextChangeType,
};
pub use event_provider::{CancelHandle, EventProvider, EventReceiver, Subscription};
pub use locator::{Locator, ProviderExt};
pub use node::{Node, RawPlatformData, Rect, StateSet, Toggled};
pub use provider::{AppInfo, AppTarget, PermissionStatus, Provider, QueryOptions, WindowHandle};
pub use role::Role;
pub use tree::Tree;
