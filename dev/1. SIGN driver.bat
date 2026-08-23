@echo off
title OpenLEAudio - sign driver
REM Creates a signing certificate and signs the INF so Windows can accept it.
REM Run once before switching the adapter for the first time.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\sign-driver.ps1\"','-Sign'"
echo.
pause
