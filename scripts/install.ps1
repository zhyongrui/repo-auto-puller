Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Repo = "zhyongrui/repo-auto-puller"
$InstallDir = Join-Path $env:LOCALAPPDATA "repo-auto-puller"
$ConfigDir = Join-Path $env:APPDATA "repo-auto-puller"
$ConfigPath = Join-Path $ConfigDir "config.toml"
$BinaryPath = Join-Path $InstallDir "repo-auto-puller.exe"
$Asset = "repo-auto-puller-x86_64-pc-windows-msvc.zip"

if ($env:PROCESSOR_ARCHITECTURE -notin @("AMD64", "x86_64")) {
    throw "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

try {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null

    $DownloadUrl = "https://github.com/$Repo/releases/latest/download/$Asset"
    $ArchivePath = Join-Path $TempDir $Asset

    Write-Host "Downloading $DownloadUrl"
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ArchivePath

    Expand-Archive -Path $ArchivePath -DestinationPath $TempDir -Force
    Copy-Item -Path (Join-Path $TempDir "repo-auto-puller.exe") -Destination $BinaryPath -Force

    if (-not (Test-Path $ConfigPath)) {
        @"
[defaults]
log_file = "$($env:LOCALAPPDATA.Replace('\', '\\'))/repo-auto-puller/repo-auto-puller.log"
state_file = "$($env:LOCALAPPDATA.Replace('\', '\\'))/repo-auto-puller/status.json"
history_file = "$($env:LOCALAPPDATA.Replace('\', '\\'))/repo-auto-puller/history.jsonl"
verbose = false

[[repositories]]
name = "my-repo"
path = "C:/path/to/your/repo"
interval_seconds = 60
enabled = true
dry_run = false
"@ | Set-Content -Path $ConfigPath -Encoding UTF8
        Write-Host "Wrote example config to $ConfigPath"
    }
    else {
        Write-Host "Keeping existing config at $ConfigPath"
    }

    Write-Host ""
    Write-Host "Installed repo-auto-puller to $BinaryPath"
    Write-Host "Config file: $ConfigPath"
    Write-Host ""
    Write-Host "Next steps:"
    Write-Host "1. Run: `"$BinaryPath`" --config `"$ConfigPath`" init --repo-path C:/path/to/your/repo --name my-repo"
    Write-Host "2. Run: `"$BinaryPath`" --config `"$ConfigPath`" check-config"
    Write-Host "3. Run: `"$BinaryPath`" --config `"$ConfigPath`" install-service --enable --start"
    Write-Host "4. Run: `"$BinaryPath`" --config `"$ConfigPath`" status --repo my-repo"
}
finally {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force $TempDir
    }
}
