//! Cross-platform accessibility test application for xa11y integration tests.
//!
//! Built with `eframe` (the egui framework) and its `accesskit` feature, which
//! pushes the live egui widget tree through AccessKit to the host platform's
//! a11y bridge (AT-SPI2 on Linux, AX on macOS, UIA on Windows). The widget
//! schema mirrors the Tauri / Qt / GTK / Cocoa test apps so the shared compat
//! suite (`tests/suites/python/`) can run unchanged against egui.
//!
//! Launched by `tests/harness/launch.py egui …` and exercised by the
//! `Integ (egui) on …` CI matrix.

use eframe::egui;

const APP_TITLE: &str = "xa11y-egui-test-app";

// Items 1..=5 are present at startup; Add/Remove Item mutate this list to
// drive `StructureChanged` notifications through AccessKit. The starting
// count matches the Tauri test app.
const INITIAL_ITEM_COUNT: usize = 5;

#[derive(Default, PartialEq, Eq)]
enum RadioChoice {
    #[default]
    A,
    B,
    C,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Fruit {
    #[default]
    Apple,
    Banana,
    Cherry,
    Date,
    Elderberry,
}

impl Fruit {
    fn label(&self) -> &'static str {
        match self {
            Fruit::Apple => "Apple",
            Fruit::Banana => "Banana",
            Fruit::Cherry => "Cherry",
            Fruit::Date => "Date",
            Fruit::Elderberry => "Elderberry",
        }
    }
}

struct TestApp {
    cancel_enabled: bool,
    agree: bool,
    subscribe: bool,
    radio: RadioChoice,
    fruit: Fruit,
    volume: f64,
    quantity: i64,
    progress: f32,
    search: String,
    notes: String,
    items: Vec<String>,
    next_item_ix: usize,
    selected_item: Option<usize>,
    status: String,
}

impl Default for TestApp {
    fn default() -> Self {
        Self {
            cancel_enabled: false,
            agree: false,
            subscribe: true,
            radio: RadioChoice::A,
            fruit: Fruit::Apple,
            volume: 50.0,
            quantity: 42,
            progress: 0.75,
            search: "hello world".to_string(),
            notes: "Line 1\nLine 2\nLine 3".to_string(),
            items: (1..=INITIAL_ITEM_COUNT)
                .map(|i| format!("Item {i}"))
                .collect(),
            next_item_ix: INITIAL_ITEM_COUNT + 1,
            selected_item: None,
            status: "Status: Ready".to_string(),
        }
    }
}

impl eframe::App for TestApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.buttons_group(ui);
            self.checkboxes_group(ui);
            self.radios_group(ui);
            self.combo_group(ui);
            self.range_group(ui);
            self.input_group(ui);
            self.text_group(ui);
            self.list_group(ui);
            self.dynamic_group(ui);
        });
    }
}

impl TestApp {
    fn buttons_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Buttons");
        ui.horizontal(|ui| {
            if ui
                .button("OK")
                .on_hover_text("Confirm the dialog")
                .clicked()
            {
                self.cancel_enabled = true;
            }
            ui.add_enabled(self.cancel_enabled, egui::Button::new("Cancel"));
        });
        ui.separator();
    }

    fn checkboxes_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Checkboxes");
        ui.checkbox(&mut self.agree, "Agree to terms");
        ui.checkbox(&mut self.subscribe, "Subscribe");
        ui.separator();
    }

    fn radios_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Options");
        ui.radio_value(&mut self.radio, RadioChoice::A, "Option A");
        ui.radio_value(&mut self.radio, RadioChoice::B, "Option B");
        ui.radio_value(&mut self.radio, RadioChoice::C, "Option C");
        ui.separator();
    }

    fn combo_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("ComboBox");
        egui::ComboBox::from_label("Fruit")
            .selected_text(self.fruit.label())
            .show_ui(ui, |ui| {
                for variant in [
                    Fruit::Apple,
                    Fruit::Banana,
                    Fruit::Cherry,
                    Fruit::Date,
                    Fruit::Elderberry,
                ] {
                    let label = variant.label();
                    ui.selectable_value(&mut self.fruit, variant, label);
                }
            });
        ui.separator();
    }

    fn range_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Range Controls");
        ui.add(egui::Slider::new(&mut self.volume, 0.0..=100.0).text("Volume"));
        ui.horizontal(|ui| {
            ui.label("Quantity");
            ui.add(egui::DragValue::new(&mut self.quantity).range(0..=999));
        });
        ui.horizontal(|ui| {
            ui.label("Progress");
            ui.add(egui::ProgressBar::new(self.progress).text("75%"));
        });
        ui.separator();
    }

    fn input_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Input");
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.search);
        });
        ui.separator();
    }

    fn text_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Text");
        ui.label("Heading Text");
        ui.horizontal(|ui| {
            ui.label("Notes");
            ui.text_edit_multiline(&mut self.notes);
        });
        ui.separator();
    }

    fn list_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("List");
        for (idx, item) in self.items.iter().enumerate() {
            let selected = self.selected_item == Some(idx);
            if ui.selectable_label(selected, item).clicked() {
                self.selected_item = Some(idx);
            }
        }
        ui.separator();
    }

    fn dynamic_group(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dynamic");
        ui.label(&self.status);
        ui.horizontal(|ui| {
            if ui.button("Submit").clicked() {
                self.status = if self.status == "Status: Submitted" {
                    "Status: Ready".to_string()
                } else {
                    "Status: Submitted".to_string()
                };
            }
            if ui.button("Add Item").clicked() {
                self.items.push(format!("Item {}", self.next_item_ix));
                self.next_item_ix += 1;
            }
            if ui.button("Remove Item").clicked() {
                self.items.pop();
            }
        });
    }
}

fn main() -> eframe::Result {
    // `with_active(true)` + `with_visible(true)` force the window to come up
    // foreground+focused on creation. accesskit-winit's macOS bridge only
    // publishes the AX tree once the host window has reported focus to winit,
    // and on the macos-latest GitHub runner an unbundled binary doesn't always
    // get a Focused event without an explicit activation request — matches the
    // hand-rolled `WindowEvent::Focused(true)` poke the AccessKit + winit test
    // app uses for the Xvfb path on Linux.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([520.0, 800.0])
            .with_active(true)
            .with_visible(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|_cc| Ok(Box::<TestApp>::default())),
    )
}
