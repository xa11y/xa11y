pub mod app;
pub mod element;
pub mod error;
pub mod event;
pub mod event_provider;
pub mod input;
pub mod locator;
pub mod provider;
pub mod role;
pub mod screenshot;
pub mod selector;

/// Shared in-memory mock Provider. Gated behind `test-support` so only the
/// language bindings' test builds (and other explicit opt-ins) compile it.
/// Also available during `cargo test` on this crate so internal unit tests
/// can exercise end-to-end provider flows without duplicating the fixture.
#[cfg(any(feature = "test-support", test))]
pub mod mock;

// Re-export primary types at the crate root for convenience.
pub use app::App;
pub use element::{Element, ElementData, RawPlatformData, Rect, StateSet, Toggled};
pub use error::{Error, Result};
pub use event::{ElementState, Event, EventKind, StateFlag};
pub use event_provider::{CancelHandle, EventReceiver, RecvStatus, Subscription, SubscriptionIter};
pub use input::{
    anchor_point, point_for, Anchor, ClickOptions, ClickTarget, DragOptions, InputProvider,
    InputSim, IntoPoint, Key, Keyboard, Mouse, MouseButton, Point, ScrollDelta,
};
pub use locator::Locator;
pub use provider::Provider;
pub use role::{unknown_role, Role};
pub use screenshot::{Screenshot, ScreenshotProvider, Screenshotter};
pub use selector::Selector;

/// Maximum tree traversal depth for providers. Prevents stack overflow from
/// circular accessibility trees (e.g. Qt/PySide6 apps where the application
/// node lists itself as its own child).
pub const MAX_TREE_DEPTH: u32 = 50;
