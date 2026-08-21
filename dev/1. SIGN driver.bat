@echo off
title OpenLEAudio - sign driver
REM Creates a signing certificate and signs the INF so Windows can accept it.
REM Run once before switching the adapter for the first time.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\sign-driver.ps1\"','-Sign'"
