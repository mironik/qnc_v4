# QNC active broadcast player live smoke tests.
#
# Runs qnc-player-runner real-output tests. The test suite creates real
# MP4/MOV/MXF/MPEG-TS/audio fixtures through FFmpeg. Set QNC_REAL_MXF_CORPUS_DIR
# to additionally test a local MXF corpus.

$ErrorActionPreference = "Stop"

$AppDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Root = Split-Path -Parent $AppDir

Push-Location $Root
try {
    Write-Host "=== QNC active broadcast player live tests ===" -ForegroundColor Cyan
    Write-Host "cwd: $Root"
    if ($env:QNC_REAL_MXF_CORPUS_DIR) {
        Write-Host "MXF corpus: $env:QNC_REAL_MXF_CORPUS_DIR"
    } else {
        Write-Host "MXF corpus: not configured; generated fixtures only" -ForegroundColor DarkGray
    }
    Write-Host ""

    Write-Host "cargo test -p qnc-player-runner --test real_output_smoke -- --nocapture" -ForegroundColor DarkGray
    & cargo test -p qnc-player-runner --test real_output_smoke -- --nocapture
    if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host ""
    Write-Host "PASS - qnc-player-runner live smoke green." -ForegroundColor Green
} finally {
    Pop-Location
}
