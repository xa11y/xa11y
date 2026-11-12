// WinForms test application for xa11y integration tests.
//
// This is the first Microsoft UI framework in the integ matrix. Everything
// else on Windows (Qt, AccessKit, egui, Electron, Tauri) reaches UIA through a
// third-party bridge, so the WinForms-specific shapes the Windows provider
// handles were previously only unit-tested. What this app adds:
//
//   * A DataGridView, whose cells are `ControlType.DataItem` + the TableItem
//     pattern (DataGridViewCell.DataGridViewCellAccessibleObject), exercising
//     the row-vs-cell disambiguation in xa11y-windows/src/uia.rs against a
//     Microsoft framework rather than Qt.
//   * A "Legacy Tile" whose accessible object is an MSAA chart with a
//     default action and no UIA activation pattern, so `press` must reach
//     LegacyIAccessible.DoDefaultAction (issue #505).
//   * A ComboBox at the top of an overflowing AutoScroll panel ("Scroll
//     Panel"): its drop-down is a popup window that extends past the panel,
//     and must not be clipped to it.
//   * An AutoScroll panel of ten buttons ("Long Pane"). Controls scrolled
//     out of it keep `IsOffscreen = false`, and the provider clips them to
//     the panel's viewport (issue #502). A ListBox cannot show this: WinForms
//     and WPF both report scrolled-out list items offscreen themselves.
//   * An "Announce" button that raises a UIA Notification event
//     (AccessibleObject.RaiseAutomationNotification) with the text "Saved".
//     AccessKit's Windows adapter never raises one, so this is the fixture
//     for announcement text on Windows (issue #480).
//   * The WinForms UIA provider surface generally: control types come from
//     each AccessibleObject's Role via AccessibleRoleControlTypeMap, and the
//     top-level Form's type comes from the HWND host provider.
//
// Widget names mirror test-apps/qt/app.py so the shared Python/JS/CLI suites
// can run unmodified; see APP_CONFIGS["winforms"] in
// tests/suites/python/conftest.py for the per-widget contract (including the
// pieces WinForms genuinely cannot express).
//
// Launch:  xa11y-winforms-test-app.exe [--pid-file PATH]
// The optional --pid-file writes the PID so a test harness can kill it.

namespace Xa11y.TestApp.WinForms;

internal static class Program
{
    [STAThread]
    private static void Main(string[] args)
    {
        string? pidFile = null;
        for (int i = 0; i < args.Length - 1; i++)
        {
            if (args[i] == "--pid-file")
            {
                pidFile = args[i + 1];
            }
        }

        if (pidFile is not null)
        {
            File.WriteAllText(pidFile, Environment.ProcessId.ToString());
        }

        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.Run(new TestForm());
    }
}

/// <summary>
///  Main window holding every widget type the shared suites look for.
/// </summary>
internal sealed class TestForm : Form
{
    private const string WindowTitle = "xa11y-winforms-test-app";

    private readonly Button _cancelButton;
    private readonly ListBox _itemsList;
    private readonly Label _statusLabel;
    private readonly DataGridView _usersTable;

    public TestForm()
    {
        Text = WindowTitle;
        AccessibleName = WindowTitle;
        // WinForms reports a control's Name as its UIA AutomationId, so the
        // form and the "Agree to terms" check box have one. test_stable_id.py
        // checks that stable_id stays the window handle and the AutomationId
        // is reported in raw["automation_id"].
        Name = "TestForm";
        ClientSize = new Size(1100, 840);
        StartPosition = FormStartPosition.CenterScreen;

        _cancelButton = MakeButton("Cancel");
        _cancelButton.Enabled = false;

        _itemsList = MakeItemsList();
        _statusLabel = MakeLabel("Status: Ready");
        _usersTable = MakeUsersTable();

        var page = new FlowLayoutPanel
        {
            Dock = DockStyle.Fill,
            FlowDirection = FlowDirection.TopDown,
            // Wrap into further columns instead of scrolling, so every widget
            // stays on screen (an offscreen widget is still in the UIA tree,
            // but keeping it visible makes failures easier to reproduce by eye).
            WrapContents = true,
            AutoScroll = true,
            Padding = new Padding(8),
        };

        page.Controls.Add(BuildButtons());
        page.Controls.Add(BuildCheckBoxes());
        page.Controls.Add(BuildRadioButtons());
        page.Controls.Add(BuildRangeControls());
        page.Controls.Add(BuildInput());
        page.Controls.Add(BuildText());
        page.Controls.Add(BuildList());
        page.Controls.Add(BuildTable());
        page.Controls.Add(BuildDynamic());
        page.Controls.Add(BuildScrollPanel());
        page.Controls.Add(BuildLongPane());
        page.Controls.Add(BuildAnnouncements());
        // Last, after a group that ends in a button: WinForms names an
        // unnamed control after the Label just before it in tab order, so a
        // group placed after one ending in a Label ("Panel Filler", "Legacy
        // activations: N") lends its inner panel that label's name, and the
        // legacy test's unique-label lookup then finds two.
        page.Controls.Add(BuildLegacy());

        Controls.Add(page);
        Controls.Add(BuildToolStrip());
        Controls.Add(BuildStatusStrip());

        var menu = BuildMenuStrip();
        Controls.Add(menu);
        MainMenuStrip = menu;
    }

    protected override void OnLoad(EventArgs e)
    {
        base.OnLoad(e);

        // Select a single cell so the suites can assert per-cell selection.
        // Deferred to Load because CurrentCell needs the grid laid out.
        // WinForms grid cells expose no SelectionItem pattern — the selection
        // reaches UIA only as the MSAA STATE_SYSTEM_SELECTED bit on
        // LegacyIAccessible.State, which is the path xa11y-windows reads when
        // SelectionItem is absent.
        _usersTable.CurrentCell = _usersTable.Rows[0].Cells[0];
        _usersTable.Rows[0].Cells[0].Selected = true;
    }

    // ── Widget sections ──────────────────────────────────────────────────

    private GroupBox BuildButtons()
    {
        var ok = MakeButton("OK");
        // Set for parity with the other test apps. WinForms routes
        // AccessibleDescription to MSAA accDescription rather than to UIA's
        // HelpText/FullDescription, which is what xa11y reads as
        // `description`, so the shared suite does not assert it — see
        // `ok_button_description` in APP_CONFIGS.
        ok.AccessibleDescription = "Confirm the dialog";
        ok.Click += (_, _) =>
        {
            _cancelButton.Enabled = !_cancelButton.Enabled;
        };

        return MakeGroup("Buttons", new Size(300, 80), FlowDirection.LeftToRight, ok, _cancelButton);
    }

    private static GroupBox BuildCheckBoxes()
    {
        var agree = new CheckBox
        {
            Name = "agreeCheckBox",
            Text = "Agree to terms",
            AccessibleName = "Agree to terms",
            AutoSize = true,
        };
        var subscribe = new CheckBox
        {
            Text = "Subscribe",
            AccessibleName = "Subscribe",
            Checked = true,
            AutoSize = true,
        };

        return MakeGroup("Checkboxes", new Size(300, 90), FlowDirection.TopDown, agree, subscribe);
    }

    private static GroupBox BuildRadioButtons()
    {
        var a = MakeRadio("Option A");
        a.Checked = true;

        return MakeGroup(
            "Options",
            new Size(300, 120),
            FlowDirection.TopDown,
            a,
            MakeRadio("Option B"),
            MakeRadio("Option C"));
    }

    private static GroupBox BuildRangeControls()
    {
        var volume = new TrackBar
        {
            AccessibleName = "Volume",
            Minimum = 0,
            Maximum = 100,
            Value = 50,
            TickFrequency = 10,
            Width = 260,
        };
        var quantity = new NumericUpDown
        {
            AccessibleName = "Quantity",
            Minimum = 0,
            Maximum = 999,
            Value = 42,
            Width = 100,
        };
        var progress = new ProgressBar
        {
            AccessibleName = "Progress",
            Minimum = 0,
            Maximum = 100,
            Value = 75,
            Width = 260,
        };

        return MakeGroup(
            "Range Controls",
            new Size(300, 180),
            FlowDirection.TopDown,
            volume,
            quantity,
            progress);
    }

    private static GroupBox BuildInput()
    {
        var search = new TextBox
        {
            AccessibleName = "Search",
            Text = "hello world",
            Width = 260,
        };

        // Password entry (#522, #597): an `ES_PASSWORD` edit, which reports
        // UIA `IsPassword`. The Win32 edit proxy answers ValuePattern.Value
        // with a placeholder ("Access denied") rather than the text.
        var password = new TextBox
        {
            AccessibleName = "Password",
            Text = "hunter2",
            UseSystemPasswordChar = true,
            Width = 260,
        };

        return MakeGroup("Input", new Size(300, 100), FlowDirection.TopDown, search, password);
    }

    private static GroupBox BuildText()
    {
        var heading = MakeLabel("Heading Text");
        var notes = new TextBox
        {
            AccessibleName = "Notes",
            Multiline = true,
            Text = "Line 1\r\nLine 2\r\nLine 3",
            Size = new Size(260, 80),
        };

        return MakeGroup("Text", new Size(300, 150), FlowDirection.TopDown, heading, notes);
    }

    private GroupBox BuildList() =>
        MakeGroup("List", new Size(300, 150), FlowDirection.TopDown, _itemsList);

    private GroupBox BuildTable() =>
        MakeGroup("Table", new Size(460, 180), FlowDirection.TopDown, _usersTable);

    private GroupBox BuildDynamic()
    {
        var submit = MakeButton("Submit");
        submit.Click += (_, _) =>
        {
            var next = _statusLabel.Text == "Status: Submitted"
                ? "Status: Ready"
                : "Status: Submitted";
            _statusLabel.Text = next;
            _statusLabel.AccessibleName = next;
        };

        var addItem = MakeButton("Add Item");
        addItem.Click += (_, _) => _itemsList.Items.Add($"Item {_itemsList.Items.Count + 1}");

        var removeItem = MakeButton("Remove Item");
        removeItem.Click += (_, _) =>
        {
            if (_itemsList.Items.Count > 0)
            {
                _itemsList.Items.RemoveAt(_itemsList.Items.Count - 1);
            }
        };

        return MakeGroup(
            "Dynamic",
            new Size(300, 190),
            FlowDirection.TopDown,
            _statusLabel,
            submit,
            addItem,
            removeItem);
    }

    private static GroupBox BuildAnnouncements()
    {
        var announce = MakeButton("Announce");
        // Raised on the button's own accessible object, so the event's sender
        // is the button: an app-scoped subscription's per-window
        // notification handler receives it. ImportantMostRecent is pinned by
        // the suite as the event's `platform_priority` (and `assertive`).
        announce.Click += (_, _) =>
        {
            bool raised = announce.AccessibilityObject.RaiseAutomationNotification(
                System.Windows.Forms.Automation.AutomationNotificationKind.ActionCompleted,
                System.Windows.Forms.Automation.AutomationNotificationProcessing.ImportantMostRecent,
                "Saved");
            // False when the OS is too old for UIA notifications or no
            // client listens; the suite then times out waiting, which is the
            // failure it should report. Shown for a human reproducing it.
            announce.Text = raised ? "Announce" : "Announce (not raised)";
        };

        return MakeGroup("Announcements", new Size(300, 80), FlowDirection.LeftToRight, announce);
    }

    private static GroupBox BuildLegacy()
    {
        var status = MakeLabel("Legacy activations: 0");
        int count = 0;
        var tile = new LegacyTile(() =>
        {
            count++;
            var text = $"Legacy activations: {count}";
            status.Text = text;
            status.AccessibleName = text;
        })
        {
            AccessibleName = "Legacy Tile",
            Size = new Size(120, 40),
            BackColor = Color.LightSteelBlue,
        };

        return MakeGroup("Legacy", new Size(300, 110), FlowDirection.TopDown, tile, status);
    }

    // ── Chrome ───────────────────────────────────────────────────────────

    private static MenuStrip BuildMenuStrip()
    {
        var menu = new MenuStrip { AccessibleName = "Main Menu" };

        var file = new ToolStripMenuItem("&File");
        file.DropDownItems.Add(new ToolStripMenuItem("&New"));
        file.DropDownItems.Add(new ToolStripMenuItem("&Open"));
        file.DropDownItems.Add(new ToolStripMenuItem("&Save"));
        file.DropDownItems.Add(new ToolStripSeparator());
        file.DropDownItems.Add(new ToolStripMenuItem("E&xit"));

        var edit = new ToolStripMenuItem("&Edit");
        edit.DropDownItems.Add(new ToolStripMenuItem("&Undo"));
        edit.DropDownItems.Add(new ToolStripMenuItem("&Redo"));

        var help = new ToolStripMenuItem("&Help");
        help.DropDownItems.Add(new ToolStripMenuItem("&About"));

        menu.Items.AddRange([file, edit, help]);
        return menu;
    }

    private static ToolStrip BuildToolStrip()
    {
        var bar = new ToolStrip { AccessibleName = "Main Toolbar" };
        foreach (var label in new[] { "New", "Open", "Save" })
        {
            bar.Items.Add(new ToolStripButton(label)
            {
                AccessibleName = label,
                DisplayStyle = ToolStripItemDisplayStyle.Text,
            });
        }

        return bar;
    }

    private static StatusStrip BuildStatusStrip()
    {
        var status = new StatusStrip { AccessibleName = "Status Bar" };
        status.Items.Add(new ToolStripStatusLabel("Ready") { AccessibleName = "Ready" });
        return status;
    }

    // ── Widget factories ─────────────────────────────────────────────────

    private static Button MakeButton(string name) => new()
    {
        Text = name,
        AccessibleName = name,
        AutoSize = true,
    };

    private static RadioButton MakeRadio(string name) => new()
    {
        Text = name,
        AccessibleName = name,
        AutoSize = true,
    };

    private static Label MakeLabel(string text) => new()
    {
        Text = text,
        AccessibleName = text,
        AutoSize = true,
    };

    private static ListBox MakeItemsList()
    {
        var list = new ListBox
        {
            AccessibleName = "Items",
            Size = new Size(260, 100),
        };
        for (int i = 1; i <= 5; i++)
        {
            list.Items.Add($"Item {i}");
        }

        return list;
    }

    /// <summary>
    ///  Ten buttons in an AutoScroll panel that shows two, scrolled to the
    ///  top, so "Long Entry 10" is out of view while UIA still reports it on
    ///  screen (issue #502). Kept short: every button here also lands in the
    ///  suites' `button` selectors, and the MCP screenshot legend is capped.
    ///  Focusing a button scrolls the panel to it, which is how the suites
    ///  move the viewport: plain buttons have no UIA ScrollItem pattern for
    ///  `scroll_into_view`. TabStop is off so keyboard navigation tests never
    ///  wander in. Named apart from "Item N" so no other selector matches
    ///  them.
    /// </summary>
    private static GroupBox BuildLongPane()
    {
        var pane = new FlowLayoutPanel
        {
            AccessibleName = "Long Pane",
            AutoScroll = true,
            FlowDirection = FlowDirection.TopDown,
            WrapContents = false,
            Size = new Size(260, 60),
        };
        for (int i = 1; i <= 10; i++)
        {
            pane.Controls.Add(new Button
            {
                Text = $"Long Entry {i}",
                AccessibleName = $"Long Entry {i}",
                TabStop = false,
                AutoSize = true,
            });
        }

        return MakeGroup("Scrolled", new Size(300, 100), FlowDirection.TopDown, pane);
    }

    /// <summary>
    ///  A ComboBox in a panel whose content overflows it (issue #502). The
    ///  drop-down opens below the panel's bottom edge, as a popup window that
    ///  UIA still lists under the combo, so its entries are visible although
    ///  they lie outside the scrolling panel's bounds.
    /// </summary>
    private static GroupBox BuildScrollPanel()
    {
        var combo = new ComboBox
        {
            AccessibleName = "Panel Combo",
            DropDownStyle = ComboBoxStyle.DropDownList,
            Location = new Point(0, 0),
            Width = 200,
        };
        combo.Items.AddRange(["Panel Choice 1", "Panel Choice 2", "Panel Choice 3"]);
        combo.SelectedIndex = 0;

        // Pushes the content past the panel's 40 px so it overflows.
        var filler = new Label
        {
            Text = "Panel Filler",
            AccessibleName = "Panel Filler",
            Location = new Point(0, 150),
            AutoSize = true,
        };

        var panel = new Panel
        {
            AccessibleName = "Scroll Panel",
            AutoScroll = true,
            Size = new Size(260, 40),
        };
        panel.Controls.Add(combo);
        panel.Controls.Add(filler);

        return MakeGroup("Overflow", new Size(300, 80), FlowDirection.TopDown, panel);
    }

    /// <summary>
    ///  The canonical WinForms grid. Cells report ControlType.DataItem plus the
    ///  TableItem pattern, which is what xa11y maps to <c>table_cell</c>; the
    ///  grid itself reports ControlType.DataGrid, which maps to <c>table</c>.
    /// </summary>
    private static DataGridView MakeUsersTable()
    {
        var grid = new DataGridView
        {
            AccessibleName = "Users Table",
            AllowUserToAddRows = false,
            AllowUserToDeleteRows = false,
            AllowUserToResizeRows = false,
            ColumnHeadersVisible = true,
            // Row headers would add a second header cell per row with no name
            // worth asserting; keep the cell count equal to the data cells.
            RowHeadersVisible = false,
            ReadOnly = true,
            MultiSelect = false,
            SelectionMode = DataGridViewSelectionMode.CellSelect,
            Size = new Size(420, 130),
        };

        grid.Columns.Add("name", "Name");
        grid.Columns.Add("role", "Role");
        grid.Rows.Add("Alice", "Admin");
        grid.Rows.Add("Bob", "User");
        return grid;
    }

    private static GroupBox MakeGroup(
        string name,
        Size size,
        FlowDirection direction,
        params Control[] children)
    {
        var flow = new FlowLayoutPanel
        {
            Dock = DockStyle.Fill,
            FlowDirection = direction,
            WrapContents = false,
        };
        flow.Controls.AddRange(children);

        var box = new GroupBox
        {
            Text = name,
            AccessibleName = name,
            Size = size,
            Padding = new Padding(6, 4, 6, 6),
        };
        box.Controls.Add(flow);
        return box;
    }
}

/// <summary>
///  A control reachable only through its MSAA default action.
///  <para>
///   Its accessible object reports <see cref="AccessibleRole.Chart"/>. The
///   MSAA proxy decides Invoke from the role and the default action together:
///   a graphic with a default action is given Invoke, but a chart is on the
///   proxy's never-invokable list, and a chart also implies no Toggle,
///   SelectionItem, or ExpandCollapse. So it has a default action and no UIA
///   activation pattern, and the only way to activate it is
///   <c>LegacyIAccessible.DoDefaultAction</c>. That is the shape issue #505
///   is about, and the provider's <c>press</c> must dispatch to it. The integ
///   test asserts the pattern set, so a role that picks up a pattern fails
///   rather than passing through Invoke.
///  </para>
/// </summary>
internal sealed class LegacyTile : Control
{
    private readonly Action _activate;

    public LegacyTile(Action activate) => _activate = activate;

    protected override AccessibleObject CreateAccessibilityInstance() =>
        new LegacyTileAccessibleObject(this);

    private sealed class LegacyTileAccessibleObject : ControlAccessibleObject
    {
        private readonly LegacyTile _owner;

        public LegacyTileAccessibleObject(LegacyTile owner)
            : base(owner) => _owner = owner;

        public override AccessibleRole Role => AccessibleRole.Chart;

        public override string DefaultAction => "Activate";

        public override void DoDefaultAction() => _owner._activate();
    }
}
