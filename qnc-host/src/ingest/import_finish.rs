//! Zajednički završetak uvoza jednog klipa — koriste import i proxy workeri.

use std::path::Path;

use rusqlite::params;

use crate::ingest::db::{mark_ingest_job_done, open_ingest, thumbnail_path};
use crate::ingest::proxy_source::{classify_tv_source, recipe_for_source};
use crate::ingest::thumb::probe_media;
use crate::project::db::{bump_project_data_revision, ProjectPaths};

pub fn complete_imported_clip(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    dest_or_link: &Path,
    asset_status: &str,
    read_from_card: bool,
    card_locked: bool,
    original_path: &str,
) -> Result<(), String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let project_proxy_path = if dest_or_link.is_file() {
        dest_or_link.to_string_lossy().to_string()
    } else {
        String::new()
    };
    let poster = thumbnail_path(paths, project_id, clip_id);
    let thumb_st = if poster.is_file() { "ready" } else { "pending" };
    let thumb_path = if poster.is_file() {
        poster.to_string_lossy().to_string()
    } else {
        String::new()
    };
    let probe = if dest_or_link.is_file() {
        probe_media(dest_or_link)
    } else {
        None
    };
    let (
        duration_sec,
        fps,
        resolution,
        codec,
        has_audio,
        audio_channels,
        field_order,
        interlaced,
        source_class,
        proxy_recipe,
    ) = probe
        .as_ref()
        .map(|p| {
            let source_class = classify_tv_source(p);
            let proxy_recipe = recipe_for_source(source_class);
            (
                p.duration_sec,
                p.fps,
                p.resolution.clone(),
                p.codec.clone(),
                p.has_audio,
                p.audio_channels,
                p.field_order.clone(),
                p.interlaced,
                source_class.label().to_string(),
                proxy_recipe.id().to_string(),
            )
        })
        .unwrap_or((
            0.0,
            0.0,
            String::new(),
            String::new(),
            false,
            0,
            String::new(),
            false,
            String::new(),
            String::new(),
        ));
    conn.execute(
        "UPDATE ingest_assets SET
            import_status = 'imported',
            status = ?3,
            thumb_status = ?4,
            thumb_error = '',
            project_proxy_path = ?5,
            original_path = ?13,
            thumb_path = CASE WHEN ?6 = '' THEN thumb_path ELSE ?6 END,
            read_from_card = ?7,
            card_locked = ?8,
            duration_sec = ?9,
            fps = ?10,
            resolution = ?11,
            codec = ?12,
            has_audio = ?14,
            audio_channels = ?15,
            field_order = ?16,
            interlaced = ?17,
            source_class = ?18,
            proxy_recipe = ?19,
            metadata_json = '{}'
         WHERE source_id = ?1 AND clip_id = ?2",
        params![
            source_id,
            clip_id,
            asset_status,
            thumb_st,
            project_proxy_path,
            thumb_path,
            if read_from_card { 1 } else { 0 },
            if card_locked { 1 } else { 0 },
            duration_sec,
            fps,
            resolution,
            codec,
            original_path,
            if has_audio { 1 } else { 0 },
            audio_channels,
            field_order,
            if interlaced { 1 } else { 0 },
            source_class,
            proxy_recipe,
        ],
    )
    .map_err(|e| e.to_string())?;
    mark_ingest_job_done(&conn, "import", source_id, clip_id).map_err(|e| e.to_string())?;
    bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
    Ok(())
}
