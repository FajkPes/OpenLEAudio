@echo off
title OpenLEAudio - analyze capture
REM Extracts the HCI command sequence and firmware fragments from a capture.
REM No administrator rights are required. It only reads files in captures\.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\analyze-capture.ps1"
echo.
pause
