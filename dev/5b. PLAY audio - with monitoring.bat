@echo off
title OpenLEAudio - play audio with monitoring
REM Works like "5. PLAY audio.bat" and also prints every packet.
REM WARNING: this tool writes configuration to the headphones.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\connect.ps1" -Stream -Extra "--debug"
echo.
pause
