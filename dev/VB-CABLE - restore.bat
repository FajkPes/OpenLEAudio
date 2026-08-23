@echo off
REM Restores the VB-CABLE state saved before configuration.
title OpenLEAudio - restore VB-CABLE
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\setup-vbcable.ps1\"','-Restore'"
echo.
pause
