@echo off
title OpenLEAudio - analyze capture
REM Extracts the HCI command sequence and firmware fragments from a capture.
REM No administrator rights are required. It only reads files in captures\.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\analyze-capture.ps1"
echo.
pause
