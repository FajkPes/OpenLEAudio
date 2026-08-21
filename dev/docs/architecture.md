# OpenLEAudio architecture

## Goal

OpenLEAudio provides a configurable Windows LE Audio path without depending on the private Microsoft LE Audio implementation. The project controls a dedicated USB Bluetooth adapter through WinUSB and implements the required host-side Bluetooth layers in user mode.

## Data path

1. A Windows application renders PCM audio to VB-CABLE.
2. The OpenLEAudio client captures the virtual cable output.
3. The audio module converts and buffers PCM frames.
4. The LC3 encoder produces one frame per configured audio channel.
5. BAP and ASCS configure the headset sink ASEs.
6. The session creates a CIG and one or more CIS links.
7. ISO packets are sent through the WinUSB transport and the selected controller.

For microphone audio, the reverse path receives ISO SDUs, decodes LC3, and writes PCM to the selected capture or monitoring target.

## Components

- `app/OpenLEAudio`: WinUI settings, device discovery, setup, status, localization, and tray behavior
- `core/src/bin/agent.rs`: JSON command and event boundary between UI and core
- `core/src/transport.rs` and `winusb.rs`: controller transport
- `core/src/hci.rs`, `controller.rs`, and `link.rs`: HCI lifecycle, scanning, ACL, and connection management
- `core/src/att.rs`, `l2cap.rs`, `smp.rs`, and `bonding.rs`: GATT transport, pairing, encryption, and bond storage
- `core/src/bap.rs`, `session.rs`, and `stream.rs`: PACS, ASCS, CIS setup, and ISO streaming
- `core/src/audio.rs`: capture, conversion, monitoring, and LC3 framing
- `core/src/safety.rs`: allowlist and denylist for controller commands

## Adapter ownership

The Setup page identifies supported adapters from hardware IDs in `release/driver/olea_winusb.inf`. The same device is recognized before binding as a Bluetooth-class device and after binding as a WinUSB USBDevice node. SetupAPI enumeration is used so detection does not depend on the currently installed driver class.

Binding is per physical adapter. Recovery information is stored under `release/runtime-data` and is never included in the public release ZIP.

## Safety boundary

The custom code remains in user mode. The Microsoft-signed WinUSB driver handles kernel transport. Vendor commands associated with firmware, eFuse, calibration, or permanent controller changes are rejected. Normal Bluetooth configuration is transient and is lost when the controller resets.

## Configuration

Settings are stored as stable keys and exchanged with the UI as JSON. English is the canonical language for keys, logs, source comments, and documentation. The UI also contains an optional Czech localization pack selected at runtime.
