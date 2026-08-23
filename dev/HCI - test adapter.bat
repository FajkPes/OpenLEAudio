@echo off
REM Low-level HCI test. Read-only.
REM The adapter must use WinUSB.
title OpenLEAudio - HCI test
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\hcitest.ps1"
echo.
pause
