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
    # When a plug creates its X window under the socket, GTK can handle
    # CreateNotify before the plug publishes _XEMBED_AT_SPI_PATH. Embed the
    # already-realized plug from the host so that property is available.
    plug = subprocess.Popen(
        [sys.executable, __file__, "plug"], stdout=subprocess.PIPE, text=True
    )

    def attach(_source, _condition):
        xid = plug.stdout.readline()
        if xid:
            socket.add_id(int(xid))
        else:
            print("Collection plug exited before publishing its X11 window ID", file=sys.stderr)
        return GLib.SOURCE_REMOVE

    try:
        GLib.io_add_watch(plug.stdout, GLib.PRIORITY_DEFAULT, GLib.IO_IN | GLib.IO_HUP, attach)
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, Gtk.main_quit)
        Gtk.main()
    finally:
        plug.terminate()
        plug.wait()


def embedded():
    GLib.set_prgname("xa11y-collection-plug")
    other = Gtk.Window(title="Unrelated window")
    other.add(Gtk.Button(label="Unrelated button"))
    other.show_all()
    plug = Gtk.Plug.new(0)
    plug.add(Gtk.Button(label="Embedded button"))
    plug.show_all()

    def publish_xid():
        plug.get_display().sync()
        print(plug.get_id(), flush=True)
        return GLib.SOURCE_REMOVE

    GLib.idle_add(publish_xid)
    Gtk.main()


if __name__ == "__main__":
    if len(sys.argv) == 1:
        host()
    else:
        embedded()
