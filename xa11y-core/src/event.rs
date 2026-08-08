use serde::{Deserialize, Serialize};

use crate::element::{reader_writer_pair, ElementData};

/// The kind of accessibility event, normalized across platforms.
///
/// Variants carry payload only when that data is guaranteed to be present
/// on all supporting platforms. For everything else, re-query the `target`
/// element after receipt.
///
/// `#[non_exhaustive]`: the normalized event set grows as backends learn to
/// surface more notifications. Both bindings project these to strings, and
/// `cargo xtask check-bindings-parity` fails when a variant is missing from
/// either mapping — see `[[types.variant_coverage]]` in
/// `bindings/parity_allowlist.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    /// Keyboard focus moved to a new element.
    /// Target: the element that gained focus.
    FocusChanged,

    /// An element's value changed (slider position, text field contents,
    /// checkbox state, spin button, progress, etc.).
    /// Target: the element whose value changed.
    ValueChanged,

    /// An element's name or label changed.
    /// Target: the element whose name changed.
    NameChanged,

    /// A boolean state flag changed on an element.
    /// Target: the element whose state changed.
    ///
    /// `flag` and `value` are always populated — this variant is only emitted
    /// when both are known. Coverage varies by platform:
    /// - Linux: all state bits via Object:StateChanged.
    /// - Windows: IsEnabled, ToggleState, ExpandCollapseState via
    ///   PropertyChanged events. Per-item selection is NOT among them; it
    ///   arrives as `SelectionChanged` from
    ///   UIA_SelectionItem_ElementSelectedEventId instead.
    /// - macOS: Checked (via AXValueChanged on checkbox/radio) and Busy
    ///   (via AXElementBusyChanged). Enabled is NOT deliverable via any
    ///   public app-level macOS notification and will never fire on macOS.
    StateChanged { flag: StateFlag, value: bool },

    /// Children were added to or removed from an element, or the tree
    /// structure was otherwise invalidated.
    /// Target: the parent element whose children changed, if known.
    StructureChanged,

    /// A new window was created.
    /// Target: the window element.
    WindowOpened,

    /// A window was closed or destroyed.
    /// Target: snapshot taken at destruction time; some attributes may be absent.
    WindowClosed,

    /// A window became the active/focused window.
    /// Target: the window element.
    ///
    /// - macOS: AXFocusedWindowChanged.
    /// - Linux: Window:Activate.
    /// - Windows: NOT emitted. UIA has no first-class event, and inferring
    ///   it from focus changes was tried and removed as lossy (it misses
    ///   alt-tab and tool windows, and fires spuriously on in-app focus
    ///   moves). See `design/events.md`.
    WindowActivated,

    /// A window lost active status.
    /// Target: the window element.
    ///
    /// - Windows: NOT emitted, for the same reason as `WindowActivated`.
    WindowDeactivated,

    /// The selection changed in a list, table, or other container.
    /// Target: the container element (not the selected items).
    SelectionChanged,

    /// A menu became visible.
    /// Target: the menu element.
    ///
    /// - macOS: AXMenuOpened.
    /// - Windows: UIA_MenuOpenedEventId.
    /// - Linux: not reliably emitted; this event will not fire on Linux.
    MenuOpened,

    /// A menu was dismissed.
    /// Target: the menu element.
    MenuClosed,

    /// Text content changed in an editable element.
    /// Target: the text element (re-query its value for current contents).
    ///
    /// No payload: macOS AXValueChanged carries no delta, so change_type and
    /// position cannot be populated cross-platform.
    TextChanged,

    /// An accessibility announcement was posted (live region update, alert,
    /// or explicit announcement request).
    /// Target: the element that made the announcement, if available.
    ///
    /// No text payload: Windows UIA_LiveRegionChangedEventId carries no text,
    /// so the announcement text cannot be populated cross-platform. Consumers
    /// should re-query a nearby alert or live region element for the content.
    ///
    /// - macOS: AXAnnouncementRequested.
    /// - Linux: Object:Announcement.
    /// - Windows: UIA_NotificationEventId and UIA_LiveRegionChangedEventId.
    Announcement,
}

/// Individual state flags used in [`EventKind::StateChanged`].
///
/// `#[non_exhaustive]`: this enum tracks [`crate::StateSet`], which is itself
/// `#[non_exhaustive]` — a new state there gains a flag here. Binding
/// coverage is enforced by `cargo xtask check-bindings-parity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StateFlag {
    Enabled,
    Visible,
    Focused,
    Checked,
    Selected,
    Expanded,
    Editable,
    Focusable,
    Modal,
    Required,
    Busy,
}

reader_writer_pair! {
    /// An accessibility event delivered to subscribers.
    ///
    /// `#[non_exhaustive]`: event metadata grows — a platform-native sequence
    /// number and the originating window are both plausible additions — and
    /// that growth must not break consumers who only read one. Backends, which
    /// build them, get the opposite guarantee from [`EventParts`].
    #[derive(Debug, Clone)]
    pub struct Event;

    /// Every field a backend must decide on when it translates a platform
    /// notification into an [`Event`].
    ///
    /// Deliberately exhaustive, for the same reason as
    /// [`crate::ElementParts`]. `Event::new` is not a substitute: it takes
    /// three of the five fields, so a new one would land beside `target` and
    /// `timestamp` as a silent default rather than a compile error.
    ///
    /// Not public API (`#[doc(hidden)]`).
    #[allow(
        clippy::exhaustive_structs,
        reason = "This type IS the completeness guard for events. See \
                  ElementParts; the same reasoning applies."
    )]
    #[derive(Debug, Clone)]
    pub struct EventParts;

    fields {
        /// What happened and any type-specific data.
        pub kind: EventKind,
        /// Snapshot of the element that triggered the event, if available.
        /// None for events where the element is not available or already
        /// destroyed.
        pub target: Option<ElementData>,
        /// Name of the application that produced this event.
        pub app_name: String,
        /// Process ID of the application that produced this event.
        pub app_pid: u32,
        /// Monotonic timestamp at event receipt.
        pub timestamp: std::time::Instant,
    }
}

impl Event {
    /// An event with no `target` and `timestamp` set to now.
    ///
    /// The *partial* construction path, for tests and for callers that only
    /// have the three required fields. A backend translating a real platform
    /// notification should use [`EventParts`] instead, so that a new field
    /// fails its build rather than arriving as a default.
    pub fn new(kind: EventKind, app_name: impl Into<String>, app_pid: u32) -> Self {
        // Struct literal, not a builder: this lives in the defining crate, so
        // the compiler still checks it for completeness when a field is added.
        Self {
            kind,
            target: None,
            app_name: app_name.into(),
            app_pid,
            timestamp: std::time::Instant::now(),
        }
    }
}

/// Desired element state for wait_for operations.
///
/// Basic variants (`Attached`, `Detached`, `Visible`, `Hidden`, `Enabled`,
/// `Disabled`, `Focused`, `Unfocused`) cover common cases. For arbitrary
/// conditions, use [`Locator::wait_until`] with a closure.
///
/// `#[non_exhaustive]`: the set of named wait conditions is open — `Checked`,
/// `Selected`, and `Expanded` are all states [`crate::StateSet`] already
/// carries that could earn a shorthand here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ElementState {
    /// Wait until an element matching the selector exists in the tree.
    Attached,
    /// Wait until no element matches the selector.
    Detached,
    /// Wait until a matching element exists and is visible.
    Visible,
    /// Wait until a matching element is hidden or doesn't exist.
    Hidden,
    /// Wait until a matching element is enabled.
    Enabled,
    /// Wait until a matching element is disabled (exists but not enabled).
    Disabled,
    /// Wait until a matching element has keyboard focus.
    Focused,
    /// Wait until a matching element does not have keyboard focus.
    Unfocused,
}

impl ElementState {
    /// Evaluate whether the condition is met for the given element.
    ///
    /// `element` is `None` when no element matched the selector.
    pub fn is_met(self, element: Option<&ElementData>) -> bool {
        match self {
            Self::Attached => element.is_some(),
            Self::Detached => element.is_none(),
            Self::Visible => element.is_some_and(|e| e.states.visible),
            Self::Hidden => element.is_none() || element.is_some_and(|e| !e.states.visible),
            Self::Enabled => element.is_some_and(|e| e.states.enabled),
            Self::Disabled => element.is_some_and(|e| !e.states.enabled),
            Self::Focused => element.is_some_and(|e| e.states.focused),
            Self::Unfocused => element.is_some_and(|e| !e.states.focused),
        }
    }

    /// Whether this state represents an "absent" condition where the node may
    /// not exist in the tree when the condition is met.
    pub fn is_absence_state(self) -> bool {
        matches!(self, Self::Detached | Self::Hidden)
    }
}
