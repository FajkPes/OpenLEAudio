<#
.SYNOPSIS
    Extracts the adapter's initialisation sequence from a USBPcap capture.

.DESCRIPTION
    Turns a raw capture into the two things the firmware loader needs:

      1. the ordered list of HCI commands the Realtek driver sent, with vendor
         commands (OGF 0x3F) called out, and
      2. the firmware image itself, reassembled from the download commands.

    Realtek parts take firmware through a vendor command that carries an index
    byte followed by a chunk of the image. The chunks arrive in order, the index
    counts up, and the last one has its top bit set. Concatenating the chunk
    bodies reproduces the image byte for byte, which is exactly what our loader
    has to send back.

    Rather than hardcode that opcode, the dominant repeated vendor command is
    found in the capture and reported, so the same script still works if this
    adapter turns out to use a different one.

.PARAMETER Path
    The capture to read. Defaults to the newest pcap in the captures folder.

.PARAMETER OutputDir
    Where to write the reassembled firmware. Defaults next to the capture.
#>

param(
    [string]$Path,
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'

$Tshark = 'C:\Program Files\Wireshark\tshark.exe'
if (-not (Test-Path $Tshark)) { throw "tshark nenalezen: $Tshark" }

$captureDir = Join-Path (Split-Path $PSScriptRoot -Parent) 'captures'

if (-not $Path) {
    $Path = Get-ChildItem $captureDir -Filter *.pcap -ErrorAction SilentlyContinue |
        Sort-Object Length -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Path -or -not (Test-Path $Path)) { throw "Zadny zaznam k analyze." }
if (-not $OutputDir) { $OutputDir = Split-Path $Path -Parent }

Write-Host ""
Write-Host "  ROZBOR ZAZNAMU" -ForegroundColor Cyan
Write-Host "  soubor: $(Split-Path $Path -Leaf)"
Write-Host ""

# Finding the adapter by VID/PID only works when USBPcap injected descriptors
# for it, which does not happen for a device that re-enumerates mid-capture.
# Identify it by its traffic instead: HCI commands are class control transfers
# whose payload parses as opcode, length, then exactly that many parameter
# bytes. Nothing else on a USB bus looks quite like that in bulk.
$packets = & $Tshark -r $Path `
    -Y "usb.transfer_type==0x02 && usb.capdata" `
    -T fields -e frame.number -e frame.time_relative -e usb.device_address -e usb.capdata 2>$null

$commands = @()
foreach ($line in $packets) {
    $parts = $line -split "`t"
    if ($parts.Count -lt 4 -or -not $parts[3]) { continue }

    $bytes = ($parts[3] -replace '[^0-9a-fA-F]', '') -split '(..)' | Where-Object { $_ }
    if ($bytes.Count -lt 3) { continue }

    $opcode = [Convert]::ToInt32($bytes[1] + $bytes[0], 16)
    $length = [Convert]::ToInt32($bytes[2], 16)

    # The length byte must account for the rest of the payload, and a real
    # opcode never has OGF 0. Both together reject descriptor reads and the
    # other control traffic that shares this transfer type.
    if ($length -ne ($bytes.Count - 3)) { continue }
    if (($opcode -shr 10) -eq 0) { continue }

    $commands += [pscustomobject]@{
        Frame   = [int]$parts[0]
        Time    = [double]$parts[1]
        Address = $parts[2]
        Opcode  = $opcode
        Ogf     = ($opcode -shr 10)
        Length  = $length
        Payload = @($bytes | Select-Object -Skip 3)
    }
}

if ($commands.Count -eq 0) {
    Write-Warning "Zaznam neobsahuje zadne HCI prikazy. Inicializace se nechytila."
    Write-Warning "Nejcastejsi pricina: adapter je za externim hubem. Prepoj ho primo do desky."
    return
}

# More than one Bluetooth device can be on the bus, so keep the busiest talker.
$busiest = $commands | Group-Object Address | Sort-Object Count -Descending | Select-Object -First 1
$address = $busiest.Name
$commands = @($busiest.Group)

Write-Host "  HCI prikazy nasel na adrese $address"
Write-Host "  HCI prikazu: $($commands.Count)"
Write-Host ""

# The sequence itself, condensed: long runs of the same opcode are the firmware
# download and would otherwise bury everything interesting.
Write-Host "  Sekvence prikazu:" -ForegroundColor Cyan
$index = 0
while ($index -lt $commands.Count) {
    $opcode = $commands[$index].Opcode
    $run = 0
    while ($index + $run -lt $commands.Count -and $commands[$index + $run].Opcode -eq $opcode) { $run++ }

    $vendor = if ($commands[$index].Ogf -eq 0x3F) { "VENDOR" } else { "      " }
    $label = "0x{0:X4}" -f $opcode

    if ($run -gt 1) {
        $total = ($commands[$index..($index + $run - 1)] | Measure-Object -Property Length -Sum).Sum
        "    {0}  {1}  x{2,-5} celkem {3:N0} B parametru" -f $label, $vendor, $run, $total
    } else {
        "    {0}  {1}  len {2}  {3}" -f $label, $vendor, $commands[$index].Length, ($commands[$index].Payload -join ' ')
    }

    $index += $run
}

# The download command is the vendor opcode that repeats far more than any
# other. Anything sent only a handful of times is configuration, not payload.
$download = $commands |
    Where-Object { $_.Ogf -eq 0x3F } |
    Group-Object Opcode |
    Sort-Object Count -Descending |
    Select-Object -First 1

if (-not $download -or $download.Count -lt 8) {
    Write-Host ""
    Write-Warning "No vendor command repeats often enough to carry a firmware image."
    Write-Warning "Firmware se nejspis nenahraval - byl uz v cipu z drivejska."
    return
}

$opcodeLabel = "0x{0:X4}" -f [int]$download.Name
Write-Host ""
Write-Host "  Firmware se nahrava prikazem $opcodeLabel ($($download.Count) chunku)" -ForegroundColor Green

# Reassemble. The first parameter byte is the sequence index, the rest is image.
$image = New-Object System.Collections.Generic.List[byte]
$expected = 0
$indexLooksSequential = $true

foreach ($chunk in $download.Group) {
    if ($chunk.Payload.Count -lt 1) { continue }

    $sequence = [Convert]::ToInt32($chunk.Payload[0], 16)
    if (($sequence -band 0x7F) -ne ($expected -band 0x7F)) { $indexLooksSequential = $false }
    $expected++

    foreach ($byte in ($chunk.Payload | Select-Object -Skip 1)) {
        $image.Add([Convert]::ToByte($byte, 16))
    }
}

$stamp = [IO.Path]::GetFileNameWithoutExtension($Path)
$outFile = Join-Path $OutputDir "firmware-$stamp.bin"
[IO.File]::WriteAllBytes($outFile, $image.ToArray())

Write-Host "  chunk index is sequential: $(if ($indexLooksSequential) { 'yes' } else { 'NO - verify manually' })"
Write-Host "  slozeny obraz: $($image.Count) B"
Write-Host "  ulozeno: $outFile" -ForegroundColor Green

# Knowing where the image came from inside the container is what lets the loader
# rebuild it, so report the first bytes and how they line up with the .dat file.
$head = ($image | Select-Object -First 16 | ForEach-Object { "{0:X2}" -f $_ }) -join ' '
Write-Host "  prvnich 16 B: $head"

$signature = -join ($image | Select-Object -First 8 | ForEach-Object { [char]$_ })
Write-Host "  podpis: '$signature'"
Write-Host ""
