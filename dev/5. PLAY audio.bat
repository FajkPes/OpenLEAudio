@echo off
title OpenLEAudio - play audio
REM WARNING: this tool writes configuration to the headphones.
REM It configures ASCS, creates CIS channels, and starts audio transmission.
REM Volume starts at 10 percent to protect hearing.
REM Press Ctrl+C to stop.
cd /d "%~dp0"
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Stream
echo.
pause
