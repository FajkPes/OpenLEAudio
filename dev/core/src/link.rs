//! GATT over an established LE connection.
//!
//! Events arrive on interrupt IN and ACL data on bulk IN. Both reads block, so
//! each gets its own thread feeding one channel. Everything above this point
//! sees a single ordered stream and can wait with a timeout instead of hanging
//! forever on a device that stopped answering.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crate::att::{self, att_op, cid, AclReassembler, AttError, Characteristic, ServiceRange};
use crate::bap::PacRecord;
use crate::hci::Event;
use crate::transport::UsbTransport;

/// How long any single ATT exchange may take before we give up on it.
pub const ATT_TIMEOUT: Duration = Duration::from_secs(5);

/// Handles reserved by the SIG for PACS and ASCS characteristics.
pub mod pacs_uuid {
    pub const SINK_PAC: u16 = 0x2BC9;
    pub const SINK_AUDIO_LOCATIONS: u16 = 0x2BCA;
    pub const SOURCE_PAC: u16 = 0x2BCB;
    pub const AVAILABLE_CONTEXTS: u16 = 0x2BCD;
    pub const SUPPORTED_CONTEXTS: u16 = 0x2BCE;

    pub const SINK_ASE: u16 = 0x2BC4;
    pub const SOURCE_ASE: u16 = 0x2BC5;
    pub const ASE_CONTROL_POINT: u16 = 0x2BC6;

    pub const SERVICE_PACS: u16 = 0x1850;
    pub const SERVICE_ASCS: u16 = 0x184E;
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Att(#[from] AttError),

    #[error("no response within {0:?}")]
    Timeout(Duration),

    #[error("link closed")]
    Closed,

    #[error("transport error: {0}")]
    Transport(String),

    #[error("service {0:#06x} not found on this device")]
    ServiceMissing(u16),

    #[error("characteristic {0:#06x} not found")]
    CharacteristicMissing(u16),

    #[error(transparent)]
    Unsafe(#[from] crate::safety::SafetyViolation),

    #[error("protejsek spojeni ukoncil: {} (kod {reason:#04x})", crate::hci::disconnect_reason(*reason))]
    Disconnected { reason: u8 },
}

type Result<T> = std::result::Result<T, LinkError>;

/// Reader threads turning two blocking pipes into two queues.
///
/// Events and ACL data are kept apart on purpose. When they shared one channel,
/// whichever layer happened to be waiting consumed whatever arrived - so an SMP
/// response could be pulled out by the command loop, which discards ACL, and
/// then waited for forever by the layer that actually wanted it.
///
/// There must be exactly one pump per adapter. Two pumps means two threads
/// blocked on the same endpoint, and each incoming packet goes to whichever wins
/// the race, which loses roughly half of them.
pub struct HciPump {
    events: Receiver<Event>,
    acl: Receiver<Vec<u8>>,
    /// Events looked at by one layer and handed back for another to consume.
    /// Peeking for a dropped link must not destroy what a later step awaits.
    held: RefCell<VecDeque<Event>>,
}

impl HciPump {
    /// Starts reading. The threads end on their own when the device goes away,
    /// because the blocking read then fails rather than blocking forever.
    pub fn start(transport: UsbTransport) -> Self {
        let (event_sender, events) = mpsc::channel();
        let (acl_sender, acl) = mpsc::channel();

        let event_transport = transport.clone();
        thread::spawn(move || loop {
            match event_transport.read_event() {
                Ok(raw) => match Event::parse(&raw) {
                    Some(event) => {
                        if event_sender.send(event).is_err() {
                            break; // receiver dropped
                        }
                    }
                    None => continue, // malformed event, keep going
                },
                Err(_) => break,
            }
        });

        let acl_transport = transport;
        thread::spawn(move || loop {
            match acl_transport.read_acl() {
                Ok(packet) => {
                    if acl_sender.send(packet).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        });

        Self {
            events,
            acl,
            held: RefCell::new(VecDeque::new()),
        }
    }

    /// Next HCI event. Never consumes ACL data.
    pub fn recv_event(&self, timeout: Duration) -> Result<Event> {
        if let Some(event) = self.held.borrow_mut().pop_front() {
            return Ok(event);
        }
        Self::take(self.events.recv_timeout(timeout), timeout)
    }

    /// An event if one is already waiting, without blocking.
    pub fn try_recv_event(&self) -> Option<Event> {
        if let Some(event) = self.held.borrow_mut().pop_front() {
            return Some(event);
        }
        self.events.try_recv().ok()
    }

    /// Returns events to the queue for whoever actually wants them.
    ///
    /// They go to the front, in order, so a caller that looked at the queue and
    /// handed it back leaves it exactly as it found it. Returning them one at a
    /// time from inside a drain loop would be an infinite loop: `try_recv_event`
    /// reads this same queue, so each event would be pulled straight back out.
    pub fn put_back_events(&self, events: Vec<Event>) {
        let mut held = self.held.borrow_mut();
        for event in events.into_iter().rev() {
            held.push_front(event);
        }
    }

    /// Next ACL packet. Never consumes events.
    pub fn recv_acl(&self, timeout: Duration) -> Result<Vec<u8>> {
        Self::take(self.acl.recv_timeout(timeout), timeout)
    }

    fn take<T>(
        result: std::result::Result<T, RecvTimeoutError>,
        timeout: Duration,
    ) -> Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => Err(LinkError::Timeout(timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(LinkError::Closed),
        }
    }
}

/// A discovered service with its characteristics.
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    pub range: ServiceRange,
    pub characteristics: Vec<Characteristic>,
}

impl DiscoveredService {
    pub fn characteristic(&self, uuid: u16) -> Option<&Characteristic> {
        self.characteristics
            .iter()
            .find(|c| c.uuid.as_short() == Some(uuid))
    }
}

/// Everything read out of a device's PACS.
#[derive(Debug, Clone, Default)]
pub struct AudioCapabilities {
    pub sink_records: Vec<PacRecord>,
    pub source_records: Vec<PacRecord>,
    pub sink_locations: Option<u32>,
    pub available_contexts: Option<u16>,
    pub supported_contexts: Option<u16>,
    pub sink_ase_ids: Vec<u8>,
    pub source_ase_ids: Vec<u8>,
}

/// Where volume lives on a device that has a Volume Control Service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeControlHandles {
    pub state: u16,
    pub state_cccd: u16,
    pub control_point: u16,
}

/// A GATT client bound to one connection handle.
pub struct Link {
    transport: UsbTransport,
    pump: Rc<HciPump>,
    handle: u16,
    reassembler: AclReassembler,
    mtu: u16,
    max_acl_payload: usize,
    write_policy: crate::safety::WritePolicy,
    /// Unsolicited values the device sent, in arrival order.
    notifications: Vec<(u16, Vec<u8>)>,
    att_timeout: Duration,
}

impl Link {
    pub fn new(transport: UsbTransport, pump: Rc<HciPump>, handle: u16) -> Self {
        Self {
            transport,
            pump,
            handle,
            reassembler: AclReassembler::new(),
            mtu: att::ATT_DEFAULT_MTU,
            max_acl_payload: 27,
            write_policy: crate::safety::WritePolicy::default(),
            notifications: Vec::new(),
            att_timeout: ATT_TIMEOUT,
        }
    }

    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// Sends one ATT PDU and waits for the matching response.
    ///
    /// Events and traffic on other channels are skipped rather than treated as
    /// answers, so a notification arriving mid-request does not derail the read.
    fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>> {
        for packet in att::build_acl_packets(self.handle, cid::ATT, pdu, self.max_acl_payload) {
            self.transport
                .send_acl(&packet)
                .map_err(|e| LinkError::Transport(e.to_string()))?;
        }

        let budget = self.att_timeout;
        let deadline = std::time::Instant::now() + budget;

        loop {
            let raw = self.recv_acl_watching_link(deadline, budget)?;

            let frame = match self.reassembler.push(&raw) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(_) => continue, // damaged fragment, wait for the next one
            };

            if frame.cid != cid::ATT {
                Self::answer_signalling(&self.transport, self.handle, self.max_acl_payload, &frame);
                continue;
            }

            // Notifications are unsolicited; they are not the response we asked
            // for - but they are the device's own answer to what we last wrote,
            // so they are kept rather than dropped. Discarding them is how a
            // rejected configuration comes to look exactly like an accepted one.
            match frame.payload.first() {
                Some(&att_op::HANDLE_VALUE_NOTIFICATION) | Some(&att_op::HANDLE_VALUE_INDICATION) => {
                    if let &[_, lo, hi, ref value @ ..] = frame.payload.as_slice() {
                        self.notifications
                            .push((u16::from_le_bytes([lo, hi]), value.to_vec()));
                    }
                    // An indication, unlike a notification, owns the ATT bearer
                    // until it is confirmed. Without this one-byte answer some
                    // headsets stop replying to the request that follows it.
                    if frame.payload.first() == Some(&att_op::HANDLE_VALUE_INDICATION) {
                        self.send_att_without_wait(&[att_op::HANDLE_VALUE_CONFIRMATION])?;
                    }
                    continue;
                }
                Some(&att_op::EXCHANGE_MTU_REQUEST) if frame.payload.len() >= 3 => {
                    // A peripheral may initiate MTU exchange immediately after
                    // bonded encryption comes back. It can cross our request on
                    // the wire; treating it as our response produced the exact
                    // "malformed MTU response" failure seen on the JBL headset.
                    let peer = u16::from_le_bytes([frame.payload[1], frame.payload[2]]);
                    self.mtu = att::ATT_PREFERRED_MTU.min(peer).max(att::ATT_DEFAULT_MTU);
                    self.send_att_without_wait(&att::exchange_mtu_response(att::ATT_PREFERRED_MTU))?;
                    continue;
                }
                _ => {
                    // The peripheral may be a GATT client at the same time. JBL
                    // probes the optional host Database Hash (0x2B2A) while our
                    // primary-service discovery is outstanding. Answer its
                    // request, then keep waiting for the response to ours.
                    if let Some(response) = att::absent_local_attribute_response(&frame.payload) {
                        self.send_att_without_wait(&response)?;
                        continue;
                    }
                    return Ok(frame.payload);
                }
            }
        }
    }

    fn send_att_without_wait(&self, pdu: &[u8]) -> Result<()> {
        for packet in att::build_acl_packets(self.handle, cid::ATT, pdu, self.max_acl_payload) {
            self.transport
                .send_acl(&packet)
                .map_err(|e| LinkError::Transport(e.to_string()))?;
        }
        Ok(())
    }


    /// Answers an LE signalling request, if this frame is one.
    ///
    /// Called from every path that pulls ACL frames apart, because the request
    /// arrives unprompted and the peer starts a one-minute timer the moment it
    /// sends it. Silence there costs the whole connection, so this is never
    /// something to leave for a later step.
    pub fn answer_signalling(
        transport: &UsbTransport,
        handle: u16,
        max_acl_payload: usize,
        frame: &att::L2capFrame,
    ) -> Option<crate::l2cap::ConnectionParameters> {
        if frame.cid != cid::LE_SIGNALING {
            return None;
        }

        let crate::l2cap::Signal::ParameterUpdateRequest { identifier, parameters } =
            crate::l2cap::parse(&frame.payload)?;

        let (low, high) = parameters.interval_ms();
        crate::trace::note(&format!(
            "protejsek zada o interval {low:.1}-{high:.1} ms, latency {}, timeout {} ms - prijato",
            parameters.latency,
            parameters.supervision_timeout as u32 * 10
        ));

        let pdu = crate::l2cap::parameter_update_response(identifier, crate::l2cap::RESULT_ACCEPTED);
        for packet in att::build_acl_packets(handle, cid::LE_SIGNALING, &pdu, max_acl_payload) {
            let _ = transport.send_acl(&packet);
        }

        Some(parameters)
    }

    /// Waits for one ACL packet, giving up early if the link goes away.
    ///
    /// A dropped connection is otherwise indistinguishable from a peer that is
    /// merely slow: the stack sits out the whole timeout and then reports
    /// silence, pointing the investigation at the peer instead of the link.
    /// Events that are not about this handle are handed straight back, because
    /// later steps are waiting for them.
    fn recv_acl_watching_link(&self, deadline: std::time::Instant, budget: Duration) -> Result<Vec<u8>> {
        const POLL: Duration = Duration::from_millis(100);

        loop {
            // Drain into a local list first, then hand the whole lot back. The
            // queue must not be written while it is being read, or the same
            // event is served forever and no ACL is ever collected.
            let mut inspected = Vec::new();
            let mut dropped = None;

            while let Some(event) = self.pump.try_recv_event() {
                if let Some((handle, reason)) = crate::hci::parse_disconnection_complete(&event) {
                    if handle == self.handle {
                        dropped = Some(reason);
                        break;
                    }
                }
                inspected.push(event);
            }
            self.pump.put_back_events(inspected);

            if let Some(reason) = dropped {
                return Err(LinkError::Disconnected { reason });
            }

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(LinkError::Timeout(budget));
            }

            match self.pump.recv_acl(remaining.min(POLL)) {
                Ok(raw) => return Ok(raw),
                Err(LinkError::Timeout(_)) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Sends one SMP PDU and waits for the peer's next SMP PDU.
    ///
    /// Pairing runs on L2CAP channel 6, separate from ATT. Traffic on other
    /// channels is skipped rather than misread as a pairing response.
    pub fn smp_exchange(&mut self, pdu: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        for packet in att::build_acl_packets(self.handle, cid::SMP, pdu, self.max_acl_payload) {
            self.transport
                .send_acl(&packet)
                .map_err(|e| LinkError::Transport(e.to_string()))?;
        }

        self.smp_receive(timeout)
    }

    /// Waits for an SMP PDU without sending anything first.
    ///
    /// Needed because the peripheral sends its confirm value unprompted, between
    /// two of our messages.
    pub fn smp_receive(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let raw = self.recv_acl_watching_link(deadline, timeout)?;

            match self.reassembler.push(&raw) {
                Ok(Some(frame)) if frame.cid == cid::SMP => return Ok(frame.payload),
                Ok(Some(frame)) => {
                    Self::answer_signalling(&self.transport, self.handle, self.max_acl_payload, &frame);
                    continue;
                }
                _ => continue,
            }
        }
    }

    /// Negotiates the largest PDU both sides accept, so PAC records arrive whole.
    pub fn exchange_mtu(&mut self) -> Result<u16> {
        let response = self.request(&att::exchange_mtu_request(att::ATT_PREFERRED_MTU))?;
        self.mtu = att::parse_mtu_response(&response, att::ATT_PREFERRED_MTU)?;
        Ok(self.mtu)
    }

    /// Walks the whole primary service table.
    pub fn discover_services(&mut self) -> Result<Vec<ServiceRange>> {
        let mut services = Vec::new();
        let mut start = 0x0001u16;

        loop {
            let request =
                att::read_by_group_type_request(start, 0xFFFF, att::gatt_uuid::PRIMARY_SERVICE);

            let response = match self.request(&request) {
                Ok(response) => response,
                // "attribute not found" is how the server says it is done.
                Err(LinkError::Att(AttError::Protocol { code: 0x0A, .. })) => break,
                Err(e) => return Err(e),
            };

            let batch = match att::parse_service_ranges(&response) {
                Ok(batch) => batch,
                Err(AttError::Protocol { code: 0x0A, .. }) => break,
                Err(e) => return Err(e.into()),
            };

            if batch.is_empty() {
                break;
            }

            let last_end = batch.last().map(|s| s.end_handle).unwrap_or(0xFFFF);
            services.extend(batch);

            if last_end >= 0xFFFF {
                break;
            }
            start = last_end + 1;
        }

        Ok(services)
    }

    /// Lists the characteristics inside one service.
    pub fn discover_characteristics(&mut self, range: &ServiceRange) -> Result<Vec<Characteristic>> {
        let mut characteristics = Vec::new();
        let mut start = range.start_handle;

        while start <= range.end_handle {
            let request =
                att::read_by_type_request(start, range.end_handle, att::gatt_uuid::CHARACTERISTIC);

            let response = match self.request(&request) {
                Ok(response) => response,
                Err(LinkError::Att(AttError::Protocol { code: 0x0A, .. })) => break,
                Err(e) => return Err(e),
            };

            let batch = match att::parse_characteristics(&response) {
                Ok(batch) => batch,
                Err(AttError::Protocol { code: 0x0A, .. }) => break,
                Err(e) => return Err(e.into()),
            };

            if batch.is_empty() {
                break;
            }

            let last = batch.last().map(|c| c.declaration_handle).unwrap_or(0xFFFF);
            characteristics.extend(batch);

            if last >= range.end_handle {
                break;
            }
            start = last + 1;
        }

        Ok(characteristics)
    }

    /// Reads a characteristic, continuing with Read Blob when the value is long.
    pub fn read_characteristic(&mut self, value_handle: u16) -> Result<Vec<u8>> {
        let response = self.request(&att::read_request(value_handle))?;
        let mut value = att::parse_read_response(&response)?;

        // A response that exactly fills the MTU may have been truncated.
        while value.len() == (self.mtu - 1) as usize {
            let offset = value.len() as u16;
            let response = match self.request(&att::read_blob_request(value_handle, offset)) {
                Ok(response) => response,
                // Reading past the end is how the server signals completion.
                Err(LinkError::Att(AttError::Protocol { code: 0x07, .. })) => break,
                Err(e) => return Err(e),
            };

            let chunk = att::parse_read_response(&response)?;
            if chunk.is_empty() {
                break;
            }
            value.extend_from_slice(&chunk);
        }

        Ok(value)
    }

    /// Records which handle the stack may write to, learned from discovery.
    pub fn allow_writes_to(&mut self, handle: u16) {
        self.write_policy.allow_ase_control_point(handle);
    }

    /// Writes to a characteristic, refusing any handle discovery did not approve.
    ///
    /// The check lives here rather than at the call site so no caller can reach
    /// the device without passing it. Writing to a guessed handle is how you
    /// change a setting on someone's headphones that was never meant to be touched.
    pub fn write_characteristic(&mut self, value_handle: u16, value: &[u8]) -> Result<()> {
        self.write_policy
            .check_write(value_handle, value)
            .map_err(LinkError::Unsafe)?;

        let response = self.request(&att::write_request(value_handle, value))?;
        att::check_error(&response)?;
        Ok(())
    }

    /// Full discovery plus a read of everything PACS and ASCS expose.
    ///
    /// This is the call that finally answers what the headphones can actually do,
    /// which the Windows GATT client refuses to tell us.
    /// Shortens how long a request waits, for teardown.
    ///
    /// A peer that is going away, or has stopped answering after a failed
    /// channel, will never reply - and waiting the full timeout for each of
    /// several writes is why disconnecting looked like the program had hung.
    pub fn set_att_timeout(&mut self, timeout: Duration) {
        self.att_timeout = timeout;
    }

    /// Everything the device has notified since this was last called.
    pub fn take_notifications(&mut self) -> Vec<(u16, Vec<u8>)> {
        std::mem::take(&mut self.notifications)
    }

    /// Waits a moment for notifications to arrive, then hands them over.
    ///
    /// The device answers a control point write **after** the write response, so
    /// asking immediately would always come back empty.
    pub fn collect_notifications(&mut self, wait: Duration) -> Vec<(u16, Vec<u8>)> {
        let deadline = std::time::Instant::now() + wait;

        while std::time::Instant::now() < deadline {
            let Ok(raw) = self.pump.recv_acl(Duration::from_millis(50)) else {
                continue;
            };

            if let Ok(Some(frame)) = self.reassembler.push(&raw) {
                if Self::answer_signalling(&self.transport, self.handle, self.max_acl_payload, &frame)
                    .is_some()
                {
                    continue;
                }

                if let &[op, lo, hi, ref value @ ..] = frame.payload.as_slice() {
                    if frame.cid == cid::ATT
                        && (op == att_op::HANDLE_VALUE_NOTIFICATION
                            || op == att_op::HANDLE_VALUE_INDICATION)
                    {
                        self.notifications
                            .push((u16::from_le_bytes([lo, hi]), value.to_vec()));
                    }
                }
            }
        }

        self.take_notifications()
    }

    /// Finds each Sink ASE, without subscribing to anything.
    ///
    /// Reading is the quiet way to ask. Subscribing puts a notification on the
    /// air for every state change, and those arriving during setup are what
    /// stopped the second isochronous channel from coming up at all.
    pub fn sink_ase_handles(&mut self) -> Result<Vec<(u8, u16)>> {
        self.ase_handles(pacs_uuid::SINK_ASE)
    }

    pub fn source_ase_handles(&mut self) -> Result<Vec<(u8, u16)>> {
        self.ase_handles(pacs_uuid::SOURCE_ASE)
    }

    fn ase_handles(&mut self, wanted_uuid: u16) -> Result<Vec<(u8, u16)>> {
        let services = self.discover_services()?;

        let ascs = services
            .iter()
            .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_ASCS))
            .ok_or(LinkError::ServiceMissing(pacs_uuid::SERVICE_ASCS))?
            .clone();

        let mut sinks = Vec::new();

        for characteristic in self.discover_characteristics(&ascs)? {
            if characteristic.uuid.as_short() != Some(wanted_uuid) {
                continue;
            }

            if let Ok(value) = self.read_characteristic(characteristic.value_handle) {
                if let Some(state) = crate::bap::ase::parse_state(&value) {
                    sinks.push((state.ase_id, characteristic.value_handle));
                }
            }
        }

        Ok(sinks)
    }

    /// Subscribes to every ASE the device exposes, plus the control point.
    ///
    /// Returns the value handle of each Sink ASE, so a later notification can be
    /// tied back to the ASE it belongs to.
    pub fn subscribe_to_ase_state(&mut self) -> Result<Vec<(u8, u16)>> {
        let services = self.discover_services()?;

        let ascs = services
            .iter()
            .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_ASCS))
            .ok_or(LinkError::ServiceMissing(pacs_uuid::SERVICE_ASCS))?
            .clone();

        let characteristics = self.discover_characteristics(&ascs)?;
        let mut sinks = Vec::new();

        for characteristic in characteristics {
            let uuid = characteristic.uuid.as_short();
            let is_ase = uuid == Some(pacs_uuid::SINK_ASE) || uuid == Some(pacs_uuid::SOURCE_ASE);
            if !is_ase && uuid != Some(pacs_uuid::ASE_CONTROL_POINT) {
                continue;
            }

            // The CCCD sits immediately after the value it configures.
            let _ = self.subscribe(characteristic.value_handle + 1);

            if uuid == Some(pacs_uuid::SINK_ASE) {
                if let Ok(value) = self.read_characteristic(characteristic.value_handle) {
                    if let Some(state) = crate::bap::ase::parse_state(&value) {
                        sinks.push((state.ase_id, characteristic.value_handle));
                    }
                }
            }
        }

        Ok(sinks)
    }

    /// Approves a descriptor for subscription and turns notifications on.
    pub fn subscribe(&mut self, cccd_handle: u16) -> Result<()> {
        self.write_policy.allow_subscription(cccd_handle);
        self.write_characteristic(cccd_handle, &[0x01, 0x00])
    }

    /// Finds the Volume Control Service, if the device has one.
    ///
    /// Returns `Ok(None)` rather than an error when it is absent: plenty of LE
    /// Audio devices carry no VCS, and that is a device without remote volume,
    /// not a broken connection.
    pub fn discover_volume_control(&mut self) -> Result<Option<VolumeControlHandles>> {
        let services = self.discover_services()?;

        let Some(range) = services
            .iter()
            .find(|s| s.uuid.as_short() == Some(crate::vcs::uuid::VOLUME_CONTROL_SERVICE))
            .cloned()
        else {
            return Ok(None);
        };

        let characteristics = self.discover_characteristics(&range)?;
        let find = |uuid: u16| {
            characteristics
                .iter()
                .find(|c| c.uuid.as_short() == Some(uuid))
                .cloned()
        };

        let (Some(state), Some(control_point)) = (
            find(crate::vcs::uuid::VOLUME_STATE),
            find(crate::vcs::uuid::VOLUME_CONTROL_POINT),
        ) else {
            return Ok(None);
        };

        // The Client Characteristic Configuration descriptor sits immediately
        // after the value it configures. Discovering descriptors properly would
        // be one more round trip for a handle that is fixed by construction.
        Ok(Some(VolumeControlHandles {
            state: state.value_handle,
            state_cccd: state.value_handle + 1,
            control_point: control_point.value_handle,
        }))
    }

    /// Reads the volume the headphones currently hold.
    pub fn read_volume_state(&mut self, handle: u16) -> Result<Option<crate::vcs::VolumeState>> {
        let value = self.read_characteristic(handle)?;
        Ok(crate::vcs::parse_volume_state(&value))
    }

    /// Approves the volume control point, so writes to it stop being refused.
    pub fn allow_volume_writes_to(&mut self, handle: u16) {
        self.write_policy.allow_volume_control_point(handle);
    }

    pub fn read_audio_capabilities(&mut self) -> Result<AudioCapabilities> {
        self.exchange_mtu()?;

        let services = self.discover_services()?;
        let mut capabilities = AudioCapabilities::default();

        let pacs = services
            .iter()
            .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_PACS))
            .ok_or(LinkError::ServiceMissing(pacs_uuid::SERVICE_PACS))?
            .clone();

        let pacs_characteristics = self.discover_characteristics(&pacs)?;
        let service = DiscoveredService {
            range: pacs,
            characteristics: pacs_characteristics,
        };

        if let Some(c) = service.characteristic(pacs_uuid::SINK_PAC) {
            let value = self.read_characteristic(c.value_handle)?;
            capabilities.sink_records = PacRecord::parse_characteristic(&value);
        }

        if let Some(c) = service.characteristic(pacs_uuid::SOURCE_PAC) {
            let value = self.read_characteristic(c.value_handle)?;
            capabilities.source_records = PacRecord::parse_characteristic(&value);
        }

        if let Some(c) = service.characteristic(pacs_uuid::SINK_AUDIO_LOCATIONS) {
            let value = self.read_characteristic(c.value_handle)?;
            if value.len() >= 4 {
                capabilities.sink_locations =
                    Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]]));
            }
        }

        if let Some(c) = service.characteristic(pacs_uuid::AVAILABLE_CONTEXTS) {
            let value = self.read_characteristic(c.value_handle)?;
            if value.len() >= 2 {
                capabilities.available_contexts = Some(u16::from_le_bytes([value[0], value[1]]));
            }
        }

        if let Some(c) = service.characteristic(pacs_uuid::SUPPORTED_CONTEXTS) {
            let value = self.read_characteristic(c.value_handle)?;
            if value.len() >= 2 {
                capabilities.supported_contexts = Some(u16::from_le_bytes([value[0], value[1]]));
            }
        }

        // ASCS tells us how many streams can run at once, and their ids.
        if let Some(ascs) = services
            .iter()
            .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_ASCS))
        {
            let ascs = ascs.clone();
            for characteristic in self.discover_characteristics(&ascs)? {
                let uuid = characteristic.uuid.as_short();
                if uuid != Some(pacs_uuid::SINK_ASE) && uuid != Some(pacs_uuid::SOURCE_ASE) {
                    continue;
                }

                // First byte of an ASE value is its id.
                if let Ok(value) = self.read_characteristic(characteristic.value_handle) {
                    if let Some(&ase_id) = value.first() {
                        if uuid == Some(pacs_uuid::SINK_ASE) {
                            capabilities.sink_ase_ids.push(ase_id);
                        } else {
                            capabilities.source_ase_ids.push(ase_id);
                        }
                    }
                }
            }
        }

        Ok(capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bap::Preset;

    /// Draining the held queue and handing it back must leave it unchanged and,
    /// above all, must terminate. Returning events one at a time from inside the
    /// drain loop spun forever, and because the spin sat between reading ACL and
    /// answering it, the peer saw a host that had gone silent mid-handshake.
    #[test]
    fn inspecting_held_events_terminates_and_preserves_order() {
        let held: RefCell<VecDeque<Event>> = RefCell::new(VecDeque::new());
        for code in [0x05u8, 0x0E, 0x13] {
            held.borrow_mut().push_back(Event { code, params: vec![code] });
        }

        // The same drain-then-restore shape the ACL wait uses.
        let mut inspected = Vec::new();
        while let Some(event) = held.borrow_mut().pop_front() {
            inspected.push(event);
        }
        for event in inspected.into_iter().rev() {
            held.borrow_mut().push_front(event);
        }

        let codes: Vec<u8> = held.borrow().iter().map(|e| e.code).collect();
        assert_eq!(codes, vec![0x05, 0x0E, 0x13], "order must survive a round trip");
    }

    #[test]
    fn capabilities_default_to_empty() {
        let caps = AudioCapabilities::default();
        assert!(caps.sink_records.is_empty());
        assert!(caps.sink_ase_ids.is_empty());
    }

    #[test]
    fn service_lookup_matches_by_short_uuid() {
        use crate::att::Uuid;

        let service = DiscoveredService {
            range: ServiceRange {
                start_handle: 0x0001,
                end_handle: 0x0010,
                uuid: Uuid::Short(pacs_uuid::SERVICE_PACS),
            },
            characteristics: vec![Characteristic {
                declaration_handle: 0x0002,
                properties: 0x12,
                value_handle: 0x0003,
                uuid: Uuid::Short(pacs_uuid::SINK_PAC),
            }],
        };

        assert!(service.characteristic(pacs_uuid::SINK_PAC).is_some());
        assert!(service.characteristic(pacs_uuid::SOURCE_PAC).is_none());
        assert_eq!(
            service.characteristic(pacs_uuid::SINK_PAC).unwrap().value_handle,
            0x0003
        );
    }

    #[test]
    fn two_ase_ids_mean_two_cis_are_possible() {
        // A device exposing two sink ASEs is the layout that makes Windows set up
        // two CIS. Knowing the count is what lets us choose one stream instead.
        let mut caps = AudioCapabilities::default();
        caps.sink_ase_ids = vec![1, 2];

        assert_eq!(caps.sink_ase_ids.len(), 2);

        // With stereo-in-one-stream support we only need the first.
        let config = Preset::WindowsDefault.codec(true);
        assert_eq!(config.channel_count(), 2);
    }
}
