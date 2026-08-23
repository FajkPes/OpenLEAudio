@echo off
REM Reports the active Bluetooth adapter driver. Read-only.
title OpenLEAudio - adapter driver status
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\adapter-driver.ps1" -Status
echo.
pause
