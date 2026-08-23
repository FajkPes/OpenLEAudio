@echo off
title OpenLEAudio - switch adapter to OpenLEAudio
REM Requests administrator rights and switches the selected adapter to WinUSB.
REM The script asks for confirmation before changing anything.
echo.
echo   Working - this window stays open until the job is finished.
echo   Wait for the FINISHED banner before closing it.
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\adapter-driver.ps1\"','-Bind'"
echo.
pause
