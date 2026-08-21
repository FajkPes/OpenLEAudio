@echo off
chcp 65001 >nul
cd /d "%~dp0"
set "OUT=OpenLEAudio"

set "MISSING=0"
set "MISSING_DOTNET=0"
set "MISSING_WASDK=0"
where dotnet.exe >nul 2>nul
if errorlevel 1 (
    set "MISSING=1"
    set "MISSING_DOTNET=1"
) else (
    dotnet --list-runtimes 2>nul | findstr /B /C:"Microsoft.WindowsDesktop.App 8." >nul
    if errorlevel 1 (
        set "MISSING=1"
        set "MISSING_DOTNET=1"
    )
)

powershell.exe -NoProfile -NonInteractive -Command ^
  "if (Get-AppxPackage -Name 'Microsoft.WindowsAppRuntime.1.8*' -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }"
if errorlevel 1 (
    set "MISSING=1"
    set "MISSING_WASDK=1"
)

if "%MISSING%"=="1" (
    echo.
    echo OpenLEAudio cannot start because a required dependency is missing.
    if "%MISSING_DOTNET%"=="1" echo Missing: Microsoft .NET 8 Desktop Runtime x64
    if "%MISSING_WASDK%"=="1" echo Missing: Microsoft Windows App Runtime 1.8 x64
    echo The official Microsoft installers can be downloaded and installed automatically.
    echo.
    choice /M "Download and install dependencies"
    if errorlevel 2 exit /b 1
    call "%~dp0INSTALL dependencies.bat"
    if errorlevel 1 exit /b 1
)

if not exist "%OUT%\OpenLEAudio.exe" (
    echo OpenLEAudio.exe is missing. Download or rebuild the complete release package.
    pause
    exit /b 1
)
if not exist "%OUT%\OpenLEAudio.Client.exe" (
    echo OpenLEAudio.Client.exe is missing. Download or rebuild the complete release package.
    pause
    exit /b 1
)
if not exist "%OUT%\OpenLEAudio.pri" (
    echo OpenLEAudio.pri is missing. The user interface cannot load without this file.
    echo Download or rebuild the complete release package.
    pause
    exit /b 1
)
start "" "%OUT%\OpenLEAudio.exe"
timeout /t 5 /nobreak >nul
tasklist /FI "IMAGENAME eq OpenLEAudio.exe" 2>nul | find /I "OpenLEAudio.exe" >nul
if errorlevel 1 (
    echo.
    echo OpenLEAudio closed during startup.
    echo Check "%OUT%\logs\startup-error.log" for details.
    echo If no log exists, reinstall the dependencies or download the complete release package.
    pause
    exit /b 1
)
