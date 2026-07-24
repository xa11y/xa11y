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
            // Wrap into a second column instead of scrolling, so every widget
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

        // Select a single cell so a future config can assert per-cell
        // selection. Deferred to Load because CurrentCell needs the grid to be
        // laid out. NOTE: WinForms grid cells expose no SelectionItem pattern,
        // so xa11y reads selected=false for them today — see the
        // `table_selected_cell_name` comment in the Python APP_CONFIGS.
        _usersTable.CurrentCell = _usersTable.Rows[0].Cells[0];
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

        return MakeGroup("Input", new Size(300, 70), FlowDirection.TopDown, search);
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
