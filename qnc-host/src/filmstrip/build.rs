use std::path::{Path, PathBuf};

use crate::ingest::thumb::{
    extract_filmstrip_batch_at_seeks, extract_poster_jpeg_at_seek_cpu, media_duration_sec,
    timeline_seek_seconds,
};
use crate::project::db::ProjectPaths;

use super::store::{
    filmstrip_clip_dir, get_filmstrip, list_frames_for_clip, mark_filmstrip, save_filmstrip,
    sync_filmstrip_from_disk, FilmstripFrame,
};

fn output_path(out_dir: &Path, index: usize, sec: f64) -> PathBuf {
    let sec_label = format!("{:.2}", sec).replace('.', "_");
    out_dir.join(format!("{:03}_{}.jpg", index, sec_label))
}

fn frame_ready(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0
}

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

/// Gradi filmstrip za klip: 13 vremenskih cjelina, prvi JPG svake cjeline.
pub fn build_for_clip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    media: &Path,
    frames: u32,
) -> Result<(), String> {
    if !media.is_file() {
        let msg = format!("nema medija za klip '{clip_id}'");
        mark_filmstrip(paths, project_id, clip_id, "error", &msg)?;
        return Err(msg);
    }

    let existing = get_filmstrip(paths, project_id, clip_id);
    let existing_duration = existing
        .as_ref()
        .and_then(|v| v.get("duration_sec"))
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0);
    let Some(duration) = media_duration_sec(media)
        .or(existing_duration)
        .filter(|value| *value > 0.0)
    else {
        let msg = format!("filmstrip: trajanje nije potvrdeno za klip '{clip_id}'");
        mark_filmstrip(paths, project_id, clip_id, "error", &msg)?;
        return Err(msg);
    };
    let seeks = timeline_seek_seconds(duration, frames);

    if let Some(existing) = existing.as_ref() {
        let status = existing
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if status == "ready" {
            let db_frames = list_frames_for_clip(paths, project_id, clip_id).unwrap_or_default();
            if stored_frames_match_seeks(&db_frames, &seeks) {
                return Ok(());
            }
        }
    }

    if sync_filmstrip_from_disk(paths, project_id, clip_id, duration)? {
        let db_frames = list_frames_for_clip(paths, project_id, clip_id).unwrap_or_default();
        if stored_frames_match_seeks(&db_frames, &seeks) {
            return Ok(());
        }
    }

    mark_filmstrip(paths, project_id, clip_id, "building", "")?;
    let out_dir = filmstrip_clip_dir(paths, project_id, clip_id);
    let output_paths: Vec<PathBuf> = seeks
        .iter()
        .enumerate()
        .map(|(index, sec)| output_path(&out_dir, index, *sec))
        .collect();

    let mut errors = Vec::new();
    let mut built_frames: Vec<FilmstripFrame> = Vec::new();

    let missing: Vec<usize> = (0..seeks.len())
        .filter(|&i| !frame_ready(&output_paths[i]))
        .collect();

    if !missing.is_empty() {
        let batch_seeks: Vec<f64> = missing.iter().map(|&i| seeks[i]).collect();
        let batch_outs: Vec<PathBuf> = missing.iter().map(|&i| output_paths[i].clone()).collect();
        let batch_results = extract_filmstrip_batch_at_seeks(media, &batch_seeks, &batch_outs);
        for (slot, result) in missing.iter().zip(batch_results.into_iter()) {
            let index = *slot;
            let sec = seeks[index];
            let out = &output_paths[index];
            let ok = match result {
                Ok(()) if frame_ready(out) => true,
                _ => extract_poster_jpeg_at_seek_cpu(media, out, sec).is_ok() && frame_ready(out),
            };
            if ok {
                built_frames.push(FilmstripFrame {
                    index,
                    seek_sec: sec,
                    path: out.clone(),
                });
            } else {
                errors.push(format!("{sec}s: frame missing"));
            }
        }
    }

    for (index, sec) in seeks.iter().enumerate() {
        if frame_ready(&output_paths[index]) && !built_frames.iter().any(|f| f.index == index) {
            built_frames.push(FilmstripFrame {
                index,
                seek_sec: *sec,
                path: output_paths[index].clone(),
            });
        }
    }
    built_frames.sort_by_key(|f| f.index);

    let err_msg = errors.join("; ");
    save_filmstrip(
        paths,
        project_id,
        clip_id,
        duration,
        &built_frames,
        &err_msg,
    )?;
    if built_frames.is_empty() {
        return Err(if err_msg.is_empty() {
            "filmstrip: nema kadrova".into()
        } else {
            err_msg
        });
    }
    Ok(())
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
