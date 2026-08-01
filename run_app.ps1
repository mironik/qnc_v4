# Pokreni native qnc-app (egui) - NIJE browser / web UI.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:QNC_ROOT = $Root
# Lokalni target u ovom treeu (ne sandbox cache).
$env:CARGO_TARGET_DIR = Join-Path $Root "target"
$HostUrl = if ($env:QNC_HOST_URL) { $env:QNC_HOST_URL } else { "http://127.0.0.1:8001" }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Instaliraj Rust: https://rustup.rs"
}

Write-Host "Building qnc-app release -> $env:CARGO_TARGET_DIR"
Push-Location $Root
cargo build --release -p qnc-app
Pop-Location

$Bin = Join-Path $env:CARGO_TARGET_DIR "release\qnc-app.exe"
if (-not (Test-Path $Bin)) {
    Write-Error "Binary not found: $Bin"
}

Write-Host ""
Write-Host "NATIVE app (egui window): $Bin"
Write-Host "Host URL: $HostUrl"
Write-Host "If you see AI / Virtual / Segment / Export XML - that is WEB in the browser. Close that tab."
Write-Host ""

& $Bin --host $HostUrl
