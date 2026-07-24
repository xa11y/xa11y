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
from gi.repository import Gio, GLib, GObject, Gtk  # noqa: E402


class UserItem(GObject.Object):
    """Row model for the Users table (Gtk.ColumnView)."""

    name = GObject.Property(type=str, default="")
    role = GObject.Property(type=str, default="")

    def __init__(self, name: str, role: str) -> None:
        super().__init__(name=name, role=role)


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

        # Gtk.Button's label text is automatically its accessible name.
        self.ok_button = Gtk.Button(label="OK")
        self.ok_button.set_tooltip_text("Confirm the dialog")
        self.cancel_button = Gtk.Button(label="Cancel")
        self.cancel_button.set_sensitive(False)
        self.ok_button.connect("clicked", self._on_ok_clicked)
        btn_group.append(self.ok_button)
        btn_group.append(self.cancel_button)

        # ── Checkboxes ───────────────────────────────────────────────
        chk_group = self._make_group("Checkboxes")
        box.append(chk_group)

        # Gtk.CheckButton's label text is automatically its accessible name.
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
        for fruit in ["Apple", "Banana", "Cherry", "Date", "Elderberry"]:
            self.combo.append_text(fruit)
        self.combo.set_active(0)
        combo_group.append(self.combo)

        # ── Range controls ───────────────────────────────────────────
        range_group = self._make_group("Range Controls")
        box.append(range_group)

        # Only one slider in the app — identified by role in tests.
        self.slider = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1)
        self.slider.set_value(50)
        range_group.append(self.slider)

        # Only one spin_button in the app — identified by role + value in tests.
        spin_adj = Gtk.Adjustment(value=42, lower=0, upper=999, step_increment=1)
        self.spin = Gtk.SpinButton(adjustment=spin_adj)
        range_group.append(self.spin)

        # Only one progress_bar in the app — identified by role in tests.
        self.progress = Gtk.ProgressBar()
        self.progress.set_fraction(0.75)
        range_group.append(self.progress)

        # ── Text input ───────────────────────────────────────────────
        input_group = self._make_group("Input")
        box.append(input_group)

        # Only one text_field in the app — identified by value "hello world".
        self.text_entry = Gtk.Entry()
        self.text_entry.set_text("hello world")
        self.text_entry.set_placeholder_text("Type here...")
        input_group.append(self.text_entry)

        # ── Text area ────────────────────────────────────────────────
        text_group = self._make_group("Text")
        box.append(text_group)

        # Gtk.Label's accessible name comes from its text automatically.
        heading = Gtk.Label(label="Heading Text")
        text_group.append(heading)

        # Only one text_area in the app — identified by role in tests.
        self.text_view = Gtk.TextView()
        self.text_view.get_buffer().set_text("Line 1\nLine 2\nLine 3")
        text_group.append(self.text_view)

        # ── Switch ───────────────────────────────────────────────────
        switch_group = self._make_group("Switch")
        box.append(switch_group)

        # Gtk.Switch exposes AT-SPI role "toggle button" (62) → xa11y switch role.
        self.dark_mode_switch = Gtk.Switch()
        self.dark_mode_switch.set_active(False)
        self.dark_mode_switch.set_tooltip_text("Dark mode")
        switch_group.append(self.dark_mode_switch)

        # ── Menu button (wrapper pattern) ────────────────────────────
        # Gtk.MenuButton is the stock GNOME "outer push-button wraps inner
        # toggle-button" pattern.  In AT-SPI2 the outer push-button reports
        # NActions=0 while the inner toggle-button exposes `click`, so
        # calling press() on the outer must fall through to the inner under
        # the GTK-scoped press-fallback in xa11y-linux.
        menu_group = self._make_group("MenuButton")
        box.append(menu_group)
        popover_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        # This label becomes reachable in the AT-SPI tree once the popover
        # is shown — the integration test uses it to prove the inner
        # toggle-button was actually activated.
        popover_box.append(Gtk.Label(label="menu-popover-open"))
        popover = Gtk.Popover()
        popover.set_child(popover_box)
        self.menu_button = Gtk.MenuButton(label="More")
        self.menu_button.set_popover(popover)
        menu_group.append(self.menu_button)

        # ── List ─────────────────────────────────────────────────────
        list_group = self._make_group("List")
        box.append(list_group)

        # Only one list in the app — identified by role in tests.
        self.list_box = Gtk.ListBox()
        for i in range(1, 6):
            row = Gtk.ListBoxRow()
            row_label = Gtk.Label(label=f"Item {i}")
            row.set_child(row_label)
            self.list_box.append(row)
        list_group.append(self.list_box)

        # ── Table ────────────────────────────────────────────────────
        table_group = self._make_group("Table")
        box.append(table_group)

        # Only one table in the app — identified by role in tests.
        # Gtk.ColumnView exposes AT-SPI role "tree table" (66) → xa11y
        # table; its rows are "table row" and its cells "table cell",
        # each cell named from its child Gtk.Label's text.
        store = Gio.ListStore(item_type=UserItem)
        store.append(UserItem("Alice", "Admin"))
        store.append(UserItem("Bob", "User"))
        self.users_table = Gtk.ColumnView(model=Gtk.NoSelection(model=store))
        self.users_table.append_column(self._make_users_column("Name", "name"))
        self.users_table.append_column(self._make_users_column("Role", "role"))
        table_group.append(self.users_table)

        # ── Dynamic widgets (event tests) ────────────────────────────
        # These mutate on click so the event suite can observe:
        #   - NameChanged  (Submit → status label text change)
        #   - StructureChanged  (Add/Remove Item → list row add/remove)
        dyn_group = self._make_group("Dynamic")
        box.append(dyn_group)

        self.status_label = Gtk.Label(label="Status: Ready")
        dyn_group.append(self.status_label)

        self.submit_button = Gtk.Button(label="Submit")
        self.submit_button.connect("clicked", self._on_submit_clicked)
        dyn_group.append(self.submit_button)

        self.add_item_button = Gtk.Button(label="Add Item")
        self.add_item_button.connect("clicked", self._on_add_item_clicked)
        dyn_group.append(self.add_item_button)

        self.remove_item_button = Gtk.Button(label="Remove Item")
        self.remove_item_button.connect("clicked", self._on_remove_item_clicked)
        dyn_group.append(self.remove_item_button)

        # ── Dialog ───────────────────────────────────────────────────
        dlg_group = self._make_group("Dialogs")
        box.append(dlg_group)

        self.open_dialog_button = Gtk.Button(label="Open Dialog")
        self.open_dialog_button.connect("clicked", self._on_open_dialog_clicked)
        dlg_group.append(self.open_dialog_button)
        self._sample_dialog: Gtk.Window | None = None

    def _make_users_column(self, title: str, attr: str) -> Gtk.ColumnViewColumn:
        factory = Gtk.SignalListItemFactory()
        factory.connect(
            "setup", lambda _f, item: item.set_child(Gtk.Label(xalign=0))
        )
        factory.connect(
            "bind",
            lambda _f, item: item.get_child().set_label(
                item.get_item().get_property(attr)
            ),
        )
        return Gtk.ColumnViewColumn(title=title, factory=factory)

    def _make_group(self, name: str) -> Gtk.Box:
        inner = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        inner.set_margin_top(8)
        inner.set_margin_bottom(8)
        inner.set_margin_start(8)
        inner.set_margin_end(8)
        return inner

    def _on_ok_clicked(self, _btn: Gtk.Button) -> None:
        self.cancel_button.set_sensitive(True)

    # ── Dynamic widget handlers ──────────────────────────────────────

    def _list_rows(self) -> list[Gtk.ListBoxRow]:
        rows: list[Gtk.ListBoxRow] = []
        index = 0
        while (row := self.list_box.get_row_at_index(index)) is not None:
            rows.append(row)
            index += 1
        return rows

    def _on_submit_clicked(self, _btn: Gtk.Button) -> None:
        current = self.status_label.get_text()
        new = "Status: Submitted" if current != "Status: Submitted" else "Status: Ready"
        # Gtk.Label's accessible name follows its text automatically.
        self.status_label.set_text(new)

    def _on_add_item_clicked(self, _btn: Gtk.Button) -> None:
        new_index = len(self._list_rows()) + 1
        row = Gtk.ListBoxRow()
        row.set_child(Gtk.Label(label=f"Item {new_index}"))
        self.list_box.append(row)

    def _on_remove_item_clicked(self, _btn: Gtk.Button) -> None:
        rows = self._list_rows()
        if rows:
            self.list_box.remove(rows[-1])

    # ── Dialog handlers ──────────────────────────────────────────────

    def _on_open_dialog_clicked(self, _btn: Gtk.Button) -> None:
        if self._sample_dialog is None:
            # Gtk.Dialog is deprecated since GTK 4.10; a plain Gtk.Window
            # constructed with the (construct-only) DIALOG accessible role
            # exposes AT-SPI role "dialog" the same way. The window title
            # becomes its accessible name.
            dlg = Gtk.Window(
                accessible_role=Gtk.AccessibleRole.DIALOG,
                title="Sample Dialog",
            )
            dlg.set_transient_for(self)
            dlg.set_default_size(300, 120)
            content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
            content.set_margin_top(16)
            content.set_margin_bottom(16)
            content.set_margin_start(16)
            content.set_margin_end(16)
            close_btn = Gtk.Button(label="Close Dialog")
            close_btn.connect("clicked", lambda _b: dlg.set_visible(False))
            content.append(close_btn)
            dlg.set_child(content)
            # Hide instead of destroy so the dialog can be reopened by a
            # later test within the same app session.
            dlg.connect("close-request", self._on_dialog_close_request)
            self._sample_dialog = dlg
        self._sample_dialog.present()

    def _on_dialog_close_request(self, dlg: Gtk.Window) -> bool:
        dlg.set_visible(False)
        return True  # stop the default destroy handler


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
