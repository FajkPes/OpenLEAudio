//! HCI command and event layer.
//!
//! Everything the stack does to the controller goes through here: reset, scanning,
//! connecting, and later the CIG/CIS setup that carries LC3 audio. Commands are
//! built into byte buffers the transport hands straight to USB.

use std::fmt;

/// Builds an opcode from its Opcode Group Field and Opcode Command Field.
pub const fn opcode(ogf: u16, ocf: u16) -> u16 {
    (ogf << 10) | ocf
}

/// Opcode Group Fields.
pub mod ogf {
    pub const LINK_CONTROL: u16 = 0x01;
    pub const CONTROLLER: u16 = 0x03;
    pub const INFORMATIONAL: u16 = 0x04;
    pub const STATUS: u16 = 0x05;
    pub const LE_CONTROLLER: u16 = 0x08;
}

/// Opcodes the stack issues.
pub mod op {
    use super::{ogf, opcode};

    pub const RESET: u16 = opcode(ogf::CONTROLLER, 0x0003);
    pub const SET_EVENT_MASK: u16 = opcode(ogf::CONTROLLER, 0x0001);
    pub const WRITE_LE_HOST_SUPPORTED: u16 = opcode(ogf::CONTROLLER, 0x006D);
    pub const READ_LOCAL_VERSION: u16 = opcode(ogf::INFORMATIONAL, 0x0001);
    pub const READ_BD_ADDR: u16 = opcode(ogf::INFORMATIONAL, 0x0009);
    pub const DISCONNECT: u16 = opcode(ogf::LINK_CONTROL, 0x0006);
    pub const READ_RSSI: u16 = opcode(ogf::STATUS, 0x0005);

    pub const LE_SET_EVENT_MASK: u16 = opcode(ogf::LE_CONTROLLER, 0x0001);
    pub const LE_SET_HOST_FEATURE: u16 = opcode(ogf::LE_CONTROLLER, 0x0074);
    pub const LE_READ_BUFFER_SIZE_V2: u16 = opcode(ogf::LE_CONTROLLER, 0x0060);
    pub const LE_READ_LOCAL_FEATURES: u16 = opcode(ogf::LE_CONTROLLER, 0x0003);
    pub const LE_SET_SCAN_PARAMETERS: u16 = opcode(ogf::LE_CONTROLLER, 0x000B);
    pub const LE_SET_SCAN_ENABLE: u16 = opcode(ogf::LE_CONTROLLER, 0x000C);
    pub const LE_CREATE_CONNECTION: u16 = opcode(ogf::LE_CONTROLLER, 0x000D);
    pub const LE_CONNECTION_UPDATE: u16 = opcode(ogf::LE_CONTROLLER, 0x0013);
    pub const LE_SET_PHY: u16 = opcode(ogf::LE_CONTROLLER, 0x0032);
    pub const LE_CREATE_CONNECTION_CANCEL: u16 = opcode(ogf::LE_CONTROLLER, 0x000E);
    pub const LE_SET_EXTENDED_SCAN_PARAMETERS: u16 = opcode(ogf::LE_CONTROLLER, 0x0041);
    pub const LE_SET_EXTENDED_SCAN_ENABLE: u16 = opcode(ogf::LE_CONTROLLER, 0x0042);
    pub const LE_EXTENDED_CREATE_CONNECTION: u16 = opcode(ogf::LE_CONTROLLER, 0x0043);
    pub const LE_ENABLE_ENCRYPTION: u16 = opcode(ogf::LE_CONTROLLER, 0x0019);
    pub const LE_LTK_REQUEST_REPLY: u16 = opcode(ogf::LE_CONTROLLER, 0x001A);

    // LE Audio: isochronous channels.
    pub const LE_SET_CIG_PARAMETERS: u16 = opcode(ogf::LE_CONTROLLER, 0x0062);
    pub const LE_CREATE_CIS: u16 = opcode(ogf::LE_CONTROLLER, 0x0064);
    pub const LE_REMOVE_CIG: u16 = opcode(ogf::LE_CONTROLLER, 0x0065);
    pub const LE_SETUP_ISO_DATA_PATH: u16 = opcode(ogf::LE_CONTROLLER, 0x006E);
    pub const LE_READ_ISO_LINK_QUALITY: u16 = opcode(ogf::LE_CONTROLLER, 0x0075);
}

/// Event codes.
pub mod evt {
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    pub const COMMAND_STATUS: u8 = 0x0F;
    pub const LE_META: u8 = 0x3E;
}

/// LE meta event subevent codes.
pub mod subevt {
    pub const CONNECTION_COMPLETE: u8 = 0x01;
    pub const ADVERTISING_REPORT: u8 = 0x02;
    pub const ENHANCED_CONNECTION_COMPLETE: u8 = 0x0A;
    /// Carries devices that advertise using extended advertising, which the
    /// legacy report cannot describe - and which LE Audio devices commonly use.
    pub const EXTENDED_ADVERTISING_REPORT: u8 = 0x0D;
    pub const CIS_ESTABLISHED: u8 = 0x19;
    pub const CIS_REQUEST: u8 = 0x1A;
}

/// A 48-bit Bluetooth device address, stored in transmission order.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BdAddr(pub [u8; 6]);

impl BdAddr {
    /// Parses the usual display form, most significant byte first.
    pub fn parse(text: &str) -> Option<Self> {
        let mut bytes = [0u8; 6];
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() != 6 {
            return None;
        }

        for (i, part) in parts.iter().enumerate() {
            // Display order is reversed relative to the wire.
            bytes[5 - i] = u8::from_str_radix(part, 16).ok()?;
        }

        Some(Self(bytes))
    }
}

impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[5], b[4], b[3], b[2], b[1], b[0]
        )
    }
}

impl fmt::Debug for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BdAddr({self})")
    }
}

/// Builds an HCI command packet: opcode, parameter length, parameters.
pub fn command(opcode: u16, params: &[u8]) -> Vec<u8> {
    debug_assert!(params.len() <= 255, "HCI parameters cannot exceed 255 bytes");

    let mut packet = Vec::with_capacity(3 + params.len());
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.push(params.len() as u8);
    packet.extend_from_slice(params);
    packet
}

/// A parsed HCI event.
#[derive(Debug, Clone)]
pub struct Event {
    pub code: u8,
    pub params: Vec<u8>,
}

impl Event {
    pub fn parse(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < 2 {
            return None;
        }

        let length = buffer[1] as usize;
        if buffer.len() < 2 + length {
            return None;
        }

        Some(Self {
            code: buffer[0],
            params: buffer[2..2 + length].to_vec(),
        })
    }

    /// Subevent code, for LE meta events.
    pub fn subevent(&self) -> Option<u8> {
        if self.code == evt::LE_META {
            self.params.first().copied()
        } else {
            None
        }
    }

    /// For Command Complete: the opcode being acknowledged and its return parameters.
    pub fn command_complete(&self) -> Option<(u16, &[u8])> {
        if self.code != evt::COMMAND_COMPLETE || self.params.len() < 3 {
            return None;
        }

        let opcode = u16::from_le_bytes([self.params[1], self.params[2]]);
        Some((opcode, &self.params[3..]))
    }

    /// For Command Status: the status byte and the opcode it refers to.
    pub fn command_status(&self) -> Option<(u8, u16)> {
        if self.code != evt::COMMAND_STATUS || self.params.len() < 4 {
            return None;
        }

        let opcode = u16::from_le_bytes([self.params[2], self.params[3]]);
        Some((self.params[0], opcode))
    }

    /// For LE Enhanced Connection Complete: the handle and peer address.
    pub fn connection_complete(&self) -> Option<(u16, BdAddr)> {
        match self.connection_result()? {
            (0x00, handle, address) => Some((handle, address)),
            _ => None,
        }
    }

    /// The same event including a failed one, so the caller can say why.
    ///
    /// Both the legacy and enhanced forms put status, handle, role and peer
    /// address in the same places, which is all that is read here. Discarding a
    /// failure silently leaves nothing to report but "did not complete".
    pub fn connection_result(&self) -> Option<(u8, u16, BdAddr)> {
        match self.subevent()? {
            subevt::ENHANCED_CONNECTION_COMPLETE | subevt::CONNECTION_COMPLETE => {}
            _ => return None,
        }
        if self.params.len() < 12 {
            return None;
        }

        let status = self.params[1];
        let handle = u16::from_le_bytes([self.params[2], self.params[3]]) & 0x0FFF;
        let mut address = [0u8; 6];
        address.copy_from_slice(&self.params[6..12]);
        Some((status, handle, BdAddr(address)))
    }
}

/// Controller identity, from Read Local Version Information.
#[derive(Debug, Clone)]
pub struct LocalVersion {
    pub hci_version: u8,
    pub manufacturer: u16,
    pub lmp_version: u8,
}

impl LocalVersion {
    pub fn parse(return_params: &[u8]) -> Option<Self> {
        // status(1) hci_version(1) hci_revision(2) lmp_version(1) manufacturer(2) lmp_subversion(2)
        if return_params.len() < 9 || return_params[0] != 0x00 {
            return None;
        }

        Some(Self {
            hci_version: return_params[1],
            lmp_version: return_params[4],
            manufacturer: u16::from_le_bytes([return_params[5], return_params[6]]),
        })
    }

    /// Marketing name for the Core specification version this LMP number denotes.
    pub fn bluetooth_version(&self) -> &'static str {
        match self.lmp_version {
            9 => "5.0",
            10 => "5.1",
            11 => "5.2",
            12 => "5.3",
            13 => "5.4",
            14 => "6.0",
            15 => "6.1",
            _ => "unknown",
        }
    }

    /// LE Audio needs isochronous channels, which arrived in Core 5.2.
    pub fn supports_le_audio(&self) -> bool {
        self.lmp_version >= 11
    }
}

// ---- Command builders ----

pub fn reset() -> Vec<u8> {
    command(op::RESET, &[])
}

pub fn read_local_version() -> Vec<u8> {
    command(op::READ_LOCAL_VERSION, &[])
}

pub fn read_bd_addr() -> Vec<u8> {
    command(op::READ_BD_ADDR, &[])
}

pub fn read_rssi(handle: u16) -> Vec<u8> {
    command(op::READ_RSSI, &handle.to_le_bytes())
}

/// Asks the controller how one isochronous channel is actually doing.
///
/// This is the only honest source of packet loss on a CIS. Counting our own
/// failed USB submissions says whether the audio left this PC, which it almost
/// always did; it says nothing at all about whether the headphones heard it.
/// Everything that happens on the air - a packet that needed retransmitting, a
/// subevent that went unacknowledged, one flushed because its deadline passed -
/// is visible here and nowhere else.
pub fn le_read_iso_link_quality(cis_handle: u16) -> Vec<u8> {
    command(op::LE_READ_ISO_LINK_QUALITY, &cis_handle.to_le_bytes())
}

/// What the controller knows about one isochronous channel.
///
/// Counters are cumulative since the channel was established, so a rate is the
/// difference between two readings rather than any single one of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IsoLinkQuality {
    pub handle: u16,
    /// Packets sent whose acknowledgement never arrived. On a stream flowing
    /// from here to the headphones, this is the count that means "not heard".
    pub tx_unacked_packets: u32,
    /// Packets dropped because their transport latency deadline passed before
    /// they could be sent. Audio that was never given a chance to arrive.
    pub tx_flushed_packets: u32,
    /// Packets that had to be sent more than once. Not loss - this is the
    /// retransmission budget doing its job - but a rising number is the early
    /// warning that the budget is about to run out.
    pub retransmitted_packets: u32,
    pub tx_last_subevent_packets: u32,
    /// Received packets that failed their CRC, on the microphone direction.
    pub crc_error_packets: u32,
    /// Receive slots where nothing arrived at all.
    pub rx_unreceived_packets: u32,
    pub duplicate_packets: u32,
}

impl IsoLinkQuality {
    /// Packets the headphones can be assumed not to have played.
    pub fn lost_packets(&self) -> u64 {
        self.tx_unacked_packets as u64 + self.tx_flushed_packets as u64
    }
}

/// Reads the return parameters of LE Read ISO Link Quality.
///
/// Layout: status(1) connection_handle(2) then seven 32-bit counters.
pub fn parse_iso_link_quality(params: &[u8]) -> Option<IsoLinkQuality> {
    if params.len() < 3 + 7 * 4 || params[0] != 0x00 {
        return None;
    }

    let word = |index: usize| {
        let at = 3 + index * 4;
        u32::from_le_bytes([params[at], params[at + 1], params[at + 2], params[at + 3]])
    };

    Some(IsoLinkQuality {
        handle: u16::from_le_bytes([params[1], params[2]]),
        tx_unacked_packets: word(0),
        tx_flushed_packets: word(1),
        tx_last_subevent_packets: word(2),
        retransmitted_packets: word(3),
        crc_error_packets: word(4),
        rx_unreceived_packets: word(5),
        duplicate_packets: word(6),
    })
}

/// Unmasks every event the controller can raise, so nothing is missed.
pub fn set_event_mask() -> Vec<u8> {
    command(op::SET_EVENT_MASK, &[0xFF; 8])
}

/// Unmasks the LE events the stack relies on, including the CIS ones.
/// Tells the controller the host supports LE, and that it may not assume
/// classic-only behaviour.
pub fn write_le_host_supported() -> Vec<u8> {
    // Second byte is a reserved "simultaneous" flag that must be zero.
    command(op::WRITE_LE_HOST_SUPPORTED, &[0x01, 0x00])
}

/// Turns on a host-side LE feature bit.
///
/// Bit 32 is Connected Isochronous Streams, which a controller need not offer
/// until the host says it can handle them - and no CIS can be created without
/// it. The Windows driver sets this during initialisation on this same adapter.
pub fn le_set_host_feature(bit: u8, enable: bool) -> Vec<u8> {
    command(op::LE_SET_HOST_FEATURE, &[bit, enable as u8])
}

pub fn le_set_event_mask() -> Vec<u8> {
    command(op::LE_SET_EVENT_MASK, &[0xFF; 8])
}

pub fn le_read_local_features() -> Vec<u8> {
    command(op::LE_READ_LOCAL_FEATURES, &[])
}

/// Passive scan. Interval and window are in 0.625 ms units.
pub fn le_set_scan_parameters(interval: u16, window: u16) -> Vec<u8> {
    let mut params = Vec::with_capacity(7);
    params.push(0x00); // passive scanning
    params.extend_from_slice(&interval.to_le_bytes());
    params.extend_from_slice(&window.to_le_bytes());
    params.push(0x00); // public own address
    params.push(0x00); // accept all advertisements
    command(op::LE_SET_SCAN_PARAMETERS, &params)
}

pub fn le_set_scan_enable(enable: bool, filter_duplicates: bool) -> Vec<u8> {
    command(
        op::LE_SET_SCAN_ENABLE,
        &[enable as u8, filter_duplicates as u8],
    )
}

/// Passive extended scan on the 1M PHY. Interval and window are 0.625 ms units.
///
/// A device that advertises with extended advertising is invisible to the
/// legacy scan above - it produces no legacy report at all - and LE Audio
/// headphones commonly do exactly that. The layout here matches what the
/// Windows driver was captured sending to this same adapter.
pub fn le_set_extended_scan_parameters(interval: u16, window: u16) -> Vec<u8> {
    let mut params = Vec::with_capacity(8);
    params.push(0x00); // own address: public
    params.push(0x00); // accept all advertisements
    params.push(0x01); // scanning PHYs: LE 1M
    params.push(0x00); // passive scanning
    params.extend_from_slice(&interval.to_le_bytes());
    params.extend_from_slice(&window.to_le_bytes());
    command(op::LE_SET_EXTENDED_SCAN_PARAMETERS, &params)
}

/// Duration and period are left at zero, meaning scan until told to stop.
pub fn le_set_extended_scan_enable(enable: bool, filter_duplicates: bool) -> Vec<u8> {
    let mut params = Vec::with_capacity(6);
    params.push(enable as u8);
    params.push(filter_duplicates as u8);
    params.extend_from_slice(&0u16.to_le_bytes()); // duration: until stopped
    params.extend_from_slice(&0u16.to_le_bytes()); // period: no repetition
    command(op::LE_SET_EXTENDED_SCAN_ENABLE, &params)
}


// ---- Connection setup ----

/// Builds LE Create Connection.
///
/// The connection interval asked for here is only an opening position; the
/// peripheral usually proposes its own preference straight afterwards, and for
/// audio the interval that matters is the CIG one anyway.
pub fn le_create_connection(
    peer: BdAddr,
    peer_address_type: u8,
    supervision_timeout: u16,
) -> Vec<u8> {
    let mut params = Vec::with_capacity(25);

    params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval 60 ms
    params.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window 30 ms
    params.push(0x00);                                  // no accept list, use peer address
    params.push(peer_address_type);
    params.extend_from_slice(&peer.0);
    params.push(0x00);                                  // own address: public
    params.extend_from_slice(&0x0018u16.to_le_bytes()); // interval min 30 ms
    params.extend_from_slice(&0x0028u16.to_le_bytes()); // interval max 50 ms
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // latency
    params.extend_from_slice(&clamp_supervision(supervision_timeout).to_le_bytes());
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length

    command(op::LE_CREATE_CONNECTION, &params)
}

/// Builds LE Connection Update: new interval, peripheral latency and timeout.
///
/// Audio does not travel on the ACL - it has its own isochronous channels - so
/// once the stream is configured the ACL carries almost nothing: a volume
/// notification when somebody touches the earcup, a battery level now and then.
/// It still costs both radios a connection event every interval, and peripheral
/// latency is the specification's own answer: the headphones may skip up to that
/// many events when they have nothing to say, and wake for the next one when
/// they do. Nothing is lost, because anything we send still arrives at the very
/// next event.
///
/// The supervision timeout has to stay longer than `interval_max * (latency + 1)`
/// or the link declares itself dead in normal operation; the caller is expected
/// to have clamped it.
pub fn le_connection_update(
    handle: u16,
    interval_min: u16,
    interval_max: u16,
    latency: u16,
    supervision_timeout: u16,
) -> Vec<u8> {
    let mut params = Vec::with_capacity(14);
    params.extend_from_slice(&handle.to_le_bytes());
    params.extend_from_slice(&interval_min.to_le_bytes());
    params.extend_from_slice(&interval_max.to_le_bytes());
    params.extend_from_slice(&latency.to_le_bytes());
    params.extend_from_slice(&clamp_supervision(supervision_timeout).to_le_bytes());
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length

    command(op::LE_CONNECTION_UPDATE, &params)
}

/// Keeps a supervision timeout inside what the specification allows.
///
/// Units of 10 ms, from 100 ms to 32 s. Also enforced against the connection
/// interval: the timeout must exceed twice the maximum interval, and a
/// controller answers an impossible pair with "invalid HCI parameters" rather
/// than with anything that names the offending number.
pub fn clamp_supervision(timeout: u16) -> u16 {
    timeout.clamp(0x0064, 0x0C80)
}

/// Builds LE Extended Create Connection on the 1M PHY.
///
/// The extended form is what a controller expects once extended scanning has
/// been used; mixing the two generations is what the specification allows a
/// controller to refuse outright. Parameters follow the Windows driver capture:
/// a 30 ms connection interval and a 5 s supervision timeout.
pub fn le_extended_create_connection(
    peer: BdAddr,
    peer_address_type: u8,
    supervision_timeout: u16,
) -> Vec<u8> {
    let mut params = Vec::with_capacity(26);

    params.push(0x00); // no accept list, use the peer address below
    params.push(0x00); // own address: public
    params.push(peer_address_type);
    params.extend_from_slice(&peer.0);
    params.push(0x01); // initiating PHYs: LE 1M

    // One set of parameters per PHY named above.
    params.extend_from_slice(&0x0024u16.to_le_bytes()); // scan interval 22.5 ms
    params.extend_from_slice(&0x0012u16.to_le_bytes()); // scan window 11.25 ms
    params.extend_from_slice(&0x0018u16.to_le_bytes()); // interval min 30 ms
    params.extend_from_slice(&0x0018u16.to_le_bytes()); // interval max 30 ms
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // latency
    params.extend_from_slice(&clamp_supervision(supervision_timeout).to_le_bytes());
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length

    command(op::LE_EXTENDED_CREATE_CONNECTION, &params)
}

/// Cancels a connection attempt that is taking too long.
pub fn le_create_connection_cancel() -> Vec<u8> {
    command(op::LE_CREATE_CONNECTION_CANCEL, &[])
}

/// Builds LE Enable Encryption, which starts encryption using a known key.
///
/// With LE Secure Connections the diversifier and random number are zero - the
/// long term key came from the pairing exchange, not from a legacy lookup.
pub fn le_enable_encryption(handle: u16, long_term_key: &[u8; 16]) -> Vec<u8> {
    let mut params = Vec::with_capacity(28);

    params.extend_from_slice(&handle.to_le_bytes());
    params.extend_from_slice(&[0u8; 8]);  // random number
    params.extend_from_slice(&[0u8; 2]);  // encrypted diversifier
    params.extend_from_slice(long_term_key);

    command(op::LE_ENABLE_ENCRYPTION, &params)
}

/// Event 0x08: Encryption Change. Returns the handle and whether encryption is on.
pub fn parse_encryption_change(event: &Event) -> Option<(u16, bool)> {
    const EVT_ENCRYPTION_CHANGE: u8 = 0x08;

    if event.code != EVT_ENCRYPTION_CHANGE || event.params.len() < 4 {
        return None;
    }
    if event.params[0] != 0x00 {
        return None; // failed
    }

    let handle = u16::from_le_bytes([event.params[1], event.params[2]]) & 0x0FFF;
    Some((handle, event.params[3] != 0x00))
}

/// Parses Disconnection Complete, returning the handle and the reason code.
///
/// Worth watching during any wait for data: once the link is gone nothing will
/// ever answer, and without this the stack sits out the full timeout and then
/// blames the peer for staying silent.
pub fn parse_disconnection_complete(event: &Event) -> Option<(u16, u8)> {
    const EVT_DISCONNECTION_COMPLETE: u8 = 0x05;

    if event.code != EVT_DISCONNECTION_COMPLETE || event.params.len() < 4 {
        return None;
    }
    if event.params[0] != 0x00 {
        return None; // the disconnection command itself failed
    }

    let handle = u16::from_le_bytes([event.params[1], event.params[2]]) & 0x0FFF;
    Some((handle, event.params[3]))
}

/// Parses LE CIS Established, returning the status and CIS handle.
///
/// This is the only real proof that an isochronous channel exists. The Command
/// Status answering LE Create CIS says the controller accepted the request, not
/// that the channel came up - and on this hardware the second channel is
/// exactly the one that sometimes never does.
pub fn parse_cis_established(event: &Event) -> Option<(u8, u16)> {
    if event.subevent()? != subevt::CIS_ESTABLISHED || event.params.len() < 4 {
        return None;
    }

    let status = event.params[1];
    let handle = u16::from_le_bytes([event.params[2], event.params[3]]) & 0x0FFF;
    Some((status, handle))
}

/// Plain-language name for why a link went away.
/// How many packets the controller has finished sending, per connection.
///
/// The only proof from the controller that a stream is actually going out. Two
/// isochronous channels that both accept writes look identical from the host;
/// the difference shows up here, when one of them stops reporting completions
/// and the other keeps going.
pub fn parse_number_of_completed_packets(event: &Event) -> Vec<(u16, u16)> {
    const EVT_NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;

    if event.code != EVT_NUMBER_OF_COMPLETED_PACKETS || event.params.is_empty() {
        return Vec::new();
    }

    let count = event.params[0] as usize;
    let mut reports = Vec::with_capacity(count);

    for index in 0..count {
        let handle_at = 1 + index * 2;
        let packets_at = 1 + count * 2 + index * 2;

        if packets_at + 2 > event.params.len() {
            break;
        }

        reports.push((
            u16::from_le_bytes([event.params[handle_at], event.params[handle_at + 1]]),
            u16::from_le_bytes([event.params[packets_at], event.params[packets_at + 1]]),
        ));
    }

    reports
}

pub fn disconnect_reason(reason: u8) -> &'static str {
    match reason {
        0x08 => "spojeni vyprselo (supervision timeout)",
        0x13 => "protejsek spojeni ukoncil",
        0x14 => "protejsek ukoncil spojeni kvuli nizke baterii",
        0x15 => "protejsek se vypina",
        0x16 => "spojeni ukoncil hostitel",
        0x1A => "protejsek nepodporuje pozadovanou funkci",
        0x22 => "vyprsel LMP/LL timeout",
        0x28 => "vyprsel instant",
        0x3B => "nepovolene parametry spojeni",
        0x3D => "spojeni ukonceno kvuli chybam MIC",
        0x3E => "spojeni se nepodarilo navazat",
        _ => "viz Core spec Part D",
    }
}

// ---- Isochronous channel setup ----

/// One CIS inside a CIG.
#[derive(Debug, Clone, Copy)]
pub struct CisParams {
    pub cis_id: u8,
    /// Central to peripheral: what we send. Zero for a direction we do not use.
    pub max_sdu_c_to_p: u16,
    pub max_sdu_p_to_c: u16,
    pub phy_c_to_p: u8,
    pub phy_p_to_c: u8,
    pub rtn_c_to_p: u8,
    pub rtn_p_to_c: u8,
}

/// Builds LE Set CIG Parameters.
///
/// One CIS carrying stereo is preferred over two carrying a channel each: fewer
/// links to establish means fewer ways for setup to fail.
/// How the controller lays several channels out inside one isochronous interval.
///
/// Sequential puts one channel's subevents after the other's; interleaved
/// alternates them. For two channels going to the same pair of earpieces,
/// interleaved spreads each channel's retransmission opportunities across the
/// whole interval instead of bunching them, which is the difference between a
/// second channel that comes up and one that reports "connection failed to be
/// established" while the first is fine.
pub const PACKING_SEQUENTIAL: u8 = 0x00;
pub const PACKING_INTERLEAVED: u8 = 0x01;

pub fn le_set_cig_parameters(
    cig_id: u8,
    sdu_interval_c_to_p_us: u32,
    sdu_interval_p_to_c_us: u32,
    framing: u8,
    max_transport_latency_c_to_p_ms: u16,
    max_transport_latency_p_to_c_ms: u16,
    packing: u8,
    cis: &[CisParams],
) -> Vec<u8> {
    let mut params = Vec::with_capacity(15 + cis.len() * 9);

    params.push(cig_id);
    params.extend_from_slice(&sdu_interval_c_to_p_us.to_le_bytes()[0..3]);
    params.extend_from_slice(&sdu_interval_p_to_c_us.to_le_bytes()[0..3]);
    params.push(0x00); // worst case sleep clock accuracy: unknown
    params.push(packing);
    params.push(framing);
    params.extend_from_slice(&max_transport_latency_c_to_p_ms.to_le_bytes());
    params.extend_from_slice(&max_transport_latency_p_to_c_ms.to_le_bytes());
    params.push(cis.len() as u8);

    for entry in cis {
        params.push(entry.cis_id);
        params.extend_from_slice(&entry.max_sdu_c_to_p.to_le_bytes());
        params.extend_from_slice(&entry.max_sdu_p_to_c.to_le_bytes());
        params.push(entry.phy_c_to_p);
        params.push(entry.phy_p_to_c);
        params.push(entry.rtn_c_to_p);
        params.push(entry.rtn_p_to_c);
    }

    command(op::LE_SET_CIG_PARAMETERS, &params)
}

/// Builds LE Create CIS, pairing each isochronous stream with its ACL link.
pub fn le_create_cis(pairs: &[(u16, u16)]) -> Vec<u8> {
    let mut params = Vec::with_capacity(1 + pairs.len() * 4);
    params.push(pairs.len() as u8);

    for (cis_handle, acl_handle) in pairs {
        params.extend_from_slice(&cis_handle.to_le_bytes());
        params.extend_from_slice(&acl_handle.to_le_bytes());
    }

    command(op::LE_CREATE_CIS, &params)
}

/// Data path direction for Setup ISO Data Path.
pub const ISO_PATH_INPUT: u8 = 0x00; // host to controller: our audio going out
pub const ISO_PATH_OUTPUT: u8 = 0x01;

/// Builds LE Setup ISO Data Path in transparent mode.
///
/// Transparent means the controller does not touch the payload - we hand it
/// finished LC3 frames. That is the whole point: the encoder is ours, so the
/// bitrate is ours too.
pub fn le_setup_iso_data_path(cis_handle: u16, direction: u8, controller_delay_us: u32) -> Vec<u8> {
    let mut params = Vec::with_capacity(13);

    params.extend_from_slice(&cis_handle.to_le_bytes());
    params.push(direction);
    params.push(0x00); // data path id: HCI
    params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]); // transparent codec
    params.extend_from_slice(&controller_delay_us.to_le_bytes()[0..3]);
    params.push(0x00); // no codec configuration

    command(op::LE_SETUP_ISO_DATA_PATH, &params)
}

/// Ends a connection, telling the peer why.
///
/// Not optional housekeeping. Letting the handle go without this leaves the
/// controller believing the connection is still up, and the next attempt to
/// reach the same peer is refused - "connection attempt did not complete", or
/// an unknown connection identifier - because one already exists.
pub fn disconnect(handle: u16, reason: u8) -> Vec<u8> {
    let mut params = Vec::with_capacity(3);
    params.extend_from_slice(&handle.to_le_bytes());
    params.push(reason);
    command(op::DISCONNECT, &params)
}

/// The reason a host gives when the user asked for the disconnection.
pub const REASON_REMOTE_USER_TERMINATED: u8 = 0x13;

pub fn le_remove_cig(cig_id: u8) -> Vec<u8> {
    command(op::LE_REMOVE_CIG, &[cig_id])
}

/// Builds an HCI ISO data packet header around one already-encoded payload.
///
/// The timestamp is omitted and the packet status is "valid data"; sequence
/// numbers must increase by one per SDU or the controller drops the stream.
pub fn iso_data_packet(cis_handle: u16, sequence_number: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8 + payload.len());

    // Handle with PB flag 0b10 (complete SDU) and no timestamp.
    let header = (cis_handle & 0x0FFF) | (0b10 << 12);
    packet.extend_from_slice(&header.to_le_bytes());

    let data_load_len = (4 + payload.len()) as u16;
    packet.extend_from_slice(&data_load_len.to_le_bytes());
    packet.extend_from_slice(&sequence_number.to_le_bytes());

    // SDU length with packet status flag 0b00 in the top bits.
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(payload);
    packet
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoData<'a> {
    pub handle: u16,
    pub sequence_number: u16,
    pub payload: &'a [u8],
}

/// Parses one complete, valid HCI ISO SDU received from the controller.
/// Fragmented and invalid SDUs are ignored instead of being fed to LC3 as
/// decoder garbage; the next complete frame can be decoded independently.
pub fn parse_iso_data_packet(packet: &[u8]) -> Option<IsoData<'_>> {
    if packet.len() < 8 {
        return None;
    }
    let header = u16::from_le_bytes([packet[0], packet[1]]);
    let handle = header & 0x0fff;
    let pb = (header >> 12) & 0x03;
    let timestamp_present = header & 0x4000 != 0;
    if pb != 0b10 {
        return None;
    }

    let data_load_len = u16::from_le_bytes([packet[2], packet[3]]) as usize;
    if packet.len() < 4 + data_load_len {
        return None;
    }
    let mut offset = 4;
    if timestamp_present {
        offset += 4;
    }
    if offset + 4 > 4 + data_load_len {
        return None;
    }

    let sequence_number = u16::from_le_bytes([packet[offset], packet[offset + 1]]);
    let sdu = u16::from_le_bytes([packet[offset + 2], packet[offset + 3]]);
    let status = sdu >> 14;
    let sdu_len = (sdu & 0x3fff) as usize;
    offset += 4;
    if status != 0 || offset + sdu_len > 4 + data_load_len {
        return None;
    }

    Some(IsoData { handle, sequence_number, payload: &packet[offset..offset + sdu_len] })
}
#[cfg(test)]
mod tests {
    #[test]
    fn a_supervision_timeout_stays_inside_what_the_specification_allows() {
        assert_eq!(super::clamp_supervision(0), 0x0064, "100 ms floor");
        assert_eq!(super::clamp_supervision(0xFFFF), 0x0C80, "32 s ceiling");
        assert_eq!(super::clamp_supervision(0x03E8), 0x03E8, "10 s passes through");
    }

    #[test]
    fn complete_iso_packet_round_trips_through_parser() {
        let packet = iso_data_packet(0x0042, 17, &[1, 2, 3, 4]);
        let parsed = parse_iso_data_packet(&packet).expect("valid complete SDU");
        assert_eq!(parsed.handle, 0x0042);
        assert_eq!(parsed.sequence_number, 17);
        assert_eq!(parsed.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn invalid_or_fragmented_iso_is_not_decoded() {
        let mut invalid = iso_data_packet(0x0042, 1, &[1, 2]);
        invalid[7] |= 0x40; // packet status != valid
        assert!(parse_iso_data_packet(&invalid).is_none());

        let mut fragment = iso_data_packet(0x0042, 1, &[1, 2]);
        fragment[1] &= 0x0f; // PB 00: first fragment, not a complete SDU
        assert!(parse_iso_data_packet(&fragment).is_none());
    }

    #[test]
    fn completion_reports_are_read_per_connection() {
        // Two connections in one event: handle 0x0017 sent 3, 0x0018 sent 1.
        let event = Event {
            code: 0x13,
            params: vec![0x02, 0x17, 0x00, 0x18, 0x00, 0x03, 0x00, 0x01, 0x00],
        };

        assert_eq!(
            parse_number_of_completed_packets(&event),
            vec![(0x0017, 3), (0x0018, 1)]
        );
    }

    #[test]
    fn other_events_report_no_completions() {
        let event = Event { code: 0x05, params: vec![0x00, 0x0C, 0x00, 0x13] };
        assert!(parse_number_of_completed_packets(&event).is_empty());
    }

    use super::*;

    #[test]
    fn opcodes_match_the_specification() {
        assert_eq!(op::RESET, 0x0C03);
        assert_eq!(op::READ_LOCAL_VERSION, 0x1001);
        assert_eq!(op::LE_SET_SCAN_ENABLE, 0x200C);
        assert_eq!(op::LE_SET_CIG_PARAMETERS, 0x2062);
        assert_eq!(op::LE_CREATE_CIS, 0x2064);
        assert_eq!(op::LE_SET_EXTENDED_SCAN_PARAMETERS, 0x2041);
        assert_eq!(op::LE_SET_EXTENDED_SCAN_ENABLE, 0x2042);
        assert_eq!(op::LE_EXTENDED_CREATE_CONNECTION, 0x2043);
    }

    /// Checked against a USBPcap capture of the Windows driver driving this
    /// exact adapter, so these are the bytes known-good hardware accepts rather
    /// than a reading of the specification that might be subtly off.
    #[test]
    fn extended_commands_match_the_windows_driver_capture() {
        // 0x2041 with a 640 ms interval and an 11.25 ms window.
        assert_eq!(
            le_set_extended_scan_parameters(0x0400, 0x0012),
            vec![0x41, 0x20, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x04, 0x12, 0x00]
        );

        assert_eq!(
            le_set_extended_scan_enable(true, false),
            vec![0x42, 0x20, 0x06, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            le_set_extended_scan_enable(false, false),
            vec![0x42, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );

        // 0x2043 to 7C:FE:62:72:B4:9A, public address.
        let peer = BdAddr([0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C]);
        assert_eq!(
            le_extended_create_connection(peer, 0x00, 0x01F4),
            vec![
                0x43, 0x20, 0x1A, 0x00, 0x00, 0x00, 0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C, 0x01,
                0x24, 0x00, 0x12, 0x00, 0x18, 0x00, 0x18, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00,
                0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn reset_encodes_to_three_bytes() {
        assert_eq!(reset(), vec![0x03, 0x0C, 0x00]);
    }

    #[test]
    fn scan_parameters_encode_little_endian() {
        // 0x0010 interval, 0x0010 window
        let packet = le_set_scan_parameters(0x0010, 0x0010);
        assert_eq!(packet[0..2], [0x0B, 0x20]); // opcode
        assert_eq!(packet[2], 7); // parameter length
        assert_eq!(packet[3], 0x00); // passive
        assert_eq!(packet[4..6], [0x10, 0x00]);
        assert_eq!(packet[6..8], [0x10, 0x00]);
    }

    #[test]
    fn address_round_trips_through_display_form() {
        let address = BdAddr::parse("7C:FE:62:72:B4:9A").unwrap();
        // Wire order is reversed relative to display order.
        assert_eq!(address.0, [0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C]);
        assert_eq!(address.to_string(), "7C:FE:62:72:B4:9A");
    }

    #[test]
    fn command_complete_reports_its_opcode() {
        // Command Complete for Reset: code, len, num_packets, opcode, status
        let raw = [0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        let event = Event::parse(&raw).unwrap();

        let (opcode, params) = event.command_complete().unwrap();
        assert_eq!(opcode, op::RESET);
        assert_eq!(params, &[0x00]);
    }

    #[test]
    fn local_version_maps_lmp_to_core_version() {
        // status, hci_ver=14, hci_rev, lmp_ver=14, manufacturer, lmp_subver
        let params = [0x00, 14, 0x00, 0x00, 14, 0x0F, 0x00, 0x00, 0x00];
        let version = LocalVersion::parse(&params).unwrap();

        assert_eq!(version.lmp_version, 14);
        assert_eq!(version.bluetooth_version(), "6.0");
        assert!(version.supports_le_audio());
    }

    #[test]
    fn pre_5_2_controllers_are_rejected_for_le_audio() {
        let params = [0x00, 9, 0x00, 0x00, 9, 0x0F, 0x00, 0x00, 0x00];
        let version = LocalVersion::parse(&params).unwrap();

        assert_eq!(version.bluetooth_version(), "5.0");
        assert!(!version.supports_le_audio());
    }

    #[test]
    fn truncated_events_are_rejected() {
        assert!(Event::parse(&[0x0E]).is_none());
        assert!(Event::parse(&[0x0E, 0x04, 0x01]).is_none());
    }

    #[test]
    fn enhanced_connection_complete_yields_handle_and_address() {
        let mut raw = vec![0x3E, 0x1F, 0x0A, 0x00, 0x40, 0x00, 0x00, 0x00];
        raw.extend_from_slice(&[0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C]);
        raw.extend_from_slice(&[0u8; 19]);

        let event = Event::parse(&raw).unwrap();
        let (handle, address) = event.connection_complete().unwrap();

        assert_eq!(handle, 0x0040);
        assert_eq!(address.to_string(), "7C:FE:62:72:B4:9A");
    }
}
