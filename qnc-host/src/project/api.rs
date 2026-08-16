use std::sync::Mutex;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::app_state::AppState;

use super::appearance_settings::{load_appearance_user, save_appearance_user};
use super::collab::{start_session, touch_session};
use super::db::{open_global, project_data_revision_snapshot, ProjectPaths};
use super::keyboard_settings::{list_keyboard_presets, load_keyboard_user, save_keyboard_user};
use super::store::{
    cleanup_orphan_project_dirs, create_project, delete_projects, get_active_project_id,
    list_projects, open_project, orphan_project_dir_names,
};
use super::templates::{
    create_project_from_template, create_user_template, delete_user_template,
    ensure_templates_seeded, get_project_settings, get_project_template, get_project_workspace,
    list_project_templates, list_source_templates, prepare_project_workspace,
    save_project_settings,
};
use super::ui_state::{
    effective_template_settings, get_ui_state, get_ui_state_for_api, save_ui_state,
    touch_collab_session,
};

#[derive(Clone)]
pub struct ProjectState {
    pub paths: ProjectPaths,
    db: std::sync::Arc<Mutex<rusqlite::Connection>>,
}

impl ProjectState {
    pub fn new(root: &std::path::Path, config: &crate::config::AppConfig) -> Self {
        let paths = ProjectPaths::from_root(root, config);
        let conn = open_global(&paths).expect("project_store.db");
        ensure_templates_seeded(&conn, &paths.seed_path).ok();
        Self {
            paths,
            db: std::sync::Arc::new(Mutex::new(conn)),
        }
    }

    fn with_db<F, T>(&self, f: F) -> Result<T, (StatusCode, String)>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, String>,
    {
        let guard = self
            .db
            .lock()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        f(&guard).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
    }

    pub fn active_project_id(&self) -> Result<String, (StatusCode, String)> {
        self.with_db(|conn| {
            let id = get_active_project_id(conn).map_err(|e| e.to_string())?;
            if id.is_empty() {
                Err("Nema aktivnog projekta.".into())
            } else {
                Ok(id)
            }
        })
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/projects/ui-state",
            get(api_projects_ui_state_get).post(api_projects_ui_state_save),
        )
        .route(
            "/api/projects",
            get(api_projects_list).post(api_projects_create),
        )
        .route("/api/projects/open", post(api_projects_open))
        .route("/api/projects/delete", post(api_projects_delete))
        .route(
            "/api/projects/cleanup-orphans",
            post(api_projects_cleanup_orphans),
        )
        .route(
            "/api/projects/from-template",
            post(api_projects_from_template),
        )
        .route(
            "/api/projects/{project_id}/settings",
            get(api_project_settings_get).post(api_project_settings_save),
        )
        .route(
            "/api/projects/{project_id}/workspace",
            get(api_project_workspace_get),
        )
        .route(
            "/api/projects/{project_id}/data-revision",
            get(api_project_data_revision_get),
        )
        .route(
            "/api/project-templates",
            get(api_project_templates_list).post(api_project_template_create),
        )
        .route(
            "/api/project-templates/delete",
            post(api_project_template_delete),
        )
        .route(
            "/api/project-templates/{template_id}",
            get(api_project_template_get),
        )
        .route("/api/collab/session", post(api_collab_session))
        .route("/api/collab/touch", post(api_collab_touch))
        .route(
            "/api/settings/keyboard-shortcuts",
            get(api_keyboard_shortcuts_get).post(api_keyboard_shortcuts_save),
        )
        .route(
            "/api/settings/keyboard-shortcuts/presets",
            get(api_keyboard_presets_list),
        )
        .route(
            "/api/settings/appearance",
            get(api_appearance_get).post(api_appearance_save),
        )
}

async fn api_projects_ui_state_get(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let ui_state = get_ui_state_for_api(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "ui_state": ui_state })))
    })
}

async fn api_projects_ui_state_save(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let ui_state = save_ui_state(conn, &body).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "ui_state": ui_state })))
    })
}

async fn api_keyboard_shortcuts_get(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let user = load_keyboard_user(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "user": user })))
    })
}

async fn api_keyboard_shortcuts_save(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let user = body.get("user").cloned().unwrap_or(body);
        let saved = save_keyboard_user(conn, &user).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "user": saved })))
    })
}

async fn api_keyboard_presets_list(State(app): State<AppState>) -> Json<Value> {
    let root = app
        .project
        .paths
        .data_dir
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let catalog = list_keyboard_presets(root);
    Json(json!({
        "status": "ok",
        "presets": catalog.get("presets").cloned().unwrap_or_else(|| json!([])),
    }))
}

async fn api_appearance_get(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let user = load_appearance_user(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "user": user })))
    })
}

async fn api_appearance_save(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let user = body.get("user").cloned().unwrap_or(body);
        let saved = save_appearance_user(conn, &user).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "user": saved })))
    })
}

async fn api_projects_list(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let projects = list_projects(conn).map_err(|e| e.to_string())?;
        let active = get_active_project_id(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "active_project_id": active,
            "projects": projects,
        })))
    })
}

#[derive(serde::Deserialize)]
struct ProjectCreateBody {
    name: Option<String>,
}

async fn api_projects_create(
    State(app): State<AppState>,
    Json(body): Json<ProjectCreateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let entry = create_project(conn, &app.project.paths, body.name.as_deref())
            .map_err(|e| e.to_string())?;
        let project_id = entry
            .get("project_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !project_id.is_empty() {
            prepare_project_workspace(conn, &app.project.paths, project_id)
                .map_err(|e| e.to_string())?;
        }
        let active = get_active_project_id(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "project": entry,
            "active_project_id": active,
        })))
    })
}

async fn api_project_templates_list(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        ensure_templates_seeded(conn, &app.project.paths.seed_path).map_err(|e| e.to_string())?;
        let templates = list_project_templates(conn).map_err(|e| e.to_string())?;
        let source_templates = list_source_templates(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "templates": templates,
            "source_templates": source_templates,
        })))
    })
}

async fn api_project_template_get(
    State(app): State<AppState>,
    Path(template_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match app.project.with_db(|conn| {
        ensure_templates_seeded(conn, &app.project.paths.seed_path).map_err(|e| e.to_string())?;
        let template = get_project_template(conn, &template_id).map_err(|e| e.to_string())?;
        match template {
            Some(t) => Ok(Json(json!({ "status": "ok", "template": t }))),
            None => Err(format!("Template '{template_id}' ne postoji.")),
        }
    }) {
        Ok(v) => Ok(v),
        Err((_, msg)) if msg.contains("ne postoji") => Err((StatusCode::NOT_FOUND, msg)),
        Err(e) => Err(e),
    }
}

#[derive(serde::Deserialize)]
struct ProjectTemplateCreateBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    settings: Value,
    #[serde(default)]
    source_template_ids: Vec<Value>,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    base_template_id: String,
}

async fn api_project_template_create(
    State(app): State<AppState>,
    Json(body): Json<ProjectTemplateCreateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        ensure_templates_seeded(conn, &app.project.paths.seed_path).map_err(|e| e.to_string())?;
        let settings = if body.settings.is_null() {
            let ui = get_ui_state(conn).map_err(|e| e.to_string())?;
            Some(effective_template_settings(conn, &ui).map_err(|e| e.to_string())?)
        } else {
            Some(body.settings.clone())
        };
        let source_val = Value::Array(body.source_template_ids.clone());
        let sources = if body.source_template_ids.is_empty() {
            None
        } else {
            Some(&source_val)
        };
        let template = create_user_template(
            conn,
            &body.name,
            &body.description,
            settings.as_ref(),
            sources,
            &body.user_id,
            &body.base_template_id,
        )
        .map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "template": template })))
    })
}

#[derive(serde::Deserialize)]
struct ProjectTemplateDeleteBody {
    #[serde(default)]
    template_id: String,
}

async fn api_project_template_delete(
    State(app): State<AppState>,
    Json(body): Json<ProjectTemplateDeleteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match app.project.with_db(|conn| {
        ensure_templates_seeded(conn, &app.project.paths.seed_path).map_err(|e| e.to_string())?;
        let deleted = delete_user_template(conn, &body.template_id)?;
        let ui = get_ui_state(conn).map_err(|e| e.to_string())?;
        let selected = ui
            .get("selected_template_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let ui_state = if selected == deleted {
            save_ui_state(
                conn,
                &json!({
                    "selected_template_id": "tpl_breaking_news",
                    "reset_settings_override": true,
                    "template_create_open": false,
                }),
            )
            .map_err(|e| e.to_string())?
        } else {
            get_ui_state_for_api(conn).map_err(|e| e.to_string())?
        };
        let templates = list_project_templates(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "deleted_template_id": deleted,
            "templates": templates,
            "ui_state": ui_state,
        })))
    }) {
        Ok(v) => Ok(v),
        Err((_, msg)) if msg.contains("ne postoji") => Err((StatusCode::NOT_FOUND, msg)),
        Err((_, msg)) if msg.contains("ne može obrisati") || msg.contains("Nedostaje") => {
            Err((StatusCode::BAD_REQUEST, msg))
        }
        Err(e) => Err(e),
    }
}

#[derive(serde::Deserialize)]
struct ProjectFromTemplateBody {
    name: String,
    template_id: String,
    #[serde(default)]
    settings_override: Value,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    session_id: String,
}

async fn api_projects_from_template(
    State(app): State<AppState>,
    Json(body): Json<ProjectFromTemplateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match app.project.with_db(|conn| {
        ensure_templates_seeded(conn, &app.project.paths.seed_path).map_err(|e| e.to_string())?;
        let override_val = if body.settings_override.is_null()
            || body
                .settings_override
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(true)
        {
            let ui = get_ui_state(conn).map_err(|e| e.to_string())?;
            ui.get("settings_override")
                .filter(|v| v.is_object() && !v.as_object().unwrap().is_empty())
                .cloned()
        } else {
            Some(body.settings_override.clone())
        };
        let override_ref = override_val.as_ref();
        let result = create_project_from_template(
            conn,
            &app.project.paths,
            &body.name,
            &body.template_id,
            override_ref,
            &body.user_id,
        )
        .map_err(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                format!("Template '{}' ne postoji.", body.template_id)
            } else {
                e.to_string()
            }
        })?;
        if !body.session_id.is_empty() {
            touch_collab_session(conn, &body.session_id).map_err(|e| e.to_string())?;
        }
        let active = get_active_project_id(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "project": result.get("project"),
            "settings": result.get("settings"),
            "active_project_id": active,
        })))
    }) {
        Ok(v) => Ok(v),
        Err((_, msg)) if msg.contains("ne postoji") => Err((StatusCode::NOT_FOUND, msg)),
        Err(e) => Err(e),
    }
}

async fn api_project_settings_get(
    State(app): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let settings = get_project_settings(&app.project.paths, &project_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "status": "ok",
        "project_id": project_id,
        "settings": settings,
    })))
}

async fn api_project_workspace_get(
    State(app): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let workspace = app.project.with_db(|conn| {
        get_project_workspace(conn, &app.project.paths, &project_id).map_err(|e| e.to_string())
    })?;
    Ok(Json(json!({ "status": "ok", "workspace": workspace })))
}

async fn api_project_data_revision_get(
    State(app): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let revision = project_data_revision_snapshot(&app.project.paths, &project_id)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "status": "ok", "revision": revision })))
}

#[derive(serde::Deserialize)]
struct ProjectSettingsSaveBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    settings: Value,
    #[serde(default)]
    template_id: String,
    #[serde(default)]
    user_id: String,
}

async fn api_project_settings_save(
    State(app): State<AppState>,
    Path(project_id): Path<String>,
    Json(body): Json<ProjectSettingsSaveBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = if body.project_id.trim().is_empty() {
        project_id.clone()
    } else {
        body.project_id.clone()
    };
    let settings = save_project_settings(
        &app.project.paths,
        &pid,
        &body.settings,
        &body.template_id,
        &body.user_id,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({
        "status": "ok",
        "project_id": pid,
        "settings": settings,
    })))
}

#[derive(serde::Deserialize)]
struct ProjectOpenBody {
    project_id: String,
}

async fn api_projects_open(
    State(app): State<AppState>,
    Json(body): Json<ProjectOpenBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match app.project.with_db(|conn| {
        let proj =
            open_project(conn, &app.project.paths, &body.project_id).map_err(|e| e.to_string())?;
        if proj.is_some() {
            prepare_project_workspace(conn, &app.project.paths, &body.project_id)
                .map_err(|e| e.to_string())?;
        }
        match proj {
            Some(p) => {
                let active = get_active_project_id(conn).map_err(|e| e.to_string())?;
                Ok(Json(json!({
                    "status": "ok",
                    "project": p,
                    "active_project_id": active,
                })))
            }
            None => Err(format!("Projekt '{}' ne postoji.", body.project_id)),
        }
    }) {
        Ok(v) => Ok(v),
        Err((_, msg)) if msg.contains("ne postoji") => Err((StatusCode::NOT_FOUND, msg)),
        Err(e) => Err(e),
    }
}

#[derive(serde::Deserialize)]
struct ProjectDeleteBody {
    project_ids: Vec<String>,
}

async fn api_projects_delete(
    State(app): State<AppState>,
    Json(body): Json<ProjectDeleteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.project_ids.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "project_ids je prazan.".into(),
        ));
    }
    for pid in &body.project_ids {
        app.ingest_card_thumbs.block_project(pid);
        app.ingest_durations.block_project(pid);
        app.ingest_posters.block_project(pid);
        app.ingest_proxy.block_project(pid);
        app.ingest_import.block_project(pid);
        app.ingest_audio_wrap.block_project(pid);
        app.filmstrip.block_project(pid);
        app.waveform.block_project(pid);
    }
    app.filmstrip.wait_drained(4000).await;
    app.waveform.wait_drained(4000).await;
    app.ingest_posters.wait_drained(4000).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    app.project.with_db(|conn| {
        let (removed, _) = delete_projects(conn, &app.project.paths, &body.project_ids)
            .map_err(|e| e.to_string())?;
        let active = get_active_project_id(conn).map_err(|e| e.to_string())?;
        let projects = list_projects(conn).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "removed": removed,
            "active_project_id": active,
            "projects": projects,
        })))
    })
}

async fn api_projects_cleanup_orphans(
    State(app): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let orphans = app.project.with_db(|conn| {
        orphan_project_dir_names(conn, &app.project.paths).map_err(|e| e.to_string())
    })?;
    for name in &orphans {
        app.ingest_card_thumbs.block_project(name);
        app.ingest_durations.block_project(name);
        app.ingest_posters.block_project(name);
        app.ingest_proxy.block_project(name);
        app.ingest_import.block_project(name);
        app.ingest_audio_wrap.block_project(name);
        app.filmstrip.block_project(name);
        app.waveform.block_project(name);
    }
    app.filmstrip.wait_drained(4000).await;
    app.waveform.wait_drained(4000).await;
    app.ingest_posters.wait_drained(4000).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    app.project.with_db(|conn| {
        let (removed, leftovers) =
            cleanup_orphan_project_dirs(conn, &app.project.paths).map_err(|e| e.to_string())?;
        Ok(Json(json!({
            "status": "ok",
            "removed": removed,
            "leftovers": leftovers,
        })))
    })
}

#[derive(serde::Deserialize)]
struct CollabSessionBody {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    station_id: String,
    #[serde(default)]
    client_label: String,
    #[serde(default)]
    project_id: String,
}

async fn api_collab_session(
    State(app): State<AppState>,
    Json(body): Json<CollabSessionBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        let session = start_session(
            conn,
            &app.project.paths,
            &body.display_name,
            &body.role,
            &body.station_id,
            &body.client_label,
            &body.project_id,
        )
        .map_err(|e| e.to_string())?;
        let session_id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !session_id.is_empty() {
            touch_collab_session(conn, session_id).map_err(|e| e.to_string())?;
        }
        Ok(Json(json!({ "status": "ok", "session": session })))
    })
}

#[derive(serde::Deserialize)]
struct CollabTouchBody {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    project_id: String,
}

async fn api_collab_touch(
    State(app): State<AppState>,
    Json(body): Json<CollabTouchBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.project.with_db(|conn| {
        touch_session(conn, &app.project.paths, &body.session_id, &body.project_id)
            .map_err(|e| e.to_string())?;
        let session =
            super::collab::get_session(conn, &body.session_id).map_err(|e| e.to_string())?;
        Ok(Json(json!({ "status": "ok", "session": session })))
    })
}
