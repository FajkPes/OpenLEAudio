@echo off
REM Builds and runs all automated tests.
title OpenLEAudio - build and test
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" -Test
echo.
pause
