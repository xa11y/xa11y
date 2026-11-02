# Launch the installed xa11y UWP test app and print its process id.
#
#   powershell -File test-apps/uwp/launch.ps1            # prints e.g. "12345"
#
# Activates by AppUserModelID through IApplicationActivationManager, the
# shell's own activation API (what `explorer.exe shell:AppsFolder\<AUMID>`
# ends up calling). Unlike the explorer route it returns the PID of the app's
# own process — xa11y-uwp-test-app.exe, the one that owns the CoreWindow —
# which is what the harness watches and tears down. Note that this is NOT the
# PID of the window the user sees: that frame belongs to
# ApplicationFrameHost.exe (issue #497).
#
# Activating an app that is already running brings it forward and returns
# the same PID.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$pkg = Get-AppxPackage -Name 'xa11y.UwpTestApp'
if (-not $pkg) { throw 'xa11y.UwpTestApp is not installed; run build.ps1 and install.ps1 first' }
$aumid = "$($pkg.PackageFamilyName)!App"

# Reports UAC on stderr (stdout carries only the PID) and throws
# UwpActivationUnavailable when UAC is off.
. (Join-Path $PSScriptRoot 'uac.ps1')
Assert-UwpActivatable

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace Xa11yUwp {
    [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
                                [MarshalAs(UnmanagedType.LPWStr)] string arguments,
                                int options, out uint processId);
        [PreserveSig] int ActivateForFile(IntPtr a, IntPtr b, IntPtr c, out uint d);
        [PreserveSig] int ActivateForProtocol(IntPtr a, IntPtr b, out uint c);
    }

    [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
    class ApplicationActivationManager { }

    public static class UwpLauncher {
        public static uint Activate(string aumid) {
            var mgr = (IApplicationActivationManager)new ApplicationActivationManager();
            uint pid;
            int hr = mgr.ActivateApplication(aumid, null, 0, out pid);
            if (hr < 0) Marshal.ThrowExceptionForHR(hr);
            return pid;
        }
    }
}
'@

$started = Get-Date
try {
    [Xa11yUwp.UwpLauncher]::Activate($aumid)
}
catch {
    # Activation errors ("The app didn't start.") say nothing about why. The
    # reason is in the event logs: a crash of the app's process (Application
    # Error / Windows Error Reporting), or the app model's own account of the
    # activation. Print what they recorded since this launch, then fail.
    $err = $_
    $ex = $err.Exception
    if ($ex.InnerException) { $ex = $ex.InnerException }
    [Console]::Error.WriteLine(("Activation of {0} failed: HRESULT 0x{1:X8}: {2}" -f $aumid, $ex.HResult, $ex.Message.Trim()))
    # The full path: on a CI runner's git-bash PATH, a bare `whoami` is MSYS's.
    [Console]::Error.WriteLine("Account: $(& "$env:SystemRoot\System32\whoami.exe" /user /fo csv /nh)")
    # Written by the app itself (main.cpp, log_failure) when startup fails.
    $failureLog = Join-Path $env:LOCALAPPDATA "Packages\$($pkg.PackageFamilyName)\LocalState\startup-failure.log"
    if (Test-Path $failureLog) {
        Get-Content $failureLog | ForEach-Object { [Console]::Error.WriteLine("[app] $_") }
    } else {
        [Console]::Error.WriteLine("[app] no $failureLog")
    }
    $logs = @(
        @{ LogName = 'Application'; ProviderName = 'Application Error', 'Windows Error Reporting' },
        @{ LogName = 'Microsoft-Windows-AppModel-Runtime/Admin' },
        @{ LogName = 'Microsoft-Windows-TWinUI/Operational' },
        @{ LogName = 'Microsoft-Windows-Immersive-Shell/Operational' }
    )
    foreach ($filter in $logs) {
        $filter.StartTime = $started.AddSeconds(-5)
        try {
            Get-WinEvent -FilterHashtable $filter -MaxEvents 20 -ErrorAction Stop |
                ForEach-Object {
                    [Console]::Error.WriteLine("[$($filter.LogName)] $($_.TimeCreated) id=$($_.Id) $($_.LevelDisplayName): $($_.Message)")
                }
        }
        catch [System.Exception] {
            # "No events were found" is the expected answer from a quiet log.
            [Console]::Error.WriteLine("[$($filter.LogName)] $($_.Exception.Message)")
        }
    }
    throw $err
}
