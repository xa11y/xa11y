# Build, package and sign the xa11y UWP test app.
#
#   powershell -File test-apps/uwp/build.ps1
#
# Produces, under test-apps/uwp/out/:
#   xa11y-uwp-test-app.msix   the signed package
#   xa11y-uwp-test.cer        the public half of the signing certificate
#
# Then run install.ps1 (elevated) to trust the certificate and install the
# package. Needs only the MSVC x64 tools and a Windows 10 SDK: the app is
# C++/WinRT with Windows.UI.Xaml built in code, so neither the XAML compiler
# nor Visual Studio's UWP workload is involved.
#
# The certificate is self-signed and generated fresh on every build. Its
# subject must equal Identity/@Publisher in AppxManifest.xml, which is what
# makes the package family name (and so the AUMID) stable across builds.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Here = $PSScriptRoot
$Out = Join-Path $Here 'out'
$Layout = Join-Path $Out 'layout'
$Obj = Join-Path $Out 'obj'
$Msix = Join-Path $Out 'xa11y-uwp-test-app.msix'
$Cer = Join-Path $Out 'xa11y-uwp-test.cer'
$Publisher = 'CN=xa11y Test'

if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force -Path $Layout, $Obj | Out-Null

# ── MSVC environment ───────────────────────────────────────────────────────
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) { throw "vswhere.exe not found at $vswhere; install Visual Studio Build Tools with the C++ workload" }
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'No Visual Studio installation with the MSVC x64 tools (Microsoft.VisualStudio.Component.VC.Tools.x86.x64) found' }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($Matches[1])" -Value $Matches[2] }
}
if (-not $env:WindowsSdkDir -or -not $env:WindowsSDKVersion) { throw "vcvars64.bat did not set up a Windows SDK (WindowsSdkDir/WindowsSDKVersion unset)" }
$sdkVer = $env:WindowsSDKVersion.TrimEnd('\')
$sdkBin = Join-Path $env:WindowsSdkDir "bin\$sdkVer\x64"
$cppwinrt = Join-Path $env:WindowsSdkDir "Include\$sdkVer\cppwinrt"
Write-Host "MSVC: $vs"
Write-Host "Windows SDK: $sdkVer"

# ── Compile ────────────────────────────────────────────────────────────────
# /APPCONTAINER marks the image as runnable only inside an app container,
# which a UWP entry point requires. The CRT is linked statically (/MT) so the
# package needs no Microsoft.VCLibs framework dependency; Store certification
# would reject that, sideloading does not care. C++20, because under C++17
# C++/WinRT falls back to <experimental/coroutine>, which current MSVC
# rejects outright (STL1011).
$exe = Join-Path $Layout 'xa11y-uwp-test-app.exe'
& cl.exe /nologo /std:c++20 /EHsc /permissive- /W4 /MT /O1 `
    /I $cppwinrt `
    /Fo"$Obj\\" (Join-Path $Here 'main.cpp') `
    /Fe"$exe" `
    /link /APPCONTAINER /SUBSYSTEM:WINDOWS WindowsApp.lib
if ($LASTEXITCODE -ne 0) { throw "cl.exe failed with exit code $LASTEXITCODE" }

# ── Layout + pack ──────────────────────────────────────────────────────────
Copy-Item (Join-Path $Here 'AppxManifest.xml') $Layout
Copy-Item -Recurse (Join-Path $Here 'Assets') (Join-Path $Layout 'Assets')
& (Join-Path $sdkBin 'makeappx.exe') pack /o /d $Layout /p $Msix
if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed with exit code $LASTEXITCODE" }

# ── Sign ───────────────────────────────────────────────────────────────────
# EKU 1.3.6.1.5.5.7.3.3 (code signing) and an empty basic-constraints
# extension are what Add-AppxPackage requires of a package signer.
$cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher `
    -KeyUsage DigitalSignature -FriendlyName 'xa11y UWP test app signing' `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
try {
    & (Join-Path $sdkBin 'signtool.exe') sign /fd SHA256 /sha1 $cert.Thumbprint /s My $Msix
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed with exit code $LASTEXITCODE" }
    Export-Certificate -Cert $cert -FilePath $Cer | Out-Null
}
finally {
    # The private key is single-use: nothing else should ever be signed by it.
    Remove-Item -Path "Cert:\CurrentUser\My\$($cert.Thumbprint)" -DeleteKey
}

Write-Host "Built $Msix"
