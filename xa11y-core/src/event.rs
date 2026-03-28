use serde::{Deserialize, Serialize};

use crate::node::NodeData;
use crate::provider::AppInfo;

/// Categories of accessibility events, normalized across platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum EventKind {
    /// An element gained keyboard focus.
    FocusChanged,
    /// An element's value changed.
    ValueChanged,
    /// An element's name/label changed.
    NameChanged,
    /// A boolean state flag changed.
    StateChanged,
    /// Children were added or removed from an element.
    StructureChanged,
    /// A new window was created.
    WindowOpened,
    /// A window was closed/destroyed.
    WindowClosed,
    /// A window was activated (brought to front).
    WindowActivated,
    /// A window was deactivated (lost focus).
    WindowDeactivated,
    /// Selection changed in a list, table, or text.
    SelectionChanged,
    /// A menu was opened.
    MenuOpened,
    /// A menu was closed.
    MenuClosed,
    /// An alert or notification was posted.
    Alert,
    /// Text content changed in an editable element.
    ///
    /// On Linux, position comes from the AT-SPI event signal.
    /// On Windows 10+, position comes from `TextEditTextChangedEventId`.
    /// On macOS, position is inferred by diffing (may be `None` for ambiguous changes).
    TextChanged,
}

/// An accessibility event delivered to subscribers.
#[derive(Debug, Clone)]
pub struct Event {
    /// What kind of event occurred.
    pub kind: EventKind,
    /// The application that produced this event.
    pub app: AppInfo,
    /// A snapshot of the element that triggered the event, if available.
    pub target: Option<NodeData>,
    /// For StateChanged events: which state flag changed.
    pub state_flag: Option<StateFlag>,
    /// For StateChanged events: the new value of the flag.
    pub state_value: Option<bool>,
    /// For TextChanged events: details about the text modification.
    pub text_change: Option<TextChangeData>,
    /// Monotonic timestamp at event receipt.
    pub timestamp: std::time::Instant,
}

/// Details about a text change event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChangeData {
    /// What kind of text change occurred.
    pub change_type: TextChangeType,
    /// Character position where the change occurred.
    /// `None` on macOS when the change is ambiguous (e.g., full replacement).
    pub position: Option<u32>,
}

/// The type of text modification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextChangeType {
    /// Text was inserted.
    Insert,
    /// Text was deleted.
    Delete,
    /// Text was replaced (simultaneous insert + delete).
    Replace,
    /// Change type could not be determined (macOS fallback).
    Unknown,
}

/// Individual state flags, for use in StateChanged events and filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
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

/// Filter to narrow which events are delivered.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    /// Which event kinds to subscribe to. Empty = all events.
    pub kinds: Vec<EventKind>,
    /// Only deliver events from elements matching this selector.
    pub selector: Option<String>,
    /// For StateChanged events: only these state flags.
    pub state_flags: Vec<StateFlag>,
}

impl EventFilter {
    /// Subscribe to all events from the target.
    pub fn all() -> Self {
        Self::default()
    }

    /// Subscribe to specific event kinds.
    pub fn kinds(kinds: &[EventKind]) -> Self {
        Self {
            kinds: kinds.to_vec(),
            ..Default::default()
        }
    }

    /// Subscribe to events on elements matching a selector.
    pub fn selector(selector: &str) -> Self {
        Self {
            selector: Some(selector.to_string()),
            ..Default::default()
        }
    }

    /// Combine kind filter with selector filter.
    pub fn new(kinds: &[EventKind], selector: Option<&str>) -> Self {
        Self {
            kinds: kinds.to_vec(),
            selector: selector.map(|s| s.to_string()),
            ..Default::default()
        }
    }
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
    /// Evaluate whether the condition is met for the given node.
    ///
    /// `node` is `None` when no element matched the selector.
    pub fn is_met(self, node: Option<&NodeData>) -> bool {
        match self {
            Self::Attached => node.is_some(),
            Self::Detached => node.is_none(),
            Self::Visible => node.is_some_and(|n| n.states.visible),
            Self::Hidden => node.is_none() || node.is_some_and(|n| !n.states.visible),
            Self::Enabled => node.is_some_and(|n| n.states.enabled),
            Self::Disabled => node.is_some_and(|n| !n.states.enabled),
            Self::Focused => node.is_some_and(|n| n.states.focused),
            Self::Unfocused => node.is_some_and(|n| !n.states.focused),
        }
    }

    /// Whether this state represents an "absent" condition where the node may
    /// not exist in the tree when the condition is met.
    pub fn is_absence_state(self) -> bool {
        matches!(self, Self::Detached | Self::Hidden)
    }
}
