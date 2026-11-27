"""Two GTK 3 processes exposing a single logical accessibility subtree on X11."""
import argparse
from pathlib import Path

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("side", choices=["host", "plug"])
    parser.add_argument("xid", type=Path, help="file containing the host socket's X11 ID")
    args = parser.parse_args()
    GLib.set_prgname(f"collection-{args.side}")
    if args.side == "host":
        window = Gtk.Window(title="Collection host")
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        box.add(Gtk.Button(label="Native host button"))
        socket = Gtk.Socket()
        box.add(socket)
        window.add(box)
        window.show_all()
        args.xid.write_text(str(socket.get_id()))
    else:
        window = Gtk.Plug.new(int(args.xid.read_text()))
        window.add(Gtk.Button(label="Embedded button"))
        window.show_all()
    Gtk.main()


if __name__ == "__main__":
    main()
