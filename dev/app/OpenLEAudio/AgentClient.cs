using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace OpenLEAudio;

/// <summary>
/// The other half of the program: a Rust process that owns the adapter.
///
/// The radio stack is single threaded by construction, so it cannot be called
/// from a UI thread. Rather than hoping nobody ever does, it lives in its own
/// process and this class is the only way to reach it: one JSON object per line
/// in each direction, over a pipe.
///
/// Events arrive on a background reader thread and are handed straight to the
/// caller, who is responsible for getting them onto the UI thread. That is
/// deliberate - marshalling here would hide which thread the handler runs on.
/// </summary>
public sealed class AgentClient : IDisposable
{
    private readonly Process _process;
    private readonly StreamWriter _input;
    private readonly SemaphoreSlim _writeGate = new(1, 1);
    private int _disposing;
    private int _reading;

    /// <summary>Raised for every event line the agent emits.</summary>
    public event Action<JsonElement>? EventReceived;

    /// <summary>Raised when the agent writes to stderr, or dies.</summary>
    public event Action<string>? Trouble;

    private AgentClient(Process process)
    {
        _process = process;
        _input = process.StandardInput;
    }

    /// <summary>
    /// Starts the agent, looking for it beside the app and then in the usual
    /// cargo output directory so a developer build works without copying files.
    /// </summary>
    public static AgentClient Start()
    {
        var path = FindAgent()
            ?? throw new FileNotFoundException(
                "OpenLEAudio.Client.exe was not found. Build the core with " +
                "\"cargo build --release\" in the core directory.");

        var info = new ProcessStartInfo
        {
            FileName = path,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };

        var process = Process.Start(info)
            ?? throw new InvalidOperationException("OpenLEAudio Client could not be started.");

        return new AgentClient(process);
    }

    private static string? FindAgent()
    {
        var appDirectory = AppContext.BaseDirectory;

        var candidates = new[]
        {
            Path.Combine(appDirectory, "OpenLEAudio.Client.exe"),
            // A developer build, four levels up from bin/x64/Debug/net8.0-windows...
            Path.GetFullPath(Path.Combine(appDirectory, "..", "..", "..", "..", "..", "core", "target", "release", "OpenLEAudio.Client.exe")),
            Path.GetFullPath(Path.Combine(appDirectory, "..", "..", "..", "..", "..", "core", "target", "debug", "OpenLEAudio.Client.exe")),
        };

        foreach (var candidate in candidates)
        {
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        return null;
    }

    /// <summary>
    /// Starts consuming events after the caller has attached its handlers.
    /// Starting inside Start() races the agent's immediate "ready" event with
    /// the UI subscribing and can leave the whole window uninitialised.
    /// </summary>
    public void BeginReading()
    {
        if (Interlocked.Exchange(ref _reading, 1) != 0)
        {
            return;
        }

        _ = Task.Run(ReadStandardOutputAsync);
        _ = Task.Run(ReadStandardErrorAsync);
    }

    private async Task ReadStandardOutputAsync()
    {
        try
        {
            string? line;
            while ((line = await _process.StandardOutput.ReadLineAsync()) is not null)
            {
                if (string.IsNullOrWhiteSpace(line))
                {
                    continue;
                }

                try
                {
                    using var document = JsonDocument.Parse(line);
                    EventReceived?.Invoke(document.RootElement.Clone());
                }
                catch (JsonException)
                {
                    // A line that is not JSON is the stack talking to a human,
                    // not a protocol error. Show it rather than dropping it.
                    Trouble?.Invoke(line);
                }
            }

            ReportTrouble("The connection to the audio core ended.");
        }
        catch (Exception e) when (e is IOException or ObjectDisposedException or InvalidOperationException)
        {
            ReportTrouble($"The connection to the audio core failed: {e.Message}");
        }
    }

    private async Task ReadStandardErrorAsync()
    {
        try
        {
            string? line;
            while ((line = await _process.StandardError.ReadLineAsync()) is not null)
            {
                ReportTrouble(line);
            }
        }
        catch (Exception e) when (e is IOException or ObjectDisposedException or InvalidOperationException)
        {
            ReportTrouble($"The audio core diagnostic channel failed: {e.Message}");
        }
    }

    private void ReportTrouble(string text)
    {
        if (Volatile.Read(ref _disposing) == 0)
        {
            Trouble?.Invoke(text);
        }
    }

    /// <summary>Sends one command. Fire and forget; replies arrive as events.</summary>
    public async Task SendAsync(string command, Dictionary<string, object?>? arguments = null)
    {
        var payload = new Dictionary<string, object?> { ["cmd"] = command };
        if (arguments is not null)
        {
            foreach (var (key, value) in arguments)
            {
                payload[key] = value;
            }
        }

        await _writeGate.WaitAsync();
        try
        {
            ObjectDisposedException.ThrowIf(Volatile.Read(ref _disposing) != 0, this);
            await _input.WriteLineAsync(JsonSerializer.Serialize(payload));
            await _input.FlushAsync();
        }
        finally
        {
            _writeGate.Release();
        }
    }

    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposing, 1) != 0)
        {
            return;
        }

        try
        {
            // Finish a command write before adding the shutdown pair. StreamWriter
            // does not allow overlapping asynchronous operations; without this,
            // closing the window during a scan can corrupt the last JSON line.
            _writeGate.Wait();

            // Stop first, then quit. A quit that arrives while audio is playing
            // sits in a queue the worker cannot read until playback ends, and
            // playback does not end on its own.
            _input.WriteLine("{\"cmd\":\"stop\"}");
            _input.WriteLine("{\"cmd\":\"quit\"}");
            _input.Flush();
            _input.Close();
            _process.WaitForExit(3000);
        }
        catch (Exception)
        {
            // Shutting down; a dead pipe here is the expected case.
        }
        finally
        {
            _writeGate.Release();
        }

        try
        {
            if (!_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
            }
        }
        catch (InvalidOperationException)
        {
            // It exited between HasExited and Kill.
        }

        _process.Dispose();
        _writeGate.Dispose();
    }
}
