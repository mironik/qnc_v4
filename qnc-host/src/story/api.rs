use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;

use crate::app_state::AppState;
use crate::frame_time::frame_to_seconds;

use super::db::{
    commit_story, create_cover, create_marker, create_marker_from_frame,
    create_marker_from_part_frame, create_part, create_part_with_range, delete_cover,
    delete_marker, delete_part, load_state, move_marker, reorder_part, select_cover,
    select_marker_slot, select_part, select_shot, set_part_mark_in, set_part_mark_in_frame,
    set_part_mark_out, set_part_mark_out_frame, update_cover, update_marker, update_marker_frame,
    update_part, SegmentRangeInput,
};
use super::playback::{PlaybackState, PlaybackStore};
use super::playback_render;
use super::playlist::{build_editorial_playlist, EditorialPlaylist};
use super::timeline_model::{build_source_timeline_model, build_wrap_timeline_model};

#[derive(serde::Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
struct CreatePartBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    kind: String,
    virtual_shot_id: Option<String>,
    /// Talking Head / Voice over: source IN/OUT creates a virtual segment row.
    #[serde(default)]
    clip_id: Option<String>,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
    #[serde(default)]
    in_seconds: Option<f64>,
    #[serde(default)]
    out_seconds: Option<f64>,
}

#[derive(serde::Deserialize)]
struct PartIdBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    part_id: String,
}

#[derive(serde::Deserialize)]
struct ShotIdBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    virtual_shot_id: String,
}

#[derive(serde::Deserialize)]
struct UpdatePartBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    part_id: String,
    title: Option<String>,
    text: Option<String>,
    kind: Option<String>,
}

#[derive(serde::Deserialize)]
struct ReorderPartBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    part_id: String,
    #[serde(default)]
    direction: String,
}

#[derive(serde::Deserialize)]
struct PartMarkBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    part_id: String,
    local_frame: Option<i64>,
    local_sec: Option<f64>,
}

#[derive(serde::Deserialize)]
struct CreateMarkerBody {
    #[serde(default)]
    project_id: String,
    timeline_sec: Option<f64>,
    timeline_frame: Option<i64>,
    #[serde(default)]
    part_id: String,
    #[serde(default)]
    after_part_id: String,
    label: Option<String>,
    local_sec: Option<f64>,
    local_frame: Option<i64>,
    origin_local_sec: Option<f64>,
    origin_local_frame: Option<i64>,
}

#[derive(serde::Deserialize)]
struct MarkerIdBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    marker_id: String,
}

#[derive(serde::Deserialize)]
struct MoveMarkerBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    marker_id: String,
    #[serde(default)]
    direction: String,
}

#[derive(serde::Deserialize)]
struct UpdateMarkerBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    marker_id: String,
    timeline_sec: Option<f64>,
    timeline_frame: Option<i64>,
    label: Option<String>,
}

#[derive(serde::Deserialize)]
struct SlotIdBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    slot_id: String,
}

#[derive(serde::Deserialize)]
struct CreateCoverBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    slot_id: String,
    clip_id: Option<String>,
    virtual_shot_id: Option<String>,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
    #[serde(default)]
    source_in_frame: Option<i64>,
    #[serde(default)]
    source_out_frame: Option<i64>,
    title: Option<String>,
    note: Option<String>,
}

#[derive(serde::Deserialize)]
struct CoverIdBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    cover_id: String,
}

#[derive(serde::Deserialize)]
struct UpdateCoverBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    cover_id: String,
    title: Option<String>,
    note: Option<String>,
    clip_id: Option<String>,
    virtual_shot_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct CommitBody {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
struct SourceTimelineQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    #[serde(default)]
    duration_frames: Option<i64>,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
    #[serde(default)]
    duration_sec: Option<f64>,
    #[serde(default)]
    in_sec: Option<f64>,
    #[serde(default)]
    out_sec: Option<f64>,
    #[serde(default)]
    timeline_fps: Option<f64>,
}

#[derive(serde::Deserialize)]
struct PlaybackStartBody {
    #[serde(default)]
    project_id: String,
    /// Source mode: durable DB-first playback unit.
    #[serde(default)]
    virtual_shot_id: String,
    /// All/source mode: play one source clip through the neutral frame/audio path.
    #[serde(default)]
    clip_id: String,
    #[serde(default)]
    in_sec: Option<f64>,
    #[serde(default)]
    out_sec: Option<f64>,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
}

#[derive(serde::Deserialize)]
struct PlaybackSessionBody {
    #[serde(default)]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct PlaybackSeekBody {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    virtual_frame: Option<i64>,
    #[serde(default)]
    virtual_sec: Option<f64>,
}

#[derive(serde::Deserialize)]
struct PlaybackPauseBody {
    #[serde(default)]
    session_id: String,
    paused: bool,
}

#[derive(serde::Deserialize)]
struct PlaybackStateQuery {
    #[serde(default)]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct PlaybackAudioQuery {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    from_frame: Option<i64>,
    #[serde(default)]
    from_sec: Option<f64>,
    #[serde(default)]
    duration_frames: Option<i64>,
    #[serde(default)]
    duration_sec: Option<f64>,
}

#[derive(serde::Deserialize)]
struct PlaybackFrameQuery {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    virtual_frame: Option<i64>,
    #[serde(default)]
    virtual_sec: Option<f64>,
}

#[allow(dead_code)]
pub fn router() -> Router<AppState> {
    let router = Router::new()
        .route("/api/story/state", get(api_state))
        .route("/api/story/playlist", get(api_playlist))
        .route("/api/story/timeline-model", get(api_timeline_model))
        .route(
            "/api/story/timeline-model/source",
            get(api_source_timeline_model),
        )
        .route("/api/story/part/create", post(api_part_create))
        .route("/api/story/part/update", post(api_part_update))
        .route("/api/story/part/delete", post(api_part_delete))
        .route("/api/story/part/reorder", post(api_part_reorder))
        .route("/api/story/part/select", post(api_part_select))
        .route("/api/story/part/mark_in", post(api_part_mark_in))
        .route("/api/story/part/mark_out", post(api_part_mark_out))
        .route("/api/story/shot/select", post(api_shot_select))
        .route("/api/story/marker/create", post(api_marker_create))
        .route("/api/story/marker/delete", post(api_marker_delete))
        .route("/api/story/marker/move", post(api_marker_move))
        .route("/api/story/marker/update", post(api_marker_update))
        .route(
            "/api/story/marker_slot/select",
            post(api_marker_slot_select),
        )
        .route("/api/story/cover/create", post(api_cover_create))
        .route("/api/story/cover/update", post(api_cover_update))
        .route("/api/story/cover/delete", post(api_cover_delete))
        .route("/api/story/cover/select", post(api_cover_select))
        .route("/api/story/commit", post(api_commit))
        .route("/api/story/native/launch", post(api_native_launch))
        .route("/api/story/play-media", get(api_play_media))
        .merge(crate::editor_assets::router("/api/story"));

    if legacy_story_playback_enabled() {
        router.merge(legacy_story_playback_router())
    } else {
        router
    }
}

fn legacy_story_playback_enabled() -> bool {
    std::env::var("QNC_LEGACY_STORY_PLAYBACK")
        .map(legacy_story_playback_value_enabled)
        .unwrap_or(false)
}

fn legacy_story_playback_value_enabled(value: impl AsRef<str>) -> bool {
    matches!(
        value.as_ref().trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn legacy_story_playback_router() -> Router<AppState> {
    Router::new()
        .route("/api/story/playback/start", post(api_playback_start))
        .route("/api/story/playback/stop", post(api_playback_stop))
        .route("/api/story/playback/seek", post(api_playback_seek))
        .route("/api/story/playback/pause", post(api_playback_pause))
        .route("/api/story/playback/state", get(api_playback_state))
        .route("/api/story/playback/audio", get(api_playback_audio))
        .route("/api/story/playback/frame", get(api_playback_frame))
}

async fn api_playlist(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<EditorialPlaylist>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let playlist = build_editorial_playlist(&app.project.paths, &pid).map_err(map_bad_request)?;
    Ok(Json(playlist))
}

async fn api_playback_start(
    State(app): State<AppState>,
    Json(body): Json<PlaybackStartBody>,
) -> Result<Json<PlaybackState>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let playback: &PlaybackStore = &app.story_playback;
    let shot_id = body.virtual_shot_id.trim();
    let clip = body.clip_id.trim();
    let state = if !shot_id.is_empty() {
        playback
            .start_virtual_shot(&app.project.paths, &pid, shot_id)
            .map_err(map_bad_request)?
    } else if clip.is_empty() {
        playback
            .start(&app.project.paths, &pid)
            .map_err(map_bad_request)?
    } else {
        match (body.in_frame, body.out_frame) {
            (Some(in_frame), Some(out_frame)) => playback
                .start_source_frames(&app.project.paths, &pid, clip, in_frame, out_frame)
                .map_err(map_bad_request)?,
            _ => playback
                .start_source(&app.project.paths, &pid, clip, body.in_sec, body.out_sec)
                .map_err(map_bad_request)?,
        }
    };
    Ok(Json(state))
}

async fn api_playback_stop(
    State(app): State<AppState>,
    Json(body): Json<PlaybackSessionBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.story_playback
        .stop(&body.session_id)
        .map_err(map_bad_request)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn api_playback_seek(
    State(app): State<AppState>,
    Json(body): Json<PlaybackSeekBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let Some(frame) = body.virtual_frame else {
        let msg = if body.virtual_sec.is_some() {
            "playback seek je frame-only; pošalji virtual_frame umjesto virtual_sec"
        } else {
            "playback seek traži virtual_frame"
        };
        return Err((StatusCode::BAD_REQUEST, msg.into()));
    };
    app.story_playback
        .seek_frame(&body.session_id, frame)
        .map_err(map_bad_request)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn api_playback_pause(
    State(app): State<AppState>,
    Json(body): Json<PlaybackPauseBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    app.story_playback
        .pause(&body.session_id, body.paused)
        .map_err(map_bad_request)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn api_playback_state(
    State(app): State<AppState>,
    Query(q): Query<PlaybackStateQuery>,
) -> Result<Json<PlaybackState>, (StatusCode, String)> {
    let state = app
        .story_playback
        .state(&app.project.paths, &q.session_id)
        .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_playback_audio(
    State(app): State<AppState>,
    Query(q): Query<PlaybackAudioQuery>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let sid = q.session_id.trim();
    if sid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "session_id required".into()));
    }
    let session = app.story_playback.session(sid).map_err(map_bad_request)?;
    let fps = session
        .source_clip
        .as_ref()
        .map(|src| src.fps)
        .unwrap_or(session.playlist.timeline_fps)
        .max(1.0);
    if q.from_sec.is_some() || q.duration_sec.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "playback audio je frame-only; pošalji from_frame/duration_frames umjesto sekundi"
                .into(),
        ));
    }
    let from = q
        .from_frame
        .map(|frame| frame_to_seconds(frame.max(0), fps))
        .unwrap_or_else(|| frame_to_seconds(session.clock.virtual_frame, fps))
        .max(0.0);
    let duration = q
        .duration_frames
        .map(|frames| frame_to_seconds(frames.max(1), fps))
        .unwrap_or_else(|| frame_to_seconds((30.0 * fps).round() as i64, fps))
        .max(0.25)
        .min(120.0);
    let path = playback_render::render_mixed_audio(&app.project.paths, &session, from, duration)
        .await
        .map_err(map_bad_request)?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let chunk = crate::editor_assets::media_chunk_bytes(meta.len(), duration, 10.0);
    crate::editor_assets::serve_media_path(
        path,
        headers.get(axum::http::header::RANGE),
        Some(chunk),
    )
    .await
}

async fn api_playback_frame(
    State(app): State<AppState>,
    Query(q): Query<PlaybackFrameQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let sid = q.session_id.trim();
    if sid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "session_id required".into()));
    }
    let session = app.story_playback.session(sid).map_err(map_bad_request)?;
    if q.virtual_frame.is_none() && q.virtual_sec.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "playback frame render je frame-only; pošalji virtual_frame umjesto virtual_sec".into(),
        ));
    }
    let virtual_frame = q
        .virtual_frame
        .unwrap_or(session.clock.virtual_frame)
        .max(0);
    let path =
        playback_render::render_preview_frame_at_frame(&app.project.paths, &session, virtual_frame)
            .await
            .map_err(map_bad_request)?;
    crate::editor_assets::serve_media_path(path, None, None).await
}

async fn api_timeline_model(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let model = build_wrap_timeline_model(&app.project.paths, &pid).map_err(map_bad_request)?;
    serde_json::to_value(model)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn api_source_timeline_model(
    State(app): State<AppState>,
    Query(q): Query<SourceTimelineQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = q.clip_id.trim();
    if clip_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "clip_id required".into()));
    }
    if q.duration_sec.is_some() || q.in_sec.is_some() || q.out_sec.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "source timeline model je frame-only; pošalji duration_frames/in_frame/out_frame"
                .into(),
        ));
    }
    if q.timeline_fps.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "source timeline model FPS dolazi iz source klipa; ne šalji timeline_fps".into(),
        ));
    }
    let fps = crate::media_pool::resolve_clip_fps(&app.project.paths, &pid, clip_id)
        .map_err(map_bad_request)?;
    let duration_frames = q
        .duration_frames
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "duration_frames required".to_string(),
            )
        })?
        .max(0);
    let in_frame = q.in_frame.unwrap_or(0).max(0).min(duration_frames);
    let out_frame = q.out_frame.unwrap_or(duration_frames).max(in_frame);
    let duration = frame_to_seconds(duration_frames, fps);
    let in_sec = frame_to_seconds(in_frame, fps);
    let out_sec = frame_to_seconds(out_frame, fps);
    let model = build_source_timeline_model(&pid, clip_id, duration, in_sec, out_sec, fps)
        .map_err(map_bad_request)?;
    serde_json::to_value(model)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn api_state(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let state = load_state(&app.project.paths, &pid).map_err(map_store_err)?;
    Ok(Json(state))
}

async fn api_part_create(
    State(app): State<AppState>,
    Json(body): Json<CreatePartBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = if body.in_frame.is_some() || body.out_frame.is_some() {
        create_part_with_range(
            &app.project.paths,
            &pid,
            &body.kind,
            body.virtual_shot_id.as_deref(),
            body.clip_id.as_deref(),
            SegmentRangeInput {
                in_frame: body.in_frame,
                out_frame: body.out_frame,
                in_seconds: body.in_seconds,
                out_seconds: body.out_seconds,
            },
        )
    } else {
        create_part(
            &app.project.paths,
            &pid,
            &body.kind,
            body.virtual_shot_id.as_deref(),
            body.clip_id.as_deref(),
            body.in_seconds,
            body.out_seconds,
        )
    }
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_update(
    State(app): State<AppState>,
    Json(body): Json<UpdatePartBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let title = body.title.as_deref();
    let text = body.text.as_deref();
    let kind = body.kind.as_deref();
    let state = update_part(&app.project.paths, &pid, &body.part_id, title, text, kind)
        .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_delete(
    State(app): State<AppState>,
    Json(body): Json<PartIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = delete_part(&app.project.paths, &pid, &body.part_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_reorder(
    State(app): State<AppState>,
    Json(body): Json<ReorderPartBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = reorder_part(&app.project.paths, &pid, &body.part_id, &body.direction)
        .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_select(
    State(app): State<AppState>,
    Json(body): Json<PartIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = select_part(&app.project.paths, &pid, &body.part_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_mark_in(
    State(app): State<AppState>,
    Json(body): Json<PartMarkBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = (if let Some(local_frame) = body.local_frame {
        set_part_mark_in_frame(&app.project.paths, &pid, &body.part_id, local_frame)
    } else {
        set_part_mark_in(
            &app.project.paths,
            &pid,
            &body.part_id,
            body.local_sec.unwrap_or(0.0),
        )
    })
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_part_mark_out(
    State(app): State<AppState>,
    Json(body): Json<PartMarkBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = (if let Some(local_frame) = body.local_frame {
        set_part_mark_out_frame(&app.project.paths, &pid, &body.part_id, local_frame)
    } else {
        set_part_mark_out(
            &app.project.paths,
            &pid,
            &body.part_id,
            body.local_sec.unwrap_or(0.0),
        )
    })
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_shot_select(
    State(app): State<AppState>,
    Json(body): Json<ShotIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state =
        select_shot(&app.project.paths, &pid, &body.virtual_shot_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_marker_create(
    State(app): State<AppState>,
    Json(body): Json<CreateMarkerBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let label = body.label.as_deref();
    let part_id = if !body.part_id.trim().is_empty() {
        Some(body.part_id.as_str())
    } else if !body.after_part_id.trim().is_empty() {
        Some(body.after_part_id.as_str())
    } else {
        None
    };
    let local_frame = body.local_frame.or(body.origin_local_frame);
    let state = (if let Some(timeline_frame) = body.timeline_frame {
        create_marker_from_frame(
            &app.project.paths,
            &pid,
            timeline_frame,
            part_id,
            label,
            local_frame,
        )
    } else if let (Some(part_id), Some(local_frame)) = (part_id, local_frame) {
        create_marker_from_part_frame(&app.project.paths, &pid, part_id, label, local_frame)
    } else {
        create_marker(
            &app.project.paths,
            &pid,
            body.timeline_sec,
            part_id,
            label,
            body.local_sec.or(body.origin_local_sec),
        )
    })
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_marker_delete(
    State(app): State<AppState>,
    Json(body): Json<MarkerIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state =
        delete_marker(&app.project.paths, &pid, &body.marker_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_marker_move(
    State(app): State<AppState>,
    Json(body): Json<MoveMarkerBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = move_marker(&app.project.paths, &pid, &body.marker_id, &body.direction)
        .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_marker_update(
    State(app): State<AppState>,
    Json(body): Json<UpdateMarkerBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = if let Some(timeline_frame) = body.timeline_frame {
        update_marker_frame(
            &app.project.paths,
            &pid,
            &body.marker_id,
            timeline_frame,
            body.label.as_deref(),
        )
    } else {
        update_marker(
            &app.project.paths,
            &pid,
            &body.marker_id,
            body.timeline_sec.unwrap_or(0.0),
            body.label.as_deref(),
        )
    }
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_marker_slot_select(
    State(app): State<AppState>,
    Json(body): Json<SlotIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state =
        select_marker_slot(&app.project.paths, &pid, &body.slot_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_cover_create(
    State(app): State<AppState>,
    Json(body): Json<CreateCoverBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = create_cover(
        &app.project.paths,
        &pid,
        &body.slot_id,
        body.clip_id.as_deref(),
        body.virtual_shot_id.as_deref(),
        body.title.as_deref(),
        body.note.as_deref(),
        SegmentRangeInput {
            in_frame: body.in_frame.or(body.source_in_frame),
            out_frame: body.out_frame.or(body.source_out_frame),
            ..SegmentRangeInput::default()
        },
    )
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_cover_update(
    State(app): State<AppState>,
    Json(body): Json<UpdateCoverBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = update_cover(
        &app.project.paths,
        &pid,
        &body.cover_id,
        body.title.as_deref(),
        body.note.as_deref(),
        body.clip_id.as_deref(),
        body.virtual_shot_id.as_deref(),
    )
    .map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_cover_delete(
    State(app): State<AppState>,
    Json(body): Json<CoverIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = delete_cover(&app.project.paths, &pid, &body.cover_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_cover_select(
    State(app): State<AppState>,
    Json(body): Json<CoverIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = select_cover(&app.project.paths, &pid, &body.cover_id).map_err(map_bad_request)?;
    Ok(Json(state))
}

async fn api_commit(
    State(app): State<AppState>,
    Json(body): Json<CommitBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let state = commit_story(&app.project.paths, &pid).map_err(map_bad_request)?;
    Ok(Json(state))
}

#[derive(serde::Deserialize)]
struct NativeLaunchBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    #[serde(default)]
    seek: f64,
}

#[derive(serde::Deserialize)]
struct PlayMediaQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
}

async fn api_play_media(
    State(app): State<AppState>,
    Query(q): Query<PlayMediaQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip = q.clip_id.trim();
    if clip.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "clip_id required".into()));
    }
    let media = crate::media::resolve_play_media(&app.project.paths, &pid, clip)
        .map_err(map_bad_request)?;
    let kind = match media.kind {
        crate::media::PlayMediaKind::Proxy => "proxy",
        crate::media::PlayMediaKind::Original => "original",
    };
    Ok(Json(serde_json::json!({
        "project_id": pid,
        "clip_id": media.clip_id,
        "kind": kind,
        "path": media.path.to_string_lossy(),
        "field_order": media.field_order,
        "interlaced": media.interlaced,
        "source_class": media.source_class,
        "proxy_recipe": media.proxy_recipe,
    })))
}

async fn api_native_launch(
    State(app): State<AppState>,
    Json(body): Json<NativeLaunchBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let clip = body.clip_id.trim();
    let req = super::native_launch::NativeLaunchRequest {
        project_id: pid,
        clip_id: if clip.is_empty() {
            None
        } else {
            Some(clip.to_string())
        },
        seek: body.seek,
    };
    match super::native_launch::launch(&req) {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((StatusCode::BAD_REQUEST, e)),
    }
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
    if is_story_client_error(&e) {
        (StatusCode::BAD_REQUEST, e)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

fn is_story_client_error(message: &str) -> bool {
    let msg = message.to_lowercase();
    [
        "not found",
        "invalid",
        "required",
        "already exists",
        "proxy_missing",
        "original_missing",
        "slot_id",
        "clip_id",
        "cover_id",
        "marker_id",
        "part_id",
        "virtual_shot_id",
        "nije prona",
        "nije uvezen",
        "nije potvr",
        "nema proxy",
        "nema marker",
        "nema m-m",
        "nema valjan",
        "nedostaje",
        "odaberi",
        "treba",
        "mora biti",
        "prazan",
        "poslije in",
        "timeline_fps_invalid",
    ]
    .iter()
    .any(|needle| msg.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::{is_story_client_error, legacy_story_playback_value_enabled, map_bad_request};
    use axum::http::StatusCode;

    #[test]
    fn cover_validation_errors_are_bad_request() {
        assert!(is_story_client_error(
            "Pokrivalica iz source frameova treba clip_id"
        ));
        assert!(is_story_client_error(
            "Klip 'mironik_2002' nije uvezen u ingest"
        ));
        assert!(is_story_client_error("nema proxy za 'mironik_2002'"));
    }

    #[test]
    fn map_bad_request_keeps_unknown_errors_internal() {
        let (status, _) = map_bad_request("database is locked".into());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn legacy_story_playback_is_explicit_opt_in() {
        assert!(!legacy_story_playback_value_enabled(""));
        assert!(!legacy_story_playback_value_enabled("0"));
        assert!(!legacy_story_playback_value_enabled("false"));
        assert!(legacy_story_playback_value_enabled("1"));
        assert!(legacy_story_playback_value_enabled("true"));
        assert!(legacy_story_playback_value_enabled(" yes "));
        assert!(legacy_story_playback_value_enabled("ON"));
    }
}
