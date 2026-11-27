"""A logical GTK 3 subtree spanning two AT-SPI peers on X11."""
import signal
import subprocess
import sys

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk


def host():
    GLib.set_prgname("xa11y-collection-host")
    window = Gtk.Window(title="Collection host")
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    window.add(box)
    box.add(Gtk.Button(label="Before socket"))
    socket = Gtk.Socket()
    box.add(socket)
    after = Gtk.Button(label="After socket")
    after.connect("clicked", lambda button: button.set_label("Renamed after socket"))
    box.add(after)
    # A native recursive collection also sees the last button, past xa11y's
    # logical depth budget. GTK exposes each box as an AT-SPI filler.
    for depth in range(58):
        child = Gtk.Box()
        if depth == 28:
            child.get_accessible().set_name("Midpoint")
        box.add(child)
        box = child
    box.add(Gtk.Button(label="Too deep"))
    window.show_all()
    plug = subprocess.Popen([sys.executable, __file__, str(socket.get_id())])
    try:
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, Gtk.main_quit)
        Gtk.main()
    finally:
        plug.terminate()
        plug.wait()


def embedded(xid):
    GLib.set_prgname("xa11y-collection-plug")
    other = Gtk.Window(title="Unrelated window")
    other.add(Gtk.Button(label="Unrelated button"))
    other.show_all()
    plug = Gtk.Plug.new(xid)
    plug.add(Gtk.Button(label="Embedded button"))
    plug.show_all()
    Gtk.main()


if __name__ == "__main__":
    if len(sys.argv) == 1:
        host()
    else:
        embedded(int(sys.argv[1]))
