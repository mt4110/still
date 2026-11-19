#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $BinaryPath,
    [Parameter(Mandatory)] [ValidatePattern('^[A-Za-z0-9.-]{3,50}$')] [string] $PackageName,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $Publisher,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $PublisherDisplayName,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $DisplayName,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$')] [string] $Version,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $MakeAppxPath,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $OutputPath,
    [ValidateNotNullOrEmpty()] [string] $LicensePath = (Join-Path (Split-Path -Parent $PSScriptRoot) 'LICENSE'),
    [ValidateNotNullOrEmpty()] [string] $DesktopBinaryPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$packageVersion = [version] $Version
if ($packageVersion.Major -lt 1 -or $packageVersion.Major -gt 65535 -or
    $packageVersion.Minor -gt 65535 -or $packageVersion.Build -gt 65535 -or
    $packageVersion.Revision -ne 0) {
    throw 'Use a four-part Store version with a positive major version, parts <= 65535, and a zero revision.'
}

$binary = Get-Item -LiteralPath $BinaryPath
$makeAppx = Get-Item -LiteralPath $MakeAppxPath
$license = Get-Item -LiteralPath $LicensePath
if ($binary -isnot [IO.FileInfo] -or $makeAppx -isnot [IO.FileInfo] -or $license -isnot [IO.FileInfo]) {
    throw 'BinaryPath, MakeAppxPath and LicensePath must select files.'
}
$desktopBinary = $null
$appExecutable = 'still.exe'
$aliasExecutableAttributes = ''
$aliasSubsystemAttribute = ''
$appActivationAttributes = ''
if ($DesktopBinaryPath) {
    $desktopBinary = Get-Item -LiteralPath $DesktopBinaryPath
    if ($desktopBinary -isnot [IO.FileInfo]) {
        throw 'DesktopBinaryPath must select a file.'
    }
    $appExecutable = 'still-desktop.exe'
    $aliasExecutableAttributes = ' Executable="still.exe" EntryPoint="Windows.FullTrustApplication"'
    $aliasSubsystemAttribute = ' desktop4:Subsystem="console"'
    $appActivationAttributes = ' desktop4:SupportsMultipleInstances="true"'
}

$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
if ([IO.Path]::GetExtension($output) -ine '.msix') {
    throw 'OutputPath must end in .msix.'
}
if (Test-Path -LiteralPath $output) {
    throw "The package output already exists: $output"
}

function Assert-X64Binary([string] $Path, [int] $ExpectedSubsystem, [string] $SubsystemName) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
    try {
        $stream = $reader.BaseStream
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw 'The input does not have a Windows PE header.'
        }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 64 -or $peOffset -gt ($stream.Length - 94)) {
            throw 'The input has an invalid Windows PE header offset.'
        }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw 'The input must be a Windows x64 executable.'
        }
        $stream.Position = $peOffset + 20
        if ($reader.ReadUInt16() -lt 70) {
            throw 'The input has a truncated PE optional header.'
        }
        $stream.Position = $peOffset + 24
        if ($reader.ReadUInt16() -ne 0x020B) {
            throw 'The input must use the PE32+ format.'
        }
        $stream.Position = $peOffset + 92
        if ($reader.ReadUInt16() -ne $ExpectedSubsystem) {
            throw "The input must use the Windows $SubsystemName subsystem: $Path"
        }
    } finally {
        $reader.Dispose()
    }
}

function New-StillLogo([string] $Path, [int] $Size) {
    $image = [Drawing.Bitmap]::new($Size, $Size)
    try {
        $graphics = [Drawing.Graphics]::FromImage($image)
        $background = $null
        $lines = $null
        try {
            $background = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(20, 31, 43))
            $lines = [Drawing.Pen]::new([Drawing.Color]::FromArgb(86, 231, 188), 0.075 * $Size)
            $graphics.Clear([Drawing.Color]::Transparent)
            $graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
            $graphics.FillEllipse($background, 0.06 * $Size, 0.06 * $Size, 0.88 * $Size, 0.88 * $Size)
            $lines.StartCap = [Drawing.Drawing2D.LineCap]::Round
            $lines.EndCap = [Drawing.Drawing2D.LineCap]::Round
            $graphics.DrawLine($lines, 0.27 * $Size, 0.34 * $Size, 0.73 * $Size, 0.34 * $Size)
            $graphics.DrawLine($lines, 0.27 * $Size, 0.50 * $Size, 0.60 * $Size, 0.50 * $Size)
            $graphics.DrawLine($lines, 0.27 * $Size, 0.66 * $Size, 0.47 * $Size, 0.66 * $Size)
            $image.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
        } finally {
            if ($lines) { $lines.Dispose() }
            if ($background) { $background.Dispose() }
            $graphics.Dispose()
        }
    } finally {
        $image.Dispose()
    }
}

Add-Type -AssemblyName System.Drawing
$manifestName = [Security.SecurityElement]::Escape($PackageName)
$manifestPublisher = [Security.SecurityElement]::Escape($Publisher)
$manifestPublisherDisplayName = [Security.SecurityElement]::Escape($PublisherDisplayName)
$manifestDisplayName = [Security.SecurityElement]::Escape($DisplayName)
$manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:uap5="http://schemas.microsoft.com/appx/manifest/uap/windows10/5"
         xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
         xmlns:desktop4="http://schemas.microsoft.com/appx/manifest/desktop/windows10/4"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap uap5 uap10 desktop4 rescap">
  <Identity Name="$manifestName" Publisher="$manifestPublisher" Version="$packageVersion" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>$manifestDisplayName</DisplayName>
    <PublisherDisplayName>$manifestPublisherDisplayName</PublisherDisplayName>
    <Description>Local storage explorer and review-first cleanup tool.</Description>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.22000.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources>
    <Resource Language="en-us" />
  </Resources>
  <Applications>
    <Application Id="Still" Executable="$appExecutable" EntryPoint="Windows.FullTrustApplication"
                 uap10:TrustLevel="mediumIL" uap10:RuntimeBehavior="packagedClassicApp"$appActivationAttributes>
      <uap:VisualElements DisplayName="$manifestDisplayName" Description="Local storage explorer and review-first cleanup tool."
                          BackgroundColor="transparent" Square150x150Logo="Assets\Square150x150Logo.png"
                          Square44x44Logo="Assets\Square44x44Logo.png" />
      <Extensions>
        <uap5:Extension Category="windows.appExecutionAlias"$aliasExecutableAttributes>
          <uap5:AppExecutionAlias$aliasSubsystemAttribute>
            <uap5:ExecutionAlias Alias="still.exe" />
          </uap5:AppExecutionAlias>
        </uap5:Extension>
      </Extensions>
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
"@

$id = [guid]::NewGuid().ToString('N')
$stage = Join-Path ([IO.Path]::GetTempPath()) "still-msix-$id"
$outputDirectory = [IO.Path]::GetDirectoryName($output)
if (-not [IO.Directory]::Exists($outputDirectory)) {
    throw "The package output directory does not exist: $outputDirectory"
}
$draft = Join-Path $outputDirectory ".still-msix-$id.msix"
New-Item -ItemType Directory -Path $stage | Out-Null

try {
    $stagedBinary = Join-Path $stage 'still.exe'
    Copy-Item -LiteralPath $binary.FullName -Destination $stagedBinary
    Assert-X64Binary $stagedBinary 3 'console'
    if ($desktopBinary) {
        $stagedDesktopBinary = Join-Path $stage $appExecutable
        Copy-Item -LiteralPath $desktopBinary.FullName -Destination $stagedDesktopBinary
        Assert-X64Binary $stagedDesktopBinary 2 'GUI'
    }
    Copy-Item -LiteralPath $license.FullName -Destination (Join-Path $stage 'LICENSE')
    [IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, [Text.UTF8Encoding]::new($false))
    $assets = Join-Path $stage 'Assets'
    [IO.Directory]::CreateDirectory($assets) | Out-Null
    New-StillLogo (Join-Path $assets 'StoreLogo.png') 50
    New-StillLogo (Join-Path $assets 'Square44x44Logo.png') 44
    New-StillLogo (Join-Path $assets 'Square150x150Logo.png') 150

    & $makeAppx.FullName pack /no /d $stage /p $draft
    if ($LASTEXITCODE -ne 0) {
        throw "MakeAppx pack failed with exit code $LASTEXITCODE."
    }
    [IO.File]::Move($draft, $output)
    Get-Item -LiteralPath $output
} finally {
    if ([IO.File]::Exists($draft)) { [IO.File]::Delete($draft) }
    [IO.Directory]::Delete($stage, $true)
}
