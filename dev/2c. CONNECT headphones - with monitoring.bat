@echo off
title OpenLEAudio - connection with full monitoring
REM Prints every HCI and ACL packet in both directions.
REM Use this when exact traffic must be inspected.
REM Read-only. The tool does not write to the headphones.
cd /d "%~dp0"
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Extra "--debug"
echo.
pause
