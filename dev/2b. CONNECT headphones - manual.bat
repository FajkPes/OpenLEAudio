@echo off
title OpenLEAudio - manual connection
REM Scans nearby devices, waits for Enter, and only then connects.
REM This pause lets you start audio or prepare the headphones first.
REM Read-only. The tool does not write to the headphones.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Extra "--wait"
echo.
pause
