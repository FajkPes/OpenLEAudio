//! Records what the virtual cable actually delivers, so it can be judged by ear.
//!
//! Every measurement so far has been of our own numbers. This one leaves a file
//! behind: a WAV of exactly the samples the stack would have encoded, playable
//! on any speakers. If that file sounds like proper stereo with bass, the cable
//! is faithful and the fault is past it. If it sounds thin or mono, the problem
//! is in how Windows is feeding the cable and Bluetooth was never involved.
//!
//! The report beside it says the same thing in numbers, including the one that
//! settles it: the correlation between the two channels. Two channels carrying
//! the same audio correlate at 1.0 however loud each of them is, which is the
//! case level meters cannot tell apart from real stereo.

use std::io::Write;
use std::time::{Duration, Instant};

use olea_core::audio::{find_cable_device, AudioCapture};

const RATE: u32 = 48_000;
const SECONDS: u64 = 10;

fn main() {
    println!("OpenLEAudio - audio cable test");
    println!("{}", "=".repeat(66));
    println!("Play music or a left and right channel test now. Recording for {SECONDS} s.\n");

    let device = match find_cable_device(None) {
        Ok(device) => device,
        Err(e) => {
            eprintln!("cable could not be found: {e}");
            std::process::exit(1);
        }
    };

    let mut capture = match AudioCapture::open(&device.id, RATE) {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("cable could not be opened: {e}");
            eprintln!("check that it is configured for 2 channels at 48000 Hz");
            std::process::exit(1);
        }
    };

    println!("zdroj: {}", device.name);
    println!("format: {}\n", capture.describe());

    let mut samples: Vec<i16> = Vec::with_capacity(RATE as usize * 2 * SECONDS as usize);
    let deadline = Instant::now() + Duration::from_secs(SECONDS);

    while Instant::now() < deadline {
        match capture.next_frame(480) {
            Ok(Some(frame)) => samples.extend_from_slice(&frame),
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                eprintln!("capture failed: {e}");
                break;
            }
        }

        let done = samples.len() as f32 / (RATE as f32 * 2.0);
        print!("\r  {done:.1} s");
        let _ = std::io::stdout().flush();
    }
    println!("\n");

    if samples.is_empty() {
        eprintln!("no audio was received; is anything playing to 'CABLE Input'?");
        std::process::exit(1);
    }

    let report = analyse(&samples, &device.name, &capture.describe());
    println!("{report}");

    // Absolute, so nobody has to guess which directory the tool ran in.
    let here = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let wav_path = here.join("capture-test.wav");
    let wav = wav_path.as_path();
    match write_wav(wav, &samples) {
        Ok(()) => println!("recording: {}", wav.display()),
        Err(e) => eprintln!("WAV could not be written: {e}"),
    }

    let text_path = here.join("capture-test.txt");
    let text = text_path.as_path();
    match std::fs::write(text, &report) {
        Ok(()) => println!("report:   {}", text.display()),
        Err(e) => eprintln!("report could not be written: {e}"),
    }

    println!("\nPlay capture-test.wav through normal speakers.");
    println!("If stereo and bass are intact, the cable is working and the fault is downstream.");
}

/// Everything worth knowing about the recording, as text.
fn analyse(samples: &[i16], device: &str, format: &str) -> String {
    let (left, right) = AudioCapture::deinterleave(samples);
    let (left_db, right_db) = AudioCapture::channel_levels(samples);

    let correlation = correlation(&left, &right);
    // Silence first. "Nothing was playing" is a completely different answer
    // from "stereo", and calling two silent channels different closes a
    // question that is still wide open.
    let silent = !left_db.is_finite() && !right_db.is_finite();

    let verdict = if silent {
        "NOTHING WAS RECORDED - no audio played to 'CABLE Input' during the test"
    } else if correlation > 0.999 {
        "MONO - both channels carry the same signal"
    } else if correlation > 0.98 {
        "almost mono - channels differ only slightly"
    } else {
        "stereo - channels carry different signals"
    };

    let mut report = String::new();
    report.push_str("OpenLEAudio - audio cable test\n");
    report.push_str(&"=".repeat(66));
    report.push_str(&format!("\n\nsource:   {device}\nformat:   {format}\n"));
    report.push_str(&format!(
        "duration: {:.1} s\n\n",
        left.len() as f32 / RATE as f32
    ));

    report.push_str(&format!("left channel level:  {left_db:.1} dBFS\n"));
    report.push_str(&format!("right channel level: {right_db:.1} dBFS\n"));
    report.push_str(&format!("channel correlation:  {correlation:.4}\n"));
    report.push_str(&format!("result:               {verdict}\n\n"));

    report.push_str("band energy (left / right):\n");
    for (name, low, high) in [
        ("basy      20-250 Hz", 20.0, 250.0),
        ("mids    250-4000 Hz", 250.0, 4_000.0),
        ("treble  4000-16000 Hz", 4_000.0, 16_000.0),
    ] {
        report.push_str(&format!(
            "  {name}: {:>7.1} dB / {:>7.1} dB\n",
            band_energy(&left, low, high),
            band_energy(&right, low, high)
        ));
    }

    report.push_str(
        "\nHow to read this:\n\
         - correlation near 1.0 indicates mono regardless of individual levels\n\
         - bass far below the mids means low frequencies are already missing here\n\
         - if both checks pass, the cable is accurate and the fault is downstream\n",
    );

    report
}

/// How alike the two channels are, from -1 to 1.
///
/// The number level meters cannot give: two channels can differ in loudness
/// while carrying the same audio, and that reads as stereo on any meter.
fn correlation(left: &[i16], right: &[i16]) -> f32 {
    let n = left.len().min(right.len());
    if n == 0 {
        return 0.0;
    }

    let (mut sum_lr, mut sum_ll, mut sum_rr) = (0.0f64, 0.0f64, 0.0f64);
    for index in 0..n {
        let l = left[index] as f64;
        let r = right[index] as f64;
        sum_lr += l * r;
        sum_ll += l * l;
        sum_rr += r * r;
    }

    let denominator = (sum_ll * sum_rr).sqrt();
    if denominator <= 0.0 {
        return 0.0;
    }

    (sum_lr / denominator) as f32
}

/// Energy inside a frequency band, in dBFS.
///
/// A bank of Goertzel probes across the band rather than an FFT: it answers the
/// one question asked of it and fits in a page.
fn band_energy(samples: &[i16], low: f32, high: f32) -> f32 {
    const PROBES: usize = 12;

    let mut total = 0.0f32;
    for probe in 0..PROBES {
        // Logarithmic spacing, because hearing is.
        let ratio = probe as f32 / (PROBES - 1) as f32;
        let frequency = low * (high / low).powf(ratio);
        let magnitude = goertzel(samples, frequency);
        total += magnitude * magnitude;
    }

    let rms = (total / PROBES as f32).sqrt();
    if rms <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

fn goertzel(samples: &[i16], frequency: f32) -> f32 {
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

/// Writes a plain 16-bit stereo WAV, so any player can open it.
fn write_wav(path: &std::path::Path, samples: &[i16]) -> std::io::Result<()> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut file = std::fs::File::create(path)?;

    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // PCM header size
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&2u16.to_le_bytes())?; // stereo
    file.write_all(&RATE.to_le_bytes())?;
    file.write_all(&(RATE * 4).to_le_bytes())?; // bytes per second
    file.write_all(&4u16.to_le_bytes())?; // block align
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;

    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }

    Ok(())
}


