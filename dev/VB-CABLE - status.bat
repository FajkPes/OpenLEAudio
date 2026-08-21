@echo off
REM Reports the current VB-CABLE configuration without changing it.
title OpenLEAudio - VB-CABLE status
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\setup-vbcable.ps1"
echo.
pause
