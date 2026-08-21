@echo off
REM Configures VB-CABLE for 48000 Hz, 16-bit audio and makes it the default output.
REM Existing values are backed up and can be restored with "VB-CABLE - restore.bat".
title OpenLEAudio - configure VB-CABLE
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\setup-vbcable.ps1\"','-Apply'"
