# Report the native Raw View behind the Search TextBox (issue #606).
# Requires the installed UWP fixture. Activate it, inspect only its named
# frame, and stop the process we activated when the probe finishes.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$appPid = & (Join-Path $PSScriptRoot 'launch.ps1')
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, 'xa11y UWP Test')
    do {
        $frame = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [System.Windows.Automation.TreeScope]::Children, $condition)
        if ($frame) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $frame) { throw 'The activated UWP fixture has no xa11y UWP Test frame' }

    $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
    function Read-SearchFields($element, [int] $depth) {
        if ($depth -gt 30) { throw 'UWP Raw View exceeds the inspection depth limit' }
        $current = $element.Current
        if ($current.Name -eq 'Search') {
            $parent = $walker.GetParent($element)
            [pscustomobject]@{
                depth = $depth
                name = $current.Name
                is_control_element = $current.IsControlElement
                is_content_element = $current.IsContentElement
                class_name = $current.ClassName
                automation_id = $current.AutomationId
                control_type = $current.ControlType.ProgrammaticName
                parent_class = $parent.Current.ClassName
                parent_type = $parent.Current.ControlType.ProgrammaticName
            }
        }
        $child = $walker.GetFirstChild($element)
        while ($child) {
            Read-SearchFields $child ($depth + 1)
            $child = $walker.GetNextSibling($child)
        }
    }
    do {
        $fields = @(Read-SearchFields $frame 0)
        if ($fields.Count -gt 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($fields.Count -eq 0) { throw 'Raw View contains no Search nodes' }
    $fields | ConvertTo-Json -Depth 4
}
finally {
    Stop-Process -Id ([int] $appPid) -ErrorAction Stop
}
