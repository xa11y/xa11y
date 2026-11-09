"""xa11y stale-handle fixture: destroyed GTK 3 widgets that still answer.

GTK 3 keeps a destroyed widget's accessible registered on the AT-SPI bus for
roughly 15-20 s, and the zombie keeps answering: `DoAction`, `GrabFocus`,
`Value` and `EditableText` writes can all come back as if they worked (issues
#491 and #586). GTK 4, which `test-apps/gtk` uses, answers a destroyed object
at once with `UnknownMethod`, so it cannot show this, and the AccessKit test
app removes nodes outright. This is the GTK 3 window that
`xa11y/tests/integ/stale.rs` drives:

* a "Rebuild" button that destroys every target below and builds a fresh
  set with the same names. Its description carries the rebuild count, so a
  test can tell when the swap has happened;
* the targets: a `GtkButton` ("Doomed button"), a `GtkEntry`
  ("Doomed entry"), a `GtkSpinButton` ("Doomed spin"), and a second
  top-level `GtkWindow` ("Doomed window").

A handle read before a rebuild names a destroyed widget, and every action on
it must fail as stale. The fresh widgets are the control: the same actions
through a re-resolving locator must succeed, so a check that called
everything stale would fail the test too.

Launched by `scripts/run_integ_tests.sh`, like `test-apps/panel/panel.py`:

    python3 test-apps/stale/stale.py

Needs PyGObject and the GTK 3 typelib (`python3-gi gir1.2-gtk-3.0`).
"""

from __future__ import annotations

import signal
import sys

import gi

# Pin Gdk too: see the note in test-apps/panel/panel.py.
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402

# The AT-SPI application name, which `stale.rs` finds the fixture by.
APP_NAME = "xa11y-stale-fixture"


class Fixture:
    def __init__(self) -> None:
        self.rebuilds = 0
        self.window = Gtk.Window(title="xa11y stale fixture")
        self.window.set_default_size(320, 240)
        column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        self.rebuild_button = Gtk.Button(label="Rebuild")
        self.rebuild_button.connect("clicked", self._on_rebuild)
        column.pack_start(self.rebuild_button, False, False, 0)
        # The targets live in their own box so a rebuild can empty it.
        self.targets = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        column.pack_start(self.targets, True, True, 0)
        self.window.add(column)
        self.doomed_window: Gtk.Window | None = None
        self.window.connect("destroy", Gtk.main_quit)
        self.window.show_all()
        self._build_targets()

    def _build_targets(self) -> None:
        # Only the doomed window is referenced from here, and only until the
        # next rebuild destroys it: the zombie is GTK's (and the AT-SPI
        # bridge's) doing, not a reference held by this script.
        button = Gtk.Button(label="Doomed button")

        entry = Gtk.Entry()
        entry.set_text("doomed text")
        entry.get_accessible().set_name("Doomed entry")

        spin = Gtk.SpinButton.new_with_range(0, 100, 1)
        spin.set_value(10)
        spin.get_accessible().set_name("Doomed spin")

        for widget in (button, entry, spin):
            self.targets.pack_start(widget, False, False, 0)
        self.targets.show_all()

        self.doomed_window = Gtk.Window(title="Doomed window")
        self.doomed_window.set_default_size(200, 80)
        self.doomed_window.add(Gtk.Label(label="This window is rebuilt"))
        self.doomed_window.show_all()

        self.rebuild_button.get_accessible().set_description(
            f"rebuilds: {self.rebuilds}"
        )

    def _on_rebuild(self, _button: Gtk.Button) -> None:
        for child in self.targets.get_children():
            child.destroy()
        if self.doomed_window is not None:
            self.doomed_window.destroy()
            self.doomed_window = None
        self.rebuilds += 1
        self._build_targets()


def main() -> int:
    GLib.set_prgname(APP_NAME)
    GLib.set_application_name(APP_NAME)

    fixture = Fixture()

    signal.signal(signal.SIGTERM, lambda *_: Gtk.main_quit())
    signal.signal(signal.SIGINT, lambda *_: Gtk.main_quit())

    Gtk.main()
    del fixture
    return 0


if __name__ == "__main__":
    sys.exit(main())
