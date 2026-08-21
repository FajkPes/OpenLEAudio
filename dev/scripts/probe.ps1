<#
.SYNOPSIS
    Builds if needed and runs the OpenLEAudio probe.

.DESCRIPTION
    Read-only. Lists Bluetooth controllers and audio capture endpoints, and
    reports whether the virtual cable is present and at what sample rate.
    Writes nothing, to any device, ever.

    Exists because cargo is not on PATH and the linker needs the MSVC
    environment with the SDK pinned to 10.0.26100.0.
#>

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent
$SdkVersion = '10.0.26100.0'
$VcVars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
$CargoBin = "$env:USERPROFILE\.cargo\bin"

if (-not (Test-Path $VcVars)) {
    throw "vcvars64.bat not found at $VcVars"
}

cmd /c "`"$VcVars`" $SdkVersion >nul 2>&1 && set PATH=$CargoBin;%PATH% && cd /d `"$Root\core`" && cargo run --quiet --bin olea-probe"
