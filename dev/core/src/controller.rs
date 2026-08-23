//! Controller session: the command/event loop every other layer sits on.
//!
//! Owns the transport, sends HCI commands, waits for their completion, and keeps
//! unrelated events in a queue so nothing is lost while a command is in flight.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::hci::{self, subevt, BdAddr, Event, LocalVersion};
use crate::link::HciPump;
use crate::transport::{CommandStyle, TransportError, UsbTransport};

/// How long to wait for a controller to answer a command.
///
/// A controller that is powered but has no firmware loaded enumerates fine and
/// then says nothing at all, so every read needs a deadline. Without one the
/// stack hangs instead of reporting the problem.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error("controller rejected command {opcode:#06x} with status {status:#04x} ({})", status_name(*status))]
    CommandFailed { opcode: u16, status: u8 },

    #[error("no response to command {0:#06x}")]
    NoResponse(u16),

    #[error("controller reports Bluetooth {version}, but LE Audio needs 5.2 or newer")]
    LeAudioUnsupported { version: &'static str },

    #[error("malformed event from controller")]
    MalformedEvent,

    #[error(transparent)]
    Unsafe(#[from] crate::safety::SafetyViolation),

    #[error("controller did not send anything in time - is firmware loaded?")]
    EventTimeout,
}

type Result<T> = std::result::Result<T, ControllerError>;

/// Human-readable name for the common HCI error codes.
pub fn status_name(status: u8) -> &'static str {
    match status {
        0x00 => "success",
        0x01 => "unknown command",
        0x02 => "unknown connection identifier",
        0x0C => "command disallowed",
        0x11 => "unsupported feature or parameter",
        0x12 => "invalid command parameters",
        0x1F => "unspecified error",
        0x3E => "connection failed to be established",
        0x42 => "unacceptable connection parameters",
        _ => "see Core spec Part D",
    }
}

/// One device seen while scanning.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub address: BdAddr,
    pub address_type: u8,
    pub rssi: i8,
    pub name: Option<String>,
    pub appearance: Option<u16>,
    pub service_uuids: Vec<u16>,
}

impl DiscoveredDevice {
    /// A name to show a person, falling back to the address.
    ///
    /// Plenty of devices never put a name in their advertisement, and a blank
    /// row in a device list is worse than a hexadecimal one.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.address.to_string())
    }

    /// True when the advertisement carries an LE Audio service, which is how a
    /// device announces it can do BAP rather than plain BLE.
    pub fn is_le_audio(&self) -> bool {
        const PACS: u16 = 0x1850;
        const ASCS: u16 = 0x184E;
        const CAS: u16 = 0x1853;
        const TMAS: u16 = 0x1855;

        self.service_uuids
            .iter()
            .any(|&u| matches!(u, PACS | ASCS | CAS | TMAS))
    }
}

pub struct Controller {
    transport: UsbTransport,
    /// Shared, never duplicated: a second pump on the same adapter would race
    /// this one for every incoming packet. `Link` takes a handle to this.
    pump: Rc<HciPump>,
    queued_events: VecDeque<Event>,
    pub local_version: Option<LocalVersion>,
    pub local_address: Option<BdAddr>,
    /// Whether the last scan used the extended commands, which decides how a
    /// connection must be requested afterwards.
    extended_scan: bool,

    /// How long the link may go unheard before the controller declares it lost,
    /// in units of 10 ms.
    ///
    /// Five seconds is what the Windows driver asks for, and it is also why
    /// stepping out of range for a moment ends the connection rather than
    /// interrupting it: the ACL is gone before the headphones are back. The user
    /// gets to choose, because the cost of a longer value is only that a link
    /// that really has died takes proportionally longer to be noticed.
    supervision_timeout: u16,
}

impl Controller {
    /// Opens the first adapter bound to WinUSB.
    pub fn open() -> Result<Self> {
        Self::open_with_command_style(CommandStyle::ClassDevice)
    }

    pub fn open_with_command_style(style: CommandStyle) -> Result<Self> {
        let mut transport = UsbTransport::open_first()?;
        transport.set_command_style(style);
        let pump = Rc::new(HciPump::start(transport.clone()));

        Ok(Self {
            transport,
            pump,
            queued_events: VecDeque::new(),
            local_version: None,
            local_address: None,
            extended_scan: false,
            supervision_timeout: 0x03E8, // 10 s
        })
    }

    /// Brings the controller up and confirms it can do LE Audio.
    ///
    /// The adapter was already initialised by its previous driver, so this is a
    /// clean reset rather than a firmware load - which is why no vendor-specific
    /// code is needed here.
    pub fn initialize(&mut self) -> Result<()> {
        self.command(&hci::reset())?;

        let version_params = self.command(&hci::read_local_version())?;
        let version = LocalVersion::parse(&version_params).ok_or(ControllerError::MalformedEvent)?;

        if !version.supports_le_audio() {
            return Err(ControllerError::LeAudioUnsupported {
                version: version.bluetooth_version(),
            });
        }
        self.local_version = Some(version);

        let address_params = self.command(&hci::read_bd_addr())?;
        if address_params.len() >= 7 {
            let mut bytes = [0u8; 6];
            bytes.copy_from_slice(&address_params[1..7]);
            self.local_address = Some(BdAddr(bytes));
        }

        self.command(&hci::set_event_mask())?;
        self.command(&hci::le_set_event_mask())?;

        // Both of these are optional in the sense that the controller answers
        // without them, and neither is optional in practice: the Windows driver
        // sends both on this same adapter before it ever connects, and CIS
        // cannot be created until the host feature bit is set. A controller that
        // does not know either command is not one this stack can drive anyway,
        // so a rejection is worth reporting rather than swallowing.
        const CONNECTED_ISOCHRONOUS_STREAMS: u8 = 32;
        self.command(&hci::write_le_host_supported())?;
        self.command(&hci::le_set_host_feature(CONNECTED_ISOCHRONOUS_STREAMS, true))?;

        Ok(())
    }

    /// Sends a command and returns its return parameters, minus the status byte
    /// check which is done here.
    pub fn command(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        if packet.len() < 2 {
            return Err(ControllerError::MalformedEvent);
        }

        // Enforced here rather than at the call site, so there is no path to the
        // controller that skips it. Vendor-specific opcodes are where firmware
        // flashing lives, and nothing in this stack has a reason to send one.
        crate::safety::check_hci_command(packet).map_err(ControllerError::Unsafe)?;

        let opcode = u16::from_le_bytes([packet[0], packet[1]]);
        self.transport.send_command(packet)?;

        // Read events until this command is acknowledged. Anything else is queued
        // so an advertising report arriving mid-command is not thrown away.
        //
        // The only limit is the deadline. An earlier version also gave up after
        // 64 events, which looked like a harmless guard and was not: every ACL
        // packet the host sends comes back as a Number Of Completed Packets
        // event, so a burst of GATT traffic - service discovery, subscribing,
        // reading characteristics - leaves dozens of them queued ahead of the
        // answer we are waiting for. The command then failed with "no response"
        // while the response was sitting a few events further down the queue.
        // It only ever showed up on the busiest path, which is the worst kind of
        // bug to own: absent from every simple test.
        let deadline = Instant::now() + COMMAND_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ControllerError::NoResponse(opcode));
            }

            let event = match self.pump.recv_event(remaining) {
                Ok(event) => event,
                Err(_) => return Err(ControllerError::NoResponse(opcode)),
            };

            if let Some((completed, params)) = event.command_complete() {
                if completed == opcode {
                    return match params.first() {
                        Some(&0x00) | None => Ok(params.to_vec()),
                        Some(&status) => Err(ControllerError::CommandFailed { opcode, status }),
                    };
                }
            }

            if let Some((status, pending)) = event.command_status() {
                if pending == opcode {
                    return if status == 0x00 {
                        Ok(Vec::new())
                    } else {
                        Err(ControllerError::CommandFailed { opcode, status })
                    };
                }
            }

            self.queued_events.push_back(event);
        }
    }

    /// Next event, preferring ones queued while a command was in flight.
    pub fn next_event(&mut self) -> Result<Event> {
        self.next_event_timeout(COMMAND_TIMEOUT)
    }

    /// Next event, or an error once the deadline passes.
    pub fn next_event_timeout(&mut self, timeout: Duration) -> Result<Event> {
        if let Some(event) = self.queued_events.pop_front() {
            return Ok(event);
        }

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ControllerError::EventTimeout);
            }

            match self.pump.recv_event(remaining) {
                Ok(event) => return Ok(event),
                Err(_) => return Err(ControllerError::EventTimeout),
            }
        }
    }

    /// Scans for advertising devices and reports each one as it is seen.
    ///
    /// Blocks until `duration` elapses. Because the check happens between events,
    /// a completely silent radio can overshoot - acceptable while scanning, since
    /// advertisements are near-continuous in practice.
    pub fn scan<F>(&mut self, duration: Duration, mut on_device: F) -> Result<()>
    where
        F: FnMut(&DiscoveredDevice),
    {
        // 10 ms interval, 10 ms window in 0.625 ms units: scan continuously.
        //
        // Extended scanning first. A device advertising in the extended form
        // produces no legacy report whatsoever, so a legacy-only scan simply
        // does not see it - and LE Audio headphones commonly advertise that
        // way. Older controllers reject the extended commands, so the legacy
        // pair remains as a fallback rather than a preference.
        self.extended_scan = self
            .command(&hci::le_set_extended_scan_parameters(0x0010, 0x0010))
            .and_then(|_| self.command(&hci::le_set_extended_scan_enable(true, false)))
            .is_ok();

        if !self.extended_scan {
            self.command(&hci::le_set_scan_parameters(0x0010, 0x0010))?;
            self.command(&hci::le_set_scan_enable(true, false))?;
        }

        let deadline = Instant::now() + duration;
        let result = self.collect_advertisements(deadline, &mut on_device);

        // Always stop scanning, even if collection failed.
        let stop = if self.extended_scan {
            self.command(&hci::le_set_extended_scan_enable(false, false))
        } else {
            self.command(&hci::le_set_scan_enable(false, false))
        };
        result.and(stop.map(|_| ()))
    }

    /// True when the last scan used the extended commands.
    /// Sets the link supervision timeout used by the next connection attempt.
    pub fn set_supervision_timeout(&mut self, timeout: Duration) {
        let units = (timeout.as_millis() / 10).clamp(1, u16::MAX as u128) as u16;
        self.supervision_timeout = hci::clamp_supervision(units);
    }

    pub fn used_extended_scan(&self) -> bool {
        self.extended_scan
    }

    fn collect_advertisements<F>(&mut self, deadline: Instant, on_device: &mut F) -> Result<()>
    where
        F: FnMut(&DiscoveredDevice),
    {
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());

            let event = match self.next_event_timeout(remaining) {
                Ok(event) => event,
                // A quiet radio during a scan is normal, not a failure.
                Err(ControllerError::EventTimeout) => break,
                Err(e) => return Err(e),
            };

            let devices = match event.subevent() {
                Some(subevt::ADVERTISING_REPORT) => parse_advertising_reports(&event.params),
                Some(subevt::EXTENDED_ADVERTISING_REPORT) => {
                    parse_extended_advertising_reports(&event.params)
                }
                _ => continue,
            };

            for device in devices {
                on_device(&device);
            }
        }

        Ok(())
    }

    /// Waits for an event matching a predicate, ignoring everything else.
    ///
    /// Events that do not match are kept, except the two kinds that arrive in
    /// bulk and mean nothing later: advertising reports and completed-packet
    /// counts. Everything else goes back in the queue.
    ///
    /// This used to discard all of them, and the one that mattered was
    /// Disconnection Complete. Waiting up to eight seconds for an isochronous
    /// channel that never comes up is exactly when the peer is most likely to
    /// drop the ACL link - and the notice that it had was thrown away here. The
    /// stack then retried against a connection handle the controller no longer
    /// knew, and LE Create CIS answered "unknown connection identifier", which
    /// reads as the isochronous channels being refused rather than as the
    /// connection having ended several seconds earlier.
    pub fn wait_for_event<F>(&mut self, timeout: Duration, mut matches: F) -> Result<Option<Event>>
    where
        F: FnMut(&Event) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut keep = Vec::new();

        let found = loop {
            if Instant::now() >= deadline {
                break None;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.next_event_timeout(remaining) {
                Ok(event) if matches(&event) => break Some(event),
                Ok(event) => {
                    if !is_bulk_event(&event) {
                        keep.push(event);
                    }
                }
                Err(ControllerError::EventTimeout) => break None,
                Err(e) => {
                    // Put back what was collected before giving up, so an error
                    // here does not also lose a disconnection nobody has read.
                    for event in keep.into_iter().rev() {
                        self.queued_events.push_front(event);
                    }
                    return Err(e);
                }
            }
        };

        for event in keep.into_iter().rev() {
            self.queued_events.push_front(event);
        }

        Ok(found)
    }

    /// Opens a connection to a peer and returns its handle.
    ///
    /// Cancels the attempt on timeout rather than leaving the controller trying
    /// forever, which would make every later command fail with "command disallowed".
    pub fn connect(
        &mut self,
        peer: BdAddr,
        address_type: u8,
        timeout: Duration,
    ) -> Result<Option<u16>> {
        // An abandoned attempt answers late: its Connection Complete arrives
        // after the timeout has already passed, and would then be read as the
        // answer to the next attempt, failing it instantly. Start from silence.
        self.clear_queued_events();
        while self.pump.try_recv_event().is_some() {}

        // Match the generation used for scanning. A controller is entitled to
        // refuse a legacy connection request after extended scanning, and this
        // one is a Bluetooth 6.0 part where that is a real possibility.
        let request = if self.extended_scan {
            hci::le_extended_create_connection(peer, address_type, self.supervision_timeout)
        } else {
            hci::le_create_connection(peer, address_type, self.supervision_timeout)
        };
        self.command(&request)?;

        // Only this peer's answer counts. A connection completing for some other
        // device would otherwise be taken as ours.
        let event = self.wait_for_event(timeout, |e| {
            matches!(e.connection_result(), Some((_, _, address)) if address == peer)
        })?;

        match event.and_then(|e| e.connection_result()) {
            Some((0x00, handle, _)) => Ok(Some(handle)),
            Some((status, _, _)) => {
                // The controller said why. Passing that on is the difference
                // between a fixable report and "did not complete".
                Err(ControllerError::CommandFailed {
                    opcode: hci::op::LE_EXTENDED_CREATE_CONNECTION,
                    status,
                })
            }
            None => {
                // Nothing arrived in time. The controller is still trying, and
                // it stays in the initiating state until it is told otherwise -
                // every later LE Create Connection is then answered with
                // "command disallowed".
                //
                // Sending the cancel is not enough on its own. The cancel is
                // acknowledged immediately, but the attempt it aborts reports
                // separately afterwards, as a Connection Complete carrying
                // status 0x02. Returning before that arrives leaves the event in
                // the queue for the *next* attempt to find, where it is read as
                // that attempt's answer and fails it instantly - so a retry
                // could never succeed, however long the peer had been back in
                // range. This is why reconnecting by hand worked (the gap
                // between two clicks is long enough for the event to be
                // discarded by the next attempt's initial flush) while a prompt
                // automatic retry did not.
                let _ = self.command(&hci::le_create_connection_cancel());
                let _ = self.wait_for_event(Duration::from_secs(2), |e| {
                    e.connection_result().is_some()
                });
                self.clear_queued_events();
                while self.pump.try_recv_event().is_some() {}
                Ok(None)
            }
        }
    }

    /// Starts encryption with a key from pairing, and waits for confirmation.
    pub fn enable_encryption(
        &mut self,
        handle: u16,
        long_term_key: &[u8; 16],
        timeout: Duration,
    ) -> Result<bool> {
        self.command(&hci::le_enable_encryption(handle, long_term_key))?;

        let event = self.wait_for_event(timeout, |e| {
            hci::parse_encryption_change(e).map(|(h, _)| h) == Some(handle)
        })?;

        Ok(event
            .and_then(|e| hci::parse_encryption_change(&e))
            .map(|(_, enabled)| enabled)
            .unwrap_or(false))
    }

    /// The underlying transport, for layers that need their own reader.
    pub fn transport(&self) -> &UsbTransport {
        &self.transport
    }

    /// A handle to the one pump reading this adapter.
    ///
    /// `Link` shares it rather than starting its own, so that ACL data and
    /// events cannot be stolen from each other by a second set of reader
    /// threads on the same endpoints.
    pub fn pump(&self) -> Rc<HciPump> {
        Rc::clone(&self.pump)
    }

    /// Drains queued events, used when a caller wants a clean slate.
    pub fn clear_queued_events(&mut self) {
        self.queued_events.clear();
    }
}

/// Events that arrive continuously and carry nothing worth keeping.
///
/// Advertising reports appear by the hundred while scanning and every one of
/// them is stale a moment later. Completed-packet counts are produced by every
/// packet the host sends. Queueing either would grow without bound.
fn is_bulk_event(event: &Event) -> bool {
    if !crate::hci::parse_number_of_completed_packets(event).is_empty() {
        return true;
    }

    matches!(
        event.subevent(),
        Some(hci::subevt::ADVERTISING_REPORT) | Some(hci::subevt::EXTENDED_ADVERTISING_REPORT)
    )
}

/// Parses an LE Extended Advertising Report.
///
/// Same information as the legacy report, but each entry carries a fixed
/// 24-byte header before its data instead of 9, and the RSSI sits inside that
/// header rather than after the payload. Devices using extended advertising
/// appear here and nowhere else, which is why this exists alongside the legacy
/// parser rather than replacing it.
fn parse_extended_advertising_reports(params: &[u8]) -> Vec<DiscoveredDevice> {
    // subevent(1) num_reports(1) then per report:
    // event_type(2) address_type(1) address(6) primary_phy(1) secondary_phy(1)
    // sid(1) tx_power(1) rssi(1) periodic_interval(2) direct_address_type(1)
    // direct_address(6) data_len(1) data(n)
    const HEADER: usize = 24;

    let mut devices = Vec::new();
    if params.len() < 2 {
        return devices;
    }

    let count = params[1] as usize;
    let mut offset = 2;

    for _ in 0..count {
        if offset + HEADER > params.len() {
            break;
        }

        let address_type = params[offset + 2];
        let mut address = [0u8; 6];
        address.copy_from_slice(&params[offset + 3..offset + 9]);
        let rssi = params[offset + 13] as i8;
        let data_len = params[offset + 23] as usize;

        offset += HEADER;
        if offset + data_len > params.len() {
            break;
        }

        let data = &params[offset..offset + data_len];
        offset += data_len;

        let mut device = DiscoveredDevice {
            address: BdAddr(address),
            address_type,
            rssi,
            name: None,
            appearance: None,
            service_uuids: Vec::new(),
        };

        apply_advertising_data(&mut device, data);
        devices.push(device);
    }

    devices
}

/// Parses an LE Advertising Report, which can carry several devices at once.
fn parse_advertising_reports(params: &[u8]) -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();

    // subevent(1) num_reports(1) then per report:
    // event_type(1) address_type(1) address(6) data_len(1) data(n) rssi(1)
    if params.len() < 2 {
        return devices;
    }

    let count = params[1] as usize;
    let mut offset = 2;

    for _ in 0..count {
        if offset + 9 > params.len() {
            break;
        }

        let address_type = params[offset + 1];
        let mut address = [0u8; 6];
        address.copy_from_slice(&params[offset + 2..offset + 8]);

        let data_len = params[offset + 8] as usize;
        offset += 9;

        if offset + data_len + 1 > params.len() {
            break;
        }

        let data = &params[offset..offset + data_len];
        offset += data_len;

        let rssi = params[offset] as i8;
        offset += 1;

        let mut device = DiscoveredDevice {
            address: BdAddr(address),
            address_type,
            rssi,
            name: None,
            appearance: None,
            service_uuids: Vec::new(),
        };

        apply_advertising_data(&mut device, data);
        devices.push(device);
    }

    devices
}

/// Walks the length-type-value structures inside advertising data.
fn apply_advertising_data(device: &mut DiscoveredDevice, data: &[u8]) {
    let mut offset = 0;

    while offset < data.len() {
        let length = data[offset] as usize;
        if length == 0 || offset + 1 + length > data.len() {
            break;
        }

        let ad_type = data[offset + 1];
        let value = &data[offset + 2..offset + 1 + length];

        match ad_type {
            // Complete or shortened local name.
            0x08 | 0x09 => {
                if let Ok(name) = std::str::from_utf8(value) {
                    device.name = Some(name.to_owned());
                }
            }
            // Incomplete or complete list of 16-bit service UUIDs.
            0x02 | 0x03 => {
                for chunk in value.chunks_exact(2) {
                    device.service_uuids.push(u16::from_le_bytes([chunk[0], chunk[1]]));
                }
            }
            // Appearance.
            0x19 => {
                if value.len() >= 2 {
                    device.appearance = Some(u16::from_le_bytes([value[0], value[1]]));
                }
            }
            _ => {}
        }

        offset += 1 + length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertising_report_yields_name_and_services() {
        // subevent, num_reports, event_type, addr_type, address, data_len, data, rssi
        let mut params = vec![0x02, 0x01, 0x00, 0x00];
        params.extend_from_slice(&[0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C]);

        let mut ad = Vec::new();
        ad.extend_from_slice(&[0x05, 0x09, b'J', b'B', b'L', b'!']); // name "JBL!"
        ad.extend_from_slice(&[0x03, 0x03, 0x50, 0x18]); // service 0x1850 (PACS)

        params.push(ad.len() as u8);
        params.extend_from_slice(&ad);
        params.push(0xC0u8); // rssi -64

        let devices = parse_advertising_reports(&params);
        assert_eq!(devices.len(), 1);

        let device = &devices[0];
        assert_eq!(device.address.to_string(), "7C:FE:62:72:B4:9A");
        assert_eq!(device.name.as_deref(), Some("JBL!"));
        assert_eq!(device.service_uuids, vec![0x1850]);
        assert_eq!(device.rssi, -64);
        assert!(device.is_le_audio(), "PACS in advertisement means LE Audio");
    }

    #[test]
    fn plain_ble_device_is_not_flagged_as_le_audio() {
        let device = DiscoveredDevice {
            address: BdAddr([0; 6]),
            address_type: 0,
            rssi: -50,
            name: Some("Xbox Wireless Controller".into()),
            appearance: None,
            service_uuids: vec![0x1812], // HID over GATT
        };

        assert!(!device.is_le_audio());
    }

    #[test]
    fn truncated_report_does_not_panic() {
        assert!(parse_advertising_reports(&[0x02]).is_empty());
        assert!(parse_advertising_reports(&[0x02, 0x01, 0x00]).is_empty());
        // Claims two reports but carries one.
        let mut params = vec![0x02, 0x02, 0x00, 0x00];
        params.extend_from_slice(&[0; 6]);
        params.push(0x00);
        params.push(0xC0);
        assert_eq!(parse_advertising_reports(&params).len(), 1);
    }

    /// Builds one extended report: the 24-byte header, then advertising data.
    fn extended_report(address: [u8; 6], rssi: i8, data: &[u8]) -> Vec<u8> {
        let mut params = vec![subevt::EXTENDED_ADVERTISING_REPORT, 0x01];
        params.extend_from_slice(&[0x00, 0x00]); // event type
        params.push(0x00); // address type: public
        params.extend_from_slice(&address);
        params.extend_from_slice(&[0x01, 0x00, 0xFF, 0x7F]); // phys, sid, tx power
        params.push(rssi as u8);
        params.extend_from_slice(&[0x00, 0x00]); // periodic interval
        params.push(0x00); // direct address type
        params.extend_from_slice(&[0; 6]); // direct address
        params.push(data.len() as u8);
        params.extend_from_slice(data);
        params
    }

    #[test]
    fn extended_report_yields_the_same_device_as_a_legacy_one() {
        // Complete local name "JBL", then the PACS service UUID.
        let data = [0x04, 0x09, b'J', b'B', b'L', 0x03, 0x03, 0x50, 0x18];
        let address = [0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C];

        let devices = parse_extended_advertising_reports(&extended_report(address, -63, &data));

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].address, BdAddr(address));
        assert_eq!(devices[0].rssi, -63);
        assert_eq!(devices[0].name.as_deref(), Some("JBL"));
        assert!(devices[0].is_le_audio());
    }

    #[test]
    fn truncated_extended_report_does_not_panic() {
        assert!(parse_extended_advertising_reports(&[0x0D]).is_empty());
        assert!(parse_extended_advertising_reports(&[0x0D, 0x01, 0x00]).is_empty());

        // Header claims more advertising data than the buffer holds.
        let mut params = extended_report([0; 6], 0, &[]);
        *params.last_mut().unwrap() = 0x20;
        assert!(parse_extended_advertising_reports(&params).is_empty());
    }

    #[test]
    fn malformed_advertising_data_stops_cleanly() {
        let mut device = DiscoveredDevice {
            address: BdAddr([0; 6]),
            address_type: 0,
            rssi: 0,
            name: None,
            appearance: None,
            service_uuids: Vec::new(),
        };

        // Length byte claims more than the buffer holds.
        apply_advertising_data(&mut device, &[0x20, 0x09, b'x']);
        assert!(device.name.is_none());
    }

    #[test]
    fn status_names_cover_the_common_failures() {
        assert_eq!(status_name(0x00), "success");
        assert_eq!(status_name(0x42), "unacceptable connection parameters");
    }
}
