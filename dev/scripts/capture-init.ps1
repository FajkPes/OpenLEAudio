<#
.SYNOPSIS
    Captures how the Realtek driver initialises the adapter, firmware upload included.

.DESCRIPTION
    The chip answers no HCI under our stack, and the firmware container format is
    undocumented. Rather than guess it, this records what the vendor driver
    actually sends: USBPcap starts recording, the adapter is forced through a full
    re-enumeration, and the capture stops.

    NOTHING IS SENT TO THE DEVICE BY THIS SCRIPT. It only records what the
    Realtek driver does on its own.

    The first attempt drove the cycle with 'pnputil /restart-device', which turned
    out to rebind the driver without power-cycling the port: the capture held zero
    packets from the adapter. Disable/Enable takes the device off the bus and
    brings it back, so enumeration and firmware download happen for real.

.PARAMETER Manual
    Instead of cycling the device from software, wait for the user to physically
    unplug and replug it. The most reliable trigger there is, and the fallback if
    the software cycle still produces nothing.

.PARAMETER Seconds
    How long to keep recording after the device comes back.

.PARAMETER OutputDir
    Where to write the capture. Defaults to a captures folder in the project.
#>

param(
    [switch]$Manual,
    [int]$Seconds = 15,
    [string]$OutputDir = (Join-Path (Split-Path $PSScriptRoot -Parent) 'captures')
)

$ErrorActionPreference = 'Stop'

$UsbPcap = 'C:\Program Files\USBPcap\USBPcapCMD.exe'
$Tshark = 'C:\Program Files\Wireshark\tshark.exe'
$HardwareId = 'USB\VID_0B05&PID_1D70'

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-Elevated)) { throw "Packet capture requires an elevated PowerShell." }
if (-not (Test-Path $UsbPcap)) { throw "USBPcap nenalezen: $UsbPcap" }

New-Item -ItemType Directory -Force $OutputDir | Out-Null

$device = Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like "$HardwareId*" } | Select-Object -First 1
if (-not $device) { throw "Adapter $HardwareId is not connected." }

Write-Host ""
Write-Host "  ODPOSLECH INICIALIZACE ADAPTERU" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Zarizeni : $($device.FriendlyName)"
Write-Host "  Stav     : $($device.Status)"
$trigger = if ($Manual) { "manual unplug and reconnect" } else { "automatic disable and enable" }
Write-Host "  Spusteni : $trigger"
Write-Host ""
Write-Host "  Skript zaznamena, co posila Realtek driver pri startu adapteru."
Write-Host "  The capture sends nothing to the device."
Write-Host ""

# USBPcap filters a root hub. It reliably records devices sitting directly on
# one, but a device that re-enumerates underneath an external hub can slip past
# it: the hub's own port-change events show up while the enumeration itself does
# not, leaving a capture that looks fine and contains nothing. Say so up front
# rather than letting the user find out from an empty file.
$parentId = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_Parent' -ErrorAction SilentlyContinue).Data
$parent = if ($parentId) { Get-PnpDevice -InstanceId $parentId -ErrorAction SilentlyContinue }

if ($parent -and $parentId -notlike '*ROOT_HUB*') {
    Write-Host "  POZOR: adapter je zapojeny za externim hubem:" -ForegroundColor Yellow
    Write-Host "    $($parent.FriendlyName)"
    Write-Host ""
    Write-Host "  Capture through an external hub often records no packets." -ForegroundColor Yellow
    Write-Host "  Connect the adapter directly to a motherboard port and run the script again."
    Write-Host ""
    $answer = Read-Host "  Presto pokracovat? [a/N]"
    if ($answer -notmatch '^[aAyY]') { return }
    Write-Host ""
}

$service = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_Service').Data
if ($service -eq 'WinUSB') {
    Write-Host "  WARNING: the adapter is on WinUSB. Initialization capture requires" -ForegroundColor Yellow
    Write-Host "  the adapter to use its Windows Bluetooth driver first."
    Write-Host "  Run 'RESTORE Windows Bluetooth driver.bat', then run this script again."
    Write-Host ""
    return
}

# USBPcap exposes one control device per root hub, and which one carries our
# adapter is not knowable up front, so record on all of them and sort it out
# afterwards. Hubs that do not exist fail to open; their noise goes to a temp
# file rather than the console, so the output stays readable.
$captures = @()
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$errorLog = Join-Path $env:TEMP "usbpcap-stderr-$stamp.txt"

for ($n = 1; $n -le 8; $n++) {
    $control = "\\.\USBPcap$n"
    $output = Join-Path $OutputDir "usbpcap$n-$stamp.pcap"

    $process = Start-Process -FilePath $UsbPcap `
        -ArgumentList "-d", $control, "-o", "`"$output`"", "-A", "--inject-descriptors" `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardError $errorLog `
        -ErrorAction SilentlyContinue

    if ($process) {
        Start-Sleep -Milliseconds 300
        if (-not $process.HasExited) {
            $captures += [pscustomobject]@{ Process = $process; File = $output; Hub = $n }
            Write-Host "  nahravam na USBPcap$n"
        }
    }
}

if ($captures.Count -eq 0) { throw "Nepodarilo se spustit zadny zaznam." }

Write-Host ""

# Everything past this point must leave the adapter enabled, whatever happens.
try {
    if ($Manual) {
        Write-Host "  ===============================================" -ForegroundColor Yellow
        Write-Host "    UNPLUG THE USB ADAPTER NOW AND WAIT 3 SECONDS" -ForegroundColor Yellow
        Write-Host "    A ZASTRC HO ZPATKY DO STEJNEHO PORTU." -ForegroundColor Yellow
        Write-Host "  ===============================================" -ForegroundColor Yellow
        Write-Host ""
    } else {
        Write-Host "  Odpojuji adapter od sbernice..."
        Disable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
        Start-Sleep -Seconds 3

        Write-Host "  Reconnecting. Windows will enumerate and initialize the adapter now..."
        Enable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
    }

    Write-Host "  Nahravam $Seconds s..."
    Start-Sleep -Seconds $Seconds
} finally {
    # Guarantee the adapter is usable again even if something above threw.
    $state = Get-PnpDevice -InstanceId $device.InstanceId -ErrorAction SilentlyContinue
    if ($state -and $state.Status -ne 'OK') {
        Enable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false -ErrorAction SilentlyContinue
    }

    Write-Host "  Zastavuji zaznam..."
    foreach ($capture in $captures) {
        if (-not $capture.Process.HasExited) {
            $capture.Process.CloseMainWindow() | Out-Null
            Start-Sleep -Milliseconds 200
            if (-not $capture.Process.HasExited) { Stop-Process -Id $capture.Process.Id -Force }
        }
    }
}

Start-Sleep -Seconds 1

Write-Host ""
Write-Host "  Vysledek:" -ForegroundColor Green
$kept = @()
foreach ($capture in $captures) {
    if (Test-Path $capture.File) {
        $size = (Get-Item $capture.File).Length
        if ($size -gt 1024) {
            "    {0}  {1:N0} B" -f (Split-Path $capture.File -Leaf), $size
            $kept += $capture.File
        } else {
            Remove-Item $capture.File -Force -ErrorAction SilentlyContinue
        }
    }
}
Remove-Item $errorLog -Force -ErrorAction SilentlyContinue

# A capture without adapter traffic is worse than useless, because it looks like
# a result. Say plainly whether the interesting packets are actually in there.
if ($kept.Count -eq 0) {
    Write-Warning "Zadny zaznam neobsahuje data. Zkus to znovu s prepinacem -Manual."
} elseif (Test-Path $Tshark) {
    Write-Host ""
    Write-Host "  Chytila se inicializace?" -ForegroundColor Cyan

    # Matching on VID/PID does not work here: those only appear in the
    # descriptors USBPcap injects at start, and a device that re-enumerates
    # mid-capture never gets them. What identifies the adapter instead is the
    # shape of its traffic - a burst of control transfers appearing after the
    # capture was already running.
    $anyTraffic = $false

    foreach ($file in $kept) {
        $rows = & $Tshark -r $file -Y "frame.time_relative>1" `
            -T fields -e usb.device_address 2>$null

        $busy = $rows |
            Where-Object { $_ } |
            Group-Object |
            Where-Object { $_.Count -ge 50 } |
            Sort-Object Count -Descending

        foreach ($group in $busy) {
            Write-Host "    $(Split-Path $file -Leaf): adresa $($group.Name), $($group.Count) paketu po startu zaznamu"
            $anyTraffic = $true
        }
    }

    if ($anyTraffic) {
        Write-Host ""
        Write-Host "  Capture contains data. Run '4. ANALYZE capture.bat'," -ForegroundColor Green
        Write-Host "  ten pozna, jestli jsou to HCI prikazy adapteru."
    } else {
        Write-Warning "Po startu zaznamu neprisel zadny provoz - inicializace se nechytila."
        Write-Warning "Nejcastejsi pricina: adapter je za externim hubem. Prepoj ho primo do desky."
    }
}

Write-Host ""
Write-Host "  Stav adapteru po zaznamu:"
$after = Get-PnpDevice -InstanceId $device.InstanceId
Write-Host "    $($after.Status) - $($after.FriendlyName)"
