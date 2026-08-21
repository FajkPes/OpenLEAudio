//! ACL fragmentation, L2CAP framing and the ATT protocol.
//!
//! Everything GATT needs rides on this: ACL packets carry L2CAP frames, L2CAP
//! channel 4 carries ATT, and ATT is how PACS and ASCS are read and written.
//! Getting the reassembly right matters, because a PAC record routinely exceeds
//! one ACL packet and arrives split across several.

/// L2CAP channel identifiers used on an LE link.
pub mod cid {
    pub const ATT: u16 = 0x0004;
    pub const LE_SIGNALING: u16 = 0x0005;
    pub const SMP: u16 = 0x0006;
}

/// ATT protocol opcodes.
pub mod att_op {
    pub const ERROR_RESPONSE: u8 = 0x01;
    pub const EXCHANGE_MTU_REQUEST: u8 = 0x02;
    pub const EXCHANGE_MTU_RESPONSE: u8 = 0x03;
    pub const FIND_INFORMATION_REQUEST: u8 = 0x04;
    pub const FIND_INFORMATION_RESPONSE: u8 = 0x05;
    pub const FIND_BY_TYPE_VALUE_REQUEST: u8 = 0x06;
    pub const READ_BY_TYPE_REQUEST: u8 = 0x08;
    pub const READ_BY_TYPE_RESPONSE: u8 = 0x09;
    pub const READ_REQUEST: u8 = 0x0A;
    pub const READ_RESPONSE: u8 = 0x0B;
    pub const READ_BLOB_REQUEST: u8 = 0x0C;
    pub const READ_BLOB_RESPONSE: u8 = 0x0D;
    pub const READ_MULTIPLE_REQUEST: u8 = 0x0E;
    pub const READ_BY_GROUP_TYPE_REQUEST: u8 = 0x10;
    pub const READ_BY_GROUP_TYPE_RESPONSE: u8 = 0x11;
    pub const WRITE_REQUEST: u8 = 0x12;
    pub const WRITE_RESPONSE: u8 = 0x13;
    pub const PREPARE_WRITE_REQUEST: u8 = 0x16;
    pub const EXECUTE_WRITE_REQUEST: u8 = 0x18;
    pub const READ_MULTIPLE_VARIABLE_REQUEST: u8 = 0x20;
    pub const HANDLE_VALUE_NOTIFICATION: u8 = 0x1B;
    pub const HANDLE_VALUE_INDICATION: u8 = 0x1D;
    pub const HANDLE_VALUE_CONFIRMATION: u8 = 0x1E;
}

/// GATT attribute type UUIDs used during discovery.
pub mod gatt_uuid {
    pub const PRIMARY_SERVICE: u16 = 0x2800;
    pub const CHARACTERISTIC: u16 = 0x2803;
    pub const CLIENT_CHARACTERISTIC_CONFIG: u16 = 0x2902;
}

/// ATT default MTU on an LE link, before any exchange.
pub const ATT_DEFAULT_MTU: u16 = 23;

/// MTU the stack asks for. Large enough that a full PAC record usually fits in
/// one response rather than needing Read Blob follow-ups.
pub const ATT_PREFERRED_MTU: u16 = 517;

#[derive(Debug, thiserror::Error)]
pub enum AttError {
    #[error("ATT error on handle {handle:#06x}: {} ({code:#04x})", error_name(*code))]
    Protocol { code: u8, handle: u16 },

    #[error("malformed {0} packet")]
    Malformed(&'static str),

    #[error("unexpected ATT response {got:#04x}, expected {expected:#04x}")]
    Unexpected { got: u8, expected: u8 },
}

/// Names for the ATT error codes worth distinguishing.
pub fn error_name(code: u8) -> &'static str {
    match code {
        0x01 => "invalid handle",
        0x02 => "read not permitted",
        0x03 => "write not permitted",
        0x05 => "insufficient authentication",
        0x06 => "request not supported",
        0x07 => "invalid offset",
        0x08 => "insufficient authorization",
        0x0A => "attribute not found",
        0x0C => "insufficient encryption key size",
        0x0D => "invalid attribute value length",
        0x0F => "insufficient encryption",
        _ => "see Core spec Part F",
    }
}

// ---- ACL layer ----

/// Packet boundary flag: first fragment, non-flushable. Host to controller only.
pub const PB_FIRST: u8 = 0b00;
/// Packet boundary flag: this ACL packet continues the previous frame.
pub const PB_CONTINUING: u8 = 0b01;
/// Packet boundary flag: first fragment, flushable.
///
/// This is what a controller uses for data travelling **up** to the host, so
/// every reply from a peer starts with this value and never with `PB_FIRST`.
/// Accepting only `PB_FIRST` silently discards every incoming L2CAP frame,
/// which looks exactly like a peer that never answers.
pub const PB_FIRST_FLUSHABLE: u8 = 0b10;
/// Packet boundary flag: a complete L2CAP PDU in one packet.
pub const PB_COMPLETE: u8 = 0b11;

/// Builds ACL packets carrying one L2CAP frame, split to the controller's limit.
pub fn build_acl_packets(handle: u16, cid: u16, payload: &[u8], max_acl_len: usize) -> Vec<Vec<u8>> {
    // L2CAP basic header: payload length, then channel id.
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(&cid.to_le_bytes());
    frame.extend_from_slice(payload);

    let chunk_size = max_acl_len.max(1);
    let mut packets = Vec::new();

    for (index, chunk) in frame.chunks(chunk_size).enumerate() {
        let boundary = if index == 0 { PB_FIRST } else { PB_CONTINUING };
        let header = (handle & 0x0FFF) | ((boundary as u16) << 12);

        let mut packet = Vec::with_capacity(4 + chunk.len());
        packet.extend_from_slice(&header.to_le_bytes());
        packet.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        packet.extend_from_slice(chunk);
        packets.push(packet);
    }

    packets
}

/// Reassembles L2CAP frames from a stream of ACL packets on one connection.
#[derive(Debug, Default)]
pub struct AclReassembler {
    buffer: Vec<u8>,
    expected: usize,
}

/// A complete L2CAP frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2capFrame {
    pub cid: u16,
    pub payload: Vec<u8>,
}

impl AclReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one ACL packet. Returns a frame once enough fragments have arrived.
    ///
    /// A continuation without a start, or a length that never adds up, resets the
    /// reassembler rather than growing without bound.
    pub fn push(&mut self, packet: &[u8]) -> Result<Option<L2capFrame>, AttError> {
        if packet.len() < 4 {
            return Err(AttError::Malformed("ACL"));
        }

        let header = u16::from_le_bytes([packet[0], packet[1]]);
        let boundary = ((header >> 12) & 0b11) as u8;
        let data_len = u16::from_le_bytes([packet[2], packet[3]]) as usize;

        if packet.len() < 4 + data_len {
            return Err(AttError::Malformed("ACL"));
        }
        let data = &packet[4..4 + data_len];

        match boundary {
            PB_FIRST | PB_FIRST_FLUSHABLE | PB_COMPLETE => {
                if data.len() < 4 {
                    return Err(AttError::Malformed("L2CAP"));
                }
                let payload_len = u16::from_le_bytes([data[0], data[1]]) as usize;
                self.expected = 4 + payload_len;
                self.buffer.clear();
                self.buffer.extend_from_slice(data);
            }
            PB_CONTINUING => {
                if self.buffer.is_empty() {
                    // Continuation with nothing to continue: drop it.
                    return Ok(None);
                }
                self.buffer.extend_from_slice(data);
            }
            _ => return Ok(None),
        }

        if self.buffer.len() < self.expected {
            return Ok(None);
        }

        // Overshoot means the peer lied about a length; start over.
        if self.buffer.len() > self.expected {
            self.buffer.clear();
            self.expected = 0;
            return Err(AttError::Malformed("L2CAP"));
        }

        let cid = u16::from_le_bytes([self.buffer[2], self.buffer[3]]);
        let payload = self.buffer[4..].to_vec();
        self.buffer.clear();
        self.expected = 0;

        Ok(Some(L2capFrame { cid, payload }))
    }

    /// Forgets any partial frame, used when a link drops.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.expected = 0;
    }
}

// ---- ATT requests ----

pub fn exchange_mtu_request(mtu: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::EXCHANGE_MTU_REQUEST];
    pdu.extend_from_slice(&mtu.to_le_bytes());
    pdu
}

/// Answers a peer-initiated MTU exchange. Some headsets send this immediately
/// after encryption is restored, at the same time as our own exchange.
pub fn exchange_mtu_response(mtu: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::EXCHANGE_MTU_RESPONSE];
    pdu.extend_from_slice(&mtu.to_le_bytes());
    pdu
}

/// Answers an ATT request initiated by the peer while we are waiting for our
/// own GATT response.
///
/// OpenLEAudio is a GATT client and intentionally exposes no local attributes.
/// Some headsets nevertheless act as a client too and probe the host for the
/// optional Database Hash characteristic (0x2B2A). ATT permits one outstanding
/// request in each direction, so this can legitimately cross service discovery.
/// Returning the protocol-level absence/error response lets both directions
/// finish without pretending that the peer request was our response.
pub fn absent_local_attribute_response(pdu: &[u8]) -> Option<Vec<u8>> {
    let opcode = *pdu.first()?;
    let (handle, error) = match opcode {
        att_op::FIND_INFORMATION_REQUEST
        | att_op::FIND_BY_TYPE_VALUE_REQUEST
        | att_op::READ_BY_TYPE_REQUEST
        | att_op::READ_BY_GROUP_TYPE_REQUEST if pdu.len() >= 3 => {
            (u16::from_le_bytes([pdu[1], pdu[2]]), 0x0A) // Attribute Not Found
        }
        att_op::READ_REQUEST
        | att_op::READ_BLOB_REQUEST
        | att_op::READ_MULTIPLE_REQUEST
        | att_op::WRITE_REQUEST
        | att_op::PREPARE_WRITE_REQUEST
        | att_op::READ_MULTIPLE_VARIABLE_REQUEST if pdu.len() >= 3 => {
            (u16::from_le_bytes([pdu[1], pdu[2]]), 0x01) // Invalid Handle
        }
        att_op::EXECUTE_WRITE_REQUEST => (0x0000, 0x06), // Request Not Supported
        _ => return None,
    };

    Some(vec![att_op::ERROR_RESPONSE, opcode, handle as u8, (handle >> 8) as u8, error])
}

/// Discovers primary services in a handle range.
pub fn read_by_group_type_request(start: u16, end: u16, group_type: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::READ_BY_GROUP_TYPE_REQUEST];
    pdu.extend_from_slice(&start.to_le_bytes());
    pdu.extend_from_slice(&end.to_le_bytes());
    pdu.extend_from_slice(&group_type.to_le_bytes());
    pdu
}

/// Discovers characteristics in a handle range.
pub fn read_by_type_request(start: u16, end: u16, attribute_type: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::READ_BY_TYPE_REQUEST];
    pdu.extend_from_slice(&start.to_le_bytes());
    pdu.extend_from_slice(&end.to_le_bytes());
    pdu.extend_from_slice(&attribute_type.to_le_bytes());
    pdu
}

pub fn read_request(handle: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::READ_REQUEST];
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu
}

/// Continues a read that did not fit in one response.
pub fn read_blob_request(handle: u16, offset: u16) -> Vec<u8> {
    let mut pdu = vec![att_op::READ_BLOB_REQUEST];
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu.extend_from_slice(&offset.to_le_bytes());
    pdu
}

pub fn write_request(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut pdu = vec![att_op::WRITE_REQUEST];
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu.extend_from_slice(value);
    pdu
}

// ---- ATT responses ----

/// A discovered primary service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRange {
    pub start_handle: u16,
    pub end_handle: u16,
    pub uuid: Uuid,
}

/// A discovered characteristic declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Characteristic {
    pub declaration_handle: u16,
    pub properties: u8,
    pub value_handle: u16,
    pub uuid: Uuid,
}

impl Characteristic {
    pub fn is_readable(&self) -> bool {
        self.properties & 0x02 != 0
    }

    pub fn is_writable(&self) -> bool {
        self.properties & 0x08 != 0
    }

    pub fn supports_notify(&self) -> bool {
        self.properties & 0x10 != 0
    }
}

/// A Bluetooth UUID, which appears on the wire as either 16 or 128 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Uuid {
    Short(u16),
    Long([u8; 16]),
}

impl Uuid {
    /// The 16-bit form, for the SIG-assigned UUIDs the stack looks for.
    pub fn as_short(&self) -> Option<u16> {
        match self {
            Uuid::Short(value) => Some(*value),
            // 128-bit form of a SIG UUID is the base UUID with bytes 12..14 replaced.
            Uuid::Long(bytes) => {
                const BASE_SUFFIX: [u8; 12] = [
                    0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00,
                ];
                if bytes[0..12] == BASE_SUFFIX {
                    Some(u16::from_le_bytes([bytes[12], bytes[13]]))
                } else {
                    None
                }
            }
        }
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes.len() {
            2 => Some(Uuid::Short(u16::from_le_bytes([bytes[0], bytes[1]]))),
            16 => {
                let mut value = [0u8; 16];
                value.copy_from_slice(bytes);
                Some(Uuid::Long(value))
            }
            _ => None,
        }
    }
}

/// Checks an ATT response for the protocol-level error PDU.
pub fn check_error(pdu: &[u8]) -> Result<(), AttError> {
    if pdu.first() == Some(&att_op::ERROR_RESPONSE) {
        if pdu.len() < 5 {
            return Err(AttError::Malformed("ATT error"));
        }
        return Err(AttError::Protocol {
            code: pdu[4],
            handle: u16::from_le_bytes([pdu[2], pdu[3]]),
        });
    }
    Ok(())
}

/// Parses Read By Group Type Response into service ranges.
pub fn parse_service_ranges(pdu: &[u8]) -> Result<Vec<ServiceRange>, AttError> {
    check_error(pdu)?;

    if pdu.first() != Some(&att_op::READ_BY_GROUP_TYPE_RESPONSE) || pdu.len() < 2 {
        return Err(AttError::Unexpected {
            got: pdu.first().copied().unwrap_or(0),
            expected: att_op::READ_BY_GROUP_TYPE_RESPONSE,
        });
    }

    let entry_len = pdu[1] as usize;
    if entry_len < 6 {
        return Err(AttError::Malformed("group type response"));
    }

    let mut services = Vec::new();
    for entry in pdu[2..].chunks(entry_len) {
        if entry.len() < entry_len {
            break;
        }

        let uuid = Uuid::parse(&entry[4..entry_len])
            .ok_or(AttError::Malformed("group type response"))?;

        services.push(ServiceRange {
            start_handle: u16::from_le_bytes([entry[0], entry[1]]),
            end_handle: u16::from_le_bytes([entry[2], entry[3]]),
            uuid,
        });
    }

    Ok(services)
}

/// Parses Read By Type Response carrying characteristic declarations.
pub fn parse_characteristics(pdu: &[u8]) -> Result<Vec<Characteristic>, AttError> {
    check_error(pdu)?;

    if pdu.first() != Some(&att_op::READ_BY_TYPE_RESPONSE) || pdu.len() < 2 {
        return Err(AttError::Unexpected {
            got: pdu.first().copied().unwrap_or(0),
            expected: att_op::READ_BY_TYPE_RESPONSE,
        });
    }

    let entry_len = pdu[1] as usize;
    // handle(2) properties(1) value_handle(2) uuid(2 or 16)
    if entry_len < 7 {
        return Err(AttError::Malformed("characteristic response"));
    }

    let mut characteristics = Vec::new();
    for entry in pdu[2..].chunks(entry_len) {
        if entry.len() < entry_len {
            break;
        }

        let uuid = Uuid::parse(&entry[5..entry_len])
            .ok_or(AttError::Malformed("characteristic response"))?;

        characteristics.push(Characteristic {
            declaration_handle: u16::from_le_bytes([entry[0], entry[1]]),
            properties: entry[2],
            value_handle: u16::from_le_bytes([entry[3], entry[4]]),
            uuid,
        });
    }

    Ok(characteristics)
}

/// Extracts the value from a Read Response or Read Blob Response.
pub fn parse_read_response(pdu: &[u8]) -> Result<Vec<u8>, AttError> {
    check_error(pdu)?;

    match pdu.first() {
        Some(&att_op::READ_RESPONSE) | Some(&att_op::READ_BLOB_RESPONSE) => Ok(pdu[1..].to_vec()),
        got => Err(AttError::Unexpected {
            got: got.copied().unwrap_or(0),
            expected: att_op::READ_RESPONSE,
        }),
    }
}

/// Extracts the negotiated MTU, which is the smaller of the two proposals.
pub fn parse_mtu_response(pdu: &[u8], requested: u16) -> Result<u16, AttError> {
    check_error(pdu)?;

    if pdu.first() != Some(&att_op::EXCHANGE_MTU_RESPONSE) || pdu.len() < 3 {
        return Err(AttError::Malformed("MTU response"));
    }

    let theirs = u16::from_le_bytes([pdu[1], pdu[2]]);
    Ok(requested.min(theirs).max(ATT_DEFAULT_MTU))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_payload_becomes_one_acl_packet() {
        let packets = build_acl_packets(0x0040, cid::ATT, &[0x02, 0x05, 0x02], 27);
        assert_eq!(packets.len(), 1);

        let packet = &packets[0];
        // handle 0x0040 with PB_FIRST in the top bits
        assert_eq!(u16::from_le_bytes([packet[0], packet[1]]) & 0x0FFF, 0x0040);
        assert_eq!(u16::from_le_bytes([packet[2], packet[3]]), 7); // 4 L2CAP + 3 payload
        assert_eq!(u16::from_le_bytes([packet[4], packet[5]]), 3); // L2CAP length
        assert_eq!(u16::from_le_bytes([packet[6], packet[7]]), cid::ATT);
    }

    #[test]
    fn large_payload_is_fragmented_and_reassembled() {
        // A realistic PAC record: bigger than one ACL packet at the default size.
        let payload: Vec<u8> = (0..100u8).collect();
        let packets = build_acl_packets(0x0040, cid::ATT, &payload, 27);
        assert!(packets.len() > 1, "payload should need several fragments");

        let mut reassembler = AclReassembler::new();
        let mut frame = None;
        for packet in &packets {
            if let Some(f) = reassembler.push(packet).unwrap() {
                frame = Some(f);
            }
        }

        let frame = frame.expect("fragments should reassemble into one frame");
        assert_eq!(frame.cid, cid::ATT);
        assert_eq!(frame.payload, payload);
    }

    /// The regression this codifies cost days: replies from the peer were
    /// arriving intact and being discarded because their boundary flag was the
    /// controller-to-host one, which the reassembler did not recognise. It
    /// presented as a peer that never answered anything.
    #[test]
    fn frames_arriving_from_the_controller_are_reassembled() {
        // Handle 0x000C, PB 0b10, one ATT PDU of three bytes.
        let incoming = [
            0x0C, 0x20, // handle 0x000C, boundary 0b10
            0x07, 0x00, // ACL data length
            0x03, 0x00, // L2CAP payload length
            0x04, 0x00, // channel: ATT
            0x03, 0x17, 0x00, // Exchange MTU Response, MTU 23
        ];

        let mut reassembler = AclReassembler::new();
        let frame = reassembler
            .push(&incoming)
            .unwrap()
            .expect("a controller-to-host frame must reassemble");

        assert_eq!(frame.cid, cid::ATT);
        assert_eq!(frame.payload, vec![0x03, 0x17, 0x00]);
    }

    #[test]
    fn a_complete_pdu_in_one_packet_is_accepted() {
        // Same frame, but flagged as a complete PDU rather than a first fragment.
        let incoming = [
            0x0C, 0x30, // handle 0x000C, boundary 0b11
            0x07, 0x00, 0x03, 0x00, 0x06, 0x00, // channel: SMP
            0x02, 0x03, 0x00,
        ];

        let mut reassembler = AclReassembler::new();
        let frame = reassembler.push(&incoming).unwrap().expect("must reassemble");
        assert_eq!(frame.cid, cid::SMP);
    }

    #[test]
    fn continuation_without_a_start_is_dropped() {
        let mut reassembler = AclReassembler::new();
        // PB_CONTINUING with no preceding start.
        let packet = [0x40, 0x10, 0x02, 0x00, 0xAA, 0xBB];
        assert_eq!(reassembler.push(&packet).unwrap(), None);
    }

    #[test]
    fn truncated_acl_is_rejected() {
        let mut reassembler = AclReassembler::new();
        assert!(reassembler.push(&[0x40, 0x00]).is_err());
        // Header claims 10 bytes, packet carries 2.
        assert!(reassembler.push(&[0x40, 0x00, 0x0A, 0x00, 0x01, 0x02]).is_err());
    }

    #[test]
    fn error_response_is_surfaced_with_its_code() {
        // Error on Read Request, handle 0x0025, insufficient encryption.
        let pdu = [att_op::ERROR_RESPONSE, att_op::READ_REQUEST, 0x25, 0x00, 0x0F];
        let error = parse_read_response(&pdu).unwrap_err();

        match error {
            AttError::Protocol { code, handle } => {
                assert_eq!(code, 0x0F);
                assert_eq!(handle, 0x0025);
                assert_eq!(error_name(code), "insufficient encryption");
            }
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[test]
    fn service_discovery_parses_short_uuids() {
        // opcode, entry_len=6, then two services
        let pdu = [
            att_op::READ_BY_GROUP_TYPE_RESPONSE,
            0x06,
            0x01, 0x00, 0x09, 0x00, 0x50, 0x18, // 0x0001-0x0009, PACS
            0x0A, 0x00, 0x20, 0x00, 0x4E, 0x18, // 0x000A-0x0020, ASCS
        ];

        let services = parse_service_ranges(&pdu).unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].uuid.as_short(), Some(0x1850));
        assert_eq!(services[0].start_handle, 0x0001);
        assert_eq!(services[1].uuid.as_short(), Some(0x184E));
    }

    #[test]
    fn characteristic_discovery_reports_properties() {
        // Sink PAC characteristic 0x2BC9, readable and notifiable.
        let pdu = [
            att_op::READ_BY_TYPE_RESPONSE,
            0x07,
            0x02, 0x00,       // declaration handle
            0x12,             // properties: read | notify
            0x03, 0x00,       // value handle
            0xC9, 0x2B,       // UUID 0x2BC9
        ];

        let characteristics = parse_characteristics(&pdu).unwrap();
        assert_eq!(characteristics.len(), 1);

        let sink_pac = &characteristics[0];
        assert_eq!(sink_pac.uuid.as_short(), Some(0x2BC9));
        assert_eq!(sink_pac.value_handle, 0x0003);
        assert!(sink_pac.is_readable());
        assert!(sink_pac.supports_notify());
        assert!(!sink_pac.is_writable());
    }

    #[test]
    fn long_form_of_a_sig_uuid_is_recognised() {
        // 128-bit form of 0x1850, little-endian on the wire.
        let long = Uuid::Long([
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80,
            0x00, 0x10, 0x00, 0x00, 0x50, 0x18, 0x00, 0x00,
        ]);
        assert_eq!(long.as_short(), Some(0x1850));

        // A vendor UUID must not be mistaken for a SIG one.
        let vendor = Uuid::Long([0xAA; 16]);
        assert_eq!(vendor.as_short(), None);
    }

    #[test]
    fn mtu_negotiation_takes_the_smaller_value() {
        let response = [att_op::EXCHANGE_MTU_RESPONSE, 0x40, 0x00]; // peer offers 64
        assert_eq!(parse_mtu_response(&response, ATT_PREFERRED_MTU).unwrap(), 64);

        // Never below the protocol default, whatever the peer claims.
        let tiny = [att_op::EXCHANGE_MTU_RESPONSE, 0x05, 0x00];
        assert_eq!(parse_mtu_response(&tiny, ATT_PREFERRED_MTU).unwrap(), ATT_DEFAULT_MTU);
    }

    #[test]
    fn a_peer_database_hash_probe_gets_attribute_not_found() {
        // Read By Type 0x0001..0xFFFF, UUID 0x2B2A (Database Hash).
        let request = [att_op::READ_BY_TYPE_REQUEST, 0x01, 0x00, 0xFF, 0xFF, 0x2A, 0x2B];
        assert_eq!(
            absent_local_attribute_response(&request),
            Some(vec![att_op::ERROR_RESPONSE, att_op::READ_BY_TYPE_REQUEST, 0x01, 0x00, 0x0A])
        );
    }

    #[test]
    fn responses_are_never_mistaken_for_peer_requests() {
        assert_eq!(
            absent_local_attribute_response(&[att_op::READ_BY_TYPE_RESPONSE, 0x07]),
            None
        );
        assert_eq!(
            absent_local_attribute_response(&[att_op::EXCHANGE_MTU_RESPONSE, 0x00, 0x02]),
            None
        );
    }

    #[test]
    fn a_peer_mtu_request_can_be_answered() {
        assert_eq!(exchange_mtu_response(ATT_PREFERRED_MTU), [0x03, 0x05, 0x02]);
    }
}
