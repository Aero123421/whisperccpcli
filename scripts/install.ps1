param(
    [string]$Version = $(if ($env:WHISPERCLI_VERSION) { $env:WHISPERCLI_VERSION } else { "latest" }),
    [string]$InstallDir = $(if ($env:WHISPERCLI_INSTALL_DIR) { $env:WHISPERCLI_INSTALL_DIR } else { "$HOME\.whispercli\bin" }),
    [switch]$NoPath
)

$ErrorActionPreference = "Stop"
$Repo = "Aero123421/whisperccpcli"
$AssetName = "whispercli-windows-x64.zip"
$RootDir = Split-Path -Parent $InstallDir
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("whispercli-install-" + [Guid]::NewGuid())
$ZipPath = Join-Path $TempDir $AssetName
$ExePath = Join-Path $InstallDir "whispercli.exe"
$ChecksumsPath = Join-Path $TempDir "checksums.txt"

function Write-Step($Message) {
    Write-Host "==> $Message"
}

function Is-Truthy($Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }

    return $Value -match '^(1|true|yes|y)$' -as [bool]
}

function Resolve-Version($Value) {
    if ($Value -eq "latest") {
        return $Value
    }

    if ($Value -match "^v") {
        return $Value
    }

    return "v$Value"
}

function Get-DownloadUrl() {
    if ($Version -eq "latest") {
        return "https://github.com/$Repo/releases/latest/download/$AssetName"
    }

    return "https://github.com/$Repo/releases/download/$Version/$AssetName"
}

function Get-ChecksumsUrl() {
    if ($Version -eq "latest") {
        return "https://github.com/$Repo/releases/latest/download/checksums.txt"
    }

    return "https://github.com/$Repo/releases/download/$Version/checksums.txt"
}

function Get-Sha256($Path) {
    try {
        return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLower()
    }
    catch {
        throw "Cannot compute SHA256 for $Path. $_"
    }
}

function Add-ToUserPath($Directory) {
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @()

    if ($current) {
        $entries = $current.Split(";") | Where-Object { $_ -ne "" }
    }

    foreach ($entry in $entries) {
        if ($entry.TrimEnd("\") -ieq $Directory.TrimEnd("\")) {
            Write-Step "PATH already contains $Directory"
            return
        }
    }

    $updated = if ($current) { "$current;$Directory" } else { $Directory }
    [Environment]::SetEnvironmentVariable("Path", $updated, "User")
    Write-Step "Added $Directory to the user PATH"
    Write-Host "Open a new terminal before running whispercli by name."
}

function Test-FileWritable($Path) {
    if (!(Test-Path $Path)) {
        return
    }

    try {
        $stream = [System.IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
        $stream.Close()
    }
    catch {
        throw "Cannot update $Path because it is currently in use. Close all running whisperCLI windows, then run this installer again."
    }
}

try {
    $Version = Resolve-Version $Version
    $SkipDownload = Is-Truthy $env:WHISPERCLI_SKIP_DOWNLOAD

    Write-Step "Creating $RootDir"
    New-Item -ItemType Directory -Force -Path $RootDir | Out-Null
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "models") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "transcripts") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "logs") | Out-Null
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

    if ($SkipDownload) {
        Write-Step "WHISPERCLI_SKIP_DOWNLOAD is enabled, skipping download."
        if (!(Test-Path $ExePath)) {
            throw "whispercli.exe was not found at $ExePath. Install without skip first."
        }

        & $ExePath doctor
        exit
    }

    $url = Get-DownloadUrl
    Write-Step "Downloading $url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $ZipPath

    Write-Step "Checking checksum"
    $checksumAvailable = $true
    try {
        $checksumUrl = Get-ChecksumsUrl
        Invoke-WebRequest -Uri $checksumUrl -OutFile $ChecksumsPath
    }
    catch {
        Write-Step "Unable to download checksums.txt; skipping checksum verification."
        Write-Step "$_"
        $checksumAvailable = $false
    }

    if ($checksumAvailable) {
        $checksumLine = Get-Content -Path $ChecksumsPath |
            Where-Object { $_ -match "^\s*([A-Fa-f0-9]{64})\s+\*?$AssetName$" } |
            Select-Object -First 1
        if ($checksumLine) {
            $parts = $checksumLine -split "\s+"
            $expected = $parts[0].ToLower()
            $actual = Get-Sha256 $ZipPath
            if ($expected -ne $actual) {
                throw "SHA256 mismatch: expected $expected, got $actual"
            }
            Write-Step "SHA256 OK: $AssetName"
        }
        else {
            Write-Step "checksums.txt does not include $AssetName; skipping checksum verification."
        }
    }

    Write-Step "Installing to $InstallDir"
    Test-FileWritable $ExePath
    Expand-Archive -Path $ZipPath -DestinationPath $InstallDir -Force -ErrorAction Stop

    if (!(Test-Path $ExePath)) {
        throw "Install failed: $ExePath was not found in the downloaded archive."
    }

    if (!$NoPath) {
        Add-ToUserPath $InstallDir
    }

    Write-Step "Installed whisperCLI"
    & $ExePath doctor
}
finally {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force $TempDir
    }
}
