# QNC v2 — test na Windows (PowerShell)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$HostDir = Join-Path $Root "qnc-host"
$env:CARGO_TARGET_DIR = Join-Path $HostDir "target-check"
$Bin = Join-Path $env:CARGO_TARGET_DIR "release\qnc-host.exe"
$Port = if ($env:QNC_API_PORT) { $env:QNC_API_PORT } else { "18081" }
$Base = "http://127.0.0.1:$Port"

$env:QNC_ROOT = $Root
$env:QNC_API_PORT = $Port
$ProjectsRoot = Join-Path ([System.IO.Path]::GetTempPath()) "qnc-test-projects-$Port"
$env:QNC_PROJECTS_ROOT = $ProjectsRoot
if (Test-Path $ProjectsRoot) {
    Remove-Item -LiteralPath $ProjectsRoot -Recurse -Force
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Instaliraj Rust: https://rustup.rs"
}

Write-Host "DB-first guard (static)..."
& (Join-Path $Root "scripts\db-first-guard.ps1")
if ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "Checking qnc-host..."
Push-Location $HostDir
cargo check
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Write-Host "Building qnc-host..."
cargo build --release
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Write-Host "Legacy ingest migration (cargo test)..."
cargo test legacy_ingest --release -- --nocapture
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

$proc = Start-Process -FilePath $Bin -PassThru -NoNewWindow -WorkingDirectory $Root
Start-Sleep -Seconds 2

function Test-Get($url, $pattern, $name) {
    $r = Invoke-WebRequest -Uri $url -UseBasicParsing
    if ($r.Content -notmatch $pattern) { throw "FAIL: $name - $($r.Content)" }
    Write-Host "OK: $name"
}

function Test-GetJson($url, $pattern, $name) {
    $r = Invoke-WebRequest -Uri $url -UseBasicParsing
    if ($r.Content -notmatch $pattern) { throw "FAIL: $name - $($r.Content)" }
    Write-Host "OK: $name"
    return ($r.Content | ConvertFrom-Json)
}

function Test-PostJson($url, $body, $pattern, $name) {
    $json = if ($null -eq $body) { "{}" } else { ($body | ConvertTo-Json -Depth 20 -Compress) }
    $r = Invoke-WebRequest -Uri $url -Method POST -ContentType "application/json" -Body $json -UseBasicParsing
    if ($r.Content -notmatch $pattern) { throw "FAIL: $name - $($r.Content)" }
    Write-Host "OK: $name"
    return ($r.Content | ConvertFrom-Json)
}

try {
    $bootstrapProjectId = $null

    Test-Get "$Base/api/health" '"ok"' "GET /api/health"
    Test-Get "$Base/api/shell/runtime" 'shell_api_version' "GET /api/shell/runtime"
    $diag = Test-GetJson "$Base/api/shell/diagnostics" '"plugins_loaded"' "GET /api/shell/diagnostics"
    if ($diag.bind_host -ne "127.0.0.1") { throw "FAIL: diagnostics bind_host expected 127.0.0.1" }
    if ($diag.plugins_loaded_count -lt 1) { throw "FAIL: diagnostics plugins_loaded_count" }
    $dbFirst = Test-GetJson "$Base/api/shell/db-first" '"contract"' "GET /api/shell/db-first"
    if ($dbFirst.contract -ne "db-first-v1") { throw "FAIL: db-first contract mismatch" }
    if ($dbFirst.violations -and $dbFirst.violations.Count -gt 0) {
        throw "FAIL: db-first violations: $($dbFirst.violations -join '; ')"
    }
    if ($dbFirst.status -ne "ok") { throw "FAIL: db-first status expected ok" }
    Test-Get "$Base/api/shell/tabs" 'project' "GET /api/shell/tabs"
    Test-Get "$Base/api/shell/tabs" 'ingest' "GET /api/shell/tabs (ingest)"
    Test-Get "$Base/api/design-tools/status" '"mode":"open"' "GET /api/design-tools/status"
    Test-Get "$Base/api/shell/keyboard-shortcuts" 'presets' "GET /api/shell/keyboard-shortcuts (seed catalog)"
    $kb = Test-GetJson "$Base/api/shell/keyboard-shortcuts" '"presets"' "GET /api/shell/keyboard-shortcuts (parse)"
    $kbJson = $kb | ConvertTo-Json -Depth 30 -Compress
    if ($kbJson -match 'qstoryboard') {
        throw "FAIL: keyboard-shortcuts seed must not contain qstoryboard scope"
    }
    if ($kbJson -notmatch 'storyboard') {
        throw "FAIL: keyboard-shortcuts seed must contain storyboard scope"
    }
    Write-Host "OK: keyboard scopes use storyboard (no qstoryboard)"
    Test-Get "$Base/" 'qnc-app' "GET / (native product notice)"
    Test-Get "$Base/api/shell/components" '"status":"ok"' "GET /api/shell/components"

    $projects = Test-GetJson "$Base/api/projects" '"projects"' "GET /api/projects"
    if (-not $projects.active_project_id) {
        $bootstrap = Test-PostJson "$Base/api/projects" @{
            name = "QA bootstrap projekt"
        } '"active_project_id"' "POST /api/projects (bootstrap)"
        $bootstrapProjectId = $bootstrap.active_project_id
        if (-not $bootstrapProjectId) { throw "FAIL: bootstrap active_project_id missing" }
        $projects = Test-GetJson "$Base/api/projects" '"projects"' "GET /api/projects (after bootstrap)"
    }
    if (-not $projects.active_project_id) { throw "FAIL: active_project_id missing" }

    $templates = Test-GetJson "$Base/api/project-templates" 'tpl_breaking_news' "GET /api/project-templates"
    if (-not $templates.templates -or $templates.templates.Count -lt 1) {
        throw "FAIL: project templates not seeded"
    }
    foreach ($tpl in $templates.templates) {
        $tplTabs = @($tpl.settings.workspace.tabs)
        if ($tplTabs -contains "ingest_proxy") {
            throw "FAIL: template $($tpl.template_id) workspace.tabs contains ingest_proxy"
        }
    }
    $newsTpl = $templates.templates | Where-Object { $_.template_id -eq "tpl_news_package" } | Select-Object -First 1
    if ($newsTpl) {
        $ingestCount = @($newsTpl.settings.workspace.tabs | Where-Object { $_ -eq "ingest" }).Count
        if ($ingestCount -gt 1) {
            throw "FAIL: tpl_news_package workspace.tabs has duplicate ingest"
        }
    }
    Write-Host "OK: system templates use ingest tab id (no ingest_proxy)"

    $ui = Test-GetJson "$Base/api/projects/ui-state" '"ui_state"' "GET /api/projects/ui-state"
    if (-not $ui.ui_state) { throw "FAIL: ui_state missing" }

    $patched = Test-PostJson "$Base/api/projects/ui-state" @{
        project_name = "Test projekt QA"
        settings_override = @{ ai = @{ enabled = $true } }
    } '"ui_state"' "POST /api/projects/ui-state"
    if ($patched.ui_state.project_name -ne "Test projekt QA") {
        throw "FAIL: ui-state project_name not persisted"
    }

    $tplPatch = Test-PostJson "$Base/api/projects/ui-state" @{
        selected_template_id = "tpl_news_package"
        reset_settings_override = $true
    } '"ui_state"' "POST /api/projects/ui-state (template select)"
    if ($tplPatch.ui_state.selected_template_id -ne "tpl_news_package") {
        throw "FAIL: ui-state selected_template_id not persisted"
    }
    $tplReload = Test-GetJson "$Base/api/projects/ui-state" '"ui_state"' "GET /api/projects/ui-state (template reload)"
    if ($tplReload.ui_state.selected_template_id -ne "tpl_news_package") {
        throw "FAIL: ui-state template reload mismatch"
    }
    Write-Host "OK: Project tab template selection round-trip"

    $session = Test-PostJson "$Base/api/collab/session" @{
        display_name = "QA tester"
        role = "editor"
        station_id = "test-host"
        client_label = "test.ps1"
        project_id = $projects.active_project_id
    } '"session_id"' "POST /api/collab/session"
    if (-not $session.session.user_id) { throw "FAIL: collab session missing user_id" }

    $touched = Test-PostJson "$Base/api/collab/touch" @{
        session_id = $session.session.session_id
        project_id = $projects.active_project_id
    } '"session"' "POST /api/collab/touch"
    if (-not $touched.session.user_id) { throw "FAIL: collab touch missing user_id" }

    $created = Test-PostJson "$Base/api/projects/from-template" @{
        name = "QA test projekt"
        template_id = "tpl_breaking_news"
        settings_override = @{
            ai = @{ enabled = $false }
            storage = @{ projects_root = $ProjectsRoot }
            export = @{ directory = (Join-Path $ProjectsRoot "exports") }
        }
        user_id = $session.session.user_id
        session_id = $session.session.session_id
    } '"active_project_id"' "POST /api/projects/from-template"
    $newId = $created.active_project_id
    if (-not $newId) { throw "FAIL: from-template missing active_project_id" }

    $dbPath = Join-Path $ProjectsRoot "$newId\qnc_project.db"
    if (-not (Test-Path $dbPath)) {
        throw "FAIL: per-project db not created at $dbPath"
    }
    Write-Host "OK: qnc_project.db created ($dbPath)"

    $projectsListed = Test-GetJson "$Base/api/projects" '"projects"' "GET /api/projects (list after create)"
    if (-not ($projectsListed.projects | Where-Object { $_.project_id -eq $newId })) {
        throw "FAIL: created project missing from project index"
    }

    $openTargetId = $null
    if ($bootstrapProjectId -and $bootstrapProjectId -ne $newId) {
        $openTargetId = $bootstrapProjectId
    } else {
        $otherListed = @($projectsListed.projects | Where-Object { $_.project_id -ne $newId })
        if ($otherListed.Count -ge 1) {
            $openTargetId = $otherListed[0].project_id
        }
    }
    if ($openTargetId) {
        $opened = Test-PostJson "$Base/api/projects/open" @{
            project_id = $openTargetId
        } '"active_project_id"' "POST /api/projects/open"
        if ($opened.active_project_id -ne $openTargetId) {
            throw "FAIL: open did not set active_project_id"
        }
        $reopened = Test-PostJson "$Base/api/projects/open" @{
            project_id = $newId
        } '"active_project_id"' "POST /api/projects/open (restore active)"
        if ($reopened.active_project_id -ne $newId) {
            throw "FAIL: open restore did not set active_project_id"
        }
        Write-Host "OK: Project tab open/switch regression"
    } else {
        Write-Host "OK: Project tab open/switch regression (skipped - single project in index)"
    }

    $settings = Test-GetJson "$Base/api/projects/$([uri]::EscapeDataString($newId))/settings" '"settings"' "GET /api/projects/{id}/settings"
    if (-not $settings.settings.template_id) { throw "FAIL: project settings missing template_id" }

    $workspace = Test-GetJson "$Base/api/projects/$([uri]::EscapeDataString($newId))/workspace" '"tabs"' "GET /api/projects/{id}/workspace"
    if (-not $workspace.workspace.tabs) { throw "FAIL: workspace tabs missing" }
    if ($workspace.workspace.tabs -contains "ingest_proxy") {
        throw "FAIL: new project workspace.tabs contains ingest_proxy"
    }
    if ($workspace.workspace.active_step_id -eq "step_ingest_proxy") {
        throw "FAIL: new project active_step_id is step_ingest_proxy"
    }
    if ($workspace.workspace.entry_step_id -eq "step_ingest_proxy") {
        throw "FAIL: new project entry_step_id is step_ingest_proxy"
    }
    $activeIngest = @($workspace.workspace.steps | Where-Object { $_.status -eq "active" -and $_.tab_id -eq "ingest" })
    if ($activeIngest.Count -lt 1) {
        throw "FAIL: new project has no active ingest workflow step"
    }
    Write-Host "OK: new project workspace uses ingest (step_ingest)"

    foreach ($phantom in @('pool', 'qstoryboard', 'preview', 'search', 'library', 'off', 'audio')) {
        if ($workspace.workspace.tabs -contains $phantom) {
            throw "FAIL: workspace must not include phantom tab '$phantom'"
        }
    }
    foreach ($coreTab in @('project', 'ingest', 'storyboard')) {
        if ($workspace.workspace.tabs -notcontains $coreTab) {
            throw "FAIL: workspace missing tab '$coreTab'"
        }
    }
    Write-Host "OK: workspace is project + ingest + Story"

    $modulesCore = Test-GetJson "$Base/api/modules" '"modules"' "GET /api/modules"
    $poolMod = $modulesCore.modules | Where-Object { $_.tab_id -eq 'pool' -or $_.module_id -eq 'pool' } | Select-Object -First 1
    $storyMod = $modulesCore.modules | Where-Object { $_.tab_id -eq 'storyboard' -or $_.module_id -eq 'storyboard' } | Select-Object -First 1
    $qstoryMod = $modulesCore.modules | Where-Object { $_.tab_id -eq 'qstoryboard' -or $_.module_id -eq 'qstoryboard' } | Select-Object -First 1
    if (-not $storyMod) { throw "FAIL: story module missing from /api/modules list" }
    if ($poolMod -and $poolMod.enabled -ne $false) { throw "FAIL: media_pool plugin must be absent or disabled" }
    if ($storyMod.enabled -ne $true) { throw "FAIL: story plugin must be enabled" }
    if ($qstoryMod -and $qstoryMod.enabled -ne $false) { throw "FAIL: qstory plugin must be absent or disabled" }
    Write-Host "OK: Story enabled; legacy media_pool/qstory absent or disabled"

    $shellTabsCore = Test-GetJson "$Base/api/shell/tabs" '"tabs"' "GET /api/shell/tabs"
    if ($shellTabsCore.tabs | Where-Object { $_.tab_id -eq 'pool' }) {
        throw "FAIL: pool tab must not be visible"
    }
    if (-not ($shellTabsCore.tabs | Where-Object { $_.tab_id -eq 'storyboard' })) {
        throw "FAIL: storyboard tab should be visible when Story is enabled"
    }
    if ($shellTabsCore.tabs | Where-Object { $_.tab_id -eq 'qstoryboard' }) {
        throw "FAIL: qstoryboard tab must not be visible"
    }
    Write-Host "OK: Story tab visible; QStory/pool hidden"

    $storyState = Test-GetJson "$Base/api/story/state?project_id=$([uri]::EscapeDataString($newId))" '"project_id"' "GET /api/story/state"
    if (-not $storyState) { throw "FAIL: story API state missing" }
    Write-Host "OK: story API mounted"

    try {
        Invoke-WebRequest -Uri "$Base/api/media-pool/clips?project_id=$([uri]::EscapeDataString($newId))" -UseBasicParsing -ErrorAction Stop | Out-Null
        throw "FAIL: media-pool API must not be mounted in core profile"
    } catch {
        $status = $null
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        if ($status -ne 404) {
            throw "FAIL: media-pool API expected 404 in core profile (got $status)"
        }
    }
    Write-Host "OK: media-pool API not mounted (404)"

    $env:QNC_TEST_PROJECT_DB = $dbPath
    $env:QNC_TEST_PROJECT_ID = $newId
    Push-Location $HostDir
    $cargoTag = [guid]::NewGuid().ToString()
    $cargoOut = Join-Path ([System.IO.Path]::GetTempPath()) "qnc-legacy-ingest-$cargoTag.out.txt"
    $cargoErr = Join-Path ([System.IO.Path]::GetTempPath()) "qnc-legacy-ingest-$cargoTag.err.txt"
    $cargoProc = Start-Process `
        -FilePath "cargo" `
        -ArgumentList @("test", "legacy_ingest_corrupt_and_migrate", "--release", "--", "--nocapture") `
        -Wait `
        -PassThru `
        -WindowStyle Hidden `
        -RedirectStandardOutput $cargoOut `
        -RedirectStandardError $cargoErr
    if (Test-Path $cargoOut) { Get-Content -LiteralPath $cargoOut | Write-Host }
    if (Test-Path $cargoErr) { Get-Content -LiteralPath $cargoErr | Write-Host }
    Remove-Item -LiteralPath $cargoOut, $cargoErr -Force -ErrorAction SilentlyContinue
    if ($cargoProc.ExitCode -ne 0) {
        Pop-Location
        Remove-Item Env:QNC_TEST_PROJECT_DB -ErrorAction SilentlyContinue
        Remove-Item Env:QNC_TEST_PROJECT_ID -ErrorAction SilentlyContinue
        throw "FAIL: legacy ingest_proxy corruption fixture"
    }
    Pop-Location
    Remove-Item Env:QNC_TEST_PROJECT_DB -ErrorAction SilentlyContinue
    Remove-Item Env:QNC_TEST_PROJECT_ID -ErrorAction SilentlyContinue

    $legacyWorkspace = Test-GetJson "$Base/api/projects/$([uri]::EscapeDataString($newId))/workspace" '"tabs"' "GET /api/projects/{id}/workspace (legacy ingest migration)"
    if ($legacyWorkspace.workspace.tabs -contains "ingest_proxy") {
        throw "FAIL: migrated workspace.tabs still contains ingest_proxy"
    }
    if ($legacyWorkspace.workspace.active_step_id -eq "step_ingest_proxy") {
        throw "FAIL: migrated active_step_id is step_ingest_proxy"
    }
    if ($legacyWorkspace.workspace.entry_step_id -eq "step_ingest_proxy") {
        throw "FAIL: migrated entry_step_id is step_ingest_proxy"
    }
    $ingestSteps = @($legacyWorkspace.workspace.steps | Where-Object { $_.tab_id -eq "ingest" })
    if ($ingestSteps.Count -ne 1) {
        throw "FAIL: migrated workspace has $($ingestSteps.Count) ingest steps (expected 1)"
    }
    if ($legacyWorkspace.workspace.active_step_id -ne "step_ingest") {
        throw "FAIL: migrated active_step_id is not step_ingest"
    }
    Write-Host "OK: legacy ingest_proxy migrated on workspace load"

    $ingestState = Test-GetJson "$Base/api/ingest/state?project_id=$([uri]::EscapeDataString($newId))" '"clips"' "GET /api/ingest/state"
    if ($ingestState.project_id -ne $newId) { throw "FAIL: ingest state project_id mismatch" }
    if (-not $ingestState.sources) { throw "FAIL: ingest sources missing" }
    if ($null -eq $ingestState.clips) { throw "FAIL: ingest clips array missing" }

    $legacyIngestDb = Join-Path $ProjectsRoot "$newId\ingest\ingest.db"
    if (Test-Path $legacyIngestDb) {
        throw "FAIL: ingest created separate db at $legacyIngestDb"
    }
    Write-Host "OK: ingest uses qnc_project.db (no separate ingest.db)"

    $discovered = Test-PostJson "$Base/api/ingest/discover" @{
        project_id = $newId
        source_id = $ingestState.active_source_id
    } '"clips"' "POST /api/ingest/discover"
    if ($discovered.project_id -ne $newId) { throw "FAIL: discover project_id mismatch" }

    $selectAll = Test-PostJson "$Base/api/ingest/selection/select-all" @{
        project_id = $newId
    } '"selected_clip_ids"' "POST /api/ingest/selection/select-all"
    if ($selectAll.project_id -ne $newId) { throw "FAIL: select-all project_id mismatch" }

    $toggled = Test-PostJson "$Base/api/ingest/selection/toggle" @{
        project_id = $newId
        clip_id = "qa-test-clip"
    } '"clips"' "POST /api/ingest/selection/toggle"
    if ($toggled.status -ne "ok") { throw "FAIL: toggle status not ok" }

    # Native-only gate: host must not serve legacy web UI assets.
    try {
        Invoke-WebRequest -Uri "$Base/app/components/registry.json" -UseBasicParsing -ErrorAction Stop | Out-Null
        throw "FAIL: /app/components/registry.json must not be served (web UI removed)"
    } catch {
        if ($_.Exception.Message -match '^FAIL:') { throw }
        $status = $null
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        if ($status -ne 404) {
            throw "FAIL: /app/components/registry.json expected 404 (got $status)"
        }
    }
    try {
        Invoke-WebRequest -Uri "$Base/plugins/ingest/static/qnc-ingest.js" -UseBasicParsing -ErrorAction Stop | Out-Null
        throw "FAIL: /plugins/ingest static must not be served (web UI removed)"
    } catch {
        if ($_.Exception.Message -match '^FAIL:') { throw }
        $status = $null
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        if ($status -ne 404) {
            throw "FAIL: /plugins/ingest static expected 404 (got $status)"
        }
    }
    $tabsNoWebAssets = Test-GetJson "$Base/api/shell/tabs" '"tabs"' "GET /api/shell/tabs (no web assets)"
    $tabsJson = $tabsNoWebAssets | ConvertTo-Json -Depth 20 -Compress
    if ($tabsJson -match '/app/' -or $tabsJson -match '/plugins/') {
        throw "FAIL: /api/shell/tabs must not advertise /app or /plugins asset paths"
    }
    Write-Host "OK: native API-only gate (no web /app|/plugins assets)"

    $beforeDelete = Test-GetJson "$Base/api/projects" '"projects"' "GET /api/projects (before delete)"
    $countBeforeDelete = @($beforeDelete.projects).Count

    $deleted = Test-PostJson "$Base/api/projects/delete" @{
        project_ids = @($newId)
    } '"projects"' "POST /api/projects/delete"
    if ($deleted.removed -notcontains $newId) { throw "FAIL: project not deleted" }
    if (@($deleted.projects).Count -ne ($countBeforeDelete - 1)) {
        throw "FAIL: delete response project count mismatch"
    }

    $afterDelete = Test-GetJson "$Base/api/projects" '"projects"' "GET /api/projects (after delete)"
    if (@($afterDelete.projects).Count -ne ($countBeforeDelete - 1)) {
        throw "FAIL: project index count after delete"
    }
    if ($afterDelete.projects | Where-Object { $_.project_id -eq $newId }) {
        throw "FAIL: deleted project still listed in index"
    }
    Write-Host "OK: Project tab delete regression"

    if ($bootstrapProjectId) {
        $deletedBootstrap = Test-PostJson "$Base/api/projects/delete" @{
            project_ids = @($bootstrapProjectId)
        } '"projects"' "POST /api/projects/delete (bootstrap)"
        if ($deletedBootstrap.removed -notcontains $bootstrapProjectId) { throw "FAIL: bootstrap project not deleted" }
    }

    Write-Host ""
    Write-Host "All host integration tests passed (core: project + ingest + Story)."
}
finally {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    if (Test-Path $ProjectsRoot) {
        Remove-Item -LiteralPath $ProjectsRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
