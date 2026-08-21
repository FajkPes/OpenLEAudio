@echo off
REM Low-level HCI test. Read-only.
REM The adapter must use WinUSB.
title OpenLEAudio - HCI test
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\hcitest.ps1"
echo.
pause
