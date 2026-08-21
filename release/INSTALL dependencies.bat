@echo off
chcp 65001 >nul
setlocal
title OpenLEAudio - install dependencies

set "SETUPDIR=%~dp0dependencies"
set "DOTNET=%SETUPDIR%\windowsdesktop-runtime-8-x64.exe"
set "WASDK=%SETUPDIR%\windows-app-runtime-1.8-x64.exe"
if not exist "%SETUPDIR%" mkdir "%SETUPDIR%"

set "NEEDDOTNET=0"
set "NEEDWASDK=0"
where dotnet.exe >nul 2>nul
if errorlevel 1 (
    set "NEEDDOTNET=1"
) else (
    dotnet --list-runtimes 2>nul | findstr /B /C:"Microsoft.WindowsDesktop.App 8." >nul
    if errorlevel 1 set "NEEDDOTNET=1"
)
powershell.exe -NoProfile -NonInteractive -Command ^
  "if (Get-AppxPackage -Name 'Microsoft.WindowsAppRuntime.1.8*' -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }"
if errorlevel 1 set "NEEDWASDK=1"

if "%NEEDDOTNET%"=="0" if "%NEEDWASDK%"=="0" (
    echo All required dependencies are already installed.
    exit /b 0
)

echo.
if "%NEEDDOTNET%"=="1" if not exist "%DOTNET%" (
    echo Downloading Microsoft .NET 8 Desktop Runtime...
    powershell.exe -NoProfile -ExecutionPolicy Bypass -Command ^
      "Invoke-WebRequest -UseBasicParsing -Uri 'https://aka.ms/dotnet/8.0/windowsdesktop-runtime-win-x64.exe' -OutFile '%DOTNET%'"
    if errorlevel 1 goto :fail
) else (
    echo Using the cached .NET 8 installer from the dependencies directory.
)

if "%NEEDWASDK%"=="1" if not exist "%WASDK%" (
    echo Downloading Microsoft Windows App Runtime 1.8...
    powershell.exe -NoProfile -ExecutionPolicy Bypass -Command ^
      "Invoke-WebRequest -UseBasicParsing -Uri 'https://aka.ms/windowsappsdk/1.8/latest/windowsappruntimeinstall-x64.exe' -OutFile '%WASDK%'"
    if errorlevel 1 goto :fail
) else (
    echo Using the cached Windows App Runtime installer from the dependencies directory.
)

if "%NEEDDOTNET%"=="1" (
    echo Installing .NET 8 Desktop Runtime...
    start /wait "" "%DOTNET%" /install /quiet /norestart
    if errorlevel 1 goto :fail
)

if "%NEEDWASDK%"=="1" (
    echo Installing Windows App Runtime 1.8...
    start /wait "" "%WASDK%" --quiet
    if errorlevel 1 goto :fail
)

echo.
echo Dependencies are installed. OpenLEAudio is ready to start.
exit /b 0

:fail
echo.
echo Installation failed. Check the internet connection and administrator permissions.
pause
exit /b 1
