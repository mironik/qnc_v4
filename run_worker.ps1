# Windows - run external QNC background worker.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:QNC_ROOT = $Root
$HostUrl = if ($env:QNC_HOST_URL) { $env:QNC_HOST_URL } else { "http://127.0.0.1:8001" }
$CapabilitiesRaw = if ($env:QNC_WORKER_CAPABILITIES) { $env:QNC_WORKER_CAPABILITIES } else { "proxy_generate,filmstrip" }
$Capabilities = $CapabilitiesRaw -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ }

# Reuse the same local target tree as run_host.ps1.
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

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Instaliraj Rust: https://rustup.rs"
}

$TargetDir = $env:CARGO_TARGET_DIR
$Bin = Join-Path $TargetDir "release\qnc-worker.exe"

Push-Location $Root
cargo build --release -p qnc-worker --target-dir $TargetDir
if ($LASTEXITCODE -ne 0) {
    Pop-Location
    exit $LASTEXITCODE
}
Pop-Location

if (-not (Test-Path $Bin)) {
    Write-Error "Binary not found: $Bin"
}

try {
    [System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = [System.Diagnostics.ProcessPriorityClass]::BelowNormal
} catch {
    Write-Host "Worker priority unchanged: $($_.Exception.Message)"
}

$WorkerArgs = @("--host-url", $HostUrl)
foreach ($Capability in $Capabilities) {
    $WorkerArgs += @("--capability", $Capability)
}

Write-Host "QNC worker: $Bin"
Write-Host "Host URL: $HostUrl"
Write-Host "Capabilities: $($Capabilities -join ', ')"
Write-Host "Priority: $([System.Diagnostics.Process]::GetCurrentProcess().PriorityClass)"
& $Bin @WorkerArgs
