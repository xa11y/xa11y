# Control the fixture's package lifecycle through the same native COM API as
# PLMDebug, without requiring the optional Windows SDK debugger installation.
# `hold` suspends until stdin receives a line (or closes), then always resumes
# and disables debugging. The marker is written only after PES_SUSPENDED.
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('suspend', 'resume', 'query', 'hold')]
    [string]$Action,
    [string]$ReadyFile
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("F27C3930-8029-4AD1-94E3-3DBA417810C1"),
 InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPackageDebugSettings {
    void EnableDebugging([MarshalAs(UnmanagedType.LPWStr)] string package,
        [MarshalAs(UnmanagedType.LPWStr)] string debugger, IntPtr environment);
    void DisableDebugging([MarshalAs(UnmanagedType.LPWStr)] string package);
    void Suspend([MarshalAs(UnmanagedType.LPWStr)] string package);
    void Resume([MarshalAs(UnmanagedType.LPWStr)] string package);
    void TerminateAllProcesses([MarshalAs(UnmanagedType.LPWStr)] string package);
    void SetTargetSessionId(uint session);
    void EnumerateBackgroundTasks([MarshalAs(UnmanagedType.LPWStr)] string package,
        out uint count, out IntPtr ids, out IntPtr names);
    void ActivateBackgroundTask(IntPtr taskId);
    void StartServicing([MarshalAs(UnmanagedType.LPWStr)] string package);
    void StopServicing([MarshalAs(UnmanagedType.LPWStr)] string package);
    void StartSessionRedirection([MarshalAs(UnmanagedType.LPWStr)] string package, uint session);
    void StopSessionRedirection([MarshalAs(UnmanagedType.LPWStr)] string package);
    void GetPackageExecutionState([MarshalAs(UnmanagedType.LPWStr)] string package, out int state);
}
public static class PackageLifecycle {
    public static IPackageDebugSettings Create() {
        return (IPackageDebugSettings)Activator.CreateInstance(Type.GetTypeFromCLSID(
            new Guid("B1AEC16F-2383-4852-B0E9-8F0B1DC66B4D")));
    }
}
"@

$pkg = Get-AppxPackage -Name 'xa11y.UwpTestApp'
if (-not $pkg) { throw 'xa11y.UwpTestApp is not installed; run build.ps1 and install.ps1 first' }
$full = $pkg.PackageFullName
$settings = [PackageLifecycle]::Create()
$enabled = $false
try {
    if ($Action -eq 'query') {
        $state = 0
        $settings.GetPackageExecutionState($full, [ref]$state)
        Write-Output $state
    } else {
        if ($Action -eq 'hold' -and -not $ReadyFile) { throw 'hold requires -ReadyFile' }
        $settings.EnableDebugging($full, $null, [IntPtr]::Zero)
        $enabled = $true
        if ($Action -eq 'resume') {
            $settings.Resume($full)
            return
        }
        $settings.Suspend($full)
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        do {
            $state = 0
            $settings.GetPackageExecutionState($full, [ref]$state)
            if ($state -eq 3) { break } # PACKAGE_EXECUTION_STATE.PES_SUSPENDED
            if ([DateTime]::UtcNow -ge $deadline) { throw "Package did not suspend: state=$state" }
            Start-Sleep -Milliseconds 100
        } while ($true)
        if ($Action -eq 'hold') {
            [IO.File]::WriteAllText($ReadyFile, 'PES_SUSPENDED')
            [Console]::In.ReadLine() | Out-Null
        }
    }
} finally {
    if ($enabled) {
        try {
            if ($Action -eq 'hold') { $settings.Resume($full) }
        } finally {
            $settings.DisableDebugging($full)
        }
    }
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($settings) | Out-Null
}
