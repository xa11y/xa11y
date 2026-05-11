//! Cross-platform accessibility test application for xa11y integration tests.
//!
//! Uses AccessKit + winit to expose an accessibility tree with ~65 nodes covering
//! all xa11y Role variants. The tree is interactive: actions modify state, which
//! rebuilds the tree so integration tests can verify state changes.
//!
//! Run with: cargo run -p xa11y-test-app -- --headless

use accesskit::{
    Action, ActionData, ActionRequest, Live, Node, NodeId, Rect, Role, Toggled, Tree, TreeId,
    TreeUpdate,
};
use accesskit_winit::{Adapter, Event as AccessKitEvent, WindowEvent as AccessKitWindowEvent};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

const WINDOW_TITLE: &str = "xa11y Test App";

// ── Node IDs ──────────────────────────────────────────────────────────────────
const WINDOW: NodeId = NodeId(0);
const MENU_BAR: NodeId = NodeId(1);
const FILE_MENU_ITEM: NodeId = NodeId(2);
const FILE_MENU: NodeId = NodeId(3);
const OPEN_ITEM: NodeId = NodeId(4);
const SAVE_ITEM: NodeId = NodeId(5);
const FILE_SEPARATOR: NodeId = NodeId(6);
const QUIT_ITEM: NodeId = NodeId(7);
const EDIT_MENU_ITEM: NodeId = NodeId(8);
const EDIT_MENU: NodeId = NodeId(9);
const COPY_ITEM: NodeId = NodeId(10);
const PASTE_ITEM: NodeId = NodeId(11);
const TOOLBAR: NodeId = NodeId(12);
const NEW_BTN: NodeId = NodeId(13);
const OPEN_TOOL_BTN: NodeId = NodeId(14);
const TOOLBAR_SEP: NodeId = NodeId(15);
const TAB_GROUP: NodeId = NodeId(16);
const MAIN_TAB: NodeId = NodeId(17);
const LISTS_TAB: NodeId = NodeId(18);
const EXTRA_TAB: NodeId = NodeId(19);
const MAIN_PANEL: NodeId = NodeId(20);
const WELCOME_TEXT: NodeId = NodeId(21);
const NAME_ROW: NodeId = NodeId(22);
const NAME_LABEL: NodeId = NodeId(23);
const NAME_FIELD: NodeId = NodeId(24);
const BUTTON_ROW: NodeId = NodeId(25);
const SUBMIT_BTN: NodeId = NodeId(26);
const CANCEL_BTN: NodeId = NodeId(27);
const CHECKBOX: NodeId = NodeId(28);
const RADIO_GROUP: NodeId = NodeId(29);
const RADIO_LABEL: NodeId = NodeId(30);
const RADIO_A: NodeId = NodeId(31);
const RADIO_B: NodeId = NodeId(32);
const SLIDER: NodeId = NodeId(33);
const SPINNER: NodeId = NodeId(34);
const COMBO_BOX: NodeId = NodeId(35);
const PROGRESS_BAR: NodeId = NodeId(36);
const EXPANDER_GROUP: NodeId = NodeId(37);
const EXPANDER_BTN: NodeId = NodeId(38);
const EXPANDER_CONTENT: NodeId = NodeId(39);
const MAIN_SEPARATOR: NodeId = NodeId(40);
const IMAGE_NODE: NodeId = NodeId(41);
const STATUS_TEXT: NodeId = NodeId(42);
const LISTS_PANEL: NodeId = NodeId(43);
const FRUITS_LABEL: NodeId = NodeId(44);
const FRUIT_LIST: NodeId = NodeId(45);
const APPLE_ITEM: NodeId = NodeId(46);
const BANANA_ITEM: NodeId = NodeId(47);
const CHERRY_ITEM: NodeId = NodeId(48);
const TABLE_LABEL: NodeId = NodeId(49);
const USERS_TABLE: NodeId = NodeId(50);
const TABLE_ROW_1: NodeId = NodeId(51);
const CELL_ALICE: NodeId = NodeId(52);
const CELL_ALICE_EMAIL: NodeId = NodeId(53);
const CELL_ADMIN: NodeId = NodeId(54);
const TABLE_ROW_2: NodeId = NodeId(55);
const CELL_BOB: NodeId = NodeId(56);
const CELL_BOB_EMAIL: NodeId = NodeId(57);
const CELL_USER_ROLE: NodeId = NodeId(58);
const EXTRA_PANEL: NodeId = NodeId(59);
const VISIT_LINK: NodeId = NodeId(60);
const ROOT_TREE_ITEM: NodeId = NodeId(61);
const CHILD_TREE_ITEM: NodeId = NodeId(62);
const SETTINGS_DIALOG: NodeId = NodeId(63);
const SAMPLE_ALERT: NodeId = NodeId(64);
const SECTION_HEADING: NodeId = NodeId(65);
const SCROLL_BAR_NODE: NodeId = NodeId(66);
const SPLIT_GROUP_NODE: NodeId = NodeId(67);

// Dynamic list items — added/removed via "Add Item" / "Remove Item" buttons
const DYNAMIC_LIST_LABEL: NodeId = NodeId(68);
const DYNAMIC_LIST: NodeId = NodeId(69);
const ADD_ITEM_BTN: NodeId = NodeId(70);
const REMOVE_ITEM_BTN: NodeId = NodeId(71);

// Announcement widgets — live region + button that updates its value
const ANNOUNCE_BTN: NodeId = NodeId(72);
const ANNOUNCE_LIVE_REGION: NodeId = NodeId(73);

// Bidi marks — label whose name and value contain Unicode bidi format
// controls (LRM, RLM, isolates) so xa11y's bidi-strip behavior can be
// covered end-to-end. See xa11y issue #188.
const BIDI_LABEL: NodeId = NodeId(74);

// Dynamic items start at NodeId(100) to leave room for future static nodes
const DYNAMIC_ITEM_BASE: u64 = 100;

// ── Application State ─────────────────────────────────────────────────────────

struct AppState {
    checkbox_checked: bool,
    cancel_enabled: bool,
    text_value: String,
    slider_value: f64,
    spinner_value: f64,
    expander_expanded: bool,
    status_text: String,
    focused_id: NodeId,
    selected_radio: usize,
    selected_list_item: Option<usize>,
    dynamic_item_count: usize,
    /// Value of the live region updated by the Announce button.
    announcement_text: String,
    /// Counter so each press produces a distinct announcement payload
    /// (AccessKit only posts AXAnnouncementRequested when the value changes).
    announcement_counter: u32,
}

impl AppState {
    fn new() -> Self {
        Self {
            checkbox_checked: false,
            cancel_enabled: false,
            text_value: "John Doe".to_string(),
            slider_value: 50.0,
            spinner_value: 5.0,
            expander_expanded: false,
            status_text: "Status: Ready".to_string(),
            focused_id: SUBMIT_BTN,
            selected_radio: 0,
            selected_list_item: None,
            dynamic_item_count: 0,
            announcement_text: String::new(),
            announcement_counter: 0,
        }
    }
}

// ── Tree Builder ──────────────────────────────────────────────────────────────

fn build_tree(state: &AppState) -> TreeUpdate {
    let mut nodes = Vec::with_capacity(72 + state.dynamic_item_count);

    // Window (root)
    let mut window = Node::new(Role::Window);
    window.set_label(WINDOW_TITLE);
    window.set_children(vec![
        MENU_BAR,
        TOOLBAR,
        TAB_GROUP,
        MAIN_PANEL,
        LISTS_PANEL,
        EXTRA_PANEL,
    ]);
    window.set_bounds(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 400.0,
        y1: 700.0,
    });
    nodes.push((WINDOW, window));

    // ── Menu Bar ──
    build_menu_bar(&mut nodes);

    // ── Toolbar ──
    build_toolbar(&mut nodes);

    // ── Tab Group ──
    build_tab_group(&mut nodes);

    // ── Main Panel ──
    build_main_panel(state, &mut nodes);

    // ── Lists Panel ──
    build_lists_panel(state, &mut nodes);

    // ── Extra Panel ──
    build_extra_panel(state, &mut nodes);

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(WINDOW)),
        tree_id: TreeId::ROOT,
        focus: state.focused_id,
    }
}

fn build_menu_bar(nodes: &mut Vec<(NodeId, Node)>) {
    let mut menu_bar = Node::new(Role::MenuBar);
    menu_bar.set_children(vec![FILE_MENU_ITEM, EDIT_MENU_ITEM]);
    nodes.push((MENU_BAR, menu_bar));

    // File menu item
    let mut file_mi = Node::new(Role::MenuItem);
    file_mi.set_label("File");
    file_mi.set_children(vec![FILE_MENU]);
    file_mi.add_action(Action::Click);
    file_mi.add_action(Action::ShowContextMenu);
    nodes.push((FILE_MENU_ITEM, file_mi));

    let mut file_menu = Node::new(Role::Menu);
    file_menu.set_children(vec![OPEN_ITEM, SAVE_ITEM, FILE_SEPARATOR, QUIT_ITEM]);
    nodes.push((FILE_MENU, file_menu));

    for (id, label) in [
        (OPEN_ITEM, "Open"),
        (SAVE_ITEM, "Save"),
        (QUIT_ITEM, "Quit"),
    ] {
        let mut item = Node::new(Role::MenuItem);
        item.set_label(label);
        item.add_action(Action::Click);
        nodes.push((id, item));
    }

    let sep = Node::new(Role::Splitter);
    nodes.push((FILE_SEPARATOR, sep));

    // Edit menu item
    let mut edit_mi = Node::new(Role::MenuItem);
    edit_mi.set_label("Edit");
    edit_mi.set_children(vec![EDIT_MENU]);
    edit_mi.add_action(Action::Click);
    edit_mi.add_action(Action::ShowContextMenu);
    nodes.push((EDIT_MENU_ITEM, edit_mi));

    let mut edit_menu = Node::new(Role::Menu);
    edit_menu.set_children(vec![COPY_ITEM, PASTE_ITEM]);
    nodes.push((EDIT_MENU, edit_menu));

    for (id, label) in [(COPY_ITEM, "Copy"), (PASTE_ITEM, "Paste")] {
        let mut item = Node::new(Role::MenuItem);
        item.set_label(label);
        item.add_action(Action::Click);
        nodes.push((id, item));
    }
}

fn build_toolbar(nodes: &mut Vec<(NodeId, Node)>) {
    let mut toolbar = Node::new(Role::Toolbar);
    toolbar.set_children(vec![NEW_BTN, OPEN_TOOL_BTN, TOOLBAR_SEP]);
    nodes.push((TOOLBAR, toolbar));

    let mut new_btn = Node::new(Role::Button);
    new_btn.set_label("New");
    new_btn.add_action(Action::Click);
    new_btn.add_action(Action::Focus);
    new_btn.set_bounds(Rect {
        x0: 10.0,
        y0: 30.0,
        x1: 60.0,
        y1: 55.0,
    });
    nodes.push((NEW_BTN, new_btn));

    let mut open_tool = Node::new(Role::Button);
    open_tool.set_label("OpenTool");
    open_tool.add_action(Action::Click);
    open_tool.add_action(Action::Focus);
    open_tool.set_bounds(Rect {
        x0: 70.0,
        y0: 30.0,
        x1: 140.0,
        y1: 55.0,
    });
    nodes.push((OPEN_TOOL_BTN, open_tool));

    let toolbar_sep = Node::new(Role::Splitter);
    nodes.push((TOOLBAR_SEP, toolbar_sep));
}

fn build_tab_group(nodes: &mut Vec<(NodeId, Node)>) {
    let mut tab_group = Node::new(Role::TabList);
    tab_group.set_children(vec![MAIN_TAB, LISTS_TAB, EXTRA_TAB]);
    nodes.push((TAB_GROUP, tab_group));

    for (id, label) in [
        (MAIN_TAB, "Main"),
        (LISTS_TAB, "Lists"),
        (EXTRA_TAB, "Extra"),
    ] {
        let mut tab = Node::new(Role::Tab);
        tab.set_label(label);
        tab.add_action(Action::Click);
        nodes.push((id, tab));
    }
}

fn build_main_panel(state: &AppState, nodes: &mut Vec<(NodeId, Node)>) {
    let mut main_panel = Node::new(Role::GenericContainer);
    main_panel.set_label("Main Panel");
    main_panel.set_children(vec![
        WELCOME_TEXT,
        NAME_ROW,
        BUTTON_ROW,
        CHECKBOX,
        RADIO_GROUP,
        SLIDER,
        SPINNER,
        COMBO_BOX,
        PROGRESS_BAR,
        EXPANDER_GROUP,
        MAIN_SEPARATOR,
        IMAGE_NODE,
        STATUS_TEXT,
        BIDI_LABEL,
    ]);
    nodes.push((MAIN_PANEL, main_panel));

    // Welcome text
    let mut welcome = Node::new(Role::Label);
    welcome.set_label("Welcome to xa11y");
    nodes.push((WELCOME_TEXT, welcome));

    // Name row
    let mut name_row = Node::new(Role::GenericContainer);
    name_row.set_label("Name Row");
    name_row.set_children(vec![NAME_LABEL, NAME_FIELD]);
    nodes.push((NAME_ROW, name_row));

    let mut name_label = Node::new(Role::Label);
    name_label.set_label("Name:");
    nodes.push((NAME_LABEL, name_label));

    let mut name_field = Node::new(Role::TextInput);
    name_field.set_label("Name");
    name_field.set_value(state.text_value.clone());
    name_field.add_action(Action::SetValue);
    name_field.add_action(Action::Focus);
    name_field.set_bounds(Rect {
        x0: 80.0,
        y0: 80.0,
        x1: 300.0,
        y1: 105.0,
    });
    nodes.push((NAME_FIELD, name_field));

    // Button row
    let mut button_row = Node::new(Role::GenericContainer);
    button_row.set_label("Button Row");
    button_row.set_children(vec![SUBMIT_BTN, CANCEL_BTN]);
    nodes.push((BUTTON_ROW, button_row));

    let mut submit = Node::new(Role::Button);
    submit.set_label("Submit");
    submit.add_action(Action::Click);
    submit.add_action(Action::Focus);
    submit.set_bounds(Rect {
        x0: 16.0,
        y0: 120.0,
        x1: 96.0,
        y1: 150.0,
    });
    nodes.push((SUBMIT_BTN, submit));

    let mut cancel = Node::new(Role::Button);
    cancel.set_label("Cancel");
    cancel.add_action(Action::Click);
    cancel.add_action(Action::Focus);
    if !state.cancel_enabled {
        cancel.set_disabled();
    }
    cancel.set_bounds(Rect {
        x0: 110.0,
        y0: 120.0,
        x1: 190.0,
        y1: 150.0,
    });
    nodes.push((CANCEL_BTN, cancel));

    // Checkbox
    let mut checkbox = Node::new(Role::CheckBox);
    checkbox.set_label("I agree to terms");
    checkbox.set_toggled(if state.checkbox_checked {
        Toggled::True
    } else {
        Toggled::False
    });
    checkbox.add_action(Action::Click);
    checkbox.add_action(Action::Focus);
    nodes.push((CHECKBOX, checkbox));

    // Radio group
    let mut radio_group = Node::new(Role::RadioGroup);
    radio_group.set_label("Radio Group");
    radio_group.set_children(vec![RADIO_LABEL, RADIO_A, RADIO_B]);
    nodes.push((RADIO_GROUP, radio_group));

    let mut radio_label_node = Node::new(Role::Label);
    radio_label_node.set_label("Choose option:");
    nodes.push((RADIO_LABEL, radio_label_node));

    let mut radio_a = Node::new(Role::RadioButton);
    radio_a.set_label("Option A");
    radio_a.set_toggled(if state.selected_radio == 0 {
        Toggled::True
    } else {
        Toggled::False
    });
    radio_a.add_action(Action::Click);
    radio_a.add_action(Action::Focus);
    nodes.push((RADIO_A, radio_a));

    let mut radio_b = Node::new(Role::RadioButton);
    radio_b.set_label("Option B");
    radio_b.set_toggled(if state.selected_radio == 1 {
        Toggled::True
    } else {
        Toggled::False
    });
    radio_b.add_action(Action::Click);
    radio_b.add_action(Action::Focus);
    nodes.push((RADIO_B, radio_b));

    // Slider
    let mut slider = Node::new(Role::Slider);
    slider.set_label("Volume");
    slider.set_numeric_value(state.slider_value);
    slider.set_min_numeric_value(0.0);
    slider.set_max_numeric_value(100.0);
    slider.add_action(Action::SetValue);
    slider.add_action(Action::Increment);
    slider.add_action(Action::Decrement);
    slider.add_action(Action::Focus);
    slider.set_bounds(Rect {
        x0: 80.0,
        y0: 200.0,
        x1: 300.0,
        y1: 220.0,
    });
    nodes.push((SLIDER, slider));

    // Spinner (SpinButton)
    let mut spinner = Node::new(Role::SpinButton);
    spinner.set_label("Quantity");
    spinner.set_value(format!("{}", state.spinner_value as i64));
    spinner.set_numeric_value(state.spinner_value);
    spinner.set_min_numeric_value(0.0);
    spinner.set_max_numeric_value(100.0);
    spinner.add_action(Action::SetValue);
    spinner.add_action(Action::Increment);
    spinner.add_action(Action::Decrement);
    spinner.add_action(Action::Focus);
    nodes.push((SPINNER, spinner));

    // ComboBox
    let mut combo = Node::new(Role::ComboBox);
    combo.set_label("Color");
    combo.set_value("Red");
    combo.add_action(Action::ShowContextMenu);
    combo.add_action(Action::Focus);
    nodes.push((COMBO_BOX, combo));

    // Progress bar
    let mut progress = Node::new(Role::ProgressIndicator);
    progress.set_label("75%");
    progress.set_numeric_value(0.75);
    progress.set_min_numeric_value(0.0);
    progress.set_max_numeric_value(1.0);
    nodes.push((PROGRESS_BAR, progress));

    // Expander group — expandable via Expand/Collapse actions.
    // Content node is always a child (accesskit requires all nodes reachable from root)
    // but hidden when collapsed.
    let mut expander = Node::new(Role::GenericContainer);
    expander.set_label("Expander");
    expander.set_children(vec![EXPANDER_BTN, EXPANDER_CONTENT]);
    if state.expander_expanded {
        expander.set_expanded(true);
    } else {
        expander.set_expanded(false);
    }
    expander.add_action(Action::Expand);
    expander.add_action(Action::Collapse);
    nodes.push((EXPANDER_GROUP, expander));

    let mut expand_btn = Node::new(Role::Button);
    expand_btn.set_label("More Details");
    expand_btn.add_action(Action::Click);
    expand_btn.add_action(Action::Focus);
    nodes.push((EXPANDER_BTN, expand_btn));

    // Content is always in the tree (for accessibility), but hidden when collapsed
    let mut expand_content = Node::new(Role::Label);
    expand_content.set_label("Hidden details content");
    if !state.expander_expanded {
        expand_content.set_hidden();
    }
    nodes.push((EXPANDER_CONTENT, expand_content));

    // Separator
    let main_sep = Node::new(Role::Splitter);
    nodes.push((MAIN_SEPARATOR, main_sep));

    // Image
    let mut image = Node::new(Role::Image);
    image.set_label("Info Icon");
    image.set_description("An informational icon");
    image.set_bounds(Rect {
        x0: 16.0,
        y0: 400.0,
        x1: 64.0,
        y1: 448.0,
    });
    nodes.push((IMAGE_NODE, image));

    // Status text. For Role::Label, accesskit_atspi_common's NodeWrapper
    // reads the AT-SPI accessible-name from value(), not label()
    // (`label_comes_from_value() == (role == Role::Label)`). Setting both
    // keeps macOS/Windows (which read label) working while letting the
    // AT-SPI bridge observe a label mutation as a PropertyChange(Name).
    let mut status = Node::new(Role::Label);
    status.set_label(&*state.status_text);
    status.set_value(&*state.status_text);
    nodes.push((STATUS_TEXT, status));

    // Bidi label — name and value carry Unicode bidi format controls
    // (LRM, RLM, LRI/RLI/PDI). xa11y strips these from `name`/`value` so
    // equality assertions match the logical text; the unstripped originals
    // remain on `element.raw` (see xa11y issue #188).
    let mut bidi = Node::new(Role::Label);
    bidi.set_label("\u{200E}Bidi Label\u{200E}");
    bidi.set_value("\u{2066}5\u{2069}");
    nodes.push((BIDI_LABEL, bidi));
}

fn build_lists_panel(state: &AppState, nodes: &mut Vec<(NodeId, Node)>) {
    let mut lists_panel = Node::new(Role::GenericContainer);
    lists_panel.set_label("Lists Panel");
    lists_panel.set_children(vec![
        FRUITS_LABEL,
        FRUIT_LIST,
        TABLE_LABEL,
        USERS_TABLE,
        DYNAMIC_LIST_LABEL,
        ADD_ITEM_BTN,
        REMOVE_ITEM_BTN,
        DYNAMIC_LIST,
    ]);
    nodes.push((LISTS_PANEL, lists_panel));

    let mut fruits_label = Node::new(Role::Label);
    fruits_label.set_label("Fruits:");
    nodes.push((FRUITS_LABEL, fruits_label));

    let mut fruit_list = Node::new(Role::List);
    fruit_list.set_label("Fruit List");
    fruit_list.set_children(vec![APPLE_ITEM, BANANA_ITEM, CHERRY_ITEM]);
    nodes.push((FRUIT_LIST, fruit_list));

    let items = [
        (APPLE_ITEM, "Apple"),
        (BANANA_ITEM, "Banana"),
        (CHERRY_ITEM, "Cherry"),
    ];
    for (idx, &(id, label)) in items.iter().enumerate() {
        let mut item = Node::new(Role::ListItem);
        item.set_label(label);
        item.add_action(Action::Click);
        item.add_action(Action::Focus);
        if state.selected_list_item == Some(idx) {
            item.set_selected(true);
        }
        nodes.push((id, item));
    }

    let mut table_label = Node::new(Role::Label);
    table_label.set_label("Users Table:");
    nodes.push((TABLE_LABEL, table_label));

    let mut users_table = Node::new(Role::Table);
    users_table.set_label("Users");
    users_table.set_children(vec![TABLE_ROW_1, TABLE_ROW_2]);
    nodes.push((USERS_TABLE, users_table));

    // Row 1
    let mut row1 = Node::new(Role::Row);
    row1.set_children(vec![CELL_ALICE, CELL_ALICE_EMAIL, CELL_ADMIN]);
    nodes.push((TABLE_ROW_1, row1));

    for (id, label) in [
        (CELL_ALICE, "Alice"),
        (CELL_ALICE_EMAIL, "alice@test.com"),
        (CELL_ADMIN, "Admin"),
    ] {
        let mut cell = Node::new(Role::Cell);
        cell.set_label(label);
        nodes.push((id, cell));
    }

    // Row 2
    let mut row2 = Node::new(Role::Row);
    row2.set_children(vec![CELL_BOB, CELL_BOB_EMAIL, CELL_USER_ROLE]);
    nodes.push((TABLE_ROW_2, row2));

    for (id, label) in [
        (CELL_BOB, "Bob"),
        (CELL_BOB_EMAIL, "bob@test.com"),
        (CELL_USER_ROLE, "User"),
    ] {
        let mut cell = Node::new(Role::Cell);
        cell.set_label(label);
        nodes.push((id, cell));
    }

    // ── Dynamic list (items added/removed at runtime) ──
    let mut dynamic_label = Node::new(Role::Label);
    dynamic_label.set_label("Dynamic Items:");
    nodes.push((DYNAMIC_LIST_LABEL, dynamic_label));

    let mut add_btn = Node::new(Role::Button);
    add_btn.set_label("Add Item");
    add_btn.add_action(Action::Click);
    add_btn.add_action(Action::Focus);
    nodes.push((ADD_ITEM_BTN, add_btn));

    let mut remove_btn = Node::new(Role::Button);
    remove_btn.set_label("Remove Item");
    remove_btn.add_action(Action::Click);
    remove_btn.add_action(Action::Focus);
    nodes.push((REMOVE_ITEM_BTN, remove_btn));

    let dynamic_children: Vec<NodeId> = (0..state.dynamic_item_count)
        .map(|i| NodeId(DYNAMIC_ITEM_BASE + i as u64))
        .collect();

    let mut dynamic_list = Node::new(Role::List);
    dynamic_list.set_label("Dynamic List");
    dynamic_list.set_children(dynamic_children.clone());
    nodes.push((DYNAMIC_LIST, dynamic_list));

    for (i, id) in dynamic_children.into_iter().enumerate() {
        let mut item = Node::new(Role::ListItem);
        let label: Box<str> = format!("Item {}", i + 1).into();
        item.set_label(label);
        nodes.push((id, item));
    }
}

fn build_extra_panel(state: &AppState, nodes: &mut Vec<(NodeId, Node)>) {
    let mut extra_panel = Node::new(Role::GenericContainer);
    extra_panel.set_label("Extra Panel");
    extra_panel.set_children(vec![
        VISIT_LINK,
        ROOT_TREE_ITEM,
        SETTINGS_DIALOG,
        SAMPLE_ALERT,
        SECTION_HEADING,
        SCROLL_BAR_NODE,
        SPLIT_GROUP_NODE,
        ANNOUNCE_BTN,
        ANNOUNCE_LIVE_REGION,
    ]);
    nodes.push((EXTRA_PANEL, extra_panel));

    // Announce button + live region. Pressing the button updates the live
    // region's value, which on macOS translates to
    // AXAnnouncementRequestedNotification (posted on the window by AccessKit).
    let mut announce_btn = Node::new(Role::Button);
    announce_btn.set_label("Announce");
    announce_btn.add_action(Action::Click);
    announce_btn.add_action(Action::Focus);
    nodes.push((ANNOUNCE_BTN, announce_btn));

    // AccessKit's macOS bridge emits AXAnnouncementRequested when a live
    // region's *value* changes. The role must support values — Label does.
    let mut live = Node::new(Role::Label);
    live.set_label("Announcements");
    live.set_value(state.announcement_text.as_str());
    live.set_live(Live::Polite);
    nodes.push((ANNOUNCE_LIVE_REGION, live));

    let mut link = Node::new(Role::Link);
    link.set_label("Visit Example");
    link.add_action(Action::Click);
    nodes.push((VISIT_LINK, link));

    let mut root_tree = Node::new(Role::TreeItem);
    root_tree.set_label("Root Item");
    root_tree.set_children(vec![CHILD_TREE_ITEM]);
    root_tree.set_expanded(true);
    root_tree.add_action(Action::Expand);
    root_tree.add_action(Action::Collapse);
    nodes.push((ROOT_TREE_ITEM, root_tree));

    let mut child_tree = Node::new(Role::TreeItem);
    child_tree.set_label("Child Item");
    nodes.push((CHILD_TREE_ITEM, child_tree));

    let mut dialog = Node::new(Role::Dialog);
    dialog.set_label("Settings Dialog");
    nodes.push((SETTINGS_DIALOG, dialog));

    let mut alert = Node::new(Role::Alert);
    alert.set_label("Sample Alert");
    nodes.push((SAMPLE_ALERT, alert));

    let mut heading = Node::new(Role::Heading);
    heading.set_label("Section Title");
    nodes.push((SECTION_HEADING, heading));

    let mut scrollbar = Node::new(Role::ScrollBar);
    scrollbar.set_numeric_value(0.0);
    scrollbar.set_min_numeric_value(0.0);
    scrollbar.set_max_numeric_value(100.0);
    nodes.push((SCROLL_BAR_NODE, scrollbar));

    // SplitGroup — no direct accesskit role; use Pane which maps to AT-SPI "panel"
    // The test for this role may need adjustment based on AT-SPI mapping
    let mut split_group = Node::new(Role::Pane);
    split_group.set_label("SplitGroup");
    nodes.push((SPLIT_GROUP_NODE, split_group));
}

// ── Action Handler ────────────────────────────────────────────────────────────

fn handle_action(request: &ActionRequest, state: &mut AppState) -> bool {
    let target = request.target_node;
    let action = request.action;

    match (target, action) {
        // Submit button
        (id, Action::Click) if id == SUBMIT_BTN => {
            if state.checkbox_checked {
                state.status_text = "Status: Submitted".to_string();
            } else {
                state.status_text = "Status: Please agree to terms".to_string();
            }
            true
        }

        // Checkbox toggle
        (id, Action::Click) if id == CHECKBOX => {
            state.checkbox_checked = !state.checkbox_checked;
            state.cancel_enabled = state.checkbox_checked;
            true
        }

        // Radio buttons
        (id, Action::Click) if id == RADIO_A => {
            state.selected_radio = 0;
            true
        }
        (id, Action::Click) if id == RADIO_B => {
            state.selected_radio = 1;
            true
        }

        // Focus
        (_, Action::Focus) => {
            state.focused_id = target;
            true
        }

        // SetValue on text field
        (id, Action::SetValue) if id == NAME_FIELD => {
            if let Some(ActionData::Value(ref text)) = request.data {
                state.text_value = text.to_string();
                true
            } else {
                false
            }
        }

        // SetValue on slider
        (id, Action::SetValue) if id == SLIDER => {
            if let Some(ActionData::NumericValue(v)) = request.data {
                state.slider_value = v.clamp(0.0, 100.0);
                true
            } else {
                false
            }
        }

        // Increment/Decrement slider
        (id, Action::Increment) if id == SLIDER => {
            state.slider_value = (state.slider_value + 1.0).min(100.0);
            true
        }
        (id, Action::Decrement) if id == SLIDER => {
            state.slider_value = (state.slider_value - 1.0).max(0.0);
            true
        }

        // SetValue on spinner
        (id, Action::SetValue) if id == SPINNER => {
            if let Some(ActionData::NumericValue(v)) = request.data {
                state.spinner_value = v.clamp(0.0, 100.0);
                true
            } else {
                false
            }
        }

        // Increment/Decrement spinner
        (id, Action::Increment) if id == SPINNER => {
            state.spinner_value = (state.spinner_value + 1.0).min(100.0);
            true
        }
        (id, Action::Decrement) if id == SPINNER => {
            state.spinner_value = (state.spinner_value - 1.0).max(0.0);
            true
        }

        // Expand/Collapse
        (id, Action::Expand) if id == EXPANDER_GROUP => {
            state.expander_expanded = true;
            true
        }
        (id, Action::Collapse) if id == EXPANDER_GROUP => {
            state.expander_expanded = false;
            true
        }

        // List item selection
        (id, Action::Click) if id == APPLE_ITEM => {
            state.selected_list_item = Some(0);
            true
        }
        (id, Action::Click) if id == BANANA_ITEM => {
            state.selected_list_item = Some(1);
            true
        }
        (id, Action::Click) if id == CHERRY_ITEM => {
            state.selected_list_item = Some(2);
            true
        }

        // Add Item button — adds a dynamic list item (triggers StructureChanged)
        (id, Action::Click) if id == ADD_ITEM_BTN => {
            state.dynamic_item_count += 1;
            true
        }

        // Remove Item button — removes a dynamic list item (triggers StructureChanged)
        (id, Action::Click) if id == REMOVE_ITEM_BTN => {
            if state.dynamic_item_count > 0 {
                state.dynamic_item_count -= 1;
            }
            true
        }

        // Announce button — update live region value so AccessKit posts
        // AXAnnouncementRequested via the Cocoa bridge.
        (id, Action::Click) if id == ANNOUNCE_BTN => {
            state.announcement_counter += 1;
            state.announcement_text = format!("Announcement #{}", state.announcement_counter);
            true
        }

        // ScrollIntoView / ShowMenu — accepted no-ops
        (_, Action::ScrollIntoView) | (_, Action::ShowContextMenu) => false,

        _ => false,
    }
}

// ── Winit Application ─────────────────────────────────────────────────────────

struct WindowState {
    window: Window,
    adapter: Adapter,
    state: AppState,
}

struct Application {
    proxy: EventLoopProxy<AccessKitEvent>,
    window: Option<WindowState>,
}

impl ApplicationHandler<AccessKitEvent> for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window_attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_visible(false);

        let window = event_loop
            .create_window(window_attributes)
            .expect("Failed to create window");

        let mut adapter = Adapter::with_event_loop_proxy(event_loop, &window, self.proxy.clone());

        window.set_visible(true);

        // Under headless Xvfb (the integration-test harness) there is no
        // window manager, so winit never dispatches
        // `WindowEvent::Focused(true)` to the adapter. AccessKit's AT-SPI
        // bridge gates focus-change notifications (`focus_moved`, which
        // fans out to `StateChanged(focused, _)` and `PropertyChange` via
        // the tree diff) on the host being OS-focused — without this
        // nudge `Tree::focus()` collapses to `None` on every diff and
        // any focus transition is silently suppressed. Synthesise
        // the event on startup, then re-assert it on every AccessKit
        // user-event boundary in `user_event` (below) so that winit's
        // `Focused(false)` on window creation doesn't clobber the flag.
        adapter.process_event(&window, &WindowEvent::Focused(true));

        self.window = Some(WindowState {
            window,
            adapter,
            state: AppState::new(),
        });
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let ws = match &mut self.window {
            Some(ws) => ws,
            None => return,
        };
        ws.adapter.process_event(&ws.window, &event);
        if let WindowEvent::CloseRequested = event {
            self.window = None;
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, user_event: AccessKitEvent) {
        let ws = match &mut self.window {
            Some(ws) => ws,
            None => return,
        };

        match user_event.window_event {
            AccessKitWindowEvent::InitialTreeRequested => {
                // Re-assert host focus on every boundary the app touches —
                // cheap and idempotent when already true. See the rationale
                // on the synthesised WindowEvent::Focused(true) in
                // `resumed()`: under headless Xvfb winit never emits the
                // event itself, and without `is_host_focused = true` the
                // AT-SPI bridge's tree diff treats focus as perpetually
                // None and silently suppresses focus_moved.
                ws.adapter
                    .process_event(&ws.window, &WindowEvent::Focused(true));
                ws.adapter.update_if_active(|| build_tree(&ws.state));
            }
            AccessKitWindowEvent::ActionRequested(request) => {
                ws.adapter
                    .process_event(&ws.window, &WindowEvent::Focused(true));
                if handle_action(&request, &mut ws.state) {
                    ws.adapter.update_if_active(|| build_tree(&ws.state));
                }
            }
            AccessKitWindowEvent::AccessibilityDeactivated => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            event_loop.exit();
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = Application {
        proxy,
        window: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
