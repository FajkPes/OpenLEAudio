//! Raw wire logging, for when a layer goes quiet and nobody can say why.
//!
//! Every HCI command, event and ACL packet passes through the transport, so
//! that is the one place where the whole conversation can be seen. Without it
//! "the peer did not answer" and "we never sent anything the peer could answer"
//! look identical from the outside, and the difference decides where to look.
//!
//! Off by default and free when off: one relaxed atomic load per packet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

pub fn enable() {
    start();
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn disable() {
    ENABLED.store(false, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Which way a packet went, and on which pipe.
#[derive(Debug, Clone, Copy)]
pub enum Wire {
    Command,
    Event,
    AclOut,
    AclIn,
    IsoOut,
}

impl Wire {
    fn label(self) -> &'static str {
        match self {
            // Arrows read from the host's point of view: out is towards the
            // controller, in is back from it.
            Wire::Command => "--> CMD",
            Wire::Event => "<-- EVT",
            Wire::AclOut => "--> ACL",
            Wire::AclIn => "<-- ACL",
            Wire::IsoOut => "--> ISO",
        }
    }
}

/// Logs one packet, if tracing is on.
pub fn packet(wire: Wire, bytes: &[u8]) {
    if !is_enabled() {
        return;
    }

    let millis = start().elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "  [{millis:9.3} ms] {} {:3} B  {}{}",
        wire.label(),
        bytes.len(),
        hex(bytes),
        annotate(wire, bytes)
    );
}

/// Logs a note alongside the packet stream, on the same timeline.
pub fn note(text: &str) {
    if !is_enabled() {
        return;
    }
    let millis = start().elapsed().as_secs_f64() * 1000.0;
    eprintln!("  [{millis:9.3} ms] ... {text}");
}

/// At most the first 24 bytes; the rest is noise for this purpose.
fn hex(bytes: &[u8]) -> String {
    let shown: Vec<String> = bytes.iter().take(24).map(|b| format!("{b:02X}")).collect();
    let mut text = shown.join(" ");
    if bytes.len() > 24 {
        text.push_str(" ...");
    }
    text
}

/// A short human-readable tail, so the dump can be read without a spec to hand.
fn annotate(wire: Wire, bytes: &[u8]) -> String {
    match wire {
        Wire::Command if bytes.len() >= 3 => {
            let opcode = u16::from_le_bytes([bytes[0], bytes[1]]);
            format!("   opcode {opcode:#06x}")
        }
        Wire::Event if bytes.len() >= 2 => {
            let code = bytes[0];
            let name = match code {
                0x05 => " Disconnection Complete",
                0x0E => " Command Complete",
                0x0F => " Command Status",
                0x13 => " Number Of Completed Packets",
                0x3E => " LE Meta",
                _ => "",
            };
            if code == 0x3E && bytes.len() >= 3 {
                format!("   event {code:#04x}{name}, subevent {:#04x}", bytes[2])
            } else {
                format!("   event {code:#04x}{name}")
            }
        }
        Wire::AclOut | Wire::AclIn if bytes.len() >= 4 => {
            let header = u16::from_le_bytes([bytes[0], bytes[1]]);
            let handle = header & 0x0FFF;
            let boundary = (header >> 12) & 0b11;
            let length = u16::from_le_bytes([bytes[2], bytes[3]]);

            // A first fragment carries the L2CAP header, so the channel can be
            // named; a continuation cannot, and saying so beats guessing.
            if boundary == 0b01 || bytes.len() < 8 {
                return format!("   handle {handle:#06x}, pb {boundary}, len {length}");
            }

            let cid = u16::from_le_bytes([bytes[6], bytes[7]]);
            let channel = match cid {
                0x0004 => " ATT",
                0x0005 => " LE signalling",
                0x0006 => " SMP",
                _ => "",
            };
            format!("   handle {handle:#06x}, pb {boundary}, len {length}, cid {cid:#06x}{channel}")
        }
        _ => String::new(),
    }
}
