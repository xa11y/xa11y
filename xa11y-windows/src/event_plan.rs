//! Which UIA event registrations a subscription needs, from the event kinds
//! it asked for.
//!
//! UIA subscribes per handler and per event or property ID, and every
//! registration costs a cross-process round trip per top-level window. A
//! subscription that asked only for `announcement` therefore registers only
//! the handlers that can raise one. Core filters delivery by kind either way
//! (`Subscription::restrict`), so registering *more* than needed is never
//! wrong, only slower; registering less loses events. Every mapping below is
//! read off the dispatch code in `uia.rs` that turns the UIA callback into an
//! `EventKind`, and lists every kind that callback can emit.
//!
//! Kept free of `windows` types so the mapping is unit-tested on every
//! platform; `uia.rs` maps each variant here to its UIA ID with an exhaustive
//! `match`, so a new variant cannot be left unregistered.

use xa11y_core::{AnnouncementInfo, AnnouncementOrigin, AnnouncementParts, Politeness};

/// An event ID registered through `AddAutomationEventHandler` on each
/// top-level window, with the kinds `AutomationHandler` dispatches it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomationEvent {
    TextSelectionChanged,
    MenuOpened,
    MenuClosed,
    TextChanged,
    ElementSelected,
    ElementAddedToSelection,
    ElementRemovedFromSelection,
    LiveRegionChanged,
    /// The design-doc-listed announcement source for pre-Windows-10 alert
    /// messages and legacy providers that raise no Notification.
    SystemAlert,
}

impl AutomationEvent {
    pub(crate) const ALL: &'static [AutomationEvent] = &[
        Self::TextSelectionChanged,
        Self::MenuOpened,
        Self::MenuClosed,
        Self::TextChanged,
        Self::ElementSelected,
        Self::ElementAddedToSelection,
        Self::ElementRemovedFromSelection,
        Self::LiveRegionChanged,
        Self::SystemAlert,
    ];

    /// Every kind the handler can emit for this event ID.
    pub(crate) fn kinds(self) -> &'static [&'static str] {
        match self {
            Self::TextSelectionChanged => &["text_selection_changed"],
            Self::MenuOpened => &["menu_opened"],
            Self::MenuClosed => &["menu_closed"],
            Self::TextChanged => &["text_changed"],
            Self::ElementSelected
            | Self::ElementAddedToSelection
            | Self::ElementRemovedFromSelection => &["selection_changed"],
            Self::LiveRegionChanged | Self::SystemAlert => &["announcement"],
        }
    }
}

/// A property ID watched through `AddPropertyChangedEventHandlerNativeArray`
/// on each top-level window, with the kinds `PropertyHandler` dispatches it
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchedProperty {
    Name,
    IsEnabled,
    /// Raises both `StateChanged{Checked}` and `ValueChanged`.
    ToggleState,
    Value,
    RangeValue,
    ExpandCollapseState,
    /// Window minimize / maximize / restore. Also gates seeding the
    /// per-window visual-state baseline, which only this property reads.
    WindowVisualState,
}

impl WatchedProperty {
    pub(crate) const ALL: &'static [WatchedProperty] = &[
        Self::Name,
        Self::IsEnabled,
        Self::ToggleState,
        Self::Value,
        Self::RangeValue,
        Self::ExpandCollapseState,
        Self::WindowVisualState,
    ];

    /// Every kind the handler can emit for this property.
    pub(crate) fn kinds(self) -> &'static [&'static str] {
        match self {
            Self::Name => &["name_changed"],
            Self::IsEnabled | Self::ExpandCollapseState | Self::WindowVisualState => {
                &["state_changed"]
            }
            Self::ToggleState => &["state_changed", "value_changed"],
            Self::Value | Self::RangeValue => &["value_changed"],
        }
    }
}

/// The kinds the process-wide focus handler emits.
const FOCUS_KINDS: &[&str] = &["focus_changed"];
/// The kinds the per-window structure-changed handler emits.
const STRUCTURE_KINDS: &[&str] = &["structure_changed"];
/// The kinds the per-window notification handler emits.
const NOTIFICATION_KINDS: &[&str] = &["announcement"];
/// Kinds the Windows backend never emits, so no handler is registered for
/// them. Core rejects them before subscribing (the capability table marks
/// them unsupported); they are listed so they are not mistaken for an
/// unknown name below.
const NEVER_EMITTED: &[&str] = &["window_activated", "window_deactivated"];

/// The registrations one subscription makes.
///
/// There is no flag for the desktop open/close watch: it is registered for
/// every subscription. It is what attaches a window that opens after
/// subscribe, and every kind needs that window attached: the per-window
/// handlers obviously, `window_opened` / `window_closed` because the watch
/// raises them, and `focus_changed` because the focus handler's PID filter
/// recognizes a UWP app's frame-host senders only through the frame
/// identities recorded when a window is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandlerPlan {
    /// `AddFocusChangedEventHandler`, process-wide.
    pub focus: bool,
    /// Event IDs for the per-window `AutomationHandler`, in `ALL` order.
    pub automation: Vec<AutomationEvent>,
    /// Properties for the per-window `PropertyHandler`, in `ALL` order.
    /// Empty means the handler is not registered.
    pub properties: Vec<WatchedProperty>,
    /// The per-window `StructureHandler`.
    pub structure: bool,
    /// The per-window `NotificationHandler`
    /// (`IUIAutomation5::AddNotificationEventHandler`).
    pub notification: bool,
    /// Whether the caller named `announcement` explicitly. A system without
    /// `IUIAutomation5` cannot register the notification handler; that fails
    /// the subscription when announcements were asked for by name, and
    /// leaves the handler out of an all-kinds subscription.
    pub announcement_requested: bool,
}

impl HandlerPlan {
    /// The plan for `kinds`, as `Provider::subscribe_kinds` receives them:
    /// `None` is every kind.
    ///
    /// A name this table does not know registers everything. Core validates
    /// names against `EventKind::to_snake_case()` first, so this only happens when a
    /// kind is added to core without a row here, and over-registering keeps
    /// such a kind delivered, where under-registering would drop it.
    pub(crate) fn for_kinds(kinds: Option<&[&str]>) -> Self {
        let all = Self {
            focus: true,
            automation: AutomationEvent::ALL.to_vec(),
            properties: WatchedProperty::ALL.to_vec(),
            structure: true,
            notification: true,
            announcement_requested: false,
        };
        let Some(kinds) = kinds else {
            return all;
        };
        let known = |k: &&str| {
            FOCUS_KINDS.contains(k)
                || STRUCTURE_KINDS.contains(k)
                || NOTIFICATION_KINDS.contains(k)
                || NEVER_EMITTED.contains(k)
                || ["window_opened", "window_closed"].contains(k)
                || AutomationEvent::ALL.iter().any(|e| e.kinds().contains(k))
                || WatchedProperty::ALL.iter().any(|p| p.kinds().contains(k))
        };
        let announcement_requested = kinds.contains(&"announcement");
        if !kinds.iter().all(known) {
            return Self {
                announcement_requested,
                ..all
            };
        }
        let wants = |produced: &[&str]| produced.iter().any(|k| kinds.contains(k));
        Self {
            focus: wants(FOCUS_KINDS),
            automation: AutomationEvent::ALL
                .iter()
                .copied()
                .filter(|e| wants(e.kinds()))
                .collect(),
            properties: WatchedProperty::ALL
                .iter()
                .copied()
                .filter(|p| wants(p.kinds()))
                .collect(),
            structure: wants(STRUCTURE_KINDS),
            notification: wants(NOTIFICATION_KINDS),
            announcement_requested,
        }
    }
}

/// The UIA `NotificationProcessing` value as the SDK spells it, and the
/// normalised politeness: the two `Important*` values are assertive, the
/// rest polite. An undefined value keeps its number and has no politeness.
pub(crate) fn notification_processing(value: i32) -> (String, Option<Politeness>) {
    let (name, politeness) = match value {
        0 => ("ImportantAll", Politeness::Assertive),
        1 => ("ImportantMostRecent", Politeness::Assertive),
        2 => ("All", Politeness::Polite),
        3 => ("MostRecent", Politeness::Polite),
        4 => ("CurrentThenMostRecent", Politeness::Polite),
        other => return (other.to_string(), None),
    };
    (name.to_string(), Some(politeness))
}

/// The payload of a UIA `Notification` event, from the arguments of
/// `HandleNotificationEvent`. An empty `displayString` or `activityId` is
/// `None`: a BSTR cannot tell an empty string from an absent one, and a
/// notification with nothing to say has no text.
pub(crate) fn notification_announcement(
    processing: i32,
    display_string: String,
    activity_id: String,
) -> AnnouncementInfo {
    let (platform_priority, politeness) = notification_processing(processing);
    AnnouncementParts {
        origin: AnnouncementOrigin::Request,
        text: Some(display_string).filter(|s| !s.is_empty()),
        politeness,
        platform_priority: Some(platform_priority),
        activity_id: Some(activity_id).filter(|s| !s.is_empty()),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(kinds: &[&str]) -> HandlerPlan {
        HandlerPlan::for_kinds(Some(kinds))
    }

    #[test]
    fn no_kinds_registers_everything() {
        let p = HandlerPlan::for_kinds(None);
        assert!(p.focus && p.structure && p.notification);
        assert_eq!(p.automation, AutomationEvent::ALL);
        assert_eq!(p.properties, WatchedProperty::ALL);
        assert!(!p.announcement_requested);
    }

    #[test]
    fn announcement_registers_only_its_sources() {
        let p = plan(&["announcement"]);
        assert!(!p.focus && !p.structure);
        assert!(p.notification && p.announcement_requested);
        assert_eq!(
            p.automation,
            [
                AutomationEvent::LiveRegionChanged,
                AutomationEvent::SystemAlert
            ]
        );
        assert!(p.properties.is_empty());
    }

    #[test]
    fn focus_registers_only_the_focus_handler() {
        let p = plan(&["focus_changed"]);
        assert!(p.focus);
        assert!(!p.structure && !p.notification && !p.announcement_requested);
        assert!(p.automation.is_empty() && p.properties.is_empty());
    }

    #[test]
    fn window_open_close_needs_no_per_window_handler() {
        // The desktop watch raises them and is always registered.
        let p = plan(&["window_opened", "window_closed"]);
        assert!(!p.focus && !p.structure && !p.notification);
        assert!(p.automation.is_empty() && p.properties.is_empty());
    }

    #[test]
    fn value_changed_includes_toggle_state() {
        let p = plan(&["value_changed"]);
        assert_eq!(
            p.properties,
            [
                WatchedProperty::ToggleState,
                WatchedProperty::Value,
                WatchedProperty::RangeValue
            ]
        );
        assert!(p.automation.is_empty());
    }

    #[test]
    fn state_changed_includes_visual_state_and_toggle() {
        let p = plan(&["state_changed"]);
        assert_eq!(
            p.properties,
            [
                WatchedProperty::IsEnabled,
                WatchedProperty::ToggleState,
                WatchedProperty::ExpandCollapseState,
                WatchedProperty::WindowVisualState
            ]
        );
    }

    #[test]
    fn selection_registers_all_three_selection_item_events() {
        let p = plan(&["selection_changed"]);
        assert_eq!(
            p.automation,
            [
                AutomationEvent::ElementSelected,
                AutomationEvent::ElementAddedToSelection,
                AutomationEvent::ElementRemovedFromSelection
            ]
        );
    }

    #[test]
    fn structure_and_menu_and_text_kinds_map_one_to_one() {
        assert!(plan(&["structure_changed"]).structure);
        assert_eq!(
            plan(&["menu_opened", "menu_closed"]).automation,
            [AutomationEvent::MenuOpened, AutomationEvent::MenuClosed]
        );
        assert_eq!(
            plan(&["text_changed"]).automation,
            [AutomationEvent::TextChanged]
        );
        assert_eq!(
            plan(&["text_selection_changed"]).automation,
            [AutomationEvent::TextSelectionChanged]
        );
        assert_eq!(plan(&["name_changed"]).properties, [WatchedProperty::Name]);
    }

    #[test]
    fn an_unknown_kind_registers_everything() {
        let p = plan(&["focus_changed", "some_future_kind"]);
        assert_eq!(
            HandlerPlan {
                announcement_requested: false,
                ..p
            },
            HandlerPlan::for_kinds(None)
        );
    }

    #[test]
    fn every_core_kind_is_known_here() {
        // A kind core adds without a row in this table would register
        // everything; this keeps the table complete instead.
        for kind in xa11y_core::event_provider::event_kind_names() {
            let p = plan(&[kind]);
            assert_ne!(
                p,
                HandlerPlan::for_kinds(None),
                "{kind} has no row in the Windows handler plan"
            );
        }
    }

    #[test]
    fn processing_maps_to_its_sdk_name_and_politeness() {
        use Politeness::*;
        let cases = [
            (0, "ImportantAll", Some(Assertive)),
            (1, "ImportantMostRecent", Some(Assertive)),
            (2, "All", Some(Polite)),
            (3, "MostRecent", Some(Polite)),
            (4, "CurrentThenMostRecent", Some(Polite)),
            (9, "9", None),
        ];
        for (value, name, politeness) in cases {
            assert_eq!(
                notification_processing(value),
                (name.to_string(), politeness)
            );
        }
    }

    #[test]
    fn a_notification_carries_its_text_and_activity() {
        let a = notification_announcement(1, "Saved".into(), "save-1".into());
        assert_eq!(a.origin, AnnouncementOrigin::Request);
        assert_eq!(a.text.as_deref(), Some("Saved"));
        assert_eq!(a.politeness, Some(Politeness::Assertive));
        assert_eq!(a.platform_priority.as_deref(), Some("ImportantMostRecent"));
        assert_eq!(a.activity_id.as_deref(), Some("save-1"));

        let empty = notification_announcement(2, String::new(), String::new());
        assert_eq!(empty.text, None);
        assert_eq!(empty.activity_id, None);
        assert_eq!(empty.politeness, Some(Politeness::Polite));
    }
}
