<#
.SYNOPSIS
    Forgets every device OpenLEAudio has paired with.

.DESCRIPTION
    Deletes bonds.txt, the file that holds the long term keys agreed during
    pairing. Nothing else is touched: settings, language and the driver binding
    all stay as they are.

    This is the recovery for a headset that connects and then fails to encrypt.
    That happens when the headset has been paired to something else since - it
    has forgotten our key, so the one we saved is no longer the one it holds -
    and no amount of retrying fixes it, because both sides are certain and only
    one of them is right. Clearing the file makes the next connection pair from
    scratch.

    The app does this by itself when it detects a refused key. This script is for
    the case where that never gets far enough to run, and for starting clean.

.NOTES
    The headphones remember us too. If they still list this PC afterwards,
    remove it on the headset as well, or their side of the key exchange may be
    refused for the same reason ours was.
#>

[CmdletBinding()]
param(
    # Report what would be removed without removing it.
    [switch] $Status
)

$ErrorActionPreference = 'Stop'

$store = Join-Path $env:APPDATA 'OpenLEAudio\bonds.txt'

if (-not (Test-Path -LiteralPath $store)) {
    Write-Host "No paired devices are stored."
    Write-Host "  looked in: $store"
    exit 0
}

# Names only. The rest of each line is a key, and a key does not belong on
# screen, in a screenshot, or in a bug report.
$devices = @(
    Get-Content -LiteralPath $store |
        Where-Object { $_ -notmatch '^\s*(#|$)' } |
        ForEach-Object {
            $fields = $_ -split '\|'
            if ($fields.Count -ge 2) { "$($fields[1].Trim())  ($($fields[0].Trim()))" }
        }
)

Write-Host "Paired devices stored by OpenLEAudio:"
foreach ($device in $devices) { Write-Host "  - $device" }
Write-Host "  file: $store"
Write-Host ""

if ($Status) {
    Write-Host "Nothing was changed. Run without -Status to remove them."
    exit 0
}

$answer = Read-Host "Remove all $($devices.Count) pairing(s)? [y/N]"
if ($answer -notmatch '^(y|yes|a|ano)$') {
    Write-Host "Left alone."
    exit 0
}

# Kept rather than deleted outright: a long term key cannot be recovered, and
# somebody who ran this by mistake has a way back for as long as they notice.
$backup = "$store.removed"
Move-Item -LiteralPath $store -Destination $backup -Force

Write-Host ""
Write-Host "Removed. The next connection will pair from scratch."
Write-Host "  the old file is kept at: $backup"
Write-Host ""
Write-Host "Close OpenLEAudio first if it is running - it holds its own copy in"
Write-Host "memory and would write it back when it next saves."
Write-Host ""
Write-Host ("=" * 66)
Write-Host "  FINISHED - the stored pairings were removed" -ForegroundColor Green
Write-Host "  Nothing else is running. This window can be closed."
Write-Host ("=" * 66)
