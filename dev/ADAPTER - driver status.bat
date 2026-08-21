@echo off
REM Reports the active Bluetooth adapter driver. Read-only.
title OpenLEAudio - adapter driver status
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\adapter-driver.ps1" -Status
echo.
pause
