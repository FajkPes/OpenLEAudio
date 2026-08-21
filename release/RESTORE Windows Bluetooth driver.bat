@echo off
title OpenLEAudio - restore Windows Bluetooth driver
REM Requests administrator rights and returns the adapter to the Windows stack.
REM Keep this recovery tool available until restoration succeeds.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0scripts\adapter-driver.ps1\"','-Restore'"
