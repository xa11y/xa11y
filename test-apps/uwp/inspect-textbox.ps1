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

    # Record native button identities across controlled focus changes. A
    # TextBox template can expose a transient clear button; these observations
    # distinguish that change from duplicate results in a descendant query.
    function Read-ButtonIdentities($element, [int] $depth, [ref] $visited) {
        $visited.Value = $visited.Value + 1
        if ($depth -gt 30 -or $visited.Value -gt 256) {
            throw 'UWP button inspection exceeded depth 30 or 256 nodes'
        }
        $current = $element.Current
        if ($current.ControlType -eq [System.Windows.Automation.ControlType]::Button) {
            $bounds = $current.BoundingRectangle
            $name = [string] $current.Name
            $className = [string] $current.ClassName
            $runtimeId = @($element.GetRuntimeId())
            [pscustomobject]@{
                name = $name.Substring(0, [Math]::Min(120, $name.Length))
                class_name = $className.Substring(0, [Math]::Min(80, $className.Length))
                pid = $current.ProcessId
                runtime_id = @($runtimeId | Select-Object -First 16)
                runtime_id_truncated = $runtimeId.Count -gt 16
                bounds = @($bounds.X, $bounds.Y, $bounds.Width, $bounds.Height)
            }
        }
        $child = $walker.GetFirstChild($element)
        while ($child) {
            Read-ButtonIdentities $child ($depth + 1) $visited
            $child = $walker.GetNextSibling($child)
        }
    }
    $searchCondition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'Search'),
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ClassNameProperty, 'TextBox'))
    $buttonCondition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'OK'),
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button))
    $scope = [System.Windows.Automation.TreeScope]::Descendants
    $search = $frame.FindFirst($scope, $searchCondition)
    $ok = $frame.FindFirst($scope, $buttonCondition)
    if (-not $search -or -not $ok) { throw 'The focus probe requires Search and OK' }
    foreach ($stage in @(
        @{ name = 'button_focused'; target = $ok },
        @{ name = 'search_focused'; target = $search },
        @{ name = 'search_blurred'; target = $ok }
    )) {
        $stage.target.SetFocus()
        Start-Sleep -Milliseconds 200
        $visited = 0
        $buttons = @(Read-ButtonIdentities $frame 0 ([ref] $visited))
        $report = [pscustomobject]@{
            stage = $stage.name
            count = $buttons.Count
            truncated = $buttons.Count -gt 24
            buttons = @($buttons | Select-Object -First 24)
        }
        Write-Output ('UWP_BUTTON_IDENTITIES ' + ($report | ConvertTo-Json -Depth 6 -Compress))
    }
}
finally {
    Stop-Process -Id ([int] $appPid) -ErrorAction Stop
}
