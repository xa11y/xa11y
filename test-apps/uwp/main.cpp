// xa11y UWP test app.
//
// The smallest real UWP app that runs under ApplicationFrameHost: an
// app-container process whose Windows.UI.Core.CoreWindow is parented into an
// ApplicationFrameWindow owned by ApplicationFrameHost.exe. That frame/content
// split is what issue #497 is about, and no other test app reproduces it:
// Win32, WinForms, WPF and WinUI 3 apps all own their top-level HWND.
//
// The UI is Windows.UI.Xaml built in code rather than from markup, so the app
// needs neither the XAML compiler nor Visual Studio's UWP workload. It builds
// with cl.exe, the Windows SDK's C++/WinRT projection and WindowsApp.lib (see
// build.ps1).
//
// The widget names mirror the WinForms/WPF apps (OK, Cancel, Search, Agree to
// terms, Subscribe, Option A/B, Volume, Progress, Submit, Add Item, Remove
// Item) so the shared suites' APP_CONFIGS shape applies unchanged.

#include <windows.h>

#include <string>

#include <winrt/Windows.ApplicationModel.Activation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.UI.Xaml.Automation.h>
#include <winrt/Windows.UI.Xaml.Controls.Primitives.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.UI.Xaml.h>

using namespace winrt;
using namespace Windows::ApplicationModel::Activation;
using namespace Windows::UI::Xaml;
using namespace Windows::UI::Xaml::Automation;
using namespace Windows::UI::Xaml::Controls;

namespace {

Button make_button(hstring const& label) {
    Button button;
    button.Content(box_value(label));
    button.Margin(ThicknessHelper::FromUniformLength(4));
    return button;
}

StackPanel row() {
    StackPanel panel;
    panel.Orientation(Orientation::Horizontal);
    return panel;
}

UIElement build_content() {
    StackPanel root;
    root.Padding(ThicknessHelper::FromUniformLength(12));

    // ── Buttons ──────────────────────────────────────────────────────────
    auto ok = make_button(L"OK");
    AutomationProperties::SetHelpText(ok, L"Confirm the dialog");
    auto cancel = make_button(L"Cancel");
    auto buttons = row();
    buttons.Children().Append(ok);
    buttons.Children().Append(cancel);
    root.Children().Append(buttons);

    // ── Text field ───────────────────────────────────────────────────────
    TextBox search;
    search.Text(L"hello world");
    search.Width(240);
    search.HorizontalAlignment(HorizontalAlignment::Left);
    AutomationProperties::SetName(search, L"Search");
    root.Children().Append(search);

    // ── Check boxes ──────────────────────────────────────────────────────
    CheckBox agree;
    agree.Content(box_value(L"Agree to terms"));
    agree.IsChecked(false);
    CheckBox subscribe;
    subscribe.Content(box_value(L"Subscribe"));
    subscribe.IsChecked(true);
    auto checks = row();
    checks.Children().Append(agree);
    checks.Children().Append(subscribe);
    root.Children().Append(checks);

    // ── Radio buttons ────────────────────────────────────────────────────
    RadioButton option_a;
    option_a.Content(box_value(L"Option A"));
    option_a.GroupName(L"options");
    option_a.IsChecked(true);
    RadioButton option_b;
    option_b.Content(box_value(L"Option B"));
    option_b.GroupName(L"options");
    auto radios = row();
    radios.Children().Append(option_a);
    radios.Children().Append(option_b);
    root.Children().Append(radios);

    // ── Range controls ───────────────────────────────────────────────────
    Slider volume;
    volume.Minimum(0);
    volume.Maximum(100);
    volume.Value(50);
    volume.Width(240);
    volume.HorizontalAlignment(HorizontalAlignment::Left);
    AutomationProperties::SetName(volume, L"Volume");
    root.Children().Append(volume);

    ProgressBar progress;
    progress.Minimum(0);
    progress.Maximum(100);
    progress.Value(40);
    progress.Width(240);
    progress.HorizontalAlignment(HorizontalAlignment::Left);
    AutomationProperties::SetName(progress, L"Progress");
    root.Children().Append(progress);

    // ── Dynamic content (event tests) ────────────────────────────────────
    // Submit toggles the status text *and* its accessible name, so a press
    // produces a NameChanged; Add/Remove Item mutate the list's children,
    // producing StructureChanged.
    TextBlock status;
    status.Text(L"Status: Ready");
    AutomationProperties::SetName(status, L"Status: Ready");

    ListView items;
    AutomationProperties::SetName(items, L"Items");
    items.Height(120);
    items.Width(240);
    items.HorizontalAlignment(HorizontalAlignment::Left);

    auto submit = make_button(L"Submit");
    submit.Click([status](auto&&, auto&&) {
        hstring next = status.Text() == L"Status: Submitted" ? L"Status: Ready" : L"Status: Submitted";
        status.Text(next);
        AutomationProperties::SetName(status, next);
    });
    auto add_item = make_button(L"Add Item");
    add_item.Click([items](auto&&, auto&&) {
        auto list = items.Items();
        std::wstring label = L"Item " + std::to_wstring(list.Size() + 1);
        list.Append(box_value(hstring{label}));
    });
    auto remove_item = make_button(L"Remove Item");
    remove_item.Click([items](auto&&, auto&&) {
        auto list = items.Items();
        if (list.Size() > 0) {
            list.RemoveAtEnd();
        }
    });

    auto dynamic = row();
    dynamic.Children().Append(submit);
    dynamic.Children().Append(add_item);
    dynamic.Children().Append(remove_item);
    root.Children().Append(status);
    root.Children().Append(dynamic);
    root.Children().Append(items);

    ScrollViewer scroller;
    scroller.Content(root);
    return scroller;
}

struct App : ApplicationT<App> {
    void OnLaunched(LaunchActivatedEventArgs const&) {
        auto window = Window::Current();
        // Activating a running app calls OnLaunched again; build only once.
        if (!window.Content()) {
            window.Content(build_content());
        }
        window.Activate();
    }
};

}  // namespace

int __stdcall wWinMain(HINSTANCE, HINSTANCE, PWSTR, int) {
    init_apartment(apartment_type::single_threaded);
    Application::Start([](auto&&) { make<App>(); });
    return 0;
}
