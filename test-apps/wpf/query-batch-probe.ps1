param([int] $AppPid)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::FromHandle((Get-Process -Id $AppPid).MainWindowHandle)
$condition = [System.Windows.Automation.Condition]::TrueCondition
$request = [System.Windows.Automation.CacheRequest]::new()
$request.Add([System.Windows.Automation.AutomationElement]::NameProperty)
$request.Add([System.Windows.Automation.AutomationElement]::ControlTypeProperty)
$request.Add([System.Windows.Automation.AutomationElement]::RuntimeIdProperty)
function Walk($node, $records) {
    foreach ($child in $node.FindAll([System.Windows.Automation.TreeScope]::Children, $condition)) {
        $records.Add((Record $child))
        Walk $child $records
    }
}
function Record($node) {
    $id = $node.GetCachedPropertyValue([System.Windows.Automation.AutomationElement]::RuntimeIdProperty)
    return (($id -join '.') + ' ' + $node.Cached.ControlType.ProgrammaticName + ' ' + $node.Cached.Name)
}
$scope = $request.Activate()
try {
    foreach ($mode in @("parents", "batch", "batch", "parents", "batch", "parents")) {
        $records = [Collections.Generic.List[string]]::new()
        $clock = [Diagnostics.Stopwatch]::StartNew()
        if ($mode -eq "parents") {
            Walk $root $records
        } else {
            foreach ($node in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
                $records.Add((Record $node))
            }
        }
        $clock.Stop()
        $equal = $true
        if ($mode -eq "parents") {
            $expected = $records -join "`n"
        } else {
            $equal = $expected -ceq ($records -join "`n")
        }
        Write-Host "WPF_BATCH_PROBE mode=$mode ms=$($clock.ElapsedMilliseconds) nodes=$($records.Count) orderedParity=$equal"
    }
} finally {
    $scope.Dispose()
}
