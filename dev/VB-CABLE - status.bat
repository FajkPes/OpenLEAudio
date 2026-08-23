@echo off
REM Reports the current VB-CABLE configuration without changing it.
title OpenLEAudio - VB-CABLE status
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\release\scripts\setup-vbcable.ps1"
echo.
pause
