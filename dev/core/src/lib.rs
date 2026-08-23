//! OpenLEAudio - user-mode Bluetooth LE Audio host stack.
//!
//! The goal is full control over the LC3 stream: bitrate, sampling frequency,
//! frame duration, transport latency and CIS topology - all the parameters the
//! Microsoft LE Audio driver decides for you and never exposes.

#[cfg(windows)]
pub mod audio;
pub mod att;
pub mod bap;
pub mod bonding;
pub mod controller;
pub mod environment;
pub mod hci;
pub mod l2cap;
pub mod link;
pub mod multipoint;
pub mod safety;
#[cfg(windows)]
pub mod session;
pub mod settings;
pub mod smp;
pub mod stream;
pub mod trace;
pub mod transport;
pub mod vcs;

#[cfg(windows)]
pub mod winusb;

pub use bap::{CodecCapabilities, CodecConfiguration, PacRecord, Preset, QosConfiguration};
pub use att::{AclReassembler, AttError, Characteristic, L2capFrame, ServiceRange, Uuid};
pub use controller::{Controller, ControllerError, DiscoveredDevice};
pub use safety::{OutputLimiter, SafetyViolation, WritePolicy};
pub use stream::{AudioEncoder, StreamPlan, Topology};
pub use link::{AudioCapabilities, HciPump, Link, LinkError};
pub use hci::{BdAddr, Event, LocalVersion};
pub use transport::{ControllerInfo, TransportError, UsbTransport};
