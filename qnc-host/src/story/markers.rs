use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::frame_time::{is_valid_fps, seconds_to_timecode};

use super::db::{list_parts, read_row, touch_draft, StoryPartRow};

pub const TIMELINE_EPS: f64 = 0.001;

#[derive(Clone)]
pub struct StoryMarkerRow {
    pub marker_id: String,
    pub timeline_sec: f64,
    pub label: String,
    pub sort_index: i64,
    pub origin_part_id: String,
    pub origin_local_sec: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct StoryMarkerSlotRow {
    pub slot_id: String,
    pub slot_index: i64,
    pub start_sec: f64,
    pub end_sec: f64,
    pub duration_sec: f64,
    pub start_marker_id: String,
    pub end_marker_id: String,
    pub slot_signature: String,
    pub updated_at: String,
}

pub fn slot_signature(start_sec: f64, end_sec: f64) -> String {
    format!("start:{start_sec:.3}|end:{end_sec:.3}")
}

fn require_timeline_fps(timeline_fps: f64) -> Result<f64, String> {
    if is_valid_fps(timeline_fps) {
        Ok(timeline_fps)
    } else {
        Err("timeline_fps_invalid: project nema valjan timeline FPS".into())
    }
}

fn require_timeline_fps_sql(timeline_fps: f64) -> rusqlite::Result<f64> {
    require_timeline_fps(timeline_fps).map_err(rusqlite::Error::InvalidParameterName)
}

fn marker_timecode(timeline_sec: f64, timeline_fps: f64) -> String {
    if is_valid_fps(timeline_fps) {
        seconds_to_timecode(timeline_sec, timeline_fps)
    } else {
        String::new()
    }
}

pub fn ensure_marker_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS story_markers (
            marker_id TEXT PRIMARY KEY,
            timeline_sec REAL NOT NULL DEFAULT 0,
            tc TEXT NOT NULL DEFAULT '',
            label TEXT NOT NULL DEFAULT '',
            sort_index INTEGER NOT NULL DEFAULT 0,
            origin_part_id TEXT NOT NULL DEFAULT '',
            origin_local_sec REAL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_story_markers_timeline ON story_markers(timeline_sec);
        CREATE TABLE IF NOT EXISTS story_marker_slots (
            slot_id TEXT PRIMARY KEY,
            slot_index INTEGER NOT NULL,
            start_sec REAL NOT NULL,
            end_sec REAL NOT NULL,
            duration_sec REAL NOT NULL DEFAULT 0,
            start_marker_id TEXT NOT NULL DEFAULT '',
            end_marker_id TEXT NOT NULL DEFAULT '',
            slot_signature TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_story_marker_slots_sort ON story_marker_slots(slot_index);",
    )?;
    let _ = conn.execute(
        "ALTER TABLE story_state ADD COLUMN selected_slot_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    Ok(())
}

fn new_marker_id() -> String {
    format!("marker_{}", uuid::Uuid::new_v4().simple())
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub fn part_span_seconds(part: &StoryPartRow) -> f64 {
    let in_s = part.in_seconds.unwrap_or(0.0);
    let out_s = part.out_seconds.unwrap_or(0.0);
    if out_s > in_s {
        return round3((out_s - in_s).max(0.05));
    }
    3.0
}

pub fn timeline_duration_from_parts(parts: &[StoryPartRow]) -> f64 {
    round3(parts.iter().map(part_span_seconds).sum())
}

pub(crate) fn list_markers_rows(conn: &Connection) -> rusqlite::Result<Vec<StoryMarkerRow>> {
    list_markers(conn)
}

fn list_markers(conn: &Connection) -> rusqlite::Result<Vec<StoryMarkerRow>> {
    ensure_marker_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT marker_id, timeline_sec, label, sort_index,
                COALESCE(origin_part_id, ''), origin_local_sec, created_at, updated_at
         FROM story_markers
         ORDER BY timeline_sec ASC, marker_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryMarkerRow {
            marker_id: r.get(0)?,
            timeline_sec: r.get(1)?,
            label: r.get(2)?,
            sort_index: r.get(3)?,
            origin_part_id: r.get(4)?,
            origin_local_sec: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

pub(crate) fn list_marker_slots_rows(
    conn: &Connection,
) -> rusqlite::Result<Vec<StoryMarkerSlotRow>> {
    list_marker_slots(conn)
}

/// Match cover → materialized M–M slot (authoritative bounds + duration).
pub(crate) fn cover_slot_for_cover(
    conn: &Connection,
    cover: &super::covers::StoryCoverRow,
) -> rusqlite::Result<Option<StoryMarkerSlotRow>> {
    ensure_marker_schema(conn)?;
    let slots = list_marker_slots(conn)?;
    let sig = cover.slot_signature.trim();
    if !sig.is_empty() {
        if let Some(slot) = slots.iter().find(|slot| slot.slot_signature.trim() == sig) {
            return Ok(Some(slot.clone()));
        }
    }
    if let Some(slot) = slots.iter().find(|slot| {
        (slot.start_sec - cover.timeline_start_sec).abs() < TIMELINE_EPS
            && (slot.end_sec - cover.timeline_end_sec).abs() < TIMELINE_EPS
    }) {
        return Ok(Some(slot.clone()));
    }
    Ok(slots
        .iter()
        .find(|slot| slot.slot_index == cover.slot_index)
        .cloned())
}

/// Authoritative M–M slot width for a cover (marker_slots table, not stale cover bounds).
pub(crate) fn cover_slot_duration_sec(
    conn: &Connection,
    cover: &super::covers::StoryCoverRow,
) -> rusqlite::Result<f64> {
    Ok(cover_slot_for_cover(conn, cover)?
        .map(|slot| slot.duration_sec.max(0.0))
        .unwrap_or_else(|| (cover.timeline_end_sec - cover.timeline_start_sec).max(0.0)))
}

fn list_marker_slots(conn: &Connection) -> rusqlite::Result<Vec<StoryMarkerSlotRow>> {
    ensure_marker_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT slot_id, slot_index, start_sec, end_sec, duration_sec,
                start_marker_id, end_marker_id, slot_signature, updated_at
         FROM story_marker_slots
         ORDER BY slot_index ASC, slot_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryMarkerSlotRow {
            slot_id: r.get(0)?,
            slot_index: r.get(1)?,
            start_sec: r.get(2)?,
            end_sec: r.get(3)?,
            duration_sec: r.get(4)?,
            start_marker_id: r.get(5)?,
            end_marker_id: r.get(6)?,
            slot_signature: r.get(7)?,
            updated_at: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn marker_json(row: &StoryMarkerRow, _parts: &[StoryPartRow], timeline_fps: f64) -> Value {
    let tc = marker_timecode(row.timeline_sec, timeline_fps);
    json!({
        "marker_id": row.marker_id,
        "timeline_sec": row.timeline_sec,
        "tc": tc,
        "label": row.label,
        "sort_index": row.sort_index,
        "origin_part_id": row.origin_part_id,
        "origin_local_sec": row.origin_local_sec,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub fn marker_slot_json(row: &StoryMarkerSlotRow) -> Value {
    json!({
        "slot_id": row.slot_id,
        "slot_index": row.slot_index,
        "start_sec": row.start_sec,
        "end_sec": row.end_sec,
        "duration_sec": row.duration_sec,
        "start_marker_id": row.start_marker_id,
        "end_marker_id": row.end_marker_id,
        "slot_signature": row.slot_signature,
        "updated_at": row.updated_at,
    })
}

pub fn markers_snapshot(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<Vec<Value>> {
    let parts = list_parts(conn)?;
    Ok(list_markers(conn)?
        .iter()
        .map(|row| marker_json(row, &parts, timeline_fps))
        .collect())
}

pub fn marker_slots_snapshot(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    Ok(list_marker_slots(conn)?
        .iter()
        .map(marker_slot_json)
        .collect())
}

pub fn get_slot_by_id(conn: &Connection, slot_id: &str) -> Result<StoryMarkerSlotRow, String> {
    let slot_id = slot_id.trim();
    conn.query_row(
        "SELECT slot_id, slot_index, start_sec, end_sec, duration_sec,
                start_marker_id, end_marker_id, slot_signature, updated_at
         FROM story_marker_slots WHERE slot_id = ?1",
        params![slot_id],
        |r| {
            Ok(StoryMarkerSlotRow {
                slot_id: r.get(0)?,
                slot_index: r.get(1)?,
                start_sec: r.get(2)?,
                end_sec: r.get(3)?,
                duration_sec: r.get(4)?,
                start_marker_id: r.get(5)?,
                end_marker_id: r.get(6)?,
                slot_signature: r.get(7)?,
                updated_at: r.get(8)?,
            })
        },
    )
    .map_err(|_| format!("slot not found: {slot_id}"))
}

fn normalize_selected_slot_id(conn: &Connection, selected: &str) -> rusqlite::Result<String> {
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(String::new());
    }
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM story_marker_slots WHERE slot_id = ?1",
        params![selected],
        |r| r.get(0),
    )?;
    if exists > 0 {
        Ok(selected.to_string())
    } else {
        Ok(String::new())
    }
}

fn set_selected_slot_id(conn: &Connection, slot_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE story_state SET selected_slot_id = ?1 WHERE id = 1",
        params![slot_id],
    )?;
    Ok(())
}

/// Ensures exactly one automatic start marker (M at timeline 0) for the whole story.
/// Must never create markers at other segment starts — only here via `recompute_marker_slots`.
fn ensure_start_marker(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    let timeline_fps = require_timeline_fps_sql(timeline_fps)?;
    let parts = list_parts(conn)?;
    let duration = timeline_duration_from_parts(&parts);
    if duration <= TIMELINE_EPS {
        conn.execute("DELETE FROM story_marker_slots", [])?;
        conn.execute("DELETE FROM story_markers", [])?;
        conn.execute(
            "UPDATE story_state SET selected_slot_id = '' WHERE id = 1",
            [],
        )?;
        return Ok(());
    }
    let markers = list_markers(conn)?;
    let first_part_id = parts
        .first()
        .map(|part| part.part_id.as_str())
        .unwrap_or("");
    if let Some(marker) = markers.iter().find(|m| m.timeline_sec < TIMELINE_EPS) {
        let start_tc = seconds_to_timecode(0.0, timeline_fps);
        conn.execute(
            "UPDATE story_markers
             SET origin_part_id = ?1, origin_local_sec = 0, tc = ?2, label = ?3
             WHERE marker_id = ?4",
            params![first_part_id, start_tc, start_tc, marker.marker_id],
        )?;
        return Ok(());
    }
    let now = crate::project::db::now_str();
    let marker_id = new_marker_id();
    let start_tc = seconds_to_timecode(0.0, timeline_fps);
    conn.execute(
        "INSERT INTO story_markers
            (marker_id, timeline_sec, tc, label, sort_index, origin_part_id,
             origin_local_sec, created_at, updated_at)
         VALUES (?1, 0, ?2, ?3, 0, ?4, 0, ?5, ?5)",
        params![marker_id, start_tc, start_tc, first_part_id, now],
    )?;
    Ok(())
}

fn refresh_marker_sort_indices(conn: &Connection) -> rusqlite::Result<()> {
    let markers = list_markers(conn)?;
    for (i, m) in markers.iter().enumerate() {
        conn.execute(
            "UPDATE story_markers SET sort_index = ?1 WHERE marker_id = ?2",
            params![i as i64, m.marker_id],
        )?;
    }
    Ok(())
}

pub fn recompute_marker_slots(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    ensure_marker_schema(conn)?;
    ensure_start_marker(conn, timeline_fps)?;
    refresh_marker_sort_indices(conn)?;

    let now = crate::project::db::now_str();
    let markers = list_markers(conn)?;
    let duration = timeline_duration_from_parts(&list_parts(conn)?);

    let mut slots: Vec<(String, i64, f64, f64, String, String, String)> = Vec::new();
    if markers.len() >= 2 {
        for i in 0..markers.len() - 1 {
            let start = round3(markers[i].timeline_sec);
            let end = round3(markers[i + 1].timeline_sec);
            if end <= start + TIMELINE_EPS {
                continue;
            }
            let end = end.min(duration.max(end));
            let sig = slot_signature(start, end);
            slots.push((
                sig.clone(),
                i as i64,
                start,
                end,
                markers[i].marker_id.clone(),
                markers[i + 1].marker_id.clone(),
                sig,
            ));
        }
    }

    let new_slot_specs: Vec<(f64, f64, String)> = slots
        .iter()
        .map(|(_, _, start, end, _, _, sig)| (*start, *end, sig.clone()))
        .collect();
    super::covers::normalize_covers_for_slots(conn, &new_slot_specs)?;

    conn.execute("DELETE FROM story_marker_slots", [])?;
    for (slot_id, slot_index, start_sec, end_sec, start_mid, end_mid, sig) in slots {
        let dur = round3((end_sec - start_sec).max(0.0));
        conn.execute(
            "INSERT INTO story_marker_slots
                (slot_id, slot_index, start_sec, end_sec, duration_sec,
                 start_marker_id, end_marker_id, slot_signature, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![slot_id, slot_index, start_sec, end_sec, dur, start_mid, end_mid, sig, now],
        )?;
    }

    let row = read_row(conn)?;
    let normalized = normalize_selected_slot_id(conn, &row.selected_slot_id)?;
    if normalized != row.selected_slot_id {
        set_selected_slot_id(conn, &normalized)?;
    }
    super::covers::normalize_selected_cover_id(conn)?;
    Ok(())
}

pub fn finalize_story_mutation(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    recompute_marker_slots(conn, timeline_fps)?;
    touch_draft(conn)
}

pub fn create_marker(
    conn: &Connection,
    timeline_sec: f64,
    label: Option<&str>,
    origin_part_id: Option<&str>,
    origin_local_sec: Option<f64>,
    timeline_fps: f64,
) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    let timeline_sec = round3(timeline_sec);
    if timeline_sec < 0.0 {
        return Err("timeline_sec must be >= 0".into());
    }
    let markers = list_markers(conn).map_err(|e| e.to_string())?;
    if markers
        .iter()
        .any(|m| (m.timeline_sec - timeline_sec).abs() < TIMELINE_EPS)
    {
        return Err(format!(
            "marker already exists at timeline_sec={timeline_sec}"
        ));
    }
    let marker_id = new_marker_id();
    let now = crate::project::db::now_str();
    let origin_part_id = origin_part_id.unwrap_or("").trim();
    let tc = seconds_to_timecode(timeline_sec, timeline_fps);
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&tc)
        .to_string();
    conn.execute(
        "INSERT INTO story_markers
            (marker_id, timeline_sec, tc, label, sort_index, origin_part_id, origin_local_sec, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?7)",
        params![
            marker_id,
            timeline_sec,
            tc,
            label,
            origin_part_id,
            origin_local_sec,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    finalize_story_mutation(conn, timeline_fps).map_err(|e| e.to_string())
}

pub fn delete_marker(conn: &Connection, marker_id: &str, timeline_fps: f64) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    let marker_id = marker_id.trim();
    if marker_id.is_empty() {
        return Err("marker_id required".into());
    }
    let n = conn
        .execute(
            "DELETE FROM story_markers WHERE marker_id = ?1",
            params![marker_id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("marker not found: {marker_id}"));
    }
    finalize_story_mutation(conn, timeline_fps).map_err(|e| e.to_string())
}

pub fn update_marker(
    conn: &Connection,
    marker_id: &str,
    timeline_sec: f64,
    label: Option<&str>,
    timeline_fps: f64,
) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    let marker_id = marker_id.trim();
    if marker_id.is_empty() {
        return Err("marker_id required".into());
    }
    let timeline_sec = round3(timeline_sec);
    if !timeline_sec.is_finite() || timeline_sec < 0.0 {
        return Err("timeline_sec must be >= 0".into());
    }
    let duration =
        timeline_duration_from_parts(&list_parts(conn).map_err(|error| error.to_string())?);
    if timeline_sec > duration + TIMELINE_EPS {
        return Err(format!(
            "M marker mora biti unutar trajanja storyja ({duration:.3} s)."
        ));
    }
    let markers = list_markers(conn).map_err(|error| error.to_string())?;
    let marker = markers
        .iter()
        .find(|marker| marker.marker_id == marker_id)
        .ok_or_else(|| format!("marker not found: {marker_id}"))?;
    if marker.timeline_sec < TIMELINE_EPS {
        return Err("Početni M marker je zaključan.".into());
    }
    if markers.iter().any(|other| {
        other.marker_id != marker_id && (other.timeline_sec - timeline_sec).abs() < TIMELINE_EPS
    }) {
        return Err(format!(
            "marker already exists at timeline_sec={timeline_sec}"
        ));
    }
    let tc = seconds_to_timecode(timeline_sec, timeline_fps);
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(marker.label.as_str());
    conn.execute(
        "UPDATE story_markers
         SET timeline_sec = ?1, tc = ?2, label = ?3, updated_at = ?4
         WHERE marker_id = ?5",
        params![
            timeline_sec,
            tc,
            label,
            crate::project::db::now_str(),
            marker_id
        ],
    )
    .map_err(|error| error.to_string())?;
    finalize_story_mutation(conn, timeline_fps).map_err(|error| error.to_string())
}

pub fn move_marker(
    conn: &Connection,
    marker_id: &str,
    direction: &str,
    timeline_fps: f64,
) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    let marker_id = marker_id.trim();
    if marker_id.is_empty() {
        return Err("marker_id required".into());
    }
    let dir = direction.trim().to_lowercase();
    if dir != "up" && dir != "down" {
        return Err(format!("invalid direction: {direction}"));
    }
    let markers = list_markers(conn).map_err(|e| e.to_string())?;
    let idx = markers
        .iter()
        .position(|m| m.marker_id == marker_id)
        .ok_or_else(|| format!("marker not found: {marker_id}"))?;
    let swap_with = if dir == "up" {
        if idx == 0 {
            return Ok(());
        }
        idx - 1
    } else if idx + 1 >= markers.len() {
        return Ok(());
    } else {
        idx + 1
    };
    let a_id = markers[idx].marker_id.clone();
    let b_id = markers[swap_with].marker_id.clone();
    let a_sec = markers[idx].timeline_sec;
    let b_sec = markers[swap_with].timeline_sec;
    let a_old_tc = seconds_to_timecode(a_sec, timeline_fps);
    let b_old_tc = seconds_to_timecode(b_sec, timeline_fps);
    let a_tc = seconds_to_timecode(b_sec, timeline_fps);
    let b_tc = seconds_to_timecode(a_sec, timeline_fps);
    let a_label = if markers[idx].label == a_old_tc {
        a_tc.clone()
    } else {
        markers[idx].label.clone()
    };
    let b_label = if markers[swap_with].label == b_old_tc {
        b_tc.clone()
    } else {
        markers[swap_with].label.clone()
    };
    let now = crate::project::db::now_str();
    conn.execute(
        "UPDATE story_markers SET timeline_sec = ?1, tc = ?2, label = ?3, updated_at = ?4 WHERE marker_id = ?5",
        params![b_sec, a_tc, a_label, now, a_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE story_markers SET timeline_sec = ?1, tc = ?2, label = ?3, updated_at = ?4 WHERE marker_id = ?5",
        params![a_sec, b_tc, b_label, now, b_id],
    )
    .map_err(|e| e.to_string())?;
    finalize_story_mutation(conn, timeline_fps).map_err(|e| e.to_string())
}

pub fn select_marker_slot(conn: &Connection, slot_id: &str) -> Result<(), String> {
    let slot_id = slot_id.trim();
    if slot_id.is_empty() {
        return Err("slot_id required".into());
    }
    let _ = get_slot_by_id(conn, slot_id)?;
    set_selected_slot_id(conn, slot_id).map_err(|e| e.to_string())?;
    touch_draft(conn).map_err(|e| e.to_string())
}

/// Shift markers when a part is removed from the virtual timeline.
pub fn shift_markers_after_part_removal(
    conn: &Connection,
    part_start: f64,
    part_end: f64,
) -> rusqlite::Result<()> {
    let span = part_end - part_start;
    if span <= TIMELINE_EPS {
        return Ok(());
    }
    let markers = list_markers(conn)?;
    let now = crate::project::db::now_str();
    for m in markers {
        let t = m.timeline_sec;
        if t > part_start + TIMELINE_EPS && t < part_end - TIMELINE_EPS {
            conn.execute(
                "DELETE FROM story_markers WHERE marker_id = ?1",
                params![m.marker_id],
            )?;
        } else if t >= part_end - TIMELINE_EPS {
            let new_t = round3((t - span).max(0.0));
            conn.execute(
                "UPDATE story_markers SET timeline_sec = ?1, updated_at = ?2 WHERE marker_id = ?3",
                params![new_t, now, m.marker_id],
            )?;
        }
    }
    Ok(())
}

pub fn part_timeline_window(parts: &[StoryPartRow], part_id: &str) -> Option<(f64, f64)> {
    let mut cursor = 0.0;
    for part in parts {
        let span = part_span_seconds(part);
        if part.part_id == part_id {
            return Some((round3(cursor), round3(cursor + span)));
        }
        cursor = round3(cursor + span);
    }
    None
}

/// Convert a local offset inside one story part to cumulative virtual timeline_sec.
pub fn local_to_timeline_sec(
    parts: &[StoryPartRow],
    part_id: &str,
    local_sec: f64,
) -> Result<f64, String> {
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let (start, end) =
        part_timeline_window(parts, part_id).ok_or_else(|| format!("part not found: {part_id}"))?;
    let span = (end - start).max(0.0);
    let local = local_sec.max(0.0);
    let clamped = if span > TIMELINE_EPS {
        local.min(span)
    } else {
        0.0
    };
    Ok(round3(start + clamped))
}

pub fn resolve_marker_timeline_sec(
    parts: &[StoryPartRow],
    timeline_sec: Option<f64>,
    part_id: Option<&str>,
    local_sec: Option<f64>,
) -> Result<(f64, String, Option<f64>), String> {
    if let Some(sec) = timeline_sec {
        if sec < 0.0 {
            return Err("timeline_sec must be >= 0".into());
        }
        let origin_part = part_id.unwrap_or("").trim().to_string();
        return Ok((round3(sec), origin_part, local_sec));
    }
    let part_id = part_id
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "timeline_sec or part_id required".to_string())?;
    let local = local_sec.unwrap_or(0.0);
    let global = local_to_timeline_sec(parts, part_id, local)?;
    Ok((global, part_id.trim().to_string(), Some(local)))
}

pub fn delete_markers_for_part(conn: &Connection, part_id: &str) -> rusqlite::Result<()> {
    let parts = list_parts(conn)?;
    if let Some((start, end)) = part_timeline_window(&parts, part_id) {
        shift_markers_after_part_removal(conn, start, end)?;
    }
    Ok(())
}

pub fn ensure_materialized_slots(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    let _ = list_parts(conn)?;
    recompute_marker_slots(conn, timeline_fps)
}
