use std::path::{Path, PathBuf};
use std::sync::Arc;

use qnc_service_contracts::{
    MediaLocator, MediaProcessor, MediaRef, PosterExtractRequest, ServiceError,
};
use serde_json::json;
use tracing::info;

use crate::media::{find_card_poster_copy, proxy_poster_source_path, CardPosterKind};
use crate::project::{db::ProjectPaths, ProjectDbBroker};

use super::asset_row::IngestAssetRow;
use super::db::{
    copy_card_image_to_poster, ensure_ingest_dirs, get_meta, ingest_asset_meta,
    mark_ingest_job_done, mark_ingest_job_error, mark_ingest_job_processing, open_ingest,
    poster_exists, queue_ingest_job, set_thumb_ready_path, set_thumb_status, thumbnail_path,
};
use super::store::reconcile_thumbnail_rows;

pub struct CardThumbCopyResult {
    pub copied: usize,
    pub no_thumb_clip_ids: Vec<String>,
}

fn poster_source_label(kind: CardPosterKind) -> &'static str {
    match kind {
        CardPosterKind::Thm => "card_thm",
        CardPosterKind::Jpg => "card_jpg",
    }
}

/// Kopira THM/JPG s kartice u ingest poster ako postoji. Vraća true kad je poster spreman.
pub fn apply_card_poster_copy(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    meta: &mut serde_json::Value,
    card_root: Option<&Path>,
) -> bool {
    let poster = thumbnail_path(paths, project_id, clip_id);
    if poster_exists(&poster) {
        return true;
    }
    let found = find_card_poster_copy(meta, card_root);
    if let Some((img, kind)) = found {
        info!(
            "ingest card thumb copy: clip={} from={}",
            clip_id,
            img.display()
        );
        if copy_card_image_to_poster(&img, &poster).is_ok() && poster_exists(&poster) {
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("card_thumb_path".into(), json!(img.to_string_lossy()));
                obj.insert("poster_source".into(), json!(poster_source_label(kind)));
            }
            return true;
        }
    }
    false
}

/// Proces 1: THM → JPG na kartici; samo kopija, bez ffmpeg.
pub fn copy_thumbs_from_card(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
) -> Result<CardThumbCopyResult, String> {
    ensure_ingest_dirs(paths, project_id).map_err(|e| e.to_string())?;
    let (card_root_raw, rows): (String, Vec<IngestAssetRow>) =
        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            reconcile_thumbnail_rows(paths, project_id, &conn).map_err(|e| e.to_string())?;
            let card_root_raw = get_meta(&conn, "card_root", "").unwrap_or_default();
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                            project_proxy_path, card_thumb_path, file_extension,
                            read_from_card, card_locked, poster_source, thumb_status
                     FROM ingest_assets
                     WHERE thumb_status NOT IN ('ready')
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], IngestAssetRow::from_row)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok((card_root_raw, rows))
        })?;
    let card_root = if card_root_raw.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(card_root_raw.trim()))
    };

    let mut copied = 0usize;
    let mut no_thumb = Vec::new();

    for row in rows {
        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            queue_ingest_job(&conn, "thumb_card", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            mark_ingest_job_processing(&conn, "thumb_card", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            if row.status == "processing" {
                set_thumb_status(&conn, &row.source_id, &row.clip_id, "pending", "")
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })?;

        let mut meta = ingest_asset_meta(&row.meta_input());

        if apply_card_poster_copy(
            paths,
            project_id,
            &row.clip_id,
            &mut meta,
            card_root.as_deref(),
        ) {
            let card_thumb = meta
                .get("card_thumb_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let poster_src = meta
                .get("poster_source")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thumb_path = thumbnail_path(paths, project_id, &row.clip_id)
                .to_string_lossy()
                .to_string();
            project_db.serialize_project_write(project_id, || {
                let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                conn.execute(
                    "UPDATE ingest_assets SET
                        thumb_status = 'ready',
                        thumb_error = '',
                        thumb_path = ?3,
                        card_thumb_path = ?4,
                        poster_source = ?5,
                        metadata_json = '{}'
                     WHERE source_id = ?1 AND clip_id = ?2",
                    rusqlite::params![
                        row.source_id,
                        row.clip_id,
                        thumb_path,
                        card_thumb,
                        poster_src,
                    ],
                )
                .map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "thumb_card", &row.source_id, &row.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            copied += 1;
        } else {
            project_db.serialize_project_write(project_id, || {
                let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                set_thumb_status(&conn, &row.source_id, &row.clip_id, "no_card_thumb", "")
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "thumb_card", &row.source_id, &row.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            no_thumb.push(row.clip_id);
        }
    }

    Ok(CardThumbCopyResult {
        copied,
        no_thumb_clip_ids: no_thumb,
    })
}

/// Proces 2: generiraj poster iz proxya — samo ako na kartici nema THM/JPG.
pub async fn generate_thumbs_from_proxy(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_processor: Arc<dyn MediaProcessor>,
    project_id: &str,
    clip_ids: &[String],
) -> Result<usize, String> {
    ensure_ingest_dirs(paths, project_id).map_err(|e| e.to_string())?;
    let filter: Option<Vec<String>> = if clip_ids.is_empty() {
        None
    } else {
        Some(
            clip_ids
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
    };

    let (card_root_raw, rows): (String, Vec<IngestAssetRow>) =
        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            let card_root_raw = get_meta(&conn, "card_root", "").unwrap_or_default();
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                            project_proxy_path, card_thumb_path, file_extension,
                            read_from_card, card_locked, poster_source, thumb_status
                     FROM ingest_assets
                     WHERE thumb_status IN ('no_card_thumb', 'pending', 'error')
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], IngestAssetRow::from_row)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok((card_root_raw, rows))
        })?;
    let card_root = if card_root_raw.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(card_root_raw.trim()))
    };

    let mut done = 0usize;
    for row in rows {
        if let Some(ref ids) = filter {
            if !ids.contains(&row.clip_id) {
                continue;
            }
        }
        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            queue_ingest_job(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            Ok(())
        })?;

        let poster = thumbnail_path(paths, project_id, &row.clip_id);
        let mut meta = ingest_asset_meta(&row.meta_input());

        if apply_card_poster_copy(
            paths,
            project_id,
            &row.clip_id,
            &mut meta,
            card_root.as_deref(),
        ) {
            let card_thumb = meta
                .get("card_thumb_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let poster_src = meta
                .get("poster_source")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thumb_path = thumbnail_path(paths, project_id, &row.clip_id)
                .to_string_lossy()
                .to_string();
            project_db.serialize_project_write(project_id, || {
                let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                conn.execute(
                    "UPDATE ingest_assets SET
                        thumb_status = 'ready',
                        thumb_error = '',
                        thumb_path = ?3,
                        card_thumb_path = ?4,
                        poster_source = ?5,
                        metadata_json = '{}'
                     WHERE source_id = ?1 AND clip_id = ?2",
                    rusqlite::params![
                        row.source_id,
                        row.clip_id,
                        thumb_path,
                        card_thumb,
                        poster_src,
                    ],
                )
                .map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            done += 1;
            continue;
        }

        if poster_exists(&poster) {
            project_db.serialize_project_write(project_id, || {
                let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                set_thumb_ready_path(&conn, &row.source_id, &row.clip_id, &poster)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            done += 1;
            continue;
        }

        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            set_thumb_status(&conn, &row.source_id, &row.clip_id, "processing", "")
                .map_err(|e| e.to_string())?;
            mark_ingest_job_processing(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            Ok(())
        })?;

        let proxy = proxy_poster_source_path(&meta);

        let result = match proxy {
            Some(video) => {
                info!(
                    "ingest proxy thumb extract: clip={} from={}",
                    row.clip_id,
                    video.display()
                );
                extract_proxy_poster(
                    media_processor.clone(),
                    row.clip_id.as_str(),
                    &video,
                    &poster,
                )
                .await
            }
            None => Err("proxy nije pronađen na kartici".into()),
        };

        match result {
            Ok(()) if poster_exists(&poster) => {
                let thumb_path = poster.to_string_lossy().to_string();
                project_db.serialize_project_write(project_id, || {
                    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                    conn.execute(
                        "UPDATE ingest_assets SET
                            thumb_status = 'ready',
                            thumb_error = '',
                            thumb_path = ?3,
                            poster_source = 'proxy_ffmpeg',
                            metadata_json = '{}'
                         WHERE source_id = ?1 AND clip_id = ?2",
                        rusqlite::params![row.source_id, row.clip_id, thumb_path],
                    )
                    .map_err(|e| e.to_string())?;
                    mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })?;
                done += 1;
            }
            Ok(()) => {
                let msg = "poster nije kreiran iz proxya";
                project_db.serialize_project_write(project_id, || {
                    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                    set_thumb_status(&conn, &row.source_id, &row.clip_id, "error", msg)
                        .map_err(|e| e.to_string())?;
                    mark_ingest_job_error(&conn, "thumb_proxy", &row.source_id, &row.clip_id, msg)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })?;
            }
            Err(err) => {
                let msg = if err.len() > 240 {
                    format!("{}…", err.chars().take(240).collect::<String>())
                } else {
                    err
                };
                project_db.serialize_project_write(project_id, || {
                    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
                    set_thumb_status(&conn, &row.source_id, &row.clip_id, "error", &msg)
                        .map_err(|e| e.to_string())?;
                    mark_ingest_job_error(&conn, "thumb_proxy", &row.source_id, &row.clip_id, &msg)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })?;
            }
        }
    }

    Ok(done)
}

async fn extract_proxy_poster(
    media_processor: Arc<dyn MediaProcessor>,
    clip_id: &str,
    video: &Path,
    poster: &Path,
) -> Result<(), String> {
    media_processor
        .extract_poster(PosterExtractRequest {
            input: MediaRef {
                clip_id: clip_id.to_string(),
                locator: MediaLocator::LocalPath {
                    path: video.to_path_buf(),
                },
            },
            output_path: poster.to_path_buf(),
            seek_sec: 0.5,
        })
        .await
        .map(|_| ())
        .map_err(service_error_message)
}

fn service_error_message(error: ServiceError) -> String {
    format!("{}: {}", error.code, error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use qnc_service_contracts::{
        ArtifactRef, AudioProbe, AudioProbeRequest, AudioWrapRequest, ExtractRangeRequest,
        FilmstripFrameArtifact, FilmstripRequest, FrameExtractRequest, FrameTimebase, MediaProbe,
        ProxyBuildRequest, ScanMode, ServiceResult, WaveformPeaks, WaveformRequest,
    };
    use rusqlite::params;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn test_base(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qnc_thumb_process_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[derive(Default)]
    struct FakePosterProcessor {
        poster_calls: AtomicUsize,
    }

    #[async_trait]
    impl MediaProcessor for FakePosterProcessor {
        async fn probe(&self, _input: &MediaRef) -> ServiceResult<MediaProbe> {
            Ok(MediaProbe {
                width: 1920,
                height: 1080,
                duration_sec: Some(10.0),
                timebase: FrameTimebase::new(50, 1).unwrap(),
                scan_mode: ScanMode::Progressive,
                codec: "h264".into(),
                field_order: "progressive".into(),
                frame_count: Some(500),
                duration_frames: Some(500),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
            })
        }

        async fn probe_audio(&self, _request: AudioProbeRequest) -> ServiceResult<AudioProbe> {
            Err(unused_service_error())
        }

        async fn extract_frame(&self, _request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn extract_poster(
            &self,
            request: PosterExtractRequest,
        ) -> ServiceResult<ArtifactRef> {
            self.poster_calls.fetch_add(1, Ordering::AcqRel);
            if let Some(parent) = request.output_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
            }
            fs::write(&request.output_path, b"poster")
                .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
            Ok(ArtifactRef {
                path: request.output_path,
                media_type: "image/jpeg".into(),
                render_version: Some("test".into()),
            })
        }

        async fn build_filmstrip(
            &self,
            _request: FilmstripRequest,
        ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
            Err(unused_service_error())
        }

        async fn build_proxy(&self, _request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_audio_wrap(&self, _request: AudioWrapRequest) -> ServiceResult<ArtifactRef> {
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

    #[tokio::test]
    async fn proxy_thumb_worker_prefers_existing_camera_thumb() {
        let base = test_base("camera_thumb");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_db = ProjectDbBroker::new(paths.clone());
        let project_id = "thumb_camera_project";
        let card = base.join("card");
        fs::create_dir_all(&card).unwrap();
        let source = card.join("clip_a.MXF");
        let camera_thumb = card.join("clip_a.THM");
        fs::write(&source, b"source").unwrap();
        fs::write(&camera_thumb, b"camera-thumb").unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 card_thumb_path, file_extension, thumb_status)
             VALUES ('card', 'clip_a', 'clip_a', 'clip_a', ?1, ?1, ?2, 'mxf', 'pending')",
            params![
                source.to_string_lossy().to_string(),
                camera_thumb.to_string_lossy().to_string()
            ],
        )
        .unwrap();
        drop(conn);

        let processor = Arc::new(FakePosterProcessor::default());
        let done =
            generate_thumbs_from_proxy(&paths, &project_db, processor.clone(), project_id, &[])
                .await
                .unwrap();

        assert_eq!(done, 1);
        assert_eq!(processor.poster_calls.load(Ordering::Acquire), 0);
        let poster = thumbnail_path(&paths, project_id, "clip_a");
        assert!(poster_exists(&poster));
        assert_eq!(fs::read(&poster).unwrap(), b"camera-thumb");

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (String, String, String) = conn
            .query_row(
                "SELECT thumb_status, poster_source, thumb_path
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "ready");
        assert_eq!(row.1, "card_thm");
        assert_eq!(row.2, poster.to_string_lossy());

        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn proxy_thumb_worker_uses_media_processor_when_camera_thumb_missing() {
        let base = test_base("proxy_thumb");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_db = ProjectDbBroker::new(paths.clone());
        let project_id = "thumb_proxy_project";
        let card = base.join("card");
        fs::create_dir_all(&card).unwrap();
        let source = card.join("clip_b.MXF");
        let proxy = card.join("clip_b.MP4");
        fs::write(&source, b"source").unwrap();
        fs::write(&proxy, b"proxy").unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 proxy_path, file_extension, thumb_status)
             VALUES ('card', 'clip_b', 'clip_b', 'clip_b', ?1, ?1, ?2, 'mxf', 'pending')",
            params![
                source.to_string_lossy().to_string(),
                proxy.to_string_lossy().to_string()
            ],
        )
        .unwrap();
        drop(conn);

        let processor = Arc::new(FakePosterProcessor::default());
        let done =
            generate_thumbs_from_proxy(&paths, &project_db, processor.clone(), project_id, &[])
                .await
                .unwrap();

        assert_eq!(done, 1);
        assert_eq!(processor.poster_calls.load(Ordering::Acquire), 1);
        let poster = thumbnail_path(&paths, project_id, "clip_b");
        assert!(poster_exists(&poster));

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (String, String, String) = conn
            .query_row(
                "SELECT thumb_status, poster_source, thumb_path
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_b'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "ready");
        assert_eq!(row.1, "proxy_ffmpeg");
        assert_eq!(row.2, poster.to_string_lossy());

        let _ = fs::remove_dir_all(&base);
    }
}
