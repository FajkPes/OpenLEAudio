<#
.SYNOPSIS
    Builds and tests the OpenLEAudio stack.

.DESCRIPTION
    Pins the Windows SDK to 10.0.26100.0 on purpose. A newer SDK (10.0.28000) may be
    installed alongside it, and vcvars picks the highest version by default - but that
    one ships without dbghelp.lib, which the Rust linker needs. Pinning avoids the
    LNK1181 failure without uninstalling anything.

.PARAMETER Test
    Runs the test suites instead of just building.
#>

param(
    [switch]$Test
)

$ErrorActionPreference = 'Stop'

$SdkVersion = '10.0.26100.0'
$Root = Split-Path $PSScriptRoot -Parent
$VcVars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
$CargoBin = "$env:USERPROFILE\.cargo\bin"

if (-not (Test-Path $VcVars)) {
    throw "vcvars64.bat not found at $VcVars - adjust the path for your Visual Studio install."
}

$action = if ($Test) { 'test' } else { 'build' }

Write-Host "=== Rust core ($action, SDK $SdkVersion) ===" -ForegroundColor Cyan
cmd /c "`"$VcVars`" $SdkVersion >nul 2>&1 && set PATH=$CargoBin;%PATH% && cd /d `"$Root\core`" && cargo $action 2>&1"
if ($LASTEXITCODE -ne 0) { throw "Rust $action failed" }

Write-Host ""
Write-Host "=== C HCI layer ===" -ForegroundColor Cyan
New-Item -ItemType Directory -Force "$Root\build" | Out-Null
cmd /c "`"$VcVars`" $SdkVersion >nul 2>&1 && cd /d `"$Root`" && cl /nologo /W4 /WX /Fe:build\hci_test.exe /Fo:build\ tests\hci_test.c src\hci\hci.c"
if ($LASTEXITCODE -ne 0) { throw "C build failed" }

if ($Test) {
    Write-Host ""
    & "$Root\build\hci_test.exe"
    if ($LASTEXITCODE -ne 0) { throw "C tests failed" }
}

Write-Host ""
Write-Host "All green." -ForegroundColor Green
