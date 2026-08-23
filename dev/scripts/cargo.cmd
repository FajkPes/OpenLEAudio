@echo off
call "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat" 10.0.26100.0 >nul 2>&1
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
cd /d "%~dp0..\core"
cargo %*
