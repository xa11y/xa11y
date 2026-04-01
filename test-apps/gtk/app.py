"""xa11y GTK4 test app.

A single-window GTK4 application exposing the standard widget set used by
the xa11y integration test suite.  Run with:

    python app.py [--pid-file PATH]
"""

from __future__ import annotations

import argparse
import os
import signal
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402


class TestWindow(Gtk.ApplicationWindow):
    def __init__(self, app: Gtk.Application) -> None:
        super().__init__(application=app, title="xa11y-gtk-test-app")
        self.set_default_size(800, 700)

        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        self.set_child(scroll)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.set_margin_top(16)
        box.set_margin_bottom(16)
        box.set_margin_start(16)
        box.set_margin_end(16)
        scroll.set_child(box)

        # ── Buttons ──────────────────────────────────────────────────
        btn_group = self._make_group("Buttons")
        box.append(btn_group)

        self.ok_button = Gtk.Button(label="OK")
        self.ok_button.set_tooltip_text("Confirm the dialog")
        self.ok_button.set_accessible_description("Confirm the dialog")
        self.cancel_button = Gtk.Button(label="Cancel")
        self.cancel_button.set_sensitive(False)
        self.ok_button.connect("clicked", self._on_ok_clicked)
        btn_group.append(self.ok_button)
        btn_group.append(self.cancel_button)

        # ── Checkboxes ───────────────────────────────────────────────
        chk_group = self._make_group("Checkboxes")
        box.append(chk_group)

        self.agree_check = Gtk.CheckButton(label="Agree to terms")
        self.subscribe_check = Gtk.CheckButton(label="Subscribe")
        self.subscribe_check.set_active(True)
        chk_group.append(self.agree_check)
        chk_group.append(self.subscribe_check)

        # ── Radio buttons ────────────────────────────────────────────
        radio_group = self._make_group("Options")
        box.append(radio_group)

        self.radio_a = Gtk.CheckButton(label="Option A")
        self.radio_a.set_active(True)
        self.radio_b = Gtk.CheckButton(label="Option B")
        self.radio_b.set_group(self.radio_a)
        self.radio_c = Gtk.CheckButton(label="Option C")
        self.radio_c.set_group(self.radio_a)
        radio_group.append(self.radio_a)
        radio_group.append(self.radio_b)
        radio_group.append(self.radio_c)

        # ── ComboBox ─────────────────────────────────────────────────
        combo_group = self._make_group("ComboBox")
        box.append(combo_group)

        self.combo = Gtk.ComboBoxText()
        self.combo.set_accessible_description("Fruit selector")
        for fruit in ["Apple", "Banana", "Cherry", "Date", "Elderberry"]:
            self.combo.append_text(fruit)
        self.combo.set_active(0)
        combo_group.append(self.combo)

        # ── Range controls ───────────────────────────────────────────
        range_group = self._make_group("Range Controls")
        box.append(range_group)

        # Label linked via set_mnemonic_widget establishes the GTK4
        # LABELLED_BY accessibility relation, giving the widget its name.
        self.slider = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1)
        self.slider.set_value(50)
        self.slider.set_accessible_description("Volume")
        vol_label = Gtk.Label(label="Volume")
        vol_label.set_mnemonic_widget(self.slider)
        range_group.append(vol_label)
        range_group.append(self.slider)

        spin_adj = Gtk.Adjustment(value=42, lower=0, upper=999, step_increment=1)
        self.spin = Gtk.SpinButton(adjustment=spin_adj)
        qty_label = Gtk.Label(label="Quantity")
        qty_label.set_mnemonic_widget(self.spin)
        range_group.append(qty_label)
        range_group.append(self.spin)

        self.progress = Gtk.ProgressBar()
        self.progress.set_fraction(0.75)
        prog_label = Gtk.Label(label="Progress")
        prog_label.set_mnemonic_widget(self.progress)
        range_group.append(prog_label)
        range_group.append(self.progress)

        # ── Text input ───────────────────────────────────────────────
        input_group = self._make_group("Input")
        box.append(input_group)

        self.text_entry = Gtk.Entry()
        self.text_entry.set_text("hello world")
        self.text_entry.set_placeholder_text("Type here...")
        search_label = Gtk.Label(label="Search")
        search_label.set_mnemonic_widget(self.text_entry)
        input_group.append(search_label)
        input_group.append(self.text_entry)

        # ── Text area ────────────────────────────────────────────────
        text_group = self._make_group("Text")
        box.append(text_group)

        # Gtk.Label's accessible name comes from its text automatically.
        heading = Gtk.Label(label="Heading Text")
        text_group.append(heading)

        self.text_view = Gtk.TextView()
        self.text_view.get_buffer().set_text("Line 1\nLine 2\nLine 3")
        notes_label = Gtk.Label(label="Notes")
        notes_label.set_mnemonic_widget(self.text_view)
        text_group.append(notes_label)
        text_group.append(self.text_view)

        # ── List ─────────────────────────────────────────────────────
        list_group = self._make_group("List")
        box.append(list_group)

        self.list_box = Gtk.ListBox()
        items_label = Gtk.Label(label="Items")
        items_label.set_mnemonic_widget(self.list_box)
        list_group.append(items_label)
        for i in range(1, 6):
            row = Gtk.ListBoxRow()
            row_label = Gtk.Label(label=f"Item {i}")
            row.set_child(row_label)
            self.list_box.append(row)
        list_group.append(self.list_box)

    def _make_group(self, name: str) -> Gtk.Box:
        inner = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        inner.set_margin_top(8)
        inner.set_margin_bottom(8)
        inner.set_margin_start(8)
        inner.set_margin_end(8)
        return inner

    def _on_ok_clicked(self, _btn: Gtk.Button) -> None:
        self.cancel_button.set_sensitive(True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid-file", help="Write PID to this file on startup")
    args = parser.parse_args()

    app = Gtk.Application(application_id="com.xa11y.gtk-test-app")

    def on_activate(application: Gtk.Application) -> None:
        win = TestWindow(application)
        win.present()
        if args.pid_file:
            with open(args.pid_file, "w") as f:
                f.write(str(os.getpid()))

    app.connect("activate", on_activate)

    # Handle SIGTERM for clean shutdown
    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, app.quit)

    sys.exit(app.run([]))


if __name__ == "__main__":
    main()
