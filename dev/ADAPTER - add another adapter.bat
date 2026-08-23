@echo off
REM Lists the Bluetooth controllers present on this machine and offers to add
REM the ones the OpenLEAudio driver package does not know yet. Adding one only
REM makes it selectable in Setup; nothing is switched over here.
title OpenLEAudio - add a Bluetooth adapter
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\add-adapter.ps1"
echo.
pause
