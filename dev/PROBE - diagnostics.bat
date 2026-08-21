@echo off
REM Diagnostic inventory. Read-only.
title OpenLEAudio - diagnostics
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\probe.ps1"
echo.
pause
