//! What has to be true before any of this can work, checked out loud.
//!
//! Three things have to be in place before a single LC3 frame can reach a pair
//! of headphones: the adapter has to belong to this stack rather than to
//! Windows, a virtual cable has to exist, and that cable has to be both
//! configured and actually receiving the music.
//!
//! Every one of them can be wrong in a way that produces no error at all. An
//! adapter still bound to the Microsoft Bluetooth stack simply is not found. A
//! VB-CABLE left at its default 44.1 kHz is present in every device list and
//! quietly costs a resample on every frame. A cable that is not the default
//! playback device works perfectly and carries silence, because the music is
//! going somewhere else.
//!
//! Each of those has, at some point, been diagnosed as a codec fault, a radio
//! fault, or broken headphones. So they are checked explicitly, named in plain
//! language, and each one says what to do about it.

/// How badly a problem stands in the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Nothing will work until this is fixed.
    Blocking,
    /// It will run, but not the way the user expects.
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Blocking => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One thing that is wrong, in terms a person can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Stable identifier, so the app can decide how to present it without
    /// matching on prose that may be translated.
    pub id: &'static str,
    pub severity: Severity,
    /// What is wrong.
    pub summary: String,
    /// What to do about it. Always concrete: which button, which step.
    pub remedy: String,
    /// The Setup step that fixes it, when there is one.
    pub setup_action: Option<&'static str>,
}

impl Issue {
    fn new(
        id: &'static str,
        severity: Severity,
        summary: impl Into<String>,
        remedy: impl Into<String>,
        setup_action: Option<&'static str>,
    ) -> Self {
        Self {
            id,
            severity,
            summary: summary.into(),
            remedy: remedy.into(),
            setup_action,
        }
    }
}

/// The rate the codec runs at by default, and therefore the rate that costs
/// nothing to carry.
///
/// A cable set to anything else still works - the capture side converts - but
/// that conversion happens on every frame, inside the audio deadline, for no
/// benefit whatsoever. Matching it is free and strictly better.
pub const REQUIRED_CABLE_RATE: u32 = 48_000;

/// Everything that is wrong right now, most serious first.
///
/// An empty result means the machine is ready. Nothing here opens a stream or
/// claims a device: this runs while audio may already be playing, and a check
/// that disturbs what it is checking is worse than no check.
#[cfg(windows)]
pub fn check(playback_source: Option<&str>) -> Vec<Issue> {
    let mut issues = Vec::new();

    issues.extend(check_adapter());
    issues.extend(check_cable(playback_source));

    issues.sort_by_key(|issue| match issue.severity {
        Severity::Blocking => 0,
        Severity::Warning => 1,
    });

    issues
}

/// Whether an adapter belonging to this stack can be found at all.
///
/// Deliberately does not open it. Opening would take the device away from a
/// session that may already be running, and the question here is only whether
/// our driver is bound to anything.
#[cfg(windows)]
fn check_adapter() -> Option<Issue> {
    match crate::winusb::find_interface_path(crate::winusb::OLEA_INTERFACE_GUID) {
        Ok(_) => None,
        Err(_) => Some(Issue::new(
            "adapter-not-bound",
            Severity::Blocking,
            "No Bluetooth adapter is bound to the OpenLEAudio stack.",
            "The adapter is most likely still running on the Microsoft Bluetooth stack, \
             where LE Audio streams cannot be configured. Open Setup, step 3, choose the \
             adapter and press \"Switch to our stack\".",
            Some("adapter-bind"),
        )),
    }
}

/// Whether the virtual cable exists, is configured, and is being used.
#[cfg(windows)]
fn check_cable(playback_source: Option<&str>) -> Vec<Issue> {
    let mut issues = Vec::new();

    let capture = crate::audio::list_capture_devices().unwrap_or_default();
    let render = crate::audio::list_render_devices().unwrap_or_default();

    let cable_out = capture.iter().find(|device| {
        device.name.to_lowercase().contains("cable output") && !device.is_multichannel_cable()
    });
    let cable_in = render.iter().find(|device| {
        device.name.to_lowercase().contains("cable input") && !device.is_multichannel_cable()
    });

    let (Some(cable_out), Some(cable_in)) = (cable_out, cable_in) else {
        issues.push(Issue::new(
            "vbcable-missing",
            Severity::Blocking,
            "VB-CABLE is not installed.",
            "OpenLEAudio reads the music Windows is playing from a virtual cable; without \
             one there is nothing to encode. Open Setup, step 2, and press \
             \"Install VB-CABLE\".",
            Some("vbcable-install"),
        ));
        return issues;
    };

    // Installed but never configured. This is the case that looks like nothing
    // is wrong, because every list shows the cable and every button works.
    let formats = [
        ("CABLE Output", crate::audio::endpoint_format(&cable_out.id)),
        ("CABLE Input", crate::audio::endpoint_format(&cable_in.id)),
    ];

    // Two different problems, and only one of them stops the music.
    //
    // A channel count other than stereo cannot be worked around: there is no
    // second channel to encode, and no amount of conversion invents one. A rate
    // other than 48 kHz is converted on the way through, which works but spends
    // CPU on every frame and puts a resampler in a path that does not need one.
    let mut wrong_channels: Vec<String> = Vec::new();
    let mut wrong_rate: Vec<String> = Vec::new();
    for (label, format) in &formats {
        let Ok(format) = format else { continue };
        if format.channels != 2 {
            wrong_channels.push(format!("{label}: {}", format.describe()));
        } else if format.sample_rate != REQUIRED_CABLE_RATE {
            wrong_rate.push(format!("{label}: {}", format.describe()));
        }
    }

    if !wrong_channels.is_empty() {
        issues.push(Issue::new(
            "vbcable-not-stereo",
            Severity::Blocking,
            format!(
                "VB-CABLE is not configured as a stereo device ({}).",
                wrong_channels.join("; ")
            ),
            "A stereo stream cannot be built from a cable carrying a different \
             number of channels, and no conversion invents the missing one. \
             Open Setup, step 2, and press \"Configure VB-CABLE\".",
            Some("vbcable-setup"),
        ));
    }

    if !wrong_rate.is_empty() {
        issues.push(Issue::new(
            "vbcable-unconfigured",
            Severity::Warning,
            format!(
                "VB-CABLE is not set to {} Hz ({}).",
                REQUIRED_CABLE_RATE,
                wrong_rate.join("; ")
            ),
            "Playback still works - the rate is converted on the way through - but \
             that is avoidable work on every frame and it slightly softens the top \
             end. Open Setup, step 2, and press \"Configure VB-CABLE\".",
            Some("vbcable-setup"),
        ));
    }

    // Present and configured, but Windows is sending the music elsewhere. The
    // stream then runs perfectly and carries silence.
    match crate::audio::default_render_device() {
        Ok(default) if default.id != cable_in.id => issues.push(Issue::new(
            "vbcable-not-default",
            Severity::Warning,
            format!(
                "Windows is playing to \"{}\", not to CABLE Input.",
                default.name
            ),
            "Only audio sent to CABLE Input reaches the headphones. Either press \
             \"Configure VB-CABLE\" in Setup to make it the default playback device, or \
             route individual applications to it in the Windows volume mixer.",
            Some("vbcable-setup"),
        )),
        _ => {}
    }

    // The saved capture source no longer exists - a cable that was uninstalled,
    // renamed, or a device chosen on a different machine.
    if let Some(wanted) = playback_source.filter(|name| !name.trim().is_empty()) {
        let found = capture
            .iter()
            .any(|device| device.name.to_lowercase().contains(&wanted.to_lowercase()));

        if !found {
            issues.push(Issue::new(
                "playback-source-missing",
                Severity::Warning,
                format!("The selected audio source \"{wanted}\" is not present."),
                "Choose a different source under Settings, Playback. Until then playback \
                 falls back to whichever virtual cable can be found.",
                None,
            ));
        }
    }

    issues
}

#[cfg(not(windows))]
pub fn check(_playback_source: Option<&str>) -> Vec<Issue> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_names_are_what_the_app_matches_on() {
        assert_eq!(Severity::Blocking.as_str(), "error");
        assert_eq!(Severity::Warning.as_str(), "warning");
    }

    #[test]
    fn every_issue_says_what_to_do_about_it() {
        // A warning with no remedy is a warning nobody can act on, which is
        // worse than silence: it teaches people to ignore the whole panel.
        let issue = Issue::new(
            "example",
            Severity::Warning,
            "something is wrong",
            "press the button",
            None,
        );

        assert!(!issue.summary.is_empty());
        assert!(!issue.remedy.is_empty());
    }
}
