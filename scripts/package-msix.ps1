param (
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Version = "",
    [string]$PackageName = "",
    [string]$PublisherId = "",
    [string]$PublisherDisplayName = ""
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$rootDir = Split-Path -Parent $scriptDir

# 1. Resolve Version
if (-not $Version) {
    $tauriConfPath = Join-Path $rootDir "src-tauri/tauri.conf.json"
    if (Test-Path $tauriConfPath) {
        $tauriConf = Get-Content $tauriConfPath -Raw | ConvertFrom-Json
        $Version = $tauriConf.version
    } else {
        $Version = "0.2.4"
    }
}

# MSIX requires a 4-part version: Major.Minor.Build.Revision
$versionParts = $Version.Split(".")
while ($versionParts.Count -lt 4) {
    $versionParts += "0"
}
$msixVersion = ($versionParts[0..3] -join ".")

# 2. Resolve Package Identity
if (-not $PackageName) {
    $PackageName = $env:MS_STORE_PACKAGE_NAME
    if (-not $PackageName) { $PackageName = "CamLooper" }
}

if (-not $PublisherId) {
    $PublisherId = $env:MS_STORE_PUBLISHER_ID
    if (-not $PublisherId) { $PublisherId = "CN=CamLooper" }
}
if (-not $PublisherId.StartsWith("CN=", [System.StringComparison]::OrdinalIgnoreCase)) {
    $PublisherId = "CN=$PublisherId"
}

if (-not $PublisherDisplayName) {
    $PublisherDisplayName = $env:MS_STORE_PUBLISHER_DISPLAY_NAME
    if (-not $PublisherDisplayName) { $PublisherDisplayName = "CamLooper" }
}

Write-Host "=== CamLooper MSIX Packaging ==="
Write-Host "Target Architecture: $Target"
Write-Host "Package Name       : $PackageName"
Write-Host "Publisher ID       : $PublisherId"
Write-Host "Publisher Display  : $PublisherDisplayName"
Write-Host "Package Version    : $msixVersion (raw: $Version)"

# 3. Directories
$releaseDir = Join-Path $rootDir "src-tauri/target/$Target/release"
$stagingDir = Join-Path $releaseDir "bundle/msix_staging"
$outDir     = Join-Path $releaseDir "bundle/msix"

if (Test-Path $stagingDir) {
    Remove-Item $stagingDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stagingDir "Assets") | Out-Null
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$msixFileName = "camlooper_${Version}_x64-store.msix"
$msixPath = Join-Path $outDir $msixFileName

# 4. Copy Binaries and Dependencies
$exePath = Join-Path $releaseDir "camlooper.exe"
if (-not (Test-Path $exePath)) {
    throw "Executable not found at: $exePath. Please build the release binary first."
}
Copy-Item $exePath (Join-Path $stagingDir "camlooper.exe") -Force
Write-Host "Copied camlooper.exe"

# Drivers (softcam)
$softcamDll = Join-Path $rootDir "src-tauri/drivers/windows/softcam.dll"
if (Test-Path $softcamDll) {
    Copy-Item $softcamDll (Join-Path $stagingDir "softcam.dll") -Force
    Write-Host "Copied softcam.dll"
}
$softcamLicense = Join-Path $rootDir "src-tauri/drivers/windows/softcam-LICENSE.txt"
if (Test-Path $softcamLicense) {
    Copy-Item $softcamLicense (Join-Path $stagingDir "softcam-LICENSE.txt") -Force
}

# FFmpeg
$ffmpegSrc = Join-Path $rootDir "src-tauri/drivers/windows/ffmpeg"
if (Test-Path $ffmpegSrc) {
    Copy-Item $ffmpegSrc (Join-Path $stagingDir "ffmpeg") -Recurse -Force
    Write-Host "Copied bundled FFmpeg binaries and DLLs"
}

# 5. Copy Store Icons to Assets/
$iconsDir = Join-Path $rootDir "src-tauri/icons"
$requiredIcons = @(
    @{ Src = "StoreLogo.png";         Dst = "StoreLogo.png" },
    @{ Src = "Square150x150Logo.png";   Dst = "Square150x150Logo.png" },
    @{ Src = "Square44x44Logo.png";     Dst = "Square44x44Logo.png" },
    @{ Src = "Square310x310Logo.png";   Dst = "Square310x310Logo.png" },
    @{ Src = "Square71x71Logo.png";     Dst = "Square71x71Logo.png" },
    @{ Src = "Wide310x150Logo.png";     Dst = "Wide310x150Logo.png" }
)

foreach ($icon in $requiredIcons) {
    $srcPath = Join-Path $iconsDir $icon.Src
    if (Test-Path $srcPath) {
        Copy-Item $srcPath (Join-Path $stagingDir "Assets\$($icon.Dst)") -Force
    } else {
        Write-Warning "Icon not found: $srcPath"
    }
}
Write-Host "Copied visual assets to Assets/"

# 6. Generate AppxManifest.xml
$manifestContent = @"
<?xml version="1.0" encoding="utf-8"?>
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="uap rescap">

  <Identity
    Name="$PackageName"
    Publisher="$PublisherId"
    Version="$msixVersion"
    ProcessorArchitecture="x64" />

  <Properties>
    <DisplayName>CamLooper</DisplayName>
    <PublisherDisplayName>$PublisherDisplayName</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>

  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.17763.0" MaxVersionTested="10.0.22621.0" />
  </Dependencies>

  <Resources>
    <Resource Language="en-US" />
  </Resources>

  <Applications>
    <Application Id="App"
      Executable="camlooper.exe"
      EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements
        DisplayName="CamLooper"
        Description="Turn any video into a virtual camera for Zoom, Teams, Meet, OBS and Discord"
        BackgroundColor="transparent"
        Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png">
        <uap:DefaultTile
          Square71x71Logo="Assets\Square71x71Logo.png"
          Square310x310Logo="Assets\Square310x310Logo.png"
          Wide310x150Logo="Assets\Wide310x150Logo.png" />
      </uap:VisualElements>
    </Application>
  </Applications>

  <Capabilities>
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
"@

$manifestPath = Join-Path $stagingDir "AppxManifest.xml"
[System.IO.File]::WriteAllText($manifestPath, $manifestContent, [System.Text.Encoding]::UTF8)
Write-Host "Generated AppxManifest.xml at $manifestPath"

# 7. Locate makeappx.exe
$makeAppx = $null
$sdkPaths = @(
    "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\makeappx.exe",
    "C:\Program Files\Windows Kits\10\bin\*\x64\makeappx.exe"
)

foreach ($pattern in $sdkPaths) {
    $found = Get-ChildItem -Path $pattern -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if ($found) {
        $makeAppx = $found.FullName
        break
    }
}

if (-not $makeAppx) {
    $cmd = Get-Command makeappx.exe -ErrorAction SilentlyContinue
    if ($cmd) { $makeAppx = $cmd.Source }
}

if (-not $makeAppx) {
    throw "makeappx.exe was not found. Ensure the Windows 10/11 SDK is installed."
}

Write-Host "Using makeappx: $makeAppx"

# 8. Pack MSIX
Write-Host "Packaging MSIX to $msixPath..."
& $makeAppx pack /d $stagingDir /p $msixPath /nv /o
if ($LASTEXITCODE -ne 0) {
    throw "makeappx failed with exit code $LASTEXITCODE"
}

Write-Host "MSIX package created successfully: $msixPath"

# 9. Test-sign with a self-signed cert for local testing (Microsoft Store replaces this upon upload)
$signTool = $null
$signToolPaths = @(
    "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe",
    "C:\Program Files\Windows Kits\10\bin\*\x64\signtool.exe"
)

foreach ($pattern in $signToolPaths) {
    $found = Get-ChildItem -Path $pattern -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if ($found) {
        $signTool = $found.FullName
        break
    }
}

if ($signTool) {
    Write-Host "Signing MSIX package for testing with signtool ($signTool)..."
    try {
        $cert = New-SelfSignedCertificate -Type Custom `
            -Subject $PublisherId `
            -KeyUsage DigitalSignature `
            -FriendlyName "CamLooper MSIX Signing" `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3") `
            -ErrorAction SilentlyContinue

        if ($cert) {
            & $signTool sign /fd SHA256 /sha1 $cert.Thumbprint $msixPath
            Write-Host "Signed MSIX successfully with certificate thumbprint: $($cert.Thumbprint)"
        }
    } catch {
        Write-Warning "Test-signing step skipped: $_. Store ingestion will still sign the package."
    }
}

Write-Host "=== MSIX Build Finished Successfully ==="
