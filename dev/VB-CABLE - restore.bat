@echo off
REM Restores the VB-CABLE state saved before configuration.
title OpenLEAudio - restore VB-CABLE
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-NoExit','-File','\"%~dp0..\release\scripts\setup-vbcable.ps1\"','-Restore'"
