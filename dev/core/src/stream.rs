//! Turning capabilities into a running audio stream.
//!
//! Given what the headphones advertised and which preset the user picked, this
//! decides the concrete configuration, then walks the ASCS state machine that
//! gets a stream from Idle to Streaming:
//!
//! ```text
//!   Config Codec -> Config QoS -> Enable -> CIG/CIS -> ISO data path -> Streaming
//! ```
//!
//! The topology decision lives here too. A headset that can carry both channels
//! on one stream gets a single CIS, which is fewer links to establish and fewer
//! ways for setup to fail partway.

use crate::bap::{ascs, CodecCapabilities, CodecConfiguration, PacRecord, Preset, QosConfiguration};
use crate::hci::{self, CisParams};
use crate::link::AudioCapabilities;
use lc3_codec::common::config::{
    FrameDuration as Lc3FrameDuration, SamplingFrequency as Lc3SamplingFrequency,
};
use lc3_codec::common::complex::{Complex, Scaler};
use lc3_codec::decoder::lc3_decoder::Lc3Decoder;
use lc3_codec::encoder::lc3_encoder::Lc3Encoder;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("got {got} samples, but this configuration needs exactly {expected}")]
    WrongFrameLength { got: usize, expected: usize },

    #[error("channel {0} does not exist in this stream")]
    NoSuchChannel(usize),

    #[error("LC3 encoder rejected the frame: {0}")]
    Codec(String),
}

/// How the stereo pair is carried over the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// Both channels in one CIS. Preferred: one link to establish, not two.
    SingleCis,
    /// One CIS per channel, the layout Windows normally negotiates.
    DualCis,
}

impl Topology {
    pub fn cis_count(&self) -> usize {
        match self {
            Topology::SingleCis => 1,
            Topology::DualCis => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("device published no LC3 sink capabilities")]
    NoLc3Sink,

    #[error("device exposes no sink ASE to configure")]
    NoSinkAse,

    #[error("device published no LC3 microphone capabilities")]
    NoLc3Source,

    #[error("device exposes no Source ASE for its microphone")]
    NoSourceAse,

    #[error("none of the supported LC3 microphone profiles fits this device")]
    NoCompatibleMicrophoneCodec,

    #[error("preset is outside what this device accepts: {0}")]
    Rejected(#[from] crate::bap::ConfigRejection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneQuality {
    Voice,
    Balanced,
    High,
}

#[derive(Debug, Clone)]
pub struct MicrophonePlan {
    pub ase_id: u8,
    pub codec: CodecConfiguration,
    pub qos: QosConfiguration,
    pub cis_id: u8,
}

/// A complete, validated plan for one audio stream.
#[derive(Debug, Clone)]
pub struct StreamPlan {
    pub codec: CodecConfiguration,
    pub qos: QosConfiguration,
    pub topology: Topology,
    pub ase_ids: Vec<u8>,
    pub cig_id: u8,
    /// Reproduces the very first configuration that ever produced sound.
    ///
    /// Kept as a deliberate escape hatch rather than deleted history. Every
    /// later change was an improvement on paper - correct per-ear allocation,
    /// interleaved packing, reading the device's answers - and none of them has
    /// yet been shown to sound better than the configuration that worked by
    /// accident. Until one of them is, being able to go back in one click is
    /// worth more than tidiness.
    pub legacy: bool,
    /// Swaps which ear each stream claims.
    ///
    /// A guess made available rather than made silently. Everything on our side
    /// says stream one is the left ear, and the device still plays as if it
    /// were not - so the possibility that this device numbers its ASEs the
    /// other way round is worth one click to test.
    pub swap_ears: bool,
    /// Sequential or interleaved, as the user chose.
    pub packing: u8,
    /// The Target_Latency the device is asked to configure for.
    pub target_latency: u8,
    /// The audio context announced in Enable.
    pub context: u16,
    pub microphone: Option<MicrophonePlan>,
    pub playback_enabled: bool,
}

impl StreamPlan {
    /// Builds a plan from what the device published and the preset chosen.
    ///
    /// Every value is checked against the device's own PAC records first, so a
    /// configuration it would reject never reaches the air.
    /// Builds a plan from values the user chose, rather than from a preset.
    ///
    /// The whole point of this driver: the device publishes real ranges and the
    /// user is allowed to pick inside them. The configuration is still checked
    /// against what the device advertised, so "custom" means free choice, not
    /// permission to send something the headphones will refuse.
    pub fn build_custom(
        capabilities: &AudioCapabilities,
        codec: CodecConfiguration,
        qos: QosConfiguration,
        prefer_single_cis: bool,
    ) -> Result<Self, PlanError> {
        let records: Vec<_> = capabilities
            .sink_records
            .iter()
            .filter(|r| r.is_lc3())
            .collect();

        if records.is_empty() {
            return Err(PlanError::NoLc3Sink);
        }
        if capabilities.sink_ase_ids.is_empty() {
            return Err(PlanError::NoSinkAse);
        }

        let single = prefer_single_cis
            && records
                .iter()
                .any(|r| r.capabilities.supports_stereo_in_one_stream());

        let mut codec = codec;
        codec.channel_allocation = if single {
            crate::bap::LOCATION_STEREO
        } else {
            crate::bap::LOCATION_FRONT_LEFT
        };
        codec.frames_per_sdu = if single { 2 } else { 1 };
        // QoS must describe the configuration after topology selection. In a
        // one-CIS stereo stream one SDU contains two LC3 frames, not one.
        let mut qos = qos;
        qos.max_sdu = codec.sdu_size();

        let mut rejection = None;
        let accepted = records.iter().any(|record| {
            match record.capabilities.accepts(&codec) {
                Ok(()) => true,
                Err(e) => {
                    rejection = Some(e);
                    false
                }
            }
        });

        if !accepted {
            return Err(rejection.map(PlanError::from).unwrap_or(PlanError::NoLc3Sink));
        }

        let topology = if single { Topology::SingleCis } else { Topology::DualCis };
        let ase_ids = match topology {
            Topology::SingleCis => vec![capabilities.sink_ase_ids[0]],
            Topology::DualCis => capabilities.sink_ase_ids.iter().copied().take(2).collect(),
        };

        Ok(Self {
            codec,
            qos,
            topology,
            ase_ids,
            cig_id: 0x01,
            legacy: false,
            swap_ears: false,
            packing: crate::hci::PACKING_SEQUENTIAL,
            target_latency: crate::bap::ascs::LATENCY_BALANCED,
            context: crate::bap::ascs::CONTEXT_MEDIA,
            microphone: None,
            playback_enabled: true,
        })
    }

    pub fn build(
        capabilities: &AudioCapabilities,
        preset: Preset,
        prefer_single_cis: bool,
    ) -> Result<Self, PlanError> {
        // Codec first, then endpoints: a device with no LC3 at all should say
        // so, rather than complaining about the ASEs it also lacks.
        let records: Vec<_> = capabilities
            .sink_records
            .iter()
            .filter(|r| r.is_lc3())
            .collect();

        if records.is_empty() {
            return Err(PlanError::NoLc3Sink);
        }

        if capabilities.sink_ase_ids.is_empty() {
            return Err(PlanError::NoSinkAse);
        }

        // A device publishes one record per sample rate it supports, so the
        // first one is not the interesting one - the JBL Tune 780NC lists 16 kHz
        // first and 48 kHz last. Checking only the first rejected every good
        // configuration the device could actually play.
        let mut rejection = None;
        let mut chosen = None;

        // Two channels on two streams is the layout Windows negotiates and the
        // one this stack has actually been proven against. Carrying both on one
        // stream is fewer links to establish - genuinely better when it works -
        // but it is also the branch nothing has ever tested on hardware, because
        // the reference headset cannot do it.
        //
        // So it is used when the caller asks for it, and when there is no
        // alternative: a device with a single Sink ASE has no second stream to
        // put the other ear on, and forcing the two-stream layout there would
        // quietly fold its stereo into a mono mix.
        let one_endpoint_only = capabilities.sink_ase_ids.len() < 2;

        for record in &records {
            let caps = &record.capabilities;
            let single = caps.supports_stereo_in_one_stream()
                && (prefer_single_cis || one_endpoint_only);
            let codec = preset.codec(single);

            match caps.accepts(&codec) {
                Ok(()) => {
                    chosen = Some((codec, single));
                    break;
                }
                Err(e) => rejection = Some(e),
            }
        }

        let (codec, single) = match chosen {
            Some(found) => found,
            // Report why the closest record said no, rather than inventing a
            // reason of our own.
            None => return Err(rejection.map(PlanError::from).unwrap_or(PlanError::NoLc3Sink)),
        };

        let topology = if single {
            Topology::SingleCis
        } else {
            Topology::DualCis
        };

        let qos = preset.qos(&codec);

        // Dual CIS needs one ASE per channel; single CIS needs only the first.
        let ase_ids = match topology {
            Topology::SingleCis => vec![capabilities.sink_ase_ids[0]],
            Topology::DualCis => capabilities
                .sink_ase_ids
                .iter()
                .take(2)
                .copied()
                .collect(),
        };

        Ok(Self {
            codec,
            qos,
            topology,
            ase_ids,
            cig_id: 0x01,
            legacy: false,
            swap_ears: false,
            packing: crate::hci::PACKING_SEQUENTIAL,
            target_latency: crate::bap::ascs::LATENCY_BALANCED,
            context: crate::bap::ascs::CONTEXT_MEDIA,
            microphone: None,
            playback_enabled: true,
        })
    }

    /// Falls back through progressively safer presets until one is accepted.
    ///
    /// Used when the user's choice is out of range: rather than refusing to play,
    /// the stack drops to something the device definitely supports and reports it.
    pub fn build_with_fallback(
        capabilities: &AudioCapabilities,
        preferred: Preset,
        prefer_single_cis: bool,
    ) -> Result<(Self, Preset), PlanError> {
        let order = [preferred, Preset::WindowsDefault, Preset::Robust];

        let mut last_error = None;
        for preset in order {
            match Self::build(capabilities, preset, prefer_single_cis) {
                Ok(plan) => return Ok((plan, preset)),
                Err(e @ PlanError::Rejected(_)) => last_error = Some(e),
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or(PlanError::NoLc3Sink))
    }

    /// Estimated end-to-end audio delay for this plan.
    pub fn latency_ms(&self) -> u32 {
        self.qos.estimated_latency_ms(&self.codec)
    }

    /// Bytes of LC3 the encoder must produce per SDU interval.
    pub fn sdu_size(&self) -> u16 {
        self.codec.sdu_size()
    }

    /// The same plan, reduced to a single stream carrying a mono mix.
    ///
    /// Deliberately available as a choice rather than only as a fallback. Two
    /// isochronous channels are where this device is unreliable, and someone who
    /// would rather have sound that always works than stereo that sometimes does
    /// should be able to say so.
    /// The configuration as it was when sound first came out of the headphones.
    ///
    /// Both streams claim the left ear and the channels are packed sequentially,
    /// which is what the stack did before any of it was understood. The caller
    /// also skips reading the device's answers, because that is the other thing
    /// that was not happening then.
    pub fn into_legacy(mut self) -> Self {
        self.legacy = true;
        self
    }

    pub fn into_mono(mut self) -> Self {
        self.ase_ids.truncate(1);
        // Front Left rather than the spec's "no location" value, because this is
        // the legacy mode: it reproduces the configuration these headphones were
        // observed to play. A single stream claiming no location is the more
        // correct reading of the specification, and entirely untested here.
        self.codec.channel_allocation = crate::bap::LOCATION_FRONT_LEFT;
        self
    }

    /// Adds the headset's Source ASE to CIS 0, so playback and microphone use
    /// one bidirectional CIS just like a conversational LE Audio stream.
    pub fn with_microphone(
        mut self,
        capabilities: &AudioCapabilities,
        quality: MicrophoneQuality,
    ) -> Result<Self, PlanError> {
        let records: Vec<_> = capabilities
            .source_records
            .iter()
            .filter(|record| record.is_lc3())
            .collect();
        if records.is_empty() {
            return Err(PlanError::NoLc3Source);
        }
        let Some(&ase_id) = capabilities.source_ase_ids.first() else {
            return Err(PlanError::NoSourceAse);
        };

        let candidates: &[(u32, crate::bap::FrameDuration, u16)] = match quality {
            MicrophoneQuality::Voice => &[
                (16_000, crate::bap::FrameDuration::Ms10, 40),
                (24_000, crate::bap::FrameDuration::Ms10, 60),
                (32_000, crate::bap::FrameDuration::Ms10, 80),
            ],
            MicrophoneQuality::Balanced => &[
                (32_000, crate::bap::FrameDuration::Ms10, 80),
                (24_000, crate::bap::FrameDuration::Ms10, 60),
                (16_000, crate::bap::FrameDuration::Ms10, 40),
                (48_000, crate::bap::FrameDuration::Ms10, 100),
            ],
            MicrophoneQuality::High => &[
                (48_000, crate::bap::FrameDuration::Ms10, 100),
                (32_000, crate::bap::FrameDuration::Ms10, 80),
                (24_000, crate::bap::FrameDuration::Ms10, 60),
                (16_000, crate::bap::FrameDuration::Ms10, 40),
            ],
        };

        let codec = candidates
            .iter()
            .filter_map(|&(hz, duration, octets)| {
                Some(CodecConfiguration {
                    sampling_frequency: crate::bap::SamplingFrequency::from_hz(hz)?,
                    frame_duration: duration,
                    channel_allocation: crate::bap::LOCATION_MONO,
                    octets_per_frame: octets,
                    frames_per_sdu: 1,
                })
            })
            .find(|codec| records.iter().any(|record| record.capabilities.accepts(codec).is_ok()))
            .ok_or(PlanError::NoCompatibleMicrophoneCodec)?;

        let qos = QosConfiguration {
            sdu_interval_us: codec.frame_duration.microseconds(),
            framing: 0,
            phy: 0x02,
            max_sdu: codec.sdu_size(),
            retransmission_number: 2,
            max_transport_latency_ms: 75,
            presentation_delay_us: 40_000,
        };
        self.microphone = Some(MicrophonePlan { ase_id, codec, qos, cis_id: 0 });
        Ok(self)
    }

    pub fn microphone_only(mut self) -> Self {
        self.playback_enabled = false;
        self.ase_ids.clear();
        self.topology = Topology::SingleCis;
        self
    }

    /// Which ear a given stream is for.
    ///
    /// With one CIS carrying both channels the allocation is stereo and there is
    /// nothing to decide. With two, each ASE must be told **which** ear it is,
    /// and getting this wrong is not a subtle imaging problem: both sides
    /// claiming Front Left means the device is receiving two streams that both
    /// say they belong in the same ear. What comes out is thin and hollow, with
    /// the bass and the air gone, because that is what summing two nearly
    /// identical signals sounds like.
    pub fn channel_allocation(&self, index: usize) -> u32 {
        // Legacy: every stream claims the left ear, exactly as it did when this
        // first played. It is wrong, and it made sound.
        if self.legacy {
            return crate::bap::LOCATION_FRONT_LEFT;
        }

        // A single stream in a two-channel topology is mono: there is no other
        // ear to send the rest to, so claiming one side would leave half the
        // music unplayed.
        if self.topology == Topology::DualCis && self.ase_ids.len() == 1 {
            return self.codec.channel_allocation;
        }

        let first_is_left = !self.swap_ears;

        match self.topology {
            Topology::SingleCis => crate::bap::LOCATION_STEREO,
            Topology::DualCis if (index == 0) == first_is_left => crate::bap::LOCATION_FRONT_LEFT,
            Topology::DualCis => crate::bap::LOCATION_FRONT_RIGHT,
        }
    }

    /// The Config Codec writes, one per ASE.
    ///
    /// Separated from what follows because the device answers these with the
    /// QoS it prefers - including the presentation delay range it can actually
    /// work in. Sending Config QoS in the same breath means never reading that
    /// answer, and a value outside the device's range is accepted quietly and
    /// then never streamed.
    pub fn codec_writes(&self) -> Vec<Vec<u8>> {
        let mut per_ase: Vec<Vec<u8>> = self.ase_ids
            .iter()
            .enumerate()
            .map(|(index, &ase_id)| {
                let mut codec = self.codec;
                codec.channel_allocation = self.channel_allocation(index);
                ascs::config_codec(ase_id, self.target_latency, &codec)
            })
            .collect();

        if let Some(microphone) = &self.microphone {
            per_ase.push(ascs::config_codec(
                microphone.ase_id,
                ascs::LATENCY_BALANCED,
                &microphone.codec,
            ));
        }

        vec![ascs::batch(&per_ase)]
    }

    /// The Config QoS and Enable writes, once the device's answer is known.
    pub fn qos_and_enable_writes(&self) -> Vec<Vec<u8>> {
        let mut qos: Vec<Vec<u8>> = self
            .ase_ids
            .iter()
            .enumerate()
            .map(|(index, &ase_id)| {
                ascs::config_qos(ase_id, self.cig_id, index as u8, &self.qos)
            })
            .collect();
        let mut enable: Vec<Vec<u8>> = self
            .ase_ids
            .iter()
            .map(|&ase_id| ascs::enable(ase_id, self.context))
            .collect();

        if let Some(microphone) = &self.microphone {
            qos.push(ascs::config_qos(
                microphone.ase_id,
                self.cig_id,
                microphone.cis_id,
                &microphone.qos,
            ));
            enable.push(ascs::enable(
                microphone.ase_id,
                crate::bap::ascs::CONTEXT_CONVERSATIONAL,
            ));
        }

        vec![ascs::batch(&qos), ascs::batch(&enable)]
    }

    /// Every ASCS write, in order. Kept for callers that do not read the reply.
    pub fn ascs_sequence(&self) -> Vec<Vec<u8>> {
        let mut operations = self.codec_writes();
        operations.extend(self.qos_and_enable_writes());
        operations
    }

    /// The HCI command that creates the isochronous group for this plan.
    pub fn cig_command(&self) -> Vec<u8> {
        let per_cis_sdu = match self.topology {
            Topology::SingleCis => self.codec.sdu_size(),
            // With one channel per CIS, each carries a single channel's frames.
            Topology::DualCis => self.codec.octets_per_frame,
        };

        // One CIS per ASE we actually configured, not per topology. The two are
        // normally the same, but mono mode keeps the one-channel-per-stream
        // codec and asks for a single stream - and a CIG that reserves a channel
        // nobody configured is a channel that can only fail to come up.
        let cis_count = self
            .ase_ids
            .len()
            .max(usize::from(self.microphone.is_some()))
            .max(1);
        let cis: Vec<CisParams> = (0..cis_count)
            .map(|index| CisParams {
                cis_id: index as u8,
                max_sdu_c_to_p: if self.playback_enabled && index < self.ase_ids.len() {
                    per_cis_sdu
                } else {
                    0
                },
                max_sdu_p_to_c: self
                    .microphone
                    .as_ref()
                    .filter(|microphone| microphone.cis_id as usize == index)
                    .map(|microphone| microphone.codec.sdu_size())
                    .unwrap_or(0),
                phy_c_to_p: self.qos.phy,
                phy_p_to_c: self
                    .microphone
                    .as_ref()
                    .filter(|microphone| microphone.cis_id as usize == index)
                    .map(|microphone| microphone.qos.phy)
                    .unwrap_or(self.qos.phy),
                rtn_c_to_p: self.qos.retransmission_number,
                rtn_p_to_c: self
                    .microphone
                    .as_ref()
                    .filter(|microphone| microphone.cis_id as usize == index)
                    .map(|microphone| microphone.qos.retransmission_number)
                    .unwrap_or(0),
            })
            .collect();

        // Sequential, because that is what these headphones accept. Interleaved
        // is the textbook choice for two channels to one device and it was tried
        // here: the second channel still refused to come up. The configuration
        // that does establish both channels uses sequential packing, so that is
        // what ships, and the constant stays for the next device that wants it.
        let packing = if self.legacy { hci::PACKING_SEQUENTIAL } else { self.packing };

        hci::le_set_cig_parameters(
            self.cig_id,
            self.qos.sdu_interval_us,
            self.microphone
                .as_ref()
                .map(|microphone| microphone.qos.sdu_interval_us)
                .unwrap_or(self.qos.sdu_interval_us),
            self.qos.framing,
            self.qos.max_transport_latency_ms,
            self.microphone
                .as_ref()
                .map(|microphone| microphone.qos.max_transport_latency_ms)
                .unwrap_or(self.qos.max_transport_latency_ms),
            packing,
            &cis,
        )
    }

    /// Describes the plan in the terms the settings window shows.
    pub fn describe(&self) -> String {
        let preset = self.codec.preset_name().unwrap_or("custom");
        let hz = self.codec.sampling_frequency.hz().unwrap_or(0);
        let kbps = self.codec.bitrate_per_channel() / 1000;

        format!(
            "{preset}: {} kHz, {:.1} ms, {} B/frame = {} kbps/kanal, {} CIS, ~{} ms",
            hz / 1000,
            self.codec.frame_duration.microseconds() as f32 / 1000.0,
            self.codec.octets_per_frame,
            kbps,
            self.topology.cis_count(),
            self.latency_ms()
        )
    }
}

/// Maps our configuration onto the codec crate's own enums.
fn lc3_frame_duration(duration: crate::bap::FrameDuration) -> Lc3FrameDuration {
    match duration {
        crate::bap::FrameDuration::Ms7_5 => Lc3FrameDuration::SevenPointFiveMs,
        crate::bap::FrameDuration::Ms10 => Lc3FrameDuration::TenMs,
    }
}

fn lc3_sampling_frequency(hz: u32) -> Option<Lc3SamplingFrequency> {
    Some(match hz {
        8_000 => Lc3SamplingFrequency::Hz8000,
        16_000 => Lc3SamplingFrequency::Hz16000,
        24_000 => Lc3SamplingFrequency::Hz24000,
        32_000 => Lc3SamplingFrequency::Hz32000,
        44_100 | 48_000 => Lc3SamplingFrequency::Hz48000,
        _ => return None,
    })
}

/// Wraps the LC3 encoder and hands out one frame per SDU interval.
pub struct AudioEncoder {
    config: CodecConfiguration,
    samples_per_frame: usize,
    /// One counter per CIS. Two channels on two channels are two independent
    /// streams, and a shared counter would make each of them skip every other
    /// number - which the receiver reads as constant packet loss.
    sequence_numbers: Vec<(u16, u16)>,
    // The codec borrows its workspaces. They are owned here as raw boxed
    // slices, and the encoder is dropped before those boxes in Drop below.
    // This keeps the codec allocation-free while streaming without leaking a
    // new set of workspaces on every reconnect.
    encoder: std::mem::ManuallyDrop<Lc3Encoder<'static>>,
    integer_buf: *mut [i16],
    scaler_buf: *mut [Scaler],
    complex_buf: *mut [Complex],
    channels: usize,
}

impl AudioEncoder {
    pub fn new(config: CodecConfiguration) -> Self {
        let channels = config.channel_count() as usize;
        Self::with_channels(config, channels)
    }

    /// An encoder for the dual-CIS layout: the codec configuration describes one
    /// channel per channel, but the capture side still delivers stereo and both
    /// halves have to be encoded before they can go to their own CIS.
    pub fn stereo_pair(config: CodecConfiguration) -> Self {
        Self::with_channels(config, 2)
    }

    pub fn with_channels(config: CodecConfiguration, channels: usize) -> Self {
        let sample_rate = config.sampling_frequency.hz().unwrap_or(48_000);
        let samples_per_frame =
            (sample_rate as u64 * config.frame_duration.microseconds() as u64 / 1_000_000) as usize;

        let channels = channels.max(1);
        let duration = lc3_frame_duration(config.frame_duration);
        let frequency = lc3_sampling_frequency(sample_rate).unwrap_or(Lc3SamplingFrequency::Hz48000);

        // Lc3Encoder borrows these workspaces. Keep their Boxes alive through
        // raw slice pointers and release them explicitly after the encoder.
        let (integer_len, scaler_len, complex_len) =
            Lc3Encoder::calc_working_buffer_lengths(channels, duration, frequency);

        let integer_buf = Box::into_raw(vec![0i16; integer_len].into_boxed_slice());
        let scaler_buf = Box::into_raw(vec![0.0 as Scaler; scaler_len].into_boxed_slice());
        let complex_buf = Box::into_raw(vec![Complex::default(); complex_len].into_boxed_slice());

        let encoder = unsafe {
            Lc3Encoder::new(
                channels,
                duration,
                frequency,
                &mut *integer_buf,
                &mut *scaler_buf,
                &mut *complex_buf,
            )
        };

        Self {
            config,
            samples_per_frame,
            sequence_numbers: Vec::new(),
            encoder: std::mem::ManuallyDrop::new(encoder),
            integer_buf,
            scaler_buf,
            complex_buf,
            channels,
        }
    }

    /// Encodes one channel's worth of samples into exactly `octets_per_frame` bytes.
    ///
    /// LC3 is a fixed-rate codec: the output size is the configuration, not a
    /// result. A short buffer here would silently change the bitrate the device
    /// was told to expect, so the length is asserted rather than trusted.
    pub fn encode_channel(&mut self, channel: usize, samples: &[i16]) -> Result<Vec<u8>, EncodeError> {
        if samples.len() != self.samples_per_frame {
            return Err(EncodeError::WrongFrameLength {
                got: samples.len(),
                expected: self.samples_per_frame,
            });
        }

        if channel >= self.channels {
            return Err(EncodeError::NoSuchChannel(channel));
        }

        let mut out = vec![0u8; self.config.octets_per_frame as usize];
        self.encoder
            .encode_frame(channel, samples, &mut out)
            .map_err(|e| EncodeError::Codec(format!("{e:?}")))?;

        Ok(out)
    }

    /// Turns one interleaved stereo frame into the packets for each channel.
    ///
    /// The routing this performs - left to the first channel, right to the
    /// second - was the last part of the audio path with no test. Everything
    /// around it was measured: the capture is stereo, the two encoder channels
    /// are independent, the allocation reaches the wire. If both earpieces play
    /// the same thing, this is where it would happen, and "it looks correct" is
    /// not the same as knowing.
    ///
    /// With a single channel available the two are folded together, so the
    /// listener hears the whole mix rather than half of it.
    pub fn stereo_packets(
        &mut self,
        interleaved: &[i16],
        cis_handles: &[u16],
        swap: bool,
    ) -> Result<Vec<Vec<u8>>, EncodeError> {
        let (mut left, mut right) = crate::audio::AudioCapture::deinterleave(interleaved);

        // Which earpiece a stream comes out of is the device's decision, and it
        // does not have to follow the channel allocation we send: an ASE can be
        // wired to a fixed side in firmware. Nothing readable from the device
        // says which, so when the sides come out reversed the honest fix is to
        // send the audio the other way round rather than to argue with it.
        if swap {
            std::mem::swap(&mut left, &mut right);
        }

        if cis_handles.len() == 1 {
            let mono: Vec<i16> = left
                .iter()
                .zip(&right)
                .map(|(&l, &r)| ((l as i32 + r as i32) / 2) as i16)
                .collect();

            let payload = self.encode_channel(0, &mono)?;
            return Ok(vec![self.wrap_iso_packet(cis_handles[0], &payload)]);
        }

        let mut packets = Vec::with_capacity(cis_handles.len());

        for (channel, source) in [(0usize, &left), (1usize, &right)] {
            let Some(&handle) = cis_handles.get(channel) else {
                continue;
            };
            let payload = self.encode_channel(channel, source)?;
            packets.push(self.wrap_iso_packet(handle, &payload));
        }

        Ok(packets)
    }

    /// Encodes an interleaved stereo frame into the payload for one SDU.
    ///
    /// With both channels on one CIS the payload is left then right, back to back.
    pub fn encode_interleaved(&mut self, interleaved: &[i16]) -> Result<Vec<u8>, EncodeError> {
        if self.channels == 1 {
            return self.encode_channel(0, interleaved);
        }

        let mut left = Vec::with_capacity(self.samples_per_frame);
        let mut right = Vec::with_capacity(self.samples_per_frame);
        for pair in interleaved.chunks_exact(2) {
            left.push(pair[0]);
            right.push(pair[1]);
        }

        let mut payload = self.encode_channel(0, &left)?;
        payload.extend_from_slice(&self.encode_channel(1, &right)?);
        Ok(payload)
    }

    /// How many samples per channel one LC3 frame consumes.
    ///
    /// At 48 kHz this is 360 samples for a 7.5 ms frame and 480 for 10 ms - the
    /// audio bridge must deliver exactly this much per interval or the stream drifts.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    pub fn octets_per_frame(&self) -> u16 {
        self.config.octets_per_frame
    }

    /// Wraps an encoded payload in an HCI ISO data packet.
    ///
    /// The sequence number must advance by exactly one per SDU; a gap makes the
    /// controller treat the stream as broken.
    pub fn wrap_iso_packet(&mut self, cis_handle: u16, payload: &[u8]) -> Vec<u8> {
        let slot = match self.sequence_numbers.iter().position(|(h, _)| *h == cis_handle) {
            Some(index) => index,
            None => {
                self.sequence_numbers.push((cis_handle, 0));
                self.sequence_numbers.len() - 1
            }
        };

        let sequence = self.sequence_numbers[slot].1;
        self.sequence_numbers[slot].1 = sequence.wrapping_add(1);
        hci::iso_data_packet(cis_handle, sequence, payload)
    }

    pub fn sequence_number(&self, cis_handle: u16) -> u16 {
        self.sequence_numbers
            .iter()
            .find(|(h, _)| *h == cis_handle)
            .map(|(_, sequence)| *sequence)
            .unwrap_or(0)
    }

    /// Restarts numbering, for when a stream is torn down and set up again.
    pub fn reset(&mut self) {
        self.sequence_numbers.clear();
    }
}

impl Drop for AudioEncoder {
    fn drop(&mut self) {
        unsafe {
            // The dependent must go first; only then may its borrowed storage
            // be reconstructed and freed.
            std::mem::ManuallyDrop::drop(&mut self.encoder);
            drop(Box::from_raw(self.integer_buf));
            drop(Box::from_raw(self.scaler_buf));
            drop(Box::from_raw(self.complex_buf));
        }
    }
}

/// Allocation-free decoder for the headset's mono Source ASE.
pub struct AudioDecoder {
    samples_per_frame: usize,
    decoder: std::mem::ManuallyDrop<Lc3Decoder<'static>>,
    scaler_buf: *mut [Scaler],
    complex_buf: *mut [Complex],
}

impl AudioDecoder {
    pub fn new(config: CodecConfiguration) -> Self {
        let sample_rate = config.sampling_frequency.hz().unwrap_or(32_000);
        let samples_per_frame =
            (sample_rate as u64 * config.frame_duration.microseconds() as u64 / 1_000_000) as usize;
        let duration = lc3_frame_duration(config.frame_duration);
        let frequency = lc3_sampling_frequency(sample_rate).unwrap_or(Lc3SamplingFrequency::Hz32000);
        let (scaler_len, complex_len) =
            Lc3Decoder::calc_working_buffer_lengths(1, duration, frequency);
        let scaler_buf = Box::into_raw(vec![0.0 as Scaler; scaler_len].into_boxed_slice());
        let complex_buf = Box::into_raw(vec![Complex::default(); complex_len].into_boxed_slice());
        let decoder = unsafe {
            Lc3Decoder::new(1, duration, frequency, &mut *scaler_buf, &mut *complex_buf)
        };
        Self {
            samples_per_frame,
            decoder: std::mem::ManuallyDrop::new(decoder),
            scaler_buf,
            complex_buf,
        }
    }

    pub fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>, EncodeError> {
        let mut samples = vec![0i16; self.samples_per_frame];
        self.decoder
            .decode_frame(16, 0, payload, &mut samples)
            .map_err(|error| EncodeError::Codec(format!("LC3 microphone decode: {error:?}")))?;
        Ok(samples)
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.decoder);
            drop(Box::from_raw(self.scaler_buf));
            drop(Box::from_raw(self.complex_buf));
        }
    }
}

/// Builds capabilities as a device would report them, for planning without hardware.
pub fn synthetic_capabilities(stereo_capable: bool, ase_count: usize) -> AudioCapabilities {
    use crate::bap::Ltv;

    let caps = Ltv::encode(&[
        Ltv::new(0x01, 0x00E4u16.to_le_bytes().to_vec()), // 16, 24, 32, 48 kHz
        Ltv::new(0x02, vec![0x03]),                       // 7.5 and 10 ms
        Ltv::new(0x03, vec![if stereo_capable { 0x03 } else { 0x01 }]),
        Ltv::new(0x04, [26u16.to_le_bytes(), 155u16.to_le_bytes()].concat()),
        Ltv::new(0x05, vec![if stereo_capable { 2 } else { 1 }]),
    ]);

    let mut record = vec![1u8];
    record.extend_from_slice(&[0x06, 0x00, 0x00, 0x00, 0x00]);
    record.push(caps.len() as u8);
    record.extend_from_slice(&caps);
    record.push(0);

    AudioCapabilities {
        sink_records: PacRecord::parse_characteristic(&record),
        source_records: PacRecord::parse_characteristic(&record),
        sink_ase_ids: (1..=ase_count as u8).collect(),
        source_ase_ids: vec![5],
        ..Default::default()
    }
}

/// Reports whether a device's own capabilities allow the single-CIS layout.
pub fn can_use_single_cis(capabilities: &AudioCapabilities) -> bool {
    capabilities
        .sink_records
        .iter()
        .filter(|r| r.is_lc3())
        .any(|r| r.capabilities.supports_stereo_in_one_stream())
}

/// Convenience view of a device's LC3 limits, for the settings window.
pub fn describe_limits(caps: &CodecCapabilities) -> String {
    let rates: Vec<String> = caps
        .sampling_frequencies
        .iter()
        .filter_map(|f| f.hz())
        .map(|hz| format!("{} kHz", hz / 1000))
        .collect();

    let (min, max) = (
        caps.min_octets_per_frame.unwrap_or(0),
        caps.max_octets_per_frame.unwrap_or(0),
    );

    format!(
        "rates: {}; frames: {}{}{}; octets {}-{}",
        rates.join(", "),
        if caps.supports_7_5ms { "7.5 ms" } else { "" },
        if caps.supports_7_5ms && caps.supports_10ms { " / " } else { "" },
        if caps.supports_10ms { "10 ms" } else { "" },
        min,
        max
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_capable_device_gets_one_cis() {
        let caps = synthetic_capabilities(true, 2);
        assert!(can_use_single_cis(&caps));

        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();

        assert_eq!(plan.topology, Topology::SingleCis);
        assert_eq!(plan.ase_ids.len(), 1, "one CIS needs only one ASE");
        assert_eq!(plan.codec.channel_count(), 2);
    }

    #[test]
    fn mono_only_device_falls_back_to_two_cis() {
        let caps = synthetic_capabilities(false, 2);
        assert!(!can_use_single_cis(&caps));

        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();

        assert_eq!(plan.topology, Topology::DualCis);
        assert_eq!(plan.ase_ids, vec![1, 2]);
    }

    #[test]
    fn asking_for_two_cis_is_honoured_even_when_one_would_work() {
        let caps = synthetic_capabilities(true, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();

        assert_eq!(plan.topology, Topology::DualCis);
    }

    #[test]
    fn cig_command_sizes_each_cis_correctly() {
        let caps = synthetic_capabilities(true, 2);

        // Single CIS carries both channels: the SDU holds two frames.
        let single = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();
        let command = single.cig_command();
        assert_eq!(command[2] as usize, 15 + 9, "one CIS entry");
        assert_eq!(u16::from_le_bytes([command[19], command[20]]), 360);

        // Dual CIS: each carries one channel's frame.
        let dual = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        let command = dual.cig_command();
        assert_eq!(command[2] as usize, 15 + 18, "two CIS entries");
        assert_eq!(u16::from_le_bytes([command[19], command[20]]), 90);
    }

    #[test]
    fn custom_single_cis_qos_matches_the_stereo_sdu() {
        let caps = synthetic_capabilities(true, 2);
        let codec = Preset::WindowsDefault.codec(false);
        let qos = Preset::WindowsDefault.qos(&codec);
        let plan = StreamPlan::build_custom(&caps, codec, qos, true).unwrap();

        assert_eq!(plan.topology, Topology::SingleCis);
        assert_eq!(plan.qos.max_sdu, plan.codec.sdu_size());
        assert!(plan.qos.max_sdu > qos.max_sdu, "stereo SDU must include both channels");
    }

    #[test]
    fn microphone_makes_first_cis_bidirectional_and_uses_source_ase() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false)
            .unwrap()
            .with_microphone(&caps, MicrophoneQuality::Balanced)
            .unwrap();

        let microphone = plan.microphone.as_ref().unwrap();
        assert_eq!(microphone.ase_id, 5);
        assert_eq!(microphone.codec.sampling_frequency.hz(), Some(32_000));
        let command = plan.cig_command();
        assert_eq!(u16::from_le_bytes([command[21], command[22]]), 80);

        let codec_batch = &plan.codec_writes()[0];
        assert_eq!(codec_batch[1], 3, "two playback ASEs plus one Source ASE");
        let enable_batch = &plan.qos_and_enable_writes()[1];
        assert_eq!(enable_batch[1], 3);
    }

    #[test]
    fn swapping_the_ears_swaps_which_stream_claims_which() {
        let mut plan =
            StreamPlan::build(&synthetic_capabilities(false, 2), Preset::WindowsDefault, false)
                .unwrap();

        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_LEFT);
        assert_eq!(plan.channel_allocation(1), crate::bap::LOCATION_FRONT_RIGHT);

        plan.swap_ears = true;
        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_RIGHT);
        assert_eq!(plan.channel_allocation(1), crate::bap::LOCATION_FRONT_LEFT);

        // And it has to reach the wire, not only the accessor.
        let allocations: Vec<u32> = plan
            .ascs_sequence()
            .iter()
            .filter(|op| op[0] == ascs::OP_CONFIG_CODEC)
            .flat_map(|op| {
                op.windows(6).filter_map(|ltv| {
                    (ltv[0] == 0x05 && ltv[1] == 0x03)
                        .then(|| u32::from_le_bytes([ltv[2], ltv[3], ltv[4], ltv[5]]))
                })
            })
            .collect();

        assert_eq!(
            allocations,
            vec![crate::bap::LOCATION_FRONT_RIGHT, crate::bap::LOCATION_FRONT_LEFT]
        );
    }

    #[test]
    fn packing_follows_the_setting_except_in_legacy() {
        const PACKING: usize = 3 + 1 + 3 + 3 + 1;

        let mut plan =
            StreamPlan::build(&synthetic_capabilities(false, 2), Preset::WindowsDefault, false)
                .unwrap();

        plan.packing = crate::hci::PACKING_INTERLEAVED;
        assert_eq!(plan.cig_command()[PACKING], ascs_packing::INTERLEAVED);

        // Legacy reproduces one exact configuration, so it ignores the setting.
        let legacy = plan.into_legacy();
        assert_eq!(legacy.cig_command()[PACKING], ascs_packing::SEQUENTIAL);
    }

    #[test]
    fn legacy_mode_reproduces_the_configuration_that_first_played() {
        const PACKING: usize = 3 + 1 + 3 + 3 + 1;

        let plan = StreamPlan::build(&synthetic_capabilities(false, 2), Preset::WindowsDefault, false)
            .unwrap()
            .into_legacy();

        // Both streams claim the left ear, which is wrong and is what played.
        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_LEFT);
        assert_eq!(plan.channel_allocation(1), crate::bap::LOCATION_FRONT_LEFT);

        // And sequential packing, as it was before interleaved was tried.
        assert_eq!(plan.cig_command()[PACKING], ascs_packing::SEQUENTIAL);

        // Still two streams: legacy is not mono.
        assert_eq!(plan.ase_ids.len(), 2);
    }

    #[test]
    fn channels_are_packed_sequentially() {
        // Byte layout: cig_id, SDU interval C->P (3), SDU interval P->C (3),
        // worst case SCA, packing. The command header is three bytes.
        const PACKING: usize = 3 + 1 + 3 + 3 + 1;

        let stereo = StreamPlan::build(&synthetic_capabilities(false, 2), Preset::WindowsDefault, false)
            .unwrap();
        assert_eq!(stereo.cig_command()[PACKING], ascs_packing::SEQUENTIAL);

        let mono = stereo.into_mono();
        assert_eq!(mono.cig_command()[PACKING], ascs_packing::SEQUENTIAL);
    }

    /// Local aliases, so the test reads as the specification does.
    mod ascs_packing {
        pub const SEQUENTIAL: u8 = crate::hci::PACKING_SEQUENTIAL;
        pub const INTERLEAVED: u8 = crate::hci::PACKING_INTERLEAVED;
    }

    #[test]
    fn mono_asks_for_one_stream_that_claims_neither_ear() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false)
            .unwrap()
            .into_mono();

        assert_eq!(plan.ase_ids.len(), 1, "one stream only");
        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_LEFT);

        // Only one ASE is configured, so only one channel is reserved. A
        // channel the group reserves but no ASE was configured for can do
        // nothing except fail to come up.
        let configured = plan
            .ascs_sequence()
            .iter()
            .filter(|op| op[0] == ascs::OP_CONFIG_CODEC)
            .map(|op| op[1] as usize)
            .sum::<usize>();
        assert_eq!(configured, 1);

        // The CIG command carries the CIS count as the byte before the first
        // CIS entry; with a single stream it must be one.
        let cig = plan.cig_command();
        assert_eq!(cig[17], 1, "the group must reserve exactly one channel");
    }

    #[test]
    /// The last untested link in the audio path.
    ///
    /// Left must reach the first channel and right the second, in different
    /// packets, with different payloads. If both earpieces play the same thing,
    /// this is the test that would have caught it.
    fn left_and_right_reach_different_channels() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        let mut encoder = AudioEncoder::stereo_pair(plan.codec);

        // Left carries a tone; right is silent. Nothing subtle.
        let frame: Vec<i16> = (0..encoder.samples_per_frame())
            .flat_map(|i| {
                let value = ((i as f32 * 0.2).sin() * 20_000.0) as i16;
                [value, 0]
            })
            .collect();

        let handles = [0x0017u16, 0x0018];
        let packets = encoder.stereo_packets(&frame, &handles, false).unwrap();

        assert_eq!(packets.len(), 2, "one packet per channel");

        // Each packet must name its own channel.
        let handle_of = |packet: &[u8]| u16::from_le_bytes([packet[0], packet[1]]) & 0x0FFF;
        assert_eq!(handle_of(&packets[0]), handles[0]);
        assert_eq!(handle_of(&packets[1]), handles[1]);

        // And carry different audio: a silent right channel cannot encode to
        // the same bytes as a channel with a tone in it.
        assert_ne!(
            &packets[0][8..],
            &packets[1][8..],
            "both channels carried identical audio - both earpieces would play the same"
        );
    }

    #[test]
    fn swapping_sends_the_left_channel_to_the_second_stream() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();

        // Left has a tone, right is silent.
        let frame: Vec<i16> = (0..AudioEncoder::stereo_pair(plan.codec).samples_per_frame())
            .flat_map(|i| [((i as f32 * 0.2).sin() * 20_000.0) as i16, 0])
            .collect();

        let handles = [0x0017u16, 0x0018];

        let mut plain = AudioEncoder::stereo_pair(plan.codec);
        let straight = plain.stereo_packets(&frame, &handles, false).unwrap();

        let mut crossed = AudioEncoder::stereo_pair(plan.codec);
        let swapped = crossed.stereo_packets(&frame, &handles, true).unwrap();

        // Same handles either way: only the audio moves.
        for index in 0..2 {
            assert_eq!(&straight[index][..2], &swapped[index][..2]);
        }

        // The tone that went to the first stream now goes to the second.
        assert_eq!(&straight[0][8..], &swapped[1][8..], "left moved to the other stream");
        assert_eq!(&straight[1][8..], &swapped[0][8..], "right moved to the other stream");
    }

    #[test]
    fn one_channel_gets_the_whole_mix_rather_than_half_of_it() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        let mut encoder = AudioEncoder::stereo_pair(plan.codec);

        // Everything is in the right channel; the left is silent.
        let frame: Vec<i16> = (0..encoder.samples_per_frame())
            .flat_map(|i| [0, ((i as f32 * 0.2).sin() * 20_000.0) as i16])
            .collect();

        let mono = encoder.stereo_packets(&frame, &[0x0017], false).unwrap();
        assert_eq!(mono.len(), 1);

        // Compare against an encode of pure silence: the fold must not have
        // thrown the right channel away.
        let mut reference = AudioEncoder::stereo_pair(plan.codec);
        let silence = vec![0i16; encoder.samples_per_frame() * 2];
        let quiet = reference.stereo_packets(&silence, &[0x0017], false).unwrap();

        assert_ne!(&mono[0][8..], &quiet[0][8..], "the right channel was dropped");
    }

    #[test]
    fn each_ear_is_told_which_ear_it_is() {
        // Two ASEs both claiming Front Left is what a stereo pair sounds like
        // when it is summed into one ear: hollow, no bass, no air. The bug was
        // invisible in every log because the audio itself was fine.
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        assert_eq!(plan.topology, Topology::DualCis);

        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_LEFT);
        assert_eq!(plan.channel_allocation(1), crate::bap::LOCATION_FRONT_RIGHT);

        // And it has to reach the wire, not just the accessor.
        let allocations: Vec<u32> = plan
            .ascs_sequence()
            .iter()
            .filter(|op| op[0] == ascs::OP_CONFIG_CODEC)
            .flat_map(|op| {
                // Audio_Channel_Allocation is the 5-byte LTV with type 0x03.
                op.windows(6).filter_map(|ltv| {
                    (ltv[0] == 0x05 && ltv[1] == 0x03)
                        .then(|| u32::from_le_bytes([ltv[2], ltv[3], ltv[4], ltv[5]]))
                })
            })
            .collect();

        assert_eq!(
            allocations,
            vec![crate::bap::LOCATION_FRONT_LEFT, crate::bap::LOCATION_FRONT_RIGHT],
            "left ear then right ear"
        );
    }

    #[test]
    fn the_stream_never_announces_a_conversational_context() {
        // Announcing Conversational is what puts headphones into headset mode:
        // the microphone comes up, the codec drops to a bidirectional
        // configuration, and the music sounds like a phone call. Nothing in
        // this stack has any reason to ask for it.
        for records in [1usize, 2] {
            let caps = synthetic_capabilities(false, records);
            let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();

            for operation in plan.ascs_sequence() {
                if operation[0] != ascs::OP_ENABLE {
                    continue;
                }

                let context = u16::from_le_bytes([
                    operation[operation.len() - 2],
                    operation[operation.len() - 1],
                ]);

                assert_eq!(
                    context,
                    ascs::CONTEXT_MEDIA,
                    "enable must announce Media, not {context:#06x}"
                );
                assert_ne!(context, ascs::CONTEXT_CONVERSATIONAL);
            }
        }
    }

    #[test]
    fn ascs_sequence_configures_every_ase_in_order() {
        let caps = synthetic_capabilities(false, 2);
        let plan = StreamPlan::build(&caps, Preset::LowLatency, false).unwrap();

        let sequence = plan.ascs_sequence();
        assert_eq!(sequence.len(), 3, "one batched config, qos and enable operation");

        assert_eq!(sequence[0][0], ascs::OP_CONFIG_CODEC);
        assert_eq!(sequence[1][0], ascs::OP_CONFIG_QOS);
        assert_eq!(sequence[2][0], ascs::OP_ENABLE);
        assert_eq!(sequence[0][1], 2, "both ASEs in the codec operation");
        assert_eq!(sequence[1][1], 2, "both ASEs in the QoS operation");
        assert_eq!(sequence[2][1], 2, "both ASEs in the enable operation");
        assert_eq!(sequence[0][2], 1, "first ASE id");
        // The codec-specific LTV length is carried by the record itself. Do not
        // bake one preset's optional LTVs into the parser used by this test.
        let first_record_len = 9 + sequence[0][10] as usize;
        assert_eq!(sequence[0][2 + first_record_len], 2, "second ASE id");
    }

    #[test]
    fn out_of_range_preset_falls_back_instead_of_failing() {
        // A device that only reaches 90 octets cannot do the HighQuality preset.
        let mut caps = synthetic_capabilities(true, 1);
        if let Some(record) = caps.sink_records.first_mut() {
            record.capabilities.max_octets_per_frame = Some(90);
        }

        assert!(StreamPlan::build(&caps, Preset::HighQuality, true).is_err());

        let (plan, chosen) =
            StreamPlan::build_with_fallback(&caps, Preset::HighQuality, true).unwrap();
        assert_eq!(chosen, Preset::WindowsDefault);
        assert_eq!(plan.codec.octets_per_frame, 90);
    }

    /// Builds one PAC record for a single sample rate, the way real devices do.
    fn record_for(frequency_bits: u16, min_octets: u16, max_octets: u16) -> Vec<u8> {
        use crate::bap::Ltv;

        let caps = Ltv::encode(&[
            Ltv::new(0x01, frequency_bits.to_le_bytes().to_vec()),
            Ltv::new(0x02, vec![0x03]), // 7.5 and 10 ms
            Ltv::new(0x03, vec![0x01]), // one channel per stream
            Ltv::new(0x04, [min_octets.to_le_bytes(), max_octets.to_le_bytes()].concat()),
            Ltv::new(0x05, vec![1]),
        ]);

        let mut record = vec![0x06, 0x00, 0x00, 0x00, 0x00];
        record.push(caps.len() as u8);
        record.extend_from_slice(&caps);
        record.push(0);
        record
    }

    /// The JBL Tune 780NC publishes 16 kHz first and 48 kHz last, one record per
    /// rate. Planning against only the first record rejected every configuration
    /// the device was perfectly able to play.
    /// Two CIS are two independent streams. Sharing one counter would make each
    /// of them advance by two, which a receiver reads as every other packet
    /// having been lost.
    #[test]
    fn each_cis_numbers_its_own_packets() {
        let config = Preset::WindowsDefault.codec(false);
        let mut encoder = AudioEncoder::stereo_pair(config);

        for _ in 0..3 {
            encoder.wrap_iso_packet(0x0017, &[0u8; 4]);
            encoder.wrap_iso_packet(0x0018, &[0u8; 4]);
        }

        assert_eq!(encoder.sequence_number(0x0017), 3);
        assert_eq!(encoder.sequence_number(0x0018), 3);

        // The number really is in the packet, not just in the counter.
        let packet = encoder.wrap_iso_packet(0x0017, &[0u8; 4]);
        assert_eq!(u16::from_le_bytes([packet[4], packet[5]]), 3);
    }

    #[test]
    fn a_rate_in_a_later_record_is_still_found() {
        let mut characteristic = vec![2u8]; // two records follow
        characteristic.extend_from_slice(&record_for(0x0001, 30, 40)); // 8 kHz only
        characteristic.extend_from_slice(&record_for(0x0080, 75, 155)); // 48 kHz

        let capabilities = AudioCapabilities {
            sink_records: PacRecord::parse_characteristic(&characteristic),
            sink_ase_ids: vec![1, 2],
            ..Default::default()
        };

        assert_eq!(capabilities.sink_records.len(), 2, "both records must parse");

        let plan = StreamPlan::build(&capabilities, Preset::WindowsDefault, false)
            .expect("48 kHz is published, just not first");

        assert_eq!(plan.codec.sampling_frequency.hz(), Some(48_000));
    }

    #[test]
    fn device_without_lc3_is_rejected_clearly() {
        let caps = AudioCapabilities::default();
        match StreamPlan::build(&caps, Preset::WindowsDefault, true) {
            Err(PlanError::NoLc3Sink) => {}
            other => panic!("expected NoLc3Sink, got {:?}", other.map(|p| p.describe())),
        }
    }

    #[test]
    fn encoder_frame_size_matches_the_sample_rate() {
        let caps = synthetic_capabilities(true, 1);

        // 48 kHz at 7.5 ms is 360 samples per channel.
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();
        let encoder = AudioEncoder::new(plan.codec);
        assert_eq!(encoder.samples_per_frame(), 360);

        // 48 kHz at 7.5 ms is 360.
        let low = StreamPlan::build(&caps, Preset::LowLatency, true).unwrap();
        assert_eq!(AudioEncoder::new(low.codec).samples_per_frame(), 360);
    }

    #[test]
    fn iso_sequence_numbers_advance_by_one() {
        let caps = synthetic_capabilities(true, 1);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();
        let mut encoder = AudioEncoder::new(plan.codec);

        let payload = vec![0xAAu8; 100];
        let first = encoder.wrap_iso_packet(0x0060, &payload);
        let second = encoder.wrap_iso_packet(0x0060, &payload);

        assert_eq!(u16::from_le_bytes([first[4], first[5]]), 0);
        assert_eq!(u16::from_le_bytes([second[4], second[5]]), 1);

        // Handle survives, and the SDU length field matches the payload.
        assert_eq!(u16::from_le_bytes([first[0], first[1]]) & 0x0FFF, 0x0060);
        assert_eq!(u16::from_le_bytes([first[6], first[7]]), 100);
    }

    #[test]
    fn encoder_produces_exactly_the_configured_octet_count() {
        let caps = synthetic_capabilities(true, 1);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, true).unwrap();
        let mut encoder = AudioEncoder::new(plan.codec);

        // One second of a 440 Hz tone, sliced to one frame.
        let samples: Vec<i16> = (0..encoder.samples_per_frame() * 2)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16
            })
            .collect();

        let payload = encoder.encode_interleaved(&samples).unwrap();

        // Stereo on one CIS: two channels, each exactly octets_per_frame bytes.
        assert_eq!(payload.len(), plan.codec.octets_per_frame as usize * 2);

        // Encoded audio must not be all zeros, or nothing was actually coded.
        assert!(payload.iter().any(|&b| b != 0), "encoder produced silence");
    }

    #[test]
    fn encoder_refuses_a_short_frame() {
        let caps = synthetic_capabilities(false, 1);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        let mut encoder = AudioEncoder::new(plan.codec);

        // A partial frame would silently shift every later frame out of alignment.
        let short = vec![0i16; encoder.samples_per_frame() - 1];
        match encoder.encode_channel(0, &short) {
            Err(EncodeError::WrongFrameLength { got, expected }) => {
                assert_eq!(got, expected - 1);
            }
            other => panic!("short frame must be refused, got {other:?}"),
        }
    }

    #[test]
    fn different_audio_encodes_differently() {
        let caps = synthetic_capabilities(false, 1);
        let plan = StreamPlan::build(&caps, Preset::WindowsDefault, false).unwrap();
        let mut encoder = AudioEncoder::new(plan.codec);

        let silence = vec![0i16; encoder.samples_per_frame()];
        let tone: Vec<i16> = (0..encoder.samples_per_frame())
            .map(|i| ((i as f32 * 0.1).sin() * 8000.0) as i16)
            .collect();

        let encoded_silence = encoder.encode_channel(0, &silence).unwrap();
        let encoded_tone = encoder.encode_channel(0, &tone).unwrap();

        assert_ne!(encoded_silence, encoded_tone, "encoder must react to input");
    }

    #[test]
    fn a_device_with_one_endpoint_still_gets_both_ears() {
        // One Sink ASE and stereo on one stream: the two-stream layout has
        // nowhere to put the right channel, so it must not be chosen even
        // though it is otherwise preferred.
        let capabilities = synthetic_capabilities(true, 1);

        let plan = StreamPlan::build(&capabilities, Preset::WindowsDefault, false)
            .expect("a single-endpoint device is still playable");

        assert_eq!(plan.topology, Topology::SingleCis);
        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_STEREO);
    }

    #[test]
    fn two_endpoints_use_the_layout_windows_negotiates() {
        // The same device with two endpoints takes the proven path, even though
        // it could carry both channels on one stream.
        let capabilities = synthetic_capabilities(true, 4);

        let plan = StreamPlan::build(&capabilities, Preset::WindowsDefault, false)
            .expect("plannable");

        assert_eq!(plan.topology, Topology::DualCis);
        assert_eq!(plan.ase_ids.len(), 2);
        assert_eq!(plan.channel_allocation(0), crate::bap::LOCATION_FRONT_LEFT);
        assert_eq!(plan.channel_allocation(1), crate::bap::LOCATION_FRONT_RIGHT);
    }

    #[test]
    fn low_latency_plan_stays_under_the_target() {
        let caps = synthetic_capabilities(true, 1);
        let plan = StreamPlan::build(&caps, Preset::LowLatency, true).unwrap();

        assert!(plan.latency_ms() < 50, "got {} ms", plan.latency_ms());
        assert!(plan.describe().contains("1 CIS"));
    }
}
