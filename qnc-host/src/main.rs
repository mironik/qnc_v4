use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tracing::{info, warn};

mod app_state;
mod components;
mod config;
mod db_first;
mod design;
mod design_db;
mod editor_assets;
mod filmstrip;
mod frame_time;
mod hardware_profile;
mod ingest;
mod ingest_audio_wrap;
mod ingest_card_thumbs;
mod ingest_durations;
mod ingest_import;
mod ingest_posters;
mod ingest_proxy;
mod media;
mod media_pool;
mod modules;
mod platform;
mod project;
mod routes;
mod shell_dialog;
mod shell_fs;
mod shell_store;
mod story;
mod tabs;
mod timeline_model;
mod virtual_shots;
mod waveform;
mod workspace_paths;

use app_state::AppState;

use config::AppConfig;
use modules::ModuleStore;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "qnc_host=info,tower_http=warn".into()),
        )
        .init();

    let root = detect_root();
    let config = AppConfig::load(&root);
    if let Err(msg) = config::require_trusted_lan_for_bind(&config.bind_host) {
        eprintln!("qnc-host: {msg}");
        std::process::exit(2);
    }
    let hw = hardware_profile::ensure(&root);
    if !hw.ingest_stable {
        for w in &hw.warnings {
            warn!("hardware: {w}");
        }
    }
    let modules = Arc::new(RwLock::new(ModuleStore::load(&root.join("data"))));
    let project_state = project::ProjectState::new(&root, &config);
    let ingest_posters = Arc::new(ingest_posters::PosterWorker::new(
        project_state.paths.clone(),
    ));
    let filmstrip = Arc::new(filmstrip::FilmstripWorker::new(project_state.paths.clone()));
    let waveform = Arc::new(waveform::WaveformWorker::new(project_state.paths.clone()));
    let ingest_audio_wrap = Arc::new(ingest_audio_wrap::AudioWrapWorker::new(
        project_state.paths.clone(),
    ));
    let ingest_proxy = Arc::new(ingest_proxy::ProxyGenerateWorker::new(
        project_state.paths.clone(),
        filmstrip.clone(),
        ingest_posters.clone(),
    ));
    let ingest_import = Arc::new(ingest_import::ImportWorker::new(
        project_state.paths.clone(),
        ingest_proxy.clone(),
        filmstrip.clone(),
        ingest_posters.clone(),
        ingest_audio_wrap.clone(),
    ));
    let ingest_card_thumbs = Arc::new(ingest_card_thumbs::CardThumbWorker::new(
        project_state.paths.clone(),
        ingest_posters.clone(),
    ));
    let ingest_durations = Arc::new(ingest_durations::DurationWorker::new(
        project_state.paths.clone(),
    ));
    match ingest_import.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest import recovery queued projects={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest import recovery scan failed: {e}"),
    }
    match ingest_proxy.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest proxy recovery queued clips={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest proxy recovery scan failed: {e}"),
    }
    match ingest_card_thumbs.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest card thumbs recovery queued projects={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest card thumbs recovery scan failed: {e}"),
    }
    match ingest_durations.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest durations recovery queued projects={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest durations recovery scan failed: {e}"),
    }
    match ingest_posters.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest poster recovery queued projects={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest poster recovery scan failed: {e}"),
    }
    match filmstrip.enqueue_recoverable_projects(filmstrip::DEFAULT_FILMSTRIP_FRAMES) {
        Ok(count) if count > 0 => info!("filmstrip recovery queued clips={count}"),
        Ok(_) => {}
        Err(e) => warn!("filmstrip recovery scan failed: {e}"),
    }
    match waveform.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("waveform recovery queued clips={count}"),
        Ok(_) => {}
        Err(e) => warn!("waveform recovery scan failed: {e}"),
    }
    match ingest_audio_wrap.enqueue_recoverable_projects() {
        Ok(count) if count > 0 => info!("ingest audio wrap recovery queued projects={count}"),
        Ok(_) => {}
        Err(e) => warn!("ingest audio wrap recovery scan failed: {e}"),
    }
    ingest_posters.clone().spawn();
    ingest_proxy.clone().spawn();
    ingest_card_thumbs.clone().spawn();
    ingest_durations.clone().spawn();
    filmstrip.clone().spawn();
    waveform.clone().spawn();
    ingest_import.clone().spawn();
    ingest_audio_wrap.clone().spawn();

    // Metadata repair (fps/duration) runs once at boot, off the request path.
    {
        let maintenance_paths = project_state.paths.clone();
        tokio::task::spawn_blocking(move || {
            waveform::maintenance_purge_legacy(&maintenance_paths);
            media_pool::backfill_all_imported_metadata(&maintenance_paths);
        });
    }

    let state = AppState {
        root: root.clone(),
        config: config.clone(),
        modules,
        project: project_state,
        ingest_card_thumbs,
        ingest_durations,
        ingest_posters,
        ingest_proxy,
        ingest_import,
        ingest_audio_wrap,
        filmstrip,
        waveform,
        story_playback: story::PlaybackStore::default(),
    };

    let app = Router::new()
        .route("/api/health", get(api_health))
        .route("/api/shell/runtime", get(api_shell_runtime))
        .route("/api/shell/diagnostics", get(api_shell_diagnostics))
        .route("/api/shell/db-first", get(api_shell_db_first))
        .route("/api/shell/tabs", get(api_shell_tabs))
        .route("/api/shell/components", get(api_shell_components))
        .route(
            "/api/shell/components/sync",
            post(api_shell_components_sync),
        )
        .route("/api/shell/pick-directory", post(api_shell_pick_directory))
        .route("/api/shell/pick-files", post(api_shell_pick_files))
        .route("/api/shell/fs-roots", get(api_shell_fs_roots))
        .route("/api/shell/fs-list", get(api_shell_fs_list))
        .route(
            "/api/shell/projects-root",
            get(api_shell_projects_root).post(api_shell_projects_root_save),
        )
        .route(
            "/api/shell/keyboard-shortcuts",
            get(api_shell_keyboard_shortcuts),
        )
        .route("/api/modules", get(api_modules_list))
        .route("/api/modules/{module_id}/enable", post(api_module_enable))
        .route("/", get(api_root))
        .route("/app", get(api_root))
        .route("/gui", get(api_root))
        .merge(project::router())
        .merge(ingest::router())
        .merge(story::router())
        .merge(routes::design_tools::router())
        // Compat: legacy media-pool placeholder URLs used by older filmstrip paint.
        .route(
            "/api/media-pool/filmstrip/placeholder",
            get(crate::editor_assets::api_filmstrip_placeholder_public),
        )
        .with_state(state);

    let port = config.api_port;
    let bind_ip: std::net::IpAddr = config
        .bind_host
        .parse()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let addr = SocketAddr::from((bind_ip, port));
    let app_host = config::app_url_host(&config.bind_host);
    info!("QNC host root: {}", root.display());
    info!("Binding to {bind_ip}:{port}");
    info!("API URL: http://{app_host}:{port}/api/health");
    info!("Native UI: qnc-app (web /app removed)");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

fn detect_root() -> PathBuf {
    if let Ok(raw) = std::env::var("QNC_ROOT") {
        let p = PathBuf::from(raw);
        if workspace_paths::looks_like_root(&p) {
            return p.canonicalize().unwrap_or(p);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if workspace_paths::looks_like_root(&cwd) {
        return cwd.canonicalize().unwrap_or(cwd);
    }
    let parent = cwd.join("..");
    if workspace_paths::looks_like_root(&parent) {
        return parent.canonicalize().unwrap_or(parent);
    }
    cwd
}

async fn api_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "ffmpeg": ingest::thumb::ffmpeg_available(),
        "proxy_encoder": ingest::proxy_generate::active_proxy_encoder_label(),
        "proxy_recipes": ingest::proxy_generate::proxy_recipe_policy_snapshot(),
        "ingest_stable": hardware_profile::get().map(|p| p.ingest_stable),
    }))
}

async fn api_shell_runtime(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(platform::runtime_info(&state.root, &state.config))
}

async fn api_shell_diagnostics(State(state): State<AppState>) -> Json<serde_json::Value> {
    let plugins_root = workspace_paths::tabs_dir(&state.root);
    let scan = tabs::scan_plugin_manifests(&plugins_root);
    let plugins_loaded: Vec<String> = scan
        .manifests
        .iter()
        .filter_map(|m| {
            m.get("plugin_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    let components_catalog = components::list_global(&workspace_paths::components_dir(&state.root));
    let components_count = components_catalog
        .get("components")
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0);

    Json(serde_json::json!({
        "status": "ok",
        "bind_host": state.config.bind_host,
        "api_port": state.config.api_port,
        "api_url": format!(
            "http://{}:{}/api/health",
            config::app_url_host(&state.config.bind_host),
            state.config.api_port
        ),
        "ui": "qnc-app",
        "data_dir": state.root.join("data").to_string_lossy(),
        "projects_root": config::configured_projects_root(&state.config).to_string_lossy(),
        "plugins_loaded": plugins_loaded,
        "plugins_loaded_count": plugins_loaded.len(),
        "plugin_manifest_errors": scan.errors,
        "components_count": components_count,
    }))
}

async fn api_shell_db_first(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(db_first::diagnostics(&state.root, &state.project.paths))
}

async fn api_shell_projects_root(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "projects_root": state.project.paths.projects_root.to_string_lossy(),
        "configured_projects_root": state.config.projects_root,
    }))
}

#[derive(serde::Deserialize)]
struct ProjectsRootBody {
    projects_root: String,
}

async fn api_shell_projects_root_save(
    State(state): State<AppState>,
    Json(body): Json<ProjectsRootBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = body.projects_root.trim();
    if path.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "projects_root je prazan.".into()));
    }
    config::save_projects_root(&state.root, path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "projects_root": path,
        "requires_restart": true,
        "message": "Lokacija projekata spremljena. Ponovno pokreni QNC host da postane aktivna.",
    })))
}

async fn api_shell_tabs(State(state): State<AppState>) -> Json<serde_json::Value> {
    let manifests = tabs::list_tab_manifests(&workspace_paths::tabs_dir(&state.root));
    let store = state.modules.read().expect("module lock");
    let enabled = store.apply_enabled(&state.root, manifests);
    Json(serde_json::json!({ "status": "ok", "tabs": enabled }))
}

async fn api_shell_components(State(state): State<AppState>) -> Json<serde_json::Value> {
    let catalog = components::list_global(&workspace_paths::components_dir(&state.root));
    let mut out = catalog;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("status".into(), serde_json::json!("ok"));
    }
    Json(out)
}

async fn api_shell_keyboard_shortcuts(State(state): State<AppState>) -> impl IntoResponse {
    let path = workspace_paths::keyboard_shortcuts(&state.root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => (StatusCode::OK, [("content-type", "application/json")], raw).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "status": "error",
                "message": "keyboard-shortcuts seed missing",
            })),
        )
            .into_response(),
    }
}

async fn api_shell_components_sync() -> Json<serde_json::Value> {
    // MVP: portable sync ostaje no-op; registry se čita s diska.
    Json(serde_json::json!({ "status": "ok", "installed": [] }))
}

#[derive(serde::Deserialize)]
struct FsListQuery {
    #[serde(default)]
    path: String,
}

async fn api_shell_fs_roots() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let entries = tokio::task::spawn_blocking(shell_fs::list_roots)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "roots": true,
        "path": "",
        "parent": null,
        "entries": entries,
    })))
}

async fn api_shell_fs_list(
    Query(q): Query<FsListQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = q.path;
    let listed = tokio::task::spawn_blocking(move || shell_fs::list_directory(&path))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "roots": listed.roots,
        "path": listed.path,
        "parent": listed.parent,
        "entries": listed.entries,
    })))
}

#[derive(serde::Deserialize)]
struct PickDirectoryBody {
    initial_dir: Option<String>,
}

async fn api_shell_pick_directory(
    Json(body): Json<PickDirectoryBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let initial = body.initial_dir.unwrap_or_default().trim().to_string();
    let picked = tokio::task::spawn_blocking(move || {
        let start = std::path::PathBuf::from(&initial);
        shell_dialog::pick_directory(&start)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match picked {
        Some(path) => Ok(Json(serde_json::json!({
            "status": "ok",
            "path": path.to_string_lossy()
        }))),
        None => Err((StatusCode::CONFLICT, "cancelled".into())),
    }
}

#[derive(serde::Deserialize)]
struct PickFilesBody {
    initial_dir: Option<String>,
}

async fn api_shell_pick_files(
    Json(body): Json<PickFilesBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let initial = body.initial_dir.unwrap_or_default().trim().to_string();
    let picked = tokio::task::spawn_blocking(move || {
        let start = std::path::PathBuf::from(&initial);
        shell_dialog::pick_media_files(&start)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match picked {
        Some(paths) => {
            let out: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            Ok(Json(serde_json::json!({
                "status": "ok",
                "paths": out,
            })))
        }
        None => Err((StatusCode::CONFLICT, "cancelled".into())),
    }
}

async fn api_modules_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let manifests = tabs::list_tab_manifests(&workspace_paths::tabs_dir(&state.root));
    let store = state.modules.read().expect("module lock");
    let modules = store.as_module_list(manifests);
    Json(serde_json::json!({ "status": "ok", "modules": modules }))
}

#[derive(serde::Deserialize)]
struct EnableBody {
    enabled: bool,
}

async fn api_module_enable(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
    Json(body): Json<EnableBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let manifests = tabs::list_tab_manifests(&workspace_paths::tabs_dir(&state.root));
    let mut store = state.modules.write().expect("module lock");
    match store.set_enabled(
        &state.root.join("data"),
        &manifests,
        &module_id,
        body.enabled,
    ) {
        Ok(module) => Ok(Json(
            serde_json::json!({ "status": "ok", "module": module }),
        )),
        Err(modules::ModuleError::NotFound) => Err((
            StatusCode::NOT_FOUND,
            format!("Modul '{module_id}' ne postoji."),
        )),
        Err(modules::ModuleError::NotRemovable) => Err((
            StatusCode::FORBIDDEN,
            format!("Modul '{module_id}' je sistemski i ne moze se iskljuciti."),
        )),
    }
}

async fn api_root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "product": "qnc-host",
        "ui": "qnc-app",
        "message": "Web UI removed. Use native qnc-app against this API.",
        "health": "/api/health",
    }))
}
