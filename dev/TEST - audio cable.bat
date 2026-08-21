@echo off
chcp 65001 >nul
cd /d "%~dp0"
title OpenLEAudio - audio cable test
echo.
echo  ================================================================
echo   This test does not use Bluetooth.
echo  ================================================================
echo.
echo   1) Set the Windows output device to "CABLE Input"
echo   2) Play music or a left and right channel test
echo   3) Open capture-test.wav in a normal player when recording finishes
echo.
echo   Recording starts after the next key press and lasts 10 seconds.
echo   Start the test audio first.
echo.
pause

if not exist "core\target\release\olea-captest.exe" (
    echo Building the capture tool. This may take a moment...
    cargo build --release --quiet --manifest-path core\Cargo.toml --bin olea-captest
)

if not exist "core\target\release\olea-captest.exe" (
    echo.
    echo The capture tool could not be built.
    pause
    exit /b 1
)

echo.
"core\target\release\olea-captest.exe"

echo.
echo  ================================================================
echo   Output files:
echo     %CD%\capture-test.wav
echo     %CD%\capture-test.txt
echo  ================================================================
echo.
pause
