# OpenLEAudio 0.9 Beta for Windows x64

This directory is the ready-to-run end-user package. It does not contain source code, tests, packet captures, or development tools.

## Start the application

Run `START OpenLEAudio.bat`. The launcher checks for:

- Microsoft .NET 8 Desktop Runtime x64
- Microsoft Windows App Runtime 1.8 x64

If either dependency is missing, the launcher offers to download the official Microsoft installer. Downloaded installers are cached in `dependencies`, so they can be reused or included in an offline package.

## First-time setup

Open the Setup page in the application and complete the steps in this order:

1. Sign the driver. The local signing certificate must be renewed every two years.
2. Install and configure VB-CABLE. Setup can download the official driver package, cache it in `dependencies`, and launch its installer.
3. Choose a dedicated USB Bluetooth adapter and switch it to the OpenLEAudio WinUSB stack.
4. Fully exit OpenLEAudio from its system tray icon, then start it again.

Adapter detection matches the hardware IDs in the signed INF. It therefore works both before the switch and after Windows exposes the adapter as a WinUSB or USBDevice device.

## Restore the Windows Bluetooth driver

If the custom stack does not work, run `RESTORE Windows Bluetooth driver.bat` as administrator. OpenLEAudio stores adapter and VB-CABLE recovery data in `runtime-data`.

## Package contents

- `OpenLEAudio/`: application and audio core
- `driver/`: WinUSB INF and signed catalog
- `scripts/`: the three administrative scripts used by Setup
- `dependencies/`: optional cache for Microsoft runtime installers
- `runtime-data/`: local recovery state, created or updated on the user's computer

This is experimental software. Do not use it for safety-critical audio. Keep the restore script available until the Windows driver has been restored successfully.
