@echo off
chcp 65001 >nul
cd /d "%~dp0"
REM A build must not stop active audio or close the tray application automatically.
REM If either process is running, exit with instructions and leave it untouched.
powershell.exe -NoLogo -NoProfile -NonInteractive -Command ^
  "if (Get-Process -Name 'OpenLEAudio','OpenLEAudio.Client' -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }"
if not errorlevel 1 goto :running

REM Rust/MSVC needs the linker and a valid Windows SDK library path.
REM Visual Studio can select a preview SDK without the required x64 libraries.
REM Locate the newest complete SDK and activate it through vcvarsall.
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSROOT="
set "WINSDK="
if exist "%VSWHERE%" for /f "usebackq delims=" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%I"
for /d %%D in ("%ProgramFiles(x86)%\Windows Kits\10\Lib\10.*") do if exist "%%~fD\um\x64\DbgHelp.Lib" set "WINSDK=%%~nxD"
if defined VSROOT if defined WINSDK call "%VSROOT%\VC\Auxiliary\Build\vcvarsall.bat" amd64 %WINSDK% >nul

set "CARGO=cargo"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"

echo === core (OpenLEAudio Client) ===
"%CARGO%" build --release --manifest-path core\Cargo.toml --bin OpenLEAudio_Client || goto :fail
echo.
echo === application (WinUI 3) ===
dotnet build -c Release --no-incremental app\OpenLEAudio\OpenLEAudio.csproj || goto :fail
echo.
set "OUT=..\release\OpenLEAudio"
set "STAGE=..\release\.publish-staging"
if exist "%STAGE%" powershell.exe -NoProfile -NonInteractive -Command ^
  "$p=[IO.Path]::GetFullPath('%CD%\%STAGE%'); $release=[IO.Path]::GetFullPath('%CD%\..\release'); if (-not $p.StartsWith($release,[StringComparison]::OrdinalIgnoreCase)) { exit 9 }; Remove-Item -LiteralPath $p -Recurse -Force"
dotnet publish -c Release -p:Platform=x64 -o "%STAGE%" app\OpenLEAudio\OpenLEAudio.csproj || goto :fail
copy /Y core\target\release\OpenLEAudio_Client.exe "%STAGE%\OpenLEAudio.Client.exe" >nul
if errorlevel 1 goto :fail
powershell.exe -NoProfile -NonInteractive -Command ^
  "$target=[IO.Path]::GetFullPath('%CD%\%OUT%'); $stage=[IO.Path]::GetFullPath('%CD%\%STAGE%'); $release=[IO.Path]::GetFullPath('%CD%\..\release'); if (-not $target.StartsWith($release,[StringComparison]::OrdinalIgnoreCase) -or -not $stage.StartsWith($release,[StringComparison]::OrdinalIgnoreCase)) { exit 9 }; if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }; Move-Item -LiteralPath $stage -Destination $target"
if errorlevel 1 goto :fail
echo Build complete. Run "..\release\START OpenLEAudio.bat".
pause
exit /b 0
:running
echo.
echo OpenLEAudio is currently running. The build did not close it.
echo Exit through the system tray icon and run this BAT again.
echo A fresh release build will then be created from the current sources.
pause
exit /b 2
:fail
echo.
echo Build failed.
pause
exit /b 1
