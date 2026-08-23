//! Whether a headset is already busy with someone else, asked the standard way.
//!
//! "Multipoint" is a marketing word for a peripheral holding connections to two
//! hosts at once. It is worth being precise about where the control over it
//! actually lives, because the answer decides what this stack can honestly
//! offer.
//!
//! It is a **peripheral-side** feature. Nothing in BAP, ASCS, PACS or any other
//! adopted profile lets a host enable it, disable it, or ask a headset to switch
//! to it. Every vendor that exposes those controls does so through its own
//! protocol on its own characteristic, which means a switch built that way works
//! on exactly one brand - the opposite of what this project is for, and behind a
//! safety boundary that deliberately blocks vendor-specific traffic.
//!
//! What *is* standard, and what this module uses:
//!
//! - **Available Audio Contexts** (PACS, 0x2BCD). A device publishes which
//!   contexts it can accept at this moment, as distinct from the ones it
//!   supports in principle. A headset already streaming from a phone withdraws
//!   Media from its available set, and restores it when the phone stops. This is
//!   the only vendor-neutral way to know a second host is in the picture.
//!
//! - **Taking over by asking.** The specified way for a host to start playing is
//!   to configure its ASEs and send Enable. On a device that supports
//!   multipoint, that is also how the switch happens - there is no separate
//!   handover operation to send. So "make the headphones switch to this PC" is
//!   not a missing feature; it is the ordinary connect path, and it either works
//!   or the device tells us it will not.
//!
//! - **Giving the other host a turn.** Releasing our ASEs while leaving the ACL
//!   connection up is what lets the other device play without us having to
//!   disconnect. That is multipoint behaving correctly from our side, and it
//!   costs nothing but a Release.
//!
//! So the useful thing to build is not a switch. It is knowing, and saying,
//! which of these three states the headset is in - because today a headset busy
//! with a phone produces a stream-setup failure that reads as broken hardware.

use crate::bap::ascs::{CONTEXT_CONVERSATIONAL, CONTEXT_GAME, CONTEXT_MEDIA};
use crate::link::AudioCapabilities;

/// What the headset's published contexts say about its availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// It can take a media stream now.
    Ready,
    /// It supports media but is not offering it at the moment, which in practice
    /// means another host is using it.
    BusyElsewhere,
    /// It never supported media playback in the first place. A different problem
    /// entirely, and one no amount of waiting will fix.
    NoMediaSupport,
    /// It publishes no context information, so there is nothing to conclude.
    ///
    /// Deliberately not folded into `Ready`: "I checked and it is free" and "I
    /// could not check" are different claims, and reporting the second as the
    /// first is how a diagnostic loses its value.
    Unknown,
}

impl Availability {
    /// Whether it is worth attempting a stream at all.
    ///
    /// True for everything except a device that has never supported media.
    /// A busy device is still worth asking: Enable is the specified way to take
    /// over, and a device that will hand the stream across does so in response
    /// to exactly that request. Refusing pre-emptively would break the one
    /// standard mechanism multipoint has.
    pub fn worth_attempting(self) -> bool {
        !matches!(self, Availability::NoMediaSupport)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Ready => "ready",
            Availability::BusyElsewhere => "busy-elsewhere",
            Availability::NoMediaSupport => "no-media-support",
            Availability::Unknown => "unknown",
        }
    }

    /// A sentence for a person, saying what it means rather than what it is.
    pub fn explain(self) -> &'static str {
        match self {
            Availability::Ready => "headphones are free and ready to play",
            Availability::BusyElsewhere => {
                "headphones are currently playing from another device; \
                 starting a stream here should take them over"
            }
            Availability::NoMediaSupport => {
                "headphones do not support media playback over LE Audio"
            }
            Availability::Unknown => {
                "headphones do not publish their available contexts, \
                 so their state cannot be read"
            }
        }
    }
}

/// Reads the availability of a device from what it has published.
pub fn availability(capabilities: &AudioCapabilities) -> Availability {
    let Some(available) = capabilities.available_contexts else {
        return Availability::Unknown;
    };

    if available & CONTEXT_MEDIA != 0 {
        return Availability::Ready;
    }

    // Media is missing. Whether that is temporary depends on whether the device
    // ever claimed to support it: Supported Audio Contexts is the permanent
    // list, Available is the momentary one.
    match capabilities.supported_contexts {
        Some(supported) if supported & CONTEXT_MEDIA != 0 => Availability::BusyElsewhere,
        Some(_) => Availability::NoMediaSupport,
        // Supported was not readable, so the absence cannot be interpreted.
        // Some firmware publishes an empty Available set while idle, and calling
        // that "busy" would be a guess dressed up as a reading.
        None => Availability::Unknown,
    }
}

/// Names the contexts in a bitmap, for a log line a person can read.
pub fn describe_contexts(contexts: u16) -> String {
    if contexts == 0 {
        return "none".to_string();
    }

    let mut names = Vec::new();
    if contexts & CONTEXT_MEDIA != 0 {
        names.push("media");
    }
    if contexts & CONTEXT_CONVERSATIONAL != 0 {
        names.push("calls");
    }
    if contexts & CONTEXT_GAME != 0 {
        names.push("game");
    }

    // Anything the list above does not name still counts. Printing the leftover
    // bits keeps the line honest about how much of the value was understood.
    let named = CONTEXT_MEDIA | CONTEXT_CONVERSATIONAL | CONTEXT_GAME;
    let rest = contexts & !named;
    let mut text = names.join(", ");
    if rest != 0 {
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(&format!("other ({rest:#06x})"));
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(available: Option<u16>, supported: Option<u16>) -> AudioCapabilities {
        AudioCapabilities {
            available_contexts: available,
            supported_contexts: supported,
            ..Default::default()
        }
    }

    #[test]
    fn a_device_offering_media_is_ready() {
        let caps = capabilities(Some(CONTEXT_MEDIA | CONTEXT_CONVERSATIONAL), Some(0xFFFF));
        assert_eq!(availability(&caps), Availability::Ready);
    }

    #[test]
    fn media_supported_but_not_offered_means_another_host_has_it() {
        // The signature of multipoint: the device can do media, and right now it
        // is not offering it to us.
        let caps = capabilities(Some(CONTEXT_CONVERSATIONAL), Some(CONTEXT_MEDIA | CONTEXT_CONVERSATIONAL));
        assert_eq!(availability(&caps), Availability::BusyElsewhere);
    }

    #[test]
    fn a_device_that_never_supported_media_is_not_merely_busy() {
        let caps = capabilities(Some(CONTEXT_CONVERSATIONAL), Some(CONTEXT_CONVERSATIONAL));
        assert_eq!(availability(&caps), Availability::NoMediaSupport);
    }

    #[test]
    fn nothing_published_is_reported_as_unknown_rather_than_free() {
        assert_eq!(availability(&capabilities(None, None)), Availability::Unknown);

        // Available is empty but Supported could not be read. That is not
        // enough to call the device busy, and guessing would turn a diagnostic
        // into a rumour.
        assert_eq!(availability(&capabilities(Some(0), None)), Availability::Unknown);
    }

    #[test]
    fn a_busy_device_is_still_worth_asking() {
        // Enable is the standard way to take a stream over, so refusing to try
        // would remove the only handover mechanism multipoint actually has.
        assert!(Availability::BusyElsewhere.worth_attempting());
        assert!(Availability::Unknown.worth_attempting());
        assert!(!Availability::NoMediaSupport.worth_attempting());
    }

    #[test]
    fn unrecognised_context_bits_are_still_reported() {
        let text = describe_contexts(CONTEXT_MEDIA | 0x0400);
        assert!(text.contains("media"));
        assert!(text.contains("0x0400"));
    }

    #[test]
    fn no_contexts_reads_as_none_rather_than_an_empty_line() {
        assert_eq!(describe_contexts(0), "none");
    }
}
