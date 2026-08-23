<#
.SYNOPSIS
    Signs the WinUSB INF so Windows will install it.

.DESCRIPTION
    Windows refuses unsigned driver packages. The binary being installed is
    Microsoft's own WinUSB.sys - we only supply the INF that points a device at
    it - so a self-signed certificate is enough. This is exactly how Zadig and
    similar tools work, and it does not require test signing or touching Secure
    Boot.

    WHAT THIS CHANGES ON YOUR MACHINE:
      - Creates a non-exportable, administrator-owned code signing certificate
        named "OpenLEAudio Driver Signing"
      - Installs it into Trusted Root and Trusted Publishers (machine store)

    That means Windows will trust anything signed with THAT certificate. The
    private key is non-exportable in the machine certificate store. It is still
    a real trust decision. The adapter binding script removes the temporary
    trust and private key after the signed driver package is installed.

.PARAMETER Sign
    Creates the certificate if needed, builds the catalog and signs it.

.PARAMETER Remove
    Removes the certificate from both stores. The driver stays installed until
    you also run the adapter restore.

.PARAMETER Status
    Shows whether the certificate exists and whether the catalog is signed.
#>

[CmdletBinding(DefaultParameterSetName = 'Status')]
param(
    [Parameter(ParameterSetName = 'Status')]
    [switch]$Status,

    [Parameter(ParameterSetName = 'Sign', Mandatory)]
    [switch]$Sign,

    [Parameter(ParameterSetName = 'Remove', Mandatory)]
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'

$CertSubject = 'CN=OpenLEAudio Driver Signing'
$DriverDir = Join-Path (Split-Path $PSScriptRoot -Parent) 'driver'
$InfPath = Join-Path $DriverDir 'olea_winusb.inf'
$CatPath = Join-Path $DriverDir 'olea_winusb.cat'

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

function Get-SigningCert {
    Get-ChildItem Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $CertSubject } |
        Sort-Object NotAfter -Descending |
        Select-Object -First 1
}

function Find-SignTool {
    Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

function Show-Status {
    Write-Host ""
    $cert = Get-SigningCert
    if ($cert) {
        Write-Host "Certificate : present, valid until $($cert.NotAfter.ToString('yyyy-MM-dd'))"
        Write-Host "Otisk       : $($cert.Thumbprint)"

        $inRoot = Test-Path "Cert:\LocalMachine\Root\$($cert.Thumbprint)"
        $inPub = Test-Path "Cert:\LocalMachine\TrustedPublisher\$($cert.Thumbprint)"
        Write-Host "Trusted Root: $(if ($inRoot) { 'yes' } else { 'NO' })"
        Write-Host "Publisher   : $(if ($inPub) { 'yes' } else { 'NO' })"
    } else {
        Write-Host "Certificate : missing"
    }

    Write-Host "Katalog     : $(if (Test-Path $CatPath) { 'present' } else { 'missing' })"

    if (Test-Path $CatPath) {
        $sig = Get-AuthenticodeSignature $CatPath
        Write-Host "Podpis      : $($sig.Status)"
    }
    Write-Host ""
}

function Invoke-Sign {
    if (-not (Test-Elevated)) { throw "Driver signing requires an elevated PowerShell." }
    if (-not (Test-Path $InfPath)) { throw "INF not found: $InfPath" }

    $signtool = Find-SignTool
    if (-not $signtool) { throw "signtool.exe was not found in the Windows SDK." }

    # 1. Certificate
    $cert = Get-SigningCert
    if (-not $cert) {
        Write-Host "Vytvarim podpisovy certifikat..."
        $cert = New-SelfSignedCertificate `
            -Subject $CertSubject `
            -Type CodeSigningCert `
            -CertStoreLocation Cert:\LocalMachine\My `
            -NotAfter (Get-Date).AddYears(2) `
            -KeyUsage DigitalSignature `
            -KeyExportPolicy NonExportable
        Write-Host "  otisk: $($cert.Thumbprint)"
    } else {
        Write-Host "Pouzivam existujici certifikat $($cert.Thumbprint)"
    }

    # 2. Trust it, so Windows accepts packages signed with it.
    Write-Host "Instaluji certifikat do uloziste duveryhodnych..."
    $temp = Join-Path $env:TEMP 'olea-signing.cer'
    Export-Certificate -Cert $cert -FilePath $temp -Force | Out-Null

    foreach ($store in 'Root', 'TrustedPublisher') {
        Import-Certificate -FilePath $temp -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
        Write-Host "  LocalMachine\$store"
    }
    Remove-Item $temp -Force -ErrorAction SilentlyContinue

    # 3. Catalog. New-FileCatalog lives in Windows PowerShell 5.1, so call it there
    #    if this session is PowerShell 7.
    Write-Host "Vytvarim katalog..."
    if (Test-Path $CatPath) { Remove-Item $CatPath -Force }

    if (Get-Command New-FileCatalog -ErrorAction SilentlyContinue) {
        New-FileCatalog -Path $DriverDir -CatalogFilePath $CatPath -CatalogVersion 2 | Out-Null
    } else {
        $command = "New-FileCatalog -Path '$DriverDir' -CatalogFilePath '$CatPath' -CatalogVersion 2 | Out-Null"
        powershell.exe -NoProfile -Command $command
    }

    if (-not (Test-Path $CatPath)) { throw "The driver catalog could not be created." }

    # 4. Sign it.
    Write-Host "Podepisuji katalog..."
    & $signtool sign /sm /fd SHA256 /sha1 $cert.Thumbprint /t http://timestamp.digicert.com $CatPath
    if ($LASTEXITCODE -ne 0) {
        # Timestamping needs the internet; without it the signature still works
        # locally, it just expires with the certificate.
        Write-Warning "Timestamped signing failed. Retrying without a timestamp."
        & $signtool sign /sm /fd SHA256 /sha1 $cert.Thumbprint $CatPath
        if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }
    }

    Write-Host ""
    Write-Host "Done. You can now run: ADAPTER - switch to OpenLEAudio.bat" -ForegroundColor Green
    Show-Status
}

function Invoke-Remove {
    if (-not (Test-Elevated)) { throw "Certificate removal requires an elevated PowerShell." }

    $removed = 0
    foreach ($store in 'Root', 'TrustedPublisher') {
        Get-ChildItem "Cert:\LocalMachine\$store" -ErrorAction SilentlyContinue |
            Where-Object { $_.Subject -eq $CertSubject } |
            ForEach-Object {
                Remove-Item $_.PSPath -Force
                Write-Host "Odebrano z LocalMachine\$store"
                $removed++
            }
    }

    Get-ChildItem Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $CertSubject } |
        ForEach-Object {
            Remove-Item $_.PSPath -Force
            Write-Host "Odebran strojovy privatni klic"
            $removed++
        }

    # Remove keys created by releases before 0.9 as well.
    Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $CertSubject } |
        ForEach-Object {
            Remove-Item $_.PSPath -Force
            Write-Host "Odebran starsi uzivatelsky privatni klic"
            $removed++
        }

    if ($removed -eq 0) { Write-Host "Nothing to remove." }
    Write-Host ""
    Write-Host "Warning: the driver package may still be installed." -ForegroundColor Yellow
    Write-Host "Restore the adapter with: RESTORE Windows Bluetooth driver.bat"
}

switch ($PSCmdlet.ParameterSetName) {
    'Sign'   { Invoke-Sign;   Complete-Script "the driver package is signed" }
    'Remove' { Invoke-Remove; Complete-Script "the temporary signing certificates were removed" }
    default  { Show-Status;   Complete-Script "this was a read-only check - nothing was changed" }
}

