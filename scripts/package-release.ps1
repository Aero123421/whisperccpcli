param(
    [Parameter(Mandatory = $true)]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [string]$AssetName
)

$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
$Dist = Join-Path $Root "dist"
$Stage = Join-Path $Dist "stage-$Target"
$ExeName = if ($Target -like "*windows*") { "whispercli.exe" } else { "whispercli" }
$Binary = Join-Path $Root "target\$Target\release\$ExeName"

if (!(Test-Path $Binary)) {
    throw "Built binary not found: $Binary"
}

Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

Copy-Item $Binary (Join-Path $Stage $ExeName)
Copy-Item (Join-Path $Root "README.md") (Join-Path $Stage "README.md")
Copy-Item (Join-Path $Root "LICENSE") (Join-Path $Stage "LICENSE") -ErrorAction SilentlyContinue

$AssetPath = Join-Path $Dist $AssetName
Remove-Item -Force $AssetPath -ErrorAction SilentlyContinue

if ($AssetName.EndsWith(".zip")) {
    Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $AssetPath -Force
} else {
    Push-Location $Stage
    try {
        tar -czf $AssetPath *
    } finally {
        Pop-Location
    }
}

Write-Host "Created $AssetPath"
