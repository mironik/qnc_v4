# Windows - pokreni QNC Rust host (bez Pythona)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:QNC_ROOT = $Root
$env:QNC_API_PORT = if ($env:QNC_API_PORT) { $env:QNC_API_PORT } else { "8001" }
# Always build into this tree — ignore inherited CARGO_TARGET_DIR from other checkouts.
$env:CARGO_TARGET_DIR = Join-Path (Join-Path $Root "qnc-host") "target-check"
if (-not $env:QNC_FFMPEG) {
    $wingetFfmpeg = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages") -Filter "ffmpeg.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($wingetFfmpeg) {
        $env:QNC_FFMPEG = $wingetFfmpeg.FullName
        $ffprobeSibling = Join-Path (Split-Path -Parent $wingetFfmpeg.FullName) "ffprobe.exe"
        if (Test-Path $ffprobeSibling) {
            $env:QNC_FFPROBE = $ffprobeSibling
        }
    }
}
$HostDir = Join-Path $Root "qnc-host"
$TargetDir = $env:CARGO_TARGET_DIR
$Bin = Join-Path $TargetDir "release\qnc-host.exe"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Instaliraj Rust: https://rustup.rs"
}
Get-Process -Name "qnc-host" -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "Stopping old qnc-host (pid $($_.Id))..."
    Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
}
Start-Sleep -Milliseconds 400

Push-Location $Root
cargo build --release -p qnc-host --target-dir $TargetDir
if ($LASTEXITCODE -ne 0) {
    Pop-Location
    exit $LASTEXITCODE
}
Pop-Location
Write-Host "QNC API: http://127.0.0.1:$($env:QNC_API_PORT)/api/health"
Write-Host "Native UI: .\run_app.ps1  (LAN: QNC_BIND_HOST=0.0.0.0 requires QNC_TRUSTED_LAN=1)"
Write-Host "External worker: .\run_worker.ps1  (default capability: proxy_generate)"
Write-Host "Host binary: $Bin"
Write-Host "Root: $Root"
& $Bin
