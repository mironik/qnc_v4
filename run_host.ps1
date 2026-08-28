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

function Normalize-QncKey($Value) {
    if (-not $Value) { return "" }
    return ($Value.ToString().Trim().ToLowerInvariant() -replace "[-\s]", "_")
}

function Test-QncWorkerAutostart($Mode) {
    $modeKey = Normalize-QncKey $Mode
    switch ($modeKey) {
        { $_ -in @("1", "true", "yes", "on", "start", "enabled") } { return $true }
        { $_ -in @("0", "false", "no", "off", "none", "disabled") } { return $false }
    }
    return $false
}

function Test-QncWorkerRunning($RootPath) {
    $WorkerBin = Join-Path (Join-Path (Join-Path $RootPath "qnc-host") "target-check") "release\qnc-worker.exe"
    $Existing = Get-Process qnc-worker -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -eq $WorkerBin } catch { $false }
    } | Select-Object -First 1
    return $null -ne $Existing
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Instaliraj Rust: https://rustup.rs"
}
Push-Location $Root
cargo build --release -p qnc-host --target-dir $TargetDir
if ($LASTEXITCODE -ne 0) {
    Pop-Location
    exit $LASTEXITCODE
}
Pop-Location
Write-Host "QNC API: http://127.0.0.1:$($env:QNC_API_PORT)/api/health"
Write-Host "Native UI: .\run_app.ps1  (LAN: QNC_BIND_HOST=0.0.0.0 requires QNC_TRUSTED_LAN=1)"
Write-Host "Artifact workers: JobService owner"
Write-Host "Worker app: .\run_worker.ps1  (QNC_WORKER_AUTOSTART=0 disables local autostart)"
$WorkerAutostart = if ($env:QNC_WORKER_AUTOSTART) { $env:QNC_WORKER_AUTOSTART } else { "1" }
if (Test-QncWorkerAutostart $WorkerAutostart) {
    $WorkerScript = Join-Path $Root "run_worker.ps1"
    if (Test-QncWorkerRunning $Root) {
        Write-Host "Worker autostart: qnc-worker already running"
    } else {
        Write-Host "Worker autostart: $WorkerScript  (autostart=$WorkerAutostart)"
        Start-Process -FilePath "powershell.exe" -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $WorkerScript) -WorkingDirectory $Root -WindowStyle Hidden
    }
} else {
    Write-Host "Worker autostart: disabled by QNC_WORKER_AUTOSTART=$WorkerAutostart"
}
Write-Host "Host binary: $Bin"
Write-Host "Root: $Root"
& $Bin
