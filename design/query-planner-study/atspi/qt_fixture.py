"""Run a wide Qt form or a tree that crosses xa11y's depth boundary."""
import argparse

from PySide6.QtWidgets import QApplication, QGridLayout, QGroupBox, QPushButton, QVBoxLayout, QWidget


def child_group(parent, name):
    group = QGroupBox(name)
    parent.addWidget(group)
    return QVBoxLayout(group)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deep", action="store_true")
    args = parser.parse_args()
    app = QApplication([])
    window = QWidget()
    if args.deep:
        app.setApplicationName("xa11y-deep-probe")
        layout = QVBoxLayout(window)
        layout.addWidget(QPushButton("Shallow"))
        for i in range(58):
            layout = child_group(layout, f"Level {i}")
        layout.addWidget(QPushButton("Deep"))
    else:
        app.setApplicationName("xa11y-512-button-probe")
        grid = QGridLayout(window)
        for i in range(32):
            group = QGroupBox(f"Group {i}")
            column = QVBoxLayout(group)
            for j in range(16):
                column.addWidget(QPushButton(f"Button {16 * i + j}"))
            grid.addWidget(group, i // 8, i % 8)
    window.show()
    app.exec()


if __name__ == "__main__":
    main()
