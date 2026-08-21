//! Does the LC3 path actually carry the whole audio band?
//!
//! The stream reaches the headphones and plays, but it sounds thin - no bass and
//! no air, as if something band-limited it. Everything on the radio side reports
//! success, so guessing is expensive: each attempt costs a connection, a listen
//! and a report back.
//!
//! This measures it instead, with no hardware involved. A signal with known
//! tones goes through our encoder and back through the matching decoder, and the
//! energy at each tone is compared before and after. If the low and high tones
//! come back missing, the codec path is the problem and the radio is innocent.

use lc3_codec::common::complex::Complex;
use lc3_codec::common::config::{FrameDuration, SamplingFrequency};
use lc3_codec::decoder::lc3_decoder::Lc3Decoder;
use olea_core::bap::{CodecConfiguration, FrameDuration as BapFrameDuration, SamplingFrequency as BapRate};
use olea_core::stream::AudioEncoder;

const RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 480; // 10 ms at 48 kHz
const SAMPLES_PER_7MS5: usize = 360; // 7.5 ms at 48 kHz, what Windows uses

/// Energy at one frequency, by the Goertzel algorithm.
///
/// A whole FFT would tell us more than we need. This answers exactly one
/// question - "how much of this tone is present" - and is short enough to read.
fn tone_energy(samples: &[i16], frequency: f32) -> f32 {
    let k = frequency / RATE as f32;
    let coefficient = 2.0 * (2.0 * std::f32::consts::PI * k).cos();

    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &sample in samples {
        let s0 = sample as f32 / 32768.0 + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }

    (s1 * s1 + s2 * s2 - coefficient * s1 * s2).sqrt() / samples.len() as f32
}

/// A signal with one tone in the bass, one in the middle and one up top.
fn probe_signal(frames: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(frames * SAMPLES_PER_FRAME);

    for n in 0..frames * SAMPLES_PER_FRAME {
        let t = n as f32 / RATE as f32;
        let value = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.25
            + (2.0 * std::f32::consts::PI * 1_000.0 * t).sin() * 0.25
            + (2.0 * std::f32::consts::PI * 10_000.0 * t).sin() * 0.25;

        samples.push((value * 26_000.0) as i16);
    }

    samples
}

fn configuration(octets: u16) -> CodecConfiguration {
    CodecConfiguration {
        sampling_frequency: BapRate::HZ_48000,
        frame_duration: BapFrameDuration::Ms10,
        channel_allocation: olea_core::bap::LOCATION_FRONT_LEFT,
        octets_per_frame: octets,
        frames_per_sdu: 1,
    }
}

/// Runs the probe signal through encode and decode, returning the output.
fn round_trip(octets: u16) -> Vec<i16> {
    const FRAMES: usize = 40; // 400 ms, long past any codec warm-up

    let mut encoder = AudioEncoder::with_channels(configuration(octets), 1);

    let (scaler_len, complex_len) = Lc3Decoder::calc_working_buffer_lengths(
        1,
        FrameDuration::TenMs,
        SamplingFrequency::Hz48000,
    );
    let mut scaler_buf = vec![0.0; scaler_len];
    let mut complex_buf = vec![Complex::default(); complex_len];
    let mut decoder = Lc3Decoder::new(
        1,
        FrameDuration::TenMs,
        SamplingFrequency::Hz48000,
        &mut scaler_buf,
        &mut complex_buf,
    );

    let input = probe_signal(FRAMES);
    let mut output = Vec::with_capacity(input.len());

    for frame in input.chunks_exact(SAMPLES_PER_FRAME) {
        let encoded = encoder.encode_channel(0, frame).expect("encode failed");

        let mut decoded = [0i16; SAMPLES_PER_FRAME];
        decoder
            .decode_frame(16, 0, &encoded, &mut decoded)
            .expect("decode failed");

        output.extend_from_slice(&decoded);
    }

    // Drop the first few frames: the codec has an inherent delay and the very
    // start is genuinely quiet, which would look like a missing tone.
    output.split_off(SAMPLES_PER_FRAME * 4)
}

/// Prints what survived, so a failure says which end of the band was lost.
fn report(label: &str, input: &[i16], output: &[i16]) -> Vec<(f32, f32)> {
    let mut ratios = Vec::new();

    println!("\n{label}");
    for frequency in [60.0f32, 1_000.0, 10_000.0] {
        let before = tone_energy(input, frequency);
        let after = tone_energy(output, frequency);
        let ratio = if before > 0.0 { after / before } else { 0.0 };

        println!(
            "  {frequency:>8.0} Hz: {:.4} -> {:.4}  ({:.0} % retained)",
            before,
            after,
            ratio * 100.0
        );

        ratios.push((frequency, ratio));
    }

    ratios
}

#[test]
fn the_codec_carries_bass_middle_and_treble() {
    let input = probe_signal(40);
    let input = &input[SAMPLES_PER_FRAME * 4..];

    let output = round_trip(100);
    let ratios = report("48 kHz, 10 ms, 100 octets (default preset)", input, &output);

    for (frequency, ratio) in ratios {
        assert!(
            ratio > 0.5,
            "at {frequency} Hz only {:.0} % of the tone survived the codec - \
             this is the band limiting we hear",
            ratio * 100.0
        );
    }
}

#[test]
fn more_octets_do_not_lose_the_band() {
    let input = probe_signal(40);
    let input = &input[SAMPLES_PER_FRAME * 4..];

    let output = round_trip(155);
    let ratios = report("48 kHz, 10 ms, 155 octets (high quality)", input, &output);

    for (frequency, ratio) in ratios {
        assert!(
            ratio > 0.5,
            "at {frequency} Hz only {:.0} % survived even at the LC3 ceiling",
            ratio * 100.0
        );
    }
}

/// Encodes two different signals on the two channels of one encoder and decodes
/// both, which is exactly what the dual-CIS path does and what nothing tested.
///
/// The encoder is created once with two channels and shares its working buffers
/// between them. If those buffers overlap, or if channel one's state is not
/// really separate, the second channel comes out wrong - and the symptom on the
/// hardware is the right earpiece playing nothing while the left plays
/// everything, which looks exactly like a routing problem on the radio.
#[test]
fn both_channels_of_a_stereo_encoder_carry_their_own_audio() {
    const FRAMES: usize = 20;
    const OCTETS: u16 = 90;

    let mut encoder = AudioEncoder::with_channels(
        CodecConfiguration { frame_duration: BapFrameDuration::Ms7_5, ..configuration(OCTETS) },
        2,
    );

    let (scaler_len, complex_len) = Lc3Decoder::calc_working_buffer_lengths(
        2,
        FrameDuration::SevenPointFiveMs,
        SamplingFrequency::Hz48000,
    );
    let mut scaler_buf = vec![0.0; scaler_len];
    let mut complex_buf = vec![Complex::default(); complex_len];
    let mut decoder = Lc3Decoder::new(
        2,
        FrameDuration::SevenPointFiveMs,
        SamplingFrequency::Hz48000,
        &mut scaler_buf,
        &mut complex_buf,
    );

    // Two tones far apart, so a channel carrying the wrong one is unmistakable.
    let tone = |hz: f32, n: usize| {
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                ((2.0 * std::f32::consts::PI * hz * t).sin() * 24_000.0) as i16
            })
            .collect::<Vec<i16>>()
    };

    let samples = SAMPLES_PER_7MS5 * FRAMES;
    let left_in = tone(500.0, samples);
    let right_in = tone(4_000.0, samples);

    let mut left_out = Vec::new();
    let mut right_out = Vec::new();

    for frame in 0..FRAMES {
        let range = frame * SAMPLES_PER_7MS5..(frame + 1) * SAMPLES_PER_7MS5;

        for (channel, input, output) in [
            (0usize, &left_in, &mut left_out),
            (1usize, &right_in, &mut right_out),
        ] {
            let encoded = encoder
                .encode_channel(channel, &input[range.clone()])
                .expect("encode failed");

            let mut decoded = [0i16; SAMPLES_PER_7MS5];
            decoder
                .decode_frame(16, channel, &encoded, &mut decoded)
                .expect("decode failed");

            output.extend_from_slice(&decoded);
        }
    }

    // Skip the codec's start-up delay before measuring.
    let settled = SAMPLES_PER_7MS5 * 4;
    let left_out = &left_out[settled..];
    let right_out = &right_out[settled..];

    let left_500 = tone_energy(left_out, 500.0);
    let left_4k = tone_energy(left_out, 4_000.0);
    let right_500 = tone_energy(right_out, 500.0);
    let right_4k = tone_energy(right_out, 4_000.0);

    println!("
channel 0: 500 Hz {left_500:.4}, 4 kHz {left_4k:.4}");
    println!("channel 1: 500 Hz {right_500:.4}, 4 kHz {right_4k:.4}");

    assert!(
        left_500 > left_4k * 4.0,
        "channel 0 should carry its own 500 Hz tone, not the other channel's"
    );
    assert!(
        right_4k > right_500 * 4.0,
        "channel 1 came back carrying the wrong audio - the right earpiece          would play nothing recognisable"
    );
}


/// The band test again, at the frame length actually shipping.
///
/// The earlier measurement used 10 ms frames because that was the preset at the
/// time. The Windows trace then moved the default to 7.5 ms, and a codec is
/// perfectly entitled to behave differently at a different frame length - so
/// "LC3 is innocent" had to be re-established, not assumed.
#[test]
fn the_shipping_configuration_carries_bass_middle_and_treble() {
    const FRAMES: usize = 40;
    const OCTETS: u16 = 90;

    let mut encoder = AudioEncoder::with_channels(
        CodecConfiguration { frame_duration: BapFrameDuration::Ms7_5, ..configuration(OCTETS) },
        1,
    );

    let (scaler_len, complex_len) = Lc3Decoder::calc_working_buffer_lengths(
        1,
        FrameDuration::SevenPointFiveMs,
        SamplingFrequency::Hz48000,
    );
    let mut scaler_buf = vec![0.0; scaler_len];
    let mut complex_buf = vec![Complex::default(); complex_len];
    let mut decoder = Lc3Decoder::new(
        1,
        FrameDuration::SevenPointFiveMs,
        SamplingFrequency::Hz48000,
        &mut scaler_buf,
        &mut complex_buf,
    );

    let samples = SAMPLES_PER_7MS5 * FRAMES;
    let mut input = Vec::with_capacity(samples);
    for n in 0..samples {
        let t = n as f32 / RATE as f32;
        let value = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.25
            + (2.0 * std::f32::consts::PI * 1_000.0 * t).sin() * 0.25
            + (2.0 * std::f32::consts::PI * 10_000.0 * t).sin() * 0.25;
        input.push((value * 26_000.0) as i16);
    }

    let mut output = Vec::with_capacity(samples);
    for frame in input.chunks_exact(SAMPLES_PER_7MS5) {
        let encoded = encoder.encode_channel(0, frame).expect("encode failed");

        let mut decoded = [0i16; SAMPLES_PER_7MS5];
        decoder
            .decode_frame(16, 0, &encoded, &mut decoded)
            .expect("decode failed");

        output.extend_from_slice(&decoded);
    }

    let settled = SAMPLES_PER_7MS5 * 4;
    let ratios = report(
        "48 kHz, 7.5 ms, 90 octets (headphone stream configuration)",
        &input[settled..],
        &output[settled..],
    );

    for (frequency, ratio) in ratios {
        assert!(
            ratio > 0.5,
            "at {frequency} Hz only {:.0} % survived at the shipping configuration",
            ratio * 100.0
        );
    }
}
