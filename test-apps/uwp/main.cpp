// xa11y UWP test app.
//
// The smallest real UWP app that runs under ApplicationFrameHost: an
// app-container process whose Windows.UI.Core.CoreWindow is parented into an
// ApplicationFrameWindow owned by ApplicationFrameHost.exe. That frame/content
// split is what issue #497 is about, and no other test app reproduces it —
// Win32, WinForms, WPF and WinUI 3 apps all own their top-level HWND.
//
// The UI is Windows.UI.Xaml built in code rather than from markup, so the app
// needs neither the XAML compiler nor Visual Studio's UWP workload: it builds
// with cl.exe, the Windows SDK's C++/WinRT projection, and WindowsApp.lib
// (see build.ps1).

#include <windows.h>

#include <winrt/Windows.ApplicationModel.Activation.h>
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

CheckBox make_check_box(hstring const& label, bool checked) {
    CheckBox box;
    box.Content(box_value(label));
    box.IsChecked(checked);
    return box;
}

UIElement build_content() {
    StackPanel root;
    root.Padding(ThicknessHelper::FromUniformLength(12));

    TextBlock status;
    status.Text(L"Ready");
    AutomationProperties::SetName(status, L"Status");

    auto ok = make_button(L"OK");
    AutomationProperties::SetHelpText(ok, L"Confirm the dialog");
    ok.Click([status](auto&&, auto&&) { status.Text(L"OK pressed"); });

    auto cancel = make_button(L"Cancel");
    cancel.Click([status](auto&&, auto&&) { status.Text(L"Cancel pressed"); });

    auto submit = make_button(L"Submit");
    submit.Click([status](auto&&, auto&&) { status.Text(L"Submitted"); });

    StackPanel buttons;
    buttons.Orientation(Orientation::Horizontal);
    buttons.Children().Append(ok);
    buttons.Children().Append(cancel);
    buttons.Children().Append(submit);

    TextBox search;
    search.Text(L"hello world");
    search.Width(240);
    search.HorizontalAlignment(HorizontalAlignment::Left);
    AutomationProperties::SetName(search, L"Search");

    root.Children().Append(buttons);
    root.Children().Append(search);
    root.Children().Append(make_check_box(L"Agree to terms", false));
    root.Children().Append(make_check_box(L"Subscribe", true));
    root.Children().Append(status);
    return root;
}

struct App : ApplicationT<App> {
    void OnLaunched(LaunchActivatedEventArgs const&) {
        auto window = Window::Current();
        // A relaunch of a running app activates it again; only build once.
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
