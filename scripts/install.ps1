param(
    [string]$Version = "latest",
    [string]$InstallDir = "$HOME\.whispercli\bin",
    [switch]$NoPath
)

$ErrorActionPreference = "Stop"
$Repo = "Aero123421/whisperccpcli"
$AssetName = "whispercli-windows-x64.zip"
$RootDir = Split-Path -Parent $InstallDir
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("whispercli-install-" + [Guid]::NewGuid())
$ZipPath = Join-Path $TempDir $AssetName

function Write-Step($Message) {
    Write-Host "==> $Message"
}

function Get-DownloadUrl() {
    if ($Version -eq "latest") {
        return "https://github.com/$Repo/releases/latest/download/$AssetName"
    }

    return "https://github.com/$Repo/releases/download/$Version/$AssetName"
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
    Write-Step "Creating $RootDir"
    New-Item -ItemType Directory -Force -Path $RootDir | Out-Null
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "models") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "transcripts") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $RootDir "logs") | Out-Null
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

    $url = Get-DownloadUrl
    Write-Step "Downloading $url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $ZipPath

    Write-Step "Installing to $InstallDir"
    $exe = Join-Path $InstallDir "whispercli.exe"
    Test-FileWritable $exe
    Expand-Archive -Path $ZipPath -DestinationPath $InstallDir -Force -ErrorAction Stop

    if (!(Test-Path $exe)) {
        throw "Install failed: $exe was not found in the downloaded archive."
    }

    if (!$NoPath) {
        Add-ToUserPath $InstallDir
    }

    Write-Step "Installed whisperCLI"
    & $exe doctor
}
finally {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force $TempDir
    }
}
