@echo off
title OpenLEAudio - play audio
REM WARNING: this tool writes configuration to the headphones.
REM It configures ASCS, creates CIS channels, and starts audio transmission.
REM Volume starts at 10 percent to protect hearing.
REM Press Ctrl+C to stop.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Stream
echo.
pause
