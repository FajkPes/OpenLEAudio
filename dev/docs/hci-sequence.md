# Windows driver HCI initialization reference

Source capture: `captures/usbpcap2-20260819-194949.pcap`, recorded on 2026-08-19 while reconnecting an ASUS BT600 adapter.

## Main finding

The controller responds to Read Local Version about 2 ms after USB connection. Normal initialization transfers only a few kilobytes of commands, so the Windows driver does not upload the large Realtek firmware container during each startup.

## Controller identity

The captured response reports a modern Bluetooth controller with an LMP generation above the minimum required by `supports_le_audio()`. The observed USB interface 0 endpoints match the transport assumptions used by OpenLEAudio.

The ISO endpoint is isochronous and belongs to interface 1. Its default alternate setting has no bandwidth. ISO traffic must therefore use the correct interface and alternate setting, not a bulk endpoint.

## Control transfer addressing

The Bluetooth specification uses a class request addressed to the device for HCI commands over USB. An earlier implementation addressed the interface instead. Some controllers accept that USB transfer but silently discard the command, which looks identical to missing firmware. The HCI diagnostic tool tests the safe addressing variants and reports which one receives a response.

## Scanning

The Windows sequence uses extended LE scanning and extended connection commands. Devices that advertise only through extended advertising do not produce legacy advertising reports, so relying on the old scan path can hide LE Audio headphones completely.

OpenLEAudio implements the extended commands and does not filter scan results to devices that advertise PACS before connection. Many headsets expose their full LE Audio services only after connecting.

## Vendor commands

The capture includes Realtek vendor-specific configuration commands. OpenLEAudio does not replay them. Vendor commands can access firmware, eFuse, calibration, and other permanent controller state, so `safety.rs` blocks them. The controller operates without replaying these commands.

## Connection sequence

The reference flow is:

1. Reset and read controller capabilities.
2. Configure LE host support and event masks.
3. Configure extended scanning.
4. Discover the target and create an extended LE connection.
5. Establish encryption before protected GATT operations.
6. Discover PACS and ASCS, then configure ASE and CIS state.

This document records behavior observed on one adapter and driver version. It is a debugging reference, not a requirement to copy undocumented vendor traffic.
