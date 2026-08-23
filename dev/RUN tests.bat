@echo off
REM Builds and runs all automated tests.
title OpenLEAudio - build and test
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" -Test
echo.
pause
