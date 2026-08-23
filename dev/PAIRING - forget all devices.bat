@echo off
REM Forgets every device OpenLEAudio has paired with, so the next connection
REM pairs from scratch. Settings and the driver binding are not touched.
title OpenLEAudio - forget paired devices
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\forget-pairings.ps1"
echo.
pause
