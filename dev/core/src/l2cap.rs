//! L2CAP signalling on the LE channel.
//!
//! Only one exchange matters here, but ignoring it is fatal. A peripheral asks
//! the central to slow the connection down so it can save power, and the
//! specification gives that request a response timer (Vol 3 Part A, 6.2.1) whose
//! maximum is 60 seconds. A central that never answers gets the link torn down
//! exactly one minute later, with a reason code - "remote user terminated" -
//! that points at the peer rather than at the silence which caused it.

/// LE signalling opcodes, of which we handle exactly one.
pub mod code {
    pub const CONNECTION_PARAMETER_UPDATE_REQUEST: u8 = 0x12;
    pub const CONNECTION_PARAMETER_UPDATE_RESPONSE: u8 = 0x13;
}

pub const RESULT_ACCEPTED: u16 = 0x0000;
pub const RESULT_REJECTED: u16 = 0x0001;

/// Connection parameters a peripheral has asked for, in raw HCI units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionParameters {
    pub interval_min: u16,
    pub interval_max: u16,
    pub latency: u16,
    pub supervision_timeout: u16,
}

impl ConnectionParameters {
    /// The connection interval range in milliseconds, for reporting.
    pub fn interval_ms(&self) -> (f32, f32) {
        (self.interval_min as f32 * 1.25, self.interval_max as f32 * 1.25)
    }
}

/// A signalling PDU we recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    ParameterUpdateRequest { identifier: u8, parameters: ConnectionParameters },
}

/// Reads a signalling PDU, returning `None` for anything we do not act on.
pub fn parse(payload: &[u8]) -> Option<Signal> {
    let &[code, identifier, len_lo, len_hi, ref data @ ..] = payload else {
        return None;
    };

    let length = u16::from_le_bytes([len_lo, len_hi]) as usize;
    if data.len() < length {
        return None;
    }

    if code != code::CONNECTION_PARAMETER_UPDATE_REQUEST || length != 8 {
        return None;
    }

    Some(Signal::ParameterUpdateRequest {
        identifier,
        parameters: ConnectionParameters {
            interval_min: u16::from_le_bytes([data[0], data[1]]),
            interval_max: u16::from_le_bytes([data[2], data[3]]),
            latency: u16::from_le_bytes([data[4], data[5]]),
            supervision_timeout: u16::from_le_bytes([data[6], data[7]]),
        },
    })
}

/// Builds the response that stops the peer's one-minute timer.
pub fn parameter_update_response(identifier: u8, result: u16) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(6);
    pdu.push(code::CONNECTION_PARAMETER_UPDATE_RESPONSE);
    pdu.push(identifier);
    pdu.extend_from_slice(&2u16.to_le_bytes());
    pdu.extend_from_slice(&result.to_le_bytes());
    pdu
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes the JBL headphones sent, which went unanswered for a
    /// minute before they dropped the link.
    const CAPTURED_REQUEST: [u8; 12] =
        [0x12, 0x01, 0x08, 0x00, 0x30, 0x00, 0x30, 0x00, 0x00, 0x00, 0xF4, 0x01];

    #[test]
    fn the_captured_request_is_understood() {
        let Some(Signal::ParameterUpdateRequest { identifier, parameters }) = parse(&CAPTURED_REQUEST)
        else {
            panic!("the request from the capture was not recognised");
        };

        assert_eq!(identifier, 0x01);
        assert_eq!(parameters.interval_min, 0x0030);
        assert_eq!(parameters.interval_max, 0x0030);
        assert_eq!(parameters.latency, 0);
        assert_eq!(parameters.supervision_timeout, 0x01F4);
        assert_eq!(parameters.interval_ms(), (60.0, 60.0));
    }

    #[test]
    fn the_response_carries_the_requests_identifier() {
        let response = parameter_update_response(0x01, RESULT_ACCEPTED);
        assert_eq!(response, vec![0x13, 0x01, 0x02, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn a_truncated_request_is_not_guessed_at() {
        assert_eq!(parse(&CAPTURED_REQUEST[..8]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn other_signalling_is_left_alone() {
        // A disconnect request, which we neither parse nor answer.
        assert_eq!(parse(&[0x06, 0x01, 0x04, 0x00, 0x40, 0x00, 0x40, 0x00]), None);
    }
}
