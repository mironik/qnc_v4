# QNC native app — broadcast player test suite (policy + contracts)
#
# Measures: carrier/nosilja, A/V lockstep, contiguous PCM, soft EOS, mono buses,
#            source rack A1 media + A2 silence.
# Does NOT measure: live FFmpeg continuous pipes, rodio OutputStream, EngineCommand loop.
#
# Usage (from QNC repo root or anywhere):
#   pwsh -File qnc-app\scripts\test-broadcast-player.ps1
#   pwsh -File qnc-app\scripts\test-broadcast-player.ps1 -Quick
#   pwsh -File qnc-app\scripts\test-broadcast-player.ps1 -Full

param(
    [switch]$Quick,
    [switch]$Full
)

$ErrorActionPreference = "Stop"
$AppDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not (Test-Path (Join-Path $AppDir "Cargo.toml"))) {
    Write-Error "Expected qnc-app at $AppDir"
}

function Invoke-QncAppTests {
    param([string[]]$Filters)
    Write-Host ("cargo test --bin qnc-app -- " + ($Filters -join " ")) -ForegroundColor DarkGray
    & cargo test --bin qnc-app -- @Filters
    if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

Push-Location $AppDir
try {
    Write-Host "=== broadcast player tests (qnc-app) ===" -ForegroundColor Cyan
    Write-Host "cwd: $AppDir"
    Write-Host ""

    Write-Host "--- [1/3] Quality + lifecycle policy ---" -ForegroundColor Yellow
    Invoke-QncAppTests -Filters @(
        "broadcast_quality",
        "thin_preroll",
        "eof_before",
        "soft_eos",
        "contiguous_emit",
        "play_start",
        "decode_frontier",
        "ffmpeg_bus_pipe",
        "carrier_decode_exhausted"
    )

    if ($Quick) {
        Write-Host ""
        Write-Host "Quick mode: skip rack contracts + full broadcast module." -ForegroundColor DarkGray
        Write-Host "PASS (quick)" -ForegroundColor Green
        exit 0
    }

    Write-Host ""
    Write-Host "--- [2/3] Source rack contracts (A1 media + A2 silence) ---" -ForegroundColor Yellow
    Invoke-QncAppTests -Filters @(
        "source_program_rack",
        "render_plan_routes_source_rack",
        "resolved_plan_maps_virtual_shot"
    )

    if ($Full) {
        Write-Host ""
        Write-Host "--- [3/3] Full qnc-app bin suite ---" -ForegroundColor Yellow
        & cargo test --bin qnc-app
        if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } else {
        Write-Host ""
        Write-Host "--- [3/3] All broadcast::* unit tests ---" -ForegroundColor Yellow
        Invoke-QncAppTests -Filters @("broadcast::")
    }

    Write-Host ""
    Write-Host "PASS — policy + contracts green." -ForegroundColor Green
    Write-Host "LIVE — pwsh -File qnc-app\scripts\test-broadcast-player-live.ps1" -ForegroundColor Cyan
    Write-Host "GAP — EngineCommand::Play + rodio device still separate from unit/live decode tests." -ForegroundColor DarkYellow
} finally {
    Pop-Location
}
