use serde::{Deserialize, Serialize};

use crate::element::{reader_writer_pair, ElementData};
use crate::text_api::TextRange;

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
    /// - macOS: Checked (via AXValueChanged on checkbox/radio), Busy
    ///   (via AXElementBusyChanged), and Minimized (via
    ///   AXWindowMiniaturized / AXWindowDeminiaturized). Enabled is NOT
    ///   deliverable via any public app-level macOS notification and will
    ///   never fire on macOS.
    StateChanged { flag: StateFlag, value: bool },

    /// Children were added to or removed from an element, or the tree
    /// structure was otherwise invalidated.
    /// Target: the parent element whose children changed, if known.
    StructureChanged,

    /// A top-level window entered the application's window listing
    /// ([`App::windows`](crate::App::windows)): it was created, or a hidden
    /// window was shown again.
    /// Target: the window element.
    ///
    /// - Linux: `Window:Create`, or derived for toolkits that never emit it
    ///   (GTK 4, Qt) from the application's children and the window's
    ///   `VISIBLE` / `ICONIFIED` states. Fires once per transition.
    WindowOpened,

    /// A top-level window left the application's window listing: it was
    /// closed or destroyed, or (Linux) hidden while kept alive.
    /// Target: the window's last snapshot; some attributes may be absent.
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
    /// The variant itself carries no payload, because macOS AXValueChanged
    /// supplies no delta. Where the platform does report one, it is on
    /// [`Event::text_change`]:
    /// - Linux: `Object:TextChanged:insert` / `:delete` carry a
    ///   [`TextChange`]. The `TextChanged` raised alongside `ValueChanged` on
    ///   a text role carries none.
    /// - macOS, Windows: never set yet.
    TextChanged,

    /// The caret moved, or the text selection inside a text element changed.
    /// Target: the text element.
    ///
    /// One kind covers both because the platforms do not separate them: UIA
    /// `TextSelectionChanged` and AX `AXSelectedTextChanged` both fire on a
    /// bare caret move. No offsets are carried; read
    /// [`Text::caret`](crate::Text::caret) and
    /// [`Text::selections`](crate::Text::selections) on the target. (An offset
    /// in the event would need a conversion to UTF-16 against text that may
    /// already have changed by the time it ran.)
    ///
    /// Distinct from [`EventKind::SelectionChanged`], which is about selected
    /// *items* in a list, table or other container.
    ///
    /// - Linux: `Object:TextCaretMoved` and `Object:TextSelectionChanged`.
    /// - macOS, Windows: not emitted yet.
    TextSelectionChanged,

    /// Something asked to be announced: an explicit request with text, a
    /// live region that changed, or an alert that appeared. Target: the
    /// element that made the announcement, if available.
    ///
    /// [`Event::announcement`] says which ([`AnnouncementOrigin`]) and carries
    /// the text where the platform sends it. A live-region or alert change
    /// carries no text: read the target.
    ///
    /// - macOS: `AXAnnouncementRequested` (a request) and
    ///   `AXLiveRegionChanged`.
    /// - Linux: `Object:Announcement` (a request). AT-SPI has no live-region
    ///   signal, so an `aria-live` update arrives as an announcement only
    ///   when the toolkit sends one itself.
    /// - Windows: UIA `Notification` (a request), `LiveRegionChanged` and
    ///   `SystemAlert`.
    Announcement,
}

impl EventKind {
    /// The `snake_case` name of this kind, without its payload:
    /// `"focus_changed"`, `"state_changed"`, `"announcement"`, ... The
    /// capability table names each kind `event:<name>`, and
    /// [`SubscribeOptions::kinds`](crate::SubscribeOptions::kinds) takes
    /// these names.
    pub const fn to_snake_case(&self) -> &'static str {
        match self {
            Self::FocusChanged => "focus_changed",
            Self::ValueChanged => "value_changed",
            Self::NameChanged => "name_changed",
            Self::StateChanged { .. } => "state_changed",
            Self::StructureChanged => "structure_changed",
            Self::WindowOpened => "window_opened",
            Self::WindowClosed => "window_closed",
            Self::WindowActivated => "window_activated",
            Self::WindowDeactivated => "window_deactivated",
            Self::SelectionChanged => "selection_changed",
            Self::MenuOpened => "menu_opened",
            Self::MenuClosed => "menu_closed",
            Self::TextChanged => "text_changed",
            Self::TextSelectionChanged => "text_selection_changed",
            Self::Announcement => "announcement",
        }
    }

    /// One value of every kind, in declaration order. `StateChanged`'s
    /// payload is arbitrary: only the kind matters to the names derived from
    /// it.
    ///
    /// `to_snake_case` is an exhaustive `match`, so a new variant cannot be
    /// left unnamed; `every_event_kind_has_a_row` in `capabilities.rs` counts
    /// this list against the capability table's event rows, so it cannot be
    /// left out here either.
    pub(crate) const EVERY: [EventKind; 15] = [
        Self::FocusChanged,
        Self::ValueChanged,
        Self::NameChanged,
        Self::StateChanged {
            flag: StateFlag::Enabled,
            value: true,
        },
        Self::StructureChanged,
        Self::WindowOpened,
        Self::WindowClosed,
        Self::WindowActivated,
        Self::WindowDeactivated,
        Self::SelectionChanged,
        Self::MenuOpened,
        Self::MenuClosed,
        Self::TextChanged,
        Self::TextSelectionChanged,
        Self::Announcement,
    ];
}

/// Why an [`EventKind::Announcement`] was raised.
///
/// `#[non_exhaustive]`: the set follows what platforms report, not a closed
/// domain, and a new source of announcements (a toast, a status-bar
/// message) must not break consumers. Backends map platform events to an
/// origin, never the reverse, and the bindings' string spelling is guarded
/// by `[[types.variant_coverage]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnnouncementOrigin {
    /// An app asked for text to be spoken (UIA Notification,
    /// `AXAnnouncementRequested`, AT-SPI `Object:Announcement`).
    Request,
    /// A live region changed. No text: read the target.
    LiveRegion,
    /// An alert appeared. No text: read the target.
    Alert,
}

impl AnnouncementOrigin {
    /// `"request"`, `"live_region"` or `"alert"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::LiveRegion => "live_region",
            Self::Alert => "alert",
        }
    }
}

impl std::fmt::Display for AnnouncementOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

reader_writer_pair! {
    /// What an [`EventKind::Announcement`] carries, on [`Event::announcement`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AnnouncementInfo;

    /// Every field a backend must decide on for an announcement. Not public
    /// API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for AnnouncementInfo; see ElementParts."
    )]
    #[derive(Debug, Clone)]
    pub struct AnnouncementParts;

    fields {
        /// Which fact the announcement reports.
        pub origin: AnnouncementOrigin,
        /// The text to announce. Set for a [`AnnouncementOrigin::Request`]
        /// where the platform sends it; `None` for a live-region or alert
        /// change, which carry none.
        pub text: Option<String>,
        /// How urgently, normalised: UIA `ImportantAll` /
        /// `ImportantMostRecent` and AX high priority are `assertive`, the
        /// rest `polite`.
        pub politeness: Option<crate::semantics::Politeness>,
        /// The platform's own priority, as it spells it (`"ImportantAll"`,
        /// `"High"`, `"2"`), for a consumer that needs more than politeness.
        pub platform_priority: Option<String>,
        /// UIA's `activityId`, which groups related notifications. Windows
        /// only.
        pub activity_id: Option<String>,
    }
}

/// Individual state flags used in [`EventKind::StateChanged`].
///
/// `#[non_exhaustive]`: this enum names the states a platform reports a
/// *change* to, which is a subset of [`crate::StateSet`] rather than a mirror
/// of it — `StateSet::active` has no flag here, because no backend delivers an
/// activation as a state-change event. A new `StateSet` field therefore does
/// not automatically gain a flag; adding one is a per-backend decision about
/// whether that state is observable as a change at all. Nothing enforces the
/// correspondence, so do not read this list as complete coverage of
/// `StateSet`. Binding coverage of the variants that *do* exist is enforced by
/// `cargo xtask check-bindings-parity`.
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
    /// The window was minimized (iconified).
    Minimized,
    /// The window was maximized.
    Maximized,
    /// The window entered fullscreen.
    ///
    /// Never raised: no platform reports a fullscreen state change. The
    /// Accessibility API has no fullscreen notification, UIA's
    /// `WindowVisualState` has no fullscreen value, and AT-SPI has no
    /// fullscreen state bit. macOS exposes fullscreen as the `fullscreen`
    /// window state (`AXFullScreen`), driven by `enter_fullscreen` and
    /// `restore`, without a notification.
    Fullscreen,
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
    /// only the three required fields and defaults the rest (`target`,
    /// `timestamp`, `text_change`, `announcement`, `sequence`), so a new one
    /// would land beside them as a silent default rather than a compile
    /// error.
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
        /// For [`EventKind::TextChanged`]: what was inserted or deleted, when
        /// the platform reports it. `None` for every other kind, and for a
        /// `TextChanged` whose platform supplies no delta (macOS, Windows,
        /// and the `TextChanged` Linux derives from a value change).
        pub text_change: Option<TextChange>,
        /// For [`EventKind::Announcement`]: its origin and, where sent, its
        /// text. `None` for every other kind.
        pub announcement: Option<AnnouncementInfo>,
        /// Position in this subscription's stream, from 0. Assigned when
        /// the event is queued, so a gap means events were dropped (see
        /// [`Subscription::dropped`](crate::Subscription::dropped)). A
        /// backend writes 0; the channel overwrites it.
        pub sequence: u64,
    }
}

/// Whether a [`TextChange`] inserted or deleted text.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a text edit event reports an insertion or a \
              deletion; a replacement arrives as one of each."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextChangeKind {
    /// Text was inserted.
    Inserted,
    /// Text was deleted.
    Deleted,
}

impl TextChangeKind {
    /// The `snake_case` name this kind crosses the bindings, the CLI and MCP
    /// as: `"inserted"` or `"deleted"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for TextChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

reader_writer_pair! {
    /// The delta a [`EventKind::TextChanged`] event carries in
    /// [`Event::text_change`], when the platform reports one.
    ///
    /// Each field is what the platform supplied and nothing more: a missing
    /// range or text is `None`, never derived by reading the element's text,
    /// which may already have changed again by the time the event is read.
    ///
    /// `#[non_exhaustive]`: an edit could grow, for example, the platform's own
    /// change-source attribution. Backends build it through
    /// [`TextChangeParts`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TextChange;

    /// Every field a backend must decide on when it builds a [`TextChange`].
    ///
    /// Not public API (`#[doc(hidden)]`).
    #[allow(
        clippy::exhaustive_structs,
        reason = "This type IS the completeness guard for text changes. See \
                  ElementParts; the same reasoning applies."
    )]
    #[derive(Debug, Clone)]
    pub struct TextChangeParts;

    fields {
        /// Inserted or deleted.
        pub kind: TextChangeKind,
        /// Where, in UTF-16 code units: for an insertion, the range the new
        /// text now occupies; for a deletion, the range it occupied before.
        /// `None` when the platform reports offsets in another unit that could
        /// only be converted by reading text that may have changed since (on
        /// Linux, every toolkit but Qt counts code points).
        pub range: Option<TextRange>,
        /// The inserted or deleted text, when the platform supplies it.
        pub text: Option<String>,
    }
}

impl Event {
    /// An event with `timestamp` set to now, `sequence` 0, and no `target`,
    /// `text_change` or `announcement`.
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
            text_change: None,
            announcement: None,
            sequence: 0,
        }
    }

    /// When the event was received, as wall-clock time: `timestamp` placed on
    /// the system clock. Every event of a process is placed against the same
    /// reference, so the order of two events' times is the order they were
    /// received in, and one event's time is the same on every call. A later
    /// change to the system clock does not move it.
    pub fn time(&self) -> std::time::SystemTime {
        static ANCHOR: std::sync::OnceLock<(std::time::Instant, std::time::SystemTime)> =
            std::sync::OnceLock::new();
        let (instant, system) =
            *ANCHOR.get_or_init(|| (std::time::Instant::now(), std::time::SystemTime::now()));
        if self.timestamp >= instant {
            system + (self.timestamp - instant)
        } else {
            system - (instant - self.timestamp)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_change_kind_spells_snake_case_everywhere() {
        // The bindings, the CLI and MCP all print `to_snake_case`; serde must
        // agree so a serialized event reads the same.
        for kind in [TextChangeKind::Inserted, TextChangeKind::Deleted] {
            let json = serde_json::to_value(kind).expect("serializes");
            assert_eq!(json, serde_json::json!(kind.to_snake_case()));
            assert_eq!(kind.to_string(), kind.to_snake_case());
        }
        assert_eq!(TextChangeKind::Inserted.to_snake_case(), "inserted");
        assert_eq!(TextChangeKind::Deleted.to_snake_case(), "deleted");
    }

    #[test]
    fn text_change_is_built_from_its_parts() {
        let change: TextChange = TextChangeParts {
            kind: TextChangeKind::Deleted,
            range: Some(TextRange { start: 2, end: 4 }),
            text: Some("ab".into()),
        }
        .into();
        assert_eq!(change.kind, TextChangeKind::Deleted);
        assert_eq!(change.range, Some(TextRange { start: 2, end: 4 }));
        assert_eq!(change.text.as_deref(), Some("ab"));
        let json = serde_json::to_value(&change).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({"kind": "deleted", "range": {"start": 2, "end": 4}, "text": "ab"})
        );
    }

    #[test]
    fn event_new_carries_no_text_change() {
        let event = Event::new(EventKind::TextChanged, "app", 1);
        assert!(event.text_change.is_none());
        let event = Event::new(EventKind::TextSelectionChanged, "app", 1);
        assert_eq!(event.kind, EventKind::TextSelectionChanged);
        assert!(event.text_change.is_none());
    }
}
