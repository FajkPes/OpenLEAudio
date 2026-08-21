using Windows.Devices.Bluetooth;
using Windows.Devices.Bluetooth.GenericAttributeProfile;
using Windows.Devices.Enumeration;
using Windows.Storage.Streams;

namespace OpenLeAudio.PacsProber;

/// <summary>
/// Read-only diagnostic tool. Enumerates paired Bluetooth LE devices, reads their
/// PACS/ASCS characteristics over the Windows GATT client, and reports every LC3
/// configuration the device is willing to accept.
///
/// This tool never writes to a device, never writes to the registry, and never
/// touches driver bindings. Every GATT operation it performs is a read.
/// </summary>
internal static class Program
{
    private static bool _verbose;
    private static bool _preferCache;

    /// <summary>Cache mode to use for the device currently being reported.</summary>
    private static BluetoothCacheMode Mode => _preferCache ? BluetoothCacheMode.Cached : BluetoothCacheMode.Uncached;

    private static async Task<int> Main(string[] args)
    {
        _verbose = args.Contains("--verbose") || args.Contains("-v");

        Console.OutputEncoding = System.Text.Encoding.UTF8;
        Banner();

        try
        {
            await ReportAdapterAsync();
            await ReportDevicesAsync();
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"\nUnexpected failure: {ex.GetType().Name}: {ex.Message}");
            if (_verbose) Console.Error.WriteLine(ex.StackTrace);
            return 1;
        }

        return 0;
    }

    private static void Banner()
    {
        Console.WriteLine("PACS Prober - read-only LE Audio capability dump");
        Console.WriteLine("no writes to devices, registry, or drivers");
        Console.WriteLine(new string('=', 72));
    }

    private static async Task ReportAdapterAsync()
    {
        Section("Bluetooth adapter");

        var adapter = await BluetoothAdapter.GetDefaultAsync();
        if (adapter is null)
        {
            Console.WriteLine("  No Bluetooth adapter found.");
            return;
        }

        Console.WriteLine($"  Address            : {FormatAddress(adapter.BluetoothAddress)}");
        Console.WriteLine($"  Classic supported  : {adapter.IsClassicSupported}");
        Console.WriteLine($"  LE supported       : {adapter.IsLowEnergySupported}");
        Console.WriteLine($"  Central role       : {adapter.IsCentralRoleSupported}");
        Console.WriteLine($"  Peripheral role    : {adapter.IsPeripheralRoleSupported}");
        Console.WriteLine($"  Extended advert.   : {adapter.IsExtendedAdvertisingSupported}");
        Console.WriteLine($"  Max advert. length : {adapter.MaxAdvertisementDataLength} bytes");

        // IsLeAudioSupported only exists on newer Windows SDKs; probe it without hard-linking.
        var leAudioProperty = typeof(BluetoothAdapter).GetProperty("IsLeAudioSupported");
        if (leAudioProperty?.GetValue(adapter) is bool leAudio)
            Console.WriteLine($"  LE Audio supported : {leAudio}");

        var deviceInfo = await DeviceInformation.CreateFromIdAsync(adapter.DeviceId);
        Console.WriteLine($"  Radio device       : {deviceInfo.Name}");
    }

    private static async Task ReportDevicesAsync()
    {
        Section("Paired Bluetooth LE devices");

        var selector = BluetoothLEDevice.GetDeviceSelectorFromPairingState(true);
        var found = await DeviceInformation.FindAllAsync(selector);

        if (found.Count == 0)
        {
            Console.WriteLine("  No paired LE devices. Pair your headphones and run again.");
            return;
        }

        Console.WriteLine($"  {found.Count} paired LE device(s) found.\n");

        foreach (var info in found)
            await ReportDeviceAsync(info);
    }

    private static async Task ReportDeviceAsync(DeviceInformation info)
    {
        Console.WriteLine(new string('-', 72));
        Console.WriteLine($"DEVICE: {info.Name}");
        _preferCache = false;

        BluetoothLEDevice? device = null;
        try
        {
            device = await BluetoothLEDevice.FromIdAsync(info.Id);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"  Could not open device: {ex.Message}\n");
            return;
        }

        if (device is null)
        {
            Console.WriteLine("  Could not open device (owned by another driver or radio off).\n");
            return;
        }

        using (device)
        {
            Console.WriteLine($"  Address     : {FormatAddress(device.BluetoothAddress)} ({device.BluetoothAddressType})");
            Console.WriteLine($"  Connected   : {device.ConnectionStatus}");
            if (_verbose) Console.WriteLine($"  Device id   : {device.DeviceId}");

            // A device owned exclusively by the Microsoft LE Audio driver refuses live GATT
            // traffic, but Windows keeps an attribute cache from pairing that we can still read.
            var services = await device.GetGattServicesAsync(BluetoothCacheMode.Uncached);
            if (services.Status != GattCommunicationStatus.Success)
            {
                Console.WriteLine($"  Live discovery: {services.Status} - falling back to the Windows GATT cache");
                services = await device.GetGattServicesAsync(BluetoothCacheMode.Cached);
                _preferCache = true;
            }

            if (services.Status != GattCommunicationStatus.Success)
            {
                Console.WriteLine($"  Service discovery failed: {services.Status}");
                Console.WriteLine("  (device is disconnected, or owned exclusively by another driver");
                Console.WriteLine("   with nothing cached - connect it and retry)\n");
                return;
            }

            var known = services.Services
                .Where(s => Uuids.DescribeService(s.Uuid) != "unknown / vendor-specific")
                .ToList();

            Console.WriteLine($"  Services    : {services.Services.Count} total, {known.Count} recognised");
            foreach (var service in known)
                Console.WriteLine($"                {service.Uuid.ToString()[..8]}  {Uuids.DescribeService(service.Uuid)}");

            var pacs = services.Services.FirstOrDefault(s => s.Uuid == Uuids.Pacs);
            if (pacs is null)
            {
                Console.WriteLine("\n  No PACS service: this is not an LE Audio device.");
                var hid = services.Services.FirstOrDefault(s => s.Uuid == Uuids.Hid);
                if (hid is not null)
                {
                    Console.WriteLine("  It is an LE HID device (gamepad, keyboard, mouse) - connection");
                    Console.WriteLine("  interval tuning applies here instead of codec settings.");
                }
                Console.WriteLine();
                return;
            }

            Console.WriteLine();
            await ReportPacsAsync(pacs);

            var ascs = services.Services.FirstOrDefault(s => s.Uuid == Uuids.Ascs);
            if (ascs is not null) await ReportAscsAsync(ascs);

            Console.WriteLine();
        }
    }

    private static async Task ReportPacsAsync(GattDeviceService pacs)
    {
        Console.WriteLine("  PACS - published audio capabilities");

        await DumpPacAsync(pacs, Uuids.SinkPac, "SINK (device receives audio - playback)");
        await DumpPacAsync(pacs, Uuids.SourcePac, "SOURCE (device sends audio - microphone)");

        await DumpLocationsAsync(pacs, Uuids.SinkAudioLocations, "Sink audio locations");
        await DumpLocationsAsync(pacs, Uuids.SourceAudioLocations, "Source audio locations");
        await DumpContextsAsync(pacs, Uuids.SupportedContexts, "Supported contexts");
        await DumpContextsAsync(pacs, Uuids.AvailableContexts, "Available contexts");
    }

    private static async Task DumpPacAsync(GattDeviceService service, Guid uuid, string label)
    {
        var value = await ReadCharacteristicAsync(service, uuid);
        if (value is null) return;

        var records = PacRecord.ParseCharacteristic(value.Value);
        if (records.Count == 0) return;

        Console.WriteLine();
        Console.WriteLine($"  {label}");
        if (_verbose) Console.WriteLine($"    raw: {Convert.ToHexString(value.Value.Span)}");

        for (int i = 0; i < records.Count; i++)
            ReportRecord(records[i], i + 1, records.Count);
    }

    private static void ReportRecord(PacRecord record, int index, int total)
    {
        string header = total > 1 ? $"    Record {index}/{total}: {record.CodecName}" : $"    Codec: {record.CodecName}";
        Console.WriteLine(header);

        var caps = record.Capabilities;

        Console.WriteLine($"      Sampling rates  : {(caps.SamplingFrequencies.Count > 0 ? string.Join(", ", caps.SamplingFrequencies) : "(not advertised)")}");

        var durations = new List<string>();
        if (caps.Supports7_5Ms) durations.Add(caps.Prefers7_5Ms ? "7.5 ms (preferred)" : "7.5 ms");
        if (caps.Supports10Ms) durations.Add(caps.Prefers10Ms ? "10 ms (preferred)" : "10 ms");
        Console.WriteLine($"      Frame durations : {(durations.Count > 0 ? string.Join(", ", durations) : "(not advertised)")}");

        Console.WriteLine($"      Channels/stream : {string.Join(", ", caps.ChannelCounts)}");
        Console.WriteLine($"      Frames per SDU  : up to {caps.MaxFramesPerSdu}");

        if (caps.MinOctetsPerFrame is int min && caps.MaxOctetsPerFrame is int max)
            Console.WriteLine($"      Octets/frame    : {min} to {max}");

        foreach (var meta in record.Metadata)
            Console.WriteLine($"      Metadata        : {PacRecord.DescribeMetadata(meta)}");

        foreach (var unknown in caps.Unknown)
            Console.WriteLine($"      Unknown cap LTV : type 0x{unknown.Type:X2} = 0x{unknown.HexValue}");

        if (record.IsLc3) ReportConfigurationMatrix(caps);
    }

    /// <summary>
    /// Cross-references the advertised capability ranges against the named BAP presets,
    /// and shows the bitrate window that a custom configuration could reach.
    /// </summary>
    private static void ReportConfigurationMatrix(CodecCapabilities caps)
    {
        if (caps.MinOctetsPerFrame is not int min || caps.MaxOctetsPerFrame is not int max) return;
        if (caps.SamplingFrequencies.Count == 0) return;

        Console.WriteLine();
        Console.WriteLine("      Configurations this device will accept:");
        Console.WriteLine();
        Console.WriteLine("        preset   rate       frame    octets   bitrate/ch");
        Console.WriteLine("        -------  ---------  -------  -------  ----------");

        foreach (var freq in caps.SamplingFrequencies)
        {
            foreach (double duration in caps.FrameDurations)
            {
                var preset = BapTables.StandardConfigs
                    .FirstOrDefault(c => c.Hz == freq.Hz && Math.Abs(c.FrameMs - duration) < 0.01
                                         && c.Octets >= min && c.Octets <= max);

                if (preset.Name is not null)
                {
                    Console.WriteLine($"        {preset.Name,-7}  {freq,-9}  {duration,4} ms  {preset.Octets,7}  {BapTables.Kbps(preset.Octets, duration),6:0.0} kbps");
                }

                // The full custom window at this rate and frame duration.
                Console.WriteLine($"        {"custom",-7}  {freq,-9}  {duration,4} ms  {min,3}-{max,-3}  {BapTables.Kbps(min, duration),6:0.0}-{BapTables.Kbps(max, duration):0.0} kbps");
            }
        }

        Console.WriteLine();
        Console.WriteLine("        Windows picks one preset row and never exposes it.");
        Console.WriteLine("        Every 'custom' row is reachable but unreachable through the OS UI.");
    }

    private static async Task DumpLocationsAsync(GattDeviceService service, Guid uuid, string label)
    {
        var value = await ReadCharacteristicAsync(service, uuid);
        if (value is null || value.Value.Length < 4) return;

        var span = value.Value.Span;
        int bits = span[0] | (span[1] << 8) | (span[2] << 16) | (span[3] << 24);
        Console.WriteLine($"    {label,-24}: {BapTables.DescribeBitfield(bits, BapTables.AudioLocations)}");
    }

    private static async Task DumpContextsAsync(GattDeviceService service, Guid uuid, string label)
    {
        var value = await ReadCharacteristicAsync(service, uuid);
        if (value is null || value.Value.Length < 2) return;

        var span = value.Value.Span;

        // Supported/Available contexts carry a sink field then a source field.
        int sink = span[0] | (span[1] << 8);
        Console.WriteLine($"    {label + " (sink)",-24}: {BapTables.DescribeBitfield(sink, BapTables.AudioContexts)}");

        if (value.Value.Length >= 4)
        {
            int source = span[2] | (span[3] << 8);
            Console.WriteLine($"    {label + " (source)",-24}: {BapTables.DescribeBitfield(source, BapTables.AudioContexts)}");
        }
    }

    private static async Task ReportAscsAsync(GattDeviceService ascs)
    {
        Console.WriteLine();
        Console.WriteLine("  ASCS - audio stream endpoints");

        var characteristics = await ascs.GetCharacteristicsAsync(Mode);
        if (characteristics.Status != GattCommunicationStatus.Success && !_preferCache)
            characteristics = await ascs.GetCharacteristicsAsync(BluetoothCacheMode.Cached);
        if (characteristics.Status != GattCommunicationStatus.Success)
        {
            Console.WriteLine($"    could not enumerate: {characteristics.Status}");
            return;
        }

        int sinkAses = characteristics.Characteristics.Count(c => c.Uuid == Uuids.SinkAse);
        int sourceAses = characteristics.Characteristics.Count(c => c.Uuid == Uuids.SourceAse);

        Console.WriteLine($"    Sink ASEs   : {sinkAses}  (simultaneous incoming streams)");
        Console.WriteLine($"    Source ASEs : {sourceAses}  (simultaneous outgoing streams)");

        foreach (var characteristic in characteristics.Characteristics
                     .Where(c => c.Uuid == Uuids.SinkAse || c.Uuid == Uuids.SourceAse))
        {
            var value = await ReadValueAsync(characteristic);
            if (value is null || value.Value.Length < 2) continue;

            var span = value.Value.Span;
            string direction = characteristic.Uuid == Uuids.SinkAse ? "sink" : "source";
            Console.WriteLine($"    ASE id {span[0],-3} ({direction,-6}) state: {DescribeAseState(span[1])}");
        }
    }

    private static string DescribeAseState(byte state) => state switch
    {
        0x00 => "Idle",
        0x01 => "Codec Configured",
        0x02 => "QoS Configured",
        0x03 => "Enabling",
        0x04 => "Streaming",
        0x05 => "Disabling",
        0x06 => "Releasing",
        _ => $"reserved (0x{state:X2})",
    };

    private static async Task<ReadOnlyMemory<byte>?> ReadCharacteristicAsync(GattDeviceService service, Guid uuid)
    {
        var result = await service.GetCharacteristicsForUuidAsync(uuid, Mode);
        if (result.Status != GattCommunicationStatus.Success && !_preferCache)
            result = await service.GetCharacteristicsForUuidAsync(uuid, BluetoothCacheMode.Cached);
        if (result.Status != GattCommunicationStatus.Success || result.Characteristics.Count == 0)
            return null;

        return await ReadValueAsync(result.Characteristics[0]);
    }

    private static async Task<ReadOnlyMemory<byte>?> ReadValueAsync(GattCharacteristic characteristic)
    {
        if (!characteristic.CharacteristicProperties.HasFlag(GattCharacteristicProperties.Read))
            return null;

        GattReadResult read;
        try
        {
            read = await characteristic.ReadValueAsync(Mode);
            if (read.Status != GattCommunicationStatus.Success && !_preferCache)
                read = await characteristic.ReadValueAsync(BluetoothCacheMode.Cached);
        }
        catch (Exception ex)
        {
            if (_verbose) Console.WriteLine($"    read failed on {characteristic.Uuid}: {ex.Message}");
            return null;
        }

        if (read.Status != GattCommunicationStatus.Success) return null;

        var buffer = new byte[read.Value.Length];
        DataReader.FromBuffer(read.Value).ReadBytes(buffer);
        return buffer;
    }

    private static string FormatAddress(ulong address) =>
        string.Join(':', BitConverter.GetBytes(address).Take(6).Reverse().Select(b => b.ToString("X2")));

    private static void Section(string title)
    {
        Console.WriteLine();
        Console.WriteLine(title.ToUpperInvariant());
        Console.WriteLine(new string('-', 72));
    }
}
