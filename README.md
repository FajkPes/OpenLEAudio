# OpenLEAudio

OpenLEAudio is an experimental, configurable user-mode Bluetooth LE Audio stack for Windows. It takes control of one selected USB Bluetooth adapter through WinUSB while a second adapter can remain on the normal Windows Bluetooth stack.

The project is organized so end users do not need to download development files:

- `release/` contains the minimal ready-to-run Windows x64 package.
- `dev/` contains source code, tests, engineering notes, and development tools.
- `OpenLEAudio-0.9-beta-win-x64.zip` is the ready-to-download GitHub Release asset.

Most users should download the ZIP from [Releases](https://github.com/FajkPes/OpenLEAudio/releases), extract it, and run `START OpenLEAudio.bat`. The launcher detects missing Microsoft runtimes and offers to download their official installers.

Read [release/README.md](release/README.md) for installation and safety instructions. Development documentation starts at [dev/README-development.md](dev/README-development.md).

## Important status

Version 0.9 Beta is experimental. Use a dedicated USB Bluetooth adapter, keep a Windows-stack adapter available when possible, and use the included restore tool if you need to return the selected adapter to the Windows driver.

Made by FajkPes with a massive help from Claude Code and ChatGPT Codex.
