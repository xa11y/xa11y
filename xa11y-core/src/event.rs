use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::node::Node;
use crate::provider::AppInfo;

/// Categories of accessibility events, normalized across platforms.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// An element gained keyboard focus.
    FocusChanged,
    /// An element's value changed (text content, slider position, etc.).
    ValueChanged,
    /// An element's name/label changed.
    NameChanged,
    /// A boolean state flag changed (enabled, checked, expanded, selected, busy, etc.).
    StateChanged,
    /// Children were added or removed from an element.
    StructureChanged,
    /// A new window was created.
    WindowOpened,
    /// A window was closed/destroyed.
    WindowClosed,
    /// A window was activated (brought to front / received focus).
    WindowActivated,
    /// A window was deactivated (lost focus to another window).
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

/// Individual state flags, for use in StateChanged events and filters.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// An accessibility event delivered to subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Monotonic timestamp (time since subscription started).
    #[serde(with = "duration_millis")]
    pub timestamp: Duration,
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
            selector: selector.map(String::from),
            ..Default::default()
        }
    }

    /// Check if an event matches this filter.
    pub fn matches(&self, event: &Event) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&event.kind) {
            return false;
        }
        if !self.state_flags.is_empty() {
            if let Some(flag) = event.state_flag {
                if !self.state_flags.contains(&flag) {
                    return false;
                }
            }
        }
        true
    }
}

/// Desired element state for wait_for operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

mod duration_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(d)?;
        Ok(Duration::from_millis(millis))
    }
}
