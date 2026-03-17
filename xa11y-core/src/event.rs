use serde::{Deserialize, Serialize};

use crate::node::Node;
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
}

/// An accessibility event delivered to subscribers.
#[derive(Debug, Clone)]
pub struct Event {
    /// What kind of event occurred.
    pub kind: EventKind,
    /// The application that produced this event.
    pub app: AppInfo,
    /// A snapshot of the element that triggered the event, if available.
    pub target: Option<Node>,
    /// For StateChanged events: which state flag changed.
    pub state_flag: Option<StateFlag>,
    /// For StateChanged events: the new value of the flag.
    pub state_value: Option<bool>,
    /// Monotonic timestamp at event receipt.
    pub timestamp: std::time::Instant,
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
/// Directly mirrors Playwright's `locator.waitFor({state})`.
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
}
