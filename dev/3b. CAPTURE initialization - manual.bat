@echo off
title OpenLEAudio - capture initialization manually
REM Fallback mode that waits for you to unplug and reconnect the adapter.
REM Use it when automatic capture did not record any packets.
REM The tool only monitors traffic and sends nothing to the device.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0scripts\capture-init.ps1\"','-Manual','-Seconds','25'"
