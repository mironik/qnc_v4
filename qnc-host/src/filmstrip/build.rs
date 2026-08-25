use std::path::Path;

use crate::project::db::ProjectPaths;
use crate::project::ProjectDbBroker;

use super::store::{
    manifest_cache_key, manifest_from_frames_for_api, manifest_from_status_for_api, mark_filmstrip,
    save_filmstrip, save_filmstrip_progress, FilmstripFrame,
};

pub(super) fn stored_frames_match_seeks(frames: &[serde_json::Value], seeks: &[f64]) -> bool {
    if frames.len() < seeks.len() {
        return false;
    }
    for (index, seek) in seeks.iter().enumerate() {
        let Some(frame) = frames
            .iter()
            .find(|frame| frame.get("index").and_then(|v| v.as_i64()) == Some(index as i64))
        else {
            return false;
        };
        let stored_seek = frame
            .get("seek_sec")
            .and_then(|v| v.as_f64())
            .unwrap_or(f64::NAN);
        if (stored_seek - seek).abs() > 0.011 {
            return false;
        }
        if !frame
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| Path::new(s).is_file())
            .unwrap_or(false)
        {
            return false;
        }
    }
    true
}

pub(crate) fn save_built_filmstrip_frames(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    duration: f64,
    frames: &[FilmstripFrame],
    seeks: &[f64],
) -> Result<(), String> {
    if frames.is_empty() {
        mark_status(
            paths,
            project_db,
            project_id,
            clip_id,
            "error",
            "filmstrip: nema kadrova",
        )?;
        return Err("filmstrip: nema kadrova".into());
    }
    if !built_frames_cover_seeks(frames, seeks) {
        let msg = format!(
            "filmstrip: nepotpun set kadrova {}/{}",
            frames.len(),
            seeks.len()
        );
        save_progress(
            paths, project_db, project_id, clip_id, duration, frames, &msg,
        )?;
        return Err(msg);
    }
    save_ready(paths, project_db, project_id, clip_id, duration, frames, "")
}

fn mark_status(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    status: &str,
    error: &str,
) -> Result<(), String> {
    project_db.serialize_project_write(project_id, || {
        mark_filmstrip(paths, project_id, clip_id, status, error)
    })?;
    publish_manifest_cache_value(
        project_db,
        project_id,
        clip_id,
        manifest_from_status_for_api(project_id, clip_id, status, error, "/api/story"),
    );
    Ok(())
}

fn save_progress(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    duration: f64,
    frames: &[FilmstripFrame],
    error: &str,
) -> Result<(), String> {
    let filmstrip = project_db.serialize_project_write(project_id, || {
        save_filmstrip_progress(paths, project_id, clip_id, duration, frames, error)
    })?;
    publish_manifest_cache_value(
        project_db,
        project_id,
        clip_id,
        manifest_from_frames_for_api(project_id, clip_id, filmstrip, frames, "/api/story"),
    );
    Ok(())
}

fn save_ready(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    duration: f64,
    frames: &[FilmstripFrame],
    error: &str,
) -> Result<(), String> {
    let filmstrip = project_db.serialize_project_write(project_id, || {
        save_filmstrip(paths, project_id, clip_id, duration, frames, error)
    })?;
    publish_manifest_cache_value(
        project_db,
        project_id,
        clip_id,
        manifest_from_frames_for_api(project_id, clip_id, filmstrip, frames, "/api/story"),
    );
    Ok(())
}

fn publish_manifest_cache_value(
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    manifest: serde_json::Value,
) {
    let api = "/api/story";
    project_db.put_runtime_cache(project_id, &manifest_cache_key(clip_id, api), manifest);
}

fn built_frames_cover_seeks(frames: &[FilmstripFrame], seeks: &[f64]) -> bool {
    !seeks.is_empty()
        && (0..seeks.len()).all(|index| {
            frames
                .iter()
                .any(|frame| frame.index == index && (frame.seek_sec - seeks[index]).abs() <= 0.011)
        })
}

#[cfg(test)]
mod tests {
    use super::stored_frames_match_seeks;
    use crate::ingest::thumb::timeline_seek_seconds;
    use serde_json::json;
    use std::fs;

    #[test]
    fn filmstrip_seeks_are_segment_starts() {
        let seeks = timeline_seek_seconds(130.0, 13);
        assert_eq!(
            seeks,
            vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0]
        );
    }

    #[test]
    fn filmstrip_seeks_do_not_use_timeline_margins() {
        let seeks = timeline_seek_seconds(26.0, 13);
        assert_eq!(seeks.first().copied(), Some(0.0));
        assert_eq!(seeks.get(1).copied(), Some(2.0));
        assert_eq!(seeks.last().copied(), Some(24.0));
    }

    #[test]
    fn stored_frames_reject_old_margin_positions() {
        let dir = std::env::temp_dir().join(format!(
            "qnc_filmstrip_build_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("000_1_30.jpg");
        fs::write(&path, b"jpg").unwrap();

        let frames = vec![json!({
            "index": 0,
            "seek_sec": 1.3,
            "path": path.to_string_lossy(),
        })];
        assert!(!stored_frames_match_seeks(
            &frames,
            &timeline_seek_seconds(26.0, 13)
        ));
        let _ = fs::remove_dir_all(dir);
    }
}
