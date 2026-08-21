<#
.SYNOPSIS
    Binds a Bluetooth adapter to WinUSB for the OpenLEAudio stack, and puts it back.

.DESCRIPTION
    Rebinding is only safe if the way back is safe, so restore is the primary feature
    here, not an afterthought. Before anything changes, the current driver binding is
    written to a state file next to this script. Restore reads that file; if it is
    missing or damaged, the fallback still works, because removing the device makes
    Windows reinstall the original driver from its own driver store on the next scan.

    Nothing here runs unless you ask for it explicitly. -Status is read-only and is
    the default.

.PARAMETER Status
    Shows the current binding. Read-only. This is the default action.

.PARAMETER Bind
    Switches the adapter to WinUSB. Requires elevation. Windows Bluetooth stops
    working through this adapter until you restore it or the stack takes over.

.PARAMETER Restore
    Puts the original driver back. Requires elevation.

.PARAMETER HardwareId
    Which adapter to act on. Defaults to the ASUS USB-BT600.

.EXAMPLE
    .\adapter-driver.ps1 -Status
    .\adapter-driver.ps1 -Restore
#>

[CmdletBinding(DefaultParameterSetName = 'Status')]
param(
    [Parameter(ParameterSetName = 'Status')]
    [switch]$Status,

    [Parameter(ParameterSetName = 'Bind', Mandatory)]
    [switch]$Bind,

    [Parameter(ParameterSetName = 'Restore', Mandatory)]
    [switch]$Restore,

    [string]$HardwareId = 'USB\VID_0B05&PID_1D70'
)

$ErrorActionPreference = 'Stop'

$RuntimeData = Join-Path (Split-Path $PSScriptRoot -Parent) 'runtime-data'
$StateFile = Join-Path $RuntimeData 'adapter-state.json'
$InfPath = Join-Path (Split-Path $PSScriptRoot -Parent) 'driver\olea_winusb.inf'
$SigningCertSubject = 'CN=OpenLEAudio Driver Signing'

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-Adapter {
    # Do not enumerate only the Bluetooth class. Our signed INF intentionally
    # changes the node to USBDevice/WinUSB. CIM sees the same present PnP node
    # under both bindings and behaves consistently in Windows PowerShell 5.1.
    $device = Get-CimInstance Win32_PnPEntity -ErrorAction SilentlyContinue |
        Where-Object { $_.PNPDeviceID -like "$HardwareId*" } |
        Select-Object -First 1

    if (-not $device) {
        throw "Adapter $HardwareId is not present. Plug it in and try again."
    }
    [pscustomobject]@{
        FriendlyName = $device.Name
        InstanceId   = $device.PNPDeviceID
        Status       = $device.Status
    }
}

function Get-Binding($device) {
    $properties = @{}
    foreach ($key in 'DEVPKEY_Device_DriverInfPath', 'DEVPKEY_Device_DriverDesc',
                     'DEVPKEY_Device_Service', 'DEVPKEY_Device_DriverProvider') {
        $value = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName $key -ErrorAction SilentlyContinue).Data
        $properties[$key] = $value
    }
    $properties
}

function Find-OurDriverPackage {
    # pnputil lists packages by their published oemNN.inf name; ours is
    # recognisable by its original file name.
    $lines = & pnputil.exe /enum-drivers 2>&1
    $published = $null

    foreach ($line in $lines) {
        if ($line -match 'Published Name\s*:\s*(oem\d+\.inf)') {
            $published = $matches[1]
        }
        if ($line -match 'olea_winusb\.inf' -and $published) {
            return $published
        }
    }
    $null
}

function Show-Status {
    $device = Get-Adapter
    $binding = Get-Binding $device

    Write-Host ""
    Write-Host "Adapter    : $($device.FriendlyName)"
    Write-Host "Instance   : $($device.InstanceId)"
    Write-Host "State      : $($device.Status)"
    Write-Host "Driver INF : $($binding['DEVPKEY_Device_DriverInfPath'])"
    Write-Host "Driver     : $($binding['DEVPKEY_Device_DriverDesc'])"
    Write-Host "Service    : $($binding['DEVPKEY_Device_Service'])"
    Write-Host "Provider   : $($binding['DEVPKEY_Device_DriverProvider'])"

    $onWinUsb = $binding['DEVPKEY_Device_Service'] -eq 'WinUSB'
    Write-Host ""
    if ($onWinUsb) {
        Write-Host "Bound to WinUSB - the OpenLEAudio stack can drive this adapter." -ForegroundColor Yellow
        Write-Host "Windows Bluetooth does NOT work through it in this state."
        Write-Host "Run '.\adapter-driver.ps1 -Restore' to put Windows back."
    } else {
        Write-Host "Bound to the Windows driver - normal Bluetooth, stack cannot drive it." -ForegroundColor Green
    }

    if (Test-Path $StateFile) {
        $saved = Get-Content $StateFile -Raw | ConvertFrom-Json
        Write-Host ""
        Write-Host "Saved binding from $($saved.SavedAt): $($saved.InfPath) / $($saved.Service)"
    }
}

function Invoke-Bind {
    if (-not (Test-Elevated)) { throw "Bind needs an elevated PowerShell." }
    if (-not (Test-Path $InfPath)) { throw "INF not found at $InfPath" }

    $device = Get-Adapter
    $binding = Get-Binding $device

    if ($binding['DEVPKEY_Device_Service'] -eq 'WinUSB') {
        # Already ours. The interface may still be unpublished if a previous run
        # was interrupted, so restart the device rather than doing nothing.
        Write-Host "Adapter is already on WinUSB. Restarting the device to verify it..."
        & pnputil.exe /restart-device $device.InstanceId
        Write-Host ""
        Write-Host "Done. Verify with: PROBE - diagnostics.bat" -ForegroundColor Green
        return
    }

    # Rebinding takes the adapter away from Windows Bluetooth entirely. Ask first,
    # because the machine may have no other radio and the user would lose all
    # Bluetooth with no obvious way back.
    Write-Host ""
    Write-Host "  SWITCH ADAPTER TO WINUSB" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  Device   : $($device.FriendlyName)"
    Write-Host "  From     : $($binding['DEVPKEY_Device_DriverInfPath']) / $($binding['DEVPKEY_Device_Service'])"
    Write-Host "  To       : olea_winusb.inf / WinUSB"
    Write-Host ""
    Write-Host "  Windows Bluetooth will not use this adapter after the switch."
    Write-Host "  Restore it with: RESTORE Windows Bluetooth driver.bat"
    Write-Host ""

    $answer = Read-Host "  Continue? Type YES"
    if ($answer -ne 'YES') {
        Write-Host "`n  Canceled. Nothing was changed." -ForegroundColor Green
        return
    }

    # Record the way back before changing anything.
    New-Item -ItemType Directory -Force $RuntimeData | Out-Null
    [pscustomobject]@{
        SavedAt    = (Get-Date).ToString('s')
        InstanceId = $device.InstanceId
        HardwareId = $HardwareId
        InfPath    = $binding['DEVPKEY_Device_DriverInfPath']
        DriverDesc = $binding['DEVPKEY_Device_DriverDesc']
        Service    = $binding['DEVPKEY_Device_Service']
        Provider   = $binding['DEVPKEY_Device_DriverProvider']
    } | ConvertTo-Json | Set-Content $StateFile -Encoding UTF8

    Write-Host "Saved current binding to $StateFile"
    Write-Host "  INF     : $($binding['DEVPKEY_Device_DriverInfPath'])"
    Write-Host "  Service : $($binding['DEVPKEY_Device_Service'])"
    Write-Host ""
    Write-Host "Installing WinUSB binding..."

    & pnputil.exe /add-driver $InfPath /install

    # 3010 is ERROR_SUCCESS_REBOOT_REQUIRED: the package went in fine, Windows
    # just wants a reboot to finish tidying up. Treating it as failure aborted
    # the run before the device restart below, which is what actually publishes
    # the WinUSB device interface.
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) {
        throw "pnputil /add-driver failed with $LASTEXITCODE"
    }

    Write-Host ""
    Write-Host "Restarting the device so Windows registers the WinUSB interface..."
    & pnputil.exe /restart-device $device.InstanceId

    # The signed package is now in the Driver Store. Remove the temporary
    # machine-wide trust and private key so they cannot later sign unrelated
    # code trusted by this computer.
    foreach ($store in 'Root', 'TrustedPublisher', 'My') {
        Get-ChildItem "Cert:\LocalMachine\$store" -ErrorAction SilentlyContinue |
            Where-Object { $_.Subject -eq $SigningCertSubject } |
            ForEach-Object {
                Remove-Item $_.PSPath -Force
                Write-Host "Odebran docasny podpisovy certifikat z LocalMachine\$store"
            }
    }
    Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $SigningCertSubject } |
        ForEach-Object {
            Remove-Item $_.PSPath -Force
            Write-Host "Odebran starsi uzivatelsky podpisovy klic"
        }

    Write-Host ""
    Write-Host "Done. Verify with -Status." -ForegroundColor Yellow
}

function Invoke-Restore {
    if (-not (Test-Elevated)) { throw "Restore needs an elevated PowerShell." }

    $device = Get-Adapter

    if (Test-Path $StateFile) {
        $saved = Get-Content $StateFile -Raw | ConvertFrom-Json
        Write-Host "Restoring binding recorded at $($saved.SavedAt): $($saved.InfPath)"
    } else {
        Write-Warning "No state file. Falling back to letting Windows reinstall from its driver store."
    }

    # Our INF must leave the driver store first. It matches this hardware id
    # exactly, so a plain rescan could simply pick it again and the adapter would
    # stay on WinUSB - the rollback would look like it ran and change nothing.
    $ourPackage = Find-OurDriverPackage
    if ($ourPackage) {
        Write-Host "Removing the OpenLEAudio driver package: $ourPackage"
        & pnputil.exe /delete-driver $ourPackage /uninstall /force
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) {
            Write-Warning "delete-driver skoncil s kodem $LASTEXITCODE, pokracuji."
        }
    } else {
        Write-Host "The OpenLEAudio driver package is no longer installed."
    }

    Start-Sleep -Seconds 2
    Write-Host "Scanning for hardware so Windows reinstalls the original driver..."
    & pnputil.exe /scan-devices
    Start-Sleep -Seconds 3

    # Verify rather than assume. If the adapter is still on WinUSB the rollback
    # did not work, and saying so is more useful than a cheerful message.
    $after = Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like "$HardwareId*" } | Select-Object -First 1

    if ($after) {
        $binding = Get-Binding $after
        $service = $binding['DEVPKEY_Device_Service']

        Write-Host ""
        if ($service -eq 'WinUSB') {
            Write-Host "WARNING: the adapter is still on WinUSB." -ForegroundColor Red
            Write-Host "Unplug and reconnect the USB adapter, then run this script again."
        } else {
            Write-Host "The adapter is back on the Windows stack (service: $service)." -ForegroundColor Green
        }
    } else {
        Write-Host ""
        Write-Warning "The adapter is not visible after the scan. Unplug and reconnect it."
    }

    Write-Host ""
    Show-Status
}

switch ($PSCmdlet.ParameterSetName) {
    'Bind'    { Invoke-Bind }
    'Restore' { Invoke-Restore }
    default   { Show-Status }
}

