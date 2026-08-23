using System;
using System.Collections.Generic;
using System.Globalization;

namespace OpenLEAudio;

/// <summary>
/// What a setting costs the radio, worked out rather than guessed.
/// </summary>
/// <remarks>
/// Every number here is derived from the stream configuration the user has
/// actually chosen, using the packet timing the Bluetooth link layer defines.
/// None of it is a measurement of a battery, and it is never presented as one:
/// how long a pair of headphones lasts depends on its amplifier, its noise
/// cancelling and its cell as much as on its radio, and this stack can see none
/// of those.
///
/// What it can see is airtime, and airtime is the part it controls. A CIG
/// reserves a fixed slot for every subevent it might need, whether or not the
/// data gets through first time, so the reserved airtime is a real quantity the
/// settings move directly - and comparing one configuration against another in
/// those terms is honest in a way that "this uses 12% more battery" would not
/// be.
/// </remarks>
public static class PowerEstimate
{
    /// <summary>Bytes of link-layer framing around each isochronous payload.</summary>
    /// <remarks>
    /// Preamble, access address, header, MIC and CRC. Fixed per packet, which is
    /// why a short payload is proportionally more expensive than a long one and
    /// why halving the bitrate never halves the airtime.
    /// </remarks>
    private const double OverheadBytes = 14;

    /// <summary>Inter-frame space between a packet and its acknowledgement, in microseconds.</summary>
    private const double InterFrameSpaceUs = 150;

    /// <summary>How the radio time of one configuration compares with another.</summary>
    public sealed record Reading(double AirtimeUsPerSecond)
    {
        /// <summary>Share of every second the radio spends transmitting.</summary>
        public double DutyCycle => AirtimeUsPerSecond / 1_000_000.0;
    }

    /// <summary>
    /// Reserved transmit time per second for one stream configuration.
    /// </summary>
    public static Reading Airtime(IReadOnlyDictionary<string, string> values)
    {
        var frameUs = Number(values, "frame_ms", 10.0) * 1000.0;
        if (frameUs <= 0) frameUs = 10_000;

        var octets = Number(values, "octets", 90);
        var retransmissions = Number(values, "retransmissions", 2);
        var megabits = Text(values, "phy", "2M") == "1M" ? 1.0 : 2.0;

        // One stream per ear unless the user asked for a single mixed channel.
        var streams = Text(values, "audio_mode", "stereo") == "mono" ? 1 : 2;

        // The headset microphone is a stream in the other direction, with its
        // own reserved slots.
        var microphone = Text(values, "microphone_mode", "off") == "on" ? 1 : 0;

        var packetsPerSecond = 1_000_000.0 / frameUs;

        // Every subevent the group reserves: the first transmission plus each
        // retransmission the controller is allowed to schedule.
        var attempts = 1 + Math.Max(0, retransmissions);

        var payloadUs = (octets + OverheadBytes) * 8.0 / megabits;
        var perAttemptUs = payloadUs + InterFrameSpaceUs;

        var airtime = packetsPerSecond * attempts * perAttemptUs * (streams + microphone);
        return new Reading(airtime);
    }

    /// <summary>
    /// How much of the current airtime this one setting is responsible for.
    /// </summary>
    /// <returns>
    /// Null when the setting does not move the radio at all, which is most of
    /// them. Saying nothing is better than inventing a small number for a
    /// control that genuinely costs nothing.
    /// </returns>
    public static double? SavingIfCheapest(
        IReadOnlyDictionary<string, string> values, string key)
    {
        var cheapest = Cheapest(key);
        if (cheapest is null) return null;

        var current = Airtime(values).AirtimeUsPerSecond;
        if (current <= 0) return null;

        var alternative = new Dictionary<string, string>(values, StringComparer.Ordinal)
        {
            [key] = cheapest,
        };
        var reduced = Airtime(alternative).AirtimeUsPerSecond;

        return Math.Max(0, (current - reduced) / current);
    }

    /// <summary>The value of this setting that spends the least radio time.</summary>
    /// <remarks>
    /// Public because naming it is half the point. "The most economical value
    /// would save 18%" is useless in front of a menu reading 7.5 and 10: there
    /// is nothing in either number that says which way is cheaper, and guessing
    /// wrong costs a reconnect to find out.
    /// </remarks>
    public static string? Cheapest(string key) => key switch
    {
        // Fewer reserved retransmissions is the single largest saving available,
        // and also the one most likely to cost reliability.
        "retransmissions" => "0",
        // 2M spends half the time on air for the same data.
        "phy" => "2M",
        // Fewer octets is a lower bitrate and a shorter packet.
        "octets" => "20",
        // Ten millisecond frames mean fewer packets per second, so the fixed
        // per-packet overhead is paid less often.
        "frame_ms" => "10",
        // One stream instead of two.
        "audio_mode" => "mono",
        // No second direction to reserve slots for.
        "microphone_mode" => "off",
        _ => null,
    };

    /// <summary>
    /// A plain-language note for settings whose cost is real but not airtime.
    /// </summary>
    /// <remarks>
    /// These cannot be expressed as a share of the stream, because they apply
    /// when there is no stream. A sentence explaining the mechanism is worth
    /// more than a percentage that would have to be made up.
    /// </remarks>
    public static string? Note(string key, IReadOnlyDictionary<string, string> values) => key switch
    {
        "link_metrics" => Text(values, "link_metrics", "full") switch
        {
            "off" => "power.metrics_off",
            "signal" => "power.metrics_signal",
            _ => "power.metrics_full",
        },
        "multipoint_yield_enabled" =>
            Text(values, "multipoint_yield_enabled", "true") is "true" or "1"
                ? "power.multipoint_on"
                : "power.multipoint_off",
        "idle_timeout_min" => "power.idle",
        "reconnect_interval_s" => "power.reconnect_interval",
        "reconnect_window_min" => "power.reconnect_window",
        "monitor_enabled" => "power.monitor",
        "idle_link_latency" => Number(values, "idle_link_latency", 0) >= 1
            ? "power.idle_link_on"
            : "power.idle_link_off",
        "battery_poll_min" => Number(values, "battery_poll_min", 15) >= 1
            ? "power.battery_poll_on"
            : "power.battery_poll_off",
        "link_timeout_s" => "power.link_timeout",
        _ => null,
    };

    /// <summary>True when this setting is worth putting a battery icon beside.</summary>
    public static bool Affects(string key, IReadOnlyDictionary<string, string> values) =>
        Cheapest(key) is not null || Note(key, values) is not null;

    private static string Text(IReadOnlyDictionary<string, string> values, string key, string fallback) =>
        values.TryGetValue(key, out var value) && value.Length > 0 ? value : fallback;

    private static double Number(IReadOnlyDictionary<string, string> values, string key, double fallback) =>
        values.TryGetValue(key, out var value)
        && double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out var parsed)
            ? parsed
            : fallback;
}
