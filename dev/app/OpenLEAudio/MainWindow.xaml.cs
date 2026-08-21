using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using Microsoft.Win32;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Storage.Pickers;

namespace OpenLEAudio;

/// <summary>One row of a device list.</summary>
public sealed class DeviceRow
{
    public string Address { get; init; } = "";
    public string Name { get; init; } = "";
    public bool Paired { get; init; }
    public bool LeAudio { get; init; }
    public bool Connected { get; init; }
    public bool Streaming { get; init; }
    public bool Connecting { get; init; }
    public int Rssi { get; init; }

    /// <summary>Segoe Fluent Icons: a speaker for audio, the Bluetooth mark otherwise.</summary>
    public string Glyph => LeAudio ? "\uE767" : "\uE702";

    public string Detail => Connecting
        ? Loc.T("device.connecting")
        : Connected
            ? (Streaming ? Loc.T("device.connected_playing") : Loc.T("device.connected"))
            : Paired ? Loc.T("device.paired") : LeAudio ? "LE Audio" : "Bluetooth LE";

    public string Badge => Streaming ? Loc.T("device.playing") : Loc.T("device.connected");

    public Visibility BadgeVisibility => Connected ? Visibility.Visible : Visibility.Collapsed;

    public string Signal => Rssi == 0 ? "" : $"{Rssi} dBm";

    /// <summary>
    /// A known device is connected; an unknown one is paired. Saying "Connect"
    /// for something that will run a full key exchange loses the user's trust.
    /// </summary>
    public string ActionLabel => Connected ? Loc.T("device.disconnect") : Paired ? Loc.T("device.connect") : Loc.T("device.pair");
    public string UnpairLabel => Loc.T("devices.unpair");

    public DeviceRow With(
        string? name = null,
        bool? leAudio = null,
        bool? connected = null,
        bool? streaming = null,
        bool? connecting = null,
        bool? paired = null,
        int? rssi = null) => new()
    {
        Address = Address,
        Name = name ?? Name,
        LeAudio = leAudio ?? LeAudio,
        Paired = paired ?? Paired,
        Rssi = rssi ?? Rssi,
        Connected = connected ?? Connected,
        Streaming = streaming ?? Streaming,
        Connecting = connecting ?? Connecting,
    };
}

public sealed record AdapterChoice(string Name, string InstanceId, string HardwareId,
    string Service, string Driver, bool Supported)
{
    public override string ToString() => $"{Name}  ·  {HardwareId}";
}

public sealed partial class MainWindow : Window
{
    private readonly DispatcherQueue _ui;
    private readonly ObservableCollection<DeviceRow> _paired = new();
    private readonly ObservableCollection<DeviceRow> _found = new();
    private readonly HashSet<string> _pendingReconnectMarkers = new();
    private readonly object _logGate = new();
    private readonly StringBuilder _pendingLog = new();
    private readonly Queue<LinkHealthSample> _healthSamples = new();
    private readonly DispatcherQueueTimer _logFlushTimer;
    private AgentClient? _agent;
    private bool _followLog = true;
    private bool _debugLogEnabled;
    private bool _programmaticLogScroll;
    private System.Windows.Forms.NotifyIcon? _trayIcon;
    private bool _runInBackground;
    private bool _exitRequested;
    private bool _adaptersDetected;
    private bool _startupReconnectEnabled = true;
    private DispatcherQueueTimer? _startupReconnectTimer;
    private DateTimeOffset _startupReconnectUntil;

    // TextBlock stores UTF-16. Half a million characters plus its string is
    // approximately one MiB of retained console history, regardless of how
    // long the app stays open.
    private const int MaxLogCharacters = 512 * 1024;
    private const int MaxNormalLogLines = 500;
    private sealed record LinkHealthSample(DateTimeOffset Time, long Sent, long Failed);

    // UI state lives here, not read back out of controls. Reading a control from
    // the agent's reader thread throws, and an exception there kills the reader
    // loop - which looks exactly like a stack that stopped talking: an empty
    // list, a spinner that never stops, and a toggle that does nothing.
    private bool _adapterOn;
    private string? _connectedAddress;
    private bool _suppressToggle;

    /// Set while a reconnect is in flight, so the disconnect that starts it
    /// knows to connect again rather than simply stopping.
    private string? _reconnectTo;

    /// True while controls are being filled in from stored values.
    ///
    /// Setting a control's value raises the same event as a person changing it,
    /// so without this every refresh saves everything back - and a value edited
    /// a moment earlier gets overwritten by the one the page was built with.
    /// That is the "it reverts by itself" behaviour.
    private bool _populating;

    /// The keys currently on the page, so it is rebuilt only when they change.
    private string _settingsShape = "";
    private bool _customPreset;
    private ComboBox? _presetBox;
    private readonly List<FrameworkElement> _settingCards = new();
    private readonly Dictionary<FrameworkElement, int> _settingCardPanels = new();
    private int _settingsPanelColumns = -1;

    private static readonly HashSet<string> PresetControlledKeys = new(StringComparer.Ordinal)
    {
        "rate_hz", "frame_ms", "octets", "phy", "retransmissions",
        "max_latency_ms", "presentation_delay_ms",
    };

    /// The "saved" label beside each control, by setting key.
    private readonly Dictionary<string, TextBlock> _savedMarkers = new();

    public MainWindow()
    {
        InitializeComponent();
        _ui = DispatcherQueue.GetForCurrentThread();
        // Set runtime defaults only after every named XAML element exists.
        // Setting IsOn in XAML raises Toggled while later elements such as the
        // log ScrollViewer are still null and used to crash the whole startup.
        FollowLogSwitch.IsOn = true;
        _logFlushTimer = _ui.CreateTimer();
        _logFlushTimer.Interval = TimeSpan.FromMilliseconds(100);
        _logFlushTimer.IsRepeating = true;
        _logFlushTimer.Tick += (_, _) => FlushQueuedLog();
        _logFlushTimer.Start();
        PairedList.ItemsSource = _paired;
        DeviceList.ItemsSource = _found;
        Nav.Loaded += (_, _) => Loc.Apply(Nav);
        SettingsHost.SizeChanged += SettingsHostSizeChanged;
        AboutDetailsGrid.SizeChanged += AboutDetailsGridSizeChanged;
        AboutMappingGrid.SizeChanged += AboutMappingGridSizeChanged;

        ConfigureTray();
        AppWindow.Closing += OnAppWindowClosing;

        Closed += (_, _) =>
        {
            _logFlushTimer.Stop();
            _agent?.Dispose();
            if (_trayIcon is not null)
            {
                _trayIcon.Visible = false;
                _trayIcon.Dispose();
            }
        };

        try
        {
            _agent = AgentClient.Start();
            _agent.EventReceived += OnAgentEventOffThread;
            _agent.Trouble += Log;
            _agent.BeginReading();
        }
        catch (Exception e)
        {
            Log(e.Message);
            _ui.TryEnqueue(() =>
            {
                AdapterDetail.Text = Loc.T("status.core_failed");
                ScanStatus.Text = Loc.T("status.unavailable");
            });
        }
    }

    // ------------------------------------------------------------ dispatching

    /// <summary>
    /// The single crossing point between the reader thread and the UI.
    /// </summary>
    /// <remarks>
    /// Everything downstream of here runs on the dispatcher, so no handler has
    /// to remember which thread it is on. The try/catch is not defensive
    /// decoration: without it, one malformed field silently stops every future
    /// event from arriving.
    /// </remarks>
    private void OnAgentEventOffThread(JsonElement message) =>
        _ui.TryEnqueue(() =>
        {
            try
            {
                OnAgentEvent(message);
            }
            catch (Exception e)
            {
                Append($"UI ERROR: {e.Message}");
            }
        });

    private void Log(string text)
    {
        lock (_logGate)
        {
            _pendingLog.AppendLine(text);
        }
    }

    public void StartHidden()
    {
        if (_trayIcon is not null) _trayIcon.Visible = true;
        AppWindow.Hide();
    }

    /// <summary>For three minutes after a background Windows launch, looks for a
    /// remembered LE Audio device every five seconds and connects it.</summary>
    public void BeginStartupReconnect()
    {
        _startupReconnectUntil = DateTimeOffset.UtcNow.AddMinutes(3);
        _startupReconnectTimer = _ui.CreateTimer();
        _startupReconnectTimer.Interval = TimeSpan.FromSeconds(5);
        _startupReconnectTimer.IsRepeating = true;
        _startupReconnectTimer.Tick += (_, _) => StartupReconnectTick();
        _startupReconnectTimer.Start();
        StartupReconnectTick();
    }

    private void StartupReconnectTick()
    {
        if (!_startupReconnectEnabled || _connectedAddress is not null ||
            DateTimeOffset.UtcNow >= _startupReconnectUntil)
        {
            _startupReconnectTimer?.Stop();
            return;
        }
        if (!_adapterOn || Busy.IsActive) return;

        var candidate = _found.FirstOrDefault(row => row.Paired && row.LeAudio);
        if (candidate is not null)
        {
            Busy.IsActive = true;
            Update(candidate.Address, row => row.With(connecting: true));
            Send("connect", new() { ["address"] = candidate.Address });
            return;
        }

        _found.Clear();
        Busy.IsActive = true;
        ScanStatus.Text = Loc.T("status.scanning");
        Send("scan", new() { ["seconds"] = 3 });
    }

    private void ConfigureTray()
    {
        var menu = new System.Windows.Forms.ContextMenuStrip();
        menu.Items.Add(Loc.T("tray.open"), null, (_, _) => ShowFromTray());
        menu.Items.Add(Loc.T("tray.exit"), null, (_, _) => ExitFromTray());
        _trayIcon = new System.Windows.Forms.NotifyIcon
        {
            Text = "OpenLEAudio Client",
            Icon = System.Drawing.Icon.ExtractAssociatedIcon(Environment.ProcessPath!)
                ?? System.Drawing.SystemIcons.Application,
            ContextMenuStrip = menu,
            Visible = false,
        };
        _trayIcon.DoubleClick += (_, _) => ShowFromTray();
        _trayIcon.MouseClick += (_, e) =>
        {
            if (e.Button == System.Windows.Forms.MouseButtons.Left) ShowFromTray();
        };
    }

    private void RefreshTrayLanguage()
    {
        if (_trayIcon?.ContextMenuStrip is not { Items.Count: >= 2 } menu) return;
        menu.Items[0].Text = Loc.T("tray.open");
        menu.Items[1].Text = Loc.T("tray.exit");
    }

    private void OnAppWindowClosing(Microsoft.UI.Windowing.AppWindow sender,
        Microsoft.UI.Windowing.AppWindowClosingEventArgs args)
    {
        if (!_exitRequested && _runInBackground)
        {
            args.Cancel = true;
            StartHidden();
        }
    }

    private void ShowFromTray() => _ui.TryEnqueue(() =>
    {
        AppWindow.Show();
        Activate();
        if (_trayIcon is not null) _trayIcon.Visible = false;
    });

    /// <summary>Shows the existing window when the executable is launched again.</summary>
    internal void ShowFromExternalLaunch() => ShowFromTray();

    private void ExitFromTray() => _ui.TryEnqueue(() =>
    {
        _exitRequested = true;
        if (_trayIcon is not null) _trayIcon.Visible = false;
        Close();
    });

    private static void SetStartupEnabled(bool enabled)
    {
        const string path = @"Software\Microsoft\Windows\CurrentVersion\Run";
        const string valueName = "OpenLEAudio";
        using var key = Registry.CurrentUser.OpenSubKey(path, writable: true)
            ?? Registry.CurrentUser.CreateSubKey(path);
        if (enabled)
        {
            key.SetValue(valueName, $"\"{Environment.ProcessPath}\" --background");
        }
        else
        {
            key.DeleteValue(valueName, throwOnMissingValue: false);
        }
    }

    private void FlushQueuedLog()
    {
        string text;
        lock (_logGate)
        {
            if (_pendingLog.Length == 0)
            {
                return;
            }
            text = _pendingLog.ToString();
            _pendingLog.Clear();
        }

        Append(text.TrimEnd('\r', '\n'));
    }

    /// <summary>Appends to the log. UI thread only.</summary>
    /// <remarks>
    /// Follows the newest line only while the view is already at the bottom.
    /// Scrolling up is how someone reads what just went wrong, and yanking them
    /// back down every time a frame counter ticks makes the log unreadable
    /// exactly when it matters.
    /// </remarks>
    private void Append(string text)
    {
        var combined = LogText.Text + text + Environment.NewLine;
        if (!_debugLogEnabled)
        {
            var linesToRemove = combined.Count(character => character == '\n') - MaxNormalLogLines;
            var cut = 0;
            while (linesToRemove-- > 0)
            {
                var newline = combined.IndexOf('\n', cut);
                if (newline < 0) break;
                cut = newline + 1;
            }
            if (cut > 0) combined = combined[cut..];
        }
        if (combined.Length > MaxLogCharacters)
        {
            var remove = combined.Length - MaxLogCharacters;
            var nextLine = combined.IndexOf('\n', remove);
            combined = nextLine >= 0 ? combined[(nextLine + 1)..] : combined[^MaxLogCharacters..];
        }
        LogText.Text = combined;

        if (_followLog)
        {
            ScrollLogToBottom();
        }
    }

    private void ScrollLogToBottom()
    {
        _programmaticLogScroll = true;
        LogScroller.UpdateLayout();
        LogScroller.ChangeView(null, LogScroller.ScrollableHeight, null, true);
        _programmaticLogScroll = false;
    }

    private void LogViewChanged(object sender, ScrollViewerViewChangedEventArgs e)
    {
        if (_programmaticLogScroll || e.IsIntermediate)
        {
            return;
        }

        const double Slack = 24.0;
        var atBottom = LogScroller.VerticalOffset >= LogScroller.ScrollableHeight - Slack;
        if (!atBottom && _followLog)
        {
            _followLog = false;
            FollowLogSwitch.IsOn = false;
        }
    }

    private void FollowLogToggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle || LogScroller is null) return;
        _followLog = toggle.IsOn;
        if (_followLog)
        {
            ScrollLogToBottom();
        }
    }

    private void ScrollLogBottomClicked(object sender, RoutedEventArgs e)
    {
        FollowLogSwitch.IsOn = true;
        _followLog = true;
        ScrollLogToBottom();
    }

    private void DebugLogToggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle || LogText is null) return;
        _debugLogEnabled = toggle.IsOn;
        Send("debug", new() { ["on"] = toggle.IsOn });
        if (!toggle.IsOn)
        {
            lock (_logGate) _pendingLog.Clear();
            LogText.Text = "";
            Append(Loc.T("log.debug_disabled"));
        }
        else
        {
            Append(Loc.T("log.debug_enabled"));
        }
    }

    private void PageChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string;

        SetupPage.Visibility = tag == "setup" ? Visibility.Visible : Visibility.Collapsed;
        DevicesPage.Visibility = tag == "devices" ? Visibility.Visible : Visibility.Collapsed;
        SettingsPage.Visibility = tag == "settings" ? Visibility.Visible : Visibility.Collapsed;
        LanguagePage.Visibility = tag == "language" ? Visibility.Visible : Visibility.Collapsed;
        AboutPage.Visibility = tag == "about" ? Visibility.Visible : Visibility.Collapsed;

        if (tag == "setup" && !_adaptersDetected) _ = DetectAdaptersAsync();

        if (tag is "settings" or "language")
        {
            Send("settings");
        }
    }

    private async void Send(string command, Dictionary<string, object?>? arguments = null) =>
        await TrySend(command, arguments);

    private async Task<bool> TrySend(string command, Dictionary<string, object?>? arguments = null)
    {
        if (_agent is null)
        {
            return false;
        }

        try
        {
            await _agent.SendAsync(command, arguments);
            return true;
        }
        catch (Exception e) when (e is System.IO.IOException or ObjectDisposedException or InvalidOperationException)
        {
            // A dead agent must never leave the UI looking busy forever. This is
            // also the one place every fire-and-forget command is observed, so a
            // broken pipe becomes a visible error rather than an unobserved Task.
            Busy.IsActive = false;
            ScanStatus.Text = Loc.T("status.core_unavailable");
            Append($"Command '{command}' could not be sent: {e.Message}");
            return false;
        }
    }

    private async void DetectAdaptersClicked(object sender, RoutedEventArgs e) => await DetectAdaptersAsync();

    private async Task DetectAdaptersAsync()
    {
        SetupAdapterBox.IsEnabled = false;
        SetupAdapterBox.PlaceholderText = Loc.T("setup.detecting");
        SetupAdapterDetails.Text = Loc.T("setup.detecting");
        try
        {
            var root = FindProjectRoot() ?? throw new DirectoryNotFoundException(Loc.T("setup.files_missing"));
            var infPath = Path.Combine(root, "driver", "olea_winusb.inf");
            var supportedIds = Regex.Matches(File.ReadAllText(infPath),
                    @"USB\\VID_[0-9A-F]{4}&PID_[0-9A-F]{4}", RegexOptions.IgnoreCase)
                .Select(match => match.Value.ToUpperInvariant())
                .Distinct(StringComparer.OrdinalIgnoreCase)
                .ToArray();
            if (supportedIds.Length == 0) throw new InvalidDataException("The INF contains no supported adapter hardware IDs.");

            // SetupAPI enumerates the actual present PnP nodes. It deliberately
            // does not filter by class: our INF changes Bluetooth -> USBDevice.
            // This also avoids different Get-PnpDevice results between Windows
            // PowerShell 5.1 and PowerShell 7.
            var adapters = await Task.Run(() => EnumerateSupportedAdapters(supportedIds));

            SetupAdapterBox.ItemsSource = adapters;
            SetupAdapterBox.SelectedIndex = adapters.Count > 0 ? 0 : -1;
            _adaptersDetected = true;
            if (adapters.Count == 0) SetupAdapterDetails.Text = Loc.T("setup.none");
        }
        catch (Exception error)
        {
            SetupAdapterDetails.Text = Loc.T("setup.detect_error", error.Message);
        }
        finally
        {
            SetupAdapterBox.IsEnabled = true;
        }
    }

    private const uint DigcfPresent = 0x00000002;
    private const uint DigcfAllClasses = 0x00000004;
    private const uint SpdrpDeviceDesc = 0x00000000;
    private const uint SpdrpService = 0x00000004;
    private const uint SpdrpFriendlyName = 0x0000000C;

    [StructLayout(LayoutKind.Sequential)]
    private struct SpDevinfoData
    {
        public uint Size;
        public Guid ClassGuid;
        public uint DevInst;
        public nint Reserved;
    }

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint SetupDiGetClassDevs(nint classGuid, string? enumerator,
        nint parent, uint flags);

    [DllImport("setupapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiEnumDeviceInfo(nint set, uint index, ref SpDevinfoData data);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiGetDeviceInstanceId(nint set, ref SpDevinfoData data,
        StringBuilder instanceId, int size, out int required);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiGetDeviceRegistryProperty(nint set, ref SpDevinfoData data,
        uint property, out uint type, byte[] buffer, uint size, out uint required);

    [DllImport("setupapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetupDiDestroyDeviceInfoList(nint set);

    private static List<AdapterChoice> EnumerateSupportedAdapters(string[] supportedIds)
    {
        var result = new List<AdapterChoice>();
        var set = SetupDiGetClassDevs(0, null, 0, DigcfPresent | DigcfAllClasses);
        if (set == -1) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        try
        {
            for (uint index = 0; ; index++)
            {
                var data = new SpDevinfoData { Size = (uint)Marshal.SizeOf<SpDevinfoData>() };
                if (!SetupDiEnumDeviceInfo(set, index, ref data))
                {
                    if (Marshal.GetLastWin32Error() == 259) break; // ERROR_NO_MORE_ITEMS
                    continue;
                }
                var instanceBuffer = new StringBuilder(512);
                if (!SetupDiGetDeviceInstanceId(set, ref data, instanceBuffer,
                        instanceBuffer.Capacity, out _)) continue;
                var instance = instanceBuffer.ToString();
                var hardwareId = supportedIds.FirstOrDefault(id =>
                    instance.StartsWith(id, StringComparison.OrdinalIgnoreCase));
                if (hardwareId is null) continue;

                var name = SetupDeviceProperty(set, ref data, SpdrpFriendlyName);
                var description = SetupDeviceProperty(set, ref data, SpdrpDeviceDesc);
                var service = SetupDeviceProperty(set, ref data, SpdrpService);
                result.Add(new AdapterChoice(
                    string.IsNullOrWhiteSpace(name) ? description : name,
                    instance, hardwareId, service, description, true));
            }
        }
        finally
        {
            SetupDiDestroyDeviceInfoList(set);
        }
        return result;
    }

    private static string SetupDeviceProperty(nint set, ref SpDevinfoData data, uint property)
    {
        var buffer = new byte[2048];
        if (!SetupDiGetDeviceRegistryProperty(set, ref data, property, out _, buffer,
                (uint)buffer.Length, out _)) return "";
        return Encoding.Unicode.GetString(buffer).TrimEnd('\0');
    }

    private void SetupAdapterChanged(object sender, SelectionChangedEventArgs e)
    {
        if (SetupAdapterBox.SelectedItem is not AdapterChoice adapter) return;
        var binding = string.Equals(adapter.Service, "WinUSB", StringComparison.OrdinalIgnoreCase)
            ? Loc.T("setup.stack_ours") : Loc.T("setup.stack_windows");
        var support = adapter.Supported ? Loc.T("setup.supported") : Loc.T("setup.unsupported");
        SetupBindButton.IsEnabled = adapter.Supported ||
            string.Equals(adapter.Service, "WinUSB", StringComparison.OrdinalIgnoreCase);
        SetupAdapterDetails.Text = Loc.T("setup.adapter_detail", adapter.Name, adapter.HardwareId,
            string.IsNullOrWhiteSpace(adapter.Driver) ? "-" : adapter.Driver, binding, support);
    }

    private void SetupActionClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string action }) return;
        var root = FindProjectRoot();
        if (root is null)
        {
            ShowSetupError(Loc.T("setup.files_missing"));
            return;
        }

        var adapter = SetupAdapterBox.SelectedItem as AdapterChoice;
        if (action.StartsWith("adapter-", StringComparison.Ordinal) && adapter is null)
        {
            ShowSetupError(Loc.T("setup.choose_adapter"));
            return;
        }

        string script;
        string operation;
        var elevated = true;
        switch (action)
        {
            case "sign": script = Path.Combine(root, "scripts", "sign-driver.ps1"); operation = "-Sign"; break;
            case "adapter-bind": script = Path.Combine(root, "scripts", "adapter-driver.ps1"); operation = "-Bind"; break;
            case "adapter-restore": script = Path.Combine(root, "scripts", "adapter-driver.ps1"); operation = "-Restore"; break;
            case "adapter-status": script = Path.Combine(root, "scripts", "adapter-driver.ps1"); operation = "-Status"; elevated = false; break;
            case "vbcable-install": script = Path.Combine(root, "scripts", "setup-vbcable.ps1"); operation = "-Install"; break;
            case "vbcable-setup": script = Path.Combine(root, "scripts", "setup-vbcable.ps1"); operation = "-Apply"; break;
            case "vbcable-restore": script = Path.Combine(root, "scripts", "setup-vbcable.ps1"); operation = "-Restore"; break;
            case "vbcable-status": script = Path.Combine(root, "scripts", "setup-vbcable.ps1"); operation = ""; elevated = false; break;
            default: return;
        }

        try
        {
            var start = new ProcessStartInfo("powershell.exe") { UseShellExecute = true };
            if (elevated) start.Verb = "runas";
            start.ArgumentList.Add("-NoProfile");
            start.ArgumentList.Add("-ExecutionPolicy");
            start.ArgumentList.Add("Bypass");
            start.ArgumentList.Add("-NoExit");
            start.ArgumentList.Add("-File");
            start.ArgumentList.Add(script);
            if (!string.IsNullOrEmpty(operation)) start.ArgumentList.Add(operation);
            if (adapter is not null && action.StartsWith("adapter-", StringComparison.Ordinal))
            {
                start.ArgumentList.Add("-HardwareId");
                start.ArgumentList.Add(adapter.HardwareId);
            }
            Process.Start(start);
            SetupNotice.Severity = InfoBarSeverity.Informational;
            SetupNotice.Message = Loc.T("setup.started");
            SetupNotice.IsOpen = true;
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            ShowSetupError(error.Message);
        }
    }

    private void ShowSetupError(string message)
    {
        SetupNotice.Severity = InfoBarSeverity.Error;
        SetupNotice.Message = message;
        SetupNotice.IsOpen = true;
    }

    private static string? FindProjectRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            if (File.Exists(Path.Combine(directory.FullName, "README.md")) &&
                Directory.Exists(Path.Combine(directory.FullName, "scripts"))) return directory.FullName;
            var packaged = Path.Combine(directory.FullName, "release");
            if (File.Exists(Path.Combine(packaged, "README.md")) &&
                Directory.Exists(Path.Combine(packaged, "scripts"))) return packaged;
            directory = directory.Parent;
        }
        return null;
    }

    // --------------------------------------------------------------- commands

    private void AdapterToggled(object sender, RoutedEventArgs e)
    {
        if (_suppressToggle)
        {
            return;
        }

        Busy.IsActive = true;
        ScanStatus.Text = AdapterSwitch.IsOn ? Loc.T("status.turning_on") : Loc.T("status.turning_off");
        Send("adapter", new() { ["on"] = AdapterSwitch.IsOn });
    }

    private void ScanClicked(object sender, RoutedEventArgs e) => StartScan();

    private void StartScan()
    {
        if (!_adapterOn)
        {
            Append(Loc.T("log.bluetooth_first"));
            return;
        }

        _found.Clear();
        Busy.IsActive = true;
        ScanStatus.Text = Loc.T("status.scanning");
        Send("scan", new() { ["seconds"] = 6 });
    }

    private void ConnectClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string address })
        {
            return;
        }

        if (string.Equals(address, _connectedAddress, StringComparison.OrdinalIgnoreCase))
        {
            Busy.IsActive = true;
            Send("disconnect");
            return;
        }

        Busy.IsActive = true;
        Update(address, row => row.With(connecting: true));
        Send("connect", new() { ["address"] = address });
    }

    private void ForgetClicked(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string address })
        {
            Send("forget", new() { ["address"] = address });
        }
    }

    private void ResetSettingsClicked(object sender, RoutedEventArgs e)
    {
        // Force a rebuild: the keys are the same, but every value changed.
        _settingsShape = "";
        Send("reset-settings");
    }

    /// <summary>
    /// Puts the whole log on the clipboard.
    ///
    /// Selecting a scrolling log by hand loses the top of it every time, and the
    /// log is the first thing anyone is asked for when something goes wrong.
    /// </summary>
    private void CopyLogClicked(object sender, RoutedEventArgs e)
    {
        var package = new Windows.ApplicationModel.DataTransfer.DataPackage();
        package.SetText(LogText.Text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(package);

        CopyLogButton.Content = Loc.T("log.copied");

        var reset = _ui.CreateTimer();
        reset.Interval = TimeSpan.FromSeconds(2);
        reset.IsRepeating = false;
        reset.Tick += (t, _) =>
        {
            CopyLogButton.Content = Loc.T("log.copy");
            t.Stop();
        };
        reset.Start();
    }

    private void ClearLogClicked(object sender, RoutedEventArgs e) => LogText.Text = "";

    // ----------------------------------------------------------------- events

    private void OnAgentEvent(JsonElement message)
    {
        var name = Text(message, "event");

        switch (name)
        {
            case "ready":
                Append(Loc.T("log.core_ready"));
                // Fetched now, not when the page is first opened: the values are
                // then already there the moment someone looks.
                Send("settings");
                // Nobody opens a Bluetooth app to press "on". The toggle is
                // there to turn the radio off, not to make it work.
                _suppressToggle = true;
                AdapterSwitch.IsOn = true;
                _suppressToggle = false;
                Busy.IsActive = true;
                ScanStatus.Text = Loc.T("status.turning_on");
                Send("adapter", new() { ["on"] = true });
                break;

            case "status":
                ShowPaired(message);
                break;

            case "adapter":
                OnAdapter(message);
                break;

            case "device":
                AddDevice(message);
                break;

            case "paired":
                PromoteToPaired(
                    Text(message, "address"),
                    Text(message, "name"),
                    message.TryGetProperty("leAudio", out var pairedLeAudio) && pairedLeAudio.GetBoolean(),
                    false);
                break;

            case "capabilities":
                Append(Text(message, "summary"));
                break;

            case "connected":
                _connectedAddress = Text(message, "address");
                _healthSamples.Clear();
                Busy.IsActive = false;
                ShowConnectedHint();
                PromoteToPaired(_connectedAddress, Text(message, "name"), true, true);
                foreach (var pendingKey in _pendingReconnectMarkers)
                {
                    if (_savedMarkers.TryGetValue(pendingKey, out var pendingMarker))
                        pendingMarker.Opacity = 0;
                }
                _pendingReconnectMarkers.Clear();
                _startupReconnectTimer?.Stop();
                Append(Loc.T("log.connected"));
                Send("status");
                break;

            case "disconnected":
                Update(_connectedAddress, row => row.With(connected: false, streaming: false, connecting: false));
                _connectedAddress = null;
                _healthSamples.Clear();
                SignalMetric.Text = Loc.T("metrics.signal", "-");
                LossMetric.Text = Loc.T("metrics.loss", 0, "0.00 %");
                StabilityMetric.Text = Loc.T("metrics.waiting");
                Append(Loc.T("log.disconnected"));

                if (_reconnectTo is { } address)
                {
                    _reconnectTo = null;
                    _ = ReconnectAfterDelay(address);
                }
                break;

            case "reconnecting":
                Update(Text(message, "address"), row => row.With(connecting: true));
                Busy.IsActive = true;
                Append(Loc.T("log.reconnecting"));
                break;

            case "reconnect-stopped":
                Update(Text(message, "address"), row => row.With(connecting: false));
                Busy.IsActive = false;
                Append(Loc.T("log.reconnect_stopped"));
                break;

            case "streaming-started":
                Update(_connectedAddress, row => row.With(connected: true, streaming: true, connecting: false));
                Append($"Stream active, approximately {message.GetProperty("latencyMs").GetInt32()} ms latency.");
                break;

            case "streaming-stopped":
                Update(_connectedAddress, row => row.With(streaming: false));
                break;

            case "streaming":
                ShowStreaming(message);
                break;

            case "settings":
                ShowSettings(message);
                break;

            case "applied":
                OnApplied(message);
                break;

            case "log":
                Append(Text(message, "text"));
                break;

            case "error":
                Append(Text(message, "text"));
                OnCommandFailed(Text(message, "cmd"));
                break;

            case "done":
                OnCommandFinished(Text(message, "cmd"));
                break;
        }
    }

    /// <summary>
    /// One progress line, including what the capture side is actually carrying.
    ///
    /// The two channel levels are there to answer a question nothing else can:
    /// if they are identical over real music, the audio reaching this stack is
    /// mono and no amount of work on the radio will separate it.
    /// </summary>
    private void ShowStreaming(JsonElement message)
    {
        var failed = message.GetProperty("failed").GetInt64();
        var frames = message.GetProperty("frames").GetInt64();

        var line = $"playing: {frames} frames, L {Level(message, "leftDb")} / R {Level(message, "rightDb")}"
                   + $", bass {Level(message, "bassDb")} / mid {Level(message, "midDb")}"
                   + $" / treble {Level(message, "trebleDb")}";

        // Per channel, from the controller: the number that separates "both
        // streams are transmitting" from "one of them silently is not".
        if (message.TryGetProperty("delivered", out var delivered))
        {
            var counts = delivered.EnumerateArray().Select(v => v.GetInt64().ToString());
            line += $", delivered [{string.Join(", ", counts)}]";
        }
        int? rssi = null;
        if (message.TryGetProperty("rssi", out var rssiValue) && rssiValue.ValueKind == JsonValueKind.Number)
        {
            rssi = rssiValue.GetInt32();
            line += $", signal {rssi} dBm";
        }
        if (failed > 0)
        {
            line += $", SELHALO {failed}";
        }

        Append(line);

        var sent = message.TryGetProperty("sent", out var sentValue) ? sentValue.GetInt64() : frames;
        var now = DateTimeOffset.UtcNow;
        _healthSamples.Enqueue(new LinkHealthSample(now, sent, failed));
        while (_healthSamples.Count > 1 && now - _healthSamples.Peek().Time > TimeSpan.FromSeconds(60))
            _healthSamples.Dequeue();

        var oldest = _healthSamples.Peek();
        var lost60 = Math.Max(0, failed - oldest.Failed);
        var sent60 = Math.Max(0, sent - oldest.Sent);
        var lossPercent = sent60 > 0 ? lost60 * 100.0 / sent60 : 0.0;
        SignalMetric.Text = Loc.T("metrics.signal", rssi is null ? "-" : $"{rssi} dBm");
        LossMetric.Text = Loc.T("metrics.loss", lost60, $"{lossPercent:0.00} %");

        var goodSignal = rssi is null || rssi >= -70;
        var poorSignal = rssi is not null && rssi < -80;
        if (lost60 > 0 || poorSignal)
        {
            SignalMetric.Foreground = poorSignal
                ? Brush("SystemFillColorCriticalBrush") : Brush("TextFillColorPrimaryBrush");
            LossMetric.Foreground = lost60 > 0
                ? Brush("SystemFillColorCriticalBrush") : Brush("SystemFillColorSuccessBrush");
            StabilityMetric.Foreground = Brush("SystemFillColorCriticalBrush");
            StabilityMetric.Text = Loc.T("metrics.unstable");
        }
        else if (!goodSignal)
        {
            SignalMetric.Foreground = Brush("SystemFillColorCautionBrush");
            LossMetric.Foreground = Brush("SystemFillColorSuccessBrush");
            StabilityMetric.Foreground = Brush("SystemFillColorCautionBrush");
            StabilityMetric.Text = Loc.T("metrics.fair");
        }
        else
        {
            SignalMetric.Foreground = Brush("SystemFillColorSuccessBrush");
            LossMetric.Foreground = Brush("SystemFillColorSuccessBrush");
            StabilityMetric.Foreground = Brush("SystemFillColorSuccessBrush");
            StabilityMetric.Text = Loc.T("metrics.stable");
        }
    }

    private static string Level(JsonElement message, string property)
    {
        if (!message.TryGetProperty(property, out var value) || value.ValueKind == JsonValueKind.Null)
        {
            return "ticho";
        }

        return $"{value.GetDouble():0.0} dB";
    }

    private static string Text(JsonElement message, string property) =>
        message.TryGetProperty(property, out var value) ? value.GetString() ?? "" : "";

    private void OnAdapter(JsonElement message)
    {
        _adapterOn = message.GetProperty("on").GetBoolean();

        var version = Text(message, "version");
        var address = Text(message, "address");
        var detail = _adapterOn
            ? string.Join(" · ", new[] { version, address }.Where(s => s.Length > 0))
            : Loc.T("status.adapter_off");

        AdapterDetail.Text = detail;
        Append(_adapterOn ? $"Bluetooth on - {detail}" : "Bluetooth off.");

        if (_adapterOn)
        {
            Send("status");
            return;
        }

        // Off means off: nothing is being searched for, and nothing found
        // earlier is still there to click on.
        Busy.IsActive = false;
        ScanStatus.Text = Loc.T("status.off");
        _found.Clear();
        _connectedAddress = null;

        for (var i = 0; i < _paired.Count; i++)
        {
            _paired[i] = _paired[i].With(connected: false, streaming: false, connecting: false);
        }
    }

    private void OnCommandFinished(string command)
    {
        switch (command)
        {
            case "adapter":
                Busy.IsActive = false;
                if (_adapterOn)
                {
                    // Scanning is what the user came for; it follows the radio
                    // coming up without anyone having to ask for it.
                    StartScan();
                }
                break;

            case "scan":
                Busy.IsActive = false;
                ScanStatus.Text = _found.Count == 0
                    ? Loc.T("status.none_found")
                    : Loc.T("status.found", _found.Count);
                break;

            case "connect":
            case "disconnect":
            case "forget":
                Busy.IsActive = false;
                break;
        }
    }

    private void OnCommandFailed(string command)
    {
        Busy.IsActive = false;

        if (command == "connect")
        {
            for (var i = 0; i < _paired.Count; i++)
            {
                _paired[i] = _paired[i].With(connecting: false);
            }
            for (var i = 0; i < _found.Count; i++)
            {
                _found[i] = _found[i].With(connecting: false);
            }
        }
        else if (command == "scan")
        {
            ScanStatus.Text = Loc.T("status.scan_failed");
        }
    }

    // ---------------------------------------------------------------- devices

    private void ShowPaired(JsonElement message)
    {
        if (!message.TryGetProperty("paired", out var paired))
        {
            return;
        }

        var rows = paired.EnumerateArray().Select(device =>
        {
            var address = Text(device, "address");
            var previous = _paired.FirstOrDefault(row => AddressesMatch(row.Address, address));
            var reported = Text(device, "name");
            return new DeviceRow
            {
                Address = address,
                Name = UsefulDeviceName(reported, address) ? reported : previous?.Name ?? address,
                LeAudio = device.GetProperty("leAudio").GetBoolean() || previous?.LeAudio == true,
                Paired = true,
                Connected = string.Equals(address, _connectedAddress, StringComparison.OrdinalIgnoreCase),
                Streaming = previous?.Streaming == true &&
                            string.Equals(address, _connectedAddress, StringComparison.OrdinalIgnoreCase),
                Connecting = previous?.Connecting == true,
                Rssi = previous?.Rssi ?? 0,
            };
        }).ToList();

        _paired.Clear();
        foreach (var row in rows)
        {
            _paired.Add(row);
        }

        var pairedAddresses = rows.Select(row => row.Address).ToHashSet(StringComparer.OrdinalIgnoreCase);
        for (var index = _found.Count - 1; index >= 0; index--)
        {
            if (pairedAddresses.Contains(_found[index].Address))
                _found.RemoveAt(index);
        }

        PairedSection.Visibility = rows.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private void PromoteToPaired(string address, string reportedName, bool leAudio, bool connected)
    {
        if (string.IsNullOrWhiteSpace(address)) return;

        var found = _found.FirstOrDefault(row => AddressesMatch(row.Address, address));
        if (found is not null) _found.Remove(found);

        var existing = _paired.FirstOrDefault(row => AddressesMatch(row.Address, address));
        var source = existing ?? found ?? new DeviceRow { Address = address, Name = address };
        var name = UsefulDeviceName(reportedName, address) ? reportedName : source.Name;
        var promoted = source.With(
            name: name,
            leAudio: leAudio || source.LeAudio,
            paired: true,
            connected: connected,
            connecting: !connected && source.Connecting,
            streaming: connected && source.Streaming);

        if (existing is null)
            _paired.Add(promoted);
        else
            _paired[_paired.IndexOf(existing)] = promoted;

        PairedSection.Visibility = Visibility.Visible;
    }

    private void AddDevice(JsonElement message)
    {
        var address = Text(message, "address");
        var alreadyKnown = _paired.Any(p => AddressesMatch(p.Address, address));

        // A paired device is already listed above; showing it twice is noise.
        // Its signal strength is still worth having, so it is folded in.
        if (alreadyKnown)
        {
            var reported = Text(message, "name");
            Update(address, row => row.With(
                name: UsefulDeviceName(reported, address) ? reported : row.Name,
                leAudio: message.GetProperty("leAudio").GetBoolean() || row.LeAudio,
                rssi: message.GetProperty("rssi").GetInt32()));
            return;
        }

        var newRow = new DeviceRow
        {
            Address = address,
            Name = Text(message, "name"),
            Rssi = message.GetProperty("rssi").GetInt32(),
            LeAudio = message.GetProperty("leAudio").GetBoolean(),
            Paired = message.GetProperty("paired").GetBoolean(),
        };

        var existing = _found.FirstOrDefault(d => AddressesMatch(d.Address, address));
        if (existing is not null)
        {
            _found[_found.IndexOf(existing)] = newRow;
            return;
        }

        // LE Audio first, then by signal: the headphones someone is looking for
        // should not be listed below a fridge magnet.
        var insertAt = _found.Count;
        for (var i = 0; i < _found.Count; i++)
        {
            if (Rank(newRow) < Rank(_found[i]))
            {
                insertAt = i;
                break;
            }
        }

        _found.Insert(insertAt, newRow);
    }

    private static int Rank(DeviceRow row) => (row.LeAudio ? 0 : 100_000) + -row.Rssi;

    private static bool AddressesMatch(string a, string b) =>
        string.Equals(a, b, StringComparison.OrdinalIgnoreCase);

    /// <summary>Applies a change to whichever list holds this device.</summary>
    private void Update(string? address, Func<DeviceRow, DeviceRow> change)
    {
        if (string.IsNullOrEmpty(address))
        {
            return;
        }

        foreach (var list in new[] { _paired, _found })
        {
            for (var i = 0; i < list.Count; i++)
            {
                if (AddressesMatch(list[i].Address, address))
                {
                    list[i] = change(list[i]);
                }
            }
        }
    }

    // --------------------------------------------------------------- settings

    /// <summary>
    /// Says, while something is connected, that stream settings need a reconnect
    /// - and offers one.
    /// </summary>
    private void ShowConnectedHint()
    {
        if (_connectedAddress is null || SettingsNotice.IsOpen)
        {
            return;
        }

        SettingsNotice.Severity = InfoBarSeverity.Informational;
        SettingsNotice.Message = Loc.T("settings.connected_hint");

        var button = new Button { Content = Loc.T("settings.reconnect") };
        button.Click += ReconnectClicked;
        SettingsNotice.ActionButton = button;
        SettingsNotice.IsOpen = true;
    }

    /// <summary>
    /// One extensible source of truth for category identity, grouping, visuals
    /// and contents. Adding a category is one entry here; layout and filters
    /// consume the metadata rather than maintaining their own parallel lists.
    /// </summary>
    private sealed record SectionDefinition(
        string Id, string Group, int Panel, string TitleKey, string SubtitleKey,
        string Glyph, SolidColorBrush Accent, string[] Keys);

    private static readonly SectionDefinition[] Sections =
    {
        new("playback", "audio", 2, "section.playback", "section.playback.sub", "\uE767", Accent(45, 140, 255),
            new[] { "playback_source", "audio_mode", "swap_channels", "gain" }),
        new("codec", "audio", 0, "section.codec", "section.codec.sub", "\uE8D6", Accent(145, 102, 224),
            new[] { "rate_hz", "frame_ms", "octets" }),
        new("radio", "audio", 0, "section.radio", "section.radio.sub", "\uE701", Accent(0, 168, 120),
            new[] { "phy", "retransmissions", "max_latency_ms", "presentation_delay_ms" }),
        new("connection", "connection", 1, "section.connection", "section.connection.sub", "\uE702", Accent(245, 158, 11),
            new[] { "reconnect_enabled", "startup_reconnect_enabled", "reconnect_interval_s", "reconnect_window_min", "idle_timeout_min", "device" }),
        new("microphone", "connection", 1, "section.microphone", "section.microphone.sub", "\uE720", Accent(224, 82, 141),
            new[] { "microphone_mode", "microphone_quality", "microphone_target",
                    "microphone_gain", "monitor_enabled", "monitor_source", "monitor_mode", "monitor_gain" }),
        new("application", "application", 2, "section.application", "section.application.sub", "\uE8A7", Accent(20, 184, 166),
            new[] { "run_in_background", "start_with_windows" }),
        new("diagnostics", "application", 2, "section.tuning", "section.tuning.sub", "\uE713", Accent(100, 116, 139),
            new[] { "diagnostics", "command_style" }),
    };

    private static readonly SectionDefinition OtherSection = new(
        "other", "application", 2, "section.other", "section.other", "\uE946", Accent(90, 140, 200), Array.Empty<string>());

    private static readonly SectionDefinition LanguageSection = new(
        "language", "language", 0, "section.language", "section.language.sub", "\uE774",
        Accent(99, 102, 241), new[] { "language" });

    private void ShowSettings(JsonElement message)
    {
        var shape = string.Join(",", message.GetProperty("knobs")
            .EnumerateArray()
            .Select(k => Text(k, "key") + (k.TryGetProperty("options", out var options)
                ? options.GetRawText()
                : "")));

        // Rebuilding on every reply throws away whatever is being edited right
        // now. The page only needs rebuilding when the set of settings changes.
        if (shape == _settingsShape && _settingCards.Count > 0)
        {
            return;
        }
        _settingsShape = shape;

        _populating = true;
        SettingsHost.Children.Clear();
        SettingsHost.ColumnDefinitions.Clear();
        SettingsHost.RowDefinitions.Clear();
        _settingCards.Clear();
        _settingCardPanels.Clear();
        _savedMarkers.Clear();
        PresetHost.Children.Clear();
        _presetBox = null;
        LanguageHost.Children.Clear();
        ShowConnectedHint();

        var knobs = message.GetProperty("knobs")
            .EnumerateArray()
            .ToDictionary(k => Text(k, "key"));
        if (knobs.TryGetValue("language", out var languageKnob))
        {
            Loc.SetLanguage(Text(languageKnob, "value"));
            Loc.Apply(Nav);
        }
        _customPreset = knobs.TryGetValue("preset", out var presetKnob)
            && Text(presetKnob, "value") == "custom";
        _runInBackground = knobs.TryGetValue("run_in_background", out var backgroundKnob)
            && Text(backgroundKnob, "value") is "true" or "1";
        _startupReconnectEnabled = !knobs.TryGetValue("startup_reconnect_enabled", out var startupReconnectKnob)
            || Text(startupReconnectKnob, "value") is "true" or "1";

        var placed = new HashSet<string>();

        if (knobs.TryGetValue("preset", out presetKnob))
        {
            PresetHost.Children.Add(BuildMainPreset(Text(presetKnob, "value"),
                Text(presetKnob, "description"), Text(presetKnob, "scope")));
            placed.Add("preset");
        }

        // Language is a first-class page in the main navigation. It still uses
        // the same setting renderer and protocol as every other knob, but it no
        // longer appears as an unrelated card among audio settings.
        if (knobs.TryGetValue("language", out languageKnob))
        {
            LanguageHost.Children.Add(BuildSection(LanguageSection, new[] { languageKnob }));
            placed.Add("language");
        }

        foreach (var section in Sections)
        {
            var present = section.Keys.Where(knobs.ContainsKey).ToList();
            if (present.Count == 0)
            {
                continue;
            }

            var card = BuildSection(section, present.Select(key => knobs[key]));
            _settingCards.Add(card);
            _settingCardPanels[card] = section.Panel;
            foreach (var key in present) placed.Add(key);
        }

        // Anything the core knows about but this list has not been taught yet.
        // Better shown in a leftover section than silently missing.
        var leftovers = knobs.Where(pair => !placed.Contains(pair.Key)).ToList();
        if (leftovers.Count > 0)
        {
            var card = BuildSection(OtherSection, leftovers.Select(x => x.Value));
            _settingCards.Add(card);
            _settingCardPanels[card] = OtherSection.Panel;
        }

        _populating = false;
        LayoutSettingsCards();
    }

    private static bool UsefulDeviceName(string name, string address) =>
        !string.IsNullOrWhiteSpace(name) &&
        !string.Equals(name, address, StringComparison.OrdinalIgnoreCase) &&
        !name.Contains("unnamed", StringComparison.OrdinalIgnoreCase) &&
        !name.Contains("bez jmena", StringComparison.OrdinalIgnoreCase);

    private FrameworkElement BuildSection(SectionDefinition section, IEnumerable<JsonElement> knobs)
    {
        var content = new StackPanel { Spacing = 0 };
        var subtitle = Loc.T(section.SubtitleKey);
        var localizedTitle = Loc.T(section.TitleKey);
        var accent = section.Accent;
        var heading = new Grid { ColumnSpacing = 12, Margin = new Thickness(0, 0, 0, 8) };
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var icon = new Border
        {
            Width = 34,
            Height = 34,
            CornerRadius = new CornerRadius(8),
            Background = accent,
            Child = new FontIcon
            {
                Glyph = section.Glyph,
                FontSize = 16,
                Foreground = Brush("TextOnAccentFillColorPrimaryBrush"),
            },
        };
        heading.Children.Add(icon);

        var headingText = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        headingText.Children.Add(new TextBlock
        {
            Text = localizedTitle,
            FontSize = 18,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        });
        headingText.Children.Add(new TextBlock
        {
            Text = subtitle,
            FontSize = 11,
            Foreground = Brush("TextFillColorSecondaryBrush"),
            TextWrapping = TextWrapping.Wrap,
        });
        Grid.SetColumn(headingText, 1);
        heading.Children.Add(headingText);
        content.Children.Add(heading);
        content.Children.Add(new Border
        {
            Height = 2,
            CornerRadius = new CornerRadius(1),
            Background = accent,
            Opacity = 0.72,
            Margin = new Thickness(0, 2, 0, 4),
        });

        foreach (var knob in knobs)
        {
            content.Children.Add(Build(knob));
        }

        return new Border
        {
            Background = Brush("CardBackgroundFillColorDefaultBrush"),
            BorderBrush = Brush("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10),
            Padding = new Thickness(18, 16, 18, 8),
            VerticalAlignment = VerticalAlignment.Top,
            Child = content,
        };
    }

    private static SolidColorBrush Accent(byte r, byte g, byte b) =>
        new(Windows.UI.Color.FromArgb(255, r, g, b));

    private FrameworkElement BuildMainPreset(string value, string description, string scope)
    {
        var accent = Accent(45, 140, 255);
        var grid = new Grid { ColumnSpacing = 10 };
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(2, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition
        {
            Width = new GridLength(1, GridUnitType.Star),
            MaxWidth = 380,
        });

        grid.Children.Add(new Border
        {
            Width = 30, Height = 30, CornerRadius = new CornerRadius(7), Background = accent,
            VerticalAlignment = VerticalAlignment.Center,
            Child = new FontIcon
            {
                Glyph = "\uE9D9", FontSize = 14,
                Foreground = Brush("TextOnAccentFillColorPrimaryBrush"),
            },
        });

        var labels = new StackPanel { Spacing = 0, VerticalAlignment = VerticalAlignment.Center };
        labels.Children.Add(new TextBlock
        {
            Text = Loc.T("settings.main_preset"), FontSize = 14,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        labels.Children.Add(new TextBlock
        {
            Text = Description("preset", description), FontSize = 11,
            Foreground = Brush("TextFillColorSecondaryBrush"),
            TextWrapping = TextWrapping.NoWrap, TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        });
        Grid.SetColumn(labels, 1);
        grid.Children.Add(labels);

        var control = new Grid { VerticalAlignment = VerticalAlignment.Stretch, ColumnSpacing = 8 };
        control.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        control.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _presetBox = Choice("preset", value, new[]
        {
            ("windows", Loc.T("choice.windows")),
            ("high-quality", Loc.T("choice.high_quality")),
            ("low-latency", Loc.T("choice.low_latency")),
            ("robust", Loc.T("choice.robust")),
            ("custom", Loc.T("choice.custom")),
        });
        _presetBox.VerticalAlignment = VerticalAlignment.Center;
        _presetBox.HorizontalAlignment = HorizontalAlignment.Stretch;
        _presetBox.MinWidth = 190;
        ToolTipService.SetToolTip(_presetBox,
            $"{Description("preset", description)}\n{Scope(scope)}");
        control.Children.Add(_presetBox);
        var presetMarker = new TextBlock
        {
            Text = "●",
            FontSize = 11,
            Opacity = 0,
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = Brush("SystemFillColorSuccessBrush"),
        };
        Grid.SetColumn(presetMarker, 1);
        control.Children.Add(presetMarker);
        _savedMarkers["preset"] = presetMarker;
        RestorePendingMarker("preset", presetMarker);
        Grid.SetColumn(control, 2);
        grid.Children.Add(control);

        // Keep the preset readable in narrow windows without imposing a fixed
        // width on the description. The selector moves below the labels rather
        // than squeezing or clipping translated text.
        void ArrangePreset(double width)
        {
            var compact = width > 0 && width < 720;
            Grid.SetRow(control, compact ? 1 : 0);
            Grid.SetColumn(control, compact ? 0 : 2);
            Grid.SetColumnSpan(control, compact ? 3 : 1);
            control.Margin = compact ? new Thickness(0, 6, 0, 0) : new Thickness(0);
            _presetBox.MinWidth = compact ? 0 : 190;
        }
        grid.SizeChanged += (_, e) => ArrangePreset(e.NewSize.Width);
        ArrangePreset(grid.ActualWidth);

        return new Border
        {
            Background = Brush("CardBackgroundFillColorDefaultBrush"),
            BorderBrush = accent, BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(8), Padding = new Thickness(10, 6, 10, 6),
            Child = grid,
        };
    }


    private void LayoutSettingsCards()
    {
        if (_settingCards.Count == 0) return;

        // Detach cards before their former parents are discarded. A visual can
        // only have one parent; rebuilding after a language change reuses the
        // same card instances until the fresh model arrives.
        DetachSettingsCards(SettingsHost);
        SettingsHost.Children.Clear();
        SettingsHost.ColumnDefinitions.Clear();
        SettingsHost.RowDefinitions.Clear();
        SettingsHost.MaxWidth = double.PositiveInfinity;
        SettingsHost.HorizontalAlignment = HorizontalAlignment.Stretch;
        var panelColumns = SettingsColumnCount(SettingsHost.ActualWidth);
        _settingsPanelColumns = panelColumns;
        var panelRows = (int)Math.Ceiling(3d / panelColumns);
        for (var row = 0; row < panelRows; row++)
            SettingsHost.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        for (var column = 0; column < panelColumns; column++)
            SettingsHost.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var panelTitles = new[]
        {
            ("settings.panel.quality", "settings.panel.quality.sub", Accent(145, 102, 224)),
            ("settings.panel.connection", "settings.panel.connection.sub", Accent(245, 158, 11)),
            ("settings.panel.application", "settings.panel.application.sub", Accent(20, 184, 166)),
        };
        var stacks = new List<StackPanel>();
        for (var panelIndex = 0; panelIndex < 3; panelIndex++)
        {
            var stack = new StackPanel { Spacing = 16, VerticalAlignment = VerticalAlignment.Top };
            var panel = SettingsPanel(panelTitles[panelIndex].Item1, panelTitles[panelIndex].Item2,
                panelTitles[panelIndex].Item3, stack);
            Grid.SetColumn(panel, panelIndex % panelColumns);
            Grid.SetRow(panel, panelIndex / panelColumns);
            if (panelColumns == 2 && panelIndex == 2) Grid.SetColumnSpan(panel, 2);
            SettingsHost.Children.Add(panel);
            stacks.Add(stack);
        }
        foreach (var card in _settingCards)
            stacks[Math.Clamp(_settingCardPanels.GetValueOrDefault(card), 0, 2)].Children.Add(card);
    }

    private static int SettingsColumnCount(double width) => width switch
    {
        >= 1050 => 3,
        >= 700 => 2,
        _ => 1,
    };

    private void SettingsHostSizeChanged(object sender, SizeChangedEventArgs e)
    {
        var columns = SettingsColumnCount(e.NewSize.Width);
        if (_settingCards.Count > 0 && columns != _settingsPanelColumns)
            LayoutSettingsCards();
    }

    private void AboutDetailsGridSizeChanged(object sender, SizeChangedEventArgs e)
    {
        var compact = e.NewSize.Width > 0 && e.NewSize.Width < 800;
        Grid.SetRow(AboutAseCard, 0);
        Grid.SetColumn(AboutAseCard, 0);
        Grid.SetColumnSpan(AboutAseCard, compact ? 2 : 1);
        Grid.SetRow(AboutWindowsCard, compact ? 1 : 0);
        Grid.SetColumn(AboutWindowsCard, compact ? 0 : 1);
        Grid.SetColumnSpan(AboutWindowsCard, compact ? 2 : 1);
    }

    private void AboutMappingGridSizeChanged(object sender, SizeChangedEventArgs e)
    {
        var compact = e.NewSize.Width > 0 && e.NewSize.Width < 800;
        Grid.SetRow(AboutMappingCard, 0);
        Grid.SetColumn(AboutMappingCard, 0);
        Grid.SetColumnSpan(AboutMappingCard, compact ? 2 : 1);
        Grid.SetRow(AboutStackCard, compact ? 1 : 0);
        Grid.SetColumn(AboutStackCard, compact ? 0 : 1);
        Grid.SetColumnSpan(AboutStackCard, compact ? 2 : 1);
    }

    private FrameworkElement SettingsPanel(string titleKey, string subtitleKey,
        SolidColorBrush accent, StackPanel content)
    {
        var panel = new Grid { RowSpacing = 10 };
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        panel.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        var title = new Grid { ColumnSpacing = 10 };
        title.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(4) });
        title.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        title.Children.Add(new Border { Background = accent, CornerRadius = new CornerRadius(2) });
        var text = new StackPanel { Spacing = 1 };
        text.Children.Add(new TextBlock
        {
            Text = Loc.T(titleKey), FontSize = 16,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        });
        text.Children.Add(new TextBlock
        {
            Text = Loc.T(subtitleKey), FontSize = 11, TextWrapping = TextWrapping.Wrap,
            Foreground = Brush("TextFillColorSecondaryBrush"),
        });
        Grid.SetColumn(text, 1);
        title.Children.Add(text);
        var header = new Border
        {
            Background = Brush("CardBackgroundFillColorDefaultBrush"),
            BorderBrush = Brush("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(9),
            Padding = new Thickness(14, 10, 14, 10), Child = title,
        };
        panel.Children.Add(header);

        var scroller = SettingsScroller(content, new Thickness(0, 0, 8, 0));
        Grid.SetRow(scroller, 1);
        panel.Children.Add(scroller);
        return panel;
    }

    private static ScrollViewer SettingsScroller(UIElement content, Thickness padding) => new()
    {
        Content = content,
        Padding = padding,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        VerticalScrollMode = ScrollMode.Enabled,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        HorizontalScrollMode = ScrollMode.Disabled,
        ZoomMode = ZoomMode.Disabled,
    };

    private void DetachSettingsCards(DependencyObject root)
    {
        foreach (var stack in Descendants(root).OfType<StackPanel>())
        {
            // Remove only top-level cards. Section contents also contain Border
            // separators and setting rows and must remain untouched.
            foreach (var card in _settingCards.Where(stack.Children.Contains).ToList())
                stack.Children.Remove(card);
        }

        static IEnumerable<DependencyObject> Descendants(DependencyObject parent)
        {
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(parent); i++)
            {
                var child = VisualTreeHelper.GetChild(parent, i);
                yield return child;
                foreach (var descendant in Descendants(child)) yield return descendant;
            }
        }
    }

    private FrameworkElement Build(JsonElement knob)
    {
        var options = knob.TryGetProperty("options", out var values)
            ? values.EnumerateArray()
                .Select(option => (Text(option, "value"), Text(option, "label")))
                .ToArray()
            : Array.Empty<(string, string)>();
        return BuildKnob(Text(knob, "key"), Text(knob, "value"),
            Text(knob, "description"), Text(knob, "scope"), options);
    }

    private static Microsoft.UI.Xaml.Media.Brush Brush(string key) =>
        (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources[key];

    private FrameworkElement BuildKnob(string key, string value, string description, string scope,
        (string Value, string Label)[] dynamicOptions)
    {
        var grid = new Grid { ColumnSpacing = 16 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var labels = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        labels.Children.Add(new TextBlock { Text = Label(key), FontSize = 14, TextWrapping = TextWrapping.Wrap });
        var tradeoff = Tradeoff(key);
        labels.Children.Add(new TextBlock
        {
            Text = string.IsNullOrEmpty(tradeoff) ? Description(key, description) : $"{Description(key, description)}\n{tradeoff}",
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Foreground = Brush("TextFillColorSecondaryBrush"),
        });
        // Saying when a change takes effect is the whole reason the backend
        // reports a scope. Leaving it out is how a control gets a reputation
        // for doing nothing.
        labels.Children.Add(new TextBlock
        {
            Text = Scope(scope),
            FontSize = 11,
            Foreground = Brush("TextFillColorTertiaryBrush"),
            TextWrapping = TextWrapping.Wrap,
        });
        Grid.SetColumn(labels, 0);
        grid.Children.Add(labels);

        FrameworkElement control = key switch
        {
            "language" => LanguageControl(value),
            "preset" => Choice(key, value, new[]
            {
                ("windows", Loc.T("choice.windows")),
                ("high-quality", Loc.T("choice.high_quality")),
                ("low-latency", Loc.T("choice.low_latency")),
                ("robust", Loc.T("choice.robust")),
                ("custom", Loc.T("choice.custom")),
            }),
            "rate_hz" => Choice(key, value, new[] { "48000", "32000", "24000", "16000" }),
            "frame_ms" => Choice(key, value, new[] { "10", "7.5" }),
            "phy" => Choice(key, value, new[] { "2M", "1M" }),
            "octets" => SliderNumber(key, value, 20, 155, 1, Loc.T("slider.economical"), Loc.T("slider.detail")),
            "retransmissions" => SliderNumber(key, value, 0, 15, 1, Loc.T("slider.faster"), Loc.T("slider.resilient")),
            "max_latency_ms" => SliderNumber(key, value, 5, 200, 5, Loc.T("slider.lower_latency"), Loc.T("slider.more_headroom")),
            "presentation_delay_ms" => SliderNumber(key, value, 10, 200, 5, Loc.T("slider.faster"), Loc.T("slider.stable")),
            "gain" => SliderNumber(key, value, 0, 2, 0.05, Loc.T("slider.silent"), Loc.T("slider.boost")),
            "idle_timeout_min" => SliderNumber(key, value, 0, 120, 1, Loc.T("slider.never"), Loc.T("slider.longer")),
            "reconnect_interval_s" => SliderNumber(key, value, 1, 60, 1, Loc.T("slider.often"), Loc.T("slider.gentle")),
            "reconnect_window_min" => SliderNumber(key, value, 0, 60, 1, Loc.T("slider.unlimited"), Loc.T("slider.limited")),
            "audio_mode" => Choice(key, value, new[]
            {
                ("stereo", Loc.T("choice.stereo")),
                ("legacy", Loc.T("choice.legacy")),
                ("mono", Loc.T("choice.mono")),
            }),
            "playback_source" or "monitor_source" when dynamicOptions.Length > 0 =>
                Choice(key, value, dynamicOptions),
            "microphone_mode" => Choice(key, value, new[]
            {
                ("off", Loc.T("choice.mic_off")),
                ("on", Loc.T("choice.mic_on")),
            }),
            "microphone_quality" => Choice(key, value, new[]
            {
                ("voice", Loc.T("choice.mic_voice")),
                ("balanced", Loc.T("choice.mic_balanced")),
                ("high", Loc.T("choice.mic_high")),
            }),
            "microphone_target" => Choice(key, value, new[]
            {
                ("none", Loc.T("choice.mic_no_target")),
                ("vb-cable", Loc.T("choice.mic_vb_cable")),
                ("vb-cable-a", Loc.T("choice.mic_vb_cable_a")),
                ("vb-cable-b", Loc.T("choice.mic_vb_cable_b")),
            }),
            "monitor_mode" => Choice(key, value, new[]
            {
                ("mix", Loc.T("choice.monitor_mix")),
                ("replace", Loc.T("choice.monitor_replace")),
            }),
            "microphone_gain" => SliderNumber(key, value, 0, 2, 0.05, Loc.T("slider.silent"), Loc.T("slider.boost")),
            "monitor_gain" => SliderNumber(key, value, 0, 2, 0.05, Loc.T("slider.silent"), Loc.T("slider.boost")),
            "command_style" => Choice(key, value, new[]
            {
                ("class-device", Loc.T("choice.class_device")),
                ("windows-standard", Loc.T("choice.windows_command")),
                ("class-interface", Loc.T("choice.class_interface")),
            }),
            "reconnect_enabled" or "startup_reconnect_enabled" or "diagnostics" or "swap_channels" or
                "monitor_enabled" or
                "run_in_background" or "start_with_windows" => Switch(key, value),
            _ => TextField(key, value),
        };

        // Preset-controlled values stay fully sharp and clickable. Editing one
        // transparently switches to Custom before saving the value; a control
        // that looks enabled must always actually be usable.
        if (PresetControlledKeys.Contains(key) && !_customPreset)
            ToolTipService.SetToolTip(control, Loc.T("settings.custom_on_edit"));

        control.VerticalAlignment = VerticalAlignment.Center;

        // A tick beside the control that just saved. A page-wide banner cannot
        // say which of ten settings it meant, which is most of why saving felt
        // unreliable: it did save, and nothing said so next to the thing edited.
        var saved = new TextBlock
        {
            Text = Loc.T("common.saved"),
            FontSize = 11,
            Opacity = 0,
            HorizontalAlignment = HorizontalAlignment.Right,
            Foreground = Brush("SystemFillColorSuccessBrush"),
        };
        _savedMarkers[key] = saved;
        RestorePendingMarker(key, saved);

        // Controls with variable or translated text always live below their
        // explanation. Keeping a ComboBox beside a paragraph squeezed the
        // paragraph into a different number of lines and made opening a menu
        // look as if the whole card had been rearranged.
        var wide = control is ComboBox or TextBox or ToggleSwitch || key is "language" or
            "octets" or "retransmissions" or "max_latency_ms" or
            "presentation_delay_ms" or "gain" or "idle_timeout_min" or
            "reconnect_interval_s" or "reconnect_window_min" or "microphone_gain" or "monitor_gain";
        var column = new StackPanel
        {
            Spacing = 2,
            HorizontalAlignment = wide ? HorizontalAlignment.Stretch : HorizontalAlignment.Right,
        };
        column.Children.Add(control);
        column.Children.Add(saved);

        if (wide)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            Grid.SetColumnSpan(labels, 2);
            Grid.SetRow(column, 1);
            Grid.SetColumnSpan(column, 2);
        }
        else
        {
            Grid.SetColumn(column, 1);
        }
        grid.Children.Add(column);

        return new Border
        {
            BorderBrush = Brush("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Padding = new Thickness(0, 10, 0, 10),
            Child = grid,
        };
    }

    private static string Label(string key) => Loc.T("setting." + key);

    private static string Description(string key, string fallback)
    {
        var translated = Loc.T("desc." + key);
        return translated == "desc." + key ? fallback : translated;
    }

    private static string Scope(string scope) => scope switch
    {
        "applies immediately" => Loc.T("scope.immediately"),
        "applies after reconnecting the headphones" => Loc.T("scope.reconnect"),
        "applies after restarting the adapter" => Loc.T("scope.adapter"),
        _ => scope,
    };

    private static string LegacyLabel(string key) => key switch
    {
        "preset" => "Kvalita LC3",
        "audio_mode" => "Audio mode",
        "swap_channels" => "Swap left and right",
        "rate_hz" => "Sample rate",
        "frame_ms" => "Frame duration (ms)",
        "octets" => "Octets per frame (bitrate)",
        "phy" => "Radio mode",
        "retransmissions" => "Retransmissions",
        "max_latency_ms" => "Strop latence linku (ms)",
        "presentation_delay_ms" => "Presentation delay (ms)",
        "diagnostics" => "Diagnostika ASE",
        "device" => "Preferred device",
        "gain" => "Pre-encoder gain",
        "idle_timeout_min" => "Uspat po tichu (minuty)",
        "reconnect_enabled" => "Automatic reconnect",
        "startup_reconnect_enabled" => "Connect after Windows startup",
        "reconnect_interval_s" => "Retry interval (seconds)",
        "reconnect_window_min" => "Retry window (minutes)",
        "command_style" => "HCI command addressing",
        "microphone_mode" => "Headset microphone",
        "microphone_quality" => "Kvalita mikrofonu",
        "microphone_target" => "Microphone target",
        "microphone_monitor" => "Odposlech mikrofonu",
        "microphone_keep_playback" => "Keep playback active",
        "microphone_gain" => "Hlasitost mikrofonu",
        "run_in_background" => "Run in background",
        "start_with_windows" => "Start with Windows",
        _ => key,
    };

    private static string Tradeoff(string key)
    {
        var translated = Loc.T("trade." + key);
        return translated == "trade." + key ? "" : translated;
    }

    private ComboBox Choice(string key, string value, string[] options)
    {
        var box = StableComboBox();

        foreach (var option in options)
        {
            box.Items.Add(option);
        }

        box.SelectedItem = options.Contains(value) ? value : options[0];
        box.SelectionChanged += (_, _) => ApplyFromControl(key, box.SelectedItem as string ?? options[0]);
        return box;
    }

    private ComboBox Choice(string key, string value, (string Value, string Label)[] options)
    {
        var box = StableComboBox();
        foreach (var option in options)
        {
            box.Items.Add(TranslatedComboItem(option.Label, option.Value));
        }
        var selectedIndex = Array.FindIndex(options, option =>
            option.Value == value || option.Label.Equals(value, StringComparison.OrdinalIgnoreCase) ||
            option.Label.Contains(value, StringComparison.OrdinalIgnoreCase));
        box.SelectedIndex = Math.Max(0, selectedIndex);
        box.SelectionChanged += (_, _) =>
        {
            if (box.SelectedItem is ComboBoxItem item && item.Tag is string selected)
                ApplyFromControl(key, selected);
        };
        return box;
    }

    private FrameworkElement LanguageControl(string value)
    {
        var panel = new StackPanel { Spacing = 8, HorizontalAlignment = HorizontalAlignment.Stretch };
        var choices = Loc.Languages.ToArray();
        var box = StableComboBox();
        foreach (var language in choices)
            box.Items.Add(TranslatedComboItem(language.Name, language.Code));
        box.SelectedIndex = Math.Max(0, Array.FindIndex(choices, x => x.Code == value));
        box.SelectionChanged += (_, _) =>
        {
            if (box.SelectedItem is ComboBoxItem { Tag: string code }) Apply("language", code);
        };
        panel.Children.Add(box);

        var actions = new StackPanel { Spacing = 8 };
        var import = new Button { Content = Loc.T("language.import"), HorizontalAlignment = HorizontalAlignment.Stretch };
        import.Click += ImportLanguageClicked;
        var export = new Button { Content = Loc.T("language.export"), HorizontalAlignment = HorizontalAlignment.Stretch };
        export.Click += ExportLanguageClicked;
        actions.Children.Add(import);
        actions.Children.Add(export);
        panel.Children.Add(actions);
        return panel;
    }

    private async void ImportLanguageClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add(".json");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
            var file = await picker.PickSingleFileAsync();
            if (file is null) return;
            var code = Loc.Import(file.Path);
            Loc.SetLanguage(code);
            Apply("language", code);
            Relocalize();
            LanguageNotice.Severity = InfoBarSeverity.Success;
            LanguageNotice.Title = Loc.T("common.success");
            LanguageNotice.Message = Loc.T("language.imported", code);
            LanguageNotice.IsOpen = true;
        }
        catch (Exception error)
        {
            LanguageNotice.Severity = InfoBarSeverity.Error;
            LanguageNotice.Title = Loc.T("common.error");
            LanguageNotice.Message = Loc.T("language.import_error", error.Message);
            LanguageNotice.IsOpen = true;
        }
    }

    private async void ExportLanguageClicked(object sender, RoutedEventArgs e)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
            SuggestedFileName = "OpenLEAudio-language-template",
        };
        picker.FileTypeChoices.Add(Loc.T("language.json"), new List<string> { ".json" });
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        var file = await picker.PickSaveFileAsync();
        if (file is null) return;
        Loc.ExportTemplate(file.Path);
        LanguageNotice.Severity = InfoBarSeverity.Success;
        LanguageNotice.Title = Loc.T("common.success");
        LanguageNotice.Message = Loc.T("language.exported");
        LanguageNotice.IsOpen = true;
    }

    private void Relocalize()
    {
        Loc.Apply(Nav);
        RefreshTrayLanguage();
        for (var i = 0; i < _paired.Count; i++) _paired[i] = _paired[i].With();
        for (var i = 0; i < _found.Count; i++) _found[i] = _found[i].With();
        _settingsShape = "";
        Send("settings");
    }

    private static ComboBoxItem TranslatedComboItem(string label, string value) => new()
    {
        Tag = value,
        Content = new TextBlock
        {
            Text = label,
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 380,
        },
    };

    /// <summary>
    /// A full-width selector with an opaque popup. WinUI's default acrylic
    /// dropdown blurs the text under it, which is attractive in a simple page
    /// but unreadable over this information-dense settings grid.
    /// </summary>
    private ComboBox StableComboBox()
    {
        var box = new ComboBox
        {
            MinWidth = 190,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            MaxDropDownHeight = 360,
        };
        box.Resources["ComboBoxDropDownBackground"] = Brush("SolidBackgroundFillColorBaseBrush");
        box.Resources["ComboBoxDropDownBorderBrush"] = Brush("CardStrokeColorDefaultBrush");
        return box;
    }

    private ToggleSwitch Switch(string key, string value)
    {
        var toggle = new ToggleSwitch
        {
            IsOn = value is "true" or "1",
            OnContent = Loc.T("common.on"),
            OffContent = Loc.T("common.off"),
        };

        toggle.Toggled += (_, _) => ApplyFromControl(key, toggle.IsOn ? "true" : "false");
        return toggle;
    }

    /// <summary>A number the user can type or step, kept inside real limits.</summary>
    private NumberBox Number(string key, string value, double min, double max, double step)
    {
        var box = new NumberBox
        {
            MinWidth = 150,
            Minimum = min,
            Maximum = max,
            SmallChange = step,
            LargeChange = step * 10,
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact,
            // Out of range values are corrected rather than refused: nobody
            // wants a control that silently keeps a value the stack will reject.
            ValidationMode = NumberBoxValidationMode.InvalidInputOverwritten,
            Value = double.TryParse(value, System.Globalization.CultureInfo.InvariantCulture,
                                    out var parsed) ? parsed : min,
        };

        box.ValueChanged += (_, args) =>
        {
            if (!double.IsNaN(args.NewValue))
            {
                ApplyFromControl(key, args.NewValue.ToString(System.Globalization.CultureInfo.InvariantCulture));
            }
        };

        return box;
    }

    private FrameworkElement SliderNumber(string key, string value, double min, double max,
        double step, string lowLabel, string highLabel)
    {
        var parsed = double.TryParse(value, System.Globalization.CultureInfo.InvariantCulture,
            out var current) ? Math.Clamp(current, min, max) : min;
        var slider = new Slider
        {
            Minimum = min,
            Maximum = max,
            StepFrequency = step,
            Value = parsed,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        var number = new NumberBox
        {
            Minimum = min,
            Maximum = max,
            SmallChange = step,
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact,
            ValidationMode = NumberBoxValidationMode.InvalidInputOverwritten,
            Value = parsed,
            Width = 88,
        };
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(slider);
        Grid.SetColumn(number, 1);
        row.Children.Add(number);

        var endpoints = new Grid();
        endpoints.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        endpoints.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        endpoints.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        endpoints.Children.Add(new TextBlock
        {
            Text = lowLabel,
            FontSize = 10,
            Foreground = Brush("TextFillColorTertiaryBrush"),
        });
        var high = new TextBlock
        {
            Text = highLabel,
            FontSize = 10,
            Foreground = Brush("TextFillColorTertiaryBrush"),
        };
        Grid.SetColumn(high, 2);
        high.HorizontalAlignment = HorizontalAlignment.Right;
        endpoints.Children.Add(high);

        if (key == "gain")
        {
            var middle = new TextBlock
            {
                Text = Loc.T("slider.default"),
                FontSize = 10,
                Foreground = Brush("TextFillColorSecondaryBrush"),
            };
            Grid.SetColumn(middle, 1);
            endpoints.Children.Add(middle);
        }

        var panel = new StackPanel { Spacing = 0, HorizontalAlignment = HorizontalAlignment.Stretch };
        panel.Children.Add(row);
        panel.Children.Add(endpoints);

        var syncing = false;
        var timer = _ui.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(300);
        timer.IsRepeating = false;
        var pending = parsed;
        timer.Tick += (sender, _) =>
        {
            sender.Stop();
            ApplyFromControl(key, pending.ToString(System.Globalization.CultureInfo.InvariantCulture));
        };
        void Schedule(double next)
        {
            if (syncing || _populating || double.IsNaN(next)) return;
            pending = next;
            timer.Stop();
            timer.Start();
        }
        slider.ValueChanged += (_, args) =>
        {
            syncing = true;
            number.Value = args.NewValue;
            syncing = false;
            Schedule(args.NewValue);
        };
        number.ValueChanged += (_, args) =>
        {
            if (double.IsNaN(args.NewValue)) return;
            syncing = true;
            slider.Value = args.NewValue;
            syncing = false;
            Schedule(args.NewValue);
        };
        return panel;
    }

    private TextBox TextField(string key, string value)
    {
        var box = new TextBox { Text = value, MinWidth = 150 };
        box.LostFocus += (_, _) => ApplyFromControl(key, box.Text.Trim());
        return box;
    }

    private async void ApplyFromControl(string key, string value)
    {
        if (_populating) return;

        if (PresetControlledKeys.Contains(key) && !_customPreset)
        {
            _customPreset = true;
            SetPresetSelection("custom");
            if (!await TrySend("set", new() { ["key"] = "preset", ["value"] = "custom" }))
                return;
            await TrySend("set", new() { ["key"] = key, ["value"] = value });
            return;
        }

        Apply(key, value);
    }

    private void SetPresetSelection(string value)
    {
        if (_presetBox is null) return;
        var index = -1;
        for (var position = 0; position < _presetBox.Items.Count; position++)
        {
            if (_presetBox.Items[position] is ComboBoxItem { Tag: string tag } && tag == value)
            {
                index = position;
                break;
            }
        }
        if (index < 0 || index >= _presetBox.Items.Count) return;
        var previous = _populating;
        _populating = true;
        _presetBox.SelectedIndex = index;
        _populating = previous;
    }

    private void Apply(string key, string value)
    {
        if (_populating)
        {
            return;
        }

        Send("set", new() { ["key"] = key, ["value"] = value });
    }

    private void RestorePendingMarker(string key, TextBlock marker)
    {
        if (!_pendingReconnectMarkers.Contains(key)) return;
        marker.Text = "● " + Loc.T("settings.reconnect_required");
        marker.Foreground = Brush("SystemFillColorCriticalBrush");
        marker.Opacity = 1;
    }

    private void OnApplied(JsonElement message)
    {
        var rawKey = Text(message, "key");
        var rawValue = Text(message, "value");
        var needs = message.GetProperty("needs").EnumerateArray().ToList();
        if (rawKey == "run_in_background")
        {
            _runInBackground = rawValue is "true" or "1";
        }
        else if (rawKey == "start_with_windows")
        {
            try
            {
                SetStartupEnabled(rawValue is "true" or "1");
            }
            catch (Exception e)
            {
                SettingsNotice.IsOpen = true;
                SettingsNotice.Severity = InfoBarSeverity.Error;
                SettingsNotice.Message = Loc.T("settings.startup_error", e.Message);
            }
        }
        else if (rawKey == "language")
        {
            Loc.SetLanguage(rawValue);
            Relocalize();
        }
        else if (rawKey == "startup_reconnect_enabled")
        {
            _startupReconnectEnabled = rawValue is "true" or "1";
            if (!_startupReconnectEnabled) _startupReconnectTimer?.Stop();
        }

        // Enabling the headset Source ASE makes it a valid monitoring source;
        // disabling it removes that source. Refresh only this structural change.
        if (rawKey == "microphone_mode")
        {
            _settingsShape = "";
            Send("settings");
        }

        // A named preset changes several effective values at once. Ask for a
        // fresh model so the visible codec/radio controls always tell the truth.
        if (rawKey == "preset")
        {
            _customPreset = rawValue == "custom";
            SetPresetSelection(rawValue);
            // Named presets replace several visible values. Switching to Custom
            // alone changes no values and therefore must not rebuild the page
            // while the user is actively moving a slider.
            if (!_customPreset)
            {
                _settingsShape = "";
                Send("settings");
            }
        }
        if (_savedMarkers.TryGetValue(rawKey, out var marker))
        {
            marker.Opacity = 1;
            if (needs.Count == 0)
            {
                marker.Text = "● " + Loc.T("settings.applied_now");
                marker.Foreground = Brush("SystemFillColorSuccessBrush");
                _pendingReconnectMarkers.Remove(rawKey);

                var fade = _ui.CreateTimer();
                fade.Interval = TimeSpan.FromSeconds(2);
                fade.IsRepeating = false;
                fade.Tick += (t, _) =>
                {
                    if (!_pendingReconnectMarkers.Contains(rawKey)) marker.Opacity = 0;
                    t.Stop();
                };
                fade.Start();
            }
            else
            {
                marker.Text = "● " + Loc.T("settings.reconnect_required");
                marker.Foreground = Brush("SystemFillColorCriticalBrush");
                _pendingReconnectMarkers.Add(rawKey);
            }
        }

        var key = Label(rawKey);

        SettingsNotice.IsOpen = true;
        SettingsNotice.ActionButton = null;

        if (needs.Count == 0)
        {
            SettingsNotice.Severity = InfoBarSeverity.Success;
            SettingsNotice.Message = Loc.T("settings.saved_now", key);
            return;
        }

        SettingsNotice.Severity = InfoBarSeverity.Warning;
        SettingsNotice.Message = Loc.T("settings.saved_scope", key, Scope(Text(needs[0], "scope")));

        // A setting that needs a reconnect is useless without a way to do one.
        // Telling someone what has to happen and then leaving them to work out
        // how is the same as not applying the change at all.
        if (_connectedAddress is not null)
        {
            var button = new Button { Content = Loc.T("settings.reconnect") };
            button.Click += ReconnectClicked;
            SettingsNotice.ActionButton = button;
        }
    }

    /// <summary>Disconnects and connects again, so a pending change takes effect.</summary>
    private void ReconnectClicked(object sender, RoutedEventArgs e)
    {
        if (_connectedAddress is null)
        {
            return;
        }

        _reconnectTo = _connectedAddress;
        SettingsNotice.IsOpen = false;
        Busy.IsActive = true;
        Append(Loc.T("log.reconnect_wait"));
        Send("disconnect");
    }

    private async Task ReconnectAfterDelay(string address)
    {
        await Task.Delay(TimeSpan.FromSeconds(3));
        if (_connectedAddress is not null) return;
        Update(address, row => row.With(connecting: true));
        Busy.IsActive = true;
        Send("connect", new() { ["address"] = address });
    }
}
