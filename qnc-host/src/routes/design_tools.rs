use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;

use crate::app_state::AppState;
use crate::design;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/design-tools/status", get(api_design_status))
        .route("/api/design-tools/tokens", get(api_design_tokens))
        .route(
            "/api/design-tools/themes",
            get(api_design_themes).post(api_design_create_theme),
        )
        .route(
            "/api/design-tools/themes/{theme_id}/activate",
            post(api_design_activate_theme),
        )
        .route(
            "/api/design-tools/overrides/tokens",
            post(api_design_save_tokens),
        )
        .route(
            "/api/design-tools/timeline-lab",
            get(api_design_timeline_lab).post(api_design_save_timeline_lab),
        )
        .route(
            "/api/design-tools/project-list-lab",
            get(api_design_project_list_lab).post(api_design_save_project_list_lab),
        )
        .route(
            "/api/design-tools/project-template-settings-lab",
            get(api_design_project_template_settings_lab)
                .post(api_design_save_project_template_settings_lab),
        )
        .route(
            "/api/design-tools/ingest-clip-grid-lab",
            get(api_design_ingest_clip_grid_lab).post(api_design_save_ingest_clip_grid_lab),
        )
}

async fn api_design_status(State(state): State<AppState>) -> Json<Value> {
    Json(design::design_status(&state.root, &state.config))
}

async fn api_design_tokens(State(state): State<AppState>) -> Json<Value> {
    Json(design::merged_tokens(&state.root, &state.config))
}

async fn api_design_themes(State(state): State<AppState>) -> Json<Value> {
    Json(design::list_themes(&state.root, &state.config))
}

#[derive(serde::Deserialize)]
struct CreateThemeBody {
    label: String,
}

async fn api_design_create_theme(
    State(state): State<AppState>,
    Json(body): Json<CreateThemeBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::create_theme(&state.root, &state.config, &body.label) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::BAD_REQUEST, msg)),
    }
}

async fn api_design_activate_theme(
    State(state): State<AppState>,
    Path(theme_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::activate_theme(&state.root, &state.config, &theme_id) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::NOT_FOUND, msg)),
    }
}

#[derive(serde::Deserialize)]
struct TokenOverridesBody {
    tokens: HashMap<String, String>,
}

async fn api_design_save_tokens(
    State(state): State<AppState>,
    Json(body): Json<TokenOverridesBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::save_token_overrides(&state.root, &state.config, &body.tokens) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::FORBIDDEN, msg)),
    }
}

async fn api_design_timeline_lab(State(state): State<AppState>) -> Json<Value> {
    Json(design::load_timeline_lab_prefs(&state.root, &state.config))
}

#[derive(serde::Deserialize)]
struct TimelineLabBody {
    prefs: Value,
}

async fn api_design_save_timeline_lab(
    State(state): State<AppState>,
    Json(body): Json<TimelineLabBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::save_timeline_lab_prefs(&state.root, &state.config, body.prefs) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::FORBIDDEN, msg)),
    }
}

async fn api_design_project_list_lab(State(state): State<AppState>) -> Json<Value> {
    Json(design::load_project_list_lab_prefs(
        &state.root,
        &state.config,
    ))
}

#[derive(serde::Deserialize)]
struct ProjectListLabBody {
    prefs: Value,
}

async fn api_design_save_project_list_lab(
    State(state): State<AppState>,
    Json(body): Json<ProjectListLabBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::save_project_list_lab_prefs(&state.root, &state.config, body.prefs) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::FORBIDDEN, msg)),
    }
}

async fn api_design_project_template_settings_lab(State(state): State<AppState>) -> Json<Value> {
    Json(design::load_project_template_settings_lab_prefs(
        &state.root,
        &state.config,
    ))
}

#[derive(serde::Deserialize)]
struct ProjectTemplateSettingsLabBody {
    prefs: Value,
}

async fn api_design_save_project_template_settings_lab(
    State(state): State<AppState>,
    Json(body): Json<ProjectTemplateSettingsLabBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::save_project_template_settings_lab_prefs(&state.root, &state.config, body.prefs) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::FORBIDDEN, msg)),
    }
}

async fn api_design_ingest_clip_grid_lab(State(state): State<AppState>) -> Json<Value> {
    Json(design::load_ingest_clip_grid_lab_prefs(
        &state.root,
        &state.config,
    ))
}

#[derive(serde::Deserialize)]
struct IngestClipGridLabBody {
    prefs: Value,
}

async fn api_design_save_ingest_clip_grid_lab(
    State(state): State<AppState>,
    Json(body): Json<IngestClipGridLabBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match design::save_ingest_clip_grid_lab_prefs(&state.root, &state.config, body.prefs) {
        Ok(out) => Ok(Json(out)),
        Err(msg) => Err((StatusCode::FORBIDDEN, msg)),
    }
}
