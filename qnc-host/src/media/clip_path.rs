use std::path::PathBuf;

use crate::ingest::db::open_ingest;
use crate::project::db::ProjectPaths;

pub fn first_existing_path(values: &[String]) -> Option<PathBuf> {
    values
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Uvezeni klipovi s pronađenom medijskom datotekom (proxy/original).
pub fn imported_clip_media_rows(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<Vec<(String, PathBuf)>, String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT clip_id, project_proxy_path, proxy_path, source_path, original_path
             FROM ingest_assets
             WHERE import_status IN ('imported', 'done')
             ORDER BY clip_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (clip_id, project_proxy, proxy, source, original) = row.map_err(|e| e.to_string())?;
        if let Some(media) = first_existing_path(&[project_proxy, proxy, source, original]) {
            out.push((clip_id, media));
        }
    }
    Ok(out)
}
