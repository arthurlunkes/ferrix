# ==============================================================================
# Ferrix Core — Windows Installation Script (PowerShell)
# ==============================================================================

[CmdletBinding()]
param (
    [string]$InstallPath = "$env:ProgramFiles\Ferrix",
    [switch]$InstallService
)

$ErrorActionPreference = "Stop"

Write-Host "🦀 Installing Ferrix Core (Nginx-style Proxy Engine) on Windows..." -ForegroundColor Cyan

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$CoreDir = Split-Path -Parent $ScriptDir

# 1. Build release binary
Write-Host "==> Compiling high-performance binary with Cargo..." -ForegroundColor Green
Set-Location -Path $CoreDir
cargo build --release

$ReleaseBin = Join-Path $CoreDir "target\release\ferrix.exe"
if (-not (Test-Path $ReleaseBin)) {
    throw "Build failed: $ReleaseBin not found."
}

# 2. Create Destination Folder
Write-Host "==> Installing executable to $InstallPath..." -ForegroundColor Green
if (-not (Test-Path $InstallPath)) {
    New-Item -ItemType Directory -Path $InstallPath -Force | Out-Null
}

Copy-Item -Path $ReleaseBin -Destination (Join-Path $InstallPath "ferrix.exe") -Force

# 3. Copy default configuration
$ConfigDest = Join-Path $InstallPath "ferrix.yaml"
if (-not (Test-Path $ConfigDest)) {
    $RootConfig = Join-Path $CoreDir "..\ferrix.yaml"
    if (Test-Path $RootConfig) {
        Copy-Item -Path $RootConfig -Destination $ConfigDest -Force
    }
}

# 4. Add to System or User PATH
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
if ($CurrentPath -notlike "*$InstallPath*") {
    Write-Host "==> Adding $InstallPath to Machine PATH..." -ForegroundColor Green
    try {
        [Environment]::SetEnvironmentVariable("Path", "$CurrentPath;$InstallPath", "Machine")
        $env:Path += ";$InstallPath"
    } catch {
        Write-Warning "Could not update Machine PATH (administrator required). Updating User PATH instead."
        $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($UserPath -notlike "*$InstallPath*") {
            [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallPath", "User")
            $env:Path += ";$InstallPath"
        }
    }
}

Write-Host ""
Write-Host "🎉 Ferrix Core installed successfully!" -ForegroundColor Green
Write-Host "   Executable: $InstallPath\ferrix.exe"
Write-Host "   Configuration: $ConfigDest"
Write-Host "   Run from PowerShell/CMD with: ferrix --config `"$ConfigDest`""

