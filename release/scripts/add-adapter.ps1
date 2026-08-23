<#
.SYNOPSIS
    Teaches the OpenLEAudio driver package about another Bluetooth adapter.

.DESCRIPTION
    The stack drives its adapter through WinUSB, and WinUSB has to be bound to a
    named device. The INF therefore lists adapters by hardware ID, one line each,
    and it ships with only the one this project was developed against.

    This script adds the ones actually plugged into this machine.

    It does not guess. Every ID written to the INF is read out of Windows' own
    device enumeration, filtered to the USB Bluetooth class - so it can only ever
    name a real Bluetooth controller that is really present. That is the whole
    reason the INF is not simply matched against the device class instead:
    matching the class would claim every Bluetooth radio in the machine, and
    somebody with one radio would be left with no Bluetooth and no obvious way
    back.

    Adding an adapter does not switch anything. It only makes the adapter
    selectable; switching it over is still a separate, confirmed step.

.PARAMETER HardwareId
    Add one specific ID, e.g. 'USB\VID_8087&PID_0032'. Without it, every present
    controller missing from the INF is offered one at a time.

.PARAMETER All
    Add every present controller without asking about each one.

.PARAMETER NoSign
    Skip re-signing. Useful when several adapters are added in a row and the
    signature is refreshed once at the end.

.NOTES
    The catalog is signed against the INF, so changing the INF invalidates the
    signature. This script re-runs the signing step for you; that needs
    administrator rights and test signing enabled, exactly as the first install
    did.
#>

[CmdletBinding()]
param(
    [string] $HardwareId,
    [switch] $All,
    [switch] $NoSign
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent
$InfPath = Join-Path $Root 'driver\olea_winusb.inf'
$SignScript = Join-Path $PSScriptRoot 'sign-driver.ps1'

function Start-Step {
    param([string] $Text, [string] $Expect = 'this can take up to a minute')
    Write-Host ""
    Write-Host "-> $Text" -ForegroundColor Cyan
    Write-Host "   Working - $Expect. Do not close this window." -ForegroundColor DarkGray
}

function Complete-Script {
    param([string] $Text)
    Write-Host ""
    Write-Host ("=" * 66)
    Write-Host "  FINISHED - $Text" -ForegroundColor Green
    Write-Host "  Nothing else is running. This window can be closed."
    Write-Host ("=" * 66)
}

# The USB Bluetooth class as the specification defines it: wireless controller,
# radio frequency, Bluetooth programming interface. Every dongle presents this,
# and nothing that is not a Bluetooth controller does.
function Get-BluetoothControllers {
    $found = @{}

    foreach ($device in Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue) {
        $compatible = (Get-PnpDeviceProperty -InstanceId $device.InstanceId `
                -KeyName 'DEVPKEY_Device_CompatibleIds' -ErrorAction SilentlyContinue).Data
        if (-not $compatible) { continue }
        if (-not ($compatible -match 'Class_E0&SubClass_01&Prot_01')) { continue }

        $hardware = (Get-PnpDeviceProperty -InstanceId $device.InstanceId `
                -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction SilentlyContinue).Data

        # The plain VID/PID form, which is what an INF matches on. The longer
        # revision-qualified id would only ever match one firmware revision.
        $id = @($hardware) |
            Where-Object { $_ -match '^USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}$' } |
            Select-Object -First 1
        if (-not $id) { continue }

        $id = $id.ToUpperInvariant()
        if (-not $found.ContainsKey($id)) {
            $found[$id] = [pscustomobject]@{
                HardwareId = $id
                Name       = $device.FriendlyName
                InstanceId = $device.InstanceId
                Status     = $device.Status
            }
        }
    }

    $found.Values | Sort-Object HardwareId
}

function Get-KnownIds {
    [regex]::Matches(
        (Get-Content -LiteralPath $InfPath -Raw),
        'USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}'
    ) | ForEach-Object { $_.Value.ToUpperInvariant() } | Sort-Object -Unique
}

function Add-Id {
    param([string] $Id, [string] $Name)

    $text = Get-Content -LiteralPath $InfPath -Raw
    if ($text -match [regex]::Escape($Id)) {
        Write-Host "  Already listed: $Id"
        return $false
    }

    # A token per adapter, so the INF stays readable and Device Manager shows a
    # name rather than a bare hardware id.
    $token = 'Device.' + ($Id -replace '[^0-9A-Za-z]', '')
    $label = if ([string]::IsNullOrWhiteSpace($Name)) { 'Bluetooth Controller' } else { $Name }
    $eol = [Environment]::NewLine

    $models = "%Device.BT600% = WinUsbInstall, USB\VID_0B05&PID_1D70"
    $strings = 'Device.BT600  = "OpenLEAudio Bluetooth Controller (WinUSB)"'

    if ($text -notmatch [regex]::Escape($models) -or $text -notmatch [regex]::Escape($strings)) {
        throw "The INF does not have the expected shape; not editing it blindly."
    }

    $text = $text.Replace($models, $models + $eol + "%$token% = WinUsbInstall, $Id")
    $text = $text.Replace(
        $strings,
        $strings + $eol + "$token = ""OpenLEAudio Bluetooth Controller (WinUSB) - $label""")

    Set-Content -LiteralPath $InfPath -Value $text -Encoding UTF8 -NoNewline
    Write-Host "  Added: $Id  ($label)" -ForegroundColor Green
    return $true
}

# ---------------------------------------------------------------------------

Start-Step "Looking for Bluetooth controllers on this machine" "usually a few seconds"

$present = @(Get-BluetoothControllers)
$known = @(Get-KnownIds)

Write-Host ""
if ($present.Count -eq 0) {
    Write-Host "  No USB Bluetooth controller is present." -ForegroundColor Yellow
    Write-Host "  Plug the adapter in and run this again. An adapter already bound to"
    Write-Host "  WinUSB still appears here, so this is safe to run at any time."
    Complete-Script "nothing was found and nothing was changed"
    return
}

Write-Host "  Bluetooth controllers present:"
foreach ($device in $present) {
    $mark = if ($known -contains $device.HardwareId) { 'supported ' } else { 'NOT LISTED' }
    Write-Host ("    [{0}] {1}  {2}" -f $mark, $device.HardwareId.PadRight(24), $device.Name)
}

$candidates = @($present | Where-Object { $known -notcontains $_.HardwareId })

if ($HardwareId) {
    $wanted = $HardwareId.ToUpperInvariant()
    $candidates = @($present | Where-Object { $_.HardwareId -eq $wanted })
    if ($candidates.Count -eq 0) {
        Write-Host ""
        Write-Host "  $HardwareId is not a Bluetooth controller present on this machine." -ForegroundColor Red
        Write-Host "  Only controllers Windows is enumerating right now can be added, so a"
        Write-Host "  mistyped id can never end up naming something else."
        Complete-Script "nothing was changed"
        return
    }
}

if ($candidates.Count -eq 0) {
    Write-Host ""
    Write-Host "  Every controller present is already supported." -ForegroundColor Green
    Complete-Script "nothing needed changing"
    return
}

$added = 0
foreach ($device in $candidates) {
    if (-not $All -and -not $HardwareId) {
        Write-Host ""
        Write-Host "  Add $($device.HardwareId)  -  $($device.Name)?"
        Write-Host "  This only makes it selectable. Nothing is switched over here."
        $answer = Read-Host "  [y/N]"
        if ($answer -notmatch '^(y|yes|a|ano)$') {
            Write-Host "  Skipped."
            continue
        }
    }
    if (Add-Id -Id $device.HardwareId -Name $device.Name) { $added++ }
}

if ($added -eq 0) {
    Complete-Script "nothing was added"
    return
}

if ($NoSign) {
    Write-Host ""
    Write-Host "  The catalog no longer matches the INF. Sign it before installing:" -ForegroundColor Yellow
    Write-Host "    1. SIGN driver.bat"
    Complete-Script "$added adapter(s) added - the package still needs signing"
    return
}

Start-Step "Signing the driver package again, because the INF changed"
& powershell -NoProfile -ExecutionPolicy Bypass -File $SignScript -Sign

Complete-Script "$added adapter(s) added. Pick one in Setup, then switch it over"
