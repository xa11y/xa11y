# Integration test harness for xa11y on Windows.
#
# Launches the accesskit+winit test app and runs integration tests.
# Requires a desktop session for UI Automation to work.
#
# Usage: .\run_integ_tests_windows.ps1 [test_name_filter]

$ErrorActionPreference = "Stop"

$testFilter = $args[0]

Write-Host "=== xa11y Windows integration test harness ==="

# 1. Build everything
Write-Host "Building workspace..."
# xtask.exe can be the parent process; Windows locks running executables.
cargo build --workspace --exclude xtask 2>&1 | Write-Host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$testExit = 1
$trayFixture = $null
$testApp = $null
$richFixture = $null
try {
    # 2. Launch the background fixture before the test app. PowerShell can
    # briefly become the foreground process as it initializes WinForms; the
    # test app must launch last to preserve the harness's foreground contract.
    Write-Host "Launching native notification-area fixture..."
    # Windows PowerShell hosts the .NET Framework WinForms implementation;
    # its legacy ContextMenu is backed by a native HMENU/#32768 popup.
    $trayFixture = Start-Process -FilePath "powershell.exe" -ArgumentList "-NoProfile", "-File", ".\test-apps\windows-tray\app.ps1" -PassThru -WindowStyle Hidden

    Write-Host "Launching native rich fixture..."
    $richFixture = Start-Process -FilePath "powershell.exe" -ArgumentList "-NoProfile", "-STA", "-File", ".\test-apps\rich\windows.ps1" -PassThru -WindowStyle Hidden
    $env:XA11Y_WINDOWS_RICH_PID = "$($richFixture.Id)"
    # Wait for WPF initialization before the AccessKit app takes foreground.
    # Hidden hides the console, not the fixture's native window.
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $richFixture.Refresh()
        if ($richFixture.HasExited) { throw "Rich fixture exited before its window was ready" }
        if ($richFixture.MainWindowHandle -ne [IntPtr]::Zero) { break }
        if ([DateTime]::UtcNow -ge $readyDeadline) { throw "Timed out waiting for rich fixture window" }
        Start-Sleep -Milliseconds 100
    } while ($true)
    Write-Host "Launching xa11y-test-app..."
    $testApp = Start-Process -FilePath ".\target\debug\xa11y-test-app.exe" -ArgumentList "--headless" -PassThru -WindowStyle Hidden

    # Wait for accessibility registration
    Write-Host "Waiting for test app and tray fixture to register..."
    Start-Sleep -Seconds 3

    # 3. Run integration tests
    Write-Host "Running integration tests..."
    if ($testFilter) {
        cargo test -p xa11y --test integ_test -- --ignored --test-threads=1 $testFilter 2>&1 | Write-Host
    } else {
        cargo test -p xa11y --test integ_test -- --ignored --test-threads=1 2>&1 | Write-Host
    }
    $testExit = $LASTEXITCODE

    # A provider created on a non-pumping STA thread, used from others,
    # issue #499. Needs windows on the desktop; its own binary.
    if (-not $testFilter) {
        Write-Host "Running STA-host test..."
        cargo test -p xa11y-windows --test sta_host -- --ignored --test-threads=1 --nocapture 2>&1 | Write-Host
        if ($LASTEXITCODE -ne 0 -and $testExit -eq 0) { $testExit = $LASTEXITCODE }
    }

    # UIPI from a medium-integrity sender to the (elevated) test app, issue
    # #503. Needs the app up; its own binary, so it runs separately.
    if (-not $testFilter) {
        Write-Host "Running medium-integrity UIPI test..."
        cargo test -p xa11y-windows --test uipi_medium -- --ignored --test-threads=1 --nocapture 2>&1 | Write-Host
        if ($LASTEXITCODE -ne 0 -and $testExit -eq 0) { $testExit = $LASTEXITCODE }
    }

    # The system-aware host test (issue #506) needs the AccessKit app up: it
    # pins its own process SYSTEM_AWARE and reads that app's window, so an
    # unscoped coordinate path trips the debug_assert in
    # xa11y-windows/src/coords.rs. Its own process, so it runs separately.
    if (-not $testFilter) {
        Write-Host "Running system-aware host DPI test..."
        cargo test -p xa11y-windows --test dpi_system_aware -- --ignored --test-threads=1 --nocapture 2>&1 | Write-Host
        if ($LASTEXITCODE -ne 0 -and $testExit -eq 0) { $testExit = $LASTEXITCODE }
    }
} finally {
    # 4. Cleanup
    Write-Host "Cleaning up..."
    if ($null -ne $testApp) {
        Stop-Process -Id $testApp.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $testApp.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
    if ($null -ne $trayFixture) {
        Stop-Process -Id $trayFixture.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $trayFixture.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
    if ($null -ne $richFixture) {
        Stop-Process -Id $richFixture.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $richFixture.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
}

Write-Host "=== Integration tests finished (exit code: $testExit) ==="
exit $testExit
