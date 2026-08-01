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

/// Gradi filmstrip za klip — jedan ffmpeg batch pass, CPU per-frame fallback.
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

    if let Some(existing) = get_filmstrip(paths, project_id, clip_id) {
        let status = existing
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if status == "ready" {
            let db_frames = list_frames_for_clip(paths, project_id, clip_id).unwrap_or_default();
            let want = frames.max(crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES) as usize;
            let all_exist = db_frames.len() >= want
                && db_frames.iter().all(|f| {
                    f.get("path")
                        .and_then(|v| v.as_str())
                        .map(|s| Path::new(s).is_file())
                        .unwrap_or(false)
                });
            if all_exist {
                return Ok(());
            }
        }
    }

    let duration = media_duration_sec(media).unwrap_or(60.0);
    if sync_filmstrip_from_disk(paths, project_id, clip_id, duration)? {
        return Ok(());
    }

    mark_filmstrip(paths, project_id, clip_id, "building", "")?;
    let seeks = timeline_seek_seconds(duration, frames);
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
