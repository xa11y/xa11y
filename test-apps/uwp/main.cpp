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
// <windows.h> defines GetCurrentTime as a macro, which collides with the
// C++/WinRT projection's Storyboard::GetCurrentTime (the usual C++/WinRT
// workaround).
#undef GetCurrentTime

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <string>
#include <string_view>

#include <winrt/Windows.ApplicationModel.Activation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Storage.h>
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

// ── Startup failure log ─────────────────────────────────────────────────
// A UWP process that dies during startup leaves only "The app didn't start"
// and an exception code in the event log. So every failure path appends what
// it caught to LocalState\startup-failure.log (the one folder an app
// container may write, at %LOCALAPPDATA%\Packages\<family>\LocalState), and
// test-apps/uwp/launch.ps1 prints that file when activation fails. The
// failure still propagates: logging is the only thing these handlers add.
void log_failure(std::wstring_view where, std::wstring_view what) noexcept {
    try {
        std::wstring path{Windows::Storage::ApplicationData::Current().LocalFolder().Path()};
        path += L"\\startup-failure.log";
        FILE* file = nullptr;
        if (_wfopen_s(&file, path.c_str(), L"a, ccs=UTF-8") == 0 && file) {
            fwprintf(file, L"%.*s: %.*s\n", static_cast<int>(where.size()), where.data(),
                     static_cast<int>(what.size()), what.data());
            fclose(file);
        }
    } catch (...) {
        // Nowhere left to report a failure to write the failure report; the
        // original failure is still propagating to the caller.
    }
}

void log_hresult(std::wstring_view where, hresult_error const& error) noexcept {
    wchar_t code[16];
    swprintf_s(code, L"0x%08X ", static_cast<uint32_t>(error.code().value));
    log_failure(where, std::wstring{code} + std::wstring{error.message()});
}

struct App : ApplicationT<App> {
    App() {
        UnhandledException([](auto&&, UnhandledExceptionEventArgs const& args) {
            wchar_t code[16];
            swprintf_s(code, L"0x%08X ", static_cast<uint32_t>(args.Exception().value));
            log_failure(L"Application.UnhandledException", std::wstring{code} + std::wstring{args.Message()});
        });
    }

    void OnLaunched(LaunchActivatedEventArgs const&) {
        try {
            auto window = Window::Current();
            // Activating a running app calls OnLaunched again; build only once.
            if (!window.Content()) {
                window.Content(build_content());
            }
            window.Activate();
        } catch (hresult_error const& error) {
            log_hresult(L"OnLaunched", error);
            throw;
        }
    }
};

}  // namespace

// The main thread must join the MTA before Application::Start. With no
// apartment, Application::Start throws RPC_E_WRONG_THREAD (0x8001010E, "The
// Application Object must initially be accessed from the multi-thread
// apartment"), and an STA here failed activation the same way: both reach the
// activator only as "The app didn't start".
int __stdcall wWinMain(HINSTANCE, HINSTANCE, PWSTR, int) {
    init_apartment(apartment_type::multi_threaded);
    std::set_terminate([] {
        log_failure(L"std::terminate", L"uncaught exception or noexcept violation");
        std::abort();
    });
    try {
        Application::Start([](auto&&) { make<App>(); });
    } catch (hresult_error const& error) {
        log_hresult(L"Application::Start", error);
        throw;
    }
    return 0;
}
