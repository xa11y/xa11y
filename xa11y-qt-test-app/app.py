"""PySide6 test application for xa11y integration tests.

Exposes every common Qt widget type so xa11y can exercise its
accessibility support on real platform APIs (AT-SPI on Linux,
AXUIElement on macOS, UIA on Windows).

Launch:  python app.py [--pid-file PATH]
The optional --pid-file writes the PID so the test harness can kill it.
"""

from __future__ import annotations

import argparse
import sys

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QApplication,
    QCheckBox,
    QComboBox,
    QDial,
    QDoubleSpinBox,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QMainWindow,
    QMenuBar,
    QProgressBar,
    QPushButton,
    QRadioButton,
    QScrollArea,
    QSlider,
    QSpinBox,
    QStatusBar,
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
        self.setMinimumSize(800, 600)

        self._build_menu_bar()
        self._build_toolbar()
        self._build_status_bar()

        # Central widget with tabs
        tabs = QTabWidget()
        tabs.setAccessibleName("Main Tabs")
        tabs.addTab(self._build_basic_tab(), "Basic")
        tabs.addTab(self._build_list_tab(), "Lists")
        tabs.addTab(self._build_text_tab(), "Text")
        self.setCentralWidget(tabs)

    # ── Menu bar ────────────────────────────────────────────────────

    def _build_menu_bar(self) -> None:
        mb: QMenuBar = self.menuBar()
        file_menu = mb.addMenu("&File")
        file_menu.addAction("&New")
        file_menu.addAction("&Open")
        file_menu.addAction("&Save")
        file_menu.addSeparator()
        file_menu.addAction("E&xit")

        edit_menu = mb.addMenu("&Edit")
        edit_menu.addAction("&Undo")
        edit_menu.addAction("&Redo")
        edit_menu.addSeparator()
        edit_menu.addAction("Cu&t")
        edit_menu.addAction("&Copy")
        edit_menu.addAction("&Paste")

        help_menu = mb.addMenu("&Help")
        help_menu.addAction("&About")

    # ── Toolbar ─────────────────────────────────────────────────────

    def _build_toolbar(self) -> None:
        tb: QToolBar = self.addToolBar("Main Toolbar")
        tb.setAccessibleName("Main Toolbar")
        self.new_btn = tb.addAction("New")
        self.open_btn = tb.addAction("Open")
        self.save_btn = tb.addAction("Save")

    # ── Status bar ──────────────────────────────────────────────────

    def _build_status_bar(self) -> None:
        sb: QStatusBar = self.statusBar()
        sb.showMessage("Ready")

    # ── Basic tab ───────────────────────────────────────────────────

    def _build_basic_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)

        # Buttons
        btn_group = QGroupBox("Buttons")
        btn_group.setAccessibleName("Buttons")
        btn_layout = QHBoxLayout(btn_group)

        self.ok_btn = QPushButton("OK")
        self.ok_btn.setAccessibleName("OK")
        self.ok_btn.setAccessibleDescription("Confirm the dialog")
        btn_layout.addWidget(self.ok_btn)

        self.cancel_btn = QPushButton("Cancel")
        self.cancel_btn.setAccessibleName("Cancel")
        self.cancel_btn.setEnabled(False)
        btn_layout.addWidget(self.cancel_btn)

        layout.addWidget(btn_group)

        # Checkboxes
        chk_group = QGroupBox("Checkboxes")
        chk_group.setAccessibleName("Checkboxes")
        chk_layout = QVBoxLayout(chk_group)

        self.agree_chk = QCheckBox("Agree to terms")
        self.agree_chk.setAccessibleName("Agree to terms")
        chk_layout.addWidget(self.agree_chk)

        self.subscribe_chk = QCheckBox("Subscribe")
        self.subscribe_chk.setAccessibleName("Subscribe")
        self.subscribe_chk.setChecked(True)
        chk_layout.addWidget(self.subscribe_chk)

        layout.addWidget(chk_group)

        # Radio buttons
        radio_group = QGroupBox("Options")
        radio_group.setAccessibleName("Options")
        radio_layout = QVBoxLayout(radio_group)

        self.radio_a = QRadioButton("Option A")
        self.radio_a.setAccessibleName("Option A")
        self.radio_a.setChecked(True)
        radio_layout.addWidget(self.radio_a)

        self.radio_b = QRadioButton("Option B")
        self.radio_b.setAccessibleName("Option B")
        radio_layout.addWidget(self.radio_b)

        self.radio_c = QRadioButton("Option C")
        self.radio_c.setAccessibleName("Option C")
        radio_layout.addWidget(self.radio_c)

        layout.addWidget(radio_group)

        # ComboBox
        combo_group = QGroupBox("ComboBox")
        combo_group.setAccessibleName("ComboBox")
        combo_layout = QVBoxLayout(combo_group)

        self.combo = QComboBox()
        self.combo.setAccessibleName("Fruit")
        self.combo.addItems(["Apple", "Banana", "Cherry", "Date", "Elderberry"])
        combo_layout.addWidget(self.combo)

        self.editable_combo = QComboBox()
        self.editable_combo.setAccessibleName("Color")
        self.editable_combo.setEditable(True)
        self.editable_combo.addItems(["Red", "Green", "Blue"])
        combo_layout.addWidget(self.editable_combo)

        layout.addWidget(combo_group)

        # Sliders / Spinners
        range_group = QGroupBox("Range Controls")
        range_group.setAccessibleName("Range Controls")
        range_layout = QVBoxLayout(range_group)

        self.slider = QSlider(Qt.Orientation.Horizontal)
        self.slider.setAccessibleName("Volume")
        self.slider.setRange(0, 100)
        self.slider.setValue(50)
        range_layout.addWidget(self.slider)

        self.spinner = QSpinBox()
        self.spinner.setAccessibleName("Quantity")
        self.spinner.setRange(0, 999)
        self.spinner.setValue(42)
        range_layout.addWidget(self.spinner)

        self.double_spinner = QDoubleSpinBox()
        self.double_spinner.setAccessibleName("Price")
        self.double_spinner.setRange(0.0, 9999.99)
        self.double_spinner.setValue(19.99)
        self.double_spinner.setDecimals(2)
        range_layout.addWidget(self.double_spinner)

        self.dial = QDial()
        self.dial.setAccessibleName("Dial")
        self.dial.setRange(0, 360)
        self.dial.setValue(180)
        range_layout.addWidget(self.dial)

        self.progress = QProgressBar()
        self.progress.setAccessibleName("Progress")
        self.progress.setRange(0, 100)
        self.progress.setValue(75)
        range_layout.addWidget(self.progress)

        layout.addWidget(range_group)

        # Line edit
        input_group = QGroupBox("Input")
        input_group.setAccessibleName("Input")
        input_layout = QVBoxLayout(input_group)

        self.line_edit = QLineEdit("hello world")
        self.line_edit.setAccessibleName("Search")
        self.line_edit.setPlaceholderText("Type here...")
        input_layout.addWidget(self.line_edit)

        layout.addWidget(input_group)

        # Wire up OK button to toggle Cancel enabled state
        self.ok_btn.clicked.connect(self._on_ok_clicked)
        self.agree_chk.toggled.connect(self._on_agree_toggled)

        layout.addStretch()
        return page

    def _on_ok_clicked(self) -> None:
        self.cancel_btn.setEnabled(not self.cancel_btn.isEnabled())
        self.statusBar().showMessage("OK clicked")

    def _on_agree_toggled(self, checked: bool) -> None:
        self.statusBar().showMessage(f"Agree: {checked}")

    # ── List tab ────────────────────────────────────────────────────

    def _build_list_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)

        # List widget
        list_group = QGroupBox("List")
        list_group.setAccessibleName("List")
        list_layout = QVBoxLayout(list_group)

        self.list_widget = QListWidget()
        self.list_widget.setAccessibleName("Items")
        for i in range(5):
            self.list_widget.addItem(f"Item {i + 1}")
        list_layout.addWidget(self.list_widget)

        layout.addWidget(list_group)

        # Tree widget
        tree_group = QGroupBox("Tree")
        tree_group.setAccessibleName("Tree")
        tree_layout = QVBoxLayout(tree_group)

        self.tree_widget = QTreeWidget()
        self.tree_widget.setAccessibleName("File Browser")
        self.tree_widget.setHeaderLabels(["Name", "Size"])
        root_item = QTreeWidgetItem(["Documents", ""])
        root_item.addChild(QTreeWidgetItem(["report.pdf", "2.1 MB"]))
        root_item.addChild(QTreeWidgetItem(["notes.txt", "12 KB"]))
        self.tree_widget.addTopLevelItem(root_item)
        photos_item = QTreeWidgetItem(["Photos", ""])
        photos_item.addChild(QTreeWidgetItem(["vacation.jpg", "4.5 MB"]))
        self.tree_widget.addTopLevelItem(photos_item)
        self.tree_widget.expandAll()
        tree_layout.addWidget(self.tree_widget)

        layout.addWidget(tree_group)
        layout.addStretch()
        return page

    # ── Text tab ────────────────────────────────────────────────────

    def _build_text_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)

        # Labels
        self.heading_label = QLabel("Heading Text")
        self.heading_label.setAccessibleName("Heading Text")
        layout.addWidget(self.heading_label)

        self.info_label = QLabel("This is informational text.")
        self.info_label.setAccessibleName("Info")
        layout.addWidget(self.info_label)

        # Multi-line text editor
        self.text_edit = QTextEdit()
        self.text_edit.setAccessibleName("Notes")
        self.text_edit.setPlainText("Line 1\nLine 2\nLine 3")
        layout.addWidget(self.text_edit)

        # Scroll area with content
        scroll = QScrollArea()
        scroll.setAccessibleName("Scroll Area")
        scroll_content = QWidget()
        scroll_layout = QVBoxLayout(scroll_content)
        for i in range(20):
            lbl = QLabel(f"Scroll item {i + 1}")
            scroll_layout.addWidget(lbl)
        scroll.setWidget(scroll_content)
        scroll.setWidgetResizable(True)
        layout.addWidget(scroll)

        return page


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid-file", help="Write PID to this file")
    args = parser.parse_args()

    app = QApplication(sys.argv)
    app.setApplicationName("xa11y-qt-test-app")

    if args.pid_file:
        import os

        with open(args.pid_file, "w") as f:
            f.write(str(os.getpid()))

    window = TestWindow()
    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
