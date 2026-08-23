//! Drives a full session: adapter, scan, connect, pair, capabilities, stream.
//!
//! Safe by default. Without `--stream` the tool connects, reads what the
//! headphones can do and stops - it writes nothing to their configuration.
//!
//! Every LC3 parameter is reachable from the command line, not just the presets:
//! `--rate`, `--frame` and `--octets` set the configuration directly, and are
//! still validated against what the device itself published.

use std::time::Duration;

use olea_core::bap::{CodecConfiguration, FrameDuration, Preset, SamplingFrequency, LOCATION_STEREO};
use olea_core::link::pacs_uuid;
use olea_core::safety::OutputLimiter;
use olea_core::session::{
    best_match, describe_capabilities, Progress, ReconnectPolicy, Session, SessionConfig,
};
use olea_core::stream::{can_use_single_cis, describe_limits, StreamPlan};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let stream = args.iter().any(|a| a == "--stream");
    let wait = args.iter().any(|a| a == "--wait");

    // Turned on before anything opens the adapter, so the very first command is
    // in the log too.
    if args.iter().any(|a| a == "--debug") {
        olea_core::trace::enable();
    }
    let dual_cis = args.iter().any(|a| a == "--dual-cis");
    let preset = parse_preset(&args);
    let name_hint = value_of(&args, "--device");
    let gain = value_of(&args, "--gain").and_then(|g| g.parse::<f32>().ok());

    // Both are minutes on the command line, because that is the unit anyone
    // reasoning about "how long before it goes quiet" actually thinks in.
    let idle_timeout = match value_of(&args, "--idle-timeout") {
        Some(minutes) => minutes
            .parse::<f32>()
            .ok()
            .filter(|m| *m > 0.0)
            .map(|m| Duration::from_secs_f32(m * 60.0)),
        None => SessionConfig::default().idle_timeout,
    };

    let reconnect = if args.iter().any(|a| a == "--no-reconnect") {
        ReconnectPolicy::disabled()
    } else {
        ReconnectPolicy {
            window: value_of(&args, "--reconnect-for")
                .and_then(|m| m.parse::<f32>().ok())
                .map(|m| Duration::from_secs_f32(m * 60.0))
                .or(ReconnectPolicy::default().window),
            ..ReconnectPolicy::default()
        }
    };

    let config = SessionConfig {
        preset,
        command_style: olea_core::transport::CommandStyle::ClassDevice,
        prefer_single_cis: !dual_cis,
        audio_device: None,
        microphone_target: None,
        microphone_gain: 1.0,
        monitor_source: None,
        monitor_replace: false,
        monitor_gain: 1.0,
        live_audio: std::sync::Arc::new(std::sync::RwLock::new(
            olea_core::session::LiveAudioConfig {
                output_gain: gain.unwrap_or(1.0),
                ..Default::default()
            },
        )),
        limiter: gain.map(OutputLimiter::with_gain).unwrap_or_default(),
        scan_duration: Duration::from_secs(10),
        idle_timeout,
        reconnect,
        // A command line tool that prints what it measures wants everything.
        metrics: olea_core::session::MetricsLevel::Full,
        // A one-shot playback tool has nothing to hand the headphones back to,
        // and stopping halfway through a quiet passage would look like a fault.
        multipoint_yield: None,
        swap_channels: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            args.iter().any(|a| a == "--swap-channels"),
        )),
        link_timeout: Duration::from_secs(10),
        battery_refresh: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        battery_poll: None,
        idle_link_latency: 0,
    };

    println!("OpenLEAudio");
    println!("{}", "=".repeat(66));
    println!("  preset    : {}", preset.label());
    println!("  topology : {}", if dual_cis { "two CIS links" } else { "one CIS when supported" });
    println!("  hlasitost : {:.0} %", config.limiter.gain() * 100.0);
    println!(
        "  ticho     : {}",
        match config.idle_timeout {
            Some(t) => format!("vysilani pozastavit po {:.0} min", t.as_secs_f32() / 60.0),
            None => "vysilat porad".into(),
        }
    );
    println!(
        "  reconnect : {}",
        match (config.reconnect.enabled, config.reconnect.window) {
            (false, _) => "vypnuty".into(),
            (true, Some(w)) => format!(
                "kazdych {} s po dobu {:.0} min",
                config.reconnect.interval.as_secs(),
                w.as_secs_f32() / 60.0
            ),
            (true, None) => format!("kazdych {} s, bez omezeni", config.reconnect.interval.as_secs()),
        }
    );
    println!(
        "  rezim     : {}",
        if stream { "STREAM (writes configuration to headphones)" } else { "read-only" }
    );

    if let Err(e) = run(config, name_hint.as_deref(), stream, wait, &args) {
        eprintln!("\n  SELHALO: {e}");
        eprintln!("\n  Restore the adapter with: RESTORE Windows Bluetooth driver.bat");
        std::process::exit(1);
    }
}

fn run(
    config: SessionConfig,
    name_hint: Option<&str>,
    stream: bool,
    wait: bool,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new(config.clone());

    step(1, "adapter");
    session.open_adapter(report)?;

    step(2, "skenovani");
    let devices = session.scan(report)?;
    let device = best_match(&devices, name_hint).ok_or("zadne zarizeni")?.clone();

    // Worth saying out loud: a device that never announced LE Audio may still be
    // the right one, but it may equally be a mouse. Naming the ambiguity beats
    // failing three steps later with something unrelated.
    if !device.is_le_audio() {
        println!("
  POZOR: vybrane zarizeni neinzeruje LE Audio sluzbu.");
        println!("  It may expose the service only after connection, or this may not be an audio device.");
        println!("  Konkretni zarizeni vybere prepinac --device <jmeno>.");
    }

    println!(
        "\n  vybrano: {} ({})",
        device.name.as_deref().unwrap_or("(bez jmena)"),
        device.address
    );

    // These headphones are known to behave differently depending on what is
    // already happening when the connection is made - audio that is already
    // flowing appears to matter. Scanning is done by this point, so pausing
    // here costs nothing and puts the moment of connection under manual
    // control instead of leaving it to whenever the scan happens to finish.
    if wait {
        println!("
{}", "-".repeat(66));
        println!("  Pripraveno k pripojeni: {}", device.address);
        println!("  Prepare the device now, for example by starting audio.");
        print!("  Press Enter when you are ready to connect: ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    step(3, "pripojovani");

    // A peripheral advertises in bursts, so a single attempt can simply miss the
    // window - especially after a pause. Retrying costs a second and turns a
    // dead run into a working one.
    const ATTEMPTS: u32 = 3;
    let mut handle = None;
    for attempt in 1..=ATTEMPTS {
        match session.connect(&device, report) {
            Ok(value) => {
                handle = Some(value);
                break;
            }
            Err(e) if attempt < ATTEMPTS => {
                println!("  attempt {attempt}/{ATTEMPTS} failed: {e}");
                println!("  zkousim znovu...");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
    let handle = handle.ok_or("pripojeni se nepodarilo")?;

    let mut link = session.open_link(handle)?;

    step(4, "parovani a sifrovani");
    match session.pair(&mut link, handle, &device) {
        Ok(_) => println!("  spojeni je sifrovane"),
        Err(e) => {
            // Some devices allow reading PACS without pairing. Worth trying
            // rather than giving up, and the failure is reported either way.
            println!("  parovani selhalo: {e}");
            println!("  trying capability discovery anyway; some devices allow it");
        }
    }

    step(5, "headphone capabilities");
    let capabilities = session.read_capabilities(&mut link, report)?;

    // The volume buttons on the earcups only reach Windows if we subscribe here.
    // A device without a Volume Control Service is not a problem, just quieter
    // to report.
    match session.attach_volume_control(&mut link) {
        Ok(Some(summary)) => println!("  {summary}"),
        Ok(None) => println!("  headphones do not expose the Volume Control Service"),
        Err(e) => println!("  hlasitost se nepodarilo napojit: {e}"),
    }

    print_capabilities(&capabilities);

    step(6, "plan streamu");
    let plan = match custom_codec(args) {
        Some(codec) => {
            println!("  vlastni konfigurace ze prikazove radky");
            build_custom_plan(&capabilities, codec, !args.iter().any(|a| a == "--dual-cis"))?
        }
        None => session.plan_stream(&capabilities, report)?,
    };

    println!("\n  {}", plan.describe());
    println!("  odhad latence linku: {} ms", plan.latency_ms());
    println!("  SDU: {} B kazdych {} us", plan.sdu_size(), plan.qos.sdu_interval_us);

    if !stream {
        println!("\n{}", "-".repeat(66));
        println!("  Done. Nothing was written to the headphones.");
        println!("  Run again with --stream to start playback.");
        return Ok(());
    }

    step(7, "konfigurace streamu");
    let control_point = find_control_point(&mut link)?;
    println!("  ASE control point: handle {control_point:#06x}");

    session.configure_stream(&mut link, control_point, &plan)?;
    println!("  ASCS nakonfigurovano");

    let outcome = session.establish_isochronous(&plan, handle)?;
    println!("  {}", outcome.describe());
    let cis_handles = outcome.established;
    println!("  CIS handles: {cis_handles:?}");

    step(8, "audio");
    println!("  Nastav ve Windows vystup na 'CABLE Input' a pust zvuk.");
    println!("  Press Ctrl+C to stop.\n");

    session.run_audio(&plan, &cis_handles, Some(handle), report, || false)?;

    Ok(())
}

/// Locates the ASE control point, which is the only handle we ever write to.
fn find_control_point(link: &mut olea_core::Link) -> Result<u16, Box<dyn std::error::Error>> {
    let services = link.discover_services()?;

    let ascs = services
        .iter()
        .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_ASCS))
        .ok_or("zarizeni nema ASCS sluzbu")?
        .clone();

    let characteristic = link
        .discover_characteristics(&ascs)?
        .into_iter()
        .find(|c| c.uuid.as_short() == Some(pacs_uuid::ASE_CONTROL_POINT))
        .ok_or("ASCS nema control point")?;

    Ok(characteristic.value_handle)
}

fn print_capabilities(capabilities: &olea_core::AudioCapabilities) {
    println!("\n  {}", describe_capabilities(capabilities));

    for (index, record) in capabilities.sink_records.iter().enumerate() {
        if !record.is_lc3() {
            continue;
        }
        println!("\n  Sink PAC {}: {}", index + 1, describe_limits(&record.capabilities));

        let caps = &record.capabilities;
        if let (Some(min), Some(max)) = (caps.min_octets_per_frame, caps.max_octets_per_frame) {
            for frequency in &caps.sampling_frequencies {
                let Some(hz) = frequency.hz() else { continue };

                for (label, duration) in [("7.5 ms", 7500u32), ("10 ms", 10000u32)] {
                    let supported = if duration == 7500 { caps.supports_7_5ms } else { caps.supports_10ms };
                    if !supported {
                        continue;
                    }

                    let low = min as u32 * 8 * 1_000_000 / duration / 1000;
                    let high = max as u32 * 8 * 1_000_000 / duration / 1000;
                    println!(
                        "    {:>6} Hz, {:>6}: {}-{} B/ramec = {}-{} kbps/kanal",
                        hz, label, min, max, low, high
                    );
                }
            }
        }
    }

    println!("\n  one CIS for stereo: {}", if can_use_single_cis(capabilities) { "yes" } else { "no" });
}

/// Builds a configuration straight from command-line values.
fn custom_codec(args: &[String]) -> Option<CodecConfiguration> {
    let rate = value_of(args, "--rate")?.parse::<u32>().ok()?;
    let octets = value_of(args, "--octets")?.parse::<u16>().ok()?;

    let frame = match value_of(args, "--frame").as_deref() {
        Some("7.5") => FrameDuration::Ms7_5,
        _ => FrameDuration::Ms10,
    };

    let frequency = match rate {
        8_000 => SamplingFrequency::HZ_8000,
        16_000 => SamplingFrequency::HZ_16000,
        24_000 => SamplingFrequency::HZ_24000,
        32_000 => SamplingFrequency::HZ_32000,
        44_100 => SamplingFrequency::HZ_44100,
        48_000 => SamplingFrequency::HZ_48000,
        _ => return None,
    };

    Some(CodecConfiguration {
        sampling_frequency: frequency,
        frame_duration: frame,
        channel_allocation: LOCATION_STEREO,
        octets_per_frame: octets,
        frames_per_sdu: 2,
    })
}

/// Validates a hand-written configuration against the device before using it.
fn build_custom_plan(
    capabilities: &olea_core::AudioCapabilities,
    codec: CodecConfiguration,
    single_cis: bool,
) -> Result<StreamPlan, Box<dyn std::error::Error>> {
    let sink = capabilities
        .sink_records
        .iter()
        .find(|r| r.is_lc3())
        .ok_or("zarizeni nema LC3 sink")?;

    // Refuse anything the device did not advertise, rather than sending it and
    // hoping. This is the check Windows never lets you see.
    sink.capabilities.accepts(&codec)?;

    let mut plan = StreamPlan::build(capabilities, Preset::WindowsDefault, single_cis)?;
    plan.qos.max_sdu = codec.sdu_size();
    plan.qos.sdu_interval_us = codec.frame_duration.microseconds();
    plan.codec = codec;

    Ok(plan)
}

fn step(index: u8, what: &str) {
    println!("\n[{index}/8] {what}");
    println!("{}", "-".repeat(66));
}

fn report(progress: Progress) {
    match progress {
        Progress::AdapterReady { version, address } => {
            println!("  {version}, adresa {address}")
        }
        Progress::DeviceFound { name, address, rssi, le_audio } => {
            let mark = if le_audio { "LE Audio" } else { "        " };
            println!("  {mark}  {name:<28} {address}  {rssi} dBm")
        }
        Progress::Connected { handle } => println!("  pripojeno, handle {handle:#06x}"),
        Progress::CapabilitiesRead { .. } => {}
        Progress::StreamPlanned { .. } => {}
        Progress::Streaming { frames, backlog: _, iso_sent, iso_failed, left_db, right_db, bass_db, mid_db, treble_db, rssi, delivered, .. } => {
            let health = if iso_failed == 0 {
                "vse odeslano".to_string()
            } else {
                format!("SELHALO {iso_failed} z {iso_sent} prenosu")
            };
            let show = |db: f32| -> String { if db.is_finite() { format!("{db:.0} dB") } else { "ticho".into() } };
            println!(
                "  hraje: {frames} ramcu, L {} / R {}, basy {} / stredy {} / vysky {}, {health}",
                show(left_db),
                show(right_db),
                show(bass_db),
                show(mid_db),
                show(treble_db)
            );
            println!("    doruceno po kanalech: {delivered:?}")
            ;
            if let Some(rssi) = rssi { println!("    signal: {rssi} dBm") }
        }
        Progress::CaptureReady { device, format } => {
            println!("  zdroj zvuku: {device} - {format}")
        }
        Progress::Idle { after } => println!(
            "  silent for {} s; transmission paused and headphones remain connected",
            after.as_secs()
        ),
        Progress::Resumed => println!("  zvuk se vratil"),
        Progress::Disconnected { reason } => println!(
            "  spojeni se ztratilo: {} (kod {reason:#04x})",
            olea_core::hci::disconnect_reason(reason)
        ),
        Progress::Stopped { reason } => println!("  zastaveno: {reason}"),
        Progress::Yielded { after } => println!(
            "  ticho {} s: sluchatka uvolnena pro jine zarizeni",
            after.as_secs()
        ),
        Progress::BatteryAsked { reason } => {
            println!("  baterie: dotaz odeslan ({reason})");
            return;
        }
        Progress::Battery { levels } => println!(
            "  baterie: {}",
            levels
                .iter()
                .map(|percent| format!("{percent} %"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn parse_preset(args: &[String]) -> Preset {
    match value_of(args, "--preset").as_deref() {
        Some("low-latency") => Preset::LowLatency,
        Some("high-quality") => Preset::HighQuality,
        Some("robust") => Preset::Robust,
        _ => Preset::WindowsDefault,
    }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_help() {
    println!(
        "OpenLEAudio - konfigurovatelny LE Audio stack

POUZITI:
  olea-connect [volby]

WITHOUT --stream the tool connects, reads headphone capabilities, and exits.
Nothing is written to the headphone configuration.

ZAKLADNI:
  --stream              start actual playback
  --wait                wait for Enter after scanning before connecting
  --debug               vypsat kazdy HCI a ACL paket obema smery
  --device <name>       select headphones by name
  --gain <0.0-1.0>      hlasitost, vychozi 0.1 kvuli bezpecnosti sluchu
  --dual-cis            vynutit two CIS links misto jednoho

PRESETY:
  --preset windows-default   48 kHz, 10 ms, 100 B  = 80 kbps/kanal
  --preset low-latency       48 kHz, 7.5 ms, 75 B  = 80 kbps/kanal
  --preset high-quality      48 kHz, 10 ms, 155 B  = 124 kbps/kanal
  --preset robust            24 kHz, 10 ms, 60 B   = 48 kbps/kanal

VLASTNI KONFIGURACE LC3:
  --rate <Hz>           8000 | 16000 | 24000 | 32000 | 44100 | 48000
  --frame <ms>          7.5 | 10
  --octets <B>          octets per frame, which directly controls bitrate

  Bitrate = octets * 8 / delka ramce.
  Priklad: 48 kHz, 10 ms, 155 B = 124 kbps na kanal (strop LC3)

  Every value is validated against the capabilities declared by the headphones.
  Nepodporovanou konfiguraci tool odmitne a rekne proc."
    );
}

