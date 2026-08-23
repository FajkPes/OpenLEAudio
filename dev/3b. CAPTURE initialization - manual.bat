@echo off
title OpenLEAudio - capture initialization manually
REM Fallback mode that waits for you to unplug and reconnect the adapter.
REM Use it when automatic capture did not record any packets.
REM The tool only monitors traffic and sends nothing to the device.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0scripts\capture-init.ps1\"','-Manual','-Seconds','25'"
echo.
pause
