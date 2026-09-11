# Native Windows notification-area fixture for shell-surface integration tests.
#
# The legacy WinForms ContextMenu wraps a Win32 HMENU, so opening it creates
# the system #32768 popup window used by native tray applications. NotifyIcon
# registers with Explorer's real notification area; Windows may place a newly
# registered icon in the visible row or in the real overflow flyout.

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$menu = [System.Windows.Forms.ContextMenu]::new()
[void]$menu.MenuItems.Add("xa11y tray action one")
[void]$menu.MenuItems.Add("xa11y tray action two")

$icon = [System.Windows.Forms.NotifyIcon]::new()
$icon.Icon = [System.Drawing.SystemIcons]::Application
$icon.Text = "xa11y tray fixture"
$icon.ContextMenu = $menu
$icon.Visible = $true

try {
    [System.Windows.Forms.Application]::Run()
}
finally {
    $icon.Visible = $false
    $icon.Dispose()
    $menu.Dispose()
}
