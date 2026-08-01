use serde_json::{json, Value};

use crate::filmstrip::{
    get_filmstrip, list_frames_for_clip, pad_frames_to_default, sync_filmstrip_from_disk,
};
use crate::project::db::{ensure_project_dirs, now_str, ProjectPaths};
use crate::waveform::snapshot as waveform_snapshot;

use super::db::{open_db, pool_summary, sync_pool_from_ingest_db};
use super::ingest_db::{pending_import_count, read_imported_clips};
use super::transcripts::clip_has_transcript;

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn list_clips_enriched(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    ensure_project_dirs(paths, project_id).map_err(|e| e.to_string())?;
    sync_pool_from_ingest_db(paths, project_id)?;
    let conn = open_db(paths, project_id)?;
    let imported = read_imported_clips(paths, project_id)?;
    let mut clips: Vec<Value> = Vec::new();
    for row in imported {
        let clip_id = row
            .get("clip_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if clip_id.is_empty() {
            continue;
        }
        let proxy_path = row.get("proxy_path").and_then(|v| v.as_str());
        let thumb_path = row.get("thumb_path").and_then(|v| v.as_str());
        let source_path = row.get("source_path").and_then(|v| v.as_str());
        let original_path = row.get("original_path").and_then(|v| v.as_str());
        let card_thumb_path = row.get("card_thumb_path").and_then(|v| v.as_str());
        let transferred = true;
        let has_transcript = clip_has_transcript(&conn, &clip_id).unwrap_or(false);
        let transcript_status: String = conn
            .query_row(
                "SELECT status FROM clip_transcripts WHERE clip_id = ?1",
                rusqlite::params![clip_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "none".to_string());
        let duration = row
            .get("duration_sec")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let fps = row.get("fps").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let name = row
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&clip_id)
            .to_string();
        let mut clip = json!({
            "clip_id": clip_id,
            "name": name,
            "discovered": true,
            "validated": transferred,
            "transferred": transferred,
            "has_transcript": has_transcript,
            "transcript_status": transcript_status,
            "proxy_path": proxy_path,
            "thumb_path": thumb_path,
            "source_path": source_path,
            "original_path": original_path,
            "card_thumb_path": card_thumb_path,
            "duration_sec": duration,
            "fps": fps,
        });
        sync_filmstrip_from_disk(paths, project_id, &clip_id, duration).ok();
        if let Some(fs) = get_filmstrip(paths, project_id, &clip_id) {
            let st = fs
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("missing");
            if let Some(obj) = clip.as_object_mut() {
                obj.insert("filmstrip_status".into(), json!(st));
                if let Some(err) = fs
                    .get("error")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    obj.insert("filmstrip_error".into(), json!(err));
                }
                if st == "ready" {
                    if let Some(dur) = fs.get("duration_sec") {
                        obj.insert("timeline_duration_sec".into(), dur.clone());
                    }
                    let frames =
                        list_frames_for_clip(paths, project_id, &clip_id).unwrap_or_default();
                    if !frames.is_empty() {
                        let seeks: Vec<Value> = frames
                            .iter()
                            .map(|f| {
                                json!(f.get("seek_sec").and_then(|v| v.as_f64()).unwrap_or(0.0))
                            })
                            .collect();
                        obj.insert("timeline_seeks".into(), json!(seeks));
                        let filmstrip_frames: Vec<Value> = pad_frames_to_default(
                            frames
                                .iter()
                                .map(|f| {
                                    let index =
                                        f.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                                    json!({
                                        "frame_index": index,
                                        "seek_sec": f.get("seek_sec").and_then(|v| v.as_f64()).unwrap_or(0.0),
                                        "url": format!(
                                            "/api/story/thumbnail?clip_id={}&frame_index={}&project_id={}",
                                            url_encode(&clip_id),
                                            index,
                                            url_encode(project_id)
                                        ),
                                    })
                                })
                                .collect(),
                        );
                        obj.insert("filmstrip_frames".into(), json!(filmstrip_frames));
                    }
                }
            }
        } else if let Some(obj) = clip.as_object_mut() {
            obj.insert("filmstrip_status".into(), json!("missing"));
            obj.insert(
                "filmstrip_frames".into(),
                json!(pad_frames_to_default(Vec::new())),
            );
        }
        if let Some(obj) = clip.as_object_mut() {
            obj.insert(
                "waveform".into(),
                waveform_snapshot(paths, project_id, &clip_id),
            );
        }
        clips.push(clip);
    }
    Ok(json!({
        "clips": clips,
        "summary": pool_summary(&clips),
        "import_pending_count": pending_import_count(paths, project_id)?,
    }))
}

#[allow(dead_code)]
fn clip_signature(clip: &Value) -> String {
    let frames = clip.get("filmstrip_frames").cloned().unwrap_or(json!([]));
    let seeks = clip.get("timeline_seeks").cloned().unwrap_or(json!([]));
    serde_json::to_string(&json!({
        "clip_id": clip.get("clip_id").cloned().unwrap_or(json!("")),
        "name": clip.get("name").cloned().unwrap_or(json!("")),
        "transferred": clip.get("transferred").cloned().unwrap_or(json!(false)),
        "has_transcript": clip.get("has_transcript").cloned().unwrap_or(json!(false)),
        "transcript_status": clip.get("transcript_status").cloned().unwrap_or(json!("")),
        "filmstrip_status": clip.get("filmstrip_status").cloned().unwrap_or(json!("missing")),
        "filmstrip_error": clip.get("filmstrip_error").cloned().unwrap_or(json!("")),
        "waveform": clip.get("waveform").cloned().unwrap_or(json!({})),
        "fps": clip.get("fps").cloned().unwrap_or(json!(0)),
        "timeline_duration_sec": clip.get("timeline_duration_sec").cloned().unwrap_or(json!(0)),
        "timeline_seeks": seeks,
        "filmstrip_frames": frames,
    }))
    .unwrap_or_default()
}

#[allow(dead_code)]
pub fn mark_displayed_clips(
    paths: &ProjectPaths,
    project_id: &str,
    clips: &[Value],
) -> Result<(), String> {
    let conn = open_db(paths, project_id)?;
    let now = now_str();
    for clip in clips {
        let clip_id = clip
            .get("clip_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if clip_id.is_empty() {
            continue;
        }
        let signature = clip_signature(clip);
        conn.execute(
            "INSERT INTO media_pool_displayed_clips
                (clip_id, snapshot_signature, displayed_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(clip_id) DO UPDATE SET
                snapshot_signature = excluded.snapshot_signature,
                displayed_at = excluded.displayed_at,
                updated_at = excluded.updated_at",
            rusqlite::params![clip_id, signature, now],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[allow(dead_code)]
pub fn list_incremental_updates(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    let data = list_clips_enriched(paths, project_id)?;
    let clips = data
        .get("clips")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let conn = open_db(paths, project_id)?;
    let existing: Vec<(String, String)> = conn
        .prepare("SELECT clip_id, snapshot_signature FROM media_pool_displayed_clips")
        .map_err(|e| e.to_string())?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let existing_map: std::collections::HashMap<String, String> = existing.into_iter().collect();
    let current_ids: std::collections::HashSet<String> = clips
        .iter()
        .filter_map(|clip| {
            clip.get("clip_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let mut changed = Vec::new();
    for clip in &clips {
        let clip_id = clip
            .get("clip_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if clip_id.is_empty() {
            continue;
        }
        let signature = clip_signature(clip);
        if existing_map.get(clip_id) != Some(&signature) {
            changed.push(clip.clone());
        }
    }
    let removed: Vec<String> = existing_map
        .keys()
        .filter(|id| !current_ids.contains(*id))
        .cloned()
        .collect();
    for id in &removed {
        conn.execute(
            "DELETE FROM media_pool_displayed_clips WHERE clip_id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    }
    if !changed.is_empty() {
        mark_displayed_clips(paths, project_id, &changed)?;
    }
    Ok(json!({
        "project_id": project_id,
        "clips": changed,
        "removed_clip_ids": removed,
        "summary": data.get("summary").cloned().unwrap_or(json!({})),
        "import_pending_count": data
            .get("import_pending_count")
            .cloned()
            .unwrap_or(json!(0)),
    }))
}

pub fn mark_filmstrip_building(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<(), String> {
    crate::filmstrip::mark_filmstrip(paths, project_id, clip_id, "building", "")
}
