@echo off
REM Diagnostic inventory. Read-only.
title OpenLEAudio - diagnostics
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\probe.ps1"
echo.
pause
