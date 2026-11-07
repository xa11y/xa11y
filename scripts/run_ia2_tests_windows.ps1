# Explicit native IA2 Text/Table and owned-browser coverage, including bindings.
$ErrorActionPreference = 'Stop'
python -m pip install pywin32
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
python -m pip install -e .\xa11y-python
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build -p xa11y --bin xa11y
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Push-Location .\xa11y-js
try {
    npm ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    npx napi build --platform --js native.js --dts native.d.ts
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    node scripts/patch-native-dts.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally { Pop-Location }
cargo test -p xa11y-windows --lib ia2::
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test -p xa11y-windows --example ia2_probe --example ia2_rich_probe -- --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build -p xa11y-windows --example ia2_probe --example ia2_rich_probe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:XA11Y_IA2_PROBE = "$PWD/target/debug/examples/ia2_probe.exe"
$env:XA11Y_IA2_RICH_PROBE = "$PWD/target/debug/examples/ia2_rich_probe.exe"
$env:XA11Y_IA2_CONSUMERS = '1'
try {
    cargo test -p xa11y-windows --example ia2_probe --example ia2_rich_probe -- --ignored --test-threads=1 --nocapture
    $testExit = $LASTEXITCODE
} finally {
    Remove-Item Env:XA11Y_IA2_CONSUMERS
}
exit $testExit
