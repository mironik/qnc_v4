use std::path::Path;
use std::sync::Arc;

use crate::ingest::thumb::timeline_seek_seconds;
use crate::project::db::ProjectPaths;
use qnc_service_contracts::{
    FilmstripRequest, MediaLocator, MediaProbe, MediaProcessor, MediaRef, ServiceError,
};

use super::store::{
    filmstrip_clip_dir, get_filmstrip, list_frames_for_clip, mark_filmstrip, save_filmstrip,
    sync_filmstrip_from_disk, FilmstripFrame,
};

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
pub async fn build_for_clip(
    paths: &ProjectPaths,
    media_processor: Arc<dyn MediaProcessor>,
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

    if let Some(duration) = existing_duration {
        let seeks = timeline_seek_seconds(duration, frames);
        if filmstrip_is_current(paths, project_id, clip_id, existing.as_ref(), &seeks) {
            return Ok(());
        }
    }

    let input = media_ref(clip_id, media);
    let probe = match media_processor.probe(&input).await {
        Ok(probe) => probe,
        Err(error) => {
            let msg = service_error_message(error);
            mark_filmstrip(paths, project_id, clip_id, "error", &msg)?;
            return Err(msg);
        }
    };
    let Some(duration) = duration_from_probe(&probe).or(existing_duration) else {
        let msg = format!("filmstrip: trajanje nije potvrdeno za klip '{clip_id}'");
        mark_filmstrip(paths, project_id, clip_id, "error", &msg)?;
        return Err(msg);
    };
    let seeks = timeline_seek_seconds(duration, frames);

    if filmstrip_is_current(paths, project_id, clip_id, existing.as_ref(), &seeks) {
        return Ok(());
    }

    if sync_filmstrip_from_disk(paths, project_id, clip_id, duration)? {
        let db_frames = list_frames_for_clip(paths, project_id, clip_id).unwrap_or_default();
        if stored_frames_match_seeks(&db_frames, &seeks) {
            return Ok(());
        }
    }

    mark_filmstrip(paths, project_id, clip_id, "building", "")?;
    let out_dir = filmstrip_clip_dir(paths, project_id, clip_id);
    let artifacts = match media_processor
        .build_filmstrip(FilmstripRequest {
            input,
            frame_count: frames as usize,
            output_dir: out_dir,
        })
        .await
    {
        Ok(artifacts) => artifacts,
        Err(error) => {
            let msg = service_error_message(error);
            mark_filmstrip(paths, project_id, clip_id, "error", &msg)?;
            return Err(msg);
        }
    };

    let mut built_frames: Vec<FilmstripFrame> = artifacts
        .into_iter()
        .filter_map(|frame| {
            let path = frame.artifact.path;
            frame_ready(&path).then(|| FilmstripFrame {
                index: frame.index,
                seek_sec: seeks.get(frame.index).copied().unwrap_or(frame.seek_sec),
                path,
            })
        })
        .collect();
    built_frames.sort_by_key(|f| f.index);

    save_filmstrip(paths, project_id, clip_id, duration, &built_frames, "")?;
    if built_frames.is_empty() {
        return Err("filmstrip: nema kadrova".into());
    }
    Ok(())
}

fn filmstrip_is_current(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    existing: Option<&serde_json::Value>,
    seeks: &[f64],
) -> bool {
    let Some(existing) = existing else {
        return false;
    };
    let status = existing
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if status != "ready" {
        return false;
    }
    let db_frames = list_frames_for_clip(paths, project_id, clip_id).unwrap_or_default();
    stored_frames_match_seeks(&db_frames, seeks)
}

fn media_ref(clip_id: &str, media: &Path) -> MediaRef {
    MediaRef {
        clip_id: clip_id.to_string(),
        locator: MediaLocator::LocalPath {
            path: media.to_path_buf(),
        },
    }
}

fn duration_from_probe(probe: &MediaProbe) -> Option<f64> {
    if let Some(duration) = probe
        .duration_sec
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        return Some(duration);
    }
    let frames = probe.duration_frames.or(probe.frame_count)?;
    let fps = probe.timebase.fps_num as f64 / probe.timebase.fps_den as f64;
    if frames > 0 && fps.is_finite() && fps > 0.0 {
        Some(frames as f64 / fps)
    } else {
        None
    }
}

fn service_error_message(error: ServiceError) -> String {
    format!("{}: {}", error.code, error.message)
}

#[cfg(test)]
mod tests {
    use super::{build_for_clip, stored_frames_match_seeks};
    use crate::ingest::thumb::timeline_seek_seconds;
    use crate::project::db::{
        ensure_project_dirs_at, open_global, open_project, project_dir_in_root, ProjectPaths,
    };
    use async_trait::async_trait;
    use qnc_service_contracts::{
        ArtifactRef, ExtractRangeRequest, FilmstripFrameArtifact, FilmstripRequest,
        FrameExtractRequest, FrameTimebase, MediaProbe, MediaProcessor, ProxyBuildRequest,
        ScanMode, ServiceError, ServiceResult, WaveformPeaks, WaveformRequest,
    };
    use rusqlite::params;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn register_project(paths: &ProjectPaths, project_id: &str) {
        let global = open_global(paths).unwrap();
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        global
            .execute(
                "INSERT INTO projects (project_id, name, project_dir)
                 VALUES (?1, ?2, ?3)",
                params![
                    project_id,
                    project_id,
                    project_dir.to_string_lossy().to_string()
                ],
            )
            .unwrap();
        ensure_project_dirs_at(&project_dir).unwrap();
        let _ = open_project(paths, project_id).unwrap();
    }

    #[derive(Default)]
    struct FakeMediaProcessor {
        filmstrip_calls: AtomicUsize,
    }

    #[async_trait]
    impl MediaProcessor for FakeMediaProcessor {
        async fn probe(
            &self,
            _input: &qnc_service_contracts::MediaRef,
        ) -> ServiceResult<MediaProbe> {
            Ok(MediaProbe {
                width: 1920,
                height: 1080,
                duration_sec: Some(13.0),
                timebase: FrameTimebase::new(50, 1).unwrap(),
                scan_mode: ScanMode::Progressive,
                codec: "h264".into(),
                field_order: "progressive".into(),
                frame_count: Some(650),
                duration_frames: Some(650),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
            })
        }

        async fn extract_frame(&self, _request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_filmstrip(
            &self,
            request: FilmstripRequest,
        ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
            self.filmstrip_calls.fetch_add(1, Ordering::AcqRel);
            fs::create_dir_all(&request.output_dir)
                .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
            let mut frames = Vec::new();
            for index in 0..request.frame_count {
                let path = request.output_dir.join(format!("{index:03}_fake.jpg"));
                fs::write(&path, b"jpeg")
                    .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
                frames.push(FilmstripFrameArtifact {
                    index,
                    seek_sec: index as f64,
                    artifact: ArtifactRef {
                        path,
                        media_type: "image/jpeg".into(),
                        render_version: None,
                    },
                });
            }
            Ok(frames)
        }

        async fn build_proxy(&self, _request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_waveform(&self, _request: WaveformRequest) -> ServiceResult<WaveformPeaks> {
            Err(unused_service_error())
        }

        async fn extract_range(&self, _request: ExtractRangeRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }
    }

    fn unused_service_error() -> ServiceError {
        ServiceError::new("unused", "unused in this test")
    }

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

    #[tokio::test]
    async fn build_for_clip_uses_media_processor_and_writes_sqlite_frames() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_adapter_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "test_proj";
        let clip_id = "clip_a";
        register_project(&paths, project_id);

        let media = base.join("source.mp4");
        fs::write(&media, b"fake-video").unwrap();
        let processor = Arc::new(FakeMediaProcessor::default());
        build_for_clip(&paths, processor.clone(), project_id, clip_id, &media, 13)
            .await
            .unwrap();

        let stored =
            super::super::store::list_frames_for_clip(&paths, project_id, clip_id).unwrap();
        assert_eq!(processor.filmstrip_calls.load(Ordering::Acquire), 1);
        assert_eq!(stored.len(), 13);
        assert_eq!(stored[0].get("index").and_then(|v| v.as_i64()), Some(0));
        assert_eq!(
            stored[1].get("seek_sec").and_then(|v| v.as_f64()),
            Some(1.0)
        );
        assert!(stored.iter().all(|frame| frame
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .map(|path| path.is_file())
            .unwrap_or(false)));
        let _ = fs::remove_dir_all(&base);
    }
}
