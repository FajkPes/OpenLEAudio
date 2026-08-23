# OpenLEAudio 1.0

First release considered ready for everyday use. The headline is connection
reliability: three separate faults made the stack look like broken headphone
firmware, and all three are fixed.

> Settings reset to their defaults on first run - several defaults changed.

## Connection

- **Fixed:** events that did not match what the stack was waiting for were
  thrown away, `Disconnection Complete` included - so a failure was reported as
  `unknown connection identifier` when the link had actually ended seconds
  earlier.
- **Fixed:** releasing the headphones during silence never serviced the control
  link, so the headset dropped the connection. That feature is now **off by
  default** as well.
- **Fixed:** the link timeout was hard-coded to 5 s, so walking out of range
  ended the connection instead of interrupting it. Now a setting, default 10 s.
- **Fixed:** the 1M radio usually failed to connect - the isochronous group did
  not fit and the error said nothing useful. It now retries with lighter
  settings and reports what it asked for.
- **Fixed:** automatic reconnect never actually ran (stale controller state, a
  setup failure that skipped the retry loop, a bond missing the address type).
- **Fixed:** a refused encryption key now re-pairs automatically instead of
  telling you to unpair by hand.
- **Fixed:** a queued connect no longer runs after you press Disconnect or
  Unpair.
- Six connection attempts with different parameters became one, driven by what
  the device publishes.

## Other headphones and adapters

- The QoS the headset publishes is now **used**, not just printed -
  retransmissions and transport latency, alongside presentation delay.
- Two isochronous streams are the default topology again; one stream is used
  only where a device has a single endpoint.
- **`ADD Bluetooth adapter.bat`** adds any Bluetooth controller in your machine
  to the driver package, reading the ID from Windows itself. No need to know a
  hardware ID.
- Setup now lists adapters that are present but unsupported, instead of showing
  an empty menu.

## Audio

- Packet loss is measured for real now, from the radio (`LE Read ISO Link
  Quality`) instead of counting USB submissions that were always zero.
- Dropouts fixed: a precise timer replaces `thread::sleep`, whose 15.6 ms
  granularity broke the 7.5 ms cadence. Underruns now send silence and keep the
  beat. **Latency did not increase.**
- The Robust preset could never play at all. Sample rate conversion added.

## New

- **Left/right balance**, minus 50 to +50.
- **Battery indicator** - click it to ask the headphones right now. Optional
  periodic read for headsets that never report by themselves.
- **Headphone power saving**: let the headphones sleep through control-channel
  wake-ups. No cost to audio quality or latency. Off by default.
- **Environment checks** - wrong driver, missing or unconfigured VB-CABLE,
  missing Visual C++ runtime - each with the button that fixes it.
- **Multipoint awareness** without vendor protocols: a headset busy with a
  phone is reported as busy.
- **`FORGET paired devices.bat`** clears stored pairings.
- Codec values are coloured against what your headphones actually published.

## Interface

- Device rows could render blank. Fixed.
- "Reconnecting" is now shown on the main page and in Settings.
- Every setting has a plain-language description behind a **?**, and a battery
  icon showing what it costs in air time.
- Switching the driver now prompts for the restart it needs.
- Console: **Levels off** hides only the band breakdown; **Debug off** no longer
  wipes the window; new rate control for the playing line.
- Setup marks a missing adapter red and a Windows-bound adapter amber.

## Setup scripts

- Every script says what it is doing before a long step and ends with a framed
  **FINISHED** banner - `pnputil` produces no output while it works, and the
  window used to look finished long before it was.

## Install

1. Download `OpenLEAudio-1.0-win-x64.zip` below.
2. Extract it somewhere writable.
3. Run `START OpenLEAudio.bat`.
4. Complete the four steps on the Setup page.

## Before you rely on it

- **Developed and tested on one combination** - a JBL Tune 780NC and an ASUS
  USB-BT600. Everything above for other hardware follows the Bluetooth
  specification and is covered by tests, but has not been verified on other
  devices. Reports either way are welcome.
- Binding an adapter to WinUSB takes it away from the Windows Bluetooth stack
  until you restore it. Not intended for safety-critical audio.
- Driver signing uses a local test certificate that must be renewed every two
  years.

---

# OpenLEAudio 0.9 Beta

OpenLEAudio 0.9 Beta is the first public experimental release of a configurable
user-mode Bluetooth LE Audio stack for Windows x64.

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

## Included download

The release ZIP contains only the application, driver package, setup scripts,
dependency cache placeholders, recovery-data placeholder, and user
documentation. Development source, tests, captures, build output, private
machine state, and optional installers are not included.
