//! Settings that survive a restart, and what each one costs to change.
//!
//! Two things the configuration app needs and cannot work out for itself.
//!
//! The first is persistence: a preference set once should still be there after
//! the headphones drop out and reconnect, and after the app is closed and
//! opened again.
//!
//! The second is honesty about when a change takes effect. Some settings apply
//! to the next audio frame. Some are baked into the stream when it is set up,
//! so the headphones have to be reconnected. A stack that silently does nothing
//! until the next reconnect teaches people to distrust every control on the
//! page, so each setting carries its scope and the app can say so out loud.
//!
//! Deliberately a plain `key = value` text file. It can be read, diffed and
//! repaired in Notepad, which matters more here than compactness: this is a file
//! people will want to look at when something behaves oddly.

use std::collections::BTreeMap;
use std::time::Duration;

/// What has to happen before a change to a setting is actually heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyScope {
    /// Takes effect on the next audio frame.
    Immediately,
    /// Baked into the stream at setup: the headphones must be reconnected.
    OnReconnect,
    /// Changes how the adapter itself is driven: it must be restarted.
    OnAdapterRestart,
}

impl ApplyScope {
    /// A sentence the app can put under the control.
    pub fn explain(self) -> &'static str {
        match self {
            ApplyScope::Immediately => "applies immediately",
            ApplyScope::OnReconnect => "applies after reconnecting the headphones",
            ApplyScope::OnAdapterRestart => "applies after restarting the adapter",
        }
    }
}

/// One knob, with the cost of changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Knob {
    pub key: &'static str,
    pub scope: ApplyScope,
    /// What it does, for a tooltip.
    pub description: &'static str,
}

/// Every setting the stack understands, and what changing it costs.
///
/// The single source of truth for both the file format and the app: a knob that
/// is not here cannot be saved, which is what stops the two drifting apart.
pub const KNOBS: &[Knob] = &[
    Knob {
        key: "preset",
        scope: ApplyScope::OnReconnect,
        description: "LC3 preset: sample rate, frame duration, and bitrate",
    },
    Knob {
        key: "rate_hz",
        scope: ApplyScope::OnReconnect,
        description: "sample rate when the preset is custom (16000-48000)",
    },
    Knob {
        key: "frame_ms",
        scope: ApplyScope::OnReconnect,
        description: "frame duration in ms when the preset is custom (7.5 or 10)",
    },
    Knob {
        key: "octets",
        scope: ApplyScope::OnReconnect,
        description: "octets per frame, which determines bitrate in custom mode",
    },
    Knob {
        key: "phy",
        scope: ApplyScope::OnReconnect,
        description: "2M is faster and uses less airtime; 1M has longer range and tolerates interference better",
    },
    Knob {
        key: "retransmissions",
        scope: ApplyScope::OnReconnect,
        description: "maximum radio retransmissions; more is more robust but increases latency",
    },
    Knob {
        key: "max_latency_ms",
        scope: ApplyScope::OnReconnect,
        description: "maximum transport latency in ms",
    },
    Knob {
        key: "presentation_delay_ms",
        scope: ApplyScope::OnReconnect,
        description: "delay between receiving and presenting audio at the headphones",
    },
    Knob {
        key: "balance",
        scope: ApplyScope::Immediately,
        description: "left/right balance from -50 (only left) through 0 (even) to +50 (only right)",
    },
    Knob {
        key: "idle_link_latency",
        scope: ApplyScope::OnReconnect,
        description: "how many control-channel wake-ups the headphones may skip while audio plays; higher saves their battery, 0 leaves the link untouched",
    },
    Knob {
        key: "battery_poll_min",
        scope: ApplyScope::Immediately,
        description: "how often to ask the headphones for their battery level, on top of the updates they send by themselves",
    },
    Knob {
        key: "link_timeout_s",
        scope: ApplyScope::OnReconnect,
        description: "how long the link may go unheard before it counts as lost; longer survives walking out of range and back",
    },
    Knob {
        key: "swap_channels",
        scope: ApplyScope::Immediately,
        description: "swap left and right when the headphones play the channels in reverse",
    },
    Knob {
        key: "audio_mode",
        scope: ApplyScope::OnReconnect,
        description: "stereo with two channels, legacy compatibility, or mono with one channel",
    },
    Knob {
        key: "playback_source",
        scope: ApplyScope::OnReconnect,
        description: "Windows capture endpoint used as the music source",
    },
    Knob {
        key: "microphone_mode",
        scope: ApplyScope::OnReconnect,
        description: "headset microphone; disabled mode preserves the radio budget for playback",
    },
    Knob {
        key: "microphone_quality",
        scope: ApplyScope::OnReconnect,
        description: "headset microphone LC3 quality and bitrate",
    },
    Knob {
        key: "microphone_target",
        scope: ApplyScope::OnReconnect,
        description: "where microphone audio is delivered; VB-CABLE appears to applications as CABLE Output",
    },
    Knob {
        key: "monitor_enabled",
        scope: ApplyScope::Immediately,
        description: "listen to a selected microphone through the headphones; off by default",
    },
    Knob {
        key: "monitor_source",
        scope: ApplyScope::Immediately,
        description: "Windows microphone used for monitoring, or the headset microphone when connected",
    },
    Knob {
        key: "monitor_mode",
        scope: ApplyScope::Immediately,
        description: "mix the monitored microphone with captured audio or replace captured audio",
    },
    Knob {
        key: "monitor_gain",
        scope: ApplyScope::Immediately,
        description: "monitoring volume; 1.0 is unchanged",
    },
    Knob {
        key: "microphone_gain",
        scope: ApplyScope::Immediately,
        description: "gain applied to received microphone audio; 1.0 is unchanged",
    },
    Knob {
        key: "multipoint_yield_enabled",
        scope: ApplyScope::OnReconnect,
        description: "release the headphones after a period of silence so another device, such as a phone, can take them, and take them back automatically when this PC plays again. The headphones have to support multipoint themselves - this side of it cannot be added by software, and on a headset that only holds one connection at a time releasing simply means a short pause before playback resumes",
    },
    Knob {
        key: "multipoint_yield_s",
        scope: ApplyScope::OnReconnect,
        description: "how long silence must last before the headphones are released; too short and they change hands between tracks",
    },
    Knob {
        key: "link_metrics",
        scope: ApplyScope::OnReconnect,
        description: "how closely the radio is monitored while playing: none, signal only, or signal and packet loss; monitoring costs a little battery on both ends",
    },
    Knob {
        key: "diagnostics",
        scope: ApplyScope::OnReconnect,
        description: "print each stream state after channel establishment",
    },
    Knob {
        key: "device",
        // Read by the application when it decides what to connect to, not by
        // the stack while a stream is running, so there is nothing to reconnect
        // for.
        scope: ApplyScope::Immediately,
        description: "headphones to reach for first when connecting automatically; any paired device is used if these are not around",
    },
    Knob {
        key: "gain",
        scope: ApplyScope::Immediately,
        description: "gain before encoding; 0 is silent, 1.0 is unchanged, and 2.0 is maximum boost",
    },
    Knob {
        key: "idle_timeout_min",
        scope: ApplyScope::OnReconnect,
        description: "stop transmitting after this much silence, keeping the connection and both streams up; 0 never stops. Only takes effect when releasing for other devices is switched off, since that happens after seconds rather than minutes",
    },
    Knob {
        key: "reconnect_enabled",
        scope: ApplyScope::Immediately,
        description: "reconnect automatically after losing radio range",
    },
    Knob {
        key: "reconnect_interval_s",
        scope: ApplyScope::Immediately,
        description: "how often to retry after losing range",
    },
    Knob {
        key: "reconnect_window_min",
        scope: ApplyScope::Immediately,
        description: "how long to keep retrying before giving up; 0 means never give up",
    },
    Knob {
        key: "startup_reconnect_enabled",
        scope: ApplyScope::Immediately,
        description: "search for and connect headphones for three minutes after automatic startup",
    },
    Knob {
        key: "language",
        scope: ApplyScope::Immediately,
        description: "user interface language",
    },
    Knob {
        key: "run_in_background",
        scope: ApplyScope::Immediately,
        description: "closing the window keeps audio running and hides the app in the system tray",
    },
    Knob {
        key: "start_with_windows",
        scope: ApplyScope::Immediately,
        description: "start the application hidden after Windows sign-in",
    },
    Knob {
        key: "command_style",
        scope: ApplyScope::OnAdapterRestart,
        description: "USB recipient addressing used for HCI commands",
    },
];

pub fn knob(key: &str) -> Option<&'static Knob> {
    KNOBS.iter().find(|k| k.key == key)
}

/// Settings as stored, before they become a `SessionConfig`.
///
/// Values stay as text on purpose. A file written by a newer version keeps its
/// unknown keys instead of losing them on the next save, so switching versions
/// back and forth does not quietly delete someone's configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    values: BTreeMap<String, String>,
}

impl Settings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bumped whenever a default changes, or a setting disappears, in a way a
    /// stale file would undo.
    ///
    /// Settings are saved as absolute values, so a file written before a default
    /// moved keeps quietly overriding the new one - and the fix looks like it
    /// did nothing. An older file is replaced rather than merged, because a
    /// value the user never chose is not worth preserving at that price.
    ///
    /// Version 3 removed three experimental stereo settings. Their controls went
    /// but their saved values stayed, and kept configuring the wrong ASEs with
    /// the ears swapped - invisibly, because nothing in the app showed them any
    /// more. A setting with no control is a setting nobody can find.
    ///
    /// Version 6 removed `winning_variant`, written by the compatibility
    /// profile that tried six stream shapes in turn. The profile is gone; a file
    /// still naming a shape that no longer exists is only confusing.
    pub const VERSION: &'static str = "8";

    /// The defaults, which is also what the reset button restores.
    pub fn defaults() -> Self {
        let mut settings = Self::new();
        settings.set("version", Self::VERSION);
        settings.set("preset", "windows");
        settings.set("rate_hz", "48000");
        settings.set("frame_ms", "7.5");
        settings.set("octets", "90");
        settings.set("phy", "2M");
        settings.set("retransmissions", "13");
        settings.set("max_latency_ms", "75");
        settings.set("presentation_delay_ms", "40");
        settings.set("audio_mode", "stereo");
        settings.set("playback_source", "CABLE Output");
        // Playback quality first. A Source ASE consumes another CIS and radio
        // budget even before an application uses the microphone.
        settings.set("microphone_mode", "off");
        settings.set("microphone_quality", "balanced");
        // No implicit loop into the same cable used for music. The user can
        // select VB-CABLE explicitly (ideally a second A/B cable).
        settings.set("microphone_target", "vb-cable");
        settings.set("monitor_enabled", "false");
        settings.set("monitor_source", "default");
        settings.set("monitor_mode", "mix");
        settings.set("monitor_gain", "1.0");
        settings.set("microphone_gain", "1.0");
        settings.set("swap_channels", "false");
        settings.set("balance", "0");
        // Five seconds is what the trace shows Windows asking for, and it is
        // also the reason a walk to the kitchen ends the connection. Ten leaves
        // room to step out of range and back without the link being declared
        // dead, and costs nothing while the headphones are in range: the timeout
        // only decides how long silence is tolerated before giving up.
        settings.set("link_timeout_s", "10");
        // One small packet each way, four times an hour. Headsets are supposed
        // to notify when the level moves and most do, but a headset that
        // subscribes and then stays silent shows a battery frozen at whatever it
        // was on connect, and there is no way to tell those apart without
        // asking. Set to 0 to ask nothing and rely on notifications alone.
        settings.set("battery_poll_min", "15");
        // Off by default, deliberately. It is the largest saving available on
        // the headphones' side and it changes the timing of a link that this
        // stack has spent a long time getting to behave. Somebody who wants the
        // battery back can have it; nobody gets a connection quirk they did not
        // ask for.
        settings.set("idle_link_latency", "0");
        settings.set("diagnostics", "true");
        settings.set("link_metrics", "full");
        // Off. Releasing the endpoints during silence hands the headphones to
        // nobody when nothing else is asking for them - and it costs a full
        // stream rebuild to get them back, which is the expensive half of a
        // reconnect. Worth having for someone who really does switch between a
        // phone and this PC all day; not worth doing to everyone else by
        // default.
        settings.set("multipoint_yield_enabled", "false");
        settings.set("multipoint_yield_s", "5");
        settings.set("command_style", "class-device");
        settings.set("gain", "1.0");
        settings.set("idle_timeout_min", "5");
        settings.set("reconnect_enabled", "true");
        settings.set("reconnect_interval_s", "3");
        settings.set("reconnect_window_min", "15");
        settings.set("startup_reconnect_enabled", "true");
        settings.set("language", "en");
        settings.set("run_in_background", "false");
        settings.set("start_with_windows", "false");
        settings
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.values.insert(key.to_string(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "ne" => Some(false),
            _ => None,
        }
    }

    pub fn number(&self, key: &str) -> Option<f32> {
        self.get(key)?.parse().ok()
    }

    /// A duration in minutes, where zero means "no limit" rather than "instantly".
    pub fn minutes(&self, key: &str) -> Option<Option<Duration>> {
        let minutes = self.number(key)?;
        Some(if minutes <= 0.0 {
            None
        } else {
            Some(Duration::from_secs_f32(minutes * 60.0))
        })
    }

    /// Which of the changed settings need more than a moment to take effect.
    ///
    /// The app asks this after an edit so it can say what has to happen, instead
    /// of leaving the user to notice that nothing changed.
    pub fn scopes_touched_by(&self, previous: &Settings) -> Vec<&'static Knob> {
        let mut touched: Vec<&'static Knob> = KNOBS
            .iter()
            .filter(|k| k.scope != ApplyScope::Immediately)
            .filter(|k| self.get(k.key) != previous.get(k.key))
            .collect();

        touched.sort_by_key(|k| k.key);
        touched
    }

    /// Renders the file. Sorted, so a diff shows what changed and nothing else.
    pub fn to_text(&self) -> String {
        let mut text = String::from(
            "# OpenLEAudio settings\n# Delete this file to restore default values.\n\n",
        );

        for (key, value) in &self.values {
            if let Some(knob) = knob(key) {
                text.push_str(&format!("# {} ({})\n", knob.description, knob.scope.explain()));
            }
            text.push_str(&format!("{key} = {value}\n\n"));
        }

        text
    }

    /// Reads the file, ignoring anything it does not understand.
    ///
    /// A damaged line loses one setting, not the whole file. Refusing to load
    /// would leave someone with no way back except deleting the lot.
    pub fn from_text(text: &str) -> Self {
        let mut settings = Self::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let (key, value) = (key.trim(), value.trim());
            if !key.is_empty() {
                settings.set(key, value);
            }
        }

        settings
    }

    /// Loads from disk, falling back to the defaults if there is nothing there.
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let stored = Self::from_text(&text);

                if stored.get("version") != Some(Self::VERSION) {
                    return Self::defaults();
                }

                // Start from the defaults so a file written by an older version
                // still gets values for settings it never knew about.
                let mut settings = Self::defaults();
                for (key, value) in stored.values {
                    settings.set(&key, value);
                }
                settings
            }
            Err(_) => Self::defaults(),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_file_reads_back_the_same() {
        let mut settings = Settings::defaults();
        settings.set("device", "JBL Tune 780NC");

        assert_eq!(Settings::from_text(&settings.to_text()), settings);
    }

    #[test]
    fn a_file_from_an_older_version_is_replaced_rather_than_merged() {
        // Written when the default retransmission count was 2. Merging it would
        // keep overriding the value that replaced it.
        let stale = "retransmissions = 2
version = 1
";
        let path = std::env::temp_dir().join("olea-settings-version-test.txt");
        std::fs::write(&path, stale).unwrap();

        let loaded = Settings::load(&path);
        assert_eq!(loaded.number("retransmissions"), Some(13.0));
        assert_eq!(loaded.get("version"), Some(Settings::VERSION));

        // A removed setting must not survive either: it has no control any more,
        // so a value left behind can never be seen or corrected.
        assert_eq!(loaded.get("ase_pair"), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_key_survives_a_round_trip() {
        // Written by a newer version. Losing it on save would silently discard
        // configuration whenever someone switched back.
        let settings = Settings::from_text("future_knob = 7\ngain = 1.0\n");

        assert_eq!(settings.get("future_knob"), Some("7"));
        assert_eq!(Settings::from_text(&settings.to_text()).get("future_knob"), Some("7"));
    }

    #[test]
    fn a_damaged_line_costs_one_setting_not_the_file() {
        let settings = Settings::from_text("gain = 1.0\nthis line is nonsense\ndual_cis = true\n");

        assert_eq!(settings.number("gain"), Some(1.0));
        assert_eq!(settings.bool("dual_cis"), Some(true));
    }

    #[test]
    fn zero_minutes_means_no_limit_rather_than_no_time() {
        let settings = Settings::from_text("idle_timeout_min = 0\nreconnect_window_min = 2\n");

        assert_eq!(settings.minutes("idle_timeout_min"), Some(None));
        assert_eq!(
            settings.minutes("reconnect_window_min"),
            Some(Some(Duration::from_secs(120)))
        );
    }

    #[test]
    fn the_app_is_told_what_a_change_costs() {
        let before = Settings::defaults();

        let mut after = before.clone();
        after.set("gain", "0.5");
        let touched = after.scopes_touched_by(&before);
        assert!(touched.is_empty(), "live output gain must not request a reconnect");

        after.set("preset", "low-latency");
        let touched = after.scopes_touched_by(&before);
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].key, "preset");
        assert_eq!(touched[0].scope, ApplyScope::OnReconnect);
        assert_eq!(
            touched[0].scope.explain(),
            "applies after reconnecting the headphones"
        );
    }

    #[test]
    fn the_defaults_are_the_quality_ones() {
        let defaults = Settings::defaults();

        assert_eq!(defaults.number("gain"), Some(1.0), "no attenuation by default");
        assert_eq!(
            defaults.minutes("idle_timeout_min"),
            Some(Some(Duration::from_secs(300)))
        );
        assert_eq!(defaults.bool("reconnect_enabled"), Some(true));
        assert_eq!(defaults.bool("startup_reconnect_enabled"), Some(true));
        assert_eq!(
            defaults.minutes("reconnect_window_min"),
            Some(Some(Duration::from_secs(900)))
        );
        assert_eq!(defaults.number("reconnect_interval_s"), Some(3.0));
    }

    #[test]
    fn every_knob_says_what_changing_it_costs() {
        for k in KNOBS {
            assert!(!k.description.is_empty(), "{} has no description", k.key);
            assert!(knob(k.key).is_some());
        }
    }
}
