<#
.SYNOPSIS
    Configures VB-Audio Virtual Cable so the stack can capture from it.

.DESCRIPTION
    Audio reaches the headphones by this route:

        YouTube, games, anything  ->  Windows mixer  ->  CABLE Input (render)
        LC3 encoder  <-  our stack  <-  CABLE Output (capture)

    Two things have to line up for that to work. Both ends of the cable must run
    at the codec's sample rate, because the capture side refuses to open on a
    mismatch rather than silently resampling and adding latency. And CABLE Input
    has to be the default playback device, or nothing feeds into it.

    Read-only by default: without -Apply it reports what is set and what would
    change, and touches nothing.

    -Apply sets the endpoint format and makes CABLE Input the default output.
    Both are reversible: the previous values are saved first, and -Restore puts
    them back.

    Neither change is made by writing the registry. Those keys belong to SYSTEM
    and refuse even an administrator, which is why the first version of this
    script failed. The Sound control panel does not write them either - it calls
    IPolicyConfig and lets the audio service do it. That is the route taken here.

.PARAMETER Apply
    Actually make the changes. Without this the script only reports.

.PARAMETER Install
    Download the official VB-CABLE driver package, extract it into the release
    dependency cache, and start its x64 installer.

.PARAMETER Restore
    Put back whatever -Apply saved, and stop.

.PARAMETER Rate
    Sample rate to configure, in Hz. Must match the LC3 configuration.

.PARAMETER KeepDefaultDevice
    Configure the formats but leave the default playback device alone.
#>

param(
    [switch]$Install,
    [switch]$Apply,
    [switch]$Restore,
    [int]$Rate = 48000,
    [switch]$KeepDefaultDevice
)

$ErrorActionPreference = 'Stop'

$MMDevices = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio'
$FriendlyName = '{a45c254e-df1c-4efd-8020-67d146a850e0},2'
$DeviceFormat = '{f19f064d-082c-4e27-bc73-6882a1bb8e4c},0'
$BackupFile = Join-Path (Split-Path $PSScriptRoot -Parent) 'runtime-data\vbcable-backup.json'

# Windows gives no sign of life while it installs drivers or reconfigures audio
# endpoints, and these steps take tens of seconds. Without a line saying so the
# window looks finished and gets closed halfway through. Every long step is
# announced before it starts, and the end is unmistakable.
function Start-Step {
    param([string] $Text, [string] $Expect = 'this can take up to a minute')
    Write-Host ""
    Write-Host "-> $Text" -ForegroundColor Cyan
    Write-Host "   Working - $Expect. Do not close this window." -ForegroundColor DarkGray
}

function Complete-Step {
    param([string] $Text = 'Done.')
    Write-Host "   $Text" -ForegroundColor DarkGray
}

function Complete-Script {
    param([string] $Text)
    Write-Host ""
    Write-Host ("=" * 66)
    Write-Host "  FINISHED - $Text" -ForegroundColor Green
    Write-Host "  Nothing else is running. This window can be closed."
    Write-Host ("=" * 66)
}

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if ($Install) {
    $installed = Get-CimInstance Win32_SoundDevice -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '*VB-Audio*' -or $_.Name -like '*CABLE Input*' }
    if ($installed) {
        Complete-Script "VB-CABLE is already installed - nothing to do"
        exit 0
    }

    if (-not (Test-Elevated)) { throw "VB-CABLE installation requires an elevated PowerShell." }

    $releaseRoot = Split-Path $PSScriptRoot -Parent
    $dependencyRoot = Join-Path $releaseRoot 'dependencies'
    $archive = Join-Path $dependencyRoot 'VBCABLE_Driver_Pack45.zip'
    $expanded = Join-Path $dependencyRoot 'VB-CABLE'
    $downloadUrl = 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip'
    New-Item -ItemType Directory -Force $dependencyRoot | Out-Null

    if (-not (Test-Path $archive)) {
        Write-Host "Downloading the official VB-CABLE driver package..."
        Invoke-WebRequest -UseBasicParsing -Uri $downloadUrl -OutFile $archive
    } else {
        Write-Host "Using the cached VB-CABLE package from dependencies."
    }

    if (Test-Path $expanded) { Remove-Item -LiteralPath $expanded -Recurse -Force }
    Expand-Archive -LiteralPath $archive -DestinationPath $expanded -Force
    $installer = Get-ChildItem -LiteralPath $expanded -Recurse -File |
        Where-Object { $_.Name -eq 'VBCABLE_Setup_x64.exe' } |
        Select-Object -First 1
    if (-not $installer) { throw "VBCABLE_Setup_x64.exe was not found in the downloaded package." }

    Start-Step "Starting the official VB-CABLE installer" "it opens its own window - choose Install Driver there"
    $process = Start-Process -FilePath $installer.FullName -Verb RunAs -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "VB-CABLE installer exited with code $($process.ExitCode)." }
    Complete-Script "VB-CABLE is installed. Restart Windows, then run the configuration step"
    exit 0
}

# The stored value is a serialised PROPVARIANT: eight bytes of type header,
# then the WAVEFORMATEXTENSIBLE itself. Forty-eight bytes in total, and the
# header has to be carried through unchanged or the audio engine ignores it.
$FormatHeaderLength = 8

# A bare WAVEFORMATEXTENSIBLE - 16-bit stereo PCM at the requested rate, which
# is what the capture side of this stack converts most directly. This is what
# IPolicyConfig wants; the registry header above is only for reading back what
# is already stored.
function New-WaveFormat([int]$rate) {
    $blockAlign = 4
    $bytes = New-Object byte[] 40
    $write = {
        param($offset, $value, $size)
        $raw = [BitConverter]::GetBytes([int64]$value)
        [Array]::Copy($raw, 0, $bytes, $offset, $size)
    }

    & $write 0  0xFFFE                2   # wFormatTag: extensible
    & $write 2  2                     2   # nChannels
    & $write 4  $rate                 4   # nSamplesPerSec
    & $write 8  ($rate * $blockAlign) 4   # nAvgBytesPerSec
    & $write 12 $blockAlign           2   # nBlockAlign
    & $write 14 16                    2   # wBitsPerSample
    & $write 16 22                    2   # cbSize
    # WAVEFORMATEX ends at 18; WAVEFORMATEXTENSIBLE continues with a two-byte
    # union, then the channel mask, then the subformat GUID - 40 bytes in all.
    & $write 18 16                    2   # wValidBitsPerSample
    & $write 20 3                     4   # dwChannelMask: front left + right

    # KSDATAFORMAT_SUBTYPE_PCM
    $subformat = [guid]'00000001-0000-0010-8000-00aa00389b71'
    [Array]::Copy($subformat.ToByteArray(), 0, $bytes, 24, 16)

    return $bytes
}

# Strips the stored PROPVARIANT header, leaving the WAVEFORMATEXTENSIBLE.
function Get-WaveFormatFromStored([byte[]]$stored) {
    if (-not $stored -or $stored.Length -le $FormatHeaderLength) { return $null }
    $bytes = New-Object byte[] ($stored.Length - $FormatHeaderLength)
    [Array]::Copy($stored, $FormatHeaderLength, $bytes, 0, $bytes.Length)
    return $bytes
}

function Read-FormatBlob([byte[]]$bytes) {
    if (-not $bytes -or $bytes.Length -lt ($FormatHeaderLength + 16)) { return $null }
    $at = $FormatHeaderLength
    [pscustomobject]@{
        Channels = [BitConverter]::ToUInt16($bytes, $at + 2)
        Rate     = [BitConverter]::ToUInt32($bytes, $at + 4)
        Bits     = [BitConverter]::ToUInt16($bytes, $at + 14)
    }
}

# Finds the cable's endpoints. Both directions live under the same tree, keyed
# by an opaque GUID, so the friendly name is the only way in.
function Get-CableEndpoints {
    $found = @()
    foreach ($flow in 'Render', 'Capture') {
        $root = Join-Path $MMDevices $flow
        if (-not (Test-Path $root)) { continue }

        foreach ($key in Get-ChildItem $root -ErrorAction SilentlyContinue) {
            $properties = Join-Path $key.PSPath 'Properties'
            $name = (Get-ItemProperty $properties -Name $FriendlyName -ErrorAction SilentlyContinue).$FriendlyName
            if ($name -notlike '*CABLE*') { continue }

            $state = (Get-ItemProperty $key.PSPath -Name 'DeviceState' -ErrorAction SilentlyContinue).DeviceState

            $found += [pscustomobject]@{
                Flow       = $flow
                Name       = $name
                Id         = $key.PSChildName
                Properties = $properties
                Active     = ($state -eq 1)
                Format     = Read-FormatBlob (Get-ItemProperty $properties -Name $DeviceFormat -ErrorAction SilentlyContinue).$DeviceFormat
            }
        }
    }
    $found
}

# Neither the endpoint format nor the default device can be set by writing the
# registry: those keys belong to SYSTEM, and even an administrator is refused.
# The Sound control panel does not write them either - it calls IPolicyConfig,
# which asks the audio service to make the change. Undocumented, but it is the
# only route that works, and it is the same one Windows uses on itself.
function Get-EndpointPath($endpoint) {
    # Render endpoints are addressed 0.0.0, capture endpoints 0.0.1. The registry
    # key name already carries its own braces, so adding another pair produces a
    # path nothing matches and IPolicyConfig answers "element not found".
    $prefix = if ($endpoint.Flow -eq 'Render') { '{0.0.0.00000000}' } else { '{0.0.1.00000000}' }
    "$prefix.$($endpoint.Id)"
}

function Initialize-PolicyConfig {
    $source = @'
using System;
using System.Runtime.InteropServices;

[Guid("f8679f50-850a-41cf-9c72-430f290290c8")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPolicyConfig
{
    int GetMixFormat(string device, IntPtr format);
    int GetDeviceFormat(string device, bool def, IntPtr format);
    int ResetDeviceFormat(string device);
    int SetDeviceFormat(string device, IntPtr endpoint, IntPtr mix);
    int GetProcessingPeriod(string device, bool def, IntPtr fmt, IntPtr min);
    int SetProcessingPeriod(string device, IntPtr period);
    int GetShareMode(string device, IntPtr mode);
    int SetShareMode(string device, IntPtr mode);
    int GetPropertyValue(string device, bool store, IntPtr key, IntPtr value);
    int SetPropertyValue(string device, bool store, IntPtr key, IntPtr value);
    int SetDefaultEndpoint(string device, int role);
    int SetEndpointVisibility(string device, bool visible);
}

[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
public class PolicyConfigClient { }

// Just enough of the enumerator to read back which device is default, so the
// change can be undone rather than merely described.
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDeviceEnumerator
{
    int EnumAudioEndpoints(int dataFlow, int stateMask, out IntPtr devices);
    int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice device);
}

[Guid("D666063F-1587-4E43-81F1-B948E807363F")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDevice
{
    int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, out IntPtr iface);
    int OpenPropertyStore(int access, out IntPtr store);
    int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
    int GetState(out int state);
}

[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
public class MMDeviceEnumeratorClient { }

public static class AudioPolicy
{
    public static void SetDefault(string endpointPath)
    {
        IPolicyConfig config = (IPolicyConfig)new PolicyConfigClient();
        // Console, Multimedia, Communications - set all three so every
        // application follows, whatever role it asks for.
        for (int role = 0; role < 3; role++)
        {
            int hr = config.SetDefaultEndpoint(endpointPath, role);
            if (hr != 0) throw new COMException("SetDefaultEndpoint failed", hr);
        }
    }

    /// The device path of the current default output, or null if there is none.
    public static string GetDefaultRender()
    {
        try
        {
            IMMDeviceEnumerator enumerator = (IMMDeviceEnumerator)new MMDeviceEnumeratorClient();
            IMMDevice device;
            // eRender, eConsole
            if (enumerator.GetDefaultAudioEndpoint(0, 0, out device) != 0) return null;

            string id;
            if (device.GetId(out id) != 0) return null;
            return id;
        }
        catch
        {
            return null;
        }
    }

    public static void SetFormat(string endpointPath, byte[] waveFormat)
    {
        IPolicyConfig config = (IPolicyConfig)new PolicyConfigClient();
        IntPtr buffer = Marshal.AllocHGlobal(waveFormat.Length);
        try
        {
            Marshal.Copy(waveFormat, 0, buffer, waveFormat.Length);
            // The endpoint format and the mix format are the same request here:
            // we want the engine running at exactly what we hand it.
            int hr = config.SetDeviceFormat(endpointPath, buffer, buffer);
            if (hr != 0) throw new COMException("SetDeviceFormat failed", hr);
        }
        finally
        {
            Marshal.FreeHGlobal(buffer);
        }
    }
}
'@
    if (-not ('AudioPolicy' -as [type])) {
        Add-Type -TypeDefinition $source -Language CSharp | Out-Null
    }
}

# The default output has to be recorded before it is changed, or -Restore has
# nothing to put back.
function Get-DefaultRenderPath {
    Initialize-PolicyConfig
    [AudioPolicy]::GetDefaultRender()
}

Write-Host ""
Write-Host "  VB-CABLE setup for OpenLEAudio" -ForegroundColor Cyan
Write-Host ""

# ---- Restore ----

if ($Restore) {
    if (-not (Test-Path $BackupFile)) { throw "Zadna zaloha k obnoveni: $BackupFile" }
    if (-not (Test-Elevated)) { throw "Restoration requires an elevated PowerShell." }

    Initialize-PolicyConfig

    $backup = Get-Content $BackupFile -Raw | ConvertFrom-Json

    # A backup written by an older version of this script has no endpoint paths,
    # and the guard that stops -Apply overwriting an existing backup means such a
    # file can survive for a long time. Say so plainly rather than failing on
    # every entry in turn.
    if ($backup.Endpoints | Where-Object { -not $_.Path }) {
        Write-Warning "The backup was created by an older script version and cannot be restored automatically."
        Write-Warning "Delete '$BackupFile' and configure the device manually:"
        Write-Warning "Control Panel -> Sound -> Properties -> Advanced."
        return
    }

    foreach ($entry in $backup.Endpoints) {
        $format = Get-WaveFormatFromStored ([byte[]]$entry.Format)
        if (-not $format) {
            Write-Host "    $($entry.Name): no original format was saved; leaving it unchanged"
            continue
        }

        try {
            [AudioPolicy]::SetFormat($entry.Path, $format)
            Write-Host "    $($entry.Name): format obnoven"
        } catch {
            Write-Warning "$($entry.Name): restoration failed - $($_.Exception.Message)"
        }
    }

    if ($backup.DefaultRender) {
        try {
            [AudioPolicy]::SetDefault($backup.DefaultRender)
            Write-Host "    vychozi vystup obnoven"
        } catch {
            Write-Warning "The default output could not be restored: $($_.Exception.Message)"
        }
    }

    Write-Host ""
    Write-Host "  Obnoveno." -ForegroundColor Green
    Write-Host ""
    return
}

# ---- Report ----

$endpoints = @(Get-CableEndpoints)

if ($endpoints.Count -eq 0) {
    Write-Warning "VB-Audio Virtual Cable is not installed, or Windows has not completed its device setup yet."
    Write-Host "  Stahni z https://vb-audio.com/Cable/ a po instalaci restartuj PC."
    Write-Host ""
    return
}

$wanted = New-WaveFormat $Rate
$changes = @()

Write-Host "  Detected cable endpoints:"
foreach ($endpoint in $endpoints) {
    $current = if ($endpoint.Format) {
        "{0} Hz, {1} kanaly, {2} bit" -f $endpoint.Format.Rate, $endpoint.Format.Channels, $endpoint.Format.Bits
    } else {
        "not configured"
    }

    $ok = $endpoint.Format -and
          $endpoint.Format.Rate -eq $Rate -and
          $endpoint.Format.Channels -eq 2 -and
          $endpoint.Format.Bits -eq 16

    $mark = if ($ok) { 'OK  ' } else { 'ZMENIT' }
    $state = if ($endpoint.Active) { '' } else { '  (inactive)' }

    "    [{0}] {1,-8} {2,-42} {3}{4}" -f $mark, $endpoint.Flow, $endpoint.Name, $current, $state

    if (-not $ok) { $changes += $endpoint }
}

Write-Host ""
Write-Host "  Cilovy format: $Rate Hz, 2 kanaly, 16 bit"

# The capture side refuses to open on a mismatch, so name that consequence
# rather than leaving it to be discovered when playback silently fails.
$captureEnd = $endpoints | Where-Object { $_.Flow -eq 'Capture' } | Select-Object -First 1
if (-not $captureEnd) {
    Write-Warning "The capture endpoint CABLE Output is missing, so OpenLEAudio has no playback source."
}

if ($changes.Count -eq 0) {
    Write-Host "  Formats already match. No changes are needed." -ForegroundColor Green
} elseif (-not $Apply) {
    Write-Host ""
    Write-Host "  Nothing was changed. This was a status check only." -ForegroundColor Yellow
    Write-Host "  Run 'VB-CABLE - configure.bat' as administrator to apply changes."
}

# ---- Apply ----

if (-not $Apply) {
    Complete-Script "this was a read-only check - nothing was changed"
    return
}

if (-not (Test-Elevated)) { throw "Changing audio configuration requires an elevated PowerShell." }

Initialize-PolicyConfig

# Save first, so there is always a way back.
New-Item -ItemType Directory -Force (Split-Path $BackupFile -Parent) | Out-Null

if (-not (Test-Path $BackupFile)) {
    $backup = [pscustomobject]@{
        Saved         = (Get-Date).ToString('o')
        DefaultRender = Get-DefaultRenderPath
        Endpoints     = @($endpoints | ForEach-Object {
            [pscustomobject]@{
                Name       = $_.Name
                Path       = Get-EndpointPath $_
                Format     = (Get-ItemProperty $_.Properties -Name $DeviceFormat -ErrorAction SilentlyContinue).$DeviceFormat
            }
        })
    }
    $backup | ConvertTo-Json -Depth 5 | Set-Content $BackupFile -Encoding UTF8
    Write-Host ""
    Write-Host "  Original configuration saved to runtime-data\vbcable-backup.json"
}

Write-Host ""
$failed = 0
foreach ($endpoint in $changes) {
    try {
        [AudioPolicy]::SetFormat((Get-EndpointPath $endpoint), $wanted)
        Write-Host "    $($endpoint.Name): configured for $Rate Hz, 16-bit audio" -ForegroundColor Green
    } catch {
        $failed++
        Write-Warning "$($endpoint.Name): configuration failed - $($_.Exception.Message)"
    }
}

if ($failed -gt 0) {
    Write-Host ""
    Write-Warning "Configure failed endpoints manually: Control Panel -> Sound -> Properties"
    Write-Warning "device -> Advanced -> '2 channels, 16 bit, 48000 Hz'."
}

if (-not $KeepDefaultDevice) {
    $render = $endpoints | Where-Object { $_.Flow -eq 'Render' -and $_.Name -like '*CABLE Input*' } | Select-Object -First 1
    if (-not $render) {
        $render = $endpoints | Where-Object { $_.Flow -eq 'Render' } | Select-Object -First 1
    }

    if ($render) {
        try {
            [AudioPolicy]::SetDefault((Get-EndpointPath $render))
            Write-Host "    vychozi vystup Windows -> $($render.Name)" -ForegroundColor Green
        } catch {
            Write-Warning "The default output could not be changed: $($_.Exception.Message)"
            Write-Warning "Set it manually: Settings -> System -> Sound -> Output -> CABLE Input."
        }
    }
}

Write-Host ""
Write-Host "  Restore the previous state with 'VB-CABLE - restore.bat'."
Complete-Script "the cable is configured. Changes are already in effect - no restart needed"

