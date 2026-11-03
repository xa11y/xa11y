# Trust the signing certificate and install the xa11y UWP test app.
#
#   powershell -File test-apps/uwp/install.ps1        # elevated; after build.ps1
#
# Importing into LocalMachine\TrustedPeople needs an elevated shell (CI's
# runner account is an administrator). Any previous install of the package
# is removed first, so a rebuild with a fresh certificate installs cleanly.
#
# Prints the package family name and the AppUserModelID the harness launches:
#   xa11y.UwpTestApp_<publisher hash>!App

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Out = Join-Path $PSScriptRoot 'out'
$Msix = Join-Path $Out 'xa11y-uwp-test-app.msix'
$Cer = Join-Path $Out 'xa11y-uwp-test.cer'
$Name = 'xa11y.UwpTestApp'

if (-not (Test-Path $Msix)) { throw "$Msix not found; run build.ps1 first" }

# Fail here, before installing, rather than at the first activation.
. (Join-Path $PSScriptRoot 'uac.ps1')
Assert-UwpActivatable

# Every build signs with a fresh certificate under the same subject, so a
# local machine collects one per build; drop the old ones before trusting the
# new one.
Get-ChildItem 'Cert:\LocalMachine\TrustedPeople' |
    Where-Object { $_.Subject -eq 'CN=xa11y Test' } |
    Remove-Item

Import-Certificate -FilePath $Cer -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null

Get-AppxPackage -Name $Name | Remove-AppxPackage
Add-AppxPackage -Path $Msix

$pkg = Get-AppxPackage -Name $Name
if (-not $pkg) { throw "Add-AppxPackage reported success but $Name is not installed" }
Write-Host "PackageFamilyName: $($pkg.PackageFamilyName)"
Write-Host "AUMID: $($pkg.PackageFamilyName)!App"
