<#
.SYNOPSIS
    Connects to the headphones and reports what they can do.

.DESCRIPTION
    Read-only unless -Stream is given. Passes any extra arguments straight
    through to olea-connect, so LC3 settings work here too:

      .\connect.ps1 -Extra "--rate 48000 --frame 10 --octets 155"
#>

param(
    [switch]$Stream,
    [string]$Extra = ''
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent
$SdkVersion = '10.0.26100.0'
$VcVars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
$CargoBin = "$env:USERPROFILE\.cargo\bin"

$appArgs = $Extra
if ($Stream) { $appArgs = "$appArgs --stream" }

cmd /c "`"$VcVars`" $SdkVersion >nul 2>&1 && set PATH=$CargoBin;%PATH% && cd /d `"$Root\core`" && cargo run --quiet --bin olea-connect -- $appArgs"
