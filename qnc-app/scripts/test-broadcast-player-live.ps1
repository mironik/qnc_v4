# QNC native app — LIVE broadcast FFmpeg integration
#
# Runs continuous-pipe tests against a real media file.
# Default: auto-generate 2s lavfi fixture via ffmpeg (no env needed).
# Optional: $env:QNC_BROADCAST_TEST_MEDIA = "D:\media\clip.mp4"
#
# Usage:
#   pwsh -File qnc-app\scripts\test-broadcast-player-live.ps1

$ErrorActionPreference = "Stop"
$AppDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

Write-Host "=== broadcast player LIVE integration ===" -ForegroundColor Cyan

if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    Write-Host "SKIP: ffmpeg not on PATH (live tests will no-op inside Rust)." -ForegroundColor DarkYellow
}

if (-not [string]::IsNullOrWhiteSpace($env:QNC_BROADCAST_TEST_MEDIA)) {
    Write-Host "media: $($env:QNC_BROADCAST_TEST_MEDIA)"
} else {
    Write-Host "media: auto lavfi fixture (temp)"
}

Push-Location $AppDir
try {
    Write-Host "--- live_ffmpeg_integ + ffmpeg module ---" -ForegroundColor Yellow
    & cargo test --bin qnc-app -- live_ffmpeg_integ broadcast::ffmpeg:: --nocapture
    if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host ""
    Write-Host "PASS — live continuous ffmpeg path exercised." -ForegroundColor Green
    Write-Host "GAP — full EngineCommand::Play + rodio device still separate." -ForegroundColor DarkYellow
} finally {
    Pop-Location
}
