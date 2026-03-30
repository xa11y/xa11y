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
pub mod tree;

// Re-export primary types at the crate root for convenience.
pub use action::{Action, ActionData};
pub use app::App;
pub use element::{
    root_element, Element, ElementData, ElementIndex, RawPlatformData, Rect, StateSet, Toggled,
};
pub use error::{Error, Result};
pub use event::{ElementState, Event, EventType, StateFlag, TextChangeData, TextChangeType};
pub use event_provider::{CancelHandle, EventReceiver, Subscription, SubscriptionIter};
pub use locator::Locator;
pub use provider::{PermissionStatus, Provider};
pub use role::Role;
