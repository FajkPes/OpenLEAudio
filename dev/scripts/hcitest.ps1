<#
.SYNOPSIS
    Runs the low-level HCI probe against the adapter.

.DESCRIPTION
    Read-only. Sends Reset, Read Local Version and Read BD_ADDR, and first
    works out how the adapter wants HCI command control transfers addressed.
    No vendor commands, no writes to any paired device.

    Requires the adapter to be on WinUSB. Run 'ADAPTER - switch to OpenLEAudio.bat'
    stack.bat' first.
#>

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent
$SdkVersion = '10.0.26100.0'
$VcVars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
$CargoBin = "$env:USERPROFILE\.cargo\bin"

if (-not (Test-Path $VcVars)) {
    throw "vcvars64.bat not found at $VcVars"
}

cmd /c "`"$VcVars`" $SdkVersion >nul 2>&1 && set PATH=$CargoBin;%PATH% && cd /d `"$Root\core`" && cargo run --quiet --bin olea-hcitest"
