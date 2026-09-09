// WPF test application for xa11y integration tests.
//
// The second Microsoft UI framework in the integ matrix, and the one that
// closes the ControlType.Custom cell gap. `test-apps/winforms` covers the
// DataItem + TableItem cell shape; WPF's DataGrid is the other canonical grid
// and reports its cells differently:
//
//   * DataGridCellItemAutomationPeer.GetAutomationControlTypeCore returns
//     AutomationControlType.Custom, and the peer implements ITableItemProvider,
//     IGridItemProvider, IValueProvider and ISelectionItemProvider. That
//     Custom + TableItem shape is the branch of `map_uia_role` added in #323
//     (xa11y-windows/src/uia.rs), which had no app in the matrix producing one.
//   * DataGridItemAutomationPeer (the rows) reports DataItem, and
//     DataGridAutomationPeer reports DataGrid, so the same tree also exercises
//     the row-vs-cell disambiguation from a second framework.
//
// Two more things WPF expresses that WinForms cannot:
//
//   * Slider and ProgressBar peers implement IRangeValueProvider, so the
//     numeric value and range assertions run here rather than skipping.
//   * AutomationProperties.IsDialog publishes UIA_IsDialogPropertyId, so the
//     native-dialog role test runs (WinForms sets that property nowhere).
//
// Widget names mirror test-apps/qt/app.py so the shared Python/JS/CLI suites
// can run unmodified; see APP_CONFIGS["wpf"] in
// tests/suites/python/conftest.py for the per-widget contract.
//
// Built code-only (no XAML) to keep the app a single reviewable file, the same
// shape as test-apps/winforms/Program.cs.
//
// Launch:  xa11y-wpf-test-app.exe [--pid-file PATH]
// The optional --pid-file writes the PID so a test harness can kill it.

using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Threading;

namespace Xa11y.TestApp.Wpf;

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
            System.IO.File.WriteAllText(pidFile, Environment.ProcessId.ToString());
        }

        var app = new Application { ShutdownMode = ShutdownMode.OnMainWindowClose };
        app.Run(new TestWindow());
    }
}

/// <summary>
///  A row in the DataGrid. <see cref="ToString"/> is what
///  DataGridCellItemAutomationPeer formats its synthesized cell *name* from
///  (the item plus the column's display index), so returning the user's name
///  keeps tree dumps readable. The suites assert cell text through the
///  ValuePattern value, not that name — see `table_cell_values` in APP_CONFIGS.
/// </summary>
internal sealed class User
{
    public User(string name, string role)
    {
        Name = name;
        Role = role;
    }

    public string Name { get; }

    public string Role { get; }

    public override string ToString() => Name;
}

/// <summary>
///  Main window holding every widget type the shared suites look for.
/// </summary>
internal sealed class TestWindow : Window
{
    private const string WindowTitle = "xa11y-wpf-test-app";

    private readonly Button _cancelButton;
    private readonly ObservableCollection<string> _items = new();
    private readonly ObservableCollection<User> _users = new()
    {
        new User("Alice", "Admin"),
        new User("Bob", "User"),
    };

    private readonly TextBlock _statusLabel;
    private readonly DataGrid _usersTable;
    private Window? _sampleDialog;
    private Window? _sampleSibling;

    public TestWindow()
    {
        Title = WindowTitle;
        Width = 1120;
        Height = 880;
        WindowStartupLocation = WindowStartupLocation.CenterScreen;
        AutomationProperties.SetName(this, WindowTitle);

        for (int i = 1; i <= 5; i++)
        {
            _items.Add($"Item {i}");
        }

        _cancelButton = MakeButton("Cancel");
        _cancelButton.IsEnabled = false;

        _statusLabel = MakeText("Status: Ready");
        _usersTable = MakeUsersTable();

        // Wrap into a second column instead of scrolling, so every widget stays
        // on screen (an offscreen widget is still in the UIA tree, but keeping
        // it visible makes failures easier to reproduce by eye).
        var page = new WrapPanel { Orientation = Orientation.Vertical, Margin = new Thickness(8) };
        page.Children.Add(BuildButtons());
        page.Children.Add(BuildCheckBoxes());
        page.Children.Add(BuildRadioButtons());
        page.Children.Add(BuildRangeControls());
        page.Children.Add(BuildInput());
        page.Children.Add(BuildText());
        page.Children.Add(BuildList());
        page.Children.Add(BuildTable());
        page.Children.Add(BuildDynamic());
        page.Children.Add(BuildDialogs());

        var root = new DockPanel();
        root.Children.Add(Dock(BuildMenu(), System.Windows.Controls.Dock.Top));
        root.Children.Add(Dock(BuildToolBar(), System.Windows.Controls.Dock.Top));
        root.Children.Add(Dock(BuildStatusBar(), System.Windows.Controls.Dock.Bottom));
        root.Children.Add(new ScrollViewer
        {
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            Content = page,
        });

        Content = root;

        // Select a single cell so the suites can assert per-cell selection.
        // Deferred to Loaded priority because DataGrid.SelectedCells needs the
        // row containers generated. DataGridCellItemAutomationPeer implements
        // ISelectionItemProvider, so this reaches xa11y as UIA
        // SelectionItem.IsSelected — the pattern WinForms grid cells lack.
        Loaded += (_, _) => Dispatcher.BeginInvoke(
            DispatcherPriority.Loaded,
            new Action(SelectFirstCell));
    }

    private void SelectFirstCell()
    {
        var cell = new DataGridCellInfo(_users[0], _usersTable.Columns[0]);
        _usersTable.CurrentCell = cell;
        _usersTable.SelectedCells.Clear();
        _usersTable.SelectedCells.Add(cell);
    }

    // ── Widget sections ──────────────────────────────────────────────────

    private GroupBox BuildButtons()
    {
        var ok = MakeButton("OK");
        // WPF routes AutomationProperties.HelpText to UIA's HelpText property,
        // one of the two properties xa11y reads as `description` — so unlike
        // the WinForms app, the description is asserted here.
        AutomationProperties.SetHelpText(ok, "Confirm the dialog");
        ok.Click += (_, _) => _cancelButton.IsEnabled = !_cancelButton.IsEnabled;

        return MakeGroup("Buttons", Orientation.Horizontal, ok, _cancelButton);
    }

    private static GroupBox BuildCheckBoxes()
    {
        var agree = MakeCheckBox("Agree to terms", isChecked: false);
        var subscribe = MakeCheckBox("Subscribe", isChecked: true);

        return MakeGroup("Checkboxes", Orientation.Vertical, agree, subscribe);
    }

    private static GroupBox BuildRadioButtons()
    {
        var a = MakeRadio("Option A");
        a.IsChecked = true;

        return MakeGroup(
            "Options",
            Orientation.Vertical,
            a,
            MakeRadio("Option B"),
            MakeRadio("Option C"));
    }

    private static GroupBox BuildRangeControls()
    {
        // SliderAutomationPeer and ProgressBarAutomationPeer both implement
        // IRangeValueProvider, so numeric_value / min_value / max_value are all
        // readable here — the WinForms TrackBar and NumericUpDown are not.
        // WPF ships no spin-button control, so there is no `spin_button` group.
        var volume = new Slider
        {
            Minimum = 0,
            Maximum = 100,
            Value = 50,
            SmallChange = 1,
            LargeChange = 10,
            TickFrequency = 10,
            Width = 260,
        };
        AutomationProperties.SetName(volume, "Volume");

        var progress = new ProgressBar
        {
            Minimum = 0,
            Maximum = 100,
            Value = 75,
            Width = 260,
            Height = 20,
        };
        AutomationProperties.SetName(progress, "Progress");

        return MakeGroup("Range Controls", Orientation.Vertical, volume, progress);
    }

    private static GroupBox BuildInput()
    {
        var search = new TextBox { Text = "hello world", Width = 260 };
        AutomationProperties.SetName(search, "Search");

        return MakeGroup("Input", Orientation.Vertical, search);
    }

    private static GroupBox BuildText()
    {
        var heading = MakeText("Heading Text");
        var notes = new TextBox
        {
            Text = "Line 1\nLine 2\nLine 3",
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            Width = 260,
            Height = 80,
        };
        AutomationProperties.SetName(notes, "Notes");

        return MakeGroup("Text", Orientation.Vertical, heading, notes);
    }

    private GroupBox BuildList()
    {
        var list = new ListBox { ItemsSource = _items, Width = 260, Height = 100 };
        AutomationProperties.SetName(list, "Items");

        return MakeGroup("List", Orientation.Vertical, list);
    }

    private GroupBox BuildTable() => MakeGroup("Table", Orientation.Vertical, _usersTable);

    private GroupBox BuildDynamic()
    {
        var submit = MakeButton("Submit");
        submit.Click += (_, _) =>
        {
            var next = _statusLabel.Text == "Status: Submitted"
                ? "Status: Ready"
                : "Status: Submitted";
            _statusLabel.Text = next;
            AutomationProperties.SetName(_statusLabel, next);
        };

        var addItem = MakeButton("Add Item");
        addItem.Click += (_, _) => _items.Add($"Item {_items.Count + 1}");

        var removeItem = MakeButton("Remove Item");
        removeItem.Click += (_, _) =>
        {
            if (_items.Count > 0)
            {
                _items.RemoveAt(_items.Count - 1);
            }
        };

        return MakeGroup(
            "Dynamic",
            Orientation.Vertical,
            _statusLabel,
            submit,
            addItem,
            removeItem);
    }

    private GroupBox BuildDialogs()
    {
        var open = MakeButton("Open Dialog");
        open.Click += (_, _) => OpenSampleDialog();
        var openSibling = MakeButton("Open Sibling");
        openSibling.Click += (_, _) => OpenSampleSibling();

        return MakeGroup("Dialogs", Orientation.Vertical, open, openSibling);
    }

    /// <summary>
    ///  Shows a non-modal dialog window, mirroring the Qt app's QDialog.show().
    ///  AutomationProperties.IsDialog is what publishes UIA_IsDialogPropertyId,
    ///  the signal xa11y-windows keys the `dialog` role off for native
    ///  (non-ARIA) dialogs.
    /// </summary>
    private void OpenSampleDialog()
    {
        if (_sampleDialog is null)
        {
            var close = MakeButton("Close Dialog");
            var dialog = new Window
            {
                Title = "Sample Dialog",
                Owner = this,
                Width = 320,
                Height = 160,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                ShowInTaskbar = false,
                Content = new StackPanel { Margin = new Thickness(16), Children = { close } },
            };
            AutomationProperties.SetName(dialog, "Sample Dialog");
            AutomationProperties.SetIsDialog(dialog, true);

            close.Click += (_, _) => dialog.Hide();
            // Hide rather than destroy: a closed WPF Window cannot be shown
            // again, and the suites may open this dialog more than once a run.
            dialog.Closing += (_, e) =>
            {
                e.Cancel = true;
                dialog.Hide();
            };

            _sampleDialog = dialog;
        }

        _sampleDialog.Show();
        _sampleDialog.Activate();
    }

    /// <summary>
    ///  Shows a second non-modal, non-dialog top-level window of this process.
    ///  Event-subscription coverage needs a same-pid *sibling* top-level
    ///  window: xa11y-windows registers UIA handlers per top-level window of
    ///  the pid (and attaches new ones as windows come and go), so a sibling's
    ///  minimize must raise the same PropertyChanged(WindowVisualState) the
    ///  main window does. It is a plain Window — no IsDialog — so it maps to
    ///  Role::Window and is listed by App::windows() alongside the main window.
    ///  Hide rather than destroy, mirroring OpenSampleDialog, so the suites can
    ///  open it more than once a run.
    /// </summary>
    private void OpenSampleSibling()
    {
        if (_sampleSibling is null)
        {
            var close = MakeButton("Close Sibling");
            var sibling = new Window
            {
                Title = "Sample Sibling",
                Width = 320,
                Height = 160,
                WindowStartupLocation = WindowStartupLocation.CenterScreen,
                ShowInTaskbar = false,
                Content = new StackPanel { Margin = new Thickness(16), Children = { close } },
            };
            AutomationProperties.SetName(sibling, "Sample Sibling");

            close.Click += (_, _) => sibling.Hide();
            sibling.Closing += (_, e) =>
            {
                e.Cancel = true;
                sibling.Hide();
            };

            _sampleSibling = sibling;
        }

        _sampleSibling.Show();
        _sampleSibling.Activate();
    }

    // ── Chrome ───────────────────────────────────────────────────────────

    private static Menu BuildMenu()
    {
        var menu = new Menu();
        AutomationProperties.SetName(menu, "Main Menu");

        var file = new MenuItem { Header = "_File" };
        file.Items.Add(new MenuItem { Header = "_New" });
        file.Items.Add(new MenuItem { Header = "_Open" });
        file.Items.Add(new MenuItem { Header = "_Save" });
        file.Items.Add(new Separator());
        file.Items.Add(new MenuItem { Header = "E_xit" });

        var edit = new MenuItem { Header = "_Edit" };
        edit.Items.Add(new MenuItem { Header = "_Undo" });
        edit.Items.Add(new MenuItem { Header = "_Redo" });

        var help = new MenuItem { Header = "_Help" };
        help.Items.Add(new MenuItem { Header = "_About" });

        menu.Items.Add(file);
        menu.Items.Add(edit);
        menu.Items.Add(help);
        return menu;
    }

    private static ToolBar BuildToolBar()
    {
        var bar = new ToolBar();
        AutomationProperties.SetName(bar, "Main Toolbar");
        foreach (var label in new[] { "New", "Open", "Save" })
        {
            var button = new Button { Content = label };
            AutomationProperties.SetName(button, label);
            bar.Items.Add(button);
        }

        return bar;
    }

    private static StatusBar BuildStatusBar()
    {
        var bar = new StatusBar();
        AutomationProperties.SetName(bar, "Status Bar");
        var item = new StatusBarItem { Content = "Ready" };
        AutomationProperties.SetName(item, "Ready");
        bar.Items.Add(item);
        return bar;
    }

    // ── Widget factories ─────────────────────────────────────────────────

    private static T Dock<T>(T element, Dock side)
        where T : UIElement
    {
        DockPanel.SetDock(element, side);
        return element;
    }

    private static Button MakeButton(string name)
    {
        var button = new Button
        {
            Content = name,
            Margin = new Thickness(4),
            Padding = new Thickness(10, 3, 10, 3),
        };
        AutomationProperties.SetName(button, name);
        return button;
    }

    private static CheckBox MakeCheckBox(string name, bool isChecked)
    {
        var box = new CheckBox
        {
            Content = name,
            IsChecked = isChecked,
            Margin = new Thickness(4),
        };
        AutomationProperties.SetName(box, name);
        return box;
    }

    private static RadioButton MakeRadio(string name)
    {
        var radio = new RadioButton
        {
            Content = name,
            GroupName = "Options",
            Margin = new Thickness(4),
        };
        AutomationProperties.SetName(radio, name);
        return radio;
    }

    private static TextBlock MakeText(string text)
    {
        var block = new TextBlock { Text = text, Margin = new Thickness(4) };
        AutomationProperties.SetName(block, text);
        return block;
    }

    /// <summary>
    ///  The canonical WPF grid. Its cells are ControlType.Custom plus the
    ///  TableItem pattern — the shape xa11y-windows maps to <c>table_cell</c>
    ///  via the Custom branch of map_uia_role (#323) — while the rows report
    ///  DataItem and the grid itself reports DataGrid (xa11y <c>table</c>).
    /// </summary>
    private DataGrid MakeUsersTable()
    {
        var grid = new DataGrid
        {
            ItemsSource = _users,
            AutoGenerateColumns = false,
            CanUserAddRows = false,
            CanUserDeleteRows = false,
            CanUserResizeRows = false,
            CanUserSortColumns = false,
            IsReadOnly = true,
            // Row headers would add a nameless header cell per row; keep the
            // cell count equal to the data cells plus the column headers.
            HeadersVisibility = DataGridHeadersVisibility.Column,
            SelectionMode = DataGridSelectionMode.Single,
            // Cell-level selection: with the default FullRow unit every cell in
            // the selected row reports selected, and the suite's "siblings must
            // not leak selection" assertion could not hold.
            SelectionUnit = DataGridSelectionUnit.Cell,
            Width = 420,
            Height = 130,
        };
        AutomationProperties.SetName(grid, "Users Table");

        grid.Columns.Add(new DataGridTextColumn
        {
            Header = "Name",
            Binding = new System.Windows.Data.Binding(nameof(User.Name)),
        });
        grid.Columns.Add(new DataGridTextColumn
        {
            Header = "Role",
            Binding = new System.Windows.Data.Binding(nameof(User.Role)),
        });
        return grid;
    }

    private static GroupBox MakeGroup(
        string name,
        Orientation orientation,
        params UIElement[] children)
    {
        var stack = new StackPanel { Orientation = orientation };
        foreach (var child in children)
        {
            stack.Children.Add(child);
        }

        var box = new GroupBox
        {
            Header = name,
            Margin = new Thickness(6),
            Padding = new Thickness(6),
            Content = stack,
        };
        AutomationProperties.SetName(box, name);
        return box;
    }
}
