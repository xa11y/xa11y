/// xa11y Cocoa/AppKit test app.
///
/// A single-file AppKit application exposing the standard widget set for
/// xa11y integration tests. Compiled with:
///
///     swiftc -o xa11y-cocoa-test-app -framework Cocoa main.swift
///
/// Run with:
///
///     ./xa11y-cocoa-test-app [--headless] [--pid-file PATH]

import Cocoa
import Foundation

// ── App entry point ───────────────────────────────────────────────────────────

let args = CommandLine.arguments
let headless = args.contains("--headless")
let pidFileIndex = args.firstIndex(of: "--pid-file").map { $0 + 1 }
let pidFile = pidFileIndex.flatMap { $0 < args.count ? args[$0] : nil }

if let path = pidFile {
    try! String(ProcessInfo.processInfo.processIdentifier).write(
        toFile: path, atomically: true, encoding: .utf8
    )
}

// ── Window layout ─────────────────────────────────────────────────────────────

class AppDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!

    func applicationDidFinishLaunching(_: Notification) {
        let contentRect = NSRect(x: 0, y: 0, width: 700, height: 800)
        window = NSWindow(
            contentRect: contentRect,
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "xa11y-cocoa-test-app"
        window.setFrameAutosaveName("TestWindow")

        if headless {
            NSApp.setActivationPolicy(.accessory)
        }

        let scroll = NSScrollView(frame: contentRect)
        scroll.hasVerticalScroller = true
        scroll.autoresizingMask = [.width, .height]

        let docView = NSView(frame: NSRect(x: 0, y: 0, width: 680, height: 1320))
        scroll.documentView = docView
        window.contentView = scroll

        var y: CGFloat = 1160
        func addSection(_ title: String) -> NSView {
            let box = NSBox(frame: NSRect(x: 12, y: y - 180, width: 656, height: 180))
            box.title = title
            box.setAccessibilityLabel(title)
            docView.addSubview(box)
            y -= 190
            return box
        }

        // ── Buttons ──────────────────────────────────────────────────────
        let btnBox = NSBox(frame: NSRect(x: 12, y: y - 100, width: 656, height: 100))
        btnBox.title = "Buttons"
        docView.addSubview(btnBox)
        y -= 110

        let okButton = NSButton(frame: NSRect(x: 16, y: 20, width: 120, height: 32))
        okButton.bezelStyle = .rounded
        okButton.title = "OK"
        okButton.setAccessibilityLabel("OK")
        okButton.setAccessibilityHelp("Confirm the dialog")
        okButton.action = #selector(AppDelegate.onOKPressed)
        okButton.target = self
        btnBox.addSubview(okButton)

        cancelButton = NSButton(frame: NSRect(x: 148, y: 20, width: 120, height: 32))
        cancelButton.bezelStyle = .rounded
        cancelButton.title = "Cancel"
        cancelButton.setAccessibilityLabel("Cancel")
        cancelButton.isEnabled = false
        btnBox.addSubview(cancelButton)

        // ── Checkboxes ───────────────────────────────────────────────────
        let chkBox = NSBox(frame: NSRect(x: 12, y: y - 100, width: 656, height: 100))
        chkBox.title = "Checkboxes"
        docView.addSubview(chkBox)
        y -= 110

        let agreeCheck = NSButton(frame: NSRect(x: 16, y: 50, width: 200, height: 24))
        agreeCheck.setButtonType(.switch)
        agreeCheck.title = "Agree to terms"
        agreeCheck.setAccessibilityLabel("Agree to terms")
        agreeCheck.state = .off
        chkBox.addSubview(agreeCheck)

        let subscribeCheck = NSButton(frame: NSRect(x: 16, y: 20, width: 200, height: 24))
        subscribeCheck.setButtonType(.switch)
        subscribeCheck.title = "Subscribe"
        subscribeCheck.setAccessibilityLabel("Subscribe")
        subscribeCheck.state = .on
        chkBox.addSubview(subscribeCheck)

        // ── Radio buttons ────────────────────────────────────────────────
        let radioBox = NSBox(frame: NSRect(x: 12, y: y - 110, width: 656, height: 110))
        radioBox.title = "Options"
        docView.addSubview(radioBox)
        y -= 120

        radioA = NSButton(frame: NSRect(x: 16, y: 70, width: 150, height: 24))
        radioA.setButtonType(.radio)
        radioA.title = "Option A"
        radioA.setAccessibilityLabel("Option A")
        radioA.state = .on
        radioBox.addSubview(radioA)

        let radioB = NSButton(frame: NSRect(x: 16, y: 44, width: 150, height: 24))
        radioB.setButtonType(.radio)
        radioB.title = "Option B"
        radioB.setAccessibilityLabel("Option B")
        radioB.state = .off
        radioBox.addSubview(radioB)

        let radioC = NSButton(frame: NSRect(x: 16, y: 18, width: 150, height: 24))
        radioC.setButtonType(.radio)
        radioC.title = "Option C"
        radioC.setAccessibilityLabel("Option C")
        radioC.state = .off
        radioBox.addSubview(radioC)

        // ── ComboBox ──────────────────────────────────────────────────────
        let comboBox2 = NSBox(frame: NSRect(x: 12, y: y - 90, width: 656, height: 90))
        comboBox2.title = "ComboBox"
        docView.addSubview(comboBox2)
        y -= 100

        let combo = NSComboBox(frame: NSRect(x: 16, y: 30, width: 200, height: 26))
        combo.setAccessibilityLabel("Fruit")
        combo.addItems(withObjectValues: ["Apple", "Banana", "Cherry", "Date", "Elderberry"])
        combo.selectItem(at: 0)
        comboBox2.addSubview(combo)

        // ── Range controls ────────────────────────────────────────────────
        let rangeBox = NSBox(frame: NSRect(x: 12, y: y - 150, width: 656, height: 150))
        rangeBox.title = "Range Controls"
        docView.addSubview(rangeBox)
        y -= 160

        slider = NSSlider(frame: NSRect(x: 16, y: 105, width: 300, height: 24))
        slider.minValue = 0
        slider.maxValue = 100
        slider.doubleValue = 50
        slider.setAccessibilityLabel("Volume")
        rangeBox.addSubview(slider)

        let spinCell = NSStepperCell()
        spinCell.minValue = 0
        spinCell.maxValue = 999
        spinCell.doubleValue = 42
        let spin = NSStepper(frame: NSRect(x: 16, y: 70, width: 80, height: 26))
        spin.minValue = 0
        spin.maxValue = 999
        spin.doubleValue = 42
        spin.increment = 1
        spin.setAccessibilityLabel("Quantity")
        rangeBox.addSubview(spin)

        let spinLabel = NSTextField(frame: NSRect(x: 100, y: 70, width: 80, height: 26))
        spinLabel.stringValue = "42"
        spinLabel.isEditable = false
        spinLabel.isBordered = false
        spinLabel.backgroundColor = .clear
        rangeBox.addSubview(spinLabel)

        let progress = NSProgressIndicator(frame: NSRect(x: 16, y: 30, width: 300, height: 20))
        progress.style = .bar
        progress.isIndeterminate = false
        progress.minValue = 0
        progress.maxValue = 100
        progress.doubleValue = 75
        progress.setAccessibilityLabel("Progress")
        rangeBox.addSubview(progress)

        // ── Text field ────────────────────────────────────────────────────
        let inputBox = NSBox(frame: NSRect(x: 12, y: y - 90, width: 656, height: 90))
        inputBox.title = "Input"
        docView.addSubview(inputBox)
        y -= 100

        let textField = NSTextField(frame: NSRect(x: 16, y: 30, width: 300, height: 26))
        textField.stringValue = "hello world"
        textField.placeholderString = "Type here..."
        textField.setAccessibilityLabel("Search")
        inputBox.addSubview(textField)

        // ── Text area ─────────────────────────────────────────────────────
        let textBox = NSBox(frame: NSRect(x: 12, y: y - 120, width: 656, height: 120))
        textBox.title = "Text"
        docView.addSubview(textBox)
        y -= 130

        let heading = NSTextField(frame: NSRect(x: 16, y: 78, width: 300, height: 24))
        heading.stringValue = "Heading Text"
        heading.isEditable = false
        heading.isBordered = false
        heading.backgroundColor = .clear
        heading.setAccessibilityLabel("Heading Text")
        textBox.addSubview(heading)

        let scrolledText = NSScrollView(frame: NSRect(x: 16, y: 10, width: 400, height: 60))
        let textView = NSTextView(frame: scrolledText.bounds)
        textView.string = "Line 1\nLine 2\nLine 3"
        textView.setAccessibilityLabel("Notes")
        scrolledText.documentView = textView
        textBox.addSubview(scrolledText)

        // ── List ──────────────────────────────────────────────────────────
        let listBox = NSBox(frame: NSRect(x: 12, y: y - 150, width: 656, height: 150))
        listBox.title = "List"
        docView.addSubview(listBox)
        y -= 160

        let tableScroll = NSScrollView(frame: NSRect(x: 16, y: 10, width: 400, height: 120))
        listTable = NSTableView(frame: tableScroll.bounds)
        listTable.setAccessibilityLabel("Items")
        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("item"))
        col.title = "Item"
        listTable.addTableColumn(col)
        listTable.dataSource = self
        tableScroll.documentView = listTable
        listBox.addSubview(tableScroll)

        // ── Grid (NSGridView → AXGrid → table role) ──────────────────────
        let gridBox = NSBox(frame: NSRect(x: 12, y: y - 120, width: 656, height: 120))
        gridBox.title = "Grid"
        docView.addSubview(gridBox)
        y -= 130

        let nameLabel = NSTextField(frame: .zero)
        nameLabel.stringValue = "Name"
        nameLabel.isEditable = false
        nameLabel.isBordered = false
        nameLabel.backgroundColor = .clear

        let valueLabel = NSTextField(frame: .zero)
        valueLabel.stringValue = "Value"
        valueLabel.isEditable = false
        valueLabel.isBordered = false
        valueLabel.backgroundColor = .clear

        let gridView = NSGridView(views: [[nameLabel, valueLabel]])
        gridView.setAccessibilityLabel("Settings Grid")
        gridView.frame = NSRect(x: 16, y: 20, width: 300, height: 60)
        gridBox.addSubview(gridView)

        // ── Dynamic (events) ───────────────────────────────────────────
        // Widgets that mutate on action so event tests can exercise
        // NameChanged (Submit → status label) and StructureChanged
        // (Add/Remove Item → list rows).
        let dynBox = NSBox(frame: NSRect(x: 12, y: y - 120, width: 656, height: 120))
        dynBox.title = "Dynamic"
        docView.addSubview(dynBox)
        y -= 130

        statusLabel = NSTextField(frame: NSRect(x: 16, y: 78, width: 300, height: 24))
        statusLabel.stringValue = "Status: Ready"
        statusLabel.isEditable = false
        statusLabel.isBordered = false
        statusLabel.backgroundColor = .clear
        statusLabel.setAccessibilityLabel("Status: Ready")
        dynBox.addSubview(statusLabel)

        let submitButton = NSButton(frame: NSRect(x: 16, y: 44, width: 120, height: 24))
        submitButton.bezelStyle = .rounded
        submitButton.title = "Submit"
        submitButton.setAccessibilityLabel("Submit")
        submitButton.target = self
        submitButton.action = #selector(AppDelegate.onSubmitPressed)
        dynBox.addSubview(submitButton)

        let addItemButton = NSButton(frame: NSRect(x: 148, y: 44, width: 120, height: 24))
        addItemButton.bezelStyle = .rounded
        addItemButton.title = "Add Item"
        addItemButton.setAccessibilityLabel("Add Item")
        addItemButton.target = self
        addItemButton.action = #selector(AppDelegate.onAddItemPressed)
        dynBox.addSubview(addItemButton)

        let removeItemButton = NSButton(frame: NSRect(x: 280, y: 44, width: 120, height: 24))
        removeItemButton.bezelStyle = .rounded
        removeItemButton.title = "Remove Item"
        removeItemButton.setAccessibilityLabel("Remove Item")
        removeItemButton.target = self
        removeItemButton.action = #selector(AppDelegate.onRemoveItemPressed)
        dynBox.addSubview(removeItemButton)

        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }

    @objc func onOKPressed() {
        cancelButton.isEnabled = true
    }

    @objc func onSubmitPressed() {
        // Alternate so every press produces a distinct AXTitleChanged event.
        let current = statusLabel.stringValue
        let next = current == "Status: Submitted" ? "Status: Ready" : "Status: Submitted"
        statusLabel.stringValue = next
        // Update the accessibility label too so NameChanged fires.
        statusLabel.setAccessibilityLabel(next)
    }

    @objc func onAddItemPressed() {
        rowCount += 1
        listTable.reloadData()
    }

    @objc func onRemoveItemPressed() {
        if rowCount > 0 {
            rowCount -= 1
            listTable.reloadData()
        }
    }

    // References to controls that need state changes
    var cancelButton: NSButton!
    var radioA: NSButton!
    var slider: NSSlider!
    var listTable: NSTableView!
    var statusLabel: NSTextField!
    var rowCount: Int = 5
}

// ── NSTableViewDataSource ─────────────────────────────────────────────────────

extension AppDelegate: NSTableViewDataSource {
    func numberOfRows(in _: NSTableView) -> Int { rowCount }

    func tableView(_ tableView: NSTableView, objectValueFor column: NSTableColumn?, row: Int) -> Any? {
        "Item \(row + 1)"
    }
}

// ── Run ───────────────────────────────────────────────────────────────────────

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
