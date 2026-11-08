pub mod app;
pub mod capabilities;
pub mod config;
pub mod element;
pub mod error;
pub mod event;
pub mod event_provider;
#[doc(hidden)]
pub mod handle_cache;
pub mod identity;
pub mod input;
pub mod locator;
pub mod provider;
pub mod role;
pub mod screenshot;
pub mod selector;
pub mod semantics;
pub mod shell;
pub mod table;
pub mod text;
pub mod text_api;

/// Shared in-memory mock Provider. Gated behind `test-support` so only the
/// language bindings' test builds (and other explicit opt-ins) compile it.
/// Also available during `cargo test` on this crate so internal unit tests
/// can exercise end-to-end provider flows without duplicating the fixture.
#[cfg(any(feature = "test-support", test))]
pub mod mock;

// Re-export primary types at the crate root for convenience.
pub use app::App;
pub use capabilities::{Capabilities, Capability, Platform, Support};
pub use config::{default_timeout, set_default_timeout, DEFAULT_TIMEOUT_ENV_VAR};
pub use element::{
    Element, ElementData, HiddenReason, RawPlatformData, Rect, StateSet, Toggled, TreeNode,
};
// `#[doc(hidden)]`: the provider-side construction contract, re-exported so
// the backend crates can reach it. Not public API — see its docs.
#[doc(hidden)]
pub use element::{ElementParts, StateParts};
pub use error::{
    AutoScrollOutcome, Delivery, Diagnosis, Error, PointerTargetDiagnosis, Result, StaleReason,
};
pub use event::{ElementState, Event, EventKind, StateFlag, TextChange, TextChangeKind};
#[doc(hidden)]
pub use event::{EventParts, TextChangeParts};
pub use event_provider::{CancelHandle, EventReceiver, RecvStatus, Subscription, SubscriptionIter};
pub use identity::{NativeObject, NodeIdentity};
pub use input::{
    anchor_point, point_for, Anchor, ClickOptions, ClickTarget, DragOptions, InputProvider,
    InputSim, IntoPoint, Key, Keyboard, Mouse, MouseButton, Point, ScrollDelta,
};
pub use locator::Locator;
pub use provider::Provider;
pub use role::{unknown_role, Role};
#[doc(hidden)]
pub use screenshot::{CaptureMapping, CaptureMappingSegment};
pub use screenshot::{Screenshot, ScreenshotProvider};
pub use selector::{Selector, SelectorGroup};
#[doc(hidden)]
pub use semantics::LiveRegionParts;
pub use semantics::{Landmark, LiveRegionInfo, Politeness, RelationKind};
pub use shell::{ShellSurface, ShellSurfaceKind};
pub use table::{CellInfo, CollectionInfo, Count, HeaderKind, RowInfo, TableInfo};
#[doc(hidden)]
pub use table::{CellParts, CollectionParts, RowParts, TableParts};
pub use text::{is_bidi_control, strip_bidi, strip_bidi_opt};
#[doc(hidden)]
pub use text_api::TextAttributeParts;
pub use text_api::{
    Color, FindOptions, Text, TextAttributes, TextLines, TextRange, TextRun, TextRuns, TextScript,
    TextUnit,
};

/// Maximum tree traversal depth for providers. Prevents stack overflow from
/// circular accessibility trees (e.g. Qt/PySide6 apps where the application
/// node lists itself as its own child).
pub const MAX_TREE_DEPTH: u32 = 50;
