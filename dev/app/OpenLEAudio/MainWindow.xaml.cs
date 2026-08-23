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
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Storage.Pickers;

namespace OpenLEAudio;

/// <summary>One row of a device list.</summary>
/// <summary>One row in the device lists.</summary>
/// <remarks>
/// A record, so two rows describing the same device in the same state compare
/// equal. That is what lets the list be refreshed without replacing items that
/// have not changed, and replacing an item is what makes a list view rebuild
/// the row - visibly, if it happens on every status message.
/// </remarks>
public sealed record DeviceRow
{
    public string Address { get; init; } = "";

    /// <summary>What the row is called, never empty.</summary>
    /// <remarks>
    /// A row carrying an empty name renders as an icon, a gap and two buttons,
    /// which reads as a broken list rather than as a device whose advertisement
    /// happened to be nameless. The address is always known - it is how the row
    /// exists at all - so it is the honest fallback, and it is applied here so
    /// no caller can forget it.
    /// </remarks>
    public string Name
    {
        get => string.IsNullOrWhiteSpace(_name) ? Address : _name;
        init => _name = value;
    }

    private readonly string _name = "";
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
    /// <summary>
    /// The label in the adapter menu, saying outright when the driver package
    /// has never heard of this one.
    /// </summary>
    public override string ToString() => Supported
        ? $"{Name}  ·  {HardwareId}"
        : $"{Name}  ·  {HardwareId}  ·  {Loc.T("setup.not_listed")}";
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

    /// Whether the bass / mid / treble breakdown is written inside the playing line.
    ///
    /// Only that part. The line itself - frame count, channel levels, delivered
    /// counts, signal, loss - stays either way: it is how anyone sees that the
    /// stream is still alive, and hiding the whole thing made a working stream
    /// look like a stopped one. The three band numbers are the part that is only
    /// interesting while judging how a stream sounds.
    private bool _levelLogEnabled = true;
    private bool _debugLogEnabled;

    /// How often the playing line is allowed to reach the log.
    ///
    /// The measurement behind it keeps running at full rate; this throttles the
    /// writing alone, so the log stays readable during a long listen without
    /// costing any of the numbers at the bottom of the window.
    private TimeSpan _playingEvery = TimeSpan.Zero;
    private DateTimeOffset _lastPlayingLine = DateTimeOffset.MinValue;
    private bool _programmaticLogScroll;

    /// Whether the settings banner currently belongs to a reconnect in progress,
    /// so closing it again does not wipe a message somebody else put there.
    private bool _reconnectNoticeOwned;
    private System.Windows.Forms.NotifyIcon? _trayIcon;
    private bool _runInBackground;
    private bool _exitRequested;
    private bool _startupReconnectEnabled = true;

    /// The last set of environment problems, so identical ones are not re-logged.
    private string _lastEnvironment = "";

    /// The headphones to reach for first, by address, or empty for "whichever".
    ///
    /// This setting existed with a text box in front of it and nothing at all
    /// behind it: it saved, it survived restarts, and no code ever read it. With
    /// two paired headsets, startup reconnect took whichever the scan happened
    /// to report first - so the answer changed from one boot to the next and
    /// looked like the setting being ignored, which it was.
    private string _preferredDevice = "";
    private DispatcherQueueTimer? _startupReconnectTimer;
    private DateTimeOffset _startupReconnectUntil;

    // TextBlock stores UTF-16. Half a million characters plus its string is
    // approximately one MiB of retained console history, regardless of how
    // long the app stays open.
    private const int MaxLogCharacters = 512 * 1024;
    private const int MaxNormalLogLines = 500;
    private sealed record LinkHealthSample(DateTimeOffset Time, long Sent, long Failed);

    /// The union of a device's PAC records: everything it will accept.
    private sealed record DeviceEnvelope(
        IReadOnlyList<int> Rates,
        IReadOnlyList<double> FrameMs,
        int? OctetsMin,
        int? OctetsMax);

    /// How a chosen value stands against what the device published.
    private enum Fit
    {
        /// The device listed it. It will work.
        Supported,
        /// Within LC3, but this device never offered it. It may be refused.
        Doubtful,
        /// The device published a range and this is outside it, or LC3 does not
        /// define it at all. It will be refused.
        Refused,
    }

    // UI state lives here, not read back out of controls. Reading a control from
    // the agent's reader thread throws, and an exception there kills the reader
    // loop - which looks exactly like a stack that stopped talking: an empty
    // list, a spinner that never stops, and a toggle that does nothing.
    private bool _adapterOn;
    private string? _connectedAddress;
    private bool _suppressToggle;

    /// Battery percentages as the device last published them, in its own order.
    private List<int> _batteryLevels = new();

    /// What the connected headphones said they can decode.
    ///
    /// Null when nothing is connected, which is a real state and not a missing
    /// value: with no device to ask, no codec setting can be called wrong.
    private DeviceEnvelope? _capabilities;

    /// When the current connection was established, for the uptime display.
    private DateTimeOffset? _connectedSince;

    /// Ticks the uptime label. Nothing else needs a clock, so it runs only
    /// while something is connected.
    private DispatcherQueueTimer? _uptimeTimer;

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

    /// Marks a control that cannot share a row with its own label.
    private const string NeedsFullWidth = "wide";

    /// The "saved" label beside each control, by setting key.
    private readonly Dictionary<string, TextBlock> _savedMarkers = new();

    /// The capability warning under each codec control, by setting key.
    private readonly Dictionary<string, TextBlock> _codecNotes = new();

    /// The values a headset is entitled to refuse, and therefore the only ones
    /// worth judging against what it published.
    private static readonly HashSet<string> CodecJudgedKeys = new(StringComparer.Ordinal)
    {
        "rate_hz", "frame_ms", "octets",
    };

    /// The last value seen for each judged key, so a warning can be refreshed
    /// when capabilities arrive after the page was built.
    private readonly Dictionary<string, string> _codecValues = new();

    /// Every setting's current value, for the estimates that need more than one.
    ///
    /// Airtime is a property of the whole configuration - the bitrate matters
    /// less at 10 ms frames than at 7.5, and not at all if the stream is mono -
    /// so a per-setting estimate cannot be made from that setting alone.
    private readonly Dictionary<string, string> _settingValues = new(StringComparer.Ordinal);

    public MainWindow()
    {
        InitializeComponent();
        _ui = DispatcherQueue.GetForCurrentThread();
        // Set runtime defaults only after every named XAML element exists.
        // Setting IsOn in XAML raises Toggled while later elements such as the
        // log ScrollViewer are still null and used to crash the whole startup.
        FollowLogSwitch.IsOn = true;
        LevelLogSwitch.IsOn = true;
        FillPlayingRates();
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
        AboutTimingGrid.SizeChanged += AboutTimingGridSizeChanged;

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

        // The chosen headphones first, then any paired LE Audio device. Falling
        // back rather than insisting is deliberate: a preferred device that is
        // switched off should not stop the other pair from connecting.
        var candidate =
            _found.FirstOrDefault(row => row.Paired &&
                string.Equals(row.Address, _preferredDevice, StringComparison.OrdinalIgnoreCase))
            ?? _found.FirstOrDefault(row => row.Paired && row.LeAudio);

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

    private void LevelLogToggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle)
        {
            return;
        }

        _levelLogEnabled = toggle.IsOn;

        // A log that suddenly stops producing lines looks like the stream
        // stopping. Say which it is.
        Append(Loc.T(_levelLogEnabled ? "log.levels_on" : "log.levels_off"));
    }

    /// <summary>The choices for how often the playing line is written.</summary>
    /// <remarks>
    /// Seconds, with zero meaning "every one". Kept as a tag rather than parsed
    /// back out of the label so translating the labels cannot change behaviour.
    /// </remarks>
    private static readonly (string Key, int Seconds)[] PlayingRates =
    {
        ("console.rate_every", 0),
        ("console.rate_1s", 1),
        ("console.rate_2s", 2),
        ("console.rate_5s", 5),
        ("console.rate_15s", 15),
    };

    private void FillPlayingRates()
    {
        PlayingRateBox.Items.Clear();
        foreach (var (key, seconds) in PlayingRates)
        {
            PlayingRateBox.Items.Add(new ComboBoxItem { Content = Loc.T(key), Tag = seconds });
        }
        PlayingRateBox.SelectedIndex = 0;
    }

    private void PlayingRateChanged(object sender, SelectionChangedEventArgs e)
    {
        if (PlayingRateBox?.SelectedItem is ComboBoxItem { Tag: int seconds })
        {
            _playingEvery = TimeSpan.FromSeconds(seconds);
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
        // Only what the button says. It used to switch the band levels back on
        // as well, which quietly undid a choice the user had just made.
        FollowLogSwitch.IsOn = true;
        _followLog = true;
        ScrollLogToBottom();
    }

    private void DebugLogToggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle || LogText is null) return;
        _debugLogEnabled = toggle.IsOn;
        Send("debug", new() { ["on"] = toggle.IsOn });

        // Deliberately not cleared. Switching debug off used to empty the whole
        // window, which took the connection history with it - exactly the part
        // worth keeping, and the part that cannot be produced again without
        // reconnecting. From here on it only stops new packet-level detail and
        // lets Append trim the backlog to the shorter history limit.
        Append(Loc.T(toggle.IsOn ? "log.debug_enabled" : "log.debug_disabled"));
    }

    private void PageChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string;

        SetupPage.Visibility = tag == "setup" ? Visibility.Visible : Visibility.Collapsed;
        DevicesPage.Visibility = tag == "devices" ? Visibility.Visible : Visibility.Collapsed;
        SettingsPage.Visibility = tag == "settings" ? Visibility.Visible : Visibility.Collapsed;
        LanguagePage.Visibility = tag == "language" ? Visibility.Visible : Visibility.Collapsed;
        AboutPage.Visibility = tag == "about" ? Visibility.Visible : Visibility.Collapsed;

        if (tag == "setup")
        {
            ShowDependencies();

            // Detected again on every visit, not once per launch. A dongle
            // plugged in after startup is exactly when somebody opens this page,
            // and caching the first answer meant the page told them their
            // adapter was missing while it sat in the port in front of them.
            _ = DetectAdaptersAsync();

            // Same reason on the other side: the core's own view of the driver
            // binding is a snapshot from when it started.
            Send("check");
        }

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

    private void CheckDependenciesClicked(object sender, RoutedEventArgs e) => ShowDependencies();

    /// <summary>One runtime OpenLEAudio needs, and whether it is there.</summary>
    private sealed record Dependency(string Name, bool Present, string Detail);

    /// <summary>
    /// What the application needs from Windows before it can do anything.
    /// </summary>
    /// <remarks>
    /// Checked by looking for the files and packages themselves rather than
    /// through an installer's registry keys. A repair or a partial uninstall can
    /// leave the keys behind after the payload has gone, and a check that
    /// believes them reports everything as fine while the application refuses to
    /// start.
    ///
    /// The Visual C++ runtime is the one worth naming: nothing else in the chain
    /// installs it, and without it the process exits before the first window
    /// exists - no crash dialog, no log, nothing to go on.
    /// </remarks>
    private static List<Dependency> CheckDependencies()
    {
        var system = Environment.GetFolderPath(Environment.SpecialFolder.System);

        var vcFiles = new[] { "vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll" };
        var missingVc = vcFiles.Where(name => !File.Exists(Path.Combine(system, name))).ToList();

        // This code is running, so the two runtimes that host it are present by
        // definition. Saying so is still worth a line: someone reading this
        // panel wants to know the whole list is accounted for, not to be left
        // wondering which entries were skipped.
        return new List<Dependency>
        {
            new(".NET 8 Desktop Runtime", true,
                System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription),
            new("Windows App Runtime 1.8", true, "loaded"),
            new("Visual C++ 2015-2022 Redistributable", missingVc.Count == 0,
                missingVc.Count == 0 ? "installed" : $"missing: {string.Join(", ", missingVc)}"),
        };
    }

    private void ShowDependencies()
    {
        var dependencies = CheckDependencies();
        DependencyList.Children.Clear();

        foreach (var dependency in dependencies)
        {
            var row = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 8,
            };
            row.Children.Add(new FontIcon
            {
                Glyph = dependency.Present ? "\uE73E" : "\uEA39",
                FontSize = 13,
                Foreground = Brush(dependency.Present
                    ? "SystemFillColorSuccessBrush"
                    : "SystemFillColorCriticalBrush"),
                VerticalAlignment = VerticalAlignment.Center,
            });
            row.Children.Add(new TextBlock
            {
                Text = $"{dependency.Name} - {dependency.Detail}",
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
                VerticalAlignment = VerticalAlignment.Center,
            });
            DependencyList.Children.Add(row);
        }

        InstallDependenciesButton.IsEnabled = dependencies.Any(item => !item.Present);
    }

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

            // Supported ones first, so the menu opens on something usable even
            // when a second radio is present.
            adapters = adapters
                .OrderByDescending(adapter => adapter.Supported)
                .ThenBy(adapter => adapter.Name)
                .ToList();

            SetupAdapterBox.ItemsSource = adapters;
            SetupAdapterBox.SelectedIndex = adapters.Count > 0 ? 0 : -1;

            // A supported adapter that is simply not there is the one failure
            // on this page with no error message of its own: the menu just
            // stays empty, which reads as the page still loading. Say it, and
            // mark the control that has nothing in it.
            var none = adapters.Count == 0;
            SetupAdapterBox.BorderBrush = none
                ? Brush("SystemFillColorCriticalBrush")
                : Brush("ControlStrokeColorDefaultBrush");
            SetupAdapterBox.BorderThickness = new Thickness(none ? 2 : 1);
            SetupAdapterDetails.Foreground = none
                ? Brush("SystemFillColorCriticalBrush")
                : Brush("TextFillColorSecondaryBrush");
            if (none) SetupAdapterDetails.Text = Loc.T("setup.none");
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
    private const uint SpdrpCompatibleIds = 0x00000002;
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

                // Not in the INF, but still worth listing if it is a Bluetooth
                // controller. Leaving those out meant somebody with a different
                // dongle saw an empty menu and no way to tell "the adapter is
                // not plugged in" from "the adapter is here and this program
                // has never heard of it" - which are opposite problems with
                // opposite fixes.
                var supported = hardwareId is not null;
                if (!supported)
                {
                    var compatible = SetupDeviceProperty(set, ref data, SpdrpCompatibleIds);
                    if (!compatible.Contains("Class_E0&SubClass_01&Prot_01",
                            StringComparison.OrdinalIgnoreCase))
                    {
                        continue;
                    }
                    hardwareId = BareHardwareId(instance);
                    if (hardwareId is null) continue;
                }

                if (hardwareId is null) continue;

                var name = SetupDeviceProperty(set, ref data, SpdrpFriendlyName);
                var description = SetupDeviceProperty(set, ref data, SpdrpDeviceDesc);
                var service = SetupDeviceProperty(set, ref data, SpdrpService);
                result.Add(new AdapterChoice(
                    string.IsNullOrWhiteSpace(name) ? description : name,
                    instance, hardwareId, service, description, supported));
            }
        }
        finally
        {
            SetupDiDestroyDeviceInfoList(set);
        }
        return result;
    }

    /// <summary>The plain USB\VID_xxxx&amp;PID_xxxx part of a device instance id.</summary>
    /// <remarks>
    /// The form an INF matches on. The rest of the instance id names one
    /// physical port and one firmware revision, neither of which belongs in a
    /// driver package.
    /// </remarks>
    private static string? BareHardwareId(string instance)
    {
        var match = Regex.Match(instance, @"^USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}",
            RegexOptions.IgnoreCase);
        return match.Success ? match.Value.ToUpperInvariant() : null;
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

        // Bound to Windows rather than to us: the adapter exists and nothing
        // here will work until it is switched over.
        var ours = string.Equals(adapter.Service, "WinUSB", StringComparison.OrdinalIgnoreCase);
        SetupAdapterBox.BorderBrush = ours
            ? Brush("SystemFillColorSuccessBrush")
            : Brush("SystemFillColorCautionBrush");
        SetupAdapterBox.BorderThickness = new Thickness(ours ? 1 : 2);
        SetupAdapterDetails.Foreground = ours
            ? Brush("TextFillColorSecondaryBrush")
            : Brush("SystemFillColorCautionBrush");
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
            case "dependencies": RunDependencyInstaller(root); return;
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

            // Binding the adapter to our driver changes what the audio core
            // finds when it opens the device, and the core has already opened
            // it. Nothing short of restarting it picks that up, so say so
            // rather than leaving the user to discover it by trying to connect
            // and being told the adapter is on the Microsoft stack.
            var needsRestart = action is "adapter-bind" or "adapter-restore";
            SetupNotice.Severity = needsRestart
                ? InfoBarSeverity.Warning
                : InfoBarSeverity.Informational;
            SetupNotice.Message = Loc.T(needsRestart ? "setup.restart_needed" : "setup.started");
            if (needsRestart)
            {
                var restart = new Button { Content = Loc.T("setup.restart_now") };
                restart.Click += (_, _) => RestartApplication();
                SetupNotice.ActionButton = restart;
            }
            else
            {
                SetupNotice.ActionButton = null;
            }
            SetupNotice.IsOpen = true;
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            ShowSetupError(error.Message);
        }
    }

    /// <summary>
    /// Runs the dependency installer, which lives beside the release rather
    /// than in the scripts directory because it must work before anything else
    /// is set up.
    /// </summary>
    private void RunDependencyInstaller(string root)
    {
        var installer = Path.Combine(root, "INSTALL dependencies.bat");
        if (!File.Exists(installer))
        {
            ShowSetupError(Loc.T("setup.files_missing"));
            return;
        }

        try
        {
            // Elevated: installing a Microsoft runtime writes to system
            // directories, and a silent failure here is the hardest kind to
            // diagnose because everything downstream simply does not start.
            Process.Start(new ProcessStartInfo(installer)
            {
                UseShellExecute = true,
                Verb = "runas",
                WorkingDirectory = root,
            });
            SetupNotice.Severity = InfoBarSeverity.Informational;
            SetupNotice.Message = Loc.T("setup.started");
            SetupNotice.IsOpen = true;
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            ShowSetupError(error.Message);
        }
    }

    /// <summary>Starts a fresh copy and closes this one.</summary>
    /// <remarks>
    /// The audio core is a child process holding the adapter, so it has to go
    /// with us: a new instance would find the device still owned and report it
    /// as bound to another driver, which is the confusion this button exists to
    /// avoid.
    /// </remarks>
    private void RestartApplication()
    {
        var executable = Environment.ProcessPath;
        if (executable is null)
        {
            ShowSetupError(Loc.T("setup.files_missing"));
            return;
        }

        try
        {
            Process.Start(new ProcessStartInfo(executable) { UseShellExecute = true });
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            ShowSetupError(error.Message);
            return;
        }

        _exitRequested = true;
        _agent?.Dispose();
        Close();
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

        // Asked again before turning it on. The environment report is made once
        // at startup, and a dongle plugged in afterwards leaves it saying the
        // adapter is on the Microsoft stack - which by then is simply out of
        // date, and reads as the app refusing to see hardware that is plainly
        // there.
        if (AdapterSwitch.IsOn)
        {
            Send("check");
        }
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
                RememberCapabilities(message);
                break;

            case "connected":
                _connectedAddress = Text(message, "address");
                _healthSamples.Clear();
                ShowReconnecting(false);
                ShowConnectedHint();
                PromoteToPaired(_connectedAddress, Text(message, "name"), true, true);
                foreach (var pendingKey in _pendingReconnectMarkers)
                {
                    if (_savedMarkers.TryGetValue(pendingKey, out var pendingMarker))
                        pendingMarker.Opacity = 0;
                }
                _pendingReconnectMarkers.Clear();
                _startupReconnectTimer?.Stop();
                StartUptimeClock();
                Append(Loc.T("log.connected"));
                Send("status");
                break;

            case "disconnected":
                Update(_connectedAddress, row => row.With(connected: false, streaming: false, connecting: false));
                _connectedAddress = null;
                _healthSamples.Clear();
                StopUptimeClock();
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

            case "reconnect-started":
                // The point where a failed connection stops being an error the
                // user has to act on and becomes something the stack is
                // handling. Without this the row sat on the last error message
                // while attempts carried on invisibly underneath it.
                Update(Text(message, "address"), row => row.With(connecting: true));
                ShowReconnecting(true);
                Append(Loc.T("log.reconnect_started"));
                break;

            case "reconnecting":
                Update(Text(message, "address"), row => row.With(connecting: true));
                ShowReconnecting(true);
                Append(Loc.T("log.reconnecting"));
                break;

            case "availability":
                // Only worth a line when it is not the ordinary case. "The
                // headphones are free" is what everyone expects and saying it
                // every time buries the one occasion it is not true.
                if (Text(message, "state") != "ready")
                {
                    Append(Text(message, "detail"));
                }
                break;

            case "yielded":
                Update(_connectedAddress, row => row.With(streaming: false));
                Append(Loc.T("log.yielded"));
                break;

            case "reclaiming":
                Append(Loc.T("log.reclaiming"));
                break;

            case "reconnect-stopped":
                Update(Text(message, "address"), row => row.With(connecting: false));
                ShowReconnecting(false);
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

            case "environment":
                ShowEnvironment(message);
                break;

            case "battery":
                OnBattery(message);
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

        var line = $"playing: {frames} frames, L {Level(message, "leftDb")} / R {Level(message, "rightDb")}";

        // The band breakdown is the only optional part. Everything else on this
        // line answers "is it still playing, and is the radio keeping up", which
        // is worth keeping visible for as long as the stream runs.
        if (_levelLogEnabled)
        {
            line += $", bass {Level(message, "bassDb")} / mid {Level(message, "midDb")}"
                  + $" / treble {Level(message, "trebleDb")}";
        }

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

        // What the radio itself reports, when the controller supports being
        // asked. This is the number that means "the headphones did not hear
        // it"; the USB submit failures counted below only mean "it never left
        // this PC", which is almost never what goes wrong and is why this
        // display used to sit at zero through audible dropouts.
        long radioLost = 0;
        var haveRadio = false;
        if (message.TryGetProperty("radio", out var radio) && radio.ValueKind == JsonValueKind.Array)
        {
            foreach (var channel in radio.EnumerateArray())
            {
                if (channel.TryGetProperty("lost", out var lost)
                    && lost.ValueKind == JsonValueKind.Number)
                {
                    radioLost += lost.GetInt64();
                    haveRadio = true;
                }
            }
        }

        if (haveRadio)
        {
            line += $", lost on air {radioLost}";
        }

        if (message.TryGetProperty("underruns", out var underrunValue)
            && underrunValue.ValueKind == JsonValueKind.Number
            && underrunValue.GetInt64() > 0)
        {
            // Counted apart from radio loss on purpose: this is Windows failing
            // to hand over audio in time, and it sounds identical to a link
            // problem. Blaming the radio for it sends the search the wrong way.
            line += $", PC underruns {underrunValue.GetInt64()}";
        }

        // Printed last, so everything appended above is on the same line. The
        // metrics strip below is updated either way: throttling the display must
        // not stop the measurement, or the numbers at the bottom would freeze
        // along with the log.
        var now = DateTimeOffset.UtcNow;
        if (_playingEvery == TimeSpan.Zero || now - _lastPlayingLine >= _playingEvery)
        {
            _lastPlayingLine = now;
            Append(line);
        }

        var sent = message.TryGetProperty("sent", out var sentValue) ? sentValue.GetInt64() : frames;
        var lostTotal = haveRadio ? radioLost : failed;
        _healthSamples.Enqueue(new LinkHealthSample(now, sent, lostTotal));
        while (_healthSamples.Count > 1 && now - _healthSamples.Peek().Time > TimeSpan.FromSeconds(60))
            _healthSamples.Dequeue();

        var oldest = _healthSamples.Peek();
        var lost60 = Math.Max(0, lostTotal - oldest.Failed);
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

    /// <summary>
    /// Shows what is standing between the machine and working audio.
    /// </summary>
    /// <remarks>
    /// One bar, showing the most serious problem, with a button that goes
    /// straight to the step that fixes it. A list of five simultaneous
    /// complaints is read as noise; the first one is usually the cause of the
    /// rest anyway, and the others reappear once it is dealt with.
    ///
    /// The full set still goes to the log, so nothing is hidden - it is only
    /// not all shouted at once.
    /// </remarks>
    private void ShowEnvironment(JsonElement message)
    {
        if (!message.TryGetProperty("issues", out var issues)
            || issues.ValueKind != JsonValueKind.Array)
        {
            return;
        }

        // The check runs on startup and again whenever the radio is asked for,
        // so the same three complaints were written to the log every time. A
        // log that repeats itself is one people stop reading, which defeats the
        // point of warning them at all. The bar is still refreshed either way.
        var fingerprint = issues.GetRawText();
        var repeated = fingerprint == _lastEnvironment;
        _lastEnvironment = fingerprint;
        if (!repeated && issues.GetArrayLength() > 0)
        {
            // Laid out as a list, with the problems before the warnings and the
            // remedy indented under each one. Run together as sentences these
            // were four unbroken lines of prose that wrapped into each other,
            // and the thing a person actually needs - which button to press -
            // was buried in the middle of it.
            Append("");
            Append(Loc.T("environment.heading"));

            var ordered = issues.EnumerateArray()
                .OrderBy(issue => Text(issue, "severity") == "error" ? 0 : 1)
                .ToList();

            foreach (var issue in ordered)
            {
                var label = Text(issue, "severity") == "error"
                    ? Loc.T("environment.problem")
                    : Loc.T("environment.warning");
                Append($"  [{label}] {Text(issue, "summary")}");
                Append($"      {Text(issue, "remedy")}");
            }
            Append("");
        }

        ShowSetupStatus(issues);

        var first = issues.EnumerateArray().FirstOrDefault();
        if (first.ValueKind != JsonValueKind.Object)
        {
            EnvironmentNotice.IsOpen = false;
            EnvironmentNotice.ActionButton = null;
            return;
        }

        var count = issues.GetArrayLength();
        var blocking = Text(first, "severity") == "error";

        EnvironmentNotice.Severity = blocking
            ? InfoBarSeverity.Error
            : InfoBarSeverity.Warning;
        EnvironmentNotice.Title = Text(first, "summary");
        EnvironmentNotice.Message = count > 1
            ? $"{Text(first, "remedy")} ({count - 1} more to check in the log.)"
            : Text(first, "remedy");

        // Only when there is somewhere useful to send them. A button that opens
        // a page with nothing to press on it is worse than no button.
        var action = Text(first, "setupAction");
        if (action.Length > 0)
        {
            var button = new Button { Content = Loc.T("environment.open_setup") };
            button.Click += (_, _) =>
            {
                // Selecting the item is what switches the page: PageChanged
                // does the visibility, and reaching past it would leave the
                // navigation highlight pointing somewhere else.
                var setup = Nav.MenuItems.OfType<NavigationViewItem>()
                    .FirstOrDefault(item => (item.Tag as string) == "setup");
                if (setup is not null)
                {
                    Nav.SelectedItem = setup;
                }
            };
            EnvironmentNotice.ActionButton = button;
        }
        else
        {
            EnvironmentNotice.ActionButton = null;
        }

        EnvironmentNotice.IsOpen = true;
    }

    /// <summary>
    /// Battery levels, as the device publishes them.
    /// </summary>
    /// <remarks>
    /// One entry per Battery Service instance. Earbuds conventionally list left,
    /// right and then the case, but nothing in the specification says so - which
    /// is why the display shows them in order rather than labelling them.
    /// </remarks>
    private void OnBattery(JsonElement message)
    {
        if (!message.TryGetProperty("levels", out var levels)
            || levels.ValueKind != JsonValueKind.Array)
        {
            return;
        }

        _batteryLevels = levels.EnumerateArray()
            .Where(level => level.ValueKind == JsonValueKind.Number)
            .Select(level => level.GetInt32())
            .ToList();

        RefreshConnectionStatus();
    }

    /// <summary>
    /// Redraws the battery cells and the uptime label.
    /// </summary>
    /// <remarks>
    /// The battery is drawn rather than written, because a percentage in a row
    /// of percentages is read only when someone goes looking for it. A shape
    /// that is visibly nearly empty is noticed on the way past, which is the
    /// entire point of showing it.
    /// </remarks>
    private void RefreshConnectionStatus()
    {
        BatteryStrip.Children.Clear();

        if (_connectedAddress is null || _batteryLevels.Count == 0)
        {
            BatteryStrip.Visibility = Visibility.Collapsed;
        }
        else
        {
            BatteryStrip.Visibility = Visibility.Visible;
            for (var index = 0; index < _batteryLevels.Count; index++)
            {
                BatteryStrip.Children.Add(BuildBatteryCell(_batteryLevels[index], index));
            }
        }

        UptimeMetric.Text = _connectedSince is { } since
            ? Loc.T("metrics.uptime", FormatDuration(DateTimeOffset.UtcNow - since))
            : Loc.T("metrics.uptime", "-");
    }

    /// <summary>One battery, as an outline with a fill proportional to charge.</summary>
    private FrameworkElement BuildBatteryCell(int percent, int index)
    {
        percent = Math.Clamp(percent, 0, 100);

        // Red below a tenth, amber below a fifth. The thresholds are the ones
        // Windows itself uses, so the colour means the same thing here as it
        // does everywhere else on the machine.
        var fill = percent <= 10
            ? Brush("SystemFillColorCriticalBrush")
            : percent <= 20
                ? Brush("SystemFillColorCautionBrush")
                : Brush("SystemFillColorSuccessBrush");

        const double BodyWidth = 26;
        const double BodyHeight = 13;

        var level = new Border
        {
            Width = Math.Max(2, (BodyWidth - 4) * percent / 100.0),
            Height = BodyHeight - 4,
            CornerRadius = new CornerRadius(1),
            Background = fill,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(1.5, 0, 0, 0),
        };

        var body = new Border
        {
            Width = BodyWidth,
            Height = BodyHeight,
            CornerRadius = new CornerRadius(3),
            BorderThickness = new Thickness(1),
            BorderBrush = Brush("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
            Child = level,
        };

        // The terminal on the positive end. Small, but without it the outline
        // reads as a progress bar rather than a battery.
        var cap = new Border
        {
            Width = 2,
            Height = 5,
            CornerRadius = new CornerRadius(0, 1, 1, 0),
            Background = Brush("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
        };

        var shape = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 1 };
        shape.Children.Add(body);
        shape.Children.Add(cap);

        var cell = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        cell.Children.Add(shape);
        cell.Children.Add(new TextBlock
        {
            Text = $"{percent} %",
            FontSize = 12,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        });

        // Nothing in the specification says which instance is which ear, so the
        // tooltip numbers them rather than inventing labels that may be wrong.
        var name = _batteryLevels.Count > 1
            ? Loc.T("metrics.battery_part", index + 1, _batteryLevels.Count)
            : Loc.T("metrics.battery");
        ToolTipService.SetToolTip(cell, name + Environment.NewLine + Loc.T("battery.refresh_tip"));

        // Clickable, because a battery indicator that only moves when the
        // headphones feel like saying so is one nobody trusts. The request goes
        // to the audio loop, which owns the link while music plays and is the
        // only thing that can put a question on it.
        var button = new Button
        {
            Content = cell,
            Padding = new Thickness(4, 2, 4, 2),
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(
                Windows.UI.Color.FromArgb(0, 0, 0, 0)),
            BorderThickness = new Thickness(0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        button.Click += (_, _) =>
        {
            Append(Loc.T("log.battery_requested"));
            Send("battery");
        };

        return button;
    }

    private static string FormatDuration(TimeSpan elapsed)
    {
        if (elapsed < TimeSpan.Zero)
        {
            elapsed = TimeSpan.Zero;
        }

        return elapsed.TotalHours >= 1
            ? $"{(int)elapsed.TotalHours} h {elapsed.Minutes} min"
            : elapsed.TotalMinutes >= 1
                ? $"{elapsed.Minutes} min {elapsed.Seconds} s"
                : $"{elapsed.Seconds} s";
    }

    private void StartUptimeClock()
    {
        _connectedSince = DateTimeOffset.UtcNow;

        // A second is the shortest unit the label ever shows, so anything faster
        // would be redrawing to display the same string.
        _uptimeTimer ??= _ui.CreateTimer();
        _uptimeTimer.Interval = TimeSpan.FromSeconds(1);
        _uptimeTimer.IsRepeating = true;
        _uptimeTimer.Tick -= UptimeTick;
        _uptimeTimer.Tick += UptimeTick;
        _uptimeTimer.Start();

        RefreshConnectionStatus();
    }

    private void UptimeTick(DispatcherQueueTimer sender, object args) => RefreshConnectionStatus();

    private void StopUptimeClock()
    {
        _uptimeTimer?.Stop();
        _connectedSince = null;
        _batteryLevels = new List<int>();
        RefreshConnectionStatus();
    }

    /// <summary>
    /// Marks the Setup steps that have something wrong with them.
    /// </summary>
    /// <remarks>
    /// The Devices page shows one bar naming the most serious problem. Setup is
    /// where the problems are actually fixed, so each step says whether it is
    /// the one at fault - otherwise someone arriving here from the bar has four
    /// numbered cards and no indication which of them to press.
    /// </remarks>
    private void ShowSetupStatus(JsonElement issues)
    {
        string? adapter = null;
        string? cable = null;
        var adapterBlocking = false;
        var cableBlocking = false;

        foreach (var issue in issues.EnumerateArray())
        {
            var id = Text(issue, "id");
            var blocking = Text(issue, "severity") == "error";

            // The first problem of each kind wins, and they arrive most serious
            // first, so this is the one worth naming.
            if (id.StartsWith("adapter", StringComparison.Ordinal) && adapter is null)
            {
                adapter = Text(issue, "summary");
                adapterBlocking = blocking;
            }
            else if (id.StartsWith("vbcable", StringComparison.Ordinal) && cable is null)
            {
                cable = Text(issue, "summary");
                cableBlocking = blocking;
            }
        }

        Mark(SetupAdapterStatus, adapter, adapterBlocking);
        Mark(SetupCableStatus, cable, cableBlocking);

        void Mark(TextBlock target, string? text, bool blocking)
        {
            if (text is null)
            {
                target.Visibility = Visibility.Collapsed;
                return;
            }

            target.Text = text;
            target.Foreground = Brush(blocking
                ? "SystemFillColorCriticalBrush"
                : "SystemFillColorCautionBrush");
            target.Visibility = Visibility.Visible;
        }
    }

    /// <summary>Keeps what the device published, so codec settings can be judged against it.</summary>
    private void RememberCapabilities(JsonElement message)
    {
        if (!message.TryGetProperty("sink", out var sink) || sink.ValueKind != JsonValueKind.Object)
        {
            return;
        }

        static IReadOnlyList<T> Numbers<T>(JsonElement parent, string name, Func<JsonElement, T> read)
        {
            if (!parent.TryGetProperty(name, out var array) || array.ValueKind != JsonValueKind.Array)
            {
                return Array.Empty<T>();
            }
            return array.EnumerateArray()
                .Where(item => item.ValueKind == JsonValueKind.Number)
                .Select(read)
                .ToList();
        }

        static int? Optional(JsonElement parent, string name) =>
            parent.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number
                ? value.GetInt32()
                : null;

        _capabilities = new DeviceEnvelope(
            Numbers(sink, "rates", item => item.GetInt32()),
            Numbers(sink, "frameMs", item => item.GetDouble()),
            Optional(sink, "octetsMin"),
            Optional(sink, "octetsMax"));

        RefreshCodecWarnings();
    }

    /// <summary>
    /// Judges one codec value against what the connected headphones published.
    /// </summary>
    /// <remarks>
    /// Three answers rather than two, because "the device did not list it" and
    /// "the device published a range that excludes it" are genuinely different.
    /// The first is a value worth trying on a device whose PAC records are
    /// incomplete - plenty are. The second will be refused, and saying so before
    /// the next connection saves the user a reconnect that ends in an error.
    /// </remarks>
    private Fit JudgeCodecValue(string key, string value)
    {
        // Nothing connected means nothing to contradict. Colouring settings
        // against a device that is not there would tell the user their
        // configuration is wrong for headphones they are not using.
        if (_capabilities is not { } device)
        {
            return Fit.Supported;
        }

        switch (key)
        {
            case "rate_hz":
                if (!int.TryParse(value, out var rate)) return Fit.Supported;
                if (device.Rates.Contains(rate)) return Fit.Supported;
                // LC3 itself is defined only to 48 kHz. Anything above that has
                // no encoder behind it, whatever a device might claim.
                return rate > 48_000 ? Fit.Refused : Fit.Doubtful;

            case "frame_ms":
                if (!double.TryParse(value, System.Globalization.NumberStyles.Float,
                        System.Globalization.CultureInfo.InvariantCulture, out var frame))
                    return Fit.Supported;
                return device.FrameMs.Any(supported => Math.Abs(supported - frame) < 0.01)
                    ? Fit.Supported
                    : Fit.Refused;

            case "octets":
                if (!int.TryParse(value, out var octets)) return Fit.Supported;
                if (device.OctetsMin is not { } min || device.OctetsMax is not { } max)
                    return Fit.Doubtful;
                return octets >= min && octets <= max ? Fit.Supported : Fit.Refused;

            default:
                return Fit.Supported;
        }
    }

    /// <summary>Colours one codec control according to what the device will accept.</summary>
    private void ApplyCodecWarning(string key, string value, TextBlock note)
    {
        _codecValues[key] = value;

        switch (JudgeCodecValue(key, value))
        {
            case Fit.Refused:
                note.Text = Loc.T("codec.refused");
                note.Foreground = Brush("SystemFillColorCriticalBrush");
                note.Visibility = Visibility.Visible;
                break;

            case Fit.Doubtful:
                note.Text = Loc.T("codec.doubtful");
                note.Foreground = Brush("SystemFillColorCautionBrush");
                note.Visibility = Visibility.Visible;
                break;

            default:
                // Nothing to say. An always-present "this is fine" line trains
                // people to stop reading the place the warnings appear.
                note.Visibility = Visibility.Collapsed;
                break;
        }
    }

    /// <summary>Re-judges every codec control, after connecting or after an edit.</summary>
    private void RefreshCodecWarnings()
    {
        foreach (var (key, note) in _codecNotes)
        {
            if (_codecValues.TryGetValue(key, out var value))
            {
                ApplyCodecWarning(key, value, note);
            }
        }
    }

    private void OnAdapter(JsonElement message)
    {
        _adapterOn = message.GetProperty("on").GetBoolean();

        var version = Text(message, "version");
        var address = Text(message, "address");
        var detail = _adapterOn
            ? string.Join(" · ", new[] { version, address }.Where(s => s.Length > 0))
            : Loc.T("status.adapter_off");

        AdapterDetail.Text = detail;

        // The switch has to follow what actually happened, not what was asked
        // for. Refusing to open an adapter that is still on the Microsoft stack
        // used to leave the toggle sitting at On above the words "the adapter is
        // off" - which reads as the app being confused rather than as a
        // refusal, and invites the user to keep pressing it.
        if (AdapterSwitch.IsOn != _adapterOn)
        {
            _suppressToggle = true;
            AdapterSwitch.IsOn = _adapterOn;
            _suppressToggle = false;
        }

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

        // Updated in place rather than cleared and refilled. Clearing tears down
        // every container the list view has built and rebuilds them in the same
        // frame, and a row that arrives mid-teardown draws with nothing in it -
        // an icon, a gap and two buttons, which is what a "bugged" device row
        // actually is.
        for (var index = _paired.Count - 1; index >= 0; index--)
        {
            if (!rows.Any(row => AddressesMatch(row.Address, _paired[index].Address)))
            {
                _paired.RemoveAt(index);
            }
        }
        for (var index = 0; index < rows.Count; index++)
        {
            var existing = _paired
                .Select((row, at) => (row, at))
                .FirstOrDefault(pair => AddressesMatch(pair.row.Address, rows[index].Address),
                                (null!, -1));
            if (existing.at < 0)
            {
                _paired.Insert(Math.Min(index, _paired.Count), rows[index]);
            }
            else if (existing.at != index)
            {
                _paired.Move(existing.at, index);
                _paired[index] = rows[index];
            }
            else if (!Equals(existing.row, rows[index]))
            {
                _paired[index] = rows[index];
            }
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

    /// <summary>Shows, on every page, that a connection is being worked on.</summary>
    /// <remarks>
    /// Three places said this and none of them said it everywhere: the device
    /// row went to "Connecting…", the log printed a line that scrolls away, and
    /// the settings page - the page somebody is looking at when a change causes
    /// the reconnect - said nothing at all. So a reconnect after a settings
    /// change looked like the app having stopped responding.
    /// </remarks>
    private void ShowReconnecting(bool active, string? detail = null)
    {
        Busy.IsActive = active;

        if (active)
        {
            StabilityMetric.Foreground = Brush("SystemFillColorCautionBrush");
            StabilityMetric.Text = detail ?? Loc.T("metrics.reconnecting");

            SettingsNotice.Severity = InfoBarSeverity.Informational;
            SettingsNotice.Message = detail ?? Loc.T("settings.reconnecting");
            SettingsNotice.ActionButton = null;
            SettingsNotice.IsOpen = true;
            _reconnectNoticeOwned = true;
            return;
        }

        if (_reconnectNoticeOwned)
        {
            _reconnectNoticeOwned = false;
            SettingsNotice.IsOpen = false;
        }
    }

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
        // In the application panel rather than beside the codec, and first in
        // it. These are the controls somebody reaches for while listening -
        // source, channels, balance, volume - and they were sitting under two
        // panels of radio parameters nobody touches twice. Kept first in this
        // array because that is what puts it at the top of its panel.
        new("playback", "application", 2, "section.playback", "section.playback.sub", "\uE767", Accent(45, 140, 255),
            new[] { "playback_source", "audio_mode", "swap_channels", "balance", "gain" }),
        new("codec", "audio", 0, "section.codec", "section.codec.sub", "\uE8D6", Accent(145, 102, 224),
            new[] { "rate_hz", "frame_ms", "octets" }),
        new("radio", "audio", 0, "section.radio", "section.radio.sub", "\uE701", Accent(0, 168, 120),
            new[] { "phy", "retransmissions", "max_latency_ms", "presentation_delay_ms",
                    "idle_link_latency" }),
        new("connection", "connection", 1, "section.connection", "section.connection.sub", "\uE702", Accent(245, 158, 11),
            new[] { "device", "reconnect_enabled", "reconnect_interval_s", "reconnect_window_min",
                    "startup_reconnect_enabled", "link_timeout_s" }),
        new("sharing", "connection", 1, "section.sharing", "section.sharing.sub", "", Accent(99, 102, 241),
            new[] { "multipoint_yield_enabled", "multipoint_yield_s", "idle_timeout_min" }),
        new("microphone", "connection", 1, "section.microphone", "section.microphone.sub", "\uE720", Accent(224, 82, 141),
            new[] { "microphone_mode", "microphone_quality", "microphone_target",
                    "microphone_gain", "monitor_enabled", "monitor_source", "monitor_mode", "monitor_gain" }),
        new("application", "application", 2, "section.application", "section.application.sub", "\uE8A7", Accent(20, 184, 166),
            new[] { "run_in_background", "start_with_windows" }),
        new("diagnostics", "application", 2, "section.tuning", "section.tuning.sub", "\uE713", Accent(100, 116, 139),
            new[] { "link_metrics", "battery_poll_min", "diagnostics", "command_style" }),
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
        _codecNotes.Clear();
        PresetHost.Children.Clear();
        _presetBox = null;
        LanguageHost.Children.Clear();
        ShowConnectedHint();

        var knobs = message.GetProperty("knobs")
            .EnumerateArray()
            .ToDictionary(k => Text(k, "key"));

        _settingValues.Clear();
        foreach (var (key, knob) in knobs)
        {
            _settingValues[key] = Text(knob, "value");
        }
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
        _preferredDevice = knobs.TryGetValue("device", out var deviceKnob)
            ? Text(deviceKnob, "value")
            : "";

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

        // The same shape as every other setting: a name and a question mark.
        // The description used to sit underneath, trimmed to one line with an
        // ellipsis - which meant the sentence explaining the most important
        // control on the page was the one sentence nobody could finish reading.
        var labels = NameWithHelp(Loc.T("settings.main_preset"),
            BuildKnobHelp("preset", description, scope), 14,
            Microsoft.UI.Text.FontWeights.SemiBold);
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

    private void AboutTimingGridSizeChanged(object sender, SizeChangedEventArgs e)
    {
        var compact = e.NewSize.Width > 0 && e.NewSize.Width < 800;
        Grid.SetRow(AboutTimingCard, 0);
        Grid.SetColumn(AboutTimingCard, 0);
        Grid.SetColumnSpan(AboutTimingCard, compact ? 2 : 1);
        Grid.SetRow(AboutSharingCard, compact ? 1 : 0);
        Grid.SetColumn(AboutSharingCard, compact ? 0 : 1);
        Grid.SetColumnSpan(AboutSharingCard, compact ? 2 : 1);
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

        // The name, and a question mark that holds everything else.
        //
        // Three stacked paragraphs under every single setting made the page
        // roughly four times taller than the controls needed and turned it into
        // something to be scrolled past rather than read. The explanation still
        // matters - especially "this only takes effect after reconnecting",
        // which is the whole reason the backend reports a scope at all - so it
        // moves one click away rather than being deleted.
        var labels = NameWithHelp(Label(key), BuildKnobHelp(key, description, scope), 14,
            Microsoft.UI.Text.FontWeights.Normal, BuildPowerHint(key));
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
            // Every rate LC3 defines, so a better headset than the author's is
            // not limited by a list written against one device. 48 kHz is the
            // codec's ceiling - Bluetooth LE Audio does not define LC3 above it,
            // whatever a driver specification sheet says about the speakers - so
            // there is nothing honest to put beyond it. Anything the connected
            // device did not publish is marked rather than hidden: PAC records
            // are frequently incomplete, and a value worth trying should be
            // reachable.
            "rate_hz" => Choice(key, value, new[]
                { "48000", "44100", "32000", "24000", "16000", "8000" }),
            "frame_ms" => Choice(key, value, new[] { "10", "7.5" }),
            "phy" => Choice(key, value, new[] { "2M", "1M" }),
            "octets" => SliderNumber(key, value, 20, 155, 1, Loc.T("slider.economical"), Loc.T("slider.detail")),
            "retransmissions" => SliderNumber(key, value, 0, 15, 1, Loc.T("slider.faster"), Loc.T("slider.resilient")),
            "max_latency_ms" => SliderNumber(key, value, 5, 200, 5, Loc.T("slider.lower_latency"), Loc.T("slider.more_headroom")),
            "presentation_delay_ms" => SliderNumber(key, value, 10, 200, 5, Loc.T("slider.faster"), Loc.T("slider.stable")),
            "gain" => SliderNumber(key, value, 0, 2, 0.05, Loc.T("slider.silent"), Loc.T("slider.boost")),
            "balance" => SliderNumber(key, value, -50, 50, 1, Loc.T("slider.left"), Loc.T("slider.right")),
            "link_timeout_s" => SliderNumber(key, value, 2, 30, 1, Loc.T("slider.drop_sooner"), Loc.T("slider.survive_range")),
            "battery_poll_min" => SliderNumber(key, value, 0, 60, 1, Loc.T("slider.never_ask"), Loc.T("slider.rarely")),
            "idle_link_latency" => SliderNumber(key, value, 0, 30, 1, Loc.T("slider.untouched"), Loc.T("slider.save_battery")),
            "idle_timeout_min" => SliderNumber(key, value, 0, 120, 1, Loc.T("slider.never"), Loc.T("slider.longer")),
            "reconnect_interval_s" => SliderNumber(key, value, 1, 60, 1, Loc.T("slider.often"), Loc.T("slider.gentle")),
            "reconnect_window_min" => SliderNumber(key, value, 0, 60, 1, Loc.T("slider.unlimited"), Loc.T("slider.limited")),
            "multipoint_yield_s" => SliderNumber(key, value, 2, 60, 1, Loc.T("slider.share_sooner"), Loc.T("slider.hold_longer")),
            "link_metrics" => Choice(key, value, new[]
            {
                ("off", Loc.T("choice.metrics_off")),
                ("signal", Loc.T("choice.metrics_signal")),
                ("full", Loc.T("choice.metrics_full")),
            }),
            "audio_mode" => Choice(key, value, new[]
            {
                ("stereo", Loc.T("choice.stereo")),
                ("legacy", Loc.T("choice.legacy")),
                ("mono", Loc.T("choice.mono")),
            }),
            "playback_source" or "monitor_source" when dynamicOptions.Length > 0 =>
                Choice(key, value, dynamicOptions),
            // Built from the paired list rather than typed. An address entered by
            // hand is one transposed character away from silently matching
            // nothing, and a setting that fails that quietly is worse than none.
            "device" => Choice(key, value, PreferredDeviceOptions(value)),
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
                "multipoint_yield_enabled" or
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

        // A ToggleSwitch asks for 32 pixels of height and a width wide enough
        // for a header it does not have. Left alone it decides how tall every
        // row containing one is, and how far from its own label it sits.
        if (control is ToggleSwitch toggleControl)
        {
            toggleControl.MinWidth = 0;
            toggleControl.MinHeight = 0;
            toggleControl.VerticalContentAlignment = VerticalAlignment.Center;
        }

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

        // Codec values are the only ones a device can refuse outright, so they
        // are the only ones that carry this. It updates when capabilities
        // arrive and when the value changes, so it can never describe a value
        // that is no longer selected.
        TextBlock? fitNote = null;
        if (CodecJudgedKeys.Contains(key))
        {
            fitNote = new TextBlock
            {
                FontSize = 11,
                TextWrapping = TextWrapping.Wrap,
                Visibility = Visibility.Collapsed,
                HorizontalAlignment = HorizontalAlignment.Left,
            };
            _codecNotes[key] = fitNote;
        }

        // Only controls that genuinely need the width take a row of their own.
        //
        // Everything used to, because a ComboBox beside a three-line paragraph
        // re-wrapped the paragraph and made opening a menu look as if the card
        // had rearranged itself. The paragraph now lives behind the question
        // mark, so a name and a menu sit on one line and the page reads as a
        // list of settings rather than a wall of prose.
        //
        // Asked of the control rather than looked up in a list of keys kept
        // somewhere else: that list had to be edited every time a slider was
        // added, and forgetting produced a control too narrow to use with
        // nothing to explain why.
        var wide = (control as FrameworkElement)?.Tag as string == NeedsFullWidth;
        // The saved marker goes beside a control that shares its row, and below
        // one that does not.
        //
        // It was always below, and it keeps its space when invisible so the
        // layout does not jump the moment it appears. On a one-line row that
        // reserved space sat under the control and pushed it above the middle
        // of the row - every switch and menu ended up a few pixels higher than
        // the label it belonged to, which reads as sloppiness rather than as
        // the side effect of an invisible element it is.
        // The marker sits on the name row, at the far right, and shares nothing
        // with the control.
        //
        // It keeps its width while invisible, so the layout does not jump when
        // it appears - which means wherever it is put, it takes that space from
        // something. In front of the control it indented every menu; behind it,
        // it cropped every slider short of the card edge. On the name row there
        // is space going spare, and the control below can run the full width.
        var column = new Grid { VerticalAlignment = VerticalAlignment.Center };
        column.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        column.Children.Add(control);

        saved.HorizontalAlignment = HorizontalAlignment.Right;
        saved.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(saved, 1);
        labels.Children.Add(saved);

        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.Children.Add(column);

        // Beside the label when there is room for both, underneath when there
        // is not.
        //
        // Three panels side by side leave each card about four hundred pixels
        // wide, and a name plus a menu does not fit in that: the name wrapped to
        // three lines and broke mid-word. Rather than pick one layout and make
        // the other window size look bad, the row measures itself - the same
        // thing the preset card and the About page already do.
        void Arrange(double width)
        {
            // Room for the longest name in this card plus a usable menu beside
            // it. Three panels inside a page capped at 1500 leave each card a
            // little over four hundred, which is enough - the previous figure of
            // 500 was above that, so every row stacked at the one window size
            // the layout was designed for and the page became a column of
            // full-width boxes.
            const double Together = 380;
            var stacked = wide || width < Together;

            Grid.SetColumnSpan(labels, stacked ? 2 : 1);
            Grid.SetRow(column, stacked ? 1 : 0);
            Grid.SetColumn(column, stacked ? 0 : 1);
            Grid.SetColumnSpan(column, stacked ? 2 : 1);
            column.Margin = stacked ? new Thickness(0, 6, 0, 0) : new Thickness(0);
            column.HorizontalAlignment = stacked
                ? HorizontalAlignment.Stretch
                : HorizontalAlignment.Right;

            if (control is ComboBox or TextBox)
            {
                control.HorizontalAlignment = stacked
                    ? HorizontalAlignment.Stretch
                    : HorizontalAlignment.Right;
                control.MinWidth = stacked ? 0 : 150;
                control.MaxWidth = stacked ? double.PositiveInfinity : 280;
            }
        }

        grid.SizeChanged += (_, e) => Arrange(e.NewSize.Width);
        Arrange(wide ? 0 : 420);

        // The capability warning gets its own full-width row underneath.
        //
        // It reads as a sentence, not a label, and a sentence wrapped into the
        // narrow right-hand column beside a dropdown comes out four words at a
        // time. It also has to be visible without opening anything: this is the
        // one thing on the page that says a value will be refused, and a
        // warning behind a question mark is a warning nobody sees in time.
        if (fitNote is not null)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            Grid.SetRow(fitNote, grid.RowDefinitions.Count - 1);
            Grid.SetColumnSpan(fitNote, 2);
            fitNote.Margin = new Thickness(0, 4, 0, 0);
            grid.Children.Add(fitNote);
            ApplyCodecWarning(key, value, fitNote);
        }

        return new Border
        {
            BorderBrush = Brush("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Padding = new Thickness(0, 10, 0, 10),
            Child = grid,
        };
    }

    /// <summary>The paired headphones, plus "whichever is nearest".</summary>
    private (string Value, string Label)[] PreferredDeviceOptions(string current)
    {
        var options = new List<(string, string)> { ("", Loc.T("choice.device_any")) };
        options.AddRange(_paired.Select(row => (row.Address, $"{row.Name}  ·  {row.Address}")));

        // A device paired on this machine but not currently in the list would
        // otherwise be dropped from the menu, and selecting anything else would
        // silently discard the saved choice.
        if (current.Length > 0 && !options.Any(option =>
                string.Equals(option.Item1, current, StringComparison.OrdinalIgnoreCase)))
        {
            options.Add((current, current));
        }

        return options.ToArray();
    }

    /// <summary>
    /// How one value of a setting is written on its own control.
    /// </summary>
    /// <remarks>
    /// So advice about a setting can name the option the way the menu does. A
    /// note saying to choose "mono" when the menu offers "Mono - one channel"
    /// leaves the reader matching strings by eye.
    /// </remarks>
    private static string ValueLabel(string key, string value) => (key, value) switch
    {
        ("audio_mode", "mono") => Loc.T("choice.mono"),
        ("audio_mode", "stereo") => Loc.T("choice.stereo"),
        ("audio_mode", "legacy") => Loc.T("choice.legacy"),
        ("microphone_mode", "off") => Loc.T("choice.mic_off"),
        ("microphone_mode", "on") => Loc.T("choice.mic_on"),
        ("frame_ms", _) => $"{value} ms",
        ("phy", _) => value,
        // Numbers speak for themselves; the unit is already in the label.
        _ => value,
    };

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

    /// <summary>A setting name with its help button, laid out so neither crowds the other.</summary>
    /// <remarks>
    /// A grid rather than a horizontal stack. A stack gives every child its
    /// natural width and lets the row overflow, so a name longer than the column
    /// pushed the question mark off the edge and it was silently clipped - the
    /// explanation was there and simply unreachable. Here the name takes the
    /// slack and wraps; the button always keeps its place.
    /// </remarks>
    private static Grid NameWithHelp(string name, FrameworkElement help, double fontSize,
        Windows.UI.Text.FontWeight weight, FrameworkElement? power = null)
    {
        // Auto for the name, star for the space after it, so the buttons sit
        // immediately beside the text rather than at the far right of the
        // column - out there they stop reading as "help about this setting" and
        // start reading as stray controls.
        //
        // The catch is that an Auto column asks for the width the text wants on
        // one line, so a long name in a narrow card overflowed and was clipped:
        // "Playback capture sc", with the question mark sliced in half. The
        // measured width below is what stops that. It cannot be expressed
        // declaratively, because the answer depends on how wide the card turned
        // out to be.
        var row = new Grid { ColumnSpacing = 6, VerticalAlignment = VerticalAlignment.Center };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var text = new TextBlock
        {
            Text = name,
            FontSize = fontSize,
            FontWeight = weight,
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
        };
        row.Children.Add(text);

        var buttons = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 2,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Center,
        };
        buttons.Children.Add(help);
        if (power is not null)
        {
            buttons.Children.Add(power);
        }

        Grid.SetColumn(buttons, 1);
        row.Children.Add(buttons);

        // Give the name everything except the room the buttons need. Without a
        // ceiling the Auto column reports the width of the whole name on one
        // line and the row is clipped instead of wrapping.
        row.SizeChanged += (_, e) =>
        {
            var reserved = (power is null ? 1 : 2) * 22 + 12;
            var available = e.NewSize.Width - reserved;
            text.MaxWidth = available > 40 ? available : 40;
        };

        return row;
    }

    /// <summary>
    /// The battery icon beside a setting, and what it costs the radio.
    /// </summary>
    /// <remarks>
    /// Shown only where there is something true to say. Most settings do not
    /// touch the radio at all, and a battery icon on all of them would make the
    /// few that matter invisible among the ones that do not.
    ///
    /// The figure is reserved airtime, computed from the configuration on the
    /// page - not a battery measurement, which this program has no way to take.
    /// The flyout says so, because a number that looks measured and is not is
    /// worse than no number.
    /// </remarks>
    private FrameworkElement? BuildPowerHint(string key)
    {
        if (!PowerEstimate.Affects(key, _settingValues))
        {
            return null;
        }

        var content = new StackPanel { Spacing = 8, MaxWidth = 320 };
        content.Children.Add(new TextBlock
        {
            Text = Loc.T("power.title"),
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });

        var saving = PowerEstimate.SavingIfCheapest(_settingValues, key);
        if (saving is { } share)
        {
            var cheapest = PowerEstimate.Cheapest(key);
            content.Children.Add(new TextBlock
            {
                Text = share < 0.005
                    ? Loc.T("power.already_cheapest")
                    : cheapest is null
                        ? Loc.T("power.saving", $"{share * 100:0}")
                        : Loc.T("power.saving_named", ValueLabel(key, cheapest), $"{share * 100:0}"),
                TextWrapping = TextWrapping.Wrap,
            });

            var duty = PowerEstimate.Airtime(_settingValues).DutyCycle;
            content.Children.Add(new TextBlock
            {
                Text = Loc.T("power.duty", $"{Math.Min(duty, 1.0) * 100:0.0}"),
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
                Foreground = Brush("TextFillColorSecondaryBrush"),
            });
        }

        if (PowerEstimate.Note(key, _settingValues) is { } noteKey)
        {
            content.Children.Add(new TextBlock
            {
                Text = Loc.T(noteKey),
                TextWrapping = TextWrapping.Wrap,
            });
        }

        content.Children.Add(new Border
        {
            Height = 1,
            Background = Brush("CardStrokeColorDefaultBrush"),
            Margin = new Thickness(0, 2, 0, 2),
        });
        content.Children.Add(new TextBlock
        {
            Text = Loc.T("power.caveat"),
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Foreground = Brush("TextFillColorTertiaryBrush"),
        });

        var button = new Button
        {
            // Battery10: the only one of the battery glyphs with enough fill to
            // still look like a battery rather than an empty rounded rectangle
            // at this size.
            Content = new FontIcon { Glyph = "\uE85A", FontSize = 13 },
            Padding = new Thickness(0),
            Width = 20,
            Height = 20,
            MinWidth = 20,
            CornerRadius = new CornerRadius(10),
            Background = Brush("SubtleFillColorTransparentBrush"),
            BorderThickness = new Thickness(0),
            Foreground = Brush("TextFillColorTertiaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
            Flyout = new Flyout { Content = content },
        };

        AutomationProperties.SetName(button, Loc.T("power.title"));
        ToolTipService.SetToolTip(button, Loc.T("power.title"));
        return button;
    }

    /// <summary>
    /// The question mark beside a setting name, and the explanation behind it.
    /// </summary>
    /// <remarks>
    /// A flyout rather than a tooltip: a tooltip cannot be opened deliberately,
    /// disappears while it is being read, and never appears at all by keyboard
    /// or touch. This one opens on click, stays until dismissed, and is
    /// reachable by tab - so the explanation is genuinely available rather than
    /// technically present.
    ///
    /// The scope line is last and separated, because it answers a different
    /// question from the rest: not "what does this do" but "why has nothing
    /// changed yet".
    /// </remarks>
    private FrameworkElement BuildKnobHelp(string key, string description, string scope)
    {
        var content = new StackPanel { Spacing = 8, MaxWidth = 320 };
        content.Children.Add(new TextBlock
        {
            Text = Description(key, description),
            TextWrapping = TextWrapping.Wrap,
        });

        var tradeoff = Tradeoff(key);
        if (!string.IsNullOrEmpty(tradeoff))
        {
            content.Children.Add(new TextBlock
            {
                Text = tradeoff,
                TextWrapping = TextWrapping.Wrap,
                FontSize = 12,
                Foreground = Brush("TextFillColorSecondaryBrush"),
            });
        }

        content.Children.Add(new Border
        {
            Height = 1,
            Background = Brush("CardStrokeColorDefaultBrush"),
            Margin = new Thickness(0, 2, 0, 2),
        });
        content.Children.Add(new TextBlock
        {
            Text = Scope(scope),
            TextWrapping = TextWrapping.Wrap,
            FontSize = 12,
            Foreground = Brush("TextFillColorTertiaryBrush"),
        });

        var button = new Button
        {
            Content = new FontIcon { Glyph = "\uE9CE", FontSize = 12 },
            Padding = new Thickness(0),
            Width = 20,
            Height = 20,
            MinWidth = 20,
            CornerRadius = new CornerRadius(10),
            Background = Brush("SubtleFillColorTransparentBrush"),
            BorderThickness = new Thickness(0),
            Foreground = Brush("TextFillColorTertiaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
            Flyout = new Flyout { Content = content },
        };

        // Named for screen readers and for the hover tooltip, so the button is
        // not an unlabelled circle to anyone who cannot see the glyph.
        AutomationProperties.SetName(button, Loc.T("settings.explain", Label(key)));
        ToolTipService.SetToolTip(button, Loc.T("settings.explain", Label(key)));

        return button;
    }

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
        var panel = new StackPanel
        {
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Tag = NeedsFullWidth,
        };
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

        var panel = new StackPanel
        {
            Spacing = 0,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            // A slider squeezed into whatever a translated label leaves over is
            // too short to aim with, so it takes a row of its own.
            Tag = NeedsFullWidth,
        };
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

        _settingValues[key] = value;

        // Judged before it is sent, so the warning appears as the value is
        // chosen rather than after the next reconnect fails.
        if (_codecNotes.TryGetValue(key, out var note))
        {
            ApplyCodecWarning(key, value, note);
        }

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
