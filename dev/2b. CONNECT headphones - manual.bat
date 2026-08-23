@echo off
title OpenLEAudio - manual connection
REM Scans nearby devices, waits for Enter, and only then connects.
REM This pause lets you start audio or prepare the headphones first.
REM Read-only. The tool does not write to the headphones.
cd /d "%~dp0"
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Extra "--wait"
echo.
pause
