namespace OpenLeAudio.PacsProber;

/// <summary>
/// Assigned numbers from the Bluetooth SIG Basic Audio Profile (BAP) and the
/// Published Audio Capabilities Service (PACS).
/// </summary>
public static class Uuids
{
    private static Guid Sig(ushort id) => new($"0000{id:x4}-0000-1000-8000-00805f9b34fb");

    // Services
    public static readonly Guid Pacs = Sig(0x1850); // Published Audio Capabilities
    public static readonly Guid Ascs = Sig(0x184E); // Audio Stream Control
    public static readonly Guid Bass = Sig(0x184F); // Broadcast Audio Scan
    public static readonly Guid Vcs = Sig(0x1844);  // Volume Control
    public static readonly Guid Mics = Sig(0x184D); // Microphone Control
    public static readonly Guid Cas = Sig(0x1853);  // Common Audio
    public static readonly Guid Tmas = Sig(0x1855); // Telephony and Media Audio
    public static readonly Guid Csis = Sig(0x1846); // Coordinated Set Identification
    public static readonly Guid Hid = Sig(0x1812);  // Human Interface Device (HOGP)

    // PACS characteristics
    public static readonly Guid SinkPac = Sig(0x2BC9);
    public static readonly Guid SinkAudioLocations = Sig(0x2BCA);
    public static readonly Guid SourcePac = Sig(0x2BCB);
    public static readonly Guid SourceAudioLocations = Sig(0x2BCC);
    public static readonly Guid AvailableContexts = Sig(0x2BCD);
    public static readonly Guid SupportedContexts = Sig(0x2BCE);

    // ASCS characteristics
    public static readonly Guid SinkAse = Sig(0x2BC4);
    public static readonly Guid SourceAse = Sig(0x2BC5);

    public static string DescribeService(Guid g) =>
        g == Pacs ? "PACS - Published Audio Capabilities"
        : g == Ascs ? "ASCS - Audio Stream Control"
        : g == Bass ? "BASS - Broadcast Audio Scan"
        : g == Vcs ? "VCS - Volume Control"
        : g == Mics ? "MICS - Microphone Control"
        : g == Cas ? "CAS - Common Audio"
        : g == Tmas ? "TMAS - Telephony and Media Audio"
        : g == Csis ? "CSIS - Coordinated Set Identification"
        : g == Hid ? "HIDS - HID over GATT"
        : "unknown / vendor-specific";
}

/// <summary>A sampling frequency advertised by a codec capability record.</summary>
public readonly record struct SamplingFrequency(int Bit, int Hz)
{
    public override string ToString() =>
        Hz % 1000 == 0 ? $"{Hz / 1000} kHz" : $"{Hz / 1000.0:0.0##} kHz";
}

public static class BapTables
{
    /// <summary>Supported_Sampling_Frequencies bitfield.</summary>
    public static readonly SamplingFrequency[] SamplingFrequencies =
    [
        new(0, 8000), new(1, 11025), new(2, 16000), new(3, 22050),
        new(4, 24000), new(5, 32000), new(6, 44100), new(7, 48000),
        new(8, 88200), new(9, 96000), new(10, 176400), new(11, 192000),
        new(12, 384000),
    ];

    /// <summary>Audio Location bitfield, Assigned Numbers 6.12.1.</summary>
    public static readonly (int Bit, string Name)[] AudioLocations =
    [
        (0, "Front Left"), (1, "Front Right"), (2, "Front Center"),
        (3, "LFE 1"), (4, "Back Left"), (5, "Back Right"),
        (6, "Front Left of Center"), (7, "Front Right of Center"),
        (8, "Back Center"), (9, "LFE 2"),
        (10, "Side Left"), (11, "Side Right"), (12, "Top Front Left"),
        (13, "Top Front Right"), (14, "Top Front Center"), (15, "Top Center"),
        (16, "Top Back Left"), (17, "Top Back Right"), (18, "Top Side Left"),
        (19, "Top Side Right"), (20, "Top Back Center"), (21, "Bottom Front Center"),
        (22, "Bottom Front Left"), (23, "Bottom Front Right"),
        (24, "Front Left Wide"), (25, "Front Right Wide"),
        (26, "Left Surround"), (27, "Right Surround"),
    ];

    /// <summary>Context Type bitfield, Assigned Numbers 6.12.3.</summary>
    public static readonly (int Bit, string Name)[] AudioContexts =
    [
        (0, "Unspecified"), (1, "Conversational"), (2, "Media"), (3, "Game"),
        (4, "Instructional"), (5, "Voice Assistants"), (6, "Live"),
        (7, "Sound Effects"), (8, "Notifications"), (9, "Ringtone"),
        (10, "Alerts"), (11, "Emergency Alarm"),
    ];

    /// <summary>
    /// The named codec configuration settings from BAP Table 5.2, so we can flag which
    /// of the official presets a device covers and which custom points it also allows.
    /// </summary>
    public static readonly (string Name, int Hz, double FrameMs, int Octets)[] StandardConfigs =
    [
        ("8_1", 8000, 7.5, 26), ("8_2", 8000, 10.0, 30),
        ("16_1", 16000, 7.5, 30), ("16_2", 16000, 10.0, 40),
        ("24_1", 24000, 7.5, 45), ("24_2", 24000, 10.0, 60),
        ("32_1", 32000, 7.5, 60), ("32_2", 32000, 10.0, 80),
        ("441_1", 44100, 7.5, 97), ("441_2", 44100, 10.0, 130),
        ("48_1", 48000, 7.5, 75), ("48_2", 48000, 10.0, 100),
        ("48_3", 48000, 7.5, 90), ("48_4", 48000, 10.0, 120),
        ("48_5", 48000, 7.5, 117), ("48_6", 48000, 10.0, 155),
    ];

    /// <summary>LC3 bitrate for a single channel, in kbps.</summary>
    public static double Kbps(int octetsPerFrame, double frameMs) => octetsPerFrame * 8 / frameMs;

    public static string DescribeBitfield(int bits, (int Bit, string Name)[] table)
    {
        var names = table.Where(e => (bits & (1 << e.Bit)) != 0).Select(e => e.Name).ToList();
        return names.Count == 0 ? "(none)" : string.Join(", ", names);
    }
}

/// <summary>One Type-Length-Value element as used throughout BAP codec structures.</summary>
public readonly record struct Ltv(byte Type, ReadOnlyMemory<byte> Value)
{
    public int AsInt()
    {
        var span = Value.Span;
        int result = 0;
        for (int i = span.Length - 1; i >= 0; i--) result = (result << 8) | span[i];
        return result;
    }

    public string HexValue => Convert.ToHexString(Value.Span);

    /// <summary>Walks a concatenated LTV buffer, stopping cleanly on a truncated tail.</summary>
    public static List<Ltv> ParseAll(ReadOnlyMemory<byte> buffer)
    {
        var items = new List<Ltv>();
        int offset = 0;
        while (offset < buffer.Length)
        {
            int length = buffer.Span[offset];
            if (length == 0) break;                         // padding, not an element
            if (offset + 1 + length > buffer.Length) break; // truncated
            items.Add(new Ltv(buffer.Span[offset + 1], buffer.Slice(offset + 2, length - 1)));
            offset += 1 + length;
        }
        return items;
    }
}

/// <summary>Codec capabilities decoded from the LTV block of one PAC record.</summary>
public sealed class CodecCapabilities
{
    public List<SamplingFrequency> SamplingFrequencies { get; } = [];
    public bool Supports7_5Ms { get; private set; }
    public bool Supports10Ms { get; private set; }
    public bool Prefers7_5Ms { get; private set; }
    public bool Prefers10Ms { get; private set; }
    public List<int> ChannelCounts { get; } = [];
    public int? MinOctetsPerFrame { get; private set; }
    public int? MaxOctetsPerFrame { get; private set; }
    public int MaxFramesPerSdu { get; private set; } = 1;
    public List<Ltv> Unknown { get; } = [];

    public IEnumerable<double> FrameDurations
    {
        get
        {
            if (Supports7_5Ms) yield return 7.5;
            if (Supports10Ms) yield return 10.0;
        }
    }

    public static CodecCapabilities Parse(ReadOnlyMemory<byte> ltvBuffer)
    {
        var caps = new CodecCapabilities();
        foreach (var ltv in Ltv.ParseAll(ltvBuffer))
        {
            switch (ltv.Type)
            {
                case 0x01: // Supported_Sampling_Frequencies
                {
                    int bits = ltv.AsInt();
                    foreach (var freq in BapTables.SamplingFrequencies)
                        if ((bits & (1 << freq.Bit)) != 0) caps.SamplingFrequencies.Add(freq);
                    break;
                }
                case 0x02: // Supported_Frame_Durations
                {
                    int bits = ltv.AsInt();
                    caps.Supports7_5Ms = (bits & 0x01) != 0;
                    caps.Supports10Ms = (bits & 0x02) != 0;
                    caps.Prefers7_5Ms = (bits & 0x10) != 0;
                    caps.Prefers10Ms = (bits & 0x20) != 0;
                    break;
                }
                case 0x03: // Supported_Audio_Channel_Counts
                {
                    int bits = ltv.AsInt();
                    for (int i = 0; i < 8; i++)
                        if ((bits & (1 << i)) != 0) caps.ChannelCounts.Add(i + 1);
                    break;
                }
                case 0x04: // Supported_Octets_Per_Codec_Frame
                {
                    var span = ltv.Value.Span;
                    if (span.Length >= 4)
                    {
                        caps.MinOctetsPerFrame = span[0] | (span[1] << 8);
                        caps.MaxOctetsPerFrame = span[2] | (span[3] << 8);
                    }
                    break;
                }
                case 0x05: // Supported_Max_Codec_Frames_Per_SDU
                    caps.MaxFramesPerSdu = ltv.AsInt();
                    break;
                default:
                    caps.Unknown.Add(ltv);
                    break;
            }
        }

        // A record that omits channel counts means single channel only.
        if (caps.ChannelCounts.Count == 0) caps.ChannelCounts.Add(1);
        return caps;
    }
}

/// <summary>One entry of a Sink PAC or Source PAC characteristic.</summary>
public sealed class PacRecord
{
    public required byte CodingFormat { get; init; }
    public required ushort CompanyId { get; init; }
    public required ushort VendorCodecId { get; init; }
    public required CodecCapabilities Capabilities { get; init; }
    public required List<Ltv> Metadata { get; init; }

    public bool IsLc3 => CodingFormat == 0x06;

    public string CodecName => CodingFormat switch
    {
        0x06 => "LC3",
        0x02 => "CVSD",
        0x03 => "A-law",
        0x04 => "u-law",
        0x05 => "aptX",
        0xFF => $"vendor-specific (company 0x{CompanyId:X4}, codec 0x{VendorCodecId:X4})",
        _ => $"coding format 0x{CodingFormat:X2}",
    };

    /// <summary>
    /// Parses a Sink_PAC / Source_PAC characteristic value: a count byte followed by
    /// that many variable-length records.
    /// </summary>
    public static List<PacRecord> ParseCharacteristic(ReadOnlyMemory<byte> value)
    {
        var records = new List<PacRecord>();
        if (value.Length < 1) return records;

        int count = value.Span[0];
        int offset = 1;

        for (int i = 0; i < count; i++)
        {
            if (offset + 6 > value.Length) break;
            var span = value.Span;

            byte codingFormat = span[offset];
            ushort companyId = (ushort)(span[offset + 1] | (span[offset + 2] << 8));
            ushort vendorCodecId = (ushort)(span[offset + 3] | (span[offset + 4] << 8));
            int capsLength = span[offset + 5];
            offset += 6;

            if (offset + capsLength > value.Length) break;
            var caps = CodecCapabilities.Parse(value.Slice(offset, capsLength));
            offset += capsLength;

            if (offset >= value.Length) break;
            int metadataLength = value.Span[offset];
            offset += 1;

            if (offset + metadataLength > value.Length) break;
            var metadata = Ltv.ParseAll(value.Slice(offset, metadataLength));
            offset += metadataLength;

            records.Add(new PacRecord
            {
                CodingFormat = codingFormat,
                CompanyId = companyId,
                VendorCodecId = vendorCodecId,
                Capabilities = caps,
                Metadata = metadata,
            });
        }

        return records;
    }

    public static string DescribeMetadata(Ltv ltv) => ltv.Type switch
    {
        0x01 => $"Preferred Audio Contexts: {BapTables.DescribeBitfield(ltv.AsInt(), BapTables.AudioContexts)}",
        0x02 => $"Streaming Audio Contexts: {BapTables.DescribeBitfield(ltv.AsInt(), BapTables.AudioContexts)}",
        0x03 => "Program Info",
        0x04 => "Language",
        0x05 => "CCID List",
        0x06 => "Parental Rating",
        0x07 => "Program Info URI",
        0x08 => "Audio Active State",
        0x09 => "Broadcast Audio Immediate Rendering",
        0xFF => $"Vendor Specific (0x{ltv.HexValue})",
        _ => $"type 0x{ltv.Type:X2} (0x{ltv.HexValue})",
    };
}
