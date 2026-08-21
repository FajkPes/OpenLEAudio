@echo off
title OpenLEAudio - connect and inspect headphones
REM Connects to headphones and reports their supported LC3 configurations.
REM Read-only. The tool does not write to the headphones.
REM Put the headphones in pairing mode before starting.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1"
echo.
pause
