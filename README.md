# OpenLEAudio — legacy version

This is the original OpenLEAudio project. For the newer version, visit [OpenLEAudio 4](https://github.com/FajkPes/OpenLEAudio-v4).

OpenLEAudio was created to make Bluetooth LE Audio on Windows more configurable and to expose connection diagnostics. It runs a user-space audio stack using a dedicated USB Bluetooth adapter through WinUSB; another adapter can remain on the standard Windows stack.

## Features

- Configure audio and connection parameters, including codec, bitrate, PHY and latency.
- Inspect connection status and diagnostics, and use automatic reconnection.
- Use headset controls and available battery and signal information.
- Restore the dedicated adapter to the Windows driver.

## Download and setup

This repository contains the original 0.9 and 1.0 Windows releases. Download a ZIP from [Releases](https://github.com/FajkPes/OpenLEAudio/releases), extract it and run `START OpenLEAudio.bat`. The setup requires a compatible dedicated USB Bluetooth adapter and VB-CABLE. The reference hardware was a JBL TUNE 780NC and ASUS USB-BT600; compatibility with other devices is not guaranteed.

The old 0.9 and 1.0 downloads and their source tags are also preserved in [OpenLEAudio 4 releases](https://github.com/FajkPes/OpenLEAudio-v4/releases). See the [detailed original documentation](README-DETAILS.md) for installation and configuration.

## Credits

**Idea, concept and design:** FajkPes  
**Coding:** ChatGPT Codex, Claude and Google Antigravity
