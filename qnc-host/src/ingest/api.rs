use std::path::Path;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::fs;

use crate::app_state::AppState;

use super::db::resolve_ingest_poster_path;
use super::orchestrator::{after_discover, after_import_queued};
use super::store::{
    discover, load_state, needs_duration_probe, queue_import, register_media_paths, save_selection,
    select_all_clips, set_active_source, set_browse_path, set_ingest_archive_original,
    toggle_clip_selection,
};
use crate::media_pool::proxy_path_for_clip;
use crate::waveform::{peaks_for_channel, ready as waveform_ready, snapshot as waveform_snapshot};

#[derive(serde::Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project_id: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ingest/state", get(api_ingest_state))
        .route("/api/ingest/source", post(api_ingest_source))
        .route("/api/ingest/selection", post(api_ingest_selection))
        .route(
            "/api/ingest/selection/toggle",
            post(api_ingest_selection_toggle),
        )
        .route(
            "/api/ingest/selection/select-all",
            post(api_ingest_selection_select_all),
        )
        .route("/api/ingest/discover", post(api_ingest_discover))
        .route("/api/ingest/import", post(api_ingest_import))
        .route("/api/ingest/browse", post(api_ingest_browse))
        .route("/api/ingest/options", post(api_ingest_options))
        .route(
            "/api/ingest/register-files",
            post(api_ingest_register_files),
        )
        .route(
            "/api/ingest/thumbs/copy-card",
            post(api_ingest_thumbs_copy_card),
        )
        .route(
            "/api/ingest/thumbs/from-proxy",
            post(api_ingest_thumbs_from_proxy),
        )
        .route("/api/ingest/thumbnail", get(api_ingest_thumbnail))
        .route(
            "/api/ingest/waveform/status",
            get(api_ingest_waveform_status),
        )
        .route("/api/ingest/waveform/peaks", get(api_ingest_waveform_peaks))
}

async fn api_ingest_state(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    // Prvo snapshotr (posteri), tek onda enqueue — inače GET čeka probe.
    let needs = needs_duration_probe(&app.project.paths, &pid).unwrap_or(false);
    let state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if needs {
        app.ingest_durations.enqueue(&pid);
    }
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestSourceBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    source_id: String,
}

async fn api_ingest_source(
    State(app): State<AppState>,
    Json(body): Json<IngestSourceBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    set_active_source(&app.project.paths, &pid, &body.source_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    after_discover(&app, &pid);
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestSelectionBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    selected_clip_ids: Vec<String>,
}

async fn api_ingest_selection(
    State(app): State<AppState>,
    Json(body): Json<IngestSelectionBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    save_selection(&app.project.paths, &pid, &body.selected_clip_ids)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(serde::Deserialize)]
struct IngestToggleBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
}

async fn api_ingest_selection_toggle(
    State(app): State<AppState>,
    Json(body): Json<IngestToggleBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    toggle_clip_selection(&app.project.paths, &pid, &body.clip_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(serde::Deserialize)]
struct IngestSelectAllBody {
    #[serde(default)]
    project_id: String,
}

async fn api_ingest_selection_select_all(
    State(app): State<AppState>,
    Json(body): Json<IngestSelectAllBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    select_all_clips(&app.project.paths, &pid)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(serde::Deserialize)]
struct IngestDiscoverBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    source_id: String,
}

async fn api_ingest_discover(
    State(app): State<AppState>,
    Json(body): Json<IngestDiscoverBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let paths = app.project.paths.clone();
    let project_id = pid.clone();
    let source_id = body.source_id.clone();
    tokio::task::spawn_blocking(move || discover(&paths, &project_id, &source_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Grid prvo (klipovi u snapshotr); thumb copy + duration u pozadini.
    let state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    after_discover(&app, &pid);
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestBrowseBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    path: String,
}

async fn api_ingest_browse(
    State(app): State<AppState>,
    Json(body): Json<IngestBrowseBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let path = body.path.trim();
    if path.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path je prazan.".into()));
    }
    set_browse_path(&app.project.paths, &pid, path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let source_id = {
        let conn = crate::ingest::db::open_ingest(&app.project.paths, &pid)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::ingest::db::get_meta(&conn, "active_source_id", "local")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };
    let paths = app.project.paths.clone();
    let project_id = pid.clone();
    let sid = source_id.clone();
    tokio::task::spawn_blocking(move || discover(&paths, &project_id, &sid))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    after_discover(&app, &pid);
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestOptionsBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    archive_original: bool,
}

async fn api_ingest_options(
    State(app): State<AppState>,
    Json(body): Json<IngestOptionsBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    set_ingest_archive_original(&app.project.paths, &pid, body.archive_original)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(serde::Deserialize)]
struct IngestRegisterFilesBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    paths: Vec<String>,
}

async fn api_ingest_register_files(
    State(app): State<AppState>,
    Json(body): Json<IngestRegisterFilesBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let file_paths: Vec<std::path::PathBuf> = body
        .paths
        .iter()
        .map(|p| std::path::PathBuf::from(p.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    register_media_paths(&app.project.paths, &pid, &body.source_id, &file_paths, &[])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Novi fajlovi — dopusti ponovni duration probe (reset "done").
    if let Ok(conn) = crate::ingest::db::open_ingest(&app.project.paths, &pid) {
        let _ = crate::ingest::db::set_meta(&conn, "durations_probe", "");
    }
    let state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    after_discover(&app, &pid);
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestImportBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_ids: Vec<String>,
}

async fn api_ingest_import(
    State(app): State<AppState>,
    Json(body): Json<IngestImportBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let queued = queue_import(&app.project.paths, &pid, &body.clip_ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    after_import_queued(&app, &pid);
    let mut state = load_state(&app.project.paths, &pid)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(obj) = state.as_object_mut() {
        obj.insert("import_queued".into(), serde_json::json!(true));
        if let Some(n) = queued.get("queued") {
            obj.insert("queued".into(), n.clone());
        }
        if let Some(n) = queued.get("skipped_imported") {
            obj.insert("skipped_imported".into(), n.clone());
        }
    }
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct IngestThumbsBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_ids: Vec<String>,
}

/// Proces 1: THM → JPG na kartici (kopija). Vraća clip_id bez slike na kartici.
async fn api_ingest_thumbs_copy_card(
    State(app): State<AppState>,
    Json(body): Json<IngestThumbsBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    app.ingest_card_thumbs.enqueue(&pid);
    Ok(Json(json!({
        "status": "ok",
        "phase": "copy_card",
        "queued": true,
    })))
}

/// Proces 2: generiraj poster iz proxya (orchestrator šalje nakon copy-card).
async fn api_ingest_thumbs_from_proxy(
    State(app): State<AppState>,
    Json(body): Json<IngestThumbsBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    app.ingest_posters
        .enqueue_proxy_generate(&pid, &body.clip_ids);
    Ok(Json(json!({
        "status": "ok",
        "phase": "from_proxy",
        "queued_clip_ids": body.clip_ids,
    })))
}

#[derive(serde::Deserialize)]
struct ThumbnailQuery {
    project_id: String,
    clip_id: String,
}

async fn api_ingest_thumbnail(
    State(app): State<AppState>,
    Query(q): Query<ThumbnailQuery>,
) -> Result<Response, StatusCode> {
    let path = resolve_ingest_poster_path(&app.project.paths, &q.project_id, &q.clip_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    serve_file(&path).await
}

#[derive(serde::Deserialize)]
struct WaveformStatusQuery {
    project_id: String,
    clip_id: String,
}

async fn api_ingest_waveform_status(
    State(app): State<AppState>,
    Query(q): Query<WaveformStatusQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = q.clip_id.trim();
    if clip_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "clip_id je prazan".into()));
    }
    if !waveform_ready(&app.project.paths, &pid, clip_id) {
        if let Some(path) = proxy_path_for_clip(&app.project.paths, &pid, clip_id) {
            app.waveform.enqueue(&pid, clip_id, &path);
        }
    }
    Ok(Json(json!({
        "project_id": pid,
        "clip_id": clip_id,
        "waveform": waveform_snapshot(&app.project.paths, &pid, clip_id),
    })))
}

#[derive(serde::Deserialize)]
struct WaveformPeaksQuery {
    project_id: String,
    clip_id: String,
    #[serde(default = "default_waveform_channel")]
    channel: u8,
}

fn default_waveform_channel() -> u8 {
    1
}

async fn api_ingest_waveform_peaks(
    State(app): State<AppState>,
    Query(q): Query<WaveformPeaksQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = q.clip_id.trim();
    if clip_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "clip_id je prazan".into()));
    }
    let channel = if q.channel == 2 { 2 } else { 1 };
    let peaks = peaks_for_channel(&app.project.paths, &pid, clip_id, channel)
        .ok_or((StatusCode::NOT_FOUND, "waveform nije spreman".into()))?;
    Ok(Json(json!({
        "project_id": pid,
        "clip_id": clip_id,
        "channel": channel,
        "peak_count": peaks.len(),
        "peaks": peaks,
    })))
}

async fn serve_file(path: &Path) -> Result<Response, StatusCode> {
    let bytes = fs::read(path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let ct = mime_for_path(path);
    Ok(([(header::CONTENT_TYPE, ct)], bytes).into_response())
}

fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

fn resolve_project_id(app: &AppState, project_id: &str) -> Result<String, (StatusCode, String)> {
    if !project_id.trim().is_empty() {
        return Ok(project_id.trim().to_string());
    }
    app.project.active_project_id()
}
