//! GTK3 test application for xa11y integration tests.
//!
//! Creates a window with various UI elements that can be inspected
//! and interacted with via the AT-SPI2 accessibility API.

#[cfg(target_os = "linux")]
fn main() {
    use gtk::glib;
    use gtk::prelude::*;

    gtk::init().expect("Failed to initialize GTK");

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("xa11y Test App");
    window.set_default_size(400, 700);
    // Set accessible name via ATK
    if let Some(accessible) = window.accessible() {
        use gtk::prelude::AtkObjectExt;
        accessible.set_name("xa11y Test App");
    }

    // --- Menu bar ---
    let menu_bar = gtk::MenuBar::new();
    let file_menu_item = gtk::MenuItem::with_label("File");
    let file_menu = gtk::Menu::new();
    let open_item = gtk::MenuItem::with_label("Open");
    let save_item = gtk::MenuItem::with_label("Save");
    let quit_item = gtk::MenuItem::with_label("Quit");
    file_menu.append(&open_item);
    file_menu.append(&save_item);
    file_menu.append(&gtk::SeparatorMenuItem::new());
    file_menu.append(&quit_item);
    file_menu_item.set_submenu(Some(&file_menu));
    menu_bar.append(&file_menu_item);

    let edit_menu_item = gtk::MenuItem::with_label("Edit");
    let edit_menu = gtk::Menu::new();
    let copy_item = gtk::MenuItem::with_label("Copy");
    let paste_item = gtk::MenuItem::with_label("Paste");
    edit_menu.append(&copy_item);
    edit_menu.append(&paste_item);
    edit_menu_item.set_submenu(Some(&edit_menu));
    menu_bar.append(&edit_menu_item);

    quit_item.connect_activate(|_| {
        gtk::main_quit();
    });

    let main_vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_vbox.pack_start(&menu_bar, false, false, 0);

    // --- Toolbar ---
    let toolbar = gtk::Toolbar::new();
    let tool_new = gtk::ToolButton::new(gtk::Image::NONE, Some("New"));
    let tool_open = gtk::ToolButton::new(gtk::Image::NONE, Some("OpenTool"));
    let tool_sep = gtk::SeparatorToolItem::new();
    toolbar.insert(&tool_new, -1);
    toolbar.insert(&tool_open, -1);
    toolbar.insert(&tool_sep, -1);
    main_vbox.pack_start(&toolbar, false, false, 0);

    // --- Notebook / Tabs ---
    let notebook = gtk::Notebook::new();

    // == Tab 1: Main form ==
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);

    // --- Static text / Label ---
    let label = gtk::Label::new(Some("Welcome to xa11y"));
    label.set_widget_name("welcome_label");
    vbox.pack_start(&label, false, false, 0);

    // --- Text entry (single-line) ---
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Enter your name"));
    entry.set_text("John Doe");
    entry.set_widget_name("name_entry");
    let entry_label = gtk::Label::new(Some("Name:"));
    entry_label.set_mnemonic_widget(Some(&entry));
    let entry_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    entry_box.pack_start(&entry_label, false, false, 0);
    entry_box.pack_start(&entry, true, true, 0);
    vbox.pack_start(&entry_box, false, false, 0);

    // --- Buttons ---
    let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let submit_button = gtk::Button::with_label("Submit");
    submit_button.set_widget_name("submit_button");
    button_box.pack_start(&submit_button, false, false, 0);

    let cancel_button = gtk::Button::with_label("Cancel");
    cancel_button.set_widget_name("cancel_button");
    cancel_button.set_sensitive(false); // disabled
    button_box.pack_start(&cancel_button, false, false, 0);

    vbox.pack_start(&button_box, false, false, 0);

    // --- Checkbox ---
    let checkbox = gtk::CheckButton::with_label("I agree to terms");
    checkbox.set_widget_name("agree_checkbox");
    vbox.pack_start(&checkbox, false, false, 0);

    // --- Radio buttons ---
    let radio_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let radio_label = gtk::Label::new(Some("Choose option:"));
    radio_box.pack_start(&radio_label, false, false, 0);
    let radio1 = gtk::RadioButton::with_label("Option A");
    radio1.set_widget_name("option_a");
    radio_box.pack_start(&radio1, false, false, 0);
    let radio2 = gtk::RadioButton::with_label_from_widget(&radio1, "Option B");
    radio2.set_widget_name("option_b");
    radio_box.pack_start(&radio2, false, false, 0);
    vbox.pack_start(&radio_box, false, false, 0);

    // --- Slider ---
    let slider_label = gtk::Label::new(Some("Volume:"));
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    slider.set_value(50.0);
    slider.set_widget_name("volume_slider");
    let slider_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    slider_box.pack_start(&slider_label, false, false, 0);
    slider_box.pack_start(&slider, true, true, 0);
    vbox.pack_start(&slider_box, false, false, 0);

    // --- SpinButton ---
    let spin_label = gtk::Label::new(Some("Quantity:"));
    let spin_adjustment = gtk::Adjustment::new(5.0, 0.0, 100.0, 1.0, 10.0, 0.0);
    let spin_button = gtk::SpinButton::new(Some(&spin_adjustment), 1.0, 0);
    spin_button.set_widget_name("quantity_spin");
    let spin_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    spin_box.pack_start(&spin_label, false, false, 0);
    spin_box.pack_start(&spin_button, false, false, 0);
    vbox.pack_start(&spin_box, false, false, 0);

    // --- ComboBox ---
    let combo_label = gtk::Label::new(Some("Color:"));
    let combo = gtk::ComboBoxText::new();
    combo.append_text("Red");
    combo.append_text("Green");
    combo.append_text("Blue");
    combo.set_active(Some(0));
    combo.set_widget_name("color_combo");
    let combo_box_container = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    combo_box_container.pack_start(&combo_label, false, false, 0);
    combo_box_container.pack_start(&combo, false, false, 0);
    vbox.pack_start(&combo_box_container, false, false, 0);

    // --- Progress bar ---
    let progress = gtk::ProgressBar::new();
    progress.set_fraction(0.75);
    progress.set_text(Some("75%"));
    progress.set_show_text(true);
    progress.set_widget_name("progress_bar");
    vbox.pack_start(&progress, false, false, 0);

    // --- Expander ---
    let expander = gtk::Expander::new(Some("More Details"));
    expander.set_widget_name("details_expander");
    let expander_content = gtk::Label::new(Some("Hidden details content"));
    expander.add(&expander_content);
    expander.set_expanded(false);
    vbox.pack_start(&expander, false, false, 0);

    // --- Separator ---
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    vbox.pack_start(&separator, false, false, 0);

    // --- Image ---
    let image = gtk::Image::from_icon_name(Some("dialog-information"), gtk::IconSize::Dialog);
    image.set_widget_name("info_image");
    if let Some(acc) = image.accessible() {
        use gtk::prelude::AtkObjectExt;
        acc.set_name("Info Icon");
        acc.set_description("An informational icon");
    }
    vbox.pack_start(&image, false, false, 0);

    // --- Status label (updated by button clicks) ---
    let status_label = gtk::Label::new(Some("Status: Ready"));
    status_label.set_widget_name("status_label");
    vbox.pack_start(&status_label, false, false, 0);

    notebook.append_page(&vbox, Some(&gtk::Label::new(Some("Main"))));

    // == Tab 2: List & Table ==
    let tab2_vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tab2_vbox.set_margin_start(16);
    tab2_vbox.set_margin_end(16);
    tab2_vbox.set_margin_top(16);
    tab2_vbox.set_margin_bottom(16);

    // --- ListBox ---
    let list_label = gtk::Label::new(Some("Fruits:"));
    tab2_vbox.pack_start(&list_label, false, false, 0);
    let list_box = gtk::ListBox::new();
    list_box.set_widget_name("fruit_list");
    let fruits = ["Apple", "Banana", "Cherry"];
    for fruit in &fruits {
        let row = gtk::ListBoxRow::new();
        let lbl = gtk::Label::new(Some(fruit));
        row.add(&lbl);
        list_box.add(&row);
    }
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    let scrolled_list = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled_list.set_min_content_height(100);
    scrolled_list.add(&list_box);
    tab2_vbox.pack_start(&scrolled_list, false, false, 0);

    // --- TreeView (used as table) ---
    let table_label = gtk::Label::new(Some("Users Table:"));
    tab2_vbox.pack_start(&table_label, false, false, 0);
    let list_store = gtk::ListStore::new(&[glib::Type::STRING, glib::Type::STRING, glib::Type::STRING]);
    list_store.set(&list_store.append(), &[(0, &"Alice"), (1, &"alice@test.com"), (2, &"Admin")]);
    list_store.set(&list_store.append(), &[(0, &"Bob"), (1, &"bob@test.com"), (2, &"User")]);
    let tree_view = gtk::TreeView::with_model(&list_store);
    tree_view.set_widget_name("users_table");

    let name_col = gtk::TreeViewColumn::new();
    name_col.set_title("Name");
    let name_cell = gtk::CellRendererText::new();
    gtk::prelude::CellLayoutExt::pack_start(&name_col, &name_cell, true);
    gtk::prelude::CellLayoutExt::add_attribute(&name_col, &name_cell, "text", 0);
    tree_view.append_column(&name_col);

    let email_col = gtk::TreeViewColumn::new();
    email_col.set_title("Email");
    let email_cell = gtk::CellRendererText::new();
    gtk::prelude::CellLayoutExt::pack_start(&email_col, &email_cell, true);
    gtk::prelude::CellLayoutExt::add_attribute(&email_col, &email_cell, "text", 1);
    tree_view.append_column(&email_col);

    let role_col = gtk::TreeViewColumn::new();
    role_col.set_title("Role");
    let role_cell = gtk::CellRendererText::new();
    gtk::prelude::CellLayoutExt::pack_start(&role_col, &role_cell, true);
    gtk::prelude::CellLayoutExt::add_attribute(&role_col, &role_cell, "text", 2);
    tree_view.append_column(&role_col);

    let scrolled_table = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled_table.set_min_content_height(100);
    scrolled_table.add(&tree_view);
    tab2_vbox.pack_start(&scrolled_table, true, true, 0);

    notebook.append_page(&tab2_vbox, Some(&gtk::Label::new(Some("Lists"))));

    main_vbox.pack_start(&notebook, true, true, 0);

    // --- Connect signals ---
    let status_clone = status_label.clone();
    let checkbox_clone = checkbox.clone();
    submit_button.connect_clicked(move |_| {
        if checkbox_clone.is_active() {
            status_clone.set_text("Status: Submitted");
        } else {
            status_clone.set_text("Status: Please agree to terms");
        }
    });

    let checkbox_clone2 = checkbox.clone();
    let cancel_clone = cancel_button.clone();
    checkbox_clone2.connect_toggled(move |cb| {
        cancel_clone.set_sensitive(cb.is_active());
    });

    window.add(&main_vbox);

    window.connect_delete_event(|_, _| {
        gtk::main_quit();
        glib::Propagation::Stop
    });

    // Show all widgets
    window.show_all();

    // If --headless is passed, quit after a short delay (for testing)
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--headless") {
        // Run main loop briefly to let AT-SPI register, then keep running
        // The test harness will kill us when done.
        // No auto-quit — the test process manages our lifetime.
    }

    gtk::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("xa11y-test-app requires Linux with GTK3");
    std::process::exit(1);
}
