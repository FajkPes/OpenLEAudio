//! USB transport for a Bluetooth controller, per Core Specification Vol 4 Part B.
//!
//! A Bluetooth USB controller exposes a fixed layout on interface 0:
//!
//! | Channel      | Endpoint                                    |
//! |--------------|---------------------------------------------|
//! | HCI commands | control OUT, class request to the interface |
//! | HCI events   | interrupt IN                                |
//! | ACL data     | bulk IN / bulk OUT                          |
//!
//! Because that layout is standardised, this module works against any adapter
//! bound to WinUSB - there is no per-vendor code here and no firmware loading,
//! since the controller was already initialised by its original driver before
//! we took the interface.

use std::sync::Arc;

use nusb::DeviceInfo;

use crate::winusb::{PipeType, WinUsbInterface};

/// USB device class assigned to Bluetooth controllers.
const USB_CLASS_WIRELESS: u8 = 0xE0;
const USB_SUBCLASS_RF: u8 = 0x01;
const USB_PROTOCOL_BLUETOOTH: u8 = 0x01;

/// Interface 0 carries commands, events and ACL; the next one carries ISO, and
/// is reached as an associated interface rather than by number.
const HCI_INTERFACE: u8 = 0;

/// Largest HCI event is 2 header bytes plus a 255-byte payload.
const MAX_EVENT_LEN: usize = 257;

/// Largest ACL packet we are willing to receive in one transfer.
const MAX_ACL_LEN: usize = 1024 + 4;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("no Bluetooth controller bound to WinUSB was found")]
    NoDevice,

    #[error("controller found, but it is still owned by another driver - bind it to WinUSB first")]
    DeviceBusy,

    #[error("USB error: {0}")]
    Usb(String),

    #[error("packet too large: {0} bytes")]
    PacketTooLarge(usize),

    #[error("controller does not expose endpoint {0:#04x} in its descriptors")]
    MissingEndpoint(u8),

    #[error(
        "ISO endpoint {address:#04x} is isochronous on interface {interface}, \
         so it cannot be written as bulk - drive it through the winusb module"
    )]
    IsoNeedsWinUsb { address: u8, interface: u8 },
}

type Result<T> = std::result::Result<T, TransportError>;

/// A Bluetooth controller reachable over USB.
///
/// Cloneable so reader threads can own their own view of the same interface:
/// events arrive on interrupt IN while ACL data arrives on bulk IN, and both
/// reads block, so they cannot share one thread.
#[derive(Clone)]
pub struct UsbTransport {
    /// Shared rather than duplicated: WinUSB allows exactly one open handle per
    /// device, so every thread works through the same one. That constraint is
    /// also why this stack drives WinUSB directly instead of through nusb -
    /// audio needs the isochronous interface at the same time as commands and
    /// ACL, and two owners cannot both have it.
    interface: Arc<WinUsbInterface>,
    event_endpoint: u8,
    acl_in_endpoint: u8,
    acl_out_endpoint: u8,
    iso_out: Option<IsoEndpoint>,
    command_style: CommandStyle,
}

/// How the HCI command control transfer is addressed.
///
/// Core Specification Vol 4 Part B says request type 0x20 - class request to the
/// device. A capture of the Windows driver talking to this same adapter showed
/// it using 0x00 instead, and the stack originally used 0x21. A controller that
/// accepts the transfer but ignores a request it does not recognise looks
/// exactly like a controller with no firmware, so this is selectable and
/// `hcitest` reports which one the hardware actually answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStyle {
    /// 0x20 - class request to the device. What the specification requires.
    ClassDevice,
    /// 0x21 - class request to interface 0.
    ClassInterface,
    /// 0x00 - standard request to the device, as seen from the Windows driver.
    StandardDevice,
}

impl CommandStyle {
    /// Every variant, in the order worth trying.
    pub const ALL: [CommandStyle; 3] = [
        CommandStyle::ClassDevice,
        CommandStyle::ClassInterface,
        CommandStyle::StandardDevice,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CommandStyle::ClassDevice => "0x20 class/device (dle specifikace)",
            CommandStyle::ClassInterface => "0x21 class/interface",
            CommandStyle::StandardDevice => "0x00 standard/device (Windows-compatible)",
        }
    }

    pub fn from_setting(value: &str) -> Option<Self> {
        Some(match value {
            "class-device" => Self::ClassDevice,
            "class-interface" => Self::ClassInterface,
            "windows-standard" => Self::StandardDevice,
            _ => return None,
        })
    }

    /// The bmRequestType byte and wIndex this addressing produces.
    fn parts(self) -> (u8, u16) {
        match self {
            // Host to device, class request, recipient device.
            CommandStyle::ClassDevice => (0x20, 0),
            CommandStyle::ClassInterface => (0x21, HCI_INTERFACE as u16),
            CommandStyle::StandardDevice => (0x00, 0),
        }
    }
}

/// Where and how a controller wants audio written.
///
/// Read from the descriptors rather than assumed. On the ASUS BT600 the ISO OUT
/// endpoint turned out to be genuinely isochronous and to live on interface 1,
/// whose default alternate setting reserves no bandwidth at all - writing it as
/// bulk on interface 0 could only ever have gone nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoEndpoint {
    pub address: u8,
    pub interface: u8,
    pub alternate_setting: u8,
    pub max_packet_size: usize,
    pub isochronous: bool,
}

/// Summary of a candidate controller, used before deciding to open one.
#[derive(Debug, Clone)]
pub struct ControllerInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

impl UsbTransport {
    /// Lists every USB device that presents itself as a Bluetooth controller.
    ///
    /// Devices still owned by the Microsoft stack show up here too - they are
    /// only openable once bound to WinUSB.
    pub fn list_controllers() -> Result<Vec<(DeviceInfo, ControllerInfo)>> {
        let devices = nusb::list_devices().map_err(|e| TransportError::Usb(e.to_string()))?;

        let mut found = Vec::new();
        for info in devices {
            if !is_bluetooth_controller(&info) {
                continue;
            }

            let summary = ControllerInfo {
                vendor_id: info.vendor_id(),
                product_id: info.product_id(),
                manufacturer: info.manufacturer_string().map(str::to_owned),
                product: info.product_string().map(str::to_owned),
            };
            found.push((info, summary));
        }

        Ok(found)
    }

    /// Opens the first controller that WinUSB will actually hand over.
    pub fn open_first() -> Result<Self> {
        let candidates = Self::list_controllers()?;
        if candidates.is_empty() {
            return Err(TransportError::NoDevice);
        }

        let mut last_error = None;
        for (info, _) in candidates {
            match Self::open(&info) {
                Ok(transport) => return Ok(transport),
                Err(e) => last_error = Some(e),
            }
        }

        Err(last_error.unwrap_or(TransportError::DeviceBusy))
    }

    /// Opens the adapter and claims its HCI interface.
    ///
    /// `info` only decides *which* device; the handle itself comes from the
    /// WinUSB device interface our INF publishes, because that is the only way
    /// to reach the isochronous interface as well.
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        let _ = info;

        let path = crate::winusb::find_interface_path(crate::winusb::OLEA_INTERFACE_GUID)
            .map_err(|e| TransportError::Usb(e.to_string()))?;

        let interface =
            WinUsbInterface::open(&path, HCI_INTERFACE).map_err(|_| TransportError::DeviceBusy)?;

        // Events and ACL arrive in whatever size the controller has ready, which
        // is almost never the size we asked for.
        for pipe in [0x81u8, 0x82] {
            interface
                .allow_short_reads(pipe)
                .map_err(|e| TransportError::Usb(e.to_string()))?;
        }

        // The isochronous endpoints live on the next interface. Its absence is
        // not fatal here - only audio needs it, and the failure should surface
        // when audio starts rather than when the adapter opens.
        let iso_out = interface
            .associated(0)
            .ok()
            .and_then(|iso| {
                (0..8u8)
                    .filter_map(|setting| {
                        iso.pipes(setting)
                            .into_iter()
                            .find(|p| p.pipe_type == PipeType::Isochronous && !p.is_input())
                            .map(|p| (setting, p))
                    })
                    .max_by_key(|(_, pipe)| pipe.max_packet_size)
                    .map(|(setting, pipe)| IsoEndpoint {
                        address: pipe.id,
                        interface: iso.interface_number(),
                        alternate_setting: setting,
                        max_packet_size: pipe.max_packet_size as usize,
                        isochronous: true,
                    })
            });

        Ok(Self {
            interface: Arc::new(interface),
            event_endpoint: 0x81,
            acl_in_endpoint: 0x82,
            acl_out_endpoint: 0x02,
            iso_out,
            command_style: CommandStyle::ClassDevice,
        })
    }

    /// How this controller wants audio written, as read from its descriptors.
    pub fn iso_endpoint(&self) -> Option<IsoEndpoint> {
        self.iso_out
    }

    /// Opens the isochronous interface for audio.
    ///
    /// Reached through the same handle as everything else, because WinUSB only
    /// ever allows one.
    pub fn open_iso_sink(&self) -> Result<crate::winusb::IsoSink> {
        let iso = self
            .interface
            .associated(0)
            .map_err(|e| TransportError::Usb(e.to_string()))?;

        crate::winusb::IsoSink::from_interface(iso).map_err(|e| TransportError::Usb(e.to_string()))
    }

    /// Selects how HCI command control transfers are addressed.
    pub fn set_command_style(&mut self, style: CommandStyle) {
        self.command_style = style;
    }

    pub fn command_style(&self) -> CommandStyle {
        self.command_style
    }

    /// Sends an HCI command. The buffer is opcode, length, then parameters -
    /// exactly the layout the `hci` C module parses.
    pub fn send_command(&self, packet: &[u8]) -> Result<()> {
        if packet.len() > 255 + 3 {
            return Err(TransportError::PacketTooLarge(packet.len()));
        }

        crate::trace::packet(crate::trace::Wire::Command, packet);

        let (request_type, index) = self.command_style.parts();

        self.interface
            .control_out(request_type, 0x00, 0x0000, index, packet)
            .map_err(|e| TransportError::Usb(e.to_string()))
    }

    /// Waits for the next HCI event. Returns event code, then parameters.
    pub fn read_event(&self) -> Result<Vec<u8>> {
        let raw = self
            .interface
            .read_pipe(self.event_endpoint, MAX_EVENT_LEN)
            .map_err(|e| TransportError::Usb(e.to_string()))?;

        crate::trace::packet(crate::trace::Wire::Event, &raw);
        Ok(raw)
    }

    /// Sends an ACL data packet.
    pub fn send_acl(&self, packet: &[u8]) -> Result<()> {
        if packet.len() > MAX_ACL_LEN {
            return Err(TransportError::PacketTooLarge(packet.len()));
        }

        crate::trace::packet(crate::trace::Wire::AclOut, packet);

        self.interface
            .write_pipe(self.acl_out_endpoint, packet)
            .map_err(|e| TransportError::Usb(e.to_string()))
    }

    /// Sends an HCI ISO data packet - the encoded audio itself.
    ///
    /// Over the USB transport this goes out the **bulk** endpoint, the same one
    /// ACL uses, not the isochronous one. That looks wrong twice over - the
    /// packets are called isochronous and the adapter really does expose an
    /// isochronous endpoint - so it is worth writing down why.
    ///
    /// The isochronous endpoints carry SCO, which is why their alternate
    /// settings are the classic 9/17/25/33/49/63 byte SCO sizes. Connected
    /// Isochronous Streams arrived later and were given no endpoint of their
    /// own; the controller tells ISO from ACL by the connection handle in the
    /// packet header, which it assigned itself when it created the CIS. Linux
    /// btusb does exactly this: `HCI_ISODATA_PKT` is handed to `alloc_bulk_urb`
    /// like ACL, and only MediaTek parts, which define a private interrupt
    /// endpoint, deviate.
    ///
    /// Writing these packets to the isochronous pipe instead fails with
    /// `ERROR_INVALID_PARAMETER`, because an isochronous write has to be a
    /// multiple of the endpoint's packet size and a 108 byte ISO packet is not
    /// a multiple of 63.
    pub fn send_iso(&self, packet: &[u8]) -> Result<()> {
        if packet.len() > MAX_ACL_LEN {
            return Err(TransportError::PacketTooLarge(packet.len()));
        }

        crate::trace::packet(crate::trace::Wire::IsoOut, packet);

        self.interface
            .write_pipe(self.acl_out_endpoint, packet)
            .map_err(|e| TransportError::Usb(e.to_string()))
    }

    /// Wakes every blocked reader, so the threads holding this can finish.
    ///
    /// Called before letting the transport go. Without it the adapter stays
    /// open long after the session that owned it has been dropped.
    pub fn abort_reads(&self) {
        self.interface.abort_pipe(self.event_endpoint);
        self.interface.abort_pipe(self.acl_in_endpoint);
    }

    /// Waits for the next ACL data packet.
    pub fn read_acl(&self) -> Result<Vec<u8>> {
        let raw = self
            .interface
            .read_pipe(self.acl_in_endpoint, MAX_ACL_LEN)
            .map_err(|e| TransportError::Usb(e.to_string()))?;

        crate::trace::packet(crate::trace::Wire::AclIn, &raw);
        Ok(raw)
    }
}

/// True when the device advertises the standard Bluetooth controller triple.
fn is_bluetooth_controller(info: &DeviceInfo) -> bool {
    // Composite devices report the class per interface rather than per device.
    if info.class() == USB_CLASS_WIRELESS
        && info.subclass() == USB_SUBCLASS_RF
        && info.protocol() == USB_PROTOCOL_BLUETOOTH
    {
        return true;
    }

    info.interfaces().any(|i| {
        i.class() == USB_CLASS_WIRELESS
            && i.subclass() == USB_SUBCLASS_RF
            && i.protocol() == USB_PROTOCOL_BLUETOOTH
    })
}

