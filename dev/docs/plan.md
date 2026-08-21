# OpenLEAudio development plan

## Completed for 0.9 Beta

- WinUSB transport for supported USB Bluetooth adapters
- Adapter discovery before and after switching away from the Windows Bluetooth driver
- Driver signing, binding, status, and restoration workflow
- Extended LE scanning and connection establishment
- SMP pairing, bond storage, and encrypted reconnect support
- GATT discovery for PACS, ASCS, and volume control
- LC3 capability parsing and configurable presets
- Stereo, legacy, and mono topology modes
- VB-CABLE setup, backup, status, and restoration
- WinUI setup, devices, settings, language, activity, and about pages
- English default UI with optional Czech localization
- Three-second reconnect action and startup reconnect window
- Minimal framework-dependent Windows x64 release package
- Smart dependency detection and optional installer download

## Beta validation priorities

1. Validate pairing and encrypted reconnect across more headset vendors.
2. Validate PACS and ASCS variants, especially multiple PAC records and unusual channel allocation rules.
3. Exercise one-CIS and two-CIS stereo topologies under packet loss.
4. Confirm presentation delay negotiation and retransmission limits.
5. Test microphone Source ASE operation without reducing playback stability.
6. Test driver restoration after interruption, reboot, and adapter replug.
7. Expand diagnostics while keeping normal UI messages concise.

## Later work

- More controller hardware IDs and compatibility profiles
- Better automatic recovery after radio or USB faults
- Signed public driver distribution strategy
- Installer or bootstrapper with native dependency diagnostics
- Repeatable hardware-in-the-loop regression tests
- Additional UI languages provided as isolated runtime localization packs

## Release criteria

A public beta build must compile from a clean tree, pass Rust and native tests, contain no machine-specific state, detect missing runtimes, provide a working Windows driver restore path, and keep all source documentation in English.
