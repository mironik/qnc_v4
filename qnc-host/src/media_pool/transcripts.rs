use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::db::open_db;
use crate::project::db::{now_str, ProjectPaths};

pub fn clip_has_transcript(conn: &Connection, clip_id: &str) -> Result<bool, String> {
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM clip_transcripts WHERE clip_id = ?1",
            params![clip_id],
            |r| r.get(0),
        )
        .ok();
    Ok(status.as_deref() == Some("complete"))
}

pub fn get_transcript(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<Option<Value>, String> {
    let conn = open_db(paths, project_id)?;
    get_transcript_conn(&conn, clip_id)
}

fn get_transcript_conn(conn: &Connection, clip_id: &str) -> Result<Option<Value>, String> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT status, text_body, COALESCE(transcript_json, '{}')
             FROM clip_transcripts WHERE clip_id = ?1",
            params![clip_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((status, text, transcript_json)) = row else {
        return Ok(None);
    };
    if status != "complete" {
        return Ok(None);
    }
    if let Ok(transcript) = serde_json::from_str::<Value>(&transcript_json) {
        if transcript
            .get("segments")
            .and_then(Value::as_array)
            .is_some()
        {
            return Ok(Some(transcript));
        }
    }
    let mut stmt = conn
        .prepare(
            "SELECT start_sec, end_sec, text FROM clip_transcript_segments
             WHERE clip_id = ?1 ORDER BY segment_index",
        )
        .map_err(|e| e.to_string())?;
    let segments: Vec<Value> = stmt
        .query_map(params![clip_id], |r| {
            Ok(json!({
                "start": r.get::<_, f64>(0)?,
                "end": r.get::<_, f64>(1)?,
                "text": r.get::<_, String>(2)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(Some(json!({
        "text": text,
        "segments": segments,
    })))
}

pub fn save_transcript(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    status: &str,
    transcript: &Value,
) -> Result<Value, String> {
    let conn = open_db(paths, project_id)?;
    save_transcript_conn(&conn, clip_id, status, transcript)
}

fn save_transcript_conn(
    conn: &Connection,
    clip_id: &str,
    status: &str,
    transcript: &Value,
) -> Result<Value, String> {
    let now = now_str();
    let text = transcript
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let transcript_json = transcript.to_string();
    conn.execute(
        "INSERT INTO clip_transcripts
            (clip_id, status, text_body, transcript_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(clip_id) DO UPDATE SET
           status = excluded.status,
           text_body = excluded.text_body,
           transcript_json = excluded.transcript_json,
           updated_at = excluded.updated_at",
        params![clip_id, status, text, transcript_json, now],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM clip_transcript_segments WHERE clip_id = ?1",
        params![clip_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(segments) = transcript.get("segments").and_then(|v| v.as_array()) {
        for (idx, seg) in segments.iter().enumerate() {
            let start = seg.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let end = seg.get("end").and_then(|v| v.as_f64()).unwrap_or(start);
            let seg_text = seg.get("text").and_then(|v| v.as_str()).unwrap_or("");
            conn.execute(
                "INSERT INTO clip_transcript_segments (clip_id, segment_index, start_sec, end_sec, text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![clip_id, idx as i64, start, end, seg_text],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(json!({
        "status": status,
        "clip_id": clip_id,
        "has_transcript": status == "complete",
    }))
}

pub fn migrate_transcript_files(
    conn: &Connection,
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<(), String> {
    let project_dir = paths.project_dir(project_id);
    let dir = project_dir.join("transcripts");
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM clip_transcripts WHERE clip_id = ?1",
                params![name],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if exists > 0 {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let parsed = serde_json::from_str::<Value>(&raw).unwrap_or(json!({}));
        save_transcript_conn(conn, name, "complete", &parsed)?;
    }
    quarantine_legacy_transcript_dir(&project_dir, &dir)?;
    Ok(())
}

fn quarantine_legacy_transcript_dir(project_dir: &Path, dir: &Path) -> Result<(), String> {
    let target = legacy_transcript_quarantine_path(project_dir);
    fs::rename(dir, &target).map_err(|error| {
        format!(
            "legacy transcript sidecar imported to DB, but quarantine failed: {} -> {} ({error})",
            dir.display(),
            target.display()
        )
    })
}

fn legacy_transcript_quarantine_path(project_dir: &Path) -> PathBuf {
    let stamp = now_str();
    let pid = std::process::id();
    project_dir.join(format!("transcripts.legacy_imported.{stamp}.{pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_round_trip_preserves_translation_metadata() {
        let base = std::env::temp_dir().join(format!(
            "qnc_transcript_metadata_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let paths = ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        };
        let transcript = json!({
            "text": "Dobar dan.",
            "source_text": "Good afternoon.",
            "language": "hr",
            "source_language": "en",
            "translated": true,
            "translation_model": "translategemma:4b",
            "segments": [{
                "start": 1.0,
                "end": 2.0,
                "text": "Dobar dan.",
                "source_text": "Good afternoon."
            }]
        });
        save_transcript(&paths, "project", "clip", "complete", &transcript).unwrap();
        let loaded = get_transcript(&paths, "project", "clip")
            .unwrap()
            .expect("saved transcript");
        assert_eq!(loaded, transcript);
        assert!(
            !paths.project_dir("project").join("transcripts").exists(),
            "DB-first transcript save must not create a project sidecar"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn legacy_transcript_sidecar_is_imported_then_quarantined() {
        let base = std::env::temp_dir().join(format!(
            "qnc_transcript_legacy_import_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let paths = ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        };
        let project_dir = paths.project_dir("project");
        let legacy_dir = project_dir.join("transcripts");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("clip.json"),
            r#"{"text":"Legacy tekst","segments":[{"start":0.0,"end":1.0,"text":"Legacy tekst"}]}"#,
        )
        .unwrap();

        let _conn = crate::media_pool::db::open_db(&paths, "project").unwrap();
        let loaded = get_transcript(&paths, "project", "clip")
            .unwrap()
            .expect("imported transcript");
        assert_eq!(
            loaded.get("text").and_then(Value::as_str),
            Some("Legacy tekst")
        );
        assert!(
            !legacy_dir.exists(),
            "active legacy transcript sidecar must be removed from the project truth path"
        );
        let quarantined = std::fs::read_dir(&project_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("transcripts.legacy_imported.")
            });
        assert!(
            quarantined,
            "legacy transcript files should be quarantined, not active"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
