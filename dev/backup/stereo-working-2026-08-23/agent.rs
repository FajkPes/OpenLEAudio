//! The process the configuration app talks to.
//!
//! The app is WinUI 3 and cannot drive WinUSB isochronous pipes; this stack is
//! Rust and has no business drawing a window. So they are two processes with a
//! pipe between them, speaking one JSON object per line in each direction.
//!
//! Why a pipe rather than a DLL the app calls into: the session is deliberately
//! single threaded - the HCI pump, the link and the encoder all share one
//! `Rc`-based world and cannot be touched from a UI thread. A pipe makes that
//! constraint structural instead of something the app has to remember. It also
//! means a crash in the radio stack closes a pipe rather than taking the window
//! with it.
//!
//! Commands arrive on stdin, events leave on stdout, and anything the stack
//! wants to say to a human goes to stderr so it never corrupts the protocol.

use std::io::{BufRead, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use olea_core::bonding::{Bond, BondStore};
use olea_core::bap::Preset;
use olea_core::safety::OutputLimiter;
use olea_core::controller::DiscoveredDevice;
use olea_core::session::{
    describe_capabilities, LiveAudioConfig, Progress, ReconnectPolicy, Session, SessionConfig,
};
use olea_core::settings::Settings;
use olea_core::stream::{MicrophoneQuality, StreamPlan};
use serde_json::{json, Value};

fn main() {
    let (commands_tx, commands_rx) = channel::<Value>();

    // Playback blocks the worker for as long as it runs, so anything that has to
    // interrupt it cannot go through the same queue - it would be read only once
    // the thing it is meant to stop has already finished. This flag is shared
    // with the worker and cleared here, on the thread that is always listening.
    let playing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Flipped from this thread while audio is running, so the effect is heard
    // the moment the switch moves. Going through the command queue would mean
    // waiting for playback to end - which is exactly the thing being judged.
    let swap_channels = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live_audio = std::sync::Arc::new(std::sync::RwLock::new(LiveAudioConfig::default()));

    // The session owns `Rc`s and must never leave the thread that built it.
    let worker = {
        let playing = playing.clone();
        let swap_channels = swap_channels.clone();
        let live_audio = live_audio.clone();
        std::thread::spawn(move || run_worker(commands_rx, playing, swap_channels, live_audio))
    };

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(line) {
            Ok(mut command) => {
                let name = command
                    .get("cmd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // Anything that has to interrupt playback is noticed here, on
                // the thread that is always listening. Disconnect belongs in
                // this list: while audio runs the worker is blocked, so a
                // queued disconnect would only be read once the thing it is
                // meant to end had ended on its own.
                // Playback deliberately occupies the radio worker. Settings
                // must not sit behind that hours-long operation: persist and
                // acknowledge them on this always-listening thread, then leave
                // a lightweight reload command for the worker. This is also
                // what makes a named LC3 preset immediately update every value
                // shown in the UI while the current stream keeps playing.
                if name == "set" {
                    let key = command.get("key").and_then(Value::as_str).unwrap_or("");
                    let value = command.get("value").and_then(Value::as_str).unwrap_or("");
                    let mut saved = Settings::load(&settings_path());
                    match apply_setting_change(&mut saved, key, value)
                        .and_then(|needs| {
                            saved.save(&settings_path())
                                .map_err(|e| format!("save failed: {e}"))?;
                            Ok(needs)
                        })
                    {
                        Ok(needs) => {
                            sync_live_from_settings(&live_audio, &saved);
                            swap_channels.store(
                                saved.bool("swap_channels").unwrap_or(false),
                                std::sync::atomic::Ordering::Relaxed,
                            );
                        emit(json!({
                            "event": "applied",
                            "key": key,
                            "value": value,
                                "needs": needs,
                        }));
                            command["prePersisted"] = json!(true);
                        }
                        Err(text) => {
                            emit(json!({ "event": "error", "cmd": "set", "text": text }));
                            emit(json!({ "event": "done", "cmd": "set" }));
                            continue;
                        }
                    }
                }

                if name == "settings" {
                    let saved = Settings::load(&settings_path());
                    emit_settings_snapshot(&saved);
                    emit(json!({ "event": "done", "cmd": "settings" }));
                    continue;
                }

                if name == "debug" {
                    if command.get("on").and_then(Value::as_bool).unwrap_or(false) {
                        olea_core::trace::enable();
                    } else {
                        olea_core::trace::disable();
                    }
                }

                if matches!(name.as_str(), "stop" | "quit" | "disconnect")
                    || (name == "adapter"
                        && command.get("on").and_then(Value::as_bool) == Some(false))
                {
                    playing.store(false, std::sync::atomic::Ordering::Relaxed);
                }

                if commands_tx.send(command).is_err() {
                    break;
                }
            }
            Err(e) => emit(json!({ "event": "error", "text": format!("unreadable command: {e}") })),
        }
    }

    // Standard input closing means the app is gone. Stopping playback first is
    // not tidiness: the worker spends playback blocked inside `run_audio`, so
    // joining without this waits forever, and the agent keeps running as an
    // orphan - still holding the adapter. The next launch then finds the device
    // taken and reports it as "still owned by another driver", which sends the
    // investigation to the driver binding instead of to this process.
    playing.store(false, std::sync::atomic::Ordering::Relaxed);
    drop(commands_tx);
    let _ = worker.join();
}

/// Writes one event. Flushed immediately: the app is waiting on this line.
fn emit(value: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn log(text: impl Into<String>) {
    emit(json!({ "event": "log", "text": text.into() }));
}

fn useful_device_name(name: &str, address: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && !trimmed.eq_ignore_ascii_case(address)
        && !trimmed.eq_ignore_ascii_case("(unnamed)")
        && !trimmed.eq_ignore_ascii_case("(bez jmena)")
}

fn sync_live_from_settings(
    live_audio: &std::sync::Arc<std::sync::RwLock<LiveAudioConfig>>,
    settings: &Settings,
) {
    let Ok(mut live) = live_audio.write() else { return };
    live.monitor_enabled = settings.bool("monitor_enabled").unwrap_or(false);
    live.monitor_source = settings.get("monitor_source").unwrap_or("default").to_owned();
    live.monitor_replace = settings.get("monitor_mode").unwrap_or("mix") == "replace";
    live.monitor_gain = settings.number("monitor_gain").unwrap_or(1.0);
    live.output_gain = settings.number("gain").unwrap_or(1.0);
    live.microphone_gain = settings.number("microphone_gain").unwrap_or(1.0);
}

/// Everything the worker remembers between commands.
struct Agent {
    session: Option<Session>,
    bonds: BondStore,
    settings: Settings,
    found: Vec<DiscoveredDevice>,
    /// Cleared from the reading thread to interrupt playback.
    playing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Shared with the reading thread so it takes effect during playback.
    swap_channels: std::sync::Arc<std::sync::atomic::AtomicBool>,
    live_audio: std::sync::Arc<std::sync::RwLock<LiveAudioConfig>>,
    /// Kept alive after connecting: the GATT link is what configures the stream,
    /// and dropping it would tear down everything the audio needs.
    link: Option<olea_core::Link>,
    connected: Option<(String, u16)>,
    /// The ASEs this connection configured, so they can be released again.
    configured_ases: Vec<u8>,
    /// Read once per connection. Rediscovering the same services before every
    /// stream costs a second of round trips and floods the event queue for no
    /// new information.
    capabilities: Option<olea_core::AudioCapabilities>,
    control_point: Option<u16>,
    /// Set only when the controller reports that the peer link ended itself.
    /// A user stop deliberately leaves this empty and must never trigger retry.
    lost_reason: Option<u8>,
}

fn run_worker(
    commands: Receiver<Value>,
    playing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    swap_channels: std::sync::Arc<std::sync::atomic::AtomicBool>,
    live_audio: std::sync::Arc<std::sync::RwLock<LiveAudioConfig>>,
) {
    let mut agent = Agent {
        session: None,
        bonds: BondStore::load(&BondStore::default_path()),
        settings: Settings::load(&settings_path()),
        found: Vec::new(),
        playing,
        swap_channels,
        live_audio,
        link: None,
        connected: None,
        configured_ases: Vec::new(),
        capabilities: None,
        control_point: None,
        lost_reason: None,
    };

    agent.swap_channels.store(
        agent.settings.bool("swap_channels").unwrap_or(false),
        std::sync::atomic::Ordering::Relaxed,
    );
    agent.sync_live_audio();

    emit(json!({ "event": "ready", "paired": agent.bonds.len() }));

    while let Ok(command) = commands.recv() {
        let name = command.get("cmd").and_then(Value::as_str).unwrap_or("");

        let result = match name {
            "status" => agent.status(),
            "adapter" => agent.adapter(command.get("on").and_then(Value::as_bool).unwrap_or(true)),
            "scan" => agent.scan(command.get("seconds").and_then(Value::as_u64).unwrap_or(8)),
            "connect" => agent.connect(&command),
            "forget" => agent.forget(&command),
            "settings" => agent.report_settings(),
            "set" => agent.set(&command),
            "reset-settings" => agent.reset_settings(),
            "play" => agent.play(),
            "disconnect" => agent.disconnect(),
            "debug" => Ok(()),
            "stop" => Ok(()),
            "quit" => break,
            other => Err(format!("unknown command '{other}'")),
        };

        if let Err(text) = result {
            // Include the command so the UI can clear the matching spinner and
            // connecting state. A text-only error left a failed device row in
            // "Connecting..." forever even though the worker had already stopped.
            emit(json!({ "event": "error", "cmd": name, "text": text }));
        }
        emit(json!({ "event": "done", "cmd": name }));
    }
}

fn settings_path() -> std::path::PathBuf {
    BondStore::default_path().with_file_name("settings.txt")
}

fn apply_setting_change(settings: &mut Settings, key: &str, value: &str) -> Result<Vec<Value>, String> {
    let before = settings.clone();
    if key == "microphone_mode" && !matches!(value, "off" | "on") {
        return Err("unknown microphone mode".into());
    }
    if key == "microphone_quality" && !matches!(value, "voice" | "balanced" | "high") {
        return Err("unknown microphone quality".into());
    }
    if key == "microphone_target"
        && !matches!(value, "vb-cable" | "vb-cable-a" | "vb-cable-b" | "none")
    {
        return Err("unknown microphone target".into());
    }
    if key == "monitor_mode" && !matches!(value, "mix" | "replace") {
        return Err("unknown monitoring mode".into());
    }
    if key == "command_style"
        && olea_core::transport::CommandStyle::from_setting(value).is_none()
    {
        return Err("unknown HCI command addressing mode".into());
    }
    if matches!(key, "gain" | "microphone_gain" | "monitor_gain") {
        let gain = value.parse::<f32>().map_err(|_| "gain must be a number from 0 to 2")?;
        if !(0.0..=2.0).contains(&gain) {
            return Err("gain must be in the range from 0 to 2".into());
        }
    }

    settings.set(key, value);
    if key == "microphone_mode"
        && value == "off"
        && settings.get("monitor_source") == Some("headset")
    {
        settings.set("monitor_source", "default");
    }

    if key == "preset" && value != "custom" {
        let preset = parse_preset(value).ok_or("unknown LC3 preset")?;
        let codec = preset.codec(false);
        let qos = preset.qos(&codec);
        settings.set("rate_hz", codec.sampling_frequency.hz().unwrap_or(48_000).to_string());
        settings.set(
            "frame_ms",
            if codec.frame_duration.microseconds() == 7_500 { "7.5" } else { "10" },
        );
        settings.set("octets", codec.octets_per_frame.to_string());
        settings.set("phy", if qos.phy == 0x01 { "1M" } else { "2M" });
        settings.set("retransmissions", qos.retransmission_number.to_string());
        settings.set("max_latency_ms", qos.max_transport_latency_ms.to_string());
        settings.set("presentation_delay_ms", (qos.presentation_delay_us / 1000).to_string());
    }

    Ok(settings
        .scopes_touched_by(&before)
        .iter()
        .map(|knob| json!({ "key": knob.key, "scope": knob.scope.explain() }))
        .collect())
}

fn emit_settings_snapshot(settings: &Settings) {
    let capture_devices = olea_core::audio::list_capture_devices().unwrap_or_default();
    let playback_options: Vec<Value> = capture_devices
        .iter()
        .map(|device| json!({ "value": device.id, "label": device.name }))
        .collect();
    let mut monitor_options = vec![json!({
        "value": "default",
        "label": "Windows default microphone"
    })];
    monitor_options.extend(
        capture_devices
            .iter()
            .filter(|device| !device.is_virtual_cable())
            .map(|device| json!({ "value": device.id, "label": device.name })),
    );
    if settings.get("microphone_mode").unwrap_or("off") == "on" {
        monitor_options.push(json!({
            "value": "headset",
            "label": "Headset microphone (LE Audio)"
        }));
    }
    let knobs: Vec<Value> = olea_core::settings::KNOBS
        .iter()
        .map(|knob| {
            let mut item = json!({
                "key": knob.key,
                "description": knob.description,
                "scope": knob.scope.explain(),
                "value": settings.get(knob.key).unwrap_or(""),
            });
            if knob.key == "playback_source" {
                item["options"] = json!(playback_options);
            } else if knob.key == "monitor_source" {
                item["options"] = json!(monitor_options);
            }
            item
        })
        .collect();
    emit(json!({ "event": "settings", "knobs": knobs }));
}

impl Agent {
    fn status(&mut self) -> Result<(), String> {
        emit(json!({
            "event": "status",
            "adapterOn": self.session.is_some(),
            "paired": self.bonds.all().map(|b| json!({
                "address": b.address,
                "name": b.name,
                "leAudio": b.le_audio,
            })).collect::<Vec<_>>(),
        }));
        Ok(())
    }

    /// Turns the radio on or off, which for this stack means owning the adapter.
    ///
    /// Off drops the session entirely rather than sending a reset: the adapter
    /// then belongs to nobody, which is the state Windows can take it back from.
    fn adapter(&mut self, on: bool) -> Result<(), String> {
        if !on {
            // Everything holding the adapter has to go, in order: the audio
            // loop, then the GATT link, then the reader threads, then the
            // session. Leaving any of them behind keeps the device open and the
            // next attempt to switch Bluetooth back on is refused.
            let _ = self.disconnect();
            if let Some(session) = self.session.as_mut() {
                session.shutdown();
            }
            self.session = None;
            self.found.clear();
            emit(json!({ "event": "adapter", "on": false }));
            return Ok(());
        }

        if self.session.is_some() {
            return self.status();
        }

        self.settings = Settings::load(&settings_path());
        self.sync_live_audio();
        let mut session = Session::new(self.session_config());
        let mut detail = json!({ "event": "adapter", "on": true });

        session
            .open_adapter(|progress| {
                if let Progress::AdapterReady { version, address } = progress {
                    detail["version"] = json!(version);
                    detail["address"] = json!(address);
                }
            })
            .map_err(|e| format!("adapter could not be enabled: {e}"))?;

        self.session = Some(session);
        emit(detail);
        Ok(())
    }

    fn session_config(&self) -> SessionConfig {
        let defaults = SessionConfig::default();
        let command_style = self.settings.get("command_style")
            .and_then(olea_core::transport::CommandStyle::from_setting)
            .unwrap_or(defaults.command_style);

        SessionConfig {
            command_style,
            prefer_single_cis: true,
            audio_device: self.settings.get("playback_source").map(str::to_owned),
            limiter: self
                .settings
                .number("gain")
                .map(OutputLimiter::with_gain)
                .unwrap_or_default(),
            microphone_target: match self.settings.get("microphone_target") {
                Some("vb-cable") => Some("CABLE Input".to_string()),
                Some("vb-cable-a") => Some("CABLE-A Input".to_string()),
                Some("vb-cable-b") => Some("CABLE-B Input".to_string()),
                _ => None,
            },
            microphone_gain: self.settings.number("microphone_gain").unwrap_or(1.0),
            monitor_source: self
                .settings
                .bool("monitor_enabled")
                .unwrap_or(false)
                .then(|| self.settings.get("monitor_source").unwrap_or("default").to_owned()),
            monitor_replace: self.settings.get("monitor_mode").unwrap_or("mix") == "replace",
            monitor_gain: self.settings.number("monitor_gain").unwrap_or(1.0),
            live_audio: self.live_audio.clone(),
            idle_timeout: self
                .settings
                .minutes("idle_timeout_min")
                .unwrap_or(defaults.idle_timeout),
            swap_channels: self.swap_channels.clone(),
            reconnect: ReconnectPolicy {
                enabled: self.settings.bool("reconnect_enabled").unwrap_or(true),
                interval: self
                    .settings
                    .number("reconnect_interval_s")
                    .map(Duration::from_secs_f32)
                    .unwrap_or(defaults.reconnect.interval),
                window: self
                    .settings
                    .minutes("reconnect_window_min")
                    .unwrap_or(defaults.reconnect.window),
            },
            ..defaults
        }
    }

    /// Scans, reporting each device as it is seen and whether we already know it.
    fn scan(&mut self, seconds: u64) -> Result<(), String> {
        let bonds = &self.bonds;
        let session = self
            .session
            .as_mut()
            .ok_or("adapter is off")?;

        session.config_mut().scan_duration = Duration::from_secs(seconds.clamp(1, 60));

        let devices = session
            .scan(|progress| {
                if let Progress::DeviceFound { name, address, rssi, le_audio } = progress {
                    emit(json!({
                        "event": "device",
                        "address": address,
                        "name": name,
                        "rssi": rssi,
                        "leAudio": le_audio,
                        "paired": bonds.contains(&address),
                    }));
                }
            })
            .map_err(|e| format!("scan failed: {e}"))?;

        self.found = devices;
        Ok(())
    }

    /// Connects to a device, pairing first only when we do not already know it.
    fn connect(&mut self, command: &Value) -> Result<(), String> {
        let address = command
            .get("address")
            .and_then(Value::as_str)
            .ok_or("address is missing")?
            .to_string();

        let already_paired = self.bonds.contains(&address);
        let device = self
            .found
            .iter()
            .find(|d| d.address.to_string().eq_ignore_ascii_case(&address))
            .cloned()
            .ok_or("device is not in the scan results; start a scan first")?;

        self.refresh_connection_config()?;

        self.connect_device(&address, &device, already_paired)?;

        let policy = self
            .session
            .as_ref()
            .ok_or("adapter is off")?
            .config()
            .reconnect;
        let lost_at = Instant::now();

        while let Some(reason) = self.lost_reason.take() {
            if !ReconnectPolicy::worth_reconnecting(reason) || !policy.should_retry(lost_at.elapsed()) {
                break;
            }

            log(format!(
                "connection was lost ({}); next attempt in {} s",
                olea_core::hci::disconnect_reason(reason),
                policy.interval.as_secs()
            ));

            // Reuse the playback flag as an interruptible wait token. The stdin
            // reader clears it immediately for disconnect, adapter-off or quit,
            // so the user never has to wait out the reconnect window.
            self.playing.store(true, std::sync::atomic::Ordering::Relaxed);
            let retry_at = Instant::now() + policy.interval;
            while Instant::now() < retry_at
                && self.playing.load(std::sync::atomic::Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            if !self.playing.swap(false, std::sync::atomic::Ordering::Relaxed) {
                log("automatic reconnect canceled");
                return Ok(());
            }

            if !policy.should_retry(lost_at.elapsed()) {
                break;
            }

            log(format!("trying to reconnect {}", device.display_name()));
            emit(json!({ "event": "reconnecting", "address": address }));
            if let Err(error) = self.connect_device(&address, &device, true) {
                log(format!("attempt failed: {error}"));
                // A failed connection has no playback event to set a reason.
                // Keep the retry loop alive with the original link-loss reason.
                self.lost_reason = Some(reason);
            }
        }

        if policy.enabled && !policy.should_retry(lost_at.elapsed()) {
            log("automatic reconnect ended: retry window expired");
            emit(json!({ "event": "reconnect-stopped", "address": address }));
        }

        Ok(())
    }

    /// Applies settings whose documented scope is the next connection.
    fn refresh_connection_config(&mut self) -> Result<(), String> {
        self.settings = Settings::load(&settings_path());
        self.sync_live_audio();
        let defaults = SessionConfig::default();
        let session = self.session.as_mut().ok_or("adapter is off")?;
        let config = session.config_mut();

        // `device` is the preferred Bluetooth headset. It must never select the
        // Windows audio source (that old key collision is what made HyperX
        // QuadCast replace the music capture).
        config.audio_device = self.settings.get("playback_source").map(str::to_owned);
        config.microphone_target = match self.settings.get("microphone_target") {
            Some("vb-cable") => Some("CABLE Input".to_string()),
            Some("vb-cable-a") => Some("CABLE-A Input".to_string()),
            Some("vb-cable-b") => Some("CABLE-B Input".to_string()),
            _ => None,
        };
        config.microphone_gain = self.settings.number("microphone_gain").unwrap_or(1.0);
        config.monitor_source = self
            .settings
            .bool("monitor_enabled")
            .unwrap_or(false)
            .then(|| self.settings.get("monitor_source").unwrap_or("default").to_owned());
        config.monitor_replace = self.settings.get("monitor_mode").unwrap_or("mix") == "replace";
        config.monitor_gain = self.settings.number("monitor_gain").unwrap_or(1.0);
        config.live_audio = self.live_audio.clone();
        config.limiter = OutputLimiter::with_gain(
            self.settings.number("gain").unwrap_or(defaults.limiter.gain()),
        );
        config.idle_timeout = self
            .settings
            .minutes("idle_timeout_min")
            .unwrap_or(defaults.idle_timeout);
        config.reconnect = ReconnectPolicy {
            enabled: self.settings.bool("reconnect_enabled").unwrap_or(true),
            interval: Duration::from_secs_f32(
                self.settings
                    .number("reconnect_interval_s")
                    .unwrap_or(defaults.reconnect.interval.as_secs_f32()),
            ),
            window: self
                .settings
                .minutes("reconnect_window_min")
                .unwrap_or(defaults.reconnect.window),
        };

        Ok(())
    }

    fn sync_live_audio(&self) {
        let Ok(mut live) = self.live_audio.write() else { return };
        live.monitor_enabled = self.settings.bool("monitor_enabled").unwrap_or(false);
        live.monitor_source = self.settings.get("monitor_source").unwrap_or("default").to_owned();
        live.monitor_replace = self.settings.get("monitor_mode").unwrap_or("mix") == "replace";
        live.monitor_gain = self.settings.number("monitor_gain").unwrap_or(1.0);
        live.output_gain = self.settings.number("gain").unwrap_or(1.0);
        live.microphone_gain = self.settings.number("microphone_gain").unwrap_or(1.0);
    }

    /// Establishes, configures and plays one physical connection attempt.
    fn connect_device(
        &mut self,
        address: &str,
        device: &DiscoveredDevice,
        already_paired: bool,
    ) -> Result<(), String> {
        self.lost_reason = None;
        // Advertising names are optional and may disappear between scans. Do
        // not replace a useful paired name with the address just because this
        // particular advertisement was nameless.
        let advertised_name = device.display_name();
        let stored_bond = self.bonds.get(address).cloned();
        let friendly_name = if useful_device_name(&advertised_name, address) {
            advertised_name
        } else {
            stored_bond
                .as_ref()
                .map(|bond| bond.name.clone())
                .filter(|name| useful_device_name(name, address))
                .unwrap_or_else(|| address.to_string())
        };
        let le_audio = device.is_le_audio()
            || stored_bond.as_ref().map(|bond| bond.le_audio).unwrap_or(false);
        let session = self.session.as_mut().ok_or("adapter is off")?;

        log(if already_paired {
            format!("connecting {friendly_name} (already paired)")
        } else {
            format!("pairing {friendly_name}")
        });

        let handle = session
            .connect(&device, |p| report(p))
            .map_err(|e| format!("connection failed: {e}"))?;

        let mut link = match session.open_link(handle) {
            Ok(link) => link,
            Err(e) => {
                session.disconnect(handle);
                return Err(format!("connection could not be opened: {e}"));
            }
        };

        let stored_key = self
            .bonds
            .get(address)
            .map(|bond| bond.long_term_key)
            .filter(|key| key.iter().any(|&byte| byte != 0));

        let key_result = if let Some(key) = stored_key {
            session.resume_encryption(handle, &key).map(|_| {
                log("restored an encrypted connection from the saved bond");
                key
            }).map_err(|e| format!("encryption restore failed: {e}; try unpairing the device"))
        } else {
            session.pair(&mut link, handle, device)
                .map_err(|e| format!("pairing failed: {e}"))
        };
        let long_term_key = match key_result {
            Ok(key) => key,
            Err(error) => {
                session.disconnect(handle);
                return Err(error);
            }
        };

        // Remember it only once the key actually exists, so a failed attempt
        // never leaves a bond that cannot be used.
        self.bonds.insert(Bond {
            address: device.address.to_string(),
            name: friendly_name.clone(),
            long_term_key,
            le_audio,
        });
        let _ = self.bonds.save(&BondStore::default_path());
        // The bond exists now, before the slower PACS/ASCS discovery starts.
        // Let the UI move the row immediately instead of showing a successfully
        // paired headset under "discovered" for several more seconds.
        emit(json!({
            "event": "paired",
            "address": address,
            "name": friendly_name,
            "leAudio": le_audio,
        }));

        let session = self.session.as_mut().ok_or("adapter is off")?;
        let capabilities = match session.read_capabilities(&mut link, |p| report(p)) {
            Ok(capabilities) => capabilities,
            Err(e) => {
                session.disconnect(handle);
                return Err(format!("capability discovery failed: {e}"));
            }
        };

        emit(json!({
            "event": "capabilities",
            "address": address,
            "summary": describe_capabilities(&capabilities),
        }));

        // The raw records, byte for byte. Everything about this device has been
        // read through one parser, and "stereo in one stream: no" - the single
        // fact that sent the whole design down the two-channel path - has never
        // been checked against the bytes it came from. If that reading is wrong,
        // so is every decision built on it.
        for (index, record) in capabilities.sink_records.iter().enumerate() {
            let hex: String = record
                .raw
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");

            log(format!(
                "  PAC {}: channels {:?}, maximum frames per SDU {} | {hex}",
                index + 1,
                record.capabilities.channel_counts,
                record.capabilities.max_frames_per_sdu
            ));
        }

        let control_point = match find_control_point(&mut link) {
            Ok(control_point) => control_point,
            Err(error) => {
                session.disconnect(handle);
                return Err(error);
            }
        };
        self.capabilities = Some(capabilities);
        self.control_point = Some(control_point);

        match session.attach_volume_control(&mut link) {
            Ok(Some(summary)) => log(summary),
            Ok(None) => log("device does not expose volume control"),
            Err(e) => log(format!("volume control could not be attached: {e}")),
        }

        // Keep the link: configuring the stream and everything after it happens
        // over this same connection, and dropping it here is exactly why the
        // first version of this agent connected successfully and then played
        // nothing at all.
        self.link = Some(link);
        self.connected = Some((address.to_string(), handle));

        emit(json!({
            "event": "connected",
            "address": address,
            "handle": handle,
            "name": friendly_name,
        }));

        // Windows starts the stream as soon as a headset connects, and so do we.
        // If any later ASCS/CIS/audio setup step fails, close the already-live
        // ACL handle before returning. Otherwise the next click/retry races a
        // ghost connection still retained by the controller and headphones.
        let result = self.play();
        if result.is_err() {
            let _ = self.disconnect();
        }
        result
    }

    /// Configures the stream, brings up the isochronous channels and plays.
    ///
    /// Blocks until the audio stops, so the reading thread is the only place
    /// that can interrupt it.
    fn play(&mut self) -> Result<(), String> {
        let (_, handle) = self.connected.clone().ok_or("no device is connected")?;
        let mut link = self.link.take().ok_or("connection is not open")?;

        let result = self.play_on(&mut link, handle);
        // A controller-reported disconnect makes this Link permanently stale.
        // Keeping it used to leave both the UI and the next attempt believing a
        // dead ACL connection was still usable.
        if self.connected.is_some() {
            self.link = Some(link);
        }
        result
    }

    fn play_on(&mut self, link: &mut olea_core::Link, handle: u16) -> Result<(), String> {
        let microphone_enabled = self.settings.get("microphone_mode").unwrap_or("off") == "on";

        let capabilities = self
            .capabilities
            .clone()
            .ok_or("device capabilities have not been loaded")?;
        let control_point = self.control_point.ok_or("ASE control point is unknown")?;

        let chosen = self.settings.get("preset").unwrap_or("windows").to_string();
        let custom_codec = self.custom_codec();
        let custom_qos = self.custom_qos();

        let prefer_single_cis = self
            .session
            .as_ref()
            .ok_or("adapter is off")?
            .config()
            .prefer_single_cis;

        let (plan, preset_label) = if chosen == "custom" {
            let plan = StreamPlan::build_custom(
                &capabilities,
                custom_codec,
                custom_qos,
                prefer_single_cis,
            )
            .map_err(|e| format!("device rejected the custom configuration: {e}"))?;

            (plan, "custom".to_string())
        } else {
            let (plan, preset) = StreamPlan::build_with_fallback(
                &capabilities,
                parse_preset(&chosen).unwrap_or(Preset::WindowsDefault),
                prefer_single_cis,
            )
            .map_err(|e| format!("stream could not be scheduled: {e}"))?;

            (plan, preset.label().to_string())
        };

        // The radio settings apply whichever way the plan was built. A preset
        // chooses the codec; how hard the radio works to deliver it is a
        // separate decision and the user is allowed to make it.
        let mut plan = plan;

        // Exactly what Windows does, and nothing else. The experimental
        // alternatives - swapped ears, other ASE pairs, interleaved packing -
        // were removed once the trace showed what the real driver sends. Their
        // values lingered in saved settings and quietly configured ASE 1 as the
        // right ear and ASE 4 as the left, which made every later measurement
        // meaningless. A setting with no control is a setting nobody can see.

        // Only when the user asked for custom. A preset carries radio settings
        // that were measured, not guessed, and letting a stale saved value
        // override them makes a fix look like it did nothing.
        if chosen == "custom" {
            if let Some(phy) = self.settings.get("phy") {
                plan.qos.phy = if phy == "1M" { 0x01 } else { 0x02 };
            }
            if let Some(rtn) = self.settings.number("retransmissions") {
                plan.qos.retransmission_number = rtn as u8;
            }
        }
        let preset = PresetLabel(preset_label);

        // Mono is a choice, not only a fallback: two isochronous channels are
        // where these headphones are unreliable, and one that always works can
        // be worth more than stereo that sometimes does.
        let mode = self.settings.get("audio_mode").unwrap_or("stereo").to_string();
        let mut plan = match mode.as_str() {
            "mono" => {
                log("mono mode: one channel with left and right mixed");
                plan.into_mono()
            }
            "legacy" => {
                log("legacy mode: both sides report left, sequential packing, without response reads");
                plan.into_legacy()
            }
            _ => plan,
        };
        if microphone_enabled {
            let quality = match self.settings.get("microphone_quality").unwrap_or("balanced") {
                "voice" => MicrophoneQuality::Voice,
                "high" => MicrophoneQuality::High,
                _ => MicrophoneQuality::Balanced,
            };
            plan = plan
                .with_microphone(&capabilities, quality)
                .map_err(|error| format!("headset microphone could not be configured: {error}"))?;
            log("headset microphone enabled; playback remains active");
        } else {
            log("headset microphone disabled; Source ASE is released and radio budget stays with playback");
        }
        log(if self.settings.bool("monitor_enabled").unwrap_or(false) {
            "monitoring enabled according to settings"
        } else {
            "monitoring disabled; no PC microphone is opened"
        });
        let _legacy = plan.legacy;


        log(format!("ASE control point {control_point:#06x}, preset {}", preset.0));

        // Needed before configuring, to read each ASE's answer afterwards.
        let sink_handles = link.sink_ase_handles().unwrap_or_default();
        let source_handles = if plan.microphone.is_some() {
            link.source_ase_handles().unwrap_or_default()
        } else {
            Vec::new()
        };

        // Subscribe before configuring, or the device's answer arrives before
        // anyone is listening for it. Legacy mode does not: not subscribing is
        // part of what it reproduces, and the extra traffic is one of the few
        // differences between then and now.


        // Printed rather than left in the hex: which ear each stream claims is
        // the one thing that cannot be checked by listening to a log, and
        // getting it wrong sounds like a codec fault rather than a routing one.
        for (index, allocation) in (0..plan.ase_ids.len())
            .map(|i| (i, plan.channel_allocation(i)))
        {
            // Never fatal. This line exists to describe what is about to be
            // sent; failing the whole stream because a diagnostic met a value
            // it did not recognise is worse than printing the number.
            let side = match allocation {
                olea_core::bap::LOCATION_FRONT_LEFT => "left".to_string(),
                olea_core::bap::LOCATION_FRONT_RIGHT => "right".to_string(),
                olea_core::bap::LOCATION_STEREO => "stereo".to_string(),
                olea_core::bap::LOCATION_MONO => "mono".to_string(),
                other => format!("{other:#010x}"),
            };
            log(format!("  ASE {} → {side} ({allocation:#010x})", plan.ase_ids[index]));
        }

        // Recorded before the result is known: the ASEs are configured on the
        // device the moment the writes land, so they need releasing even if
        // everything after this fails.
        self.configured_ases = plan.ase_ids.clone();
        if let Some(microphone) = &plan.microphone {
            self.configured_ases.push(microphone.ase_id);
        }

        // Everything back to Idle first, microphone included. Whatever
        // connected to these headphones before us may have left endpoints
        // configured, and those hold isochronous budget the two streams we
        // actually want are then short of.
        {
            let session = self.session.as_mut().ok_or("adapter is off")?;
            session.release_all_streams(link, control_point, &capabilities);
        }
        std::thread::sleep(Duration::from_millis(200));

        // Config Codec first, on its own. The device answers by publishing the
        // QoS it prefers, and that answer is worth reading before committing to
        // a presentation delay it may not be able to meet.
        {
            let session = self.session.as_mut().ok_or("adapter is off")?;
            session
                .write_ascs(link, control_point, &plan.codec_writes())
                .map_err(|e| format!("konfigurace kodeku selhala: {e}"))?;
        }

        let mut plan = plan;
        if let Some(delay) = ask_device_for_delay(link, &sink_handles, plan.qos.presentation_delay_us)
        {
            if delay != plan.qos.presentation_delay_us {
                log(format!(
                    "headphones request {} ms delay instead of {} ms; using their value",
                    delay / 1000,
                    plan.qos.presentation_delay_us / 1000
                ));
                plan.qos.presentation_delay_us = delay;
            }
        }

        {
            let session = self.session.as_mut().ok_or("adapter is off")?;
            session
                .write_ascs(link, control_point, &plan.qos_and_enable_writes())
                .map_err(|e| format!("konfigurace QoS selhala: {e}"))?;
        }

        // What the headphones actually thought of it. A write response only
        // means the bytes arrived; this is the part that says yes or no.


        // These headphones establish the second channel only sometimes. Windows
        // has the same trouble with them - the difference is what happens next.
        //
        // A channel that reports a failure has had its handle released by the
        // controller, so there is nothing left to ask again for: the only real
        // recovery is to hand the streams back, tear the group down and build
        // the whole thing afresh. Carrying on with one channel is not an option
        // on this device - it refuses to start either stream until it has both,
        // so a partial success is silence with extra steps.
        // Ordered most-likely-first, and the one that works is remembered so the
        // next connection starts there. Trying variants is not elegance, it is
        // the only honest response to a device that behaves differently from one
        // attempt to the next: measure which shape it accepts instead of arguing
        // with it.
        // The normal path is specification-driven and uses the parameters read
        // from this device. The extra shapes below are a compatibility profile
        // for the JBL Tune 780NC firmware seen in the supplied traces; trying
        // those blindly on every LE Audio headset is the opposite of universal.
        let jbl_tune_780 = self
            .connected
            .as_ref()
            .and_then(|(address, _)| self.bonds.get(address))
            .map(|bond| bond.name.to_ascii_lowercase().contains("jbl tune 780"))
            .unwrap_or(false);
        let variant_count = if jbl_tune_780 { VARIANTS.len() } else { 1 };
        let mut order: Vec<usize> = (0..variant_count).collect();
        if let Some(known) = self.settings.get("winning_variant") {
            if let Some(position) = VARIANTS.iter().position(|v| v.name == known) {
                if position < variant_count {
                    order.retain(|&i| i != position);
                    order.insert(0, position);
                    log(format!("starting with the variant that succeeded previously: {known}"));
                }
            }
        }

        if jbl_tune_780 {
            log("JBL Tune 780NC compatibility profile active");
        } else {
            log("generic BAP profile without JBL-specific attempts");
        }

        let mut cis = Vec::new();
        let mut survivors: Vec<u16> = Vec::new();
        let attempts = order.len();

        for (attempt, &index) in order.iter().enumerate() {
            let variant = &VARIANTS[index];
            let attempt = attempt + 1;

            plan.target_latency = variant.target_latency;
            plan.context = variant.context;
            // In Custom, the chosen PHY is authoritative. Compatibility
            // variants may adjust it only for named presets; otherwise 1M was
            // silently changed back to 2M on the very first attempt.
            if chosen != "custom" {
                plan.qos.phy = variant.phy;
            }

            if attempt > 1 {
                log(format!("pokus {attempt}/{attempts}: {}", variant.describe()));

                // Start over from Idle: reusing ASEs left mid-flight is an
                // invalid transition and the device refuses the channel again.
                {
                    let session = self.session.as_mut().ok_or("adapter is off")?;
                    session.release_streams(link, control_point, &plan.ase_ids);
                }
                std::thread::sleep(Duration::from_millis(300));

                // Best effort. After a failed channel this device goes quiet on
                // ATT for a while; treating that as fatal turns one bad attempt
                // into a dead session, when the right answer is to stop trying
                // new shapes and say so.
                let session = self.session.as_mut().ok_or("adapter is off")?;
                let reconfigured = session
                    .write_ascs(link, control_point, &plan.codec_writes())
                    .and_then(|_| {
                        session.write_ascs(link, control_point, &plan.qos_and_enable_writes())
                    });

                if let Err(e) = reconfigured {
                    log(format!("  headphones stopped responding ({e}); ending retries"));
                    break;
                }
            }

            let session = self.session.as_mut().ok_or("adapter is off")?;
            match session.establish_isochronous(&plan, handle) {
                Ok(outcome) if outcome.complete() => {
                    log(format!("USPELO: {} - {}", variant.describe(), outcome.describe()));
                    self.settings.set("winning_variant", variant.name);
                    let _ = self.settings.save(&settings_path());
                    cis = outcome.established;
                    break;
                }
                Ok(outcome) => {
                    log(format!("  {}", outcome.describe()));
                    survivors = outcome.established;
                }
                Err(e) => log(format!("  {e}")),
            }

            {
                // Order matters: a channel that did come up keeps its ASE busy
                // and keeps the group un-removable, and the reconfiguration
                // that follows then times out against a device with nothing
                // left to answer with.
                let session = self.session.as_mut().ok_or("adapter is off")?;
                session.release_cis(&survivors);
                session.release_isochronous(&plan);
                survivors = Vec::new();
            }

        }

        if cis.is_empty() {
            return Err(format!(
                "none of {attempts} variants established both channels; try legacy or mono mode"
            ));
        }

        // For a Sink ASE the headphones are the Audio Sink. BAP 5.6.3.2 says
        // the server must initiate Receiver Start Ready autonomously once its
        // data path is ready. Sending opcode 0x04 from this client is only valid
        // for a Source ASE (where this client would be the Audio Sink), so do
        // not try to force the transition from the wrong side.
        //
        // More importantly, CAP says to wait for every Sink ASE used by the
        // stream to reach Streaming before sending audio. The old code read the
        // states once, printed them, and sent LC3 regardless. With one ASE still
        // Enabling these headphones render the surviving channel into both ears:
        // precisely "right on both sides, left missing".
        let wanted_ases: Vec<(u8, u16)> = sink_handles
            .iter()
            .copied()
            .filter(|(ase_id, _)| plan.ase_ids.contains(ase_id))
            .collect();
        if wanted_ases.len() != plan.ase_ids.len() {
            return Err(format!(
                "stereo stream cannot be verified: found {} of {} Sink ASE characteristics",
                wanted_ases.len(),
                plan.ase_ids.len()
            ));
        }

        // A Source ASE waits for the client (the audio receiver in this
        // direction) to confirm that its HCI output path is ready.
        if let Some(microphone) = &plan.microphone {
            if !source_handles.iter().any(|(ase_id, _)| *ase_id == microphone.ase_id) {
                return Err(format!(
                    "microphone cannot be verified: Source ASE {} has no readable state characteristic",
                    microphone.ase_id
                ));
            }
            let session = self.session.as_mut().ok_or("adapter is off")?;
            session.start_receivers(link, control_point, &[microphone.ase_id]);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut states = Vec::new();

        link.set_att_timeout(Duration::from_millis(350));
        while Instant::now() < deadline {
            states.clear();

            for &(ase_id, handle) in &wanted_ases {
                let state = link
                    .read_characteristic(handle)
                    .ok()
                    .and_then(|value| olea_core::bap::ase::parse_state(&value))
                    .map(|state| state.state);
                states.push((ase_id, state));
            }

            if states.iter().all(|(_, state)| {
                *state == Some(olea_core::bap::ase::STATE_STREAMING)
            }) {
                break;
            }

            std::thread::sleep(Duration::from_millis(80));
        }
        link.set_att_timeout(olea_core::link::ATT_TIMEOUT);

        if self.settings.bool("diagnostics").unwrap_or(false) {
            for &(ase_id, state) in &states {
                log(format!(
                    "  ASE {ase_id} je ve stavu {}",
                    state
                        .map(olea_core::bap::ase::state_name)
                        .unwrap_or("could not be read")
                ));
            }
        }

        let not_streaming: Vec<String> = states
            .iter()
            .filter(|(_, state)| *state != Some(olea_core::bap::ase::STATE_STREAMING))
            .map(|(ase_id, state)| {
                format!(
                    "ASE {ase_id}: {}",
                    state
                        .map(olea_core::bap::ase::state_name)
                        .unwrap_or("unreadable state")
                )
            })
            .collect();

        if !not_streaming.is_empty() {
            return Err(format!(
                "headphones did not start both stereo channels ({}); audio is not sent because one channel could play as mono in both ears",
                not_streaming.join(", ")
            ));
        }

        if let Some(microphone) = &plan.microphone {
            let source_handle = source_handles
                .iter()
                .find(|(ase_id, _)| *ase_id == microphone.ase_id)
                .map(|(_, handle)| *handle)
                .ok_or("microphone state is unavailable")?;
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = None;
            link.set_att_timeout(Duration::from_millis(350));
            while Instant::now() < deadline {
                state = link
                    .read_characteristic(source_handle)
                    .ok()
                    .and_then(|value| olea_core::bap::ase::parse_state(&value))
                    .map(|state| state.state);
                if state == Some(olea_core::bap::ase::STATE_STREAMING) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            link.set_att_timeout(olea_core::link::ATT_TIMEOUT);
            if state != Some(olea_core::bap::ase::STATE_STREAMING) {
                return Err(format!(
                    "microphone Source ASE {} is not in Streaming state ({})",
                    microphone.ase_id,
                    state
                        .map(olea_core::bap::ase::state_name)
                        .unwrap_or("unreadable state")
                ));
            }
            log(format!(
                "microphone Source ASE {} active: {} Hz, {} octets per frame",
                microphone.ase_id,
                microphone.codec.sampling_frequency.hz().unwrap_or(0),
                microphone.codec.octets_per_frame
            ));
        }

        emit(json!({ "event": "streaming-started", "cis": cis, "latencyMs": plan.latency_ms() }));

        let playing = self.playing.clone();
        playing.store(true, std::sync::atomic::Ordering::Relaxed);

        let session = self.session.as_mut().ok_or("adapter is off")?;
        let mut lost_reason = None;
        let outcome = session.run_audio(
            &plan,
            &cis,
            Some(handle),
            |progress| {
                if let Progress::Disconnected { reason } = &progress {
                    lost_reason = Some(*reason);
                }
                report(progress);
            },
            || !playing.load(std::sync::atomic::Ordering::Relaxed),
        );

        playing.store(false, std::sync::atomic::Ordering::Relaxed);

        // Best effort only. If the channels are still established this fails,
        // and the cleanup at the start of the next attempt is what actually
        // guarantees a clean group.
        session.release_isochronous(&plan);
        emit(json!({ "event": "streaming-stopped" }));

        if let Some(reason) = lost_reason {
            let address = self.connected.take().map(|(address, _)| address).unwrap_or_default();
            self.capabilities = None;
            self.control_point = None;
            self.configured_ases.clear();
            self.lost_reason = Some(reason);
            emit(json!({
                "event": "disconnected",
                "address": address,
                "reason": reason,
                "automatic": true,
            }));
        } else if let Err(error) = &outcome {
            // A transport/audio failure is not a successful stop. Tear down
            // the stale ACL state and enter the same reconnect path as a radio
            // link loss, so the UI cannot remain falsely "connected".
            log(format!("playback failed: {error}; restoring the connection"));
            session.disconnect(handle);
            let address = self.connected.take().map(|(address, _)| address).unwrap_or_default();
            self.link = None;
            self.capabilities = None;
            self.control_point = None;
            self.configured_ases.clear();
            self.lost_reason = Some(0x08); // connection timeout: safe reconnect class
            emit(json!({
                "event": "disconnected",
                "address": address,
                "reason": 0x08,
                "automatic": true,
            }));
            return Ok(());
        }

        outcome.map_err(|e| format!("playback ended: {e}"))
    }

    /// The codec configuration the user typed in, with the defaults filled in.
    fn custom_codec(&self) -> olea_core::bap::CodecConfiguration {
        use olea_core::bap::{CodecConfiguration, FrameDuration, SamplingFrequency};

        let rate = self.settings.number("rate_hz").unwrap_or(48_000.0) as u32;
        let frame = self.settings.number("frame_ms").unwrap_or(10.0);

        CodecConfiguration {
            sampling_frequency: SamplingFrequency::from_hz(rate)
                .unwrap_or(SamplingFrequency::HZ_48000),
            frame_duration: if frame < 8.75 { FrameDuration::Ms7_5 } else { FrameDuration::Ms10 },
            channel_allocation: olea_core::bap::LOCATION_FRONT_LEFT,
            octets_per_frame: self.settings.number("octets").unwrap_or(100.0) as u16,
            frames_per_sdu: 1,
        }
    }

    fn custom_qos(&self) -> olea_core::bap::QosConfiguration {
        use olea_core::bap::QosConfiguration;

        let codec = self.custom_codec();

        QosConfiguration {
            sdu_interval_us: codec.frame_duration.microseconds(),
            framing: 0,
            // 1M reaches further and shrugs off interference; 2M spends less
            // time on air, which matters when two channels share the interval.
            phy: match self.settings.get("phy") {
                Some("1M") => 0x01,
                _ => 0x02,
            },
            max_sdu: codec.octets_per_frame,
            retransmission_number: self.settings.number("retransmissions").unwrap_or(2.0) as u8,
            max_transport_latency_ms: self.settings.number("max_latency_ms").unwrap_or(20.0) as u16,
            presentation_delay_us: (self.settings.number("presentation_delay_ms").unwrap_or(40.0)
                * 1000.0) as u32,
        }
    }

    fn disconnect(&mut self) -> Result<(), String> {
        self.playing.store(false, std::sync::atomic::Ordering::Relaxed);

        // Drop our side first, then tell the controller. Dropping the link
        // alone leaves the connection standing as far as the controller is
        // concerned, and every later attempt to reach the same headphones is
        // refused because one already exists.
        let handle = self.connected.take().map(|(_, handle)| handle);

        // Hand the streams back before the link goes: an ASE left in Enabling
        // makes the next connection's configuration an illegal transition, and
        // the device refuses the isochronous channel rather than explaining.
        if let (Some(session), Some(link), Some(control_point)) = (
            self.session.as_mut(),
            self.link.as_mut(),
            self.control_point,
        ) {
            let ases = std::mem::take(&mut self.configured_ases);
            if !ases.is_empty() {
                session.release_streams(link, control_point, &ases);
            }
        }

        self.link = None;
        self.capabilities = None;
        self.control_point = None;

        if let (Some(session), Some(handle)) = (self.session.as_mut(), handle) {
            session.disconnect(handle);
        }

        emit(json!({ "event": "disconnected" }));
        Ok(())
    }

    fn forget(&mut self, command: &Value) -> Result<(), String> {
        let address = command
            .get("address")
            .and_then(Value::as_str)
            .ok_or("address is missing")?;

        if self.bonds.remove(address) {
            self.bonds
                .save(&BondStore::default_path())
                .map_err(|e| format!("save failed: {e}"))?;
            log(format!("{address} removed"));
        }
        self.status()
    }

    fn report_settings(&mut self) -> Result<(), String> {
        emit_settings_snapshot(&self.settings);
        Ok(())
    }

    fn set(&mut self, command: &Value) -> Result<(), String> {
        let key = command.get("key").and_then(Value::as_str).ok_or("key is missing")?;
        let value = command.get("value").and_then(Value::as_str).ok_or("value is missing")?;
        if command.get("prePersisted").and_then(Value::as_bool) == Some(true) {
            self.settings = Settings::load(&settings_path());
            self.sync_live_audio();
            return Ok(());
        }

        let needs = apply_setting_change(&mut self.settings, key, value)?;
        self.settings.save(&settings_path()).map_err(|e| format!("save failed: {e}"))?;
        self.sync_live_audio();
        emit(json!({ "event": "applied", "key": key, "value": value, "needs": needs }));
        Ok(())
    }

    fn reset_settings(&mut self) -> Result<(), String> {
        self.settings = Settings::defaults();
        self.sync_live_audio();
        self.settings
            .save(&settings_path())
            .map_err(|e| format!("save failed: {e}"))?;
        self.report_settings()
    }
}

/// Reads each configured ASE and returns a presentation delay it will accept.
///
/// Read rather than subscribed: notifications arriving during setup are what
/// stopped the second isochronous channel coming up, and a plain read gets the
/// same value without putting anything extra on the air.
fn ask_device_for_delay(
    link: &mut olea_core::Link,
    sink_handles: &[(u8, u16)],
    wanted_us: u32,
) -> Option<u32> {
    use olea_core::bap::ase;

    let mut chosen = None;

    for &(ase_id, handle) in sink_handles {
        let Ok(value) = link.read_characteristic(handle) else {
            continue;
        };
        let Some(qos) = ase::parse_preferred_qos(&value) else {
            continue;
        };

        log(format!(
            "  ASE {ase_id} requests {}-{} µs delay, prefers {} µs, RTN {}, latency {} ms",
            qos.presentation_delay_min_us,
            qos.presentation_delay_max_us,
            qos.preferred_delay_min_us,
            qos.retransmission_preference,
            qos.max_transport_latency_ms
        ));

        // What it actually configured, not what it was asked for. The two do not
        // have to match, and until now nothing in this stack could tell.
        if let Some(applied) = ase::parse_configured_codec(&value) {
            log(format!(
                "  ASE {ase_id} configured: {} Hz, {} ms, {} octets, allocation {:#010x}",
                applied.sampling_frequency.hz().unwrap_or(0),
                applied.frame_duration.microseconds() as f32 / 1000.0,
                applied.octets_per_frame,
                applied.channel_allocation
            ));
        }

        let delay = qos.choose_presentation_delay(wanted_us);

        // Every ASE has to live with the same number, so the largest of the
        // per-ASE choices is the one that satisfies all of them.
        chosen = Some(chosen.map_or(delay, |current: u32| current.max(delay)));
    }

    chosen
}

/// A level for the app: null when there is nothing there to measure.
fn level(decibels: f32) -> Option<f32> {
    decibels.is_finite().then(|| (decibels * 10.0).round() / 10.0)
}

/// Locates the ASE control point, the only handle the stream ever writes to.
fn find_control_point(link: &mut olea_core::Link) -> Result<u16, String> {
    use olea_core::link::pacs_uuid;

    let services = link
        .discover_services()
        .map_err(|e| format!("discovery selhalo: {e}"))?;

    let ascs = services
        .iter()
        .find(|s| s.uuid.as_short() == Some(pacs_uuid::SERVICE_ASCS))
        .ok_or("device does not expose the ASCS service")?
        .clone();

    link.discover_characteristics(&ascs)
        .map_err(|e| format!("discovery selhalo: {e}"))?
        .into_iter()
        .find(|c| c.uuid.as_short() == Some(pacs_uuid::ASE_CONTROL_POINT))
        .map(|c| c.value_handle)
        .ok_or_else(|| "ASCS has no control point".to_string())
}

/// One way of asking the device to set the stream up.
///
/// Every field here is something the specification lets a host choose freely and
/// the device is allowed to react to. Which combination these headphones accept
/// is a property of their firmware, not something derivable - so it is measured.
struct Variant {
    name: &'static str,
    target_latency: u8,
    context: u16,
    phy: u8,
}

impl Variant {
    fn describe(&self) -> String {
        let latency = match self.target_latency {
            olea_core::bap::ascs::LATENCY_LOW => "low latency",
            olea_core::bap::ascs::LATENCY_HIGH_RELIABILITY => "high reliability",
            _ => "balanced",
        };
        let context = match self.context {
            olea_core::bap::ascs::CONTEXT_GAME => "hra",
            olea_core::bap::ascs::CONTEXT_CONVERSATIONAL => "hovor",
            _ => "media",
        };
        let phy = if self.phy == 0x01 { "1M" } else { "2M" };

        format!("{} ({latency}, {context}, {phy})", self.name)
    }
}

/// Most likely first: what the Windows trace shows, then progressively more
/// conservative shapes.
const VARIANTS: &[Variant] = {
    use olea_core::bap::ascs::*;

    &[
        Variant { name: "windows", target_latency: LATENCY_BALANCED, context: CONTEXT_MEDIA, phy: 0x02 },
        Variant { name: "spolehlivost", target_latency: LATENCY_HIGH_RELIABILITY, context: CONTEXT_MEDIA, phy: 0x02 },
        Variant { name: "hra", target_latency: LATENCY_LOW, context: CONTEXT_GAME, phy: 0x02 },
        Variant { name: "1M-vyvazena", target_latency: LATENCY_BALANCED, context: CONTEXT_MEDIA, phy: 0x01 },
        Variant { name: "1M-spolehlivost", target_latency: LATENCY_HIGH_RELIABILITY, context: CONTEXT_MEDIA, phy: 0x01 },
        Variant { name: "nizka-latence", target_latency: LATENCY_LOW, context: CONTEXT_MEDIA, phy: 0x02 },
    ]
};

/// The name of whatever produced the plan, preset or not.
struct PresetLabel(String);

fn parse_preset(name: &str) -> Option<Preset> {
    match name {
        "windows" => Some(Preset::WindowsDefault),
        "low-latency" => Some(Preset::LowLatency),
        "high-quality" => Some(Preset::HighQuality),
        "robust" => Some(Preset::Robust),
        _ => None,
    }
}

/// Forwards a progress report to the app as a line of text.
fn report(progress: Progress) {
    let text = match progress {
        Progress::AdapterReady { version, address } => format!("adapter {version}, {address}"),
        Progress::Connected { handle } => format!("connected, handle {handle:#06x}"),
        Progress::CapabilitiesRead { summary } => summary,
        Progress::StreamPlanned { summary } => summary,
        Progress::Streaming {
            frames,
            backlog,
            iso_failed,
            iso_sent,
            rssi,
            left_db,
            right_db,
            bass_db,
            mid_db,
            treble_db,
            delivered,
            ..
        } => {
            emit(json!({
                "event": "streaming",
                "frames": frames,
                "backlog": backlog,
                "failed": iso_failed,
                "sent": iso_sent,
                "rssi": rssi,
                "leftDb": level(left_db),
                "rightDb": level(right_db),
                "bassDb": level(bass_db),
                "midDb": level(mid_db),
                "trebleDb": level(treble_db),
                "delivered": delivered,
            }));
            return;
        }
        Progress::CaptureReady { device, format } => format!("zdroj zvuku: {device} - {format}"),
        Progress::Idle { after } => format!("silent for {} s, transmission paused", after.as_secs()),
        Progress::Resumed => "audio resumed".into(),
        Progress::Disconnected { reason } => format!(
            "peer disconnected: {} (code {reason:#04x})",
            olea_core::hci::disconnect_reason(reason)
        ),
        Progress::Stopped { reason } => reason,
        Progress::DeviceFound { .. } => return,
    };

    log(text);
}

/// Not used yet, but the app will need it: a deadline the UI can show.
#[allow(dead_code)]
fn remaining(deadline: Instant) -> u64 {
    deadline.saturating_duration_since(Instant::now()).as_secs()
}

#[allow(dead_code)]
fn unused(_: Sender<Value>) {}

