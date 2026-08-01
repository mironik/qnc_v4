# QNC active broadcast player test suite.
#
# This script targets the modular player stack only:
# qnc-broadcast-player, qnc-media-ffmpeg, qnc-player-output,
# qnc-player-monitor, qnc-player-monitor-bridge, qnc-player-runtime,
# and qnc-player-runner unless -Quick is used.

param(
    [switch]$Quick,
    [switch]$Full
)

$ErrorActionPreference = "Stop"

$AppDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Root = Split-Path -Parent $AppDir

function Invoke-Cargo {
    param(
        [string]$Label,
        [string[]]$CargoArgs
    )
    Write-Host "--- $Label ---" -ForegroundColor Yellow
    Write-Host ("cargo " + ($CargoArgs -join " ")) -ForegroundColor DarkGray
    & cargo @CargoArgs
    if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

Push-Location $Root
try {
    Write-Host "=== QNC active broadcast player tests ===" -ForegroundColor Cyan
    Write-Host "cwd: $Root"
    Write-Host ""

    if ($Full) {
        Invoke-Cargo -Label "Workspace" -CargoArgs @("test", "--workspace", "--no-fail-fast")
        Write-Host ""
        Write-Host "PASS - workspace green." -ForegroundColor Green
        exit 0
    }

    $packages = @(
        "qnc-broadcast-player",
        "qnc-media-ffmpeg",
        "qnc-player-output",
        "qnc-player-monitor",
        "qnc-player-monitor-bridge",
        "qnc-player-runtime"
    )

    if (-not $Quick) {
        $packages += "qnc-player-runner"
    }

    $cargoArgs = @("test", "--no-fail-fast")
    foreach ($package in $packages) {
        $cargoArgs += @("-p", $package)
    }
    Invoke-Cargo -Label "Active player crates" -CargoArgs $cargoArgs

    Write-Host ""
    if ($Quick) {
        Write-Host "PASS - active player core crates green." -ForegroundColor Green
    } else {
        Write-Host "PASS - active player stack green." -ForegroundColor Green
    }
    Write-Host "LIVE - pwsh -File qnc-app\scripts\test-broadcast-player-live.ps1" -ForegroundColor Cyan
} finally {
    Pop-Location
}
