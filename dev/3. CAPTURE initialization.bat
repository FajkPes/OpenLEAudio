@echo off
title OpenLEAudio - capture adapter initialization
REM Captures traffic sent by the Realtek driver during adapter startup.
REM The tool only monitors traffic and sends nothing to the device.
REM Restore the adapter to its Windows driver before starting.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0scripts\capture-init.ps1\"'"
echo.
pause
