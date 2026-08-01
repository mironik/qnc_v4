use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;

use crate::app_state::AppState;

use super::store::{clamp_step, increment, load_state, reset};

#[derive(serde::Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
struct ProjectBody {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
struct IncrementBody {
    #[serde(default)]
    project_id: String,
    step: Option<u32>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sdk-demo/state", get(api_state))
        .route("/api/sdk-demo/increment", post(api_increment))
        .route("/api/sdk-demo/reset", post(api_reset))
}

async fn api_state(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let state = load_state(&app.project.paths, &pid).map_err(map_store_err)?;
    Ok(Json(state))
}

async fn api_increment(
    State(app): State<AppState>,
    Json(body): Json<IncrementBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let step = clamp_step(body.step);
    let state = increment(&app.project.paths, &pid, step).map_err(map_store_err)?;
    Ok(Json(state))
}

async fn api_reset(
    State(app): State<AppState>,
    Json(body): Json<ProjectBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = reset(&app.project.paths, &pid).map_err(map_store_err)?;
    Ok(Json(state))
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
