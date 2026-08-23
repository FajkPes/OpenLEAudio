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
    describe_capabilities, LiveAudioConfig, MetricsLevel, Progress, ReconnectPolicy, Session,
    SessionConfig,
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

    // Cuts a reconnect wait short. Raised for any command that means the user
    // has moved on, so nobody has to sit out a three-minute retry window.
    let interrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Flipped from this thread while audio is running, so the effect is heard
    // the moment the switch moves. Going through the command queue would mean
    // waiting for playback to end - which is exactly the thing being judged.
    let swap_channels = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live_audio = std::sync::Arc::new(std::sync::RwLock::new(LiveAudioConfig::default()));

    // Set from this thread for the same reason: while audio runs, the worker is
    // inside the playback loop and would not read a queued command until the
    // music stopped - which is not when anyone wants to know their battery
    // level.
    let battery_refresh = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Counts the commands that mean "stop what you were asked to do". A connect
    // that was queued before one of them is stale by the time the worker reaches
    // it, and running it anyway is why pressing Disconnect could be followed by
    // the headphones connecting again on their own.
    let cancel_epoch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // The session owns `Rc`s and must never leave the thread that built it.
    let worker = {
        let playing = playing.clone();
        let interrupt = interrupt.clone();
        let swap_channels = swap_channels.clone();
        let live_audio = live_audio.clone();
        let battery_refresh = battery_refresh.clone();
        let cancel_epoch = cancel_epoch.clone();
        std::thread::spawn(move || {
            run_worker(
                commands_rx,
                playing,
                interrupt,
                swap_channels,
                battery_refresh,
                cancel_epoch,
                live_audio,
            )
        })
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

                if name == "battery" {
                    battery_refresh.store(true, std::sync::atomic::Ordering::Relaxed);
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

                // Also anything that means the user has chosen something else to
                // do. A queued command cannot be read while the worker sits in a
                // reconnect wait, so the wait has to be told from here.
                if matches!(
                    name.as_str(),
                    "stop" | "quit" | "disconnect" | "connect" | "forget" | "scan" | "adapter"
                ) {
                    interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
                }

                // Every command carries the epoch it was accepted in. The
                // worker compares it against the current one and skips anything
                // the user has since changed their mind about.
                if matches!(name.as_str(), "disconnect" | "stop" | "quit" | "forget" | "adapter") {
                    cancel_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                command["epoch"] =
                    json!(cancel_epoch.load(std::sync::atomic::Ordering::Relaxed));

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
    interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
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
    live.balance = (settings.number("balance").unwrap_or(0.0) / 50.0).clamp(-1.0, 1.0);
}

/// Everything the worker remembers between commands.
struct Agent {
    session: Option<Session>,
    bonds: BondStore,
    settings: Settings,
    found: Vec<DiscoveredDevice>,
    /// Cleared from the reading thread to interrupt playback.
    playing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Raised by the reading thread when a command arrives that must cut a
    /// reconnect wait short. Separate from `playing` because the two answer
    /// different questions, and the old code used one flag for both: a
    /// reconnect wait therefore claimed audio was running, and a stray
    /// disconnect during setup silenced a stream nobody had started.
    interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Shared with the reading thread so it takes effect during playback.
    swap_channels: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Raised when the user clicks the battery indicator.
    ///
    /// The audio loop owns the link while music plays, so a battery read cannot
    /// be issued from here. It is left as a request the loop picks up on its
    /// next pass, which is at most one frame away.
    battery_refresh: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Raised by the reading thread for anything that supersedes a connection.
    ///
    /// The retry loop can be running for a quarter of an hour, and for most of
    /// that it is inside a radio window rather than in a wait that the interrupt
    /// flag can cut short. Comparing this at the top of every round is what
    /// makes Unpair actually stop the reconnect it was meant to stop, instead of
    /// the headphones pairing themselves again a few seconds later.
    cancel_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
    /// Set when playback ended because the headphones were handed back to
    /// whatever else wants them, rather than because anything went wrong.
    yielded: Option<(u32, u32)>,
    /// Whether the last attempt got as far as an open ACL connection.
    ///
    /// "The headphones are not in range" and "the headphones are here but
    /// refused the stream" both arrive as an error string, and retrying them at
    /// the same rate is wrong in both directions: the first wants the radio
    /// listening almost continuously, the second wants to stop hammering a
    /// device that is busy tidying up.
    reached_link: bool,
}

fn run_worker(
    commands: Receiver<Value>,
    playing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    swap_channels: std::sync::Arc<std::sync::atomic::AtomicBool>,
    battery_refresh: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cancel_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    live_audio: std::sync::Arc<std::sync::RwLock<LiveAudioConfig>>,
) {
    let mut agent = Agent {
        session: None,
        bonds: BondStore::load(&BondStore::default_path()),
        settings: Settings::load(&settings_path()),
        found: Vec::new(),
        playing,
        interrupt,
        swap_channels,
        battery_refresh,
        cancel_epoch: cancel_epoch.clone(),
        live_audio,
        link: None,
        connected: None,
        configured_ases: Vec::new(),
        capabilities: None,
        control_point: None,
        lost_reason: None,
        yielded: None,
        reached_link: false,
    };

    agent.swap_channels.store(
        agent.settings.bool("swap_channels").unwrap_or(false),
        std::sync::atomic::Ordering::Relaxed,
    );
    agent.sync_live_audio();

    emit(json!({ "event": "ready", "paired": agent.bonds.len() }));

    // Before the app asks for anything. Every one of these problems produces a
    // confusing failure later rather than an obvious one now, so the honest
    // moment to raise them is the first.
    let _ = agent.check_environment();

    while let Ok(command) = commands.recv() {
        let name = command.get("cmd").and_then(Value::as_str).unwrap_or("");

        // A connect that was asked for before the user pressed Disconnect is no
        // longer what they want. Acknowledged rather than silently dropped, so
        // the app clears its spinner instead of waiting for an answer that is
        // never coming.
        let stamped = command.get("epoch").and_then(Value::as_u64).unwrap_or(0);
        if name == "connect"
            && stamped < cancel_epoch.load(std::sync::atomic::Ordering::Relaxed)
        {
            log("connect request dropped: something else was asked for in the meantime");
            emit(json!({ "event": "done", "cmd": name }));
            continue;
        }

        let result = match name {
            "status" => agent.status(),
            "check" => agent.check_environment(),
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
            // Handled on the reading thread, which raised the flag the audio
            // loop reads. Nothing left to do here.
            "battery" => Ok(()),
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

/// The reconnect policy as it stands on disk right now.
///
/// Read rather than remembered because the worker thread is occupied for the
/// whole life of a connection: settings are persisted by the reading thread,
/// and this is the only way a change to them can reach a loop that is already
/// running.
fn reconnect_policy_from_disk() -> ReconnectPolicy {
    let settings = Settings::load(&settings_path());
    let defaults = ReconnectPolicy::default();

    ReconnectPolicy {
        enabled: settings.bool("reconnect_enabled").unwrap_or(defaults.enabled),
        interval: settings
            .number("reconnect_interval_s")
            .filter(|seconds| *seconds > 0.0)
            .map(Duration::from_secs_f32)
            .unwrap_or(defaults.interval),
        window: settings
            .minutes("reconnect_window_min")
            .unwrap_or(defaults.window),
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
    if key == "link_metrics"
        && MetricsLevel::from_setting(value).is_none()
    {
        return Err("unknown radio monitoring level".into());
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

    /// Reports everything standing between here and working audio.
    fn check_environment(&mut self) -> Result<(), String> {
        let issues = olea_core::environment::check(self.settings.get("playback_source"));

        // Reported as an event only. The check runs on startup and again
        // whenever the radio is asked for, so logging from here wrote the same
        // complaints out three times before anyone had done anything; the app
        // writes them once, when they change.
        emit(json!({
            "event": "environment",
            "issues": issues
                .iter()
                .map(|issue| json!({
                    "id": issue.id,
                    "severity": issue.severity.as_str(),
                    "summary": issue.summary,
                    "remedy": issue.remedy,
                    "setupAction": issue.setup_action,
                }))
                .collect::<Vec<_>>(),
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

        // Checked here rather than left to the transport. Opening an adapter
        // that is still on the Microsoft Bluetooth stack fails with "no
        // controller found", which reads as a missing or broken adapter and
        // sends people looking at their hardware. Say what is actually true and
        // which Setup step fixes it, and refuse rather than half start.
        let blocking: Vec<_> = olea_core::environment::check(self.settings.get("playback_source"))
            .into_iter()
            .filter(|issue| issue.severity == olea_core::environment::Severity::Blocking)
            .collect();

        if let Some(issue) = blocking.first() {
            let _ = self.check_environment();
            emit(json!({ "event": "adapter", "on": false }));
            return Err(format!("{} {}", issue.summary, issue.remedy));
        }

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
            prefer_single_cis: false,
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
            metrics: self.metrics_level(),
            multipoint_yield: self.multipoint_yield(),
            link_timeout: self.link_timeout(),
            battery_refresh: self.battery_refresh.clone(),
            battery_poll: self.battery_poll(),
            idle_link_latency: self.idle_link_latency(),
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

    /// Connects to a device, pairing first only when we do not already know it,
    /// and keeps it connected for as long as the reconnect policy allows.
    ///
    /// One loop covers three things that used to be handled separately and
    /// inconsistently: the first attempt, an attempt that fell over during
    /// stream setup, and a link that dropped after playing happily for an hour.
    /// The old code only retried the last of those - a setup failure returned an
    /// error straight to the app and automatic reconnect never ran at all, which
    /// is why reconnecting by hand worked and letting the stack do it did not.
    fn connect(&mut self, command: &Value) -> Result<(), String> {
        let address = command
            .get("address")
            .and_then(Value::as_str)
            .ok_or("address is missing")?
            .to_string();

        let device = self.target_device(&address)?;

        self.refresh_connection_config()?;
        // Proves the adapter is on before anything else runs; the policy itself
        // is read from disk at the top of every round.
        self.session.as_ref().ok_or("adapter is off")?;
        // Assigned at the top of every round, from disk. Declared here only so
        // it outlives one iteration.
        let mut policy: ReconnectPolicy;

        // Cleared here rather than by the reader thread: this is the command the
        // interrupt was raised for, and leaving it set would cancel the very
        // connection it asked for.
        self.interrupt.store(false, std::sync::atomic::Ordering::Relaxed);

        // The window is measured from the moment contact was lost, and it starts
        // again every time the headphones come back. Someone who wears them all
        // day should not find the stack has quietly stopped trying because of a
        // drop that happened this morning.
        let mut retrying_since = Instant::now();
        let mut attempt = 0u32;

        // What "current" means for this connection. Anything that raises it -
        // Disconnect, Unpair, turning the adapter off - ends this loop at the
        // next opportunity rather than at the end of the retry window.
        let started_in = self.cancel_epoch.load(std::sync::atomic::Ordering::Relaxed);

        // How long the controller is left listening for the peer to advertise.
        //
        // The first attempt gets a generous window: someone has just pressed
        // Connect and the headphones are in their hand. Every attempt after
        // that uses the retry interval instead, so the radio is listening for
        // almost the whole of it rather than sleeping through most of it and
        // then asking once. A connection then completes the moment the
        // headphones walk back into range, not up to an interval later.
        let first_window = Duration::from_secs(15);

        loop {
            if self.cancel_epoch.load(std::sync::atomic::Ordering::Relaxed) > started_in {
                log("connection attempt stopped: something else was asked for");
                emit(json!({ "event": "reconnect-stopped", "address": address }));
                return Ok(());
            }

            // Unpairing has to end this too. The bond is what a reconnect uses,
            // and without this check the loop simply pairs again from scratch -
            // which is exactly what the user had just undone.
            if attempt > 0 && !self.bonds.contains(&address) {
                log("automatic reconnect stopped: the device is no longer paired");
                emit(json!({ "event": "reconnect-stopped", "address": address }));
                return Ok(());
            }

            // Re-read every round. The worker is inside this loop for as long as
            // the headphones are connected, so a policy captured once would mean
            // "reconnect off" only taking effect after the next disconnection -
            // which is the one place it can no longer be used.
            policy = reconnect_policy_from_disk();

            attempt += 1;
            let already_paired = self.bonds.contains(&address);
            if attempt > 1 {
                emit(json!({ "event": "reconnecting", "address": address, "attempt": attempt }));
            }
            let window = if attempt == 1 {
                first_window
            } else {
                policy
                    .interval
                    .clamp(Duration::from_millis(1_500), Duration::from_secs(20))
            };

            match self.connect_device(&address, &device, already_paired, window) {
                Ok(()) => {
                    // Playback ended. Either the user asked for that, or the
                    // link went away underneath it.
                    match self.lost_reason.take() {
                        Some(reason) if ReconnectPolicy::worth_reconnecting(reason) => {
                            log(format!(
                                "connection lost: {}",
                                olea_core::hci::disconnect_reason(reason)
                            ));
                            // Contact existed until a moment ago, so the clock
                            // for giving up starts now.
                            retrying_since = Instant::now();
                            attempt = 0;
                        }
                        Some(reason) => {
                            log(format!(
                                "connection ended: {} - not reconnecting",
                                olea_core::hci::disconnect_reason(reason)
                            ));
                            return Ok(());
                        }
                        // A clean stop asked for by the user.
                        None => return Ok(()),
                    }
                }
                Err(error) => {
                    self.lost_reason = None;
                    // Whatever went wrong, the peer may be left holding half a
                    // connection. Hand everything back before asking again, so
                    // a retry is as clean as pressing Disconnect and Connect.
                    self.reset_after_failure();

                    // The very first attempt gets one immediate retry and then
                    // reports. Two clean attempts cost about a second between
                    // them and cover the common case of the peer still tidying
                    // up the previous session; anything past that is a real
                    // failure the user needs to see rather than watch a spinner
                    // for.
                    if attempt == 1 {
                        log(format!("attempt failed: {error}; trying once more"));
                        if !self.wait_for(Duration::from_millis(600)) {
                            return Ok(());
                        }
                        continue;
                    }

                    if attempt == 2 && !policy.enabled {
                        return Err(error);
                    }

                    if attempt == 2 {
                        // Automatic reconnect takes over from here. Say so once,
                        // so the app can show "reconnecting" instead of an error
                        // that is about to be retried anyway.
                        log(format!("attempt failed: {error}"));
                        emit(json!({
                            "event": "reconnect-started",
                            "address": address,
                            "intervalMs": policy.interval.as_millis() as u64,
                        }));
                        retrying_since = Instant::now();
                    } else {
                        log(format!("attempt failed: {error}"));
                    }
                }
            }

            if !policy.enabled {
                return Ok(());
            }

            if !policy.should_retry(retrying_since.elapsed()) {
                log("automatic reconnect ended: retry window expired");
                emit(json!({ "event": "reconnect-stopped", "address": address }));
                return Ok(());
            }

            // A peer that never answered has already cost a full listening
            // window, so the next attempt starts almost at once - the interval
            // has effectively already elapsed with the radio doing something
            // useful. A peer that answered and then refused the stream is a
            // different matter: it is busy, and asking again immediately is how
            // one bad attempt becomes a run of them.
            let pause = if self.reached_link {
                policy.interval
            } else {
                Duration::from_millis(400)
            };

            if pause >= Duration::from_secs(1) {
                log(format!("next attempt in {:.1} s", pause.as_secs_f32()));
            }

            if !self.wait_for(pause) {
                log("automatic reconnect canceled");
                emit(json!({ "event": "reconnect-stopped", "address": address }));
                return Ok(());
            }
        }
    }

    /// The device to aim at, from this session's scan or from what we already know.
    ///
    /// A reconnect must not depend on a scan having happened. The bond stores
    /// the address, and a bonded peer can be connected to directly - requiring a
    /// fresh scan first is both slower and a reason for automatic reconnect to
    /// fail with "start a scan first" at the exact moment nobody is watching.
    fn target_device(&self, address: &str) -> Result<DiscoveredDevice, String> {
        if let Some(device) = self
            .found
            .iter()
            .find(|d| d.address.to_string().eq_ignore_ascii_case(address))
        {
            return Ok(device.clone());
        }

        let bond = self
            .bonds
            .get(address)
            .ok_or("device is not in the scan results; start a scan first")?;

        let parsed = olea_core::hci::BdAddr::parse(address)
            .ok_or("address could not be read")?;

        Ok(DiscoveredDevice {
            address: parsed,
            address_type: bond.address_type,
            name: Some(bond.name.clone()),
            rssi: 0,
            appearance: None,
            service_uuids: Vec::new(),
        })
    }

    /// Waits, letting the reader thread cut it short. False means "give up".
    ///
    /// The old code borrowed the playback flag for this. That flag means
    /// "audio is running", and overloading it meant a reconnect wait looked like
    /// playback to everything else that consults it - including the shutdown
    /// path, which then waited for audio that was never going to start.
    fn wait_for(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.interrupt.load(std::sync::atomic::Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.interrupt.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Puts our side back to nothing after a failed attempt.
    ///
    /// Everything here is best effort by design: the peer may already be gone,
    /// and an error while tidying up must not replace the error worth reporting.
    /// What matters is that the next attempt starts from the same state a fresh
    /// launch would - which is precisely why connecting by hand was more
    /// reliable than the automatic path that skipped this.
    fn reset_after_failure(&mut self) {
        let handle = self.connected.take().map(|(_, handle)| handle);

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

        self.configured_ases.clear();
        self.link = None;
        self.capabilities = None;
        self.control_point = None;

        if let (Some(session), Some(handle)) = (self.session.as_mut(), handle) {
            session.disconnect(handle);
        }
    }

    /// Applies settings whose documented scope is the next connection.
    fn refresh_connection_config(&mut self) -> Result<(), String> {
        self.settings = Settings::load(&settings_path());
        self.sync_live_audio();
        let defaults = SessionConfig::default();
        // Read before the session is borrowed mutably: both want `self`.
        let metrics = self.metrics_level();
        let multipoint_yield = self.multipoint_yield();
        let link_timeout = self.link_timeout();
        let battery_poll = self.battery_poll();
        let idle_link_latency = self.idle_link_latency();
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
        config.metrics = metrics;
        config.multipoint_yield = multipoint_yield;
        config.link_timeout = link_timeout;
        config.battery_refresh = self.battery_refresh.clone();
        config.battery_poll = battery_poll;
        config.idle_link_latency = idle_link_latency;
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

    /// How long the link may go unheard before it counts as lost.
    ///
    /// Clamped to what the specification allows rather than trusted: the file is
    /// editable by hand, and a controller answers an impossible value with
    /// "invalid HCI parameters" from a command that names nothing.
    fn link_timeout(&self) -> Duration {
        let seconds = self.settings.number("link_timeout_s").unwrap_or(10.0);
        Duration::from_secs_f32(seconds.clamp(2.0, 30.0))
    }

    /// How often to ask for the battery level, or `None` for "never ask".
    fn battery_poll(&self) -> Option<Duration> {
        let minutes = self.settings.number("battery_poll_min").unwrap_or(15.0);
        (minutes >= 1.0).then(|| Duration::from_secs_f32(minutes.clamp(1.0, 120.0) * 60.0))
    }

    /// How many control-channel wake-ups the headphones may skip while playing.
    fn idle_link_latency(&self) -> u16 {
        self.settings
            .number("idle_link_latency")
            .unwrap_or(0.0)
            .clamp(0.0, 30.0) as u16
    }

    fn metrics_level(&self) -> MetricsLevel {
        self.settings
            .get("link_metrics")
            .and_then(MetricsLevel::from_setting)
            .unwrap_or_default()
    }

    /// How long silence lasts before the headphones are handed back, if ever.
    fn multipoint_yield(&self) -> Option<Duration> {
        if !self.settings.bool("multipoint_yield_enabled").unwrap_or(false) {
            return None;
        }

        // A floor rather than a raw value. Yielding after a second would hand
        // the headphones away in the gap between two tracks and then spend the
        // next second taking them back, which is audible and pointless.
        let seconds = self.settings.number("multipoint_yield_s").unwrap_or(5.0);
        Some(Duration::from_secs_f32(seconds.max(2.0)))
    }

    fn sync_live_audio(&self) {
        let Ok(mut live) = self.live_audio.write() else { return };
        live.monitor_enabled = self.settings.bool("monitor_enabled").unwrap_or(false);
        live.monitor_source = self.settings.get("monitor_source").unwrap_or("default").to_owned();
        live.monitor_replace = self.settings.get("monitor_mode").unwrap_or("mix") == "replace";
        live.monitor_gain = self.settings.number("monitor_gain").unwrap_or(1.0);
        live.output_gain = self.settings.number("gain").unwrap_or(1.0);
        live.microphone_gain = self.settings.number("microphone_gain").unwrap_or(1.0);
        live.balance = (self.settings.number("balance").unwrap_or(0.0) / 50.0).clamp(-1.0, 1.0);
    }

    /// Establishes, configures and plays one physical connection attempt.
    fn connect_device(
        &mut self,
        address: &str,
        device: &DiscoveredDevice,
        already_paired: bool,
        radio_window: Duration,
    ) -> Result<(), String> {
        self.lost_reason = None;
        self.reached_link = false;
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
            .connect_within(&device, radio_window, |p| report(p))
            .map_err(|e| format!("connection failed: {e}"))?;

        // Past this point a failure is about configuring a peer we can talk to,
        // not about a peer that is not there. The two deserve very different
        // retry behaviour and this is the only place that can tell them apart.
        self.reached_link = true;

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

        // Set when the saved key turned out to be dead, so it can be forgotten
        // once the borrow of the session is over.
        let mut stale_bond = false;
        let key_result = if let Some(key) = stored_key {
            match session.resume_encryption(handle, &key) {
                Ok(()) => {
                    log("restored an encrypted connection from the saved bond");
                    Ok(key)
                }
                Err(error) => {
                    // The saved key is no longer the one the headphones hold.
                    // That happens whenever they are paired to something else in
                    // the meantime, or reset - it is ordinary, not damage - and
                    // the only way out of it is a fresh key exchange. Telling
                    // the user to unpair by hand was asking them to perform the
                    // recovery the stack can perform itself, and until they did
                    // every reconnect failed identically.
                    log(format!(
                        "the saved key was refused ({error}); pairing again from scratch"
                    ));
                    stale_bond = true;
                    session
                        .pair(&mut link, handle, device)
                        .map_err(|e| format!("pairing again failed: {e}"))
                }
            }
        } else {
            session.pair(&mut link, handle, device)
                .map_err(|e| format!("pairing failed: {e}"))
        };
        if stale_bond {
            self.bonds.remove(address);
            let _ = self.bonds.save(&BondStore::default_path());
        }

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
            // From the advertisement that got us here. Without it a later
            // reconnect has to guess, and guessing wrong never completes.
            address_type: device.address_type,
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
            // The union of every PAC record, so the settings page can colour a
            // value the moment it is chosen instead of accepting it and failing
            // on the next connection. A device publishes one record per sample
            // rate, so any single record understates what it can do.
            "sink": codec_envelope(&capabilities.sink_records),
            "source": codec_envelope(&capabilities.source_records),
        }));

        // Whether anyone else has these headphones right now, read the only way
        // the specification provides. A headset busy with a phone otherwise
        // fails somewhere in stream setup with a message about ASEs, which
        // reads as broken hardware rather than as the entirely normal situation
        // it is.
        let availability = olea_core::multipoint::availability(&capabilities);
        log(format!("multipoint: {}", availability.explain()));
        if let Some(contexts) = capabilities.available_contexts {
            log(format!(
                "  available now: {} (supported: {})",
                olea_core::multipoint::describe_contexts(contexts),
                capabilities
                    .supported_contexts
                    .map(olea_core::multipoint::describe_contexts)
                    .unwrap_or_else(|| "not published".into()),
            ));
        }
        emit(json!({
            "event": "availability",
            "address": address,
            "state": availability.as_str(),
            "detail": availability.explain(),
        }));

        if !availability.worth_attempting() {
            session.disconnect(handle);
            return Err(format!("{}", availability.explain()));
        }

        // The raw records, byte for byte. Everything about this device has been
        // read through one parser, and "stereo in one stream: no" - the single
        // fact that sent the whole design down the two-channel path - has never
        // been checked against the bytes it came from. If that reading is wrong,
        // so is every decision built on it.
        for (index, record) in capabilities.sink_records.iter().enumerate() {
            // Only with debug on. These four lines of hex are here to check a
            // parser against the bytes it came from - worth every character when
            // that is the question, and pure noise on the twentieth reconnect of
            // an ordinary evening.
            if !olea_core::trace::is_enabled() {
                break;
            }
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

        // Read once here, then updated by notification. Nothing polls it: the
        // device tells us when the level moves, which costs no airtime in
        // between and is the whole reason to subscribe rather than ask.
        match session.attach_batteries(&mut link) {
            Ok(levels) if levels.is_empty() => {
                log("device does not report its battery over GATT")
            }
            // The number goes to the indicator, not to the console. It is a
            // value that belongs on a display: it changes slowly, it is always
            // visible up there, and printing it on every connect only pushed
            // the lines that do need reading further up.
            Ok(levels) => {
                emit(json!({ "event": "battery", "address": address, "levels": levels }));
            }
            Err(e) => log(format!("battery level could not be read: {e}")),
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
        loop {
            let (_, handle) = self.connected.clone().ok_or("no device is connected")?;
            let mut link = self.link.take().ok_or("connection is not open")?;

            self.yielded = None;
            let result = self.play_on(&mut link, handle);
            // A controller-reported disconnect makes this Link permanently stale.
            // Keeping it used to leave both the UI and the next attempt believing a
            // dead ACL connection was still usable.
            if self.connected.is_some() {
                self.link = Some(link);
            }

            // Anything other than a deliberate hand-back is the end of playback.
            let Some((sample_rate, frame_us)) = self.yielded.take() else {
                return result;
            };
            if result.is_err() || self.connected.is_none() {
                return result;
            }

            // Nothing is configured on the headphones now and nothing is being
            // transmitted. Waiting here is what lets a phone have them, and the
            // moment this PC makes a sound again the loop builds the whole
            // stream back - configure, QoS, enable - which is also the
            // specified way to take a multipoint device over.
            let heard = {
                let playing = self.playing.clone();
                let interrupt = self.interrupt.clone();
                // Raised for the duration of the wait, because the reading
                // thread clears it for Disconnect, adapter-off and quit - and
                // this wait has no other way to hear about any of them. It is
                // cleared again below whichever way the wait ends.
                playing.store(true, std::sync::atomic::Ordering::Relaxed);
                let session = self.session.as_mut().ok_or("adapter is off")?;
                let heard = session
                    .wait_for_sound(handle, sample_rate, frame_us, || {
                        interrupt.load(std::sync::atomic::Ordering::Relaxed)
                            || !playing.load(std::sync::atomic::Ordering::Relaxed)
                    })
                    .map_err(|e| format!("waiting for audio failed: {e}"));
                playing.store(false, std::sync::atomic::Ordering::Relaxed);
                heard?
            };

            if !heard {
                return Ok(());
            }

            log("audio is back: taking the headphones again");
            emit(json!({ "event": "reclaiming" }));
        }
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
        let wanted = ask_device_for_qos(link, &sink_handles, plan.qos.presentation_delay_us);

        if let Some(delay) = wanted.presentation_delay_us {
            if delay != plan.qos.presentation_delay_us {
                log(format!(
                    "headphones request {} ms delay instead of {} ms; using their value",
                    delay / 1000,
                    plan.qos.presentation_delay_us / 1000
                ));
                plan.qos.presentation_delay_us = delay;
            }
        }

        // The radio numbers, taken from the device for the same reason as the
        // delay: they are what its firmware was tuned around, and the preset's
        // values are what one particular headset was observed to be sent by the
        // Windows driver. On that headset the two agree exactly, so this changes
        // nothing there - and on anything else it is the difference between
        // asking for what the device wants and asking for what a different
        // device wanted.
        //
        // Not applied to the custom preset. There the user is driving, and a
        // control that silently overrides itself is worse than no control.
        if chosen != "custom" {
            if let Some(rtn) = wanted.retransmissions {
                if rtn != plan.qos.retransmission_number {
                    log(format!(
                        "headphones recommend {rtn} retransmissions instead of {}; using theirs",
                        plan.qos.retransmission_number
                    ));
                    plan.qos.retransmission_number = rtn;
                }
            }

            // A ceiling, not a preference: asking for longer than the server
            // supports is a configuration it can only refuse.
            if let Some(limit) = wanted.max_transport_latency_ms {
                if plan.qos.max_transport_latency_ms > limit {
                    log(format!(
                        "headphones support at most {limit} ms transport latency, not {}; using theirs",
                        plan.qos.max_transport_latency_ms
                    ));
                    plan.qos.max_transport_latency_ms = limit;
                }
            }

            // Only when 2M is genuinely absent from what the device offers.
            // Every LE Audio device is expected to do 2M, so this is a fallback
            // for hardware that says otherwise rather than a routine choice.
            if let Some(phy) = wanted.phy_preference {
                if phy & 0x02 == 0 && phy & 0x01 != 0 && plan.qos.phy != 0x01 {
                    log("headphones do not offer the 2M radio; falling back to 1M");
                    plan.qos.phy = 0x01;
                }
            }
        }

        {
            let session = self.session.as_mut().ok_or("adapter is off")?;
            session
                .write_ascs(link, control_point, &plan.qos_and_enable_writes())
                .map_err(|e| format!("konfigurace QoS selhala: {e}"))?;
        }

        // One shape, driven by what this device published, and nothing else.
        //
        // There used to be six: a compatibility profile that walked through
        // other latencies, contexts and PHYs whenever the first attempt did not
        // establish both channels. It cost up to half a minute, and every shape
        // it tried left the headphones configured for something the user never
        // asked for - a game context at 1M, say - so a connection that
        // "succeeded" could sound nothing like the preset on screen. Worse, the
        // winning shape was remembered and used first next time, so the settings
        // page and the actual stream drifted permanently apart.
        //
        // A device that refuses the specification-driven configuration is
        // telling us something real. Retrying the whole connection cleanly is
        // both faster and honest; guessing at parameters is not.
        plan.target_latency = olea_core::bap::ascs::LATENCY_BALANCED;
        plan.context = olea_core::bap::ascs::CONTEXT_MEDIA;

        let cis = {
            let session = self.session.as_mut().ok_or("adapter is off")?;
            match session.establish_isochronous(&plan, handle) {
                Ok(outcome) if outcome.complete() => {
                    log(format!("channels established: {}", outcome.describe()));
                    outcome.established
                }
                Ok(outcome) => {
                    // A partial result is silence with extra steps on a device
                    // that will not start either stream without both, and the
                    // survivors hold the group un-removable. Hand them back so
                    // the next attempt starts from nothing.
                    let survivors = outcome.established.clone();
                    session.release_cis(&survivors);
                    session.release_isochronous(&plan);
                    return Err(format!(
                        "headphones did not establish every channel ({}); retrying the connection",
                        outcome.describe()
                    ));
                }
                Err(e) => {
                    session.release_isochronous(&plan);
                    return Err(format!("isochronous channels could not be established: {e}"));
                }
            }
        };

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
        let mut yielded = false;
        let outcome = session.run_audio(
            &plan,
            &cis,
            Some(handle),
            |progress| {
                match &progress {
                    Progress::Disconnected { reason } => lost_reason = Some(*reason),
                    Progress::Yielded { .. } => yielded = true,
                    _ => {}
                }
                report(progress);
            },
            || !playing.load(std::sync::atomic::Ordering::Relaxed),
        );

        playing.store(false, std::sync::atomic::Ordering::Relaxed);

        // Handing the headphones back is the whole point, so it has to be a
        // real Release rather than simply stopping the audio. An endpoint left
        // configured keeps the headset ours as far as it is concerned, and the
        // phone that wanted it gets nothing.
        if yielded {
            let ases = std::mem::take(&mut self.configured_ases);
            if !ases.is_empty() {
                session.release_streams(link, control_point, &ases);
            }
            self.yielded = Some((
                plan.codec.sampling_frequency.hz().unwrap_or(48_000),
                plan.qos.sdu_interval_us,
            ));
            log("silence: headphones released, another device may take them");
            emit(json!({ "event": "yielded" }));
        }

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
/// What the headphones published about the stream they have just configured.
///
/// Every field is what the device asked for, never what we hoped for. The delay
/// was already being read; the rest was read, printed and then thrown away,
/// which is the worst of both - the log showed the device stating a preference
/// and the stack sending something else.
#[derive(Debug, Default, Clone, Copy)]
struct DevicePreference {
    presentation_delay_us: Option<u32>,
    /// The retransmission count the server recommends.
    retransmissions: Option<u8>,
    /// The longest transport latency the server supports. Ours must not exceed it.
    max_transport_latency_ms: Option<u16>,
    /// Bit 0 is 1M, bit 1 is 2M. Only consulted when 2M is absent.
    phy_preference: Option<u8>,
}

fn ask_device_for_qos(
    link: &mut olea_core::Link,
    sink_handles: &[(u8, u16)],
    wanted_us: u32,
) -> DevicePreference {
    use olea_core::bap::ase;

    let mut preference = DevicePreference::default();
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

        // The retransmission count is a recommendation, so the highest any ASE
        // asks for is the one that keeps them all happy. Transport latency is a
        // ceiling, so the lowest is the only one every ASE can meet.
        if qos.retransmission_preference > 0 {
            let rtn = qos.retransmission_preference;
            preference.retransmissions =
                Some(preference.retransmissions.map_or(rtn, |current: u8| current.max(rtn)));
        }
        if qos.max_transport_latency_ms > 0 {
            let latency = qos.max_transport_latency_ms;
            preference.max_transport_latency_ms = Some(
                preference
                    .max_transport_latency_ms
                    .map_or(latency, |current: u16| current.min(latency)),
            );
        }
        if qos.phy_preference != 0 {
            let phy = qos.phy_preference;
            preference.phy_preference =
                Some(preference.phy_preference.map_or(phy, |current: u8| current & phy));
        }
    }

    preference.presentation_delay_us = chosen;
    preference
}

/// Everything the device said it can accept, flattened into one shape.
///
/// Each PAC record covers one part of what a device supports, and the sum of
/// them is what it will actually take. Reporting the records individually would
/// make the app do this join, and it would then have to know as much about BAP
/// as the stack does.
fn codec_envelope(records: &[olea_core::PacRecord]) -> Value {
    let mut rates: Vec<u32> = Vec::new();
    let mut channels: Vec<u8> = Vec::new();
    let mut frame_ms: Vec<f32> = Vec::new();
    let mut min_octets: Option<u16> = None;
    let mut max_octets: Option<u16> = None;
    let mut max_frames_per_sdu = 0u8;

    for record in records.iter().filter(|record| record.is_lc3()) {
        let caps = &record.capabilities;

        for frequency in &caps.sampling_frequencies {
            if let Some(hz) = frequency.hz() {
                if !rates.contains(&hz) {
                    rates.push(hz);
                }
            }
        }
        for count in &caps.channel_counts {
            if !channels.contains(count) {
                channels.push(*count);
            }
        }
        if caps.supports_7_5ms && !frame_ms.contains(&7.5) {
            frame_ms.push(7.5);
        }
        if caps.supports_10ms && !frame_ms.contains(&10.0) {
            frame_ms.push(10.0);
        }

        // The widest range any record allows. A value outside every record is
        // certain to be refused; one inside a record the device only offers at
        // another sample rate is a maybe, and the app shows those differently.
        if let Some(min) = caps.min_octets_per_frame {
            min_octets = Some(min_octets.map_or(min, |current: u16| current.min(min)));
        }
        if let Some(max) = caps.max_octets_per_frame {
            max_octets = Some(max_octets.map_or(max, |current: u16| current.max(max)));
        }
        max_frames_per_sdu = max_frames_per_sdu.max(caps.max_frames_per_sdu);
    }

    rates.sort_unstable();
    channels.sort_unstable();
    frame_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    json!({
        "rates": rates,
        "frameMs": frame_ms,
        "channels": channels,
        "octetsMin": min_octets,
        "octetsMax": max_octets,
        "maxFramesPerSdu": max_frames_per_sdu,
    })
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
        // Not logged here. The same text arrives again a moment later on the
        // "capabilities" event, which the app prints, and two identical
        // paragraphs at the top of every connection made the console look like
        // it was retrying something.
        Progress::CapabilitiesRead { summary: _ } => return,
        Progress::StreamPlanned { summary } => summary,
        Progress::Streaming {
            frames,
            backlog,
            iso_failed,
            iso_sent,
            underruns,
            rssi,
            left_db,
            right_db,
            bass_db,
            mid_db,
            treble_db,
            delivered,
            quality,
            ..
        } => {
            // Cumulative counters straight from the controller. The app turns
            // them into a rate; sending a rate from here would mean deciding the
            // window on its behalf, and the window is a display choice.
            let radio: Vec<Value> = quality
                .iter()
                .map(|q| {
                    json!({
                        "handle": q.handle,
                        "lost": q.lost_packets(),
                        "unacked": q.tx_unacked_packets,
                        "flushed": q.tx_flushed_packets,
                        "retransmitted": q.retransmitted_packets,
                        "crcErrors": q.crc_error_packets,
                        "unreceived": q.rx_unreceived_packets,
                    })
                })
                .collect();

            emit(json!({
                "event": "streaming",
                "frames": frames,
                "backlog": backlog,
                "failed": iso_failed,
                "sent": iso_sent,
                "underruns": underruns,
                "rssi": rssi,
                "leftDb": level(left_db),
                "rightDb": level(right_db),
                "bassDb": level(bass_db),
                "midDb": level(mid_db),
                "trebleDb": level(treble_db),
                "delivered": delivered,
                "radio": radio,
            }));
            return;
        }
        Progress::Battery { levels } => {
            emit(json!({ "event": "battery", "levels": levels }));
            return;
        }
        Progress::BatteryAsked { reason } => format!(
            "battery level requested from the headphones ({reason})"
        ),
        Progress::CaptureReady { device, format } => format!("zdroj zvuku: {device} - {format}"),
        Progress::Idle { after } => format!("silent for {} s, transmission paused", after.as_secs()),
        Progress::Yielded { after } => format!(
            "silent for {} s, handing the headphones back",
            after.as_secs()
        ),
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

