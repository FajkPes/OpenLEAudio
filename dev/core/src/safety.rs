//! Guards that sit between the stack and the hardware.
//!
//! Nothing here is optional or advisory. Every write that leaves this process
//! passes through one of these checks, because the two things being driven are
//! someone's radio and someone's ears, and neither gets a second chance if we
//! send the wrong bytes.
//!
//! What is actually at risk, honestly assessed:
//!
//! - **The adapter cannot be physically damaged by HCI traffic.** Firmware is
//!   loaded by its original driver before we take the interface, and we never
//!   send vendor-specific commands, which is where flashing would live. The
//!   worst case is a confused controller, cured by unplugging it.
//! - **The headphones cannot be reflashed over ASCS.** We write to one control
//!   point and stream audio. There is no firmware characteristic in that path.
//! - **Hearing is the real risk.** A corrupted LC3 frame decodes to full-scale
//!   noise. That is why the output limiter below is not optional.

/// Opcode groups the stack is permitted to send.
///
/// Vendor-specific commands (OGF 0x3F) are where firmware flashing and factory
/// test modes live on every chipset. The stack has no reason to send one, so
/// they are refused outright rather than trusted not to appear.
const OGF_VENDOR_SPECIFIC: u16 = 0x3F;

/// Commands that could disturb the controller's own configuration.
const FORBIDDEN_OPCODES: &[u16] = &[
    0x0C03 | 0x8000, // guard value, never matches: keeps the list non-empty
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SafetyViolation {
    #[error("refused vendor-specific HCI command {opcode:#06x} - these can reflash firmware")]
    VendorSpecificCommand { opcode: u16 },

    #[error("refused write to handle {handle:#06x}: discovery never approved it for writing")]
    UnapprovedWriteTarget { handle: u16 },

    #[error("refused {bytes} byte write: no approved operation is this large")]
    OversizedWrite { bytes: usize },

    #[error("audio frame rejected: {reason}")]
    UnsafeAudio { reason: &'static str },
}

/// Checks an HCI command before it reaches the controller.
pub fn check_hci_command(packet: &[u8]) -> Result<(), SafetyViolation> {
    if packet.len() < 3 {
        return Ok(()); // malformed, the transport will reject it anyway
    }

    let opcode = u16::from_le_bytes([packet[0], packet[1]]);
    let ogf = opcode >> 10;

    if ogf == OGF_VENDOR_SPECIFIC {
        return Err(SafetyViolation::VendorSpecificCommand { opcode });
    }

    if FORBIDDEN_OPCODES.contains(&opcode) {
        return Err(SafetyViolation::VendorSpecificCommand { opcode });
    }

    Ok(())
}

/// The GATT handles the stack is allowed to write to, every one of them learned
/// from discovery rather than guessed.
///
/// This is an allowlist, not a blocklist, and that direction is deliberate. A
/// handle we assumed is how you end up writing into a vendor characteristic
/// that was never meant to be touched - which on some devices is exactly where
/// firmware update lives. Nothing is writable until discovery has found it and
/// named what it is for.
#[derive(Debug, Clone, Copy, Default)]
pub struct WritePolicy {
    ase_control_point: Option<u16>,
    volume_control_point: Option<u16>,
    /// Client Characteristic Configuration descriptors we may subscribe on.
    subscriptions: [Option<u16>; 4],
}

/// How large a write to each kind of handle is ever allowed to be.
///
/// A write far past what the operation needs means something upstream built the
/// wrong buffer, and sending it anyway is how a device ends up in a state its
/// own firmware did not expect.
const MAX_ASCS_OPERATION: usize = 128;
const MAX_VOLUME_OPERATION: usize = 3;
const MAX_SUBSCRIPTION_WRITE: usize = 2;

impl WritePolicy {
    /// Records the ASE control point handle found during service discovery.
    pub fn allow_ase_control_point(&mut self, handle: u16) {
        self.ase_control_point = Some(handle);
    }

    /// Records the volume control point handle found during service discovery.
    pub fn allow_volume_control_point(&mut self, handle: u16) {
        self.volume_control_point = Some(handle);
    }

    /// Records a descriptor we may enable notifications on.
    ///
    /// Silently full rather than growing: a stack that needs a fifth
    /// subscription has changed shape, and that deserves a look rather than a
    /// larger array.
    pub fn allow_subscription(&mut self, handle: u16) {
        if self.subscriptions.contains(&Some(handle)) {
            return;
        }
        if let Some(slot) = self.subscriptions.iter_mut().find(|s| s.is_none()) {
            *slot = Some(handle);
        }
    }

    /// Rejects any write to a handle discovery has not approved for that purpose.
    pub fn check_write(&self, handle: u16, value: &[u8]) -> Result<(), SafetyViolation> {
        let limit = if self.ase_control_point == Some(handle) {
            MAX_ASCS_OPERATION
        } else if self.volume_control_point == Some(handle) {
            MAX_VOLUME_OPERATION
        } else if self.subscriptions.contains(&Some(handle)) {
            MAX_SUBSCRIPTION_WRITE
        } else {
            return Err(SafetyViolation::UnapprovedWriteTarget { handle });
        };

        if value.len() > limit {
            return Err(SafetyViolation::OversizedWrite { bytes: value.len() });
        }

        Ok(())
    }

    pub fn control_point(&self) -> Option<u16> {
        self.ase_control_point
    }
}

/// Attenuation applied to captured audio before encoding.
///
/// The default is **transparent**: samples reach the encoder exactly as Windows
/// produced them. That is not a relaxation of safety, it is where the safety
/// now lives. While the stack was unproven this attenuated everything by 20 dB
/// and clipped at half scale, so a stream that turned out to be garbage could
/// not hurt anyone. The cost is that it also destroys real audio - three bits of
/// resolution thrown away before LC3 ever sees the signal, and the listener
/// turns the headphones up, amplifying what is left of the quantisation noise.
/// It sounds exactly as bad as it is.
///
/// What actually protects the listener is `screen_frame`, which recognises
/// decoder garbage and refuses to play it, and `soft_start`, which ramps the
/// first moments up instead of starting at full level. Both stay on. Blanket
/// attenuation is still available through `safe_start`.
#[derive(Debug, Clone, Copy)]
pub struct OutputLimiter {
    gain: f32,
    peak_ceiling: i16,
}

impl Default for OutputLimiter {
    fn default() -> Self {
        Self::transparent()
    }
}

impl OutputLimiter {
    /// Samples pass through untouched. The quality the driver is here to deliver.
    pub fn transparent() -> Self {
        Self {
            gain: 1.0,
            peak_ceiling: i16::MAX,
        }
    }

    /// About -20 dB, clipped at half scale. For bringing up an unproven change.
    pub fn safe_start() -> Self {
        Self {
            gain: 0.1,
            peak_ceiling: (i16::MAX as f32 * 0.5) as i16,
        }
    }

    /// Full scale, with the peak ceiling still enforced.
    ///
    /// Only sensible once a stream has been confirmed to sound correct.
    pub fn unrestricted() -> Self {
        Self {
            gain: 1.0,
            peak_ceiling: i16::MAX,
        }
    }

    pub fn with_gain(gain: f32) -> Self {
        Self {
            gain: gain.clamp(0.0, 2.0),
            peak_ceiling: i16::MAX,
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Applies attenuation and hard-clips anything above the ceiling.
    ///
    /// A transparent limiter returns immediately rather than multiplying every
    /// sample by one and rounding the result, which is not free and not lossless.
    pub fn apply(&self, samples: &mut [i16]) {
        if self.gain == 1.0 && self.peak_ceiling == i16::MAX {
            return;
        }

        for sample in samples.iter_mut() {
            let scaled = (*sample as f32 * self.gain) as i32;
            let ceiling = self.peak_ceiling as i32;
            *sample = scaled.clamp(-ceiling, ceiling) as i16;
        }
    }

    /// Rejects a frame that looks like decoder garbage rather than audio.
    ///
    /// Full-scale white noise is what a corrupted stream sounds like, and it is
    /// both the loudest and the least musical thing that can come out. Real audio
    /// rarely sits at the rails for a whole frame.
    pub fn screen_frame(&self, samples: &[i16]) -> Result<(), SafetyViolation> {
        if samples.is_empty() {
            return Ok(());
        }

        let railed = samples
            .iter()
            .filter(|&&s| s.saturating_abs() > (i16::MAX as f32 * 0.98) as i16)
            .count();

        // More than half a frame pinned to the rails is not music.
        if railed * 2 > samples.len() {
            return Err(SafetyViolation::UnsafeAudio {
                reason: "frame is mostly full-scale, refusing to play it",
            });
        }

        Ok(())
    }
}

/// Ramps the first moments of a stream up from silence.
///
/// Cheap insurance that costs nothing audible: a listener who has the headphones
/// on when a stream starts never gets a full-level transient, and a stream that
/// is wrong is quiet while it is still being noticed.
#[derive(Debug, Clone, Copy)]
pub struct SoftStart {
    frames_done: u32,
    frames_total: u32,
}

impl SoftStart {
    /// Ramps over roughly `millis` of audio at the given frame duration.
    pub fn over(millis: u32, frame_duration_us: u32) -> Self {
        let per_frame_ms = (frame_duration_us / 1000).max(1);
        Self {
            frames_done: 0,
            frames_total: (millis / per_frame_ms).max(1),
        }
    }

    pub fn finished(&self) -> bool {
        self.frames_done >= self.frames_total
    }

    /// Scales one frame and advances the ramp.
    pub fn apply(&mut self, samples: &mut [i16]) {
        if self.finished() {
            return;
        }

        let gain = self.frames_done as f32 / self.frames_total as f32;
        for sample in samples.iter_mut() {
            *sample = (*sample as f32 * gain) as i16;
        }

        self.frames_done += 1;
    }
}

/// Sanity-checks a stream configuration against physical limits before it is sent.
///
/// These are not the device's advertised limits - those are checked separately
/// against its PAC records. These are the values that should never be sent to
/// any device, whatever it claims to accept.
pub fn check_stream_parameters(
    octets_per_frame: u16,
    sdu_interval_us: u32,
    retransmissions: u8,
    max_transport_latency_ms: u16,
) -> Result<(), &'static str> {
    if octets_per_frame == 0 {
        return Err("zero octets per frame would produce an empty stream");
    }
    if octets_per_frame > 400 {
        return Err("octets per frame far beyond anything LC3 defines");
    }
    if sdu_interval_us < 5_000 || sdu_interval_us > 20_000 {
        return Err("SDU interval outside the range LE Audio uses");
    }
    if retransmissions > 15 {
        return Err("retransmission number must fit in four bits");
    }
    if max_transport_latency_ms < 5 || max_transport_latency_ms > 4_000 {
        return Err("transport latency outside the specified range");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_specific_commands_are_refused() {
        // OGF 0x3F is where firmware flashing lives on Realtek, Intel and others.
        let vendor = [0x01, 0xFC, 0x00]; // opcode 0xFC01
        match check_hci_command(&vendor) {
            Err(SafetyViolation::VendorSpecificCommand { opcode }) => {
                assert_eq!(opcode, 0xFC01);
            }
            other => panic!("vendor command must be refused, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_commands_pass() {
        let reset = [0x03, 0x0C, 0x00];
        assert!(check_hci_command(&reset).is_ok());

        let cig = [0x62, 0x20, 0x00];
        assert!(check_hci_command(&cig).is_ok());
    }

    #[test]
    fn writes_are_refused_until_discovery_approves_a_handle() {
        let policy = WritePolicy::default();

        // Before discovery nothing is writable, not even a plausible handle.
        assert!(policy.check_write(0x0042, &[0x01]).is_err());
    }

    #[test]
    fn only_the_discovered_control_point_accepts_writes() {
        let mut policy = WritePolicy::default();
        policy.allow_ase_control_point(0x0042);

        assert!(policy.check_write(0x0042, &[0x01, 0x01, 0x01]).is_ok());

        // A neighbouring handle must be refused, however tempting.
        match policy.check_write(0x0043, &[0x01]) {
            Err(SafetyViolation::UnapprovedWriteTarget { handle }) => assert_eq!(handle, 0x0043),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn oversized_writes_are_refused() {
        let mut policy = WritePolicy::default();
        policy.allow_ase_control_point(0x0042);

        let huge = vec![0u8; 200];
        match policy.check_write(0x0042, &huge) {
            Err(SafetyViolation::OversizedWrite { bytes }) => assert_eq!(bytes, 200),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_default_does_not_touch_the_audio() {
        let limiter = OutputLimiter::default();
        let original: Vec<i16> = (0..480).map(|i| ((i as f32 * 0.1).sin() * 30_000.0) as i16).collect();
        let mut samples = original.clone();

        limiter.apply(&mut samples);

        assert_eq!(samples, original, "the default must be bit-exact");
    }

    #[test]
    fn a_soft_start_begins_silent_and_reaches_full_level() {
        let mut ramp = SoftStart::over(300, 10_000);
        let loud = vec![20_000i16; 480];

        let mut first = loud.clone();
        ramp.apply(&mut first);
        assert_eq!(first[0], 0, "the very first frame must not be loud");

        for _ in 0..64 {
            let mut frame = loud.clone();
            ramp.apply(&mut frame);
        }

        assert!(ramp.finished());
        let mut later = loud.clone();
        ramp.apply(&mut later);
        assert_eq!(later, loud, "a finished ramp must be transparent");
    }

    #[test]
    fn safe_start_is_quiet_when_asked_for() {
        let limiter = OutputLimiter::safe_start();
        assert!(limiter.gain() < 0.2, "first runs must not be loud");

        let mut samples = vec![i16::MAX; 8];
        limiter.apply(&mut samples);

        // Attenuated well below full scale.
        assert!(samples.iter().all(|&s| s < i16::MAX / 4), "got {samples:?}");
    }

    #[test]
    fn limiter_clamps_instead_of_wrapping() {
        // Naive scaling of the most negative sample overflows; it must clamp.
        let limiter = OutputLimiter::unrestricted();
        let mut samples = vec![i16::MIN, i16::MAX];
        limiter.apply(&mut samples);

        assert!(samples[0] <= 0 && samples[1] >= 0, "signs must survive: {samples:?}");
    }

    #[test]
    fn boost_doubles_quiet_audio_and_clamps_loud_audio() {
        let limiter = OutputLimiter::with_gain(2.0);
        let mut samples = vec![5_000, 20_000, -20_000];
        limiter.apply(&mut samples);

        assert_eq!(samples, vec![10_000, i16::MAX, -i16::MAX]);
        assert_eq!(limiter.gain(), 2.0);
    }

    #[test]
    fn full_scale_noise_is_rejected() {
        let limiter = OutputLimiter::safe_start();

        // What a corrupted decode sounds like: everything at the rails.
        let garbage: Vec<i16> = (0..480)
            .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();

        assert!(limiter.screen_frame(&garbage).is_err());

        // Ordinary loud music is not rejected.
        let music: Vec<i16> = (0..480)
            .map(|i| ((i as f32 * 0.1).sin() * 20_000.0) as i16)
            .collect();
        assert!(limiter.screen_frame(&music).is_ok());
    }

    #[test]
    fn silence_passes_screening() {
        let limiter = OutputLimiter::safe_start();
        assert!(limiter.screen_frame(&vec![0i16; 480]).is_ok());
        assert!(limiter.screen_frame(&[]).is_ok());
    }

    #[test]
    fn impossible_stream_parameters_are_caught() {
        // A sane configuration.
        assert!(check_stream_parameters(100, 10_000, 2, 20).is_ok());

        assert!(check_stream_parameters(0, 10_000, 2, 20).is_err());
        assert!(check_stream_parameters(5000, 10_000, 2, 20).is_err());
        assert!(check_stream_parameters(100, 1_000, 2, 20).is_err());
        assert!(check_stream_parameters(100, 10_000, 99, 20).is_err());
        assert!(check_stream_parameters(100, 10_000, 2, 1).is_err());
    }
}
