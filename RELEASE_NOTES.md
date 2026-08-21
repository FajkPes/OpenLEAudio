# OpenLEAudio 0.9 Beta

OpenLEAudio 0.9 Beta is the first public experimental release of a configurable user-mode Bluetooth LE Audio stack for Windows x64.

## Highlights

- Dedicated USB Bluetooth adapter control through Microsoft WinUSB
- Adapter detection on both the Windows Bluetooth driver and OpenLEAudio WinUSB driver
- Guided driver signing, binding, status, and restoration workflow
- Automatic dependency checks with clear recovery messages
- Optional automatic download of Microsoft runtimes and the official VB-CABLE package
- Extended LE discovery, pairing, encrypted reconnect, PACS, ASCS, and CIS support
- Configurable LC3 quality, frame duration, PHY, retransmissions, latency, and channel topology
- Stereo playback through VB-CABLE with optional headset microphone routing
- Three-second reconnect action
- Automatic startup reconnect attempts every five seconds for three minutes, enabled by default
- English interface by default with optional Czech UI translation

## Installation

1. Download `OpenLEAudio-0.9-beta-win-x64.zip` from this release.
2. Extract the entire ZIP to a writable directory.
3. Run `START OpenLEAudio.bat`.
4. Complete the four steps on the Setup page.

The launcher uses already installed Microsoft runtimes when available. Missing dependencies are identified before startup and can be downloaded from their official sources.

## Important beta notes

- This release is experimental and is not intended for safety-critical audio.
- Use a dedicated USB Bluetooth adapter when possible.
- Keep `RESTORE Windows Bluetooth driver.bat` available until restoration succeeds.
- Driver signing uses a local test certificate that must be renewed every two years.
- VB-CABLE installation may require a Windows restart before configuration.

## Included download

The release ZIP contains only the application, driver package, setup scripts, dependency cache placeholders, recovery-data placeholder, and user documentation. Development source, tests, captures, build output, private machine state, and optional installers are not included.
