use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::app_state::AppState;
use crate::export_hires::{hires_export_status, submit_hires_export_job};
use crate::project::db::project_settings_snapshot_from_conn;

#[derive(serde::Deserialize)]
pub(crate) struct RenderSubmitBody {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct RenderJobQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    job_id: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/render/hires/submit", post(api_render_hires_submit))
        .route("/api/render/hires/status", get(api_render_hires_status))
}

pub(crate) async fn api_render_hires_status(
    State(app): State<AppState>,
    Query(q): Query<RenderJobQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let job_id = q.job_id.trim();
    if job_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "job_id required".into()));
    }
    let response = hires_export_status(&app.project.paths, &app.project_db, &pid, job_id)
        .map_err(map_bad_request)?;
    Ok(Json(response))
}

pub(crate) async fn api_render_hires_submit(
    State(app): State<AppState>,
    Json(body): Json<RenderSubmitBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let settings_snapshot = app
        .project_db
        .with_project_read(&pid, |conn| {
            project_settings_snapshot_from_conn(conn, &pid).map_err(|e| e.to_string())
        })
        .map_err(map_store_err)?;
    let project_settings = effective_settings_from_snapshot(&settings_snapshot);
    let response = submit_hires_export_job(
        &app.project.paths,
        &app.project_db,
        &app.media_gateway,
        &pid,
        &project_settings,
    )
    .map_err(map_bad_request)?;
    serde_json::to_value(response)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn effective_settings_from_snapshot(snapshot: &Value) -> Value {
    snapshot
        .get("settings")
        .cloned()
        .filter(|settings| settings.is_object())
        .unwrap_or_else(|| json!({}))
}

fn resolve_project_id(app: &AppState, project_id: &str) -> Result<String, (StatusCode, String)> {
    if !project_id.trim().is_empty() {
        return Ok(project_id.trim().to_string());
    }
    app.project.active_project_id()
}

fn map_store_err(e: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e)
}

fn map_bad_request(e: String) -> (StatusCode, String) {
    if is_client_error(&e) {
        (StatusCode::BAD_REQUEST, e)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

fn is_client_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("required")
        || m.contains("nije valjan")
        || m.contains("nema valjan")
        || m.contains("nema otvorenog")
        || m.contains("nije dostupan")
        || m.contains("ne postoji")
        || m.contains("prazan")
        || m.contains("nema sto exportirati")
}
