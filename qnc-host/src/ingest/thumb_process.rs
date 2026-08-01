use std::path::{Path, PathBuf};

use serde_json::json;
use tracing::info;

use crate::media::{find_card_poster_copy, proxy_poster_source_path, CardPosterKind};
use crate::project::db::ProjectPaths;

use super::asset_row::IngestAssetRow;
use super::db::{
    copy_card_image_to_poster, ensure_ingest_dirs, get_meta, ingest_asset_meta,
    mark_ingest_job_done, mark_ingest_job_error, mark_ingest_job_processing, open_ingest,
    poster_exists, queue_ingest_job, set_thumb_ready_path, set_thumb_status, thumbnail_path,
};
use super::store::reconcile_thumbnail_rows;
use super::thumb::extract_poster_jpeg;

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
    project_id: &str,
) -> Result<CardThumbCopyResult, String> {
    ensure_ingest_dirs(paths, project_id).map_err(|e| e.to_string())?;
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    reconcile_thumbnail_rows(paths, project_id, &conn).map_err(|e| e.to_string())?;

    let card_root_raw = get_meta(&conn, "card_root", "").unwrap_or_default();
    let card_root = if card_root_raw.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(card_root_raw.trim()))
    };

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
    let rows: Vec<IngestAssetRow> = stmt
        .query_map([], IngestAssetRow::from_row)
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut copied = 0usize;
    let mut no_thumb = Vec::new();

    for row in rows {
        queue_ingest_job(&conn, "thumb_card", &row.source_id, &row.clip_id)
            .map_err(|e| e.to_string())?;
        mark_ingest_job_processing(&conn, "thumb_card", &row.source_id, &row.clip_id)
            .map_err(|e| e.to_string())?;
        if row.status == "processing" {
            set_thumb_status(&conn, &row.source_id, &row.clip_id, "pending", "")
                .map_err(|e| e.to_string())?;
        }

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
                .unwrap_or("");
            let poster_src = meta
                .get("poster_source")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
                    thumbnail_path(paths, project_id, &row.clip_id).to_string_lossy(),
                    card_thumb,
                    poster_src,
                ],
            )
            .map_err(|e| e.to_string())?;
            mark_ingest_job_done(&conn, "thumb_card", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            copied += 1;
        } else {
            set_thumb_status(&conn, &row.source_id, &row.clip_id, "no_card_thumb", "")
                .map_err(|e| e.to_string())?;
            mark_ingest_job_done(&conn, "thumb_card", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            no_thumb.push(row.clip_id);
        }
    }

    Ok(CardThumbCopyResult {
        copied,
        no_thumb_clip_ids: no_thumb,
    })
}

/// Proces 2: generiraj poster iz proxya — samo ako na kartici nema THM/JPG.
pub fn generate_thumbs_from_proxy(
    paths: &ProjectPaths,
    project_id: &str,
    clip_ids: &[String],
) -> Result<usize, String> {
    ensure_ingest_dirs(paths, project_id).map_err(|e| e.to_string())?;
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;

    let card_root_raw = get_meta(&conn, "card_root", "").unwrap_or_default();
    let card_root = if card_root_raw.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(card_root_raw.trim()))
    };

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
    let rows: Vec<IngestAssetRow> = stmt
        .query_map([], IngestAssetRow::from_row)
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut done = 0usize;
    for row in rows {
        if let Some(ref ids) = filter {
            if !ids.contains(&row.clip_id) {
                continue;
            }
        }
        queue_ingest_job(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
            .map_err(|e| e.to_string())?;

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
                .unwrap_or("");
            let poster_src = meta
                .get("poster_source")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
                    thumbnail_path(paths, project_id, &row.clip_id).to_string_lossy(),
                    card_thumb,
                    poster_src,
                ],
            )
            .map_err(|e| e.to_string())?;
            mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            done += 1;
            continue;
        }

        if poster_exists(&poster) {
            set_thumb_ready_path(&conn, &row.source_id, &row.clip_id, &poster)
                .map_err(|e| e.to_string())?;
            mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            done += 1;
            continue;
        }

        set_thumb_status(&conn, &row.source_id, &row.clip_id, "processing", "")
            .map_err(|e| e.to_string())?;
        mark_ingest_job_processing(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
            .map_err(|e| e.to_string())?;

        let proxy = proxy_poster_source_path(&meta);

        let result = match proxy {
            Some(video) => {
                info!(
                    "ingest proxy thumb ffmpeg: clip={} from={}",
                    row.clip_id,
                    video.display()
                );
                extract_poster_jpeg(&video, &poster)
            }
            None => Err("proxy nije pronađen na kartici".into()),
        };

        match result {
            Ok(()) if poster_exists(&poster) => {
                conn.execute(
                    "UPDATE ingest_assets SET
                        thumb_status = 'ready',
                        thumb_error = '',
                        thumb_path = ?3,
                        poster_source = 'proxy_ffmpeg',
                        metadata_json = '{}'
                     WHERE source_id = ?1 AND clip_id = ?2",
                    rusqlite::params![row.source_id, row.clip_id, poster.to_string_lossy()],
                )
                .map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "thumb_proxy", &row.source_id, &row.clip_id)
                    .map_err(|e| e.to_string())?;
                done += 1;
            }
            Ok(()) => {
                let msg = "poster nije kreiran iz proxya";
                set_thumb_status(&conn, &row.source_id, &row.clip_id, "error", msg)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_error(&conn, "thumb_proxy", &row.source_id, &row.clip_id, msg)
                    .map_err(|e| e.to_string())?;
            }
            Err(err) => {
                let msg = if err.len() > 240 {
                    format!("{}…", err.chars().take(240).collect::<String>())
                } else {
                    err
                };
                set_thumb_status(&conn, &row.source_id, &row.clip_id, "error", &msg)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_error(&conn, "thumb_proxy", &row.source_id, &row.clip_id, &msg)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(done)
}
