# Suspend, resume or query the installed xa11y UWP test app's lifecycle.
#
#   powershell -File test-apps/uwp/plm.ps1 suspend
#   powershell -File test-apps/uwp/plm.ps1 resume
#   powershell -File test-apps/uwp/plm.ps1 query
#
# Drives PLMDebug.exe, the Process Lifetime Management debugging tool that
# ships with the Debugging Tools for Windows in the Windows SDK
# (Windows Kits\10\Debuggers\<arch>\plmdebug.exe). A suspended UWP app keeps
# its process but loses the CoreWindow child of its ApplicationFrameHost
# frame, which is the state #497's AppUserModelID fallback exists for.
#
# `/enableDebug` is applied first so the app's lifecycle stays where PLMDebug
# puts it instead of the shell resuming or terminating it on its own; it
# persists until `/disableDebug` or the package is reinstalled. Suspending a
# package that is not running is an error from PLMDebug, reported as-is.

param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('suspend', 'resume', 'query')]
    [string]$Action
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$pkg = Get-AppxPackage -Name 'xa11y.UwpTestApp'
if (-not $pkg) { throw 'xa11y.UwpTestApp is not installed; run build.ps1 and install.ps1 first' }

$kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Debuggers'
$plm = Get-ChildItem -Path $kits -Recurse -Filter plmdebug.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq 'x64' } |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $plm) {
    throw "plmdebug.exe not found under $kits; install the Debugging Tools for Windows from the Windows SDK"
}

function Invoke-Plm([string[]]$PlmArgs) {
    & $plm @PlmArgs
    if ($LASTEXITCODE -ne 0) { throw "plmdebug $($PlmArgs -join ' ') failed with exit code $LASTEXITCODE" }
}

$full = $pkg.PackageFullName
switch ($Action) {
    'suspend' { Invoke-Plm '/enableDebug', $full; Invoke-Plm '/suspend', $full }
    'resume' { Invoke-Plm '/resume', $full }
    'query' { Invoke-Plm '/query', $full }
}
