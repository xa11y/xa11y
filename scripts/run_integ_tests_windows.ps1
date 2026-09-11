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
cargo build --workspace 2>&1 | Write-Host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# 2. Launch the test application
Write-Host "Launching xa11y-test-app..."
$testApp = Start-Process -FilePath ".\target\debug\xa11y-test-app.exe" -ArgumentList "--headless" -PassThru -WindowStyle Hidden

$testExit = 1
$trayFixture = $null
try {
    Write-Host "Launching native notification-area fixture..."
    # Windows PowerShell hosts the .NET Framework WinForms implementation;
    # its legacy ContextMenu is backed by a native HMENU/#32768 popup.
    $trayFixture = Start-Process -FilePath "powershell.exe" -ArgumentList "-NoProfile", "-File", ".\test-apps\windows-tray\app.ps1" -PassThru -WindowStyle Hidden

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
} finally {
    # 4. Cleanup
    Write-Host "Cleaning up..."
    Stop-Process -Id $testApp.Id -Force -ErrorAction SilentlyContinue
    Wait-Process -Id $testApp.Id -Timeout 5 -ErrorAction SilentlyContinue
    if ($null -ne $trayFixture) {
        Stop-Process -Id $trayFixture.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $trayFixture.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
}

Write-Host "=== Integration tests finished (exit code: $testExit) ==="
exit $testExit
