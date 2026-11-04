# Dot-sourced by install.ps1 and launch.ps1: report the UAC settings UWP
# activation depends on, and fail by name when they rule it out.
#
# Windows refuses to activate a UWP app while UAC is off (EnableLUA=0); the
# activation then fails with an HRESULT that says nothing about UAC. Changing
# EnableLUA takes a reboot, so on a hosted CI runner it is a property of the
# image, not something a job can fix. ConsentPromptBehaviorAdmin=0 (elevate
# without prompting) is unrelated to activation and is printed only so a log
# shows the whole UAC configuration.

function Assert-UwpActivatable {
    $key = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    $props = Get-ItemProperty -Path $key
    $lua = $props.PSObject.Properties['EnableLUA']
    $consent = $props.PSObject.Properties['ConsentPromptBehaviorAdmin']
    $luaText = if ($lua) { $lua.Value } else { '<unset>' }
    $consentText = if ($consent) { $consent.Value } else { '<unset>' }
    [Console]::Error.WriteLine("UAC: EnableLUA=$luaText ConsentPromptBehaviorAdmin=$consentText")
    # An unset EnableLUA means the default, which is on.
    if ($lua -and $lua.Value -eq 0) {
        throw "UwpActivationUnavailable: UAC is disabled (EnableLUA=0 under $key). Windows does not activate UWP apps without UAC, and re-enabling it needs a reboot."
    }
}
