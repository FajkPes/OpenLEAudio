@echo off
title OpenLEAudio - restore Windows Bluetooth driver
REM Requests administrator rights and returns the adapter to the Windows stack.
REM Keep this recovery tool available until restoration succeeds.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0scripts\adapter-driver.ps1\"','-Restore'"
echo.
pause
