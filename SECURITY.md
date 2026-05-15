# Security Policy

## Supported Versions

Security fixes are applied to the latest released version.

## Reporting a Vulnerability

Please report vulnerabilities privately through GitHub Security Advisories when available, or by opening a minimal issue that asks for a private contact path without publishing exploit details.

## Distribution Integrity

GitHub Releases include `checksums.txt` with SHA256 hashes for release archives. The npm, shell, and PowerShell installers attempt to verify downloaded archives against that file when it is available.

For stricter environments, avoid pipe-to-shell installation:

```sh
curl -fsSLO https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.sh
less install.sh
sh install.sh
```

```powershell
iwr https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.ps1 -OutFile install.ps1
Get-Content install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```
