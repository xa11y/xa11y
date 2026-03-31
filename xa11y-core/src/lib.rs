pub mod action;
pub mod app;
pub mod element;
pub mod error;
pub mod event;
pub mod event_provider;
pub mod locator;
pub mod provider;
pub mod role;
pub mod selector;

// Re-export primary types at the crate root for convenience.
pub use action::{Action, ActionData};
pub use app::App;
pub use element::{Element, ElementData, RawPlatformData, Rect, StateSet, Toggled};
pub use error::{Error, Result};
pub use event::{ElementState, Event, EventType, StateFlag, TextChangeData, TextChangeType};
pub use event_provider::{CancelHandle, EventReceiver, Subscription, SubscriptionIter};
pub use locator::Locator;
pub use provider::Provider;
pub use role::Role;
pub use selector::Selector;

/// Maximum tree traversal depth for providers. Prevents stack overflow from
/// circular accessibility trees (e.g. Qt/PySide6 apps where the application
/// node lists itself as its own child).
pub const MAX_TREE_DEPTH: u32 = 50;
