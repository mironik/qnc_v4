//! Zajednički završetak uvoza jednog klipa — koriste import i proxy workeri.

use std::path::Path;

use rusqlite::params;

use crate::ingest::db::{mark_ingest_job_done, open_ingest, thumbnail_path};
use crate::ingest::proxy_source::{classify_tv_source, recipe_for_source};
use crate::ingest::thumb::MediaProbe;
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
    probe: Option<&MediaProbe>,
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
        fps_num,
        fps_den,
    ) = probe
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
                p.fps_num,
                p.fps_den,
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
            0,
            1,
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
            source_fps_num = ?20,
            source_fps_den = ?21,
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
            fps_num,
            fps_den,
        ],
    )
    .map_err(|e| e.to_string())?;
    mark_ingest_job_done(&conn, "import", source_id, clip_id).map_err(|e| e.to_string())?;
    bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::db::open_ingest;
    use std::fs;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    #[test]
    fn complete_imported_clip_persists_supplied_probe() {
        let base = std::env::temp_dir().join(format!(
            "qnc_import_finish_probe_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "import_finish_probe_project";
        let media = base.join("clip_a.mp4");
        fs::write(&media, b"media").unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, import_status)
             VALUES ('card', 'clip_a', 'clip_a', 'clip_a', 'processing')",
            [],
        )
        .unwrap();
        drop(conn);

        let probe = MediaProbe {
            duration_sec: 12.5,
            fps: 50.0,
            fps_num: 50,
            fps_den: 1,
            resolution: "1920x1080".into(),
            codec: "h264".into(),
            has_audio: true,
            audio_channels: 2,
            field_order: "progressive".into(),
            interlaced: false,
        };

        complete_imported_clip(
            &paths,
            project_id,
            "card",
            "clip_a",
            &media,
            "ready",
            false,
            false,
            "",
            Some(&probe),
        )
        .unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (
            String,
            f64,
            f64,
            String,
            String,
            i64,
            i64,
            String,
            String,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT import_status, duration_sec, fps, resolution, codec,
                        has_audio, audio_channels, source_class, proxy_recipe,
                        source_fps_num, source_fps_den
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row.0, "imported");
        assert!((row.1 - 12.5).abs() < 0.001);
        assert!((row.2 - 50.0).abs() < 0.001);
        assert_eq!(row.3, "1920x1080");
        assert_eq!(row.4, "h264");
        assert_eq!(row.5, 1);
        assert_eq!(row.6, 2);
        assert_eq!(row.7, "pal_50p");
        assert_eq!(row.8, "h264_native");
        assert_eq!(row.9, 50);
        assert_eq!(row.10, 1);

        let _ = fs::remove_dir_all(&base);
    }
}
