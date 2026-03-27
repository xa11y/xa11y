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
pub use action::{Action, ActionData, ScrollDirection};
pub use error::{Error, Result};
pub use event::{
    ElementState, Event, EventFilter, EventKind, StateFlag, TextChangeData, TextChangeType,
};
pub use event_provider::{CancelHandle, EventProvider, EventReceiver, Subscription};
pub use locator::Locator;
pub use node::{Node, RawNode, RawPlatformData, Rect, StateSet, Toggled, TreeData};
pub use provider::{
    AppInfo, AppTarget, PermissionStatus, Provider, ProviderExt, QueryOptions, WindowHandle,
};
pub use role::Role;
pub use tree::Tree;
