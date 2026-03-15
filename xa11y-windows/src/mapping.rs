use xa11y_core::{Action, Role, StateSet, Toggled};

// UIA Control Type IDs
const UIA_BUTTON: i32 = 50000;
const UIA_CALENDAR: i32 = 50001;
const UIA_CHECK_BOX: i32 = 50002;
const UIA_COMBO_BOX: i32 = 50003;
const UIA_EDIT: i32 = 50004;
const UIA_HYPERLINK: i32 = 50005;
const UIA_IMAGE: i32 = 50006;
const UIA_LIST_ITEM: i32 = 50007;
const UIA_LIST: i32 = 50008;
const UIA_MENU: i32 = 50009;
const UIA_MENU_BAR: i32 = 50010;
const UIA_MENU_ITEM: i32 = 50011;
const UIA_PROGRESS_BAR: i32 = 50012;
const UIA_RADIO_BUTTON: i32 = 50013;
const UIA_SCROLL_BAR: i32 = 50014;
const UIA_SLIDER: i32 = 50015;
const UIA_SPINNER: i32 = 50016;
const UIA_STATUS_BAR: i32 = 50017;
const UIA_TAB: i32 = 50018;
const UIA_TAB_ITEM: i32 = 50019;
const UIA_TEXT: i32 = 50020;
const UIA_TOOL_BAR: i32 = 50021;
const UIA_TOOL_TIP: i32 = 50022;
const UIA_TREE: i32 = 50023;
const UIA_TREE_ITEM: i32 = 50024;
const UIA_CUSTOM: i32 = 50025;
const UIA_GROUP: i32 = 50026;
const UIA_THUMB: i32 = 50027;
const UIA_DATA_GRID: i32 = 50028;
const UIA_DATA_ITEM: i32 = 50029;
const UIA_DOCUMENT: i32 = 50030;
const UIA_SPLIT_BUTTON: i32 = 50031;
const UIA_WINDOW: i32 = 50032;
const UIA_PANE: i32 = 50033;
const UIA_HEADER: i32 = 50034;
const UIA_HEADER_ITEM: i32 = 50035;
const UIA_TABLE: i32 = 50036;
const UIA_TITLE_BAR: i32 = 50037;
const UIA_SEPARATOR: i32 = 50038;
const UIA_APP_BAR: i32 = 50040;

/// Map UIA control type ID to xa11y Role.
pub fn map_role(control_type: i32) -> Role {
    match control_type {
        UIA_BUTTON | UIA_SPLIT_BUTTON => Role::Button,
        UIA_CHECK_BOX => Role::CheckBox,
        UIA_RADIO_BUTTON => Role::RadioButton,
        UIA_EDIT => Role::TextField,
        UIA_TEXT => Role::StaticText,
        UIA_COMBO_BOX => Role::ComboBox,
        UIA_LIST => Role::List,
        UIA_LIST_ITEM | UIA_DATA_ITEM => Role::ListItem,
        UIA_MENU => Role::Menu,
        UIA_MENU_BAR | UIA_APP_BAR => Role::MenuBar,
        UIA_MENU_ITEM => Role::MenuItem,
        UIA_TAB => Role::TabGroup,
        UIA_TAB_ITEM => Role::Tab,
        UIA_TABLE | UIA_DATA_GRID => Role::Table,
        UIA_HEADER | UIA_HEADER_ITEM => Role::TableCell,
        UIA_TOOL_BAR => Role::Toolbar,
        UIA_SCROLL_BAR | UIA_THUMB => Role::ScrollBar,
        UIA_SLIDER | UIA_SPINNER => Role::Slider,
        UIA_IMAGE => Role::Image,
        UIA_HYPERLINK => Role::Link,
        UIA_GROUP | UIA_PANE | UIA_CUSTOM | UIA_CALENDAR | UIA_STATUS_BAR => Role::Group,
        UIA_PROGRESS_BAR => Role::ProgressBar,
        UIA_TREE => Role::List,
        UIA_TREE_ITEM => Role::TreeItem,
        UIA_DOCUMENT => Role::WebArea,
        UIA_WINDOW | UIA_TITLE_BAR => Role::Window,
        UIA_SEPARATOR => Role::Separator,
        UIA_TOOL_TIP => Role::Alert,
        _ => Role::Unknown,
    }
}

/// Build StateSet from UIA element properties.
pub fn map_states(
    is_enabled: bool,
    is_offscreen: bool,
    has_keyboard_focus: bool,
    toggle_state: Option<i32>,
    is_selected: bool,
    expand_collapse_state: Option<i32>,
) -> StateSet {
    // Toggle states: 0=Off, 1=On, 2=Indeterminate
    let checked = toggle_state.map(|t| match t {
        1 => Toggled::On,
        2 => Toggled::Mixed,
        _ => Toggled::Off,
    });

    // ExpandCollapse states: 0=Collapsed, 1=Expanded, 2=PartiallyExpanded, 3=LeafNode
    let expanded = expand_collapse_state.and_then(|s| match s {
        0 => Some(false),
        1 | 2 => Some(true),
        _ => None, // LeafNode = not expandable
    });

    StateSet {
        enabled: is_enabled,
        visible: !is_offscreen,
        focused: has_keyboard_focus,
        checked,
        selected: is_selected,
        expanded,
        editable: false,
        required: false,
        busy: false,
    }
}

/// Map UIA pattern availability to xa11y actions.
pub fn actions_from_patterns(
    has_invoke: bool,
    has_toggle: bool,
    has_expand_collapse: bool,
    has_selection_item: bool,
    has_value: bool,
    has_range_value: bool,
    has_scroll_item: bool,
) -> Vec<Action> {
    let mut actions = Vec::new();

    if has_invoke {
        actions.push(Action::Press);
    }
    if has_toggle {
        actions.push(Action::Toggle);
    }
    if has_expand_collapse {
        actions.push(Action::Expand);
        actions.push(Action::Collapse);
    }
    if has_selection_item {
        actions.push(Action::Select);
    }
    if has_value || has_range_value {
        actions.push(Action::SetValue);
    }
    if has_range_value {
        actions.push(Action::Increment);
        actions.push(Action::Decrement);
    }
    if has_scroll_item {
        actions.push(Action::ScrollIntoView);
    }

    actions.push(Action::Focus);

    actions
}
