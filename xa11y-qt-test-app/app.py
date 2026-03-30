"""PySide6 test application for xa11y integration tests.

Exposes every common Qt widget type on a single scrollable page so that
all widgets are visible to accessibility APIs regardless of tab state.

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
        self._add_tree(layout)

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
        parent_layout.addWidget(grp)

    def _on_ok_clicked(self) -> None:
        self.cancel_btn.setEnabled(not self.cancel_btn.isEnabled())
        self.statusBar().showMessage("OK clicked")

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
