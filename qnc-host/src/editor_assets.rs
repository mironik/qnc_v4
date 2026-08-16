use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{OriginalUri, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::app_state::AppState;
use crate::filmstrip::{
    frame_path_for_index, frame_path_for_seek, get_filmstrip, list_frames_for_clip,
    pad_frames_to_default_with_placeholder, placeholder_url_for_api, sync_filmstrip_from_disk,
    PLACEHOLDER_JPEG,
};
use crate::frame_time::{rational_fps, require_fps};
use crate::ingest::db::resolve_ingest_poster_path;
use crate::ingest::thumb::{
    extract_poster_jpeg_at_seek, extract_preview_jpeg_at_seek, media_has_audio_stream,
    resolve_ffmpeg,
};
use crate::media::resolve_play_media;
use crate::media_pool::{list_clips_enriched, mark_filmstrip_building, resolve_clip_fps};
use crate::virtual_shots::{
    add_virtual_shot, add_virtual_shot_from_frames, cover_path_for_shot, derive_virtual_shot,
    derive_virtual_shot_from_frames, list_virtual_shots, update_virtual_shot,
    update_virtual_shot_from_frames, virtual_shot_frames,
};
use crate::waveform::{ready as waveform_ready, snapshot as waveform_snapshot};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VirtualStreamKind {
    Mux,
    AudioOnly,
}

pub(crate) async fn ensure_virtual_stream_cached(
    paths: &crate::project::db::ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
    fps: f64,
) -> Result<PathBuf, String> {
    ensure_virtual_stream_cached_kind(
        paths,
        project_id,
        clip_id,
        in_frame,
        out_frame,
        fps,
        VirtualStreamKind::Mux,
    )
    .await
}

pub(crate) async fn ensure_virtual_stream_cached_kind(
    paths: &crate::project::db::ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
    fps: f64,
    kind: VirtualStreamKind,
) -> Result<PathBuf, String> {
    let pid = project_id.trim();
    let clip = clip_id.trim();
    if pid.is_empty() || clip.is_empty() {
        return Err("project_id and clip_id required".into());
    }
    let play = resolve_play_media(paths, pid, clip)?;
    let proxy = play.path;
    let cache_root = std::env::temp_dir()
        .join("qnc")
        .join("qstory_virtual_stream")
        .join(safe_id(pid));
    let clip_id = clip.to_string();
    tokio::task::spawn_blocking(move || {
        ensure_virtual_stream(
            &proxy,
            &cache_root,
            &clip_id,
            in_frame,
            out_frame,
            fps,
            kind,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(serde::Deserialize)]
struct ProjectQuery {
    #[serde(default)]
    project_id: String,
}

#[derive(serde::Deserialize)]
struct ClipQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    #[serde(default)]
    duration_sec: f64,
    #[serde(default = "default_media_chunk_sec")]
    chunk_sec: f64,
}

#[derive(serde::Deserialize)]
struct ThumbnailQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    #[serde(default)]
    seek: f64,
    #[serde(default = "default_frame_index")]
    frame_index: i64,
    /// `preview` = JPEG at source/proxy raster (skip filmstrip thumbs).
    #[serde(default)]
    quality: String,
}

#[derive(Clone, serde::Deserialize)]
struct VirtualStreamQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    /// Canonical playback path: backend resolves clip/fps/in/out from the DB row.
    /// A segment streams by its part_id (uses the corrected qstory_parts IN/OUT).
    #[serde(default)]
    part_id: String,
    /// A cover streams by its cover_id (slot-bounded source trim from the DB).
    #[serde(default)]
    cover_id: String,
    /// Editorial path: backend resolves clip/fps/in/out from the shot itself.
    #[serde(default)]
    virtual_shot_id: String,
    /// Seconds-based cut (authoritative). Backend converts to source frames itself.
    #[serde(default)]
    in_seconds: Option<f64>,
    #[serde(default)]
    out_seconds: Option<f64>,
    /// Deprecated frame-based fallback for older clients. Ignored when seconds are present.
    #[serde(default)]
    in_frame: i64,
    #[serde(default)]
    out_frame: i64,
    /// When true with part_id: encode audio-only (OFF / A1 under cover). Accepts 1/true/yes.
    #[serde(default)]
    audio_only: String,
}

#[derive(serde::Deserialize)]
struct TimelineBuildBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    #[serde(default = "default_frames")]
    frames: u32,
    #[serde(default)]
    media_path: String,
}

#[derive(serde::Deserialize)]
struct VirtualShotBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    /// New editorial path: derive from an existing virtual shot (in/out are local to it).
    #[serde(default)]
    source_shot_id: String,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
    #[serde(default)]
    in_seconds: f64,
    #[serde(default)]
    out_seconds: f64,
}

#[derive(serde::Deserialize)]
struct VirtualShotUpdateBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    shot_id: String,
    #[serde(default)]
    in_frame: Option<i64>,
    #[serde(default)]
    out_frame: Option<i64>,
    #[serde(default)]
    in_seconds: f64,
    #[serde(default)]
    out_seconds: f64,
}

#[derive(serde::Deserialize)]
struct VirtualShotThumbQuery {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    kind: String,
}

fn default_frame_index() -> i64 {
    -1
}

fn default_frames() -> u32 {
    crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES
}

fn default_media_chunk_sec() -> f64 {
    10.0
}

fn timeline_build_frames(raw: u32) -> u32 {
    if raw == 0 {
        default_frames()
    } else {
        raw.clamp(1, 24)
    }
}

pub fn router(prefix: &str) -> Router<AppState> {
    Router::new()
        .route(&format!("{prefix}/clips"), get(api_clips))
        .route(&format!("{prefix}/media"), get(api_media))
        .route(&format!("{prefix}/virtual-stream"), get(api_virtual_stream))
        .route(&format!("{prefix}/thumbnail"), get(api_thumbnail))
        .route(&format!("{prefix}/filmstrip"), get(api_filmstrip))
        .route(
            &format!("{prefix}/filmstrip/placeholder"),
            get(api_filmstrip_placeholder),
        )
        .route(
            &format!("{prefix}/waveform/status"),
            get(api_waveform_status),
        )
        .route(
            &format!("{prefix}/timeline/build"),
            post(api_timeline_build),
        )
        .route(&format!("{prefix}/virtual-shot"), post(api_virtual_shot))
        .route(
            &format!("{prefix}/virtual-shot/update"),
            post(api_virtual_shot_update),
        )
        .route(
            &format!("{prefix}/virtual-shot/{{shot_id}}/thumb"),
            get(api_virtual_shot_thumb),
        )
}

async fn api_clips(
    State(app): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let data = list_clips_enriched(&app.project.paths, &pid).map_err(internal)?;
    let virtual_shots = list_virtual_shots(&app.project.paths, &pid).map_err(internal)?;
    Ok(Json(json!({
        "project_id": pid,
        "clips": data.get("clips").cloned().unwrap_or_else(|| json!([])),
        "summary": data.get("summary").cloned().unwrap_or_else(|| json!({})),
        "virtual_shots": virtual_shots,
    })))
}

async fn api_media(
    State(app): State<AppState>,
    Query(q): Query<ClipQuery>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = required_clip_id(&q.clip_id)?;
    let path = resolve_play_media(&app.project.paths, &pid, clip_id)
        .map(|m| m.path)
        .map_err(|proxy_err| (StatusCode::NOT_FOUND, proxy_err))?;
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|error| internal(error.to_string()))?
        .len();
    let chunk_bytes = media_chunk_bytes(size, q.duration_sec, q.chunk_sec);
    serve_file(path, headers.get(header::RANGE), Some(chunk_bytes)).await
}

async fn api_virtual_stream(
    State(app): State<AppState>,
    Query(q): Query<VirtualStreamQuery>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    // Canonical playback path: the backend is the single source of truth. A segment
    // resolves by part_id (story_parts IN/OUT), a cover by cover_id
    // (slot-bounded source trim from story_covers). The frontend never decides the source range.
    let (clip_id, fps, in_frame, out_frame) = if !q.part_id.trim().is_empty() {
        let (clip_id, in_frame, out_frame, fps) =
            crate::story::part_stream_frames(&app.project.paths, &pid, q.part_id.trim())
                .map_err(|error| (StatusCode::BAD_REQUEST, format!("Segment stream: {error}")))?;
        (clip_id, fps, in_frame, out_frame)
    } else if !q.cover_id.trim().is_empty() {
        let (clip_id, in_frame, out_frame, fps) =
            crate::story::cover_stream_frames(&app.project.paths, &pid, q.cover_id.trim())
                .map_err(|error| (StatusCode::BAD_REQUEST, format!("Cover stream: {error}")))?;
        (clip_id, fps, in_frame, out_frame)
    } else if !q.virtual_shot_id.trim().is_empty() {
        // Editorial path: everything derives from the virtual shot.
        // Broadcast truth: source frames come straight from the DB row (no sec*fps).
        let (clip_id, in_frame, out_frame, fps) =
            virtual_shot_frames(&app.project.paths, &pid, q.virtual_shot_id.trim()).map_err(
                |error| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("Virtualni stream: {error}"),
                    )
                },
            )?;
        (clip_id, fps, in_frame, out_frame)
    } else {
        let clip_id = required_clip_id(&q.clip_id)?.to_string();
        // Source FPS is resolved from the media/DB, never trusted from the client.
        let fps = resolve_clip_fps(&app.project.paths, &pid, &clip_id).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("Virtualni stream: {error}"),
            )
        })?;
        // Prefer seconds-based cut; the server converts to source frames with the
        // resolved fps so client-side fps guesses can never shift the cut.
        let (in_frame, out_frame) = match (q.in_seconds, q.out_seconds) {
            (Some(in_sec), Some(out_sec)) => {
                let in_frame = (in_sec.max(0.0) * fps).round() as i64;
                let out_frame = ((out_sec.max(0.0) * fps).round() as i64).max(in_frame + 1);
                (in_frame, out_frame)
            }
            _ => {
                let in_frame = q.in_frame.max(0);
                (in_frame, q.out_frame.max(in_frame + 1))
            }
        };
        (clip_id, fps, in_frame, out_frame)
    };
    let frame_count = out_frame - in_frame;
    let duration_sec = frame_count as f64 / fps;
    if duration_sec > 15.0 * 60.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Virtualni stream ne smije biti dulji od 15 minuta.".into(),
        ));
    }
    let proxy = resolve_play_media(&app.project.paths, &pid, &clip_id)
        .map(|m| m.path)
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let cache_root = std::env::temp_dir()
        .join("qnc")
        .join("qstory_virtual_stream")
        .join(safe_id(&pid));
    let generated = tokio::task::spawn_blocking(move || {
        let audio_only = matches!(
            q.audio_only.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
        let kind = if audio_only {
            VirtualStreamKind::AudioOnly
        } else {
            VirtualStreamKind::Mux
        };
        ensure_virtual_stream(
            &proxy,
            &cache_root,
            &clip_id,
            in_frame,
            out_frame,
            fps,
            kind,
        )
    })
    .await
    .map_err(|error| internal(error.to_string()))?
    .map_err(internal)?;
    let size = tokio::fs::metadata(&generated)
        .await
        .map_err(|error| internal(error.to_string()))?
        .len();
    let chunk_bytes = media_chunk_bytes(size, duration_sec, 10.0);
    serve_file(generated, headers.get(header::RANGE), Some(chunk_bytes)).await
}

async fn api_thumbnail(
    State(app): State<AppState>,
    Query(q): Query<ThumbnailQuery>,
) -> Result<Response, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = required_clip_id(&q.clip_id)?;
    let seek = q.seek.max(0.0);
    let preview = matches!(
        q.quality.trim().to_ascii_lowercase().as_str(),
        "preview" | "player" | "hi" | "high"
    );

    // Filmstrip thumbs (112×64) are for the strip only — never for the main player.
    if !preview {
        if q.frame_index >= 0 {
            if let Some(path) =
                frame_path_for_index(&app.project.paths, &pid, clip_id, q.frame_index)
            {
                return serve_file(path, None, None).await;
            }
        }
        if let Some(path) = frame_path_for_seek(&app.project.paths, &pid, clip_id, seek) {
            return serve_file(path, None, None).await;
        }
        if let Some(path) = resolve_ingest_poster_path(&app.project.paths, &pid, clip_id) {
            return serve_file(path, None, None).await;
        }
    }

    let source = match resolve_play_media(&app.project.paths, &pid, clip_id) {
        Ok(m) => m.path,
        Err(proxy_err) => crate::media::resolve_original_media(&app.project.paths, &pid, clip_id)
            .map(|m| m.path)
            .map_err(|_| (StatusCode::NOT_FOUND, proxy_err))?,
    };
    let tmp = std::env::temp_dir().join(format!(
        "qnc_editor_{}_{}_{}.jpg",
        if preview { "preview" } else { "thumb" },
        safe_id(clip_id),
        (seek * 1000.0) as i64
    ));
    if preview {
        extract_preview_jpeg_at_seek(&source, &tmp, seek).map_err(internal)?;
    } else {
        extract_poster_jpeg_at_seek(&source, &tmp, seek).map_err(internal)?;
    }
    serve_file(tmp, None, None).await
}

async fn api_filmstrip(
    State(app): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<ClipQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = required_clip_id(&q.clip_id)?;
    let duration_hint = get_filmstrip(&app.project.paths, &pid, clip_id)
        .and_then(|value| value.get("duration_sec").and_then(Value::as_f64))
        .unwrap_or(0.0);
    let _ = sync_filmstrip_from_disk(&app.project.paths, &pid, clip_id, duration_hint);
    let filmstrip = get_filmstrip(&app.project.paths, &pid, clip_id).unwrap_or_else(|| {
        json!({
            "clip_id": clip_id,
            "status": "missing",
            "duration_sec": 0,
            "frame_count": 0,
            "error": "",
        })
    });
    let namespace = uri
        .path()
        .strip_suffix("/filmstrip")
        .unwrap_or("/api/story");
    let placeholder = placeholder_url_for_api(namespace);
    let frames = pad_frames_to_default_with_placeholder(
        list_frames_for_clip(&app.project.paths, &pid, clip_id)
            .unwrap_or_default()
            .into_iter()
            .map(|frame| {
                let index = frame.get("index").and_then(Value::as_i64).unwrap_or(0);
                json!({
                    "index": index,
                    "frame_index": index,
                    "seek_sec": frame.get("seek_sec").and_then(Value::as_f64).unwrap_or(0.0),
                    "url": format!(
                        "{namespace}/thumbnail?clip_id={}&frame_index={}&project_id={}",
                        url_encode(clip_id),
                        index,
                        url_encode(&pid)
                    ),
                })
            })
            .collect::<Vec<_>>(),
        &placeholder,
    );
    Ok(Json(json!({
        "project_id": pid,
        "clip_id": clip_id,
        "filmstrip": filmstrip,
        "frames": frames,
    })))
}

async fn api_filmstrip_placeholder() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        PLACEHOLDER_JPEG,
    )
        .into_response()
}

/// Public alias for legacy `/api/media-pool/filmstrip/placeholder` callers.
pub async fn api_filmstrip_placeholder_public() -> Response {
    api_filmstrip_placeholder().await
}

async fn api_waveform_status(
    State(app): State<AppState>,
    Query(q): Query<ClipQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let clip_id = required_clip_id(&q.clip_id)?;
    Ok(Json(json!({
        "project_id": pid,
        "clip_id": clip_id,
        "waveform": waveform_snapshot(&app.project.paths, &pid, clip_id),
    })))
}

async fn api_timeline_build(
    State(app): State<AppState>,
    Json(body): Json<TimelineBuildBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let clip_id = required_clip_id(&body.clip_id)?;
    let frames = timeline_build_frames(body.frames);
    if let Some(existing) = get_filmstrip(&app.project.paths, &pid, clip_id) {
        let status = existing.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "ready" {
            if !waveform_ready(&app.project.paths, &pid, clip_id) {
                if let Some(path) = requested_media_path(&app, &pid, clip_id, &body.media_path) {
                    app.waveform.enqueue(&pid, clip_id, &path);
                }
            }
            return Ok(Json(json!({
                "status": "ready",
                "clip_id": clip_id,
                "filmstrip": existing,
                "waveform": waveform_snapshot(&app.project.paths, &pid, clip_id),
            })));
        }
        if status == "building" {
            if let Some(path) = requested_media_path(&app, &pid, clip_id, &body.media_path) {
                app.filmstrip.enqueue(&pid, clip_id, &path, frames);
            }
            return Ok(Json(json!({ "status": "building", "clip_id": clip_id })));
        }
    }
    let media = requested_media_path(&app, &pid, clip_id, &body.media_path).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Nema medijskog zapisa za '{clip_id}'."),
        )
    })?;
    mark_filmstrip_building(&app.project.paths, &pid, clip_id).map_err(internal)?;
    app.filmstrip.enqueue(&pid, clip_id, &media, frames);
    app.waveform.enqueue(&pid, clip_id, &media);
    Ok(Json(json!({ "status": "queued", "clip_id": clip_id })))
}

async fn api_virtual_shot(
    State(app): State<AppState>,
    Json(body): Json<VirtualShotBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let shot = if !body.source_shot_id.trim().is_empty() {
        match (body.in_frame, body.out_frame) {
            (Some(in_frame), Some(out_frame)) => derive_virtual_shot_from_frames(
                &app.project.paths,
                &pid,
                body.source_shot_id.trim(),
                in_frame,
                out_frame,
            ),
            _ => derive_virtual_shot(
                &app.project.paths,
                &pid,
                body.source_shot_id.trim(),
                body.in_seconds,
                body.out_seconds,
            ),
        }
    } else {
        let clip_id = required_clip_id(&body.clip_id)?;
        match (body.in_frame, body.out_frame) {
            (Some(in_frame), Some(out_frame)) => {
                add_virtual_shot_from_frames(&app.project.paths, &pid, clip_id, in_frame, out_frame)
            }
            _ => add_virtual_shot(
                &app.project.paths,
                &pid,
                clip_id,
                body.in_seconds,
                body.out_seconds,
            ),
        }
    }
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let virtual_shots = list_virtual_shots(&app.project.paths, &pid).map_err(internal)?;
    Ok(Json(json!({
        "status": "ok",
        "shot": shot,
        "virtual_shots": virtual_shots,
    })))
}

async fn api_virtual_shot_update(
    State(app): State<AppState>,
    Json(body): Json<VirtualShotUpdateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &body.project_id)?;
    let shot = match (body.in_frame, body.out_frame) {
        (Some(in_frame), Some(out_frame)) => update_virtual_shot_from_frames(
            &app.project.paths,
            &pid,
            body.shot_id.trim(),
            in_frame,
            out_frame,
        ),
        _ => update_virtual_shot(
            &app.project.paths,
            &pid,
            body.shot_id.trim(),
            body.in_seconds,
            body.out_seconds,
        ),
    }
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let virtual_shots = list_virtual_shots(&app.project.paths, &pid).map_err(internal)?;
    Ok(Json(json!({
        "status": "ok",
        "shot": shot,
        "virtual_shots": virtual_shots,
    })))
}

async fn api_virtual_shot_thumb(
    State(app): State<AppState>,
    Path(shot_id): Path<String>,
    Query(q): Query<VirtualShotThumbQuery>,
) -> Result<Response, (StatusCode, String)> {
    let pid = resolve_project_id(&app, &q.project_id)?;
    let shot_id = shot_id.trim();
    if shot_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "shot_id je prazan".into()));
    }
    let path = cover_path_for_shot(&app.project.paths, &pid, shot_id, &q.kind)
        .map_err(internal)?
        .ok_or((
            StatusCode::NOT_FOUND,
            "Virtualni kadar nije pronađen.".into(),
        ))?;
    serve_file(path, None, None).await
}

fn requested_media_path(
    app: &AppState,
    project_id: &str,
    clip_id: &str,
    requested: &str,
) -> Option<PathBuf> {
    if !requested.trim().is_empty() {
        let path = PathBuf::from(requested.trim());
        return path.is_file().then_some(path);
    }
    resolve_play_media(&app.project.paths, project_id, clip_id)
        .ok()
        .map(|m| m.path)
        .or_else(|| {
            crate::media::resolve_original_media(&app.project.paths, project_id, clip_id)
                .ok()
                .map(|m| m.path)
        })
        .filter(|path| path.is_file())
}

fn resolve_project_id(app: &AppState, project_id: &str) -> Result<String, (StatusCode, String)> {
    if project_id.trim().is_empty() {
        app.project.active_project_id()
    } else {
        Ok(project_id.trim().to_string())
    }
}

fn required_clip_id(clip_id: &str) -> Result<&str, (StatusCode, String)> {
    let clip_id = clip_id.trim();
    if clip_id.is_empty() {
        Err((StatusCode::BAD_REQUEST, "clip_id je prazan".into()))
    } else {
        Ok(clip_id)
    }
}

fn safe_id(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn internal(error: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

pub(crate) async fn serve_media_path(
    path: PathBuf,
    range: Option<&header::HeaderValue>,
    max_response_bytes: Option<u64>,
) -> Result<Response, (StatusCode, String)> {
    serve_file(path, range, max_response_bytes).await
}

async fn serve_file(
    path: PathBuf,
    range: Option<&header::HeaderValue>,
    max_response_bytes: Option<u64>,
) -> Result<Response, (StatusCode, String)> {
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|error| internal(error.to_string()))?;
    let size = meta.len();
    let content_type = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("m4a") => "audio/mp4",
        Some("mov") => "video/quicktime",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    let requested_range = parse_range_header(range, size);
    let bounded_range = bounded_media_range(requested_range, size, max_response_bytes);
    if let Some((start, end)) = bounded_range {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|error| internal(error.to_string()))?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|error| internal(error.to_string()))?;
        let length = end.saturating_sub(start).saturating_add(1);
        let mut data = vec![0_u8; length as usize];
        file.read_exact(&mut data)
            .await
            .map_err(|error| internal(error.to_string()))?;
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_LENGTH, length.to_string()),
                (header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
            ],
            data,
        )
            .into_response());
    }
    let data = tokio::fs::read(&path)
        .await
        .map_err(|error| internal(error.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        data,
    )
        .into_response())
}

pub(crate) fn media_chunk_bytes(size: u64, duration_sec: f64, chunk_sec: f64) -> u64 {
    const FALLBACK_BYTES: u64 = 8 * 1024 * 1024;
    const MIN_BYTES: u64 = 512 * 1024;
    const MAX_BYTES: u64 = 32 * 1024 * 1024;

    let seconds = if chunk_sec.is_finite() {
        chunk_sec.clamp(0.25, 10.0)
    } else {
        10.0
    };
    let estimated = if duration_sec.is_finite() && duration_sec > 0.0 && size > 0 {
        ((size as f64 / duration_sec) * seconds).ceil() as u64
    } else {
        FALLBACK_BYTES
    };
    estimated.clamp(MIN_BYTES, MAX_BYTES).min(size.max(1))
}

fn ensure_virtual_stream(
    proxy: &std::path::Path,
    cache_root: &std::path::Path,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
    fps: f64,
    kind: VirtualStreamKind,
) -> Result<PathBuf, String> {
    let fps = require_fps(fps, "virtual stream")?;
    let source_meta = std::fs::metadata(proxy).map_err(|error| error.to_string())?;
    let modified = source_meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or(0);
    std::fs::create_dir_all(cache_root).map_err(|error| error.to_string())?;
    let fps_key = (fps * 1000.0).round() as i64;
    let kind_tag = if kind == VirtualStreamKind::AudioOnly {
        "a3"
    } else {
        "v3"
    };
    let filename = format!(
        "{kind_tag}_{}_{}_{}_{}_{}_{}.mp4",
        safe_id(clip_id),
        in_frame,
        out_frame,
        fps_key,
        source_meta.len(),
        modified
    );
    let output = cache_root.join(filename);
    if output
        .metadata()
        .map(|meta| meta.len() > 1024)
        .unwrap_or(false)
    {
        return Ok(output);
    }

    let ffmpeg = resolve_ffmpeg()
        .ok_or_else(|| "FFmpeg nije dostupan za virtualni preview stream.".to_string())?;
    let temp = cache_root.join(format!(
        "{}_{}.part.mp4",
        safe_id(clip_id),
        uuid::Uuid::new_v4().simple()
    ));
    let start_sec = in_frame as f64 / fps;
    let frame_count = (out_frame - in_frame).max(1);
    let duration_sec = frame_count as f64 / fps;
    let gop = fps.round().clamp(1.0, 120.0) as i64;
    let (fps_num, fps_den) = rational_fps(fps);
    let fps_arg = if fps_den == 1 {
        fps_num.to_string()
    } else {
        format!("{fps_num}/{fps_den}")
    };
    if kind == VirtualStreamKind::AudioOnly && media_has_audio_stream(proxy) == Some(false) {
        render_silence_audio_file(&ffmpeg, &temp, duration_sec)?;
        if output.exists() {
            let _ = std::fs::remove_file(&output);
        }
        std::fs::rename(&temp, &output).map_err(|error| error.to_string())?;
        prune_virtual_stream_cache(cache_root, 96);
        return Ok(output);
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-ss")
        .arg(format!("{start_sec:.9}"))
        .arg("-i")
        .arg(proxy)
        .arg("-t")
        .arg(format!("{duration_sec:.9}"));
    if kind == VirtualStreamKind::AudioOnly {
        cmd.arg("-vn")
            .arg("-map")
            .arg("0:a:0?")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("192k")
            .arg("-ar")
            .arg("48000");
    } else {
        cmd.arg("-map")
            .arg("0:v:0")
            .arg("-map")
            .arg("0:a:0?")
            .arg("-frames:v")
            .arg(frame_count.to_string())
            .arg("-vf")
            .arg(format!("fps={fps_arg},setpts=PTS-STARTPTS"))
            .arg("-fps_mode")
            .arg("cfr")
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("ultrafast")
            .arg("-crf")
            .arg("18")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-g")
            .arg(gop.to_string())
            .arg("-keyint_min")
            .arg(gop.to_string())
            .arg("-sc_threshold")
            .arg("0")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("192k")
            .arg("-ar")
            .arg("48000");
    }
    let result = cmd
        .arg("-movflags")
        .arg("+faststart")
        .arg(&temp)
        .output()
        .map_err(|error| format!("FFmpeg virtualni stream: {error}"))?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&temp);
        let message = String::from_utf8_lossy(&result.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "FFmpeg nije kreirao virtualni stream.".into()
        } else {
            message
        });
    }
    if !temp
        .metadata()
        .map(|meta| meta.len() > 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&temp);
        return Err("Virtualni stream je prazan.".into());
    }
    if output.exists() {
        let _ = std::fs::remove_file(&output);
    }
    std::fs::rename(&temp, &output).map_err(|error| error.to_string())?;
    prune_virtual_stream_cache(cache_root, 96);
    Ok(output)
}

fn render_silence_audio_file(
    ffmpeg: &std::path::Path,
    output: &std::path::Path,
    duration_sec: f64,
) -> Result<(), String> {
    let duration = duration_sec.max(0.001);
    let result = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-t",
            &format!("{duration:.9}"),
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .output()
        .map_err(|error| format!("FFmpeg silence audio: {error}"))?;
    if !result.status.success() {
        let _ = std::fs::remove_file(output);
        let message = String::from_utf8_lossy(&result.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "FFmpeg nije kreirao silence audio.".into()
        } else {
            message
        });
    }
    if !output
        .metadata()
        .map(|meta| meta.len() > 512)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(output);
        return Err("Silence audio stream je prazan.".into());
    }
    Ok(())
}

fn prune_virtual_stream_cache(cache_root: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("mp4") {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    let remove_count = files.len() - keep;
    for (_, path) in files.into_iter().take(remove_count) {
        let _ = std::fs::remove_file(path);
    }
}

fn bounded_media_range(
    requested: Option<(u64, u64)>,
    size: u64,
    max_response_bytes: Option<u64>,
) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let limit = max_response_bytes.filter(|value| *value > 0)?;
    let (start, requested_end) = requested.unwrap_or((0, size - 1));
    let end = requested_end
        .min(start.saturating_add(limit - 1))
        .min(size - 1);
    Some((start, end))
}

fn parse_range_header(range: Option<&header::HeaderValue>, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let raw = range?.to_str().ok()?.trim();
    let spec = raw.strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (start_raw, end_raw) = first.split_once('-')?;
    if start_raw.is_empty() {
        let suffix = end_raw.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        let length = suffix.min(size);
        return Some((size - length, size - 1));
    }
    let start = start_raw.parse::<u64>().ok()?;
    if start >= size {
        return None;
    }
    let end = if end_raw.is_empty() {
        size - 1
    } else {
        end_raw.parse::<u64>().ok()?.min(size - 1)
    };
    (end >= start).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::{bounded_media_range, media_chunk_bytes};

    #[test]
    fn qstory_media_response_is_limited_to_ten_seconds() {
        let size = 100 * 1024 * 1024;
        let bytes = media_chunk_bytes(size, 100.0, 10.0);
        assert_eq!(bytes, 10 * 1024 * 1024);
        assert_eq!(
            bounded_media_range(Some((20, size - 1)), size, Some(bytes)),
            Some((20, 20 + bytes - 1))
        );
    }

    #[test]
    fn qstory_media_without_range_still_returns_a_bounded_chunk() {
        let size = 200 * 1024 * 1024;
        let bytes = media_chunk_bytes(size, 0.0, 10.0);
        assert_eq!(bytes, 8 * 1024 * 1024);
        assert_eq!(
            bounded_media_range(None, size, Some(bytes)),
            Some((0, bytes - 1))
        );
    }
}
