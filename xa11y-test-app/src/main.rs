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
    window.set_default_size(400, 500);
    // Set accessible name via ATK
    if let Some(accessible) = window.accessible() {
        use gtk::prelude::AtkObjectExt;
        accessible.set_name("xa11y Test App");
    }

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

    // --- Progress bar ---
    let progress = gtk::ProgressBar::new();
    progress.set_fraction(0.75);
    progress.set_text(Some("75%"));
    progress.set_show_text(true);
    progress.set_widget_name("progress_bar");
    vbox.pack_start(&progress, false, false, 0);

    // --- Status label (updated by button clicks) ---
    let status_label = gtk::Label::new(Some("Status: Ready"));
    status_label.set_widget_name("status_label");
    vbox.pack_start(&status_label, false, false, 0);

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

    window.add(&vbox);

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
