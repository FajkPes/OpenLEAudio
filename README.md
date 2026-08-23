# OpenLEAudio

**A configurable Bluetooth LE Audio stack for Windows that runs in user space.**

OpenLEAudio takes control of one selected USB Bluetooth adapter through WinUSB
while a second adapter can remain on the normal Windows Bluetooth stack. It uses
VB-Cable as a kernel driver to deliver sound.

Tested for JBL TUNE 780NC LE Audio and Asus BT600 bluetooth adapter. My JBL's had
a problem with connection with standard Windows LE driver so I wanted to make my
own one and it worked. Hopefully it would also work with your LE Audio headphones.

[**Download the latest release**](https://github.com/FajkPes/OpenLEAudio/releases/latest)
 | [Release notes](RELEASE_NOTES.md)
 | [Install guide](release/README.md)
 | [Development](dev/README-development.md)

---

## What it does

| Feature | Details |
|---|---|
| **Real LE Audio** | LC3 codec, stereo over two isochronous channels, PACS / ASCS / CIS as the specification defines them |
| **Everything is adjustable** | Sample rate, frame duration, bitrate, PHY, retransmissions, latency and presentation delay. Not one fixed profile |
| **Automatic reconnect** | Retries while you are out of range and connects the moment you walk back in |
| **Battery and signal** | Headphone battery, connection uptime, signal strength, and real packet loss read from the radio |
| **Volume follows the earcups** | Press volume on the headphones and the Windows slider moves with it |
| **Headset microphone** | Optional, published to Windows through VB-CABLE |
| **Left/right balance** | Minus 50 to plus 50, applied live |
| **Power saving** | Stops transmitting during silence, and can let the headphones sleep between control messages |
| **Two languages** | English and Czech, plus importable JSON language packs |
| **Always reversible** | One click puts the adapter back on the Windows driver |

## Why it exists

The Windows LE Audio driver gives you no say in how the stream is built, and
when it will not connect it does not tell you why. This stack does both: every
parameter is on a settings page, and the console says what was sent, what came
back, and what the headphones asked for instead.

## Requirements

- Windows 10 (build 19041) or Windows 11, x64
- A dedicated USB Bluetooth adapter that supports LE Audio. An ASUS USB-BT600 is
  the reference; others can be added from the Setup page
- [VB-CABLE](https://vb-audio.com/Cable/), which the installer offers to fetch
- .NET 8, Windows App SDK and the Visual C++ runtime, all detected and offered
  for download if missing

## Getting started

1. Download the ZIP from [Releases](https://github.com/FajkPes/OpenLEAudio/releases/latest) and extract it somewhere writable.
2. Run `START OpenLEAudio.bat`.
3. Work through the four steps on the **Setup** page: sign the driver, pick the
   adapter, switch it over, configure VB-CABLE.
4. Go to **Devices**, search, and pair your headphones.

The launcher checks for missing Microsoft runtimes before anything starts and
offers to download the official installers.

## Handy tools

Beside the app, in the release folder:

| Script | What it does |
|---|---|
| `RESTORE Windows Bluetooth driver.bat` | Puts the adapter back on the Windows stack |
| `ADD Bluetooth adapter.bat` | Adds another Bluetooth adapter to the driver package |
| `FORGET paired devices.bat` | Clears stored pairings so the next connection pairs fresh |
| `INSTALL dependencies.bat` | Installs the Microsoft runtimes |

## How it is built

No kernel code of ours is loaded. WinUSB ships with Windows and is signed by
Microsoft, so Secure Boot stays on. Everything above the USB transport runs in
user space: HCI, L2CAP, ATT/GATT, SMP pairing, BAP signalling, LC3 and ISO
transport.

- **`release/`** is the ready-to-run Windows x64 package
- **`dev/core/`** is the stack itself, in Rust
- **`dev/app/`** is the interface, WinUI 3 and C#

Vendor-specific HCI commands are blocked, GATT writes are limited to handles
found during discovery, packet lengths are validated, and audio passes through a
limiter.

## Status

Version 1.0 is the first release considered ready for everyday use, and it still
changes a driver binding on your machine. Use a dedicated USB Bluetooth adapter,
keep a Windows-stack adapter available when possible, and use the included
restore tool if you need to return the selected adapter to the Windows driver.

It was developed and tested against one headset and one adapter. Support for
other hardware follows the Bluetooth specification and is covered by tests, but
has not been verified on other devices. Reports either way are welcome in
[Issues](https://github.com/FajkPes/OpenLEAudio/issues).

---

Made by FajkPes, Claude Code and Chat GPT Codex. Enjoy!
