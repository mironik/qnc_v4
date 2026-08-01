use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use rusqlite::params;
use serde_json::{json, Value};

use tracing::info;

use crate::ingest::db::open_ingest;
use crate::ingest::thumb::resolve_ffmpeg;
use crate::project::db::{now_str, open_global, ProjectPaths};
use crate::project::list_project_ids;

static SCHEMA_READY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

const WAVEFORM_RENDER_VERSION: i64 = 4;
const PEAK_BUCKETS: usize = 1200;
const PCM_SAMPLE_RATE: u32 = 8000;

#[allow(dead_code)]
pub fn bootstrap_schema(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    ensure_schema(paths, project_id)
}

fn schema_cache_key(paths: &ProjectPaths, project_id: &str) -> String {
    format!(
        "{}::{}",
        paths.project_dir(project_id).display(),
        project_id
    )
}

fn ensure_schema(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    let key = schema_cache_key(paths, project_id);
    {
        let guard = SCHEMA_READY.lock().expect("waveform schema cache");
        if guard.as_ref().is_some_and(|ready| ready.contains(&key)) {
            return Ok(());
        }
    }
    ensure_schema_inner(paths, project_id)?;
    let mut guard = SCHEMA_READY.lock().expect("waveform schema cache");
    guard.get_or_insert_with(HashSet::new).insert(key);
    Ok(())
}

fn ensure_schema_inner(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    let conn = open_ingest(paths, project_id).map_err(|error| error.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audio_waveforms (
            clip_id TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'missing',
            a1_peaks TEXT NOT NULL DEFAULT '[]',
            a2_peaks TEXT NOT NULL DEFAULT '[]',
            peak_count INTEGER NOT NULL DEFAULT 0,
            a1_path TEXT NOT NULL DEFAULT '',
            a2_path TEXT NOT NULL DEFAULT '',
            error TEXT NOT NULL DEFAULT '',
            render_version INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT ''
        );",
    )
    .map_err(|error| error.to_string())?;
    for (column, sql_type) in [
        ("a1_peaks", "TEXT NOT NULL DEFAULT '[]'"),
        ("a2_peaks", "TEXT NOT NULL DEFAULT '[]'"),
        ("peak_count", "INTEGER NOT NULL DEFAULT 0"),
        ("render_version", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let _ = conn.execute(
            &format!("ALTER TABLE audio_waveforms ADD COLUMN {column} {sql_type}"),
            [],
        );
    }
    invalidate_legacy_waveforms(&conn)?;
    Ok(())
}

fn invalidate_legacy_waveforms(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(
        "UPDATE audio_waveforms
         SET status = 'missing', render_version = 0, a1_path = '', a2_path = ''
         WHERE render_version < ?1",
        params![WAVEFORM_RENDER_VERSION],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn trim_empty_media_pool_root(project_dir: &Path) {
    let media_pool_root = project_dir.join("media_pool");
    if media_pool_root.is_dir() {
        let empty = fs::read_dir(&media_pool_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if empty {
            let _ = fs::remove_dir(&media_pool_root);
        }
    }
}

/// Ukloni legacy PNG wave direktorij (`media_pool/waveforms/**`) s diska.
pub fn purge_project_waveform_disk_tree(paths: &ProjectPaths, project_id: &str) {
    let project_dir = paths.project_dir(project_id);
    let waveforms_root = project_dir.join("media_pool").join("waveforms");
    if waveforms_root.is_dir() {
        let _ = fs::remove_dir_all(&waveforms_root);
    }
    trim_empty_media_pool_root(&project_dir);
}

/// Boot održavanje: obriši legacy wave PNG-e i invalidiraj stare DB redove.
pub fn maintenance_purge_legacy(paths: &ProjectPaths) {
    let Ok(global) = open_global(paths) else {
        return;
    };
    let Ok(project_ids) = list_project_ids(&global) else {
        return;
    };
    let mut purged = 0usize;
    for project_id in project_ids {
        purge_project_waveform_disk_tree(paths, &project_id);
        if ensure_schema_inner(paths, &project_id).is_ok() {
            let key = schema_cache_key(paths, &project_id);
            SCHEMA_READY
                .lock()
                .expect("waveform schema cache")
                .get_or_insert_with(HashSet::new)
                .insert(key);
            purged += 1;
        }
    }
    if purged > 0 {
        info!(
            "waveform maintenance: peaks v{WAVEFORM_RENDER_VERSION}, legacy disk purge for {purged} project(s)"
        );
    }
}

fn peaks_non_empty(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() > 2 && trimmed != "[]"
}

pub fn ready(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> bool {
    let Ok(conn) = open_ingest(paths, project_id) else {
        return false;
    };
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT status, a1_peaks, render_version
             FROM audio_waveforms WHERE clip_id = ?1",
            params![clip_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();
    row.map(|(status, a1_peaks, version)| {
        status == "ready" && version == WAVEFORM_RENDER_VERSION && peaks_non_empty(&a1_peaks)
    })
    .unwrap_or(false)
}

pub fn peaks_for_channel(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    channel: u8,
) -> Option<Vec<f32>> {
    let conn = open_ingest(paths, project_id).ok()?;
    let column = if channel == 2 { "a2_peaks" } else { "a1_peaks" };
    let sql = format!(
        "SELECT {column} FROM audio_waveforms
         WHERE clip_id = ?1 AND status = 'ready' AND render_version = ?2"
    );
    let raw: String = conn
        .query_row(&sql, params![clip_id, WAVEFORM_RENDER_VERSION], |row| {
            row.get(0)
        })
        .ok()?;
    parse_peaks_json(&raw)
}

pub fn snapshot(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> Value {
    let encoded_project = url_encode(project_id);
    let encoded_clip = url_encode(clip_id);
    let peaks_url = |channel: u8| {
        format!(
            "/api/ingest/waveform/peaks?project_id={encoded_project}&clip_id={encoded_clip}&channel={channel}"
        )
    };
    let fallback = || {
        json!({
            "status": "missing",
            "a1_ready": false,
            "a2_ready": false,
            "peak_count": 0,
            "a1_peaks_url": peaks_url(1),
            "a2_peaks_url": peaks_url(2),
            "error": "",
        })
    };
    if ensure_schema(paths, project_id).is_err() {
        return fallback();
    }
    let Ok(conn) = open_ingest(paths, project_id) else {
        return fallback();
    };
    let row: Option<(String, String, String, String, i64, i64)> = conn
        .query_row(
            "SELECT status, a1_peaks, a2_peaks, error, peak_count, render_version
             FROM audio_waveforms WHERE clip_id = ?1",
            params![clip_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .ok();
    let Some((status, a1_peaks, a2_peaks, error, peak_count, version)) = row else {
        return fallback();
    };
    let current = version == WAVEFORM_RENDER_VERSION;
    json!({
        "status": if current { status } else { "stale".into() },
        "a1_ready": current && peaks_non_empty(&a1_peaks),
        "a2_ready": current && peaks_non_empty(&a2_peaks),
        "peak_count": if current { peak_count } else { 0 },
        "a1_peaks_url": peaks_url(1),
        "a2_peaks_url": peaks_url(2),
        "error": error,
    })
}

fn mark(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    status: &str,
    a1_peaks: &[f32],
    a2_peaks: &[f32],
    error: &str,
) -> Result<(), String> {
    ensure_schema(paths, project_id)?;
    let conn = open_ingest(paths, project_id).map_err(|error| error.to_string())?;
    let a1_json = peaks_to_json(a1_peaks);
    let a2_json = peaks_to_json(a2_peaks);
    let peak_count = a1_peaks.len().max(a2_peaks.len()) as i64;
    conn.execute(
        "INSERT INTO audio_waveforms
            (clip_id,status,a1_peaks,a2_peaks,peak_count,a1_path,a2_path,error,render_version,updated_at)
         VALUES (?1,?2,?3,?4,?5,'','',?6,?7,?8)
         ON CONFLICT(clip_id) DO UPDATE SET
            status=excluded.status,
            a1_peaks=excluded.a1_peaks,
            a2_peaks=excluded.a2_peaks,
            peak_count=excluded.peak_count,
            a1_path='',
            a2_path='',
            error=excluded.error,
            render_version=excluded.render_version,
            updated_at=excluded.updated_at",
        params![
            clip_id,
            status,
            a1_json,
            a2_json,
            peak_count,
            error,
            WAVEFORM_RENDER_VERSION,
            now_str()
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn peaks_to_json(peaks: &[f32]) -> String {
    serde_json::to_string(peaks).unwrap_or_else(|_| "[]".into())
}

fn parse_peaks_json(raw: &str) -> Option<Vec<f32>> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let n = item.as_f64()? as f32;
        if n.is_finite() && n >= 0.0 {
            out.push(n);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn decode_pcm_mono(
    ffmpeg: &Path,
    media: &Path,
    stream_index: u8,
    channel_index: u8,
) -> Result<Vec<f32>, String> {
    let pan = format!("pan=mono|c0=c{channel_index}");
    let map = format!("0:a:{stream_index}");
    let rate = PCM_SAMPLE_RATE.to_string();
    let result = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(media)
        .args([
            "-map", &map, "-af", &pan, "-ac", "1", "-ar", &rate, "-f", "f32le", "-",
        ])
        .output()
        .map_err(|error| format!("waveform ffmpeg: {error}"))?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
    }
    let samples = bytes_to_f32_samples(&result.stdout);
    if samples.is_empty() {
        return Err("prazan audio uzorak".into());
    }
    Ok(samples)
}

fn bytes_to_f32_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .filter(|sample| sample.is_finite())
        .collect()
}

fn bucket_max_peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(buckets);
    let samples_per_bucket = (samples.len() as f64 / buckets as f64).max(1.0);
    for i in 0..buckets {
        let start = ((i as f64) * samples_per_bucket) as usize;
        let end = (((i + 1) as f64) * samples_per_bucket) as usize;
        let end = end.min(samples.len()).max(start.saturating_add(1));
        let peak = samples[start..end]
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0f32, f32::max);
        out.push(peak);
    }
    let max = out.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0 {
        for value in &mut out {
            *value /= max;
        }
    }
    out
}

fn extract_lane_peaks(
    ffmpeg: &Path,
    media: &Path,
    stream_index: u8,
    channel_index: u8,
) -> Result<Vec<f32>, String> {
    let samples = decode_pcm_mono(ffmpeg, media, stream_index, channel_index)?;
    Ok(bucket_max_peaks(&samples, PEAK_BUCKETS))
}

pub fn build_for_clip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    media: &Path,
) -> Result<(), String> {
    purge_project_waveform_disk_tree(paths, project_id);
    if ready(paths, project_id, clip_id) {
        return Ok(());
    }
    let ffmpeg = resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan".to_string())?;
    mark(paths, project_id, clip_id, "building", &[], &[], "")?;

    let a1 = match extract_lane_peaks(&ffmpeg, media, 0, 0) {
        Ok(peaks) => peaks,
        Err(error) => {
            mark(paths, project_id, clip_id, "failed", &[], &[], &error)?;
            return Err(error);
        }
    };

    let a2 = extract_lane_peaks(&ffmpeg, media, 1, 0)
        .or_else(|_| extract_lane_peaks(&ffmpeg, media, 0, 1))
        .unwrap_or_default();
    let warning = if a2.is_empty() {
        "A2 nije dostupna".into()
    } else {
        String::new()
    };
    mark(paths, project_id, clip_id, "ready", &a1, &a2, &warning)
}

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_max_peaks_normalizes() {
        let samples = vec![0.0f32, 0.5, -1.0, 0.25, 0.75];
        let peaks = bucket_max_peaks(&samples, 3);
        assert_eq!(peaks.len(), 3);
        assert!((peaks.iter().copied().fold(0.0f32, f32::max) - 1.0).abs() < 0.001);
    }

    #[test]
    fn parse_peaks_json_roundtrip() {
        let raw = peaks_to_json(&[0.1, 0.5, 1.0]);
        let parsed = parse_peaks_json(&raw).expect("parse peaks");
        assert_eq!(parsed.len(), 3);
    }
}
