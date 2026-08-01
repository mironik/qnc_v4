# QNC v2 DB-first static guard.
# Keep this guard conservative: it must catch legacy runtime regressions without
# blocking the current web-to-native migration state.
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Split-Path -Parent $ScriptDir
$violations = New-Object System.Collections.Generic.List[string]

function Add-Violation($message) {
    $violations.Add($message) | Out-Null
}

foreach ($relative in @(
    "qnc-host\src",
    "qnc-app\src",
    "seed\system_seed.json",
    "seed\keyboard-shortcuts.json",
    "seed\tabs\project",
    "seed\tabs\ingest",
    "seed\tabs\story"
)) {
    $path = Join-Path $Root $relative
    if (-not (Test-Path $path)) {
        Add-Violation "missing required DB-first path: $relative"
    }
}

foreach ($legacyWeb in @("app", "plugins")) {
    $path = Join-Path $Root $legacyWeb
    if (Test-Path $path) {
        Add-Violation "legacy web tree still at repo root: $legacyWeb (must live only under web-arhive/)"
    }
}

$legacyEntrypoints = @(
    "main.py",
    "server.py",
    "app.py",
    "requirements.txt",
    "pyproject.toml"
)

foreach ($name in $legacyEntrypoints) {
    $path = Join-Path $Root $name
    if (Test-Path $path) {
        Add-Violation "legacy Python runtime entrypoint present: $name"
    }
}

$hostFiles = Get-ChildItem -Path (Join-Path $Root "qnc-host\src") -Recurse -File -Include *.rs
$hostPythonRefs = $hostFiles | Select-String -Pattern "FastAPI|uvicorn|python\s" -CaseSensitive:$false
foreach ($match in $hostPythonRefs) {
    Add-Violation "legacy Python runtime reference in $($match.Path):$($match.LineNumber)"
}

if ($violations.Count -gt 0) {
    Write-Host "DB-first guard failed:" -ForegroundColor Red
    foreach ($violation in $violations) {
        Write-Host " - $violation" -ForegroundColor Red
    }
    exit 1
}

Write-Host "OK: DB-first static guard"
