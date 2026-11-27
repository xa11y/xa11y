# A WPF ScrollViewer supplies the UIA viewport and stable child peers used by
# the observation cache and the WPF query cost model.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationFramework
$window = New-Object Windows.Window
$window.Title = 'xa11y scrolling queries'
$window.Width = 400
$window.Height = 300
$window.ShowActivated = $false
$scroll = New-Object Windows.Controls.ScrollViewer
$list = New-Object Windows.Controls.StackPanel
for ($i = 0; $i -lt 32; $i++) {
    $button = New-Object Windows.Controls.Button
    $button.Content = "Probe $i"
    $button.Height = 32
    [void]$list.Children.Add($button)
}
$first = $list.Children[0]
$last = $list.Children[31]
$first.Add_Click({ $scroll.ScrollToEnd() })
$last.Add_Click({
    # Destroy the scrolling container and reparent its two surviving peers
    # in reverse order. Both are now visible even with the old query active.
    $list.Children.Clear()
    $scroll.Content = $null
    $replacement = New-Object Windows.Controls.StackPanel
    [void]$replacement.Children.Add($last)
    [void]$replacement.Children.Add($first)
    $window.Content = $replacement
})
$scroll.Content = $list
$window.Content = $scroll
$window.Add_ContentRendered({
    [Console]::WriteLine('ready')
    [Console]::Out.Flush()
})
[void]$window.ShowDialog()
