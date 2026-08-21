# OpenLEAudio development guide

This directory contains source code, tests, packet captures, engineering documentation, and developer utilities. The distributable end-user package is generated in `../release`.

OpenLEAudio is an experimental configurable alternative to the Microsoft LE Audio path. It gives the user control over LC3 bitrate, sample rate, frame duration, presentation delay, retransmissions, radio PHY, microphone routing, and CIS topology.

## Architecture

Windows applications send audio to VB-CABLE. The OpenLEAudio client captures the cable output, encodes PCM audio as LC3, configures the headset through GATT PACS and ASCS, creates one or more CIS channels, and sends ISO data through a dedicated USB Bluetooth controller bound to WinUSB.

The stack runs in user mode. Its own code is not loaded into the Windows kernel. A selected adapter is taken over per device, which lets another Bluetooth adapter remain on the Windows stack for controllers and other peripherals.

## Main directories

- `app/OpenLEAudio`: WinUI 3 desktop application
- `core`: Rust Bluetooth LE Audio stack and command-line tools
- `src/hci`: small C HCI reference implementation
- `tests`: native HCI tests
- `scripts`: development, capture, and diagnostic scripts
- `docs`: architecture, plans, and reverse-engineering notes
- `tools/PacsProber`: PACS inspection utility
- `captures`: local USB and audio captures used during development

## Common entry points

- `BUILD application.bat`: builds the Rust core and WinUI application, then updates `../release/OpenLEAudio`
- `RUN tests.bat`: builds and runs automated tests
- `PROBE - diagnostics.bat`: reports adapters and audio devices without changing them
- `ADAPTER - driver status.bat`: reports the active driver for supported adapters
- `1. SIGN driver.bat`: creates or renews the local test certificate and signs the driver package
- `ADAPTER - switch to OpenLEAudio.bat`: binds the selected adapter to WinUSB
- `VB-CABLE - configure.bat`: saves the existing audio configuration and prepares VB-CABLE

## Safety rules

Vendor HCI commands are denied by default. The stack does not write controller firmware, eFuse, or permanent calibration data. Adapter and VB-CABLE changes have explicit restore paths in `../release`.

Audio streaming writes normal GATT and HCI configuration to the headphones. Start at a low volume and do not use the software for safety-critical audio.

## Build requirements

- Windows 10 or Windows 11 x64
- Visual Studio Build Tools with MSVC and a complete Windows SDK
- Rust stable toolchain
- .NET 8 SDK
- Windows App SDK 1.8 dependencies restored by NuGet

Run `BUILD application.bat` from this directory. If OpenLEAudio is running, exit it from the system tray first. The script intentionally refuses to terminate a running audio session.

## Release policy

Only the root files and `release` directory are intended for normal GitHub users. The release ZIP excludes source code, captures, build output, machine-specific recovery JSON, dependency installers, symbols, and unused satellite language resources.
