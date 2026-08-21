using Microsoft.UI.Xaml;
using System;
using System.Linq;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

namespace OpenLEAudio;

public partial class App : Application
{
    private const string InstanceMutexName = @"Local\OpenLEAudio.Client.Instance";
    private const string ShowEventName = @"Local\OpenLEAudio.Client.Show";

    private Window? _window;
    private readonly Mutex _instanceMutex;
    private readonly bool _isPrimaryInstance;
    private EventWaitHandle? _showEvent;
    private EventWaitHandle? _stopListener;

    public App()
    {
        UnhandledException += OnUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += (_, eventArgs) =>
            ReportFatal("Unhandled runtime exception", eventArgs.ExceptionObject as Exception);
        TaskScheduler.UnobservedTaskException += (_, eventArgs) =>
        {
            ReportFatal("Unobserved background task exception", eventArgs.Exception);
            eventArgs.SetObserved();
        };
        _instanceMutex = new Mutex(initiallyOwned: true, InstanceMutexName, out _isPrimaryInstance);
        try
        {
            InitializeComponent();
        }
        catch (Exception exception)
        {
            ReportFatal("Application resources could not be loaded", exception);
            throw;
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (!_isPrimaryInstance)
        {
            SignalRunningInstance();
            Exit();
            return;
        }

        try
        {
            var window = new MainWindow();
            _window = window;
            _window.Activate();
            StartActivationListener(window);

            if (System.Environment.GetCommandLineArgs().Contains("--background"))
            {
                window.StartHidden();
                window.BeginStartupReconnect();
            }
        }
        catch (Exception exception)
        {
            ReportFatal("The main window could not be created", exception);
            Exit();
        }
    }

    private void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs args)
    {
        ReportFatal("OpenLEAudio encountered an unexpected UI error", args.Exception);
        args.Handled = true;
    }

    private static void ReportFatal(string summary, Exception? exception)
    {
        var details = exception is null ? summary : $"{summary}.{Environment.NewLine}{Environment.NewLine}{exception}";
        try
        {
            var logDirectory = Path.Combine(AppContext.BaseDirectory, "logs");
            Directory.CreateDirectory(logDirectory);
            var logPath = Path.Combine(logDirectory, "startup-error.log");
            File.AppendAllText(logPath,
                $"[{DateTimeOffset.Now:O}] {details}{Environment.NewLine}{Environment.NewLine}");
            details += $"{Environment.NewLine}{Environment.NewLine}Details were saved to:{Environment.NewLine}{logPath}";
        }
        catch
        {
            // The message box still reports the original error if logging fails.
        }

        MessageBox(IntPtr.Zero, details, "OpenLEAudio could not continue", 0x00000010);
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int MessageBox(IntPtr hWnd, string text, string caption, uint type);

    private void StartActivationListener(MainWindow window)
    {
        _showEvent = new EventWaitHandle(false, EventResetMode.AutoReset, ShowEventName);
        _stopListener = new EventWaitHandle(false, EventResetMode.ManualReset);
        var show = _showEvent;
        var stop = _stopListener;

        _ = Task.Run(() =>
        {
            var handles = new WaitHandle[] { show, stop };
            while (WaitHandle.WaitAny(handles) == 0)
                window.ShowFromExternalLaunch();
        });

        window.Closed += (_, _) =>
        {
            stop.Set();
            _showEvent?.Dispose();
            _stopListener?.Dispose();
            try { _instanceMutex.ReleaseMutex(); } catch (ApplicationException) { }
            _instanceMutex.Dispose();
        };
    }

    private static void SignalRunningInstance()
    {
        // The first process may still be creating its event. A bounded retry
        // handles two launches crossing without ever creating two windows.
        for (var attempt = 0; attempt < 20; attempt++)
        {
            try
            {
                using var show = EventWaitHandle.OpenExisting(ShowEventName);
                show.Set();
                return;
            }
            catch (WaitHandleCannotBeOpenedException)
            {
                Thread.Sleep(50);
            }
        }
    }
}
