"""xa11y test panel — a Linux desktop panel for the shell-surface tests.

The Linux shell-surface classifier keys on one thing: an AT-SPI `frame`
carrying the `window-type:dock` attribute, which mirrors the window
manager's `_NET_WM_WINDOW_TYPE` hint. Nothing in the Xvfb integ environment
vends one — no desktop environment runs there — so
`ShellSurface::by_kind(Panel)` had nothing to match and the Linux half of
the feature was covered only by a unit test against a synthetic attribute
map (issue #383).

This is the smallest thing that produces a real one: a GTK3 window with
`GDK_WINDOW_TYPE_HINT_DOCK`, which sets both the X11 property and the
AT-SPI attribute the classifier reads. GTK3 rather than GTK4 because GTK4
dropped type hints entirely and has no way to express "this window is a
dock".

Launched by `scripts/run_integ_tests.sh`; the widgets below are what
`xa11y/tests/integ/shell.rs` asserts it can reach through the surface root.

    python3 test-apps/panel/panel.py

Needs PyGObject and the GTK 3 typelib (`python3-gi gir1.2-gtk-3.0` on
Debian/Ubuntu).
"""

from __future__ import annotations

import signal
import sys

import gi

# Both versions have to be pinned, not just Gtk. A machine that also has the
# GTK 4 typelib installed — which any checkout running the `gtk` test app does
# — resolves a bare `from gi.repository import Gdk` to Gdk 4.0, the newest
# available, and Gtk 3.0 then fails to load against it with
# `RepositoryError: Requiring namespace 'Gdk' version '3.0', but '4.0' is
# already loaded`. The panel dies on import and the display vends no dock
# frame.
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, Gtk  # noqa: E402

# The window title becomes the surface's name, since a frame's AT-SPI name is
# its title. `shell.rs` matches on it to prove it found *this* panel rather
# than some other dock frame that happened to be on the display.
PANEL_TITLE = "xa11y-test-panel"

# One named, pressable widget inside the panel: the assertion that a surface
# root is an ordinary search root, not just a node that enumerates.
PANEL_BUTTON_LABEL = "Panel Button"


def main() -> int:
    window = Gtk.Window(title=PANEL_TITLE)
    # The one line this fixture exists for.
    window.set_type_hint(Gdk.WindowTypeHint.DOCK)
    window.set_default_size(640, 32)
    window.move(0, 0)

    row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
    row.pack_start(Gtk.Label(label="xa11y panel"), False, False, 8)
    row.pack_start(Gtk.Button(label=PANEL_BUTTON_LABEL), False, False, 8)
    window.add(row)

    window.connect("destroy", Gtk.main_quit)
    window.show_all()

    # The harness stops the panel with SIGTERM; without this the default
    # handler kills the process before GTK can unmap the window, which leaves
    # the frame in the AT-SPI registry for the next run to trip over.
    signal.signal(signal.SIGTERM, lambda *_: Gtk.main_quit())
    signal.signal(signal.SIGINT, lambda *_: Gtk.main_quit())

    Gtk.main()
    return 0


if __name__ == "__main__":
    sys.exit(main())
