"""PySide6 test application for xa11y integration tests.

Exposes every common Qt widget type on a single scrollable page so that
all widgets are visible to accessibility APIs regardless of tab state.

Launch:  python app.py [--pid-file PATH]
The optional --pid-file writes the PID so the test harness can kill it.
"""

from __future__ import annotations

import argparse
import sys

from PySide6.QtCore import QPoint, Qt, QTimer
from PySide6.QtGui import QFont, QTextCharFormat, QTextCursor
from PySide6.QtWidgets import (
    QApplication,
    QCheckBox,
    QComboBox,
    QDialog,
    QDoubleSpinBox,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QMainWindow,
    QPlainTextEdit,
    QMenuBar,
    QProgressBar,
    QPushButton,
    QRadioButton,
    QScrollArea,
    QSlider,
    QSpinBox,
    QStatusBar,
    QTableWidget,
    QTableWidgetItem,
    QTabWidget,
    QTextEdit,
    QToolBar,
    QTreeWidget,
    QTreeWidgetItem,
    QVBoxLayout,
    QWidget,
)


class TestWindow(QMainWindow):
    """Main window containing all widget types for accessibility testing."""

    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("xa11y-qt-test-app")
        # The minimum sits well below the initial size so the window-geometry
        # suites can shrink the window (a grown target can be refused on a
        # small display that the window already fills).
        self.setMinimumSize(400, 300)
        self.resize(800, 600)

        self._build_menu_bar()
        self._build_toolbar()
        self._build_status_bar()

        # Single scrollable page with all widgets
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        content = QWidget()
        layout = QVBoxLayout(content)

        self._add_buttons(layout)
        self._add_checkboxes(layout)
        self._add_radio_buttons(layout)
        self._add_comboboxes(layout)
        self._add_range_controls(layout)
        self._add_input(layout)
        self._add_text(layout)
        self._add_list(layout)
        self._add_table(layout)
        self._add_tree(layout)
        self._add_dynamic(layout)
        self._add_dialogs(layout)
        self._add_rich(layout)

        layout.addStretch()
        scroll.setWidget(content)
        self.setCentralWidget(scroll)

    # ── Menu bar ────────────────────────────────────────────────────

    def _build_menu_bar(self) -> None:
        mb: QMenuBar = self.menuBar()
        file_menu = mb.addMenu("&File")
        file_menu.addAction("&New")
        file_menu.addAction("&Open")
        file_menu.addAction("&Save")
        file_menu.addSeparator()
        file_menu.addAction("E&xit")
        self.file_menu = file_menu

        edit_menu = mb.addMenu("&Edit")
        edit_menu.addAction("&Undo")
        edit_menu.addAction("&Redo")

        help_menu = mb.addMenu("&Help")
        help_menu.addAction("&About")

    # ── Toolbar ─────────────────────────────────────────────────────

    def _build_toolbar(self) -> None:
        tb: QToolBar = self.addToolBar("Main Toolbar")
        tb.setAccessibleName("Main Toolbar")
        tb.addAction("New")
        tb.addAction("Open")
        tb.addAction("Save")

    # ── Status bar ──────────────────────────────────────────────────

    def _build_status_bar(self) -> None:
        sb: QStatusBar = self.statusBar()
        sb.showMessage("Ready")

    # ── Widget sections ─────────────────────────────────────────────

    def _add_buttons(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Buttons")
        grp.setAccessibleName("Buttons")
        lay = QHBoxLayout(grp)

        self.ok_btn = QPushButton("OK")
        self.ok_btn.setAccessibleName("OK")
        self.ok_btn.setAccessibleDescription("Confirm the dialog")
        lay.addWidget(self.ok_btn)

        self.cancel_btn = QPushButton("Cancel")
        self.cancel_btn.setAccessibleName("Cancel")
        self.cancel_btn.setEnabled(False)
        lay.addWidget(self.cancel_btn)

        self.ok_btn.clicked.connect(self._on_ok_clicked)

        open_file_menu_btn = QPushButton("Open File Menu")
        open_file_menu_btn.setAccessibleName("Open File Menu")
        open_file_menu_btn.clicked.connect(self._open_file_menu)
        lay.addWidget(open_file_menu_btn)
        parent_layout.addWidget(grp)

    def _on_ok_clicked(self) -> None:
        self.cancel_btn.setEnabled(not self.cancel_btn.isEnabled())
        self.statusBar().showMessage("OK clicked")

    def _open_file_menu(self) -> None:
        self.file_menu.popup(self.mapToGlobal(QPoint(20, 40)))

    def _add_checkboxes(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Checkboxes")
        grp.setAccessibleName("Checkboxes")
        lay = QVBoxLayout(grp)

        self.agree_chk = QCheckBox("Agree to terms")
        self.agree_chk.setAccessibleName("Agree to terms")
        lay.addWidget(self.agree_chk)

        self.subscribe_chk = QCheckBox("Subscribe")
        self.subscribe_chk.setAccessibleName("Subscribe")
        self.subscribe_chk.setChecked(True)
        lay.addWidget(self.subscribe_chk)

        self.agree_chk.toggled.connect(
            lambda checked: self.statusBar().showMessage(f"Agree: {checked}")
        )
        parent_layout.addWidget(grp)

    def _add_radio_buttons(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Options")
        grp.setAccessibleName("Options")
        lay = QVBoxLayout(grp)

        self.radio_a = QRadioButton("Option A")
        self.radio_a.setAccessibleName("Option A")
        self.radio_a.setChecked(True)
        lay.addWidget(self.radio_a)

        self.radio_b = QRadioButton("Option B")
        self.radio_b.setAccessibleName("Option B")
        lay.addWidget(self.radio_b)

        self.radio_c = QRadioButton("Option C")
        self.radio_c.setAccessibleName("Option C")
        lay.addWidget(self.radio_c)

        parent_layout.addWidget(grp)

    def _add_comboboxes(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("ComboBox")
        grp.setAccessibleName("ComboBox")
        lay = QVBoxLayout(grp)

        self.combo = QComboBox()
        self.combo.setAccessibleName("Fruit")
        self.combo.addItems(["Apple", "Banana", "Cherry", "Date", "Elderberry"])
        lay.addWidget(self.combo)

        self.editable_combo = QComboBox()
        self.editable_combo.setAccessibleName("Color")
        self.editable_combo.setEditable(True)
        self.editable_combo.addItems(["Red", "Green", "Blue"])
        lay.addWidget(self.editable_combo)

        parent_layout.addWidget(grp)

    def _add_range_controls(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Range Controls")
        grp.setAccessibleName("Range Controls")
        lay = QVBoxLayout(grp)

        self.slider = QSlider(Qt.Orientation.Horizontal)
        self.slider.setAccessibleName("Volume")
        self.slider.setRange(0, 100)
        self.slider.setValue(50)
        lay.addWidget(self.slider)

        self.spinner = QSpinBox()
        self.spinner.setAccessibleName("Quantity")
        self.spinner.setRange(0, 999)
        self.spinner.setValue(42)
        lay.addWidget(self.spinner)

        self.double_spinner = QDoubleSpinBox()
        self.double_spinner.setAccessibleName("Price")
        self.double_spinner.setRange(0.0, 9999.99)
        self.double_spinner.setValue(19.99)
        self.double_spinner.setDecimals(2)
        lay.addWidget(self.double_spinner)

        self.progress = QProgressBar()
        self.progress.setAccessibleName("Progress")
        self.progress.setRange(0, 100)
        self.progress.setValue(75)
        lay.addWidget(self.progress)

        parent_layout.addWidget(grp)

    def _add_input(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Input")
        grp.setAccessibleName("Input")
        lay = QVBoxLayout(grp)

        self.line_edit = QLineEdit("hello world")
        self.line_edit.setAccessibleName("Search")
        self.line_edit.setPlaceholderText("Type here...")
        lay.addWidget(self.line_edit)

        parent_layout.addWidget(grp)

    def _add_text(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Text")
        grp.setAccessibleName("Text")
        lay = QVBoxLayout(grp)

        self.heading_label = QLabel("Heading Text")
        self.heading_label.setAccessibleName("Heading Text")
        lay.addWidget(self.heading_label)

        self.text_edit = QTextEdit()
        self.text_edit.setAccessibleName("Notes")
        self.text_edit.setPlainText("Line 1\nLine 2\nLine 3")
        self.text_edit.setMaximumHeight(100)
        lay.addWidget(self.text_edit)

        parent_layout.addWidget(grp)

    def _add_list(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("List")
        grp.setAccessibleName("List")
        lay = QVBoxLayout(grp)

        self.list_widget = QListWidget()
        self.list_widget.setAccessibleName("Items")
        self.list_widget.setMaximumHeight(120)
        for i in range(5):
            self.list_widget.addItem(f"Item {i + 1}")
        lay.addWidget(self.list_widget)

        # A list longer than its viewport (issue #590): its first entry is in
        # view and its last is scrolled out. Qt's AT-SPI bridge clears
        # VISIBLE on a row outside the view, which xa11y reports as
        # hidden_reason "scrolled_out" because the row's parent is a list
        # and the row lies outside it.
        self.long_pane = QListWidget()
        self.long_pane.setAccessibleName("Long Pane")
        self.long_pane.setFixedHeight(60)
        for i in range(10):
            self.long_pane.addItem(f"Long Entry {i + 1}")
        lay.addWidget(self.long_pane)
        # Qt's AT-SPI bridge offers no way to scroll a row into view (no
        # ScrollTo, no focus or select action on the row), so the suite
        # scrolls the pane by pressing these.
        to_end = QPushButton("Scroll Long Pane To End")
        to_end.clicked.connect(self.long_pane.scrollToBottom)
        lay.addWidget(to_end)
        to_start = QPushButton("Scroll Long Pane To Start")
        to_start.clicked.connect(self.long_pane.scrollToTop)
        lay.addWidget(to_start)

        parent_layout.addWidget(grp)

        # An inactive tab page: hidden by a container that does not scroll,
        # so its contents must read hidden_reason "not_showing" (#590).
        self.tabs = QTabWidget()
        self.tabs.setAccessibleName("Tabs")
        self.tabs.addTab(QPushButton("Tab One Button"), "Tab One")
        self.tabs.addTab(QPushButton("Tab Two Button"), "Tab Two")
        parent_layout.addWidget(self.tabs)

    def _add_table(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Table")
        grp.setAccessibleName("Table")
        lay = QVBoxLayout(grp)

        self.users_table = QTableWidget(2, 2)
        self.users_table.setAccessibleName("Users Table")
        self.users_table.setHorizontalHeaderLabels(["Name", "Role"])
        self.users_table.setVerticalHeaderLabels(["Row 1", "Row 2"])
        for row, values in enumerate((("Alice", "Admin"), ("Bob", "User"))):
            for column, value in enumerate(values):
                self.users_table.setItem(row, column, QTableWidgetItem(value))
        # Select a single cell so the suites can assert that per-cell
        # selection state survives every platform bridge (UIA SelectionItem,
        # AT-SPI selected state, AX container selection on macOS).
        self.users_table.setSelectionBehavior(
            QTableWidget.SelectionBehavior.SelectItems
        )
        self.users_table.setSelectionMode(QTableWidget.SelectionMode.SingleSelection)
        self.users_table.setCurrentCell(0, 0)
        self.users_table.setMaximumHeight(120)
        lay.addWidget(self.users_table)

        parent_layout.addWidget(grp)

    def _add_tree(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Tree")
        grp.setAccessibleName("Tree")
        lay = QVBoxLayout(grp)

        self.tree_widget = QTreeWidget()
        self.tree_widget.setAccessibleName("File Browser")
        self.tree_widget.setHeaderLabels(["Name", "Size"])
        self.tree_widget.setMaximumHeight(150)
        root_item = QTreeWidgetItem(["Documents", ""])
        root_item.addChild(QTreeWidgetItem(["report.pdf", "2.1 MB"]))
        root_item.addChild(QTreeWidgetItem(["notes.txt", "12 KB"]))
        self.tree_widget.addTopLevelItem(root_item)
        photos_item = QTreeWidgetItem(["Photos", ""])
        photos_item.addChild(QTreeWidgetItem(["vacation.jpg", "4.5 MB"]))
        self.tree_widget.addTopLevelItem(photos_item)
        self.tree_widget.expandAll()
        lay.addWidget(self.tree_widget)

        parent_layout.addWidget(grp)

    def _add_dynamic(self, parent_layout: QVBoxLayout) -> None:
        """Dynamic widgets used by the event test suite.

        These mutate on action so event tests can exercise:
          - NameChanged  (Submit → status label text change)
          - StructureChanged  (Add/Remove Item → list row add/remove)
        """
        grp = QGroupBox("Dynamic")
        grp.setAccessibleName("Dynamic")
        lay = QVBoxLayout(grp)

        self.status_label = QLabel("Status: Ready")
        self.status_label.setAccessibleName("Status: Ready")
        lay.addWidget(self.status_label)

        self.submit_btn = QPushButton("Submit")
        self.submit_btn.setAccessibleName("Submit")
        self.submit_btn.clicked.connect(self._on_submit_clicked)
        lay.addWidget(self.submit_btn)

        self.add_item_btn = QPushButton("Add Item")
        self.add_item_btn.setAccessibleName("Add Item")
        self.add_item_btn.clicked.connect(self._on_add_item_clicked)
        lay.addWidget(self.add_item_btn)

        self.remove_item_btn = QPushButton("Remove Item")
        self.remove_item_btn.setAccessibleName("Remove Item")
        self.remove_item_btn.clicked.connect(self._on_remove_item_clicked)
        lay.addWidget(self.remove_item_btn)

        parent_layout.addWidget(grp)

    def _on_submit_clicked(self) -> None:
        current = self.status_label.text()
        new = "Status: Submitted" if current != "Status: Submitted" else "Status: Ready"
        self.status_label.setText(new)
        self.status_label.setAccessibleName(new)

    def _on_add_item_clicked(self) -> None:
        new_index = self.list_widget.count() + 1
        self.list_widget.addItem(f"Item {new_index}")

    def _on_remove_item_clicked(self) -> None:
        count = self.list_widget.count()
        if count > 0:
            self.list_widget.takeItem(count - 1)

    def _add_dialogs(self, parent_layout: QVBoxLayout) -> None:
        grp = QGroupBox("Dialogs")
        grp.setAccessibleName("Dialogs")
        lay = QVBoxLayout(grp)

        open_btn = QPushButton("Open Dialog")
        open_btn.setAccessibleName("Open Dialog")
        open_btn.clicked.connect(self._open_sample_dialog)
        lay.addWidget(open_btn)

        parent_layout.addWidget(grp)

    def _open_sample_dialog(self) -> None:
        if not hasattr(self, "_sample_dialog"):
            dlg = QDialog(self)
            dlg.setWindowTitle("Sample Dialog")
            dlg.setAccessibleName("Sample Dialog")
            layout = QVBoxLayout(dlg)
            close_btn = QPushButton("Close Dialog")
            close_btn.setAccessibleName("Close Dialog")
            close_btn.clicked.connect(dlg.close)
            layout.addWidget(close_btn)
            self._sample_dialog = dlg
        self._sample_dialog.show()
        self._sample_dialog.raise_()


    def _add_rich(self, parent_layout: QVBoxLayout) -> None:
        """Text by range and merged table cells, read by test_qt_rich.py.

        Qt's AT-SPI adaptor counts text offsets in UTF-16 code units, where
        GTK counts code points, so the non-BMP characters here exercise the
        provider's UTF-16 path end to end. The table merges cells, which the
        GTK fixture cannot.
        """
        grp = QGroupBox("Rich")
        grp.setAccessibleName("Rich")
        lay = QVBoxLayout(grp)

        self.unicode_notes = QTextEdit()
        self.unicode_notes.setAccessibleName("Unicode Notes")
        self.unicode_notes.setLineWrapMode(QTextEdit.LineWrapMode.NoWrap)
        self.unicode_notes.setPlainText(UNICODE_NOTES_TEXT)
        self.unicode_notes.setMinimumHeight(90)
        # Bold "wörld" (UTF-16 6..11) so a formatting run boundary exists.
        cursor = QTextCursor(self.unicode_notes.document())
        cursor.setPosition(6)
        cursor.setPosition(11, QTextCursor.MoveMode.KeepAnchor)
        bold = QTextCharFormat()
        bold.setFontWeight(QFont.Weight.Bold)
        cursor.mergeCharFormat(bold)
        lay.addWidget(self.unicode_notes)

        self.edit_pad = QPlainTextEdit()
        self.edit_pad.setAccessibleName("Edit Pad")
        self.edit_pad.setMaximumHeight(60)
        lay.addWidget(self.edit_pad)

        self.span_table = QTableWidget(3, 3)
        self.span_table.setAccessibleName("Span Table")
        self.span_table.setHorizontalHeaderLabels(["Fruit", "Colour", "Qty"])
        rows = (
            ("Apple", "", "10"),
            ("Banana", "Yellow", "20"),
            ("Cherry", "Red", ""),
        )
        for row, values in enumerate(rows):
            for column, value in enumerate(values):
                if value:
                    self.span_table.setItem(row, column, QTableWidgetItem(value))
        # "Apple" covers Fruit and Colour; "20" covers rows 1 and 2 of Qty.
        self.span_table.setSpan(0, 0, 1, 2)
        self.span_table.setSpan(1, 2, 2, 1)
        self.span_table.setSelectionMode(
            QTableWidget.SelectionMode.SingleSelection
        )
        self.span_table.setCurrentCell(1, 1)
        self.span_table.setMinimumHeight(140)
        lay.addWidget(self.span_table)

        parent_layout.addWidget(grp)


# UTF-16 offsets: "😀" is 3..5, "wörld" 6..11, "𝄞" 12..14, the newline 14,
# "second line" 15..26; 26 units in all (24 code points).
UNICODE_NOTES_TEXT = "Hi \U0001F600 w\u00f6rld \U0001D11E\nsecond line"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid-file", help="Write PID to this file")
    parser.add_argument(
        "--open-fruit-popup",
        action="store_true",
        help="Open the Fruit combo box after launch for popup accessibility probes",
    )
    parser.add_argument(
        "--open-sample-dialog",
        action="store_true",
        help="Open an owned dialog after launch for window hierarchy probes",
    )
    parser.add_argument(
        "--open-file-menu",
        action="store_true",
        help="Open the File menu after launch for popup accessibility probes",
    )
    args = parser.parse_args()

    app = QApplication(sys.argv)
    app.setApplicationName("xa11y-qt-test-app")

    if args.pid_file:
        import os

        with open(args.pid_file, "w") as f:
            f.write(str(os.getpid()))

    window = TestWindow()
    window.show()
    if args.open_fruit_popup:
        QTimer.singleShot(500, window.combo.showPopup)
    if args.open_sample_dialog:
        QTimer.singleShot(500, window._open_sample_dialog)
    if args.open_file_menu:
        QTimer.singleShot(500, window._open_file_menu)
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
