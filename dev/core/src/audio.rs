//! Capturing Windows audio so it can be encoded and sent to the headphones.
//!
//! The route is deliberately boring, because boring is what makes it work with
//! every application rather than a special few:
//!
//! ```text
//!   YouTube, games, anything  ->  Windows mixer  ->  CABLE Input (render)
//!                                                          |
//!                                                    virtual cable
//!                                                          v
//!   LC3 encoder  <-  this module  <-  CABLE Output (capture)
//! ```
//!
//! Set the cable as the default output device and the whole system feeds it.
//! Per-application volume, the mixer and output switching all keep working,
//! because as far as Windows is concerned this is an ordinary sound card.

use std::collections::VecDeque;
use std::slice;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{
    eCapture, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    AVRT_PRIORITY_HIGH,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

/// Friendly-name property of an audio endpoint.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

/// Names that identify the virtual cable, in the order we prefer them.
const CABLE_HINTS: &[&str] = &["CABLE Output", "VB-Audio", "VoiceMeeter Output"];
const CABLE_RENDER_HINTS: &[&str] = &["CABLE Input", "VB-Audio", "VoiceMeeter Input"];

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("COM initialisation failed: {0}")]
    Com(String),

    #[error("no capture device matching {0:?} - install VB-Audio Cable, or name the device explicitly")]
    NoCableDevice(String),

    #[error("audio client error: {0}")]
    Client(String),

    #[error("device runs at {actual} Hz with {channels} channels, but the stream needs {expected} Hz stereo")]
    FormatMismatch {
        actual: u32,
        channels: u16,
        expected: u32,
    },

    #[error("device delivers {0}-bit samples, which this stack cannot convert - set the cable to 16 or 24 bit")]
    UnsupportedSampleFormat(u16),
}

type Result<T> = std::result::Result<T, AudioError>;

/// Keeps the current thread in Windows' MMCSS "Pro Audio" scheduling class.
///
/// This does not make encoding faster or change codec data. It tells Windows
/// that a short scheduling delay here is audible, so background work should not
/// hold this thread past its next 7.5/10 ms deadline. Failure is harmless: the
/// ordinary thread priority remains in effect.
pub struct AudioThreadPriority(Option<windows::Win32::Foundation::HANDLE>);

impl AudioThreadPriority {
    pub fn enter() -> Self {
        let task: Vec<u16> = "Pro Audio".encode_utf16().chain(std::iter::once(0)).collect();
        let mut index = 0u32;

        unsafe {
            match AvSetMmThreadCharacteristicsW(PCWSTR(task.as_ptr()), &mut index) {
                Ok(handle) => {
                    let _ = AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH);
                    Self(Some(handle))
                }
                Err(_) => Self(None),
            }
        }
    }
}

impl Drop for AudioThreadPriority {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}

/// One audio endpoint we could capture from.
#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub name: String,
    pub id: String,
}

impl AudioDevice {
    /// True when this looks like the virtual cable rather than a real microphone.
    /// True for the multi-channel cable variants, which are the wrong choice
    /// for a stereo stream.
    ///
    /// Recognised by name because the channel count is only knowable by opening
    /// the device, and opening the wrong one to find out is exactly what this
    /// avoids.
    pub fn is_multichannel_cable(&self) -> bool {
        let name = self.name.to_lowercase();
        name.contains("16ch") || name.contains("8ch") || name.contains("64ch")
    }

    pub fn is_virtual_cable(&self) -> bool {
        CABLE_HINTS
            .iter()
            .any(|hint| self.name.to_lowercase().contains(&hint.to_lowercase()))
    }
}

/// Initialises COM for this thread. Safe to call more than once.
fn ensure_com() -> Result<()> {
    unsafe {
        let result = CoInitializeEx(None, COINIT_MULTITHREADED);
        // S_FALSE means this thread was already initialised, which is fine.
        if result.is_err() && result.0 != 1 {
            return Err(AudioError::Com(format!("{result:?}")));
        }
    }
    Ok(())
}

fn device_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok()?;
        let text = PropVariantToStringAlloc(&value).ok()?;
        let name = text.to_string().ok()?;
        Some(name)
    }
}

/// Lists every active capture endpoint.
pub fn list_capture_devices() -> Result<Vec<AudioDevice>> {
    list_devices(eCapture)
}

/// Lists render endpoints, including CABLE Input. Writing the decoded headset
/// microphone there makes it available to applications on CABLE Output.
pub fn list_render_devices() -> Result<Vec<AudioDevice>> {
    list_devices(eRender)
}

fn list_devices(flow: windows::Win32::Media::Audio::EDataFlow) -> Result<Vec<AudioDevice>> {
    ensure_com()?;

    let mut devices = Vec::new();

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| AudioError::Com(e.to_string()))?;

        let collection = enumerator
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .map_err(|e| AudioError::Com(e.to_string()))?;

        let count = collection.GetCount().map_err(|e| AudioError::Com(e.to_string()))?;

        for index in 0..count {
            let Ok(device) = collection.Item(index) else {
                continue;
            };

            let Some(name) = device_name(&device) else {
                continue;
            };

            let id = device
                .GetId()
                .ok()
                .and_then(|id| id.to_string().ok())
                .unwrap_or_default();

            devices.push(AudioDevice { name, id });
        }
    }

    Ok(devices)
}

pub fn find_cable_render_device(preferred_name: Option<&str>) -> Result<AudioDevice> {
    let devices = list_render_devices()?;
    if let Some(wanted) = preferred_name {
        return devices
            .into_iter()
            .find(|device| device.name.to_lowercase().contains(&wanted.to_lowercase()))
            .ok_or_else(|| AudioError::NoCableDevice(wanted.to_owned()));
    }

    devices
        .into_iter()
        .find(|device| {
            let name = device.name.to_lowercase();
            CABLE_RENDER_HINTS
                .iter()
                .any(|hint| name.contains(&hint.to_lowercase()))
                && !device.is_multichannel_cable()
        })
        .ok_or_else(|| AudioError::NoCableDevice(CABLE_RENDER_HINTS.join(" / ")))
}

/// Finds the virtual cable, or the named device if one is given.
pub fn find_cable_device(preferred_name: Option<&str>) -> Result<AudioDevice> {
    let devices = list_capture_devices()?;

    if let Some(wanted) = preferred_name {
        return devices
            .into_iter()
            .find(|d| {
                d.id == wanted || d.name.to_lowercase().contains(&wanted.to_lowercase())
            })
            .ok_or_else(|| AudioError::NoCableDevice(wanted.to_owned()));
    }

    // More than one cable can be installed, and they are not interchangeable.
    // The multi-channel variants make Windows upmix stereo into sixteen
    // channels and hand it back downmixed, which collapses the stereo image and
    // cancels the bass - all before this stack sees a sample. Taking whichever
    // one happened to enumerate first is how that gets chosen by accident.
    let mut cables: Vec<AudioDevice> = devices
        .into_iter()
        .filter(AudioDevice::is_virtual_cable)
        .collect();

    cables.sort_by_key(|device| if device.is_multichannel_cable() { 1 } else { 0 });

    cables
        .into_iter()
        .next()
        .ok_or_else(|| AudioError::NoCableDevice(CABLE_HINTS.join(" / ")))
}

/// Whether the endpoint delivers floating point samples.
///
/// The shared-mode mix format is usually 32-bit float, but it is not always,
/// and the sample width alone cannot be trusted to say so: 24-bit integers in a
/// 32-bit container are a normal configuration, and reading those as floats
/// produces noise instead of a diagnosable error. So ask the format itself -
/// through the extensible subformat when there is one, since that is where the
/// answer lives whenever the tag is `WAVE_FORMAT_EXTENSIBLE`.
///
/// # Safety
///
/// `format` must point to a valid `WAVEFORMATEX`, and to a full
/// `WAVEFORMATEXTENSIBLE` whenever its tag says so - which is what
/// `GetMixFormat` guarantees.
unsafe fn format_is_float(format: *const WAVEFORMATEX) -> bool {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, which the windows crate does not export.
    const SUBTYPE_IEEE_FLOAT: windows::core::GUID =
        windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    match (*format).wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE => {
            // cbSize covers the extensible tail; anything shorter is malformed,
            // and reading the subformat out of it would be reading past the end.
            if (*format).cbSize < 22 {
                return false;
            }
            // WAVEFORMATEXTENSIBLE is packed, so the subformat cannot be
            // borrowed - it has to be copied out unaligned.
            let extensible = format as *const WAVEFORMATEXTENSIBLE;
            let subformat = std::ptr::addr_of!((*extensible).SubFormat).read_unaligned();
            subformat == SUBTYPE_IEEE_FLOAT
        }
        _ => false,
    }
}

/// A running capture from one endpoint, handing out interleaved stereo samples.
pub struct AudioCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    /// Whether samples are floating point rather than integers. Bit width alone
    /// does not say: 24-bit integers are commonly carried in 32-bit containers,
    /// and reading those as floats yields noise rather than an error.
    float_samples: bool,
    /// Samples read from the device but not yet consumed by the encoder.
    pending: VecDeque<i16>,
}

impl AudioCapture {
    /// Opens a capture endpoint and checks it matches what the codec expects.
    pub fn open(device_id: &str, expected_rate: u32) -> Result<Self> {
        Self::open_internal(device_id, Some(expected_rate), true)
    }

    /// Opens an ordinary Windows microphone. Unlike the music capture path,
    /// microphones may be mono and may use a different shared sample rate; the
    /// monitoring reader below converts both without changing the LC3 format.
    pub fn open_microphone(device_id: &str) -> Result<Self> {
        Self::open_internal(device_id, None, false)
    }

    fn open_internal(
        device_id: &str,
        expected_rate: Option<u32>,
        require_stereo: bool,
    ) -> Result<Self> {
        ensure_com()?;

        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| AudioError::Com(e.to_string()))?;

            let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
            let device = enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .map_err(|e| AudioError::Com(e.to_string()))?;

            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| AudioError::Client(e.to_string()))?;

            let format = client
                .GetMixFormat()
                .map_err(|e| AudioError::Client(e.to_string()))?;

            let sample_rate = (*format).nSamplesPerSec;
            let channels = (*format).nChannels;
            let bits_per_sample = (*format).wBitsPerSample;
            let float_samples = format_is_float(format);

            if expected_rate.is_some_and(|expected| sample_rate != expected)
                || (require_stereo && channels != 2)
                || channels == 0
            {
                // GetMixFormat allocates with CoTaskMemAlloc and hands ownership to
                // us. Returning early without freeing leaks it, and this path runs
                // once per probed sample rate.
                CoTaskMemFree(Some(format as *const _));
                return Err(AudioError::FormatMismatch {
                    actual: sample_rate,
                    channels,
                    expected: expected_rate.unwrap_or(sample_rate),
                });
            }

            // 100 ms buffer: deep enough to survive a scheduling hiccup, shallow
            // enough that it does not add meaningfully to end-to-end latency.
            const BUFFER_DURATION_100NS: i64 = 100 * 10_000;

            let initialized = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                BUFFER_DURATION_100NS,
                0,
                format,
                None,
            );

            // Initialize either copied the format or rejected it. We own the
            // allocation in both cases, so free it before propagating an error.
            CoTaskMemFree(Some(format as *const _));
            initialized.map_err(|e| AudioError::Client(e.to_string()))?;

            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| AudioError::Client(e.to_string()))?;

            client.Start().map_err(|e| AudioError::Client(e.to_string()))?;

            Ok(Self {
                client,
                capture,
                sample_rate,
                channels,
                bits_per_sample,
                float_samples,
                pending: VecDeque::with_capacity(8192),
            })
        }
    }

    /// The format the device is actually handing us, in words.
    ///
    /// Printed at every stream start because it is the one thing in the chain
    /// nobody thinks to check: Windows will happily configure a virtual cable
    /// as a sixteen channel device, upmix stereo into it, and hand the result
    /// back downmixed. Everything downstream then carries that faithfully, and
    /// the damage looks like a codec or a radio problem.
    pub fn describe(&self) -> String {
        let sample = if self.float_samples {
            format!("{} bit float", self.bits_per_sample)
        } else {
            format!("{}-bit integer", self.bits_per_sample)
        };

        format!("{} Hz, {} channels, {sample}", self.sample_rate, self.channels)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Pulls whatever the device has ready into the pending buffer.
    fn drain_device(&mut self) -> Result<()> {
        unsafe {
            loop {
                let available = self
                    .capture
                    .GetNextPacketSize()
                    .map_err(|e| AudioError::Client(e.to_string()))?;

                if available == 0 {
                    break;
                }

                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;

                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| AudioError::Client(e.to_string()))?;

                let sample_count = frames as usize * self.channels as usize;

                // Windows sets this after an overrun, device reset or other gap.
                // Samples queued before the gap no longer form one continuous
                // timeline with this packet. Keeping them plays stale audio and
                // turns a brief scheduling hiccup into permanently higher latency.
                const DATA_DISCONTINUITY: u32 = 0x1;
                if flags & DATA_DISCONTINUITY != 0 {
                    self.pending.clear();
                }

                // AUDCLNT_BUFFERFLAGS_SILENT means the buffer contents are undefined
                // and should be treated as silence rather than read.
                const SILENT: u32 = 0x2;
                if flags & SILENT != 0 {
                    self.pending.extend(std::iter::repeat(0i16).take(sample_count));
                } else {
                    match (self.float_samples, self.bits_per_sample) {
                        (true, 32) => {
                            let samples = slice::from_raw_parts(data as *const f32, sample_count);
                            self.pending.extend(samples.iter().map(|&s| {
                                (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                            }));
                        }
                        (false, 16) => {
                            let samples = slice::from_raw_parts(data as *const i16, sample_count);
                            self.pending.extend(samples.iter().copied());
                        }
                        (false, 32) => {
                            // 32-bit integer samples; the top half is all the
                            // encoder needs.
                            let samples = slice::from_raw_parts(data as *const i32, sample_count);
                            self.pending
                                .extend(samples.iter().map(|&s| (s >> 16) as i16));
                        }
                        (false, 24) => {
                            // Packed three bytes per sample, little endian. Keeping
                            // the top two is the whole conversion to i16.
                            let bytes = slice::from_raw_parts(data, sample_count * 3);
                            self.pending.extend(
                                bytes
                                    .chunks_exact(3)
                                    .map(|s| i16::from_le_bytes([s[1], s[2]])),
                            );
                        }
                        (_, bits) => {
                            // Filling with silence here would produce a stream that
                            // runs perfectly and plays nothing, which is far harder
                            // to diagnose than a refusal.
                            self.capture
                                .ReleaseBuffer(frames)
                                .map_err(|e| AudioError::Client(e.to_string()))?;
                            return Err(AudioError::UnsupportedSampleFormat(bits));
                        }
                    }
                }

                self.capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| AudioError::Client(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Returns exactly one LC3 frame worth of samples per channel, or None if the
    /// device has not produced enough yet.
    ///
    /// Returning None rather than a short frame matters: LC3 encodes fixed-size
    /// blocks, and a partial one would shift every later frame out of alignment.
    pub fn next_frame(&mut self, samples_per_channel: usize) -> Result<Option<Vec<i16>>> {
        let needed = samples_per_channel * self.channels as usize;

        if self.pending.len() < needed {
            self.drain_device()?;
        }

        if self.pending.len() < needed {
            return Ok(None);
        }

        let frame: Vec<i16> = self.pending.drain(..needed).collect();
        Ok(Some(frame))
    }

    /// Returns one mono monitoring frame at `output_rate`. All input channels
    /// are averaged and the shared-mode microphone rate is converted without
    /// allowing old microphone audio to accumulate as delayed echo.
    pub fn next_mono_frame(
        &mut self,
        output_samples: usize,
        output_rate: u32,
    ) -> Result<Option<Vec<i16>>> {
        if output_samples == 0 || output_rate == 0 {
            return Ok(Some(Vec::new()));
        }
        let input_frames = ((output_samples as u64 * self.sample_rate as u64
            + output_rate as u64 - 1)
            / output_rate as u64)
            .max(1) as usize;
        let needed = input_frames * self.channels as usize;
        if self.pending.len() < needed {
            self.drain_device()?;
        }
        if self.pending.len() < needed {
            return Ok(None);
        }

        let interleaved: Vec<i16> = self.pending.drain(..needed).collect();
        let mono: Vec<i16> = interleaved
            .chunks_exact(self.channels as usize)
            .map(|frame| {
                let sum: i32 = frame.iter().map(|&sample| sample as i32).sum();
                (sum / self.channels as i32) as i16
            })
            .collect();
        Ok(Some(
            (0..output_samples)
                .map(|index| mono[index * mono.len() / output_samples])
                .collect(),
        ))
    }

    /// Level of each channel of an interleaved frame, in dBFS.
    ///
    /// Answers a question nothing else in the chain does: is the audio arriving
    /// here actually stereo? Every stage downstream faithfully carries whatever
    /// it is given, so if the two channels are identical the fault is upstream
    /// of Bluetooth entirely - in what Windows is feeding the virtual cable -
    /// and no amount of work on isochronous channels will separate them.
    ///
    /// Silence reports as `-inf`, which is the honest answer rather than a very
    /// negative number that looks like a measurement.
    pub fn channel_levels(frame: &[i16]) -> (f32, f32) {
        let (mut left, mut right) = (0.0f64, 0.0f64);
        let mut pairs = 0u32;

        for pair in frame.chunks_exact(2) {
            left += (pair[0] as f64).powi(2);
            right += (pair[1] as f64).powi(2);
            pairs += 1;
        }

        if pairs == 0 {
            return (f32::NEG_INFINITY, f32::NEG_INFINITY);
        }

        let decibels = |sum: f64| {
            let rms = (sum / pairs as f64).sqrt() / 32768.0;
            if rms <= 0.0 {
                f32::NEG_INFINITY
            } else {
                20.0 * rms.log10() as f32
            }
        };

        (decibels(left), decibels(right))
    }

    /// Energy at a single frequency, in dBFS.
    ///
    /// One Goertzel probe. Cheap enough to run on a frame once a second, which
    /// is what turns "the bass is missing" from a description into a number.
    pub fn tone_level(samples: &[i16], frequency: f32, rate: u32) -> f32 {
        if samples.is_empty() {
            return f32::NEG_INFINITY;
        }

        let k = frequency / rate as f32;
        let coefficient = 2.0 * (2.0 * std::f32::consts::PI * k).cos();

        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &sample in samples {
            let s0 = sample as f32 / 32768.0 + coefficient * s1 - s2;
            s2 = s1;
            s1 = s0;
        }

        let magnitude =
            (s1 * s1 + s2 * s2 - coefficient * s1 * s2).sqrt() / samples.len() as f32;

        if magnitude <= 0.0 {
            f32::NEG_INFINITY
        } else {
            20.0 * magnitude.log10()
        }
    }

    /// Energy in the bass, the middle and the top of an interleaved frame.
    ///
    /// Measured on the left channel of what the capture handed over, before the
    /// encoder touches it. If the bass is already missing here, no amount of
    /// work on the codec or the radio will bring it back - and if it is present
    /// here and absent in the headphones, everything between is suspect.
    pub fn band_levels(frame: &[i16], rate: u32) -> (f32, f32, f32) {
        let (left, _) = Self::deinterleave(frame);

        let band = |probes: &[f32]| -> f32 {
            let mut total = 0.0f32;
            for &frequency in probes {
                let level = Self::tone_level(&left, frequency, rate);
                if level.is_finite() {
                    total += 10f32.powf(level / 10.0);
                }
            }

            if total <= 0.0 {
                f32::NEG_INFINITY
            } else {
                10.0 * (total / probes.len() as f32).log10()
            }
        };

        (
            band(&[40.0, 64.0, 100.0, 160.0, 250.0]),
            band(&[500.0, 800.0, 1_250.0, 2_000.0, 3_150.0]),
            band(&[5_000.0, 8_000.0, 12_500.0, 16_000.0]),
        )
    }

    /// Splits interleaved stereo into per-channel buffers, which is what the
    /// encoder wants when each channel is coded separately.
    pub fn deinterleave(frame: &[i16]) -> (Vec<i16>, Vec<i16>) {
        let mut left = Vec::with_capacity(frame.len() / 2);
        let mut right = Vec::with_capacity(frame.len() / 2);

        for pair in frame.chunks_exact(2) {
            left.push(pair[0]);
            right.push(pair[1]);
        }

        (left, right)
    }

    /// How many samples are buffered but not yet encoded.
    ///
    /// Growth here means the encoder is falling behind the device, which shows up
    /// as latency creeping upward.
    pub fn backlog(&self) -> usize {
        self.pending.len()
    }

    /// Drops buffered audio, used after an underrun so latency does not accumulate.
    pub fn flush(&mut self) {
        self.pending.clear();
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Resolves a user-selected Windows capture endpoint by stable endpoint id or
/// friendly name. `default` chooses the first physical microphone, never a
/// virtual cable, so enabling monitoring cannot silently monitor the music bus.
pub fn find_capture_device(selection: &str) -> Result<AudioDevice> {
    let devices = list_capture_devices()?;
    if selection == "default" {
        return devices
            .into_iter()
            .find(|device| !device.is_virtual_cable())
            .ok_or_else(|| AudioError::NoCableDevice("default microphone".into()));
    }
    devices
        .into_iter()
        .find(|device| {
            device.id == selection
                || device.name.eq_ignore_ascii_case(selection)
                || device.name.to_lowercase().contains(&selection.to_lowercase())
        })
        .ok_or_else(|| AudioError::NoCableDevice(selection.to_owned()))
}

/// Shared-mode render endpoint used as the microphone's Windows-facing sink.
/// The headset provides mono PCM; the render endpoint normally expects stereo
/// float at 48 kHz, so conversion happens here without changing the LC3 stream.
pub struct AudioRender {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    float_samples: bool,
}

impl AudioRender {
    pub fn open(device_id: &str) -> Result<Self> {
        ensure_com()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|error| AudioError::Com(error.to_string()))?;
            let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
            let device = enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .map_err(|error| AudioError::Com(error.to_string()))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|error| AudioError::Client(error.to_string()))?;
            let format = client
                .GetMixFormat()
                .map_err(|error| AudioError::Client(error.to_string()))?;
            let sample_rate = (*format).nSamplesPerSec;
            let channels = (*format).nChannels;
            let bits_per_sample = (*format).wBitsPerSample;
            let float_samples = format_is_float(format);
            const BUFFER_DURATION_100NS: i64 = 100 * 10_000;
            let initialized = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                BUFFER_DURATION_100NS,
                0,
                format,
                None,
            );
            CoTaskMemFree(Some(format as *const _));
            initialized.map_err(|error| AudioError::Client(error.to_string()))?;
            let buffer_frames = client
                .GetBufferSize()
                .map_err(|error| AudioError::Client(error.to_string()))?;
            let render: IAudioRenderClient = client
                .GetService()
                .map_err(|error| AudioError::Client(error.to_string()))?;
            client.Start().map_err(|error| AudioError::Client(error.to_string()))?;
            Ok(Self {
                client,
                render,
                buffer_frames,
                sample_rate,
                channels,
                bits_per_sample,
                float_samples,
            })
        }
    }

    pub fn describe(&self) -> String {
        let kind = if self.float_samples { "float" } else { "integer" };
        format!(
            "{} Hz, {} channels, {}-bit {kind}",
            self.sample_rate, self.channels, self.bits_per_sample
        )
    }

    /// Writes one mono microphone frame, resampled to the endpoint's shared
    /// format and duplicated to every render channel.
    pub fn write_mono(&mut self, source: &[i16], source_rate: u32, gain: f32) -> Result<()> {
        if source.is_empty() || source_rate == 0 {
            return Ok(());
        }
        let output_frames = ((source.len() as u64 * self.sample_rate as u64) / source_rate as u64)
            .max(1) as usize;

        unsafe {
            let padding = self
                .client
                .GetCurrentPadding()
                .map_err(|error| AudioError::Client(error.to_string()))?;
            let available = self.buffer_frames.saturating_sub(padding) as usize;
            if available < output_frames {
                // The endpoint is temporarily behind. Dropping this 10 ms
                // frame keeps latency bounded; blocking would stall both music
                // delivery and HCI event handling.
                return Ok(());
            }
            let data = self
                .render
                .GetBuffer(output_frames as u32)
                .map_err(|error| AudioError::Client(error.to_string()))?;
            let samples = output_frames * self.channels as usize;

            let sample_at = |frame: usize| -> f32 {
                let source_index = frame * source.len() / output_frames;
                (source[source_index] as f32 * gain.clamp(0.0, 2.0))
                    .clamp(i16::MIN as f32, i16::MAX as f32)
            };

            match (self.float_samples, self.bits_per_sample) {
                (true, 32) => {
                    let out = slice::from_raw_parts_mut(data as *mut f32, samples);
                    for frame in 0..output_frames {
                        let value = sample_at(frame) / 32768.0;
                        for channel in 0..self.channels as usize {
                            out[frame * self.channels as usize + channel] = value;
                        }
                    }
                }
                (false, 16) => {
                    let out = slice::from_raw_parts_mut(data as *mut i16, samples);
                    for frame in 0..output_frames {
                        let value = sample_at(frame) as i16;
                        for channel in 0..self.channels as usize {
                            out[frame * self.channels as usize + channel] = value;
                        }
                    }
                }
                (false, 32) => {
                    let out = slice::from_raw_parts_mut(data as *mut i32, samples);
                    for frame in 0..output_frames {
                        let value = (sample_at(frame) as i32) << 16;
                        for channel in 0..self.channels as usize {
                            out[frame * self.channels as usize + channel] = value;
                        }
                    }
                }
                (false, 24) => {
                    let out = slice::from_raw_parts_mut(data, samples * 3);
                    for frame in 0..output_frames {
                        let value = (sample_at(frame) as i32) << 8;
                        let bytes = value.to_le_bytes();
                        for channel in 0..self.channels as usize {
                            let offset = (frame * self.channels as usize + channel) * 3;
                            out[offset..offset + 3].copy_from_slice(&bytes[..3]);
                        }
                    }
                }
                (_, bits) => {
                    self.render
                        .ReleaseBuffer(output_frames as u32, 0x2)
                        .map_err(|error| AudioError::Client(error.to_string()))?;
                    return Err(AudioError::UnsupportedSampleFormat(bits));
                }
            }

            self.render
                .ReleaseBuffer(output_frames as u32, 0)
                .map_err(|error| AudioError::Client(error.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for AudioRender {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cable_devices_are_recognised_by_name() {
        let cable = AudioDevice {
            name: "CABLE Output (VB-Audio Virtual Cable)".into(),
            id: String::new(),
        };
        assert!(cable.is_virtual_cable());

        let microphone = AudioDevice {
            name: "Microphone (Realtek High Definition Audio)".into(),
            id: String::new(),
        };
        assert!(!microphone.is_virtual_cable());
    }

    #[test]
    fn the_plain_cable_wins_over_a_multichannel_one() {
        let sixteen = AudioDevice {
            id: "a".into(),
            name: "CABLE Out 16ch (VB-Audio Virtual Cable)".into(),
        };
        let plain = AudioDevice {
            id: "b".into(),
            name: "CABLE Output (VB-Audio Virtual Cable)".into(),
        };

        assert!(sixteen.is_virtual_cable() && plain.is_virtual_cable());
        assert!(sixteen.is_multichannel_cable());
        assert!(!plain.is_multichannel_cable(), "the stereo cable is the right one");
    }

    #[test]
    fn channel_levels_tell_stereo_from_a_doubled_mono_signal() {
        // A real stereo frame: the right channel is quieter than the left.
        let stereo: Vec<i16> = (0..480)
            .flat_map(|i| {
                let value = ((i as f32 * 0.1).sin() * 20_000.0) as i16;
                [value, value / 4]
            })
            .collect();

        let (left, right) = AudioCapture::channel_levels(&stereo);
        assert!(left > right + 6.0, "left {left} should be well above right {right}");

        // Mono duplicated into both channels, which is what a broken capture
        // looks like and is indistinguishable from stereo everywhere else.
        let doubled: Vec<i16> = (0..480)
            .flat_map(|i| {
                let value = ((i as f32 * 0.1).sin() * 20_000.0) as i16;
                [value, value]
            })
            .collect();

        let (left, right) = AudioCapture::channel_levels(&doubled);
        assert!((left - right).abs() < 0.01, "identical channels must measure identical");
    }

    #[test]
    fn silence_reports_as_silence_rather_than_a_number() {
        let (left, right) = AudioCapture::channel_levels(&vec![0i16; 480]);

        assert_eq!(left, f32::NEG_INFINITY);
        assert_eq!(right, f32::NEG_INFINITY);
    }

    #[test]
    fn deinterleave_splits_the_stereo_pair() {
        // L R L R L R
        let frame = vec![1i16, -1, 2, -2, 3, -3];
        let (left, right) = AudioCapture::deinterleave(&frame);

        assert_eq!(left, vec![1, 2, 3]);
        assert_eq!(right, vec![-1, -2, -3]);
    }

    #[test]
    fn odd_length_frame_does_not_panic() {
        // A truncated buffer must drop the stray sample, not index past the end.
        let frame = vec![1i16, -1, 2];
        let (left, right) = AudioCapture::deinterleave(&frame);

        assert_eq!(left, vec![1]);
        assert_eq!(right, vec![-1]);
    }
}

/// The Windows volume slider for the render endpoint audio is routed through.
///
/// The headphones own the volume in LE Audio, so their buttons have to end up
/// here for the two to agree. Windows keeps this per endpoint, which for us is
/// the virtual cable that stands in for the headphones.
pub struct SystemVolume {
    endpoint: windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume,
}

impl SystemVolume {
    /// Opens the volume control of the current default render device.
    pub fn open_default_render() -> Result<Self> {
        use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
        use windows::Win32::Media::Audio::{eMultimedia, eRender};

        unsafe {
            // The session may already have COM started on this thread; that is
            // not an error and must not be treated as one.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| AudioError::Com(e.to_string()))?;

            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .map_err(|e| AudioError::Com(e.to_string()))?;

            let endpoint: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| AudioError::Com(e.to_string()))?;

            Ok(Self { endpoint })
        }
    }

    /// The current position of the slider, 0.0 to 1.0.
    pub fn level(&self) -> Result<f32> {
        unsafe {
            self.endpoint
                .GetMasterVolumeLevelScalar()
                .map_err(|e| AudioError::Com(e.to_string()))
        }
    }

    pub fn muted(&self) -> Result<bool> {
        unsafe {
            self.endpoint
                .GetMute()
                .map(|m| m.as_bool())
                .map_err(|e| AudioError::Com(e.to_string()))
        }
    }

    /// Moves the slider, as if the user had.
    pub fn set_level(&self, scalar: f32) -> Result<()> {
        unsafe {
            self.endpoint
                .SetMasterVolumeLevelScalar(scalar.clamp(0.0, 1.0), std::ptr::null())
                .map_err(|e| AudioError::Com(e.to_string()))
        }
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        unsafe {
            self.endpoint
                .SetMute(muted, std::ptr::null())
                .map_err(|e| AudioError::Com(e.to_string()))
        }
    }
}
