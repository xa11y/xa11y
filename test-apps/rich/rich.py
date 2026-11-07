"""xa11y rich-content fixture — tables, collections and rich text on Linux.

The AccessKit test app that the rest of `xa11y/tests/integ/` drives cannot
show these: `accesskit_unix` implements no `org.a11y.atspi.Table` or
`TableCell` interface, reports no text attribute runs, and sets no
`posinset` / `setsize` / `level` object attributes. GTK 3's ATK adapter
implements all of them, so this is a small GTK 3 window holding one widget
per feature that `xa11y/tests/integ/rich.rs` asserts against:

* a `GtkTreeView` over a `GtkListStore` — an AT-SPI `tree table` with
  column headers, `TableCell` positions, and multiple row selection;
* a `GtkTreeView` over a `GtkTreeStore` — nested rows with a `level`;
* a `GtkListBox` — list items;
* a `GtkTextView` with tagged (bold, italic, underlined, coloured) text,
  several paragraphs, and non-BMP characters, so code-point and UTF-16
  offsets differ;
* a `GtkButton`, which has no `Text` interface, for the refusal path.

GTK 3 has no merged cells: `GtkTreeView` reports a span of 1 for every cell.
The span assertions in `rich.rs` say so rather than pretending otherwise.

Launched by `scripts/run_integ_tests.sh`, like `test-apps/panel/panel.py`:

    python3 test-apps/rich/rich.py

Needs PyGObject and the GTK 3 typelib (`python3-gi gir1.2-gtk-3.0`).
"""

from __future__ import annotations

import signal
import sys

import gi

# Pin Gdk too: see the note in test-apps/panel/panel.py.
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import GLib, Gtk, Pango  # noqa: E402

# The AT-SPI application name, which `rich.rs` finds the fixture by. GTK
# takes it from the program name, which would otherwise be `rich.py`.
APP_NAME = "xa11y-rich-fixture"

TABLE_COLUMNS = ("Fruit", "Colour", "Qty")
TABLE_ROWS = (
    ("Apple", "Red", "3"),
    ("Banana", "Yellow", "12"),
    ("Cherry", "Dark red", "40"),
)

# 😀 and 𝄞 are outside the BMP: one code point each, two UTF-16 units.
TEXT_PARAGRAPHS = (
    "Hello wörld 😀 bold italic",
    "Second line under red 𝄞 end",
    "",
    "Final paragraph",
)


def table_view() -> Gtk.TreeView:
    store = Gtk.ListStore(str, str, str)
    for row in TABLE_ROWS:
        store.append(row)
    view = Gtk.TreeView(model=store)
    for i, title in enumerate(TABLE_COLUMNS):
        view.append_column(Gtk.TreeViewColumn(title, Gtk.CellRendererText(), text=i))
    selection = view.get_selection()
    selection.set_mode(Gtk.SelectionMode.MULTIPLE)
    selection.select_path(Gtk.TreePath.new_from_indices([1]))
    view.get_accessible().set_name("Fruit table")
    return view


def tree_view() -> Gtk.TreeView:
    store = Gtk.TreeStore(str)
    produce = store.append(None, ("Produce",))
    store.append(produce, ("Carrot",))
    store.append(produce, ("Leek",))
    store.append(None, ("Dairy",))
    view = Gtk.TreeView(model=store)
    view.append_column(Gtk.TreeViewColumn("Aisle", Gtk.CellRendererText(), text=0))
    view.expand_all()
    view.get_accessible().set_name("Aisle tree")
    return view


def list_box() -> Gtk.ListBox:
    box = Gtk.ListBox()
    for label in ("First entry", "Second entry", "Third entry"):
        box.add(Gtk.Label(label=label))
    box.get_accessible().set_name("Entry list")
    return box


def text_view() -> Gtk.TextView:
    buf = Gtk.TextBuffer()
    buf.set_text("\n".join(TEXT_PARAGRAPHS))
    bold = buf.create_tag("bold", weight=Pango.Weight.BOLD)
    italic = buf.create_tag("italic", style=Pango.Style.ITALIC)
    underline = buf.create_tag(
        "underline", underline=Pango.Underline.SINGLE, foreground="#ff0000"
    )

    def tag_word(tag: Gtk.TextTag, word: str) -> None:
        start = buf.get_start_iter()
        found = start.forward_search(word, Gtk.TextSearchFlags.TEXT_ONLY, None)
        if found is None:
            raise RuntimeError(f"fixture text has no {word!r}")
        buf.apply_tag(tag, *found)

    tag_word(bold, "bold")
    tag_word(italic, "italic")
    tag_word(underline, "under red")
    # A caret at the start, nothing selected; the tests set their own.
    buf.place_cursor(buf.get_start_iter())

    view = Gtk.TextView(buffer=buf)
    view.set_wrap_mode(Gtk.WrapMode.NONE)
    view.get_accessible().set_name("Rich text")
    return view


def main() -> int:
    GLib.set_prgname(APP_NAME)
    GLib.set_application_name(APP_NAME)

    window = Gtk.Window(title="xa11y rich fixture")
    window.set_default_size(640, 600)

    column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
    column.pack_start(table_view(), False, False, 0)
    column.pack_start(tree_view(), False, False, 0)
    column.pack_start(list_box(), False, False, 0)
    column.pack_start(text_view(), True, True, 0)
    column.pack_start(Gtk.Button(label="Plain button"), False, False, 0)
    window.add(column)

    window.connect("destroy", Gtk.main_quit)
    window.show_all()

    signal.signal(signal.SIGTERM, lambda *_: Gtk.main_quit())
    signal.signal(signal.SIGINT, lambda *_: Gtk.main_quit())

    Gtk.main()
    return 0


if __name__ == "__main__":
    sys.exit(main())
