use serde::{Deserialize, Serialize};

use crate::element::ElementData;

/// The kind of accessibility event, normalized across platforms.
///
/// Variants carry payload only when that data is guaranteed to be present
/// on all supporting platforms. For everything else, re-query the `target`
/// element after receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// - Windows: IsEnabled, ToggleState, ExpandCollapseState,
    ///   SelectionItem_IsSelected via PropertyChanged events.
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
    /// - Windows: no first-class UIA event; inferred from focus changes.
    WindowActivated,

    /// A window lost active status.
    /// Target: the window element.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// An accessibility event delivered to subscribers.
#[derive(Debug, Clone)]
pub struct Event {
    /// What happened and any type-specific data.
    pub kind: EventKind,
    /// Snapshot of the element that triggered the event, if available.
    /// None for events where the element is not available or already destroyed.
    pub target: Option<ElementData>,
    /// Name of the application that produced this event.
    pub app_name: String,
    /// Process ID of the application that produced this event.
    pub app_pid: u32,
    /// Monotonic timestamp at event receipt.
    pub timestamp: std::time::Instant,
}

/// Desired element state for wait_for operations.
///
/// Basic variants (`Attached`, `Detached`, `Visible`, `Hidden`, `Enabled`,
/// `Disabled`, `Focused`, `Unfocused`) cover common cases. For arbitrary
/// conditions, use [`Locator::wait_until`] with a closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
