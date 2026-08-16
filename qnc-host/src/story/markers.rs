use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::frame_time::{frame_to_seconds, is_valid_fps, seconds_to_frame, seconds_to_timecode};

use super::db::{list_parts, read_row, touch_draft, StoryPartRow};

pub const TIMELINE_EPS: f64 = 0.001;

#[derive(Clone)]
pub struct StoryMarkerRow {
    pub marker_id: String,
    pub timeline_frame: i64,
    pub timeline_sec: f64,
    pub label: String,
    pub sort_index: i64,
    pub system_role: String,
    pub origin_part_id: String,
    pub origin_local_frame: Option<i64>,
    pub origin_local_sec: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct StoryMarkerSlotRow {
    pub slot_id: String,
    pub slot_index: i64,
    pub start_frame: i64,
    pub end_frame: i64,
    pub duration_frames: i64,
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

const SYSTEM_MARKER_START: &str = "program_start";
const SYSTEM_MARKER_END: &str = "program_end";

fn require_timeline_fps(timeline_fps: f64) -> Result<f64, String> {
    if is_valid_fps(timeline_fps) {
        Ok(timeline_fps)
    } else {
        Err("timeline_fps_invalid: story program nema valjan source FPS".into())
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
            timeline_frame INTEGER NOT NULL DEFAULT 0,
            timeline_sec REAL NOT NULL DEFAULT 0,
            tc TEXT NOT NULL DEFAULT '',
            label TEXT NOT NULL DEFAULT '',
            sort_index INTEGER NOT NULL DEFAULT 0,
            system_role TEXT NOT NULL DEFAULT '',
            origin_part_id TEXT NOT NULL DEFAULT '',
            origin_local_frame INTEGER,
            origin_local_sec REAL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_story_markers_frame ON story_markers(timeline_frame);
        CREATE INDEX IF NOT EXISTS idx_story_markers_timeline ON story_markers(timeline_sec);
        CREATE TABLE IF NOT EXISTS story_marker_slots (
            slot_id TEXT PRIMARY KEY,
            slot_index INTEGER NOT NULL,
            start_frame INTEGER NOT NULL DEFAULT 0,
            end_frame INTEGER NOT NULL DEFAULT 0,
            duration_frames INTEGER NOT NULL DEFAULT 0,
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
    migrate_marker_frame_columns(conn)?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_marker_frame_columns(conn: &Connection) -> rusqlite::Result<()> {
    for (table, column, sql_type) in [
        (
            "story_markers",
            "timeline_frame",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("story_markers", "origin_local_frame", "INTEGER"),
        ("story_markers", "system_role", "TEXT NOT NULL DEFAULT ''"),
        (
            "story_marker_slots",
            "start_frame",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "story_marker_slots",
            "end_frame",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "story_marker_slots",
            "duration_frames",
            "INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !column_exists(conn, table, column)? {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn backfill_marker_frames(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    let timeline_fps = require_timeline_fps_sql(timeline_fps)?;
    let mut stmt = conn.prepare(
        "SELECT marker_id, timeline_sec, origin_local_sec
         FROM story_markers
         WHERE timeline_frame = 0 AND timeline_sec > 0",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (marker_id, timeline_sec, origin_local_sec) in rows {
        let timeline_frame = seconds_to_frame(timeline_sec, timeline_fps);
        let origin_local_frame = origin_local_sec.map(|sec| seconds_to_frame(sec, timeline_fps));
        conn.execute(
            "UPDATE story_markers
             SET timeline_frame = ?1, origin_local_frame = ?2
             WHERE marker_id = ?3",
            params![timeline_frame, origin_local_frame, marker_id],
        )?;
    }
    Ok(())
}

fn new_marker_id() -> String {
    format!("marker_{}", uuid::Uuid::new_v4().simple())
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub fn part_span_seconds(part: &StoryPartRow) -> f64 {
    if part.duration_frames > 0 && is_valid_fps(part.fps) {
        return round3(frame_to_seconds(part.duration_frames, part.fps).max(0.05));
    }
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

pub fn part_span_frames(part: &StoryPartRow, _timeline_fps: f64) -> i64 {
    if part.duration_frames > 0 {
        return part.duration_frames.max(1);
    }
    if part.out_frame > part.in_frame {
        return (part.out_frame - part.in_frame).max(1);
    }
    if is_valid_fps(part.fps) {
        return seconds_to_frame(part_span_seconds(part), part.fps).max(1);
    }
    0
}

pub fn timeline_duration_frames_from_parts(parts: &[StoryPartRow], timeline_fps: f64) -> i64 {
    parts
        .iter()
        .map(|part| part_span_frames(part, timeline_fps))
        .sum()
}

pub(crate) fn list_markers_rows(conn: &Connection) -> rusqlite::Result<Vec<StoryMarkerRow>> {
    list_markers(conn)
}

fn list_markers(conn: &Connection) -> rusqlite::Result<Vec<StoryMarkerRow>> {
    ensure_marker_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT marker_id, timeline_frame, timeline_sec, label, sort_index,
                COALESCE(system_role, ''), COALESCE(origin_part_id, ''),
                origin_local_frame, origin_local_sec,
                created_at, updated_at
         FROM story_markers
         ORDER BY timeline_frame ASC, marker_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryMarkerRow {
            marker_id: r.get(0)?,
            timeline_frame: r.get(1)?,
            timeline_sec: r.get(2)?,
            label: r.get(3)?,
            sort_index: r.get(4)?,
            system_role: r.get(5)?,
            origin_part_id: r.get(6)?,
            origin_local_frame: r.get(7)?,
            origin_local_sec: r.get(8)?,
            created_at: r.get(9)?,
            updated_at: r.get(10)?,
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
        (cover.timeline_start_frame > 0 || cover.timeline_end_frame > 0)
            && slot.start_frame == cover.timeline_start_frame
            && slot.end_frame == cover.timeline_end_frame
    }) {
        return Ok(Some(slot.clone()));
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
        "SELECT slot_id, slot_index, start_frame, end_frame, duration_frames,
                start_sec, end_sec, duration_sec,
                start_marker_id, end_marker_id, slot_signature, updated_at
         FROM story_marker_slots
         ORDER BY slot_index ASC, slot_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryMarkerSlotRow {
            slot_id: r.get(0)?,
            slot_index: r.get(1)?,
            start_frame: r.get(2)?,
            end_frame: r.get(3)?,
            duration_frames: r.get(4)?,
            start_sec: r.get(5)?,
            end_sec: r.get(6)?,
            duration_sec: r.get(7)?,
            start_marker_id: r.get(8)?,
            end_marker_id: r.get(9)?,
            slot_signature: r.get(10)?,
            updated_at: r.get(11)?,
        })
    })?;
    rows.collect()
}

pub fn marker_json(row: &StoryMarkerRow, _parts: &[StoryPartRow], timeline_fps: f64) -> Value {
    let tc = marker_timecode(row.timeline_sec, timeline_fps);
    json!({
        "marker_id": row.marker_id,
        "timeline_frame": row.timeline_frame,
        "timeline_sec": row.timeline_sec,
        "tc": tc,
        "label": row.label,
        "sort_index": row.sort_index,
        "system_role": row.system_role,
        "origin_part_id": row.origin_part_id,
        "origin_local_frame": row.origin_local_frame,
        "origin_local_sec": row.origin_local_sec,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub fn marker_slot_json(row: &StoryMarkerSlotRow) -> Value {
    json!({
        "slot_id": row.slot_id,
        "slot_index": row.slot_index,
        "start_frame": row.start_frame,
        "end_frame": row.end_frame,
        "duration_frames": row.duration_frames,
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
    backfill_marker_frames(conn, timeline_fps)?;
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
        "SELECT slot_id, slot_index, start_frame, end_frame, duration_frames,
                start_sec, end_sec, duration_sec,
                start_marker_id, end_marker_id, slot_signature, updated_at
         FROM story_marker_slots WHERE slot_id = ?1",
        params![slot_id],
        |r| {
            Ok(StoryMarkerSlotRow {
                slot_id: r.get(0)?,
                slot_index: r.get(1)?,
                start_frame: r.get(2)?,
                end_frame: r.get(3)?,
                duration_frames: r.get(4)?,
                start_sec: r.get(5)?,
                end_sec: r.get(6)?,
                duration_sec: r.get(7)?,
                start_marker_id: r.get(8)?,
                end_marker_id: r.get(9)?,
                slot_signature: r.get(10)?,
                updated_at: r.get(11)?,
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
    let duration_frames = timeline_duration_frames_from_parts(&parts, timeline_fps);
    if duration_frames <= 0 {
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
    let marker = markers
        .iter()
        .find(|m| m.system_role == SYSTEM_MARKER_START)
        .or_else(|| markers.iter().find(|m| m.timeline_frame == 0));
    if let Some(marker) = marker {
        let start_tc = seconds_to_timecode(0.0, timeline_fps);
        conn.execute(
            "UPDATE story_markers
             SET timeline_frame = 0, origin_part_id = ?1, origin_local_frame = 0,
                 origin_local_sec = 0, tc = ?2, label = ?3, system_role = ?4
             WHERE marker_id = ?5",
            params![
                first_part_id,
                start_tc,
                start_tc,
                SYSTEM_MARKER_START,
                marker.marker_id
            ],
        )?;
        conn.execute(
            "UPDATE story_markers
             SET system_role = ''
             WHERE system_role = ?1 AND marker_id != ?2",
            params![SYSTEM_MARKER_START, marker.marker_id],
        )?;
        return Ok(());
    }
    let now = crate::project::db::now_str();
    let marker_id = new_marker_id();
    let start_tc = seconds_to_timecode(0.0, timeline_fps);
    conn.execute(
        "INSERT INTO story_markers
            (marker_id, timeline_frame, timeline_sec, tc, label, sort_index, system_role, origin_part_id,
             origin_local_frame, origin_local_sec, created_at, updated_at)
         VALUES (?1, 0, 0, ?2, ?3, 0, ?4, ?5, 0, 0, ?6, ?6)",
        params![
            marker_id,
            start_tc,
            start_tc,
            SYSTEM_MARKER_START,
            first_part_id,
            now
        ],
    )?;
    Ok(())
}

/// Ensures exactly one automatic end marker (M at program duration).
/// This is the final boundary node for the last marker slot, not a segment marker.
fn ensure_end_marker(
    conn: &Connection,
    timeline_fps: f64,
    parts: &[StoryPartRow],
    duration_frames: i64,
) -> rusqlite::Result<()> {
    let timeline_fps = require_timeline_fps_sql(timeline_fps)?;
    if duration_frames <= 0 {
        return Ok(());
    }
    let markers = list_markers(conn)?;
    let end_at_duration = markers
        .iter()
        .find(|m| m.timeline_frame == duration_frames && m.system_role != SYSTEM_MARKER_START);
    let system_end = markers.iter().find(|m| m.system_role == SYSTEM_MARKER_END);
    let keep_id = end_at_duration
        .or(system_end)
        .map(|marker| marker.marker_id.clone());

    if let (Some(system_end), Some(end_at_duration)) = (system_end, end_at_duration) {
        if system_end.marker_id != end_at_duration.marker_id {
            conn.execute(
                "DELETE FROM story_markers WHERE marker_id = ?1",
                params![system_end.marker_id],
            )?;
        }
    }

    let last_part = parts.last();
    let origin_part_id = last_part.map(|part| part.part_id.as_str()).unwrap_or("");
    let origin_local_frame = last_part
        .map(|part| part_span_frames(part, timeline_fps).max(0))
        .unwrap_or(0);
    let end_sec = round3(frame_to_seconds(duration_frames, timeline_fps));
    let end_tc = seconds_to_timecode(end_sec, timeline_fps);
    let now = crate::project::db::now_str();

    if let Some(marker_id) = keep_id {
        conn.execute(
            "UPDATE story_markers
             SET timeline_frame = ?1, timeline_sec = ?2, tc = ?3, label = ?4,
                 sort_index = 0, system_role = ?5, origin_part_id = ?6,
                 origin_local_frame = ?7, origin_local_sec = ?8, updated_at = ?9
             WHERE marker_id = ?10",
            params![
                duration_frames,
                end_sec,
                end_tc,
                end_tc,
                SYSTEM_MARKER_END,
                origin_part_id,
                origin_local_frame,
                frame_to_seconds(origin_local_frame, timeline_fps),
                now,
                marker_id
            ],
        )?;
        conn.execute(
            "UPDATE story_markers
             SET system_role = ''
             WHERE system_role = ?1 AND marker_id != ?2",
            params![SYSTEM_MARKER_END, marker_id],
        )?;
        return Ok(());
    }

    let marker_id = new_marker_id();
    conn.execute(
        "INSERT INTO story_markers
            (marker_id, timeline_frame, timeline_sec, tc, label, sort_index, system_role,
             origin_part_id, origin_local_frame, origin_local_sec, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            marker_id,
            duration_frames,
            end_sec,
            end_tc,
            end_tc,
            SYSTEM_MARKER_END,
            origin_part_id,
            origin_local_frame,
            frame_to_seconds(origin_local_frame, timeline_fps),
            now
        ],
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
    backfill_marker_frames(conn, timeline_fps)?;
    ensure_start_marker(conn, timeline_fps)?;
    let parts = list_parts(conn)?;
    let duration_frames = timeline_duration_frames_from_parts(&parts, timeline_fps);
    ensure_end_marker(conn, timeline_fps, &parts, duration_frames)?;
    refresh_marker_sort_indices(conn)?;

    let now = crate::project::db::now_str();
    let markers = list_markers(conn)?;

    let mut slots: Vec<(String, i64, i64, i64, String, String, String)> = Vec::new();
    if markers.len() >= 2 {
        for i in 0..markers.len() - 1 {
            let start = markers[i].timeline_frame.max(0);
            let end = markers[i + 1].timeline_frame.max(start);
            if end <= start {
                continue;
            }
            let end = end.min(duration_frames.max(end));
            let start_sec = round3(frame_to_seconds(start, timeline_fps));
            let end_sec = round3(frame_to_seconds(end, timeline_fps));
            let sig = slot_signature(start_sec, end_sec);
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

    let new_slot_specs: Vec<(i64, i64, f64, f64, String)> = slots
        .iter()
        .map(|(_, _, start, end, _, _, sig)| {
            (
                *start,
                *end,
                round3(frame_to_seconds(*start, timeline_fps)),
                round3(frame_to_seconds(*end, timeline_fps)),
                sig.clone(),
            )
        })
        .collect();
    super::covers::normalize_covers_for_slots(conn, &new_slot_specs)?;

    conn.execute("DELETE FROM story_marker_slots", [])?;
    for (slot_id, slot_index, start_frame, end_frame, start_mid, end_mid, sig) in slots {
        let duration_frames = (end_frame - start_frame).max(0);
        let start_sec = round3(frame_to_seconds(start_frame, timeline_fps));
        let end_sec = round3(frame_to_seconds(end_frame, timeline_fps));
        let dur = round3(frame_to_seconds(duration_frames, timeline_fps));
        conn.execute(
            "INSERT INTO story_marker_slots
                (slot_id, slot_index, start_frame, end_frame, duration_frames,
                 start_sec, end_sec, duration_sec,
                 start_marker_id, end_marker_id, slot_signature, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                slot_id,
                slot_index,
                start_frame,
                end_frame,
                duration_frames,
                start_sec,
                end_sec,
                dur,
                start_mid,
                end_mid,
                sig,
                now
            ],
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
    let timeline_frame = seconds_to_frame(timeline_sec.max(0.0), timeline_fps);
    let origin_local_frame =
        origin_local_sec.map(|sec| seconds_to_frame(sec.max(0.0), timeline_fps));
    create_marker_frame(
        conn,
        timeline_frame,
        label,
        origin_part_id,
        origin_local_frame,
        timeline_fps,
    )
}

pub fn create_marker_frame(
    conn: &Connection,
    timeline_frame: i64,
    label: Option<&str>,
    origin_part_id: Option<&str>,
    origin_local_frame: Option<i64>,
    timeline_fps: f64,
) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    if timeline_frame < 0 {
        return Err("timeline_frame must be >= 0".into());
    }
    let timeline_frame = timeline_frame.max(0);
    let timeline_sec = round3(frame_to_seconds(timeline_frame, timeline_fps));
    if timeline_sec < 0.0 {
        return Err("timeline_sec must be >= 0".into());
    }
    let markers = list_markers(conn).map_err(|e| e.to_string())?;
    if let Some(existing) = markers.iter().find(|m| m.timeline_frame == timeline_frame) {
        let origin_part_id = origin_part_id.unwrap_or("").trim();
        let origin_local_sec =
            origin_local_frame.map(|frame| frame_to_seconds(frame.max(0), timeline_fps));
        let tc = seconds_to_timecode(timeline_sec, timeline_fps);
        let label = label
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(existing.label.as_str());
        conn.execute(
            "UPDATE story_markers
             SET tc = ?1, label = ?2,
                 origin_part_id = CASE WHEN TRIM(?3) != '' THEN ?3 ELSE origin_part_id END,
                 origin_local_frame = COALESCE(?4, origin_local_frame),
                 origin_local_sec = COALESCE(?5, origin_local_sec),
                 updated_at = ?6
             WHERE marker_id = ?7",
            params![
                tc,
                label,
                origin_part_id,
                origin_local_frame,
                origin_local_sec,
                crate::project::db::now_str(),
                existing.marker_id
            ],
        )
        .map_err(|e| e.to_string())?;
        return finalize_story_mutation(conn, timeline_fps).map_err(|e| e.to_string());
    }
    let marker_id = new_marker_id();
    let now = crate::project::db::now_str();
    let origin_part_id = origin_part_id.unwrap_or("").trim();
    let origin_local_sec =
        origin_local_frame.map(|frame| frame_to_seconds(frame.max(0), timeline_fps));
    let tc = seconds_to_timecode(timeline_sec, timeline_fps);
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&tc)
        .to_string();
    conn.execute(
        "INSERT INTO story_markers
            (marker_id, timeline_frame, timeline_sec, tc, label, sort_index, origin_part_id,
             origin_local_frame, origin_local_sec, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?9)",
        params![
            marker_id,
            timeline_frame,
            timeline_sec,
            tc,
            label,
            origin_part_id,
            origin_local_frame,
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
    let markers = list_markers(conn).map_err(|e| e.to_string())?;
    let marker = markers
        .iter()
        .find(|marker| marker.marker_id == marker_id)
        .ok_or_else(|| format!("marker not found: {marker_id}"))?;
    let duration_frames = timeline_duration_frames_from_parts(
        &list_parts(conn).map_err(|error| error.to_string())?,
        timeline_fps,
    );
    if marker.timeline_frame == 0 || marker.system_role == SYSTEM_MARKER_START {
        return Err("Početni M marker je zaključan.".into());
    }
    if marker.system_role == SYSTEM_MARKER_END || marker.timeline_frame == duration_frames {
        return Err("Završni M marker je zaključan.".into());
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
    update_marker_frame(
        conn,
        marker_id,
        seconds_to_frame(timeline_sec.max(0.0), timeline_fps),
        label,
        timeline_fps,
    )
}

pub fn update_marker_frame(
    conn: &Connection,
    marker_id: &str,
    timeline_frame: i64,
    label: Option<&str>,
    timeline_fps: f64,
) -> Result<(), String> {
    let timeline_fps = require_timeline_fps(timeline_fps)?;
    let marker_id = marker_id.trim();
    if marker_id.is_empty() {
        return Err("marker_id required".into());
    }
    if timeline_frame < 0 {
        return Err("timeline_frame must be >= 0".into());
    }
    let timeline_frame = timeline_frame.max(0);
    let duration_frames = timeline_duration_frames_from_parts(
        &list_parts(conn).map_err(|error| error.to_string())?,
        timeline_fps,
    );
    if timeline_frame > duration_frames {
        let duration = frame_to_seconds(duration_frames, timeline_fps);
        return Err(format!(
            "M marker mora biti unutar trajanja storyja ({duration:.3} s)."
        ));
    }
    let markers = list_markers(conn).map_err(|error| error.to_string())?;
    let marker = markers
        .iter()
        .find(|marker| marker.marker_id == marker_id)
        .ok_or_else(|| format!("marker not found: {marker_id}"))?;
    if marker.timeline_frame == 0 || marker.system_role == SYSTEM_MARKER_START {
        return Err("Početni M marker je zaključan.".into());
    }
    if marker.system_role == SYSTEM_MARKER_END || marker.timeline_frame == duration_frames {
        return Err("Završni M marker je zaključan.".into());
    }
    if markers
        .iter()
        .any(|other| other.marker_id != marker_id && other.timeline_frame == timeline_frame)
    {
        return Err(format!(
            "marker already exists at timeline_frame={timeline_frame}"
        ));
    }
    let timeline_sec = round3(frame_to_seconds(timeline_frame, timeline_fps));
    let tc = seconds_to_timecode(timeline_sec, timeline_fps);
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(marker.label.as_str());
    conn.execute(
        "UPDATE story_markers
         SET timeline_frame = ?1, timeline_sec = ?2, tc = ?3, label = ?4, updated_at = ?5
         WHERE marker_id = ?6",
        params![
            timeline_frame,
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
    if markers[idx].timeline_frame == 0 || markers[idx].system_role == SYSTEM_MARKER_START {
        return Err("Početni M marker je zaključan.".into());
    }
    if markers[idx].system_role == SYSTEM_MARKER_END {
        return Err("Završni M marker je zaključan.".into());
    }
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
    if markers[swap_with].timeline_frame == 0
        || markers[swap_with].system_role == SYSTEM_MARKER_START
        || markers[swap_with].system_role == SYSTEM_MARKER_END
    {
        return Ok(());
    }
    let a_id = markers[idx].marker_id.clone();
    let b_id = markers[swap_with].marker_id.clone();
    let a_frame = markers[idx].timeline_frame;
    let b_frame = markers[swap_with].timeline_frame;
    let a_sec = frame_to_seconds(a_frame, timeline_fps);
    let b_sec = frame_to_seconds(b_frame, timeline_fps);
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
        "UPDATE story_markers
         SET timeline_frame = ?1, timeline_sec = ?2, tc = ?3, label = ?4, updated_at = ?5
         WHERE marker_id = ?6",
        params![b_frame, b_sec, a_tc, a_label, now, a_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE story_markers
         SET timeline_frame = ?1, timeline_sec = ?2, tc = ?3, label = ?4, updated_at = ?5
         WHERE marker_id = ?6",
        params![a_frame, a_sec, b_tc, b_label, now, b_id],
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
pub fn shift_markers_after_part_removal_frames(
    conn: &Connection,
    part_start_frame: i64,
    part_end_frame: i64,
    timeline_fps: f64,
) -> rusqlite::Result<()> {
    let timeline_fps = require_timeline_fps_sql(timeline_fps)?;
    let span = part_end_frame - part_start_frame;
    if span <= 0 {
        return Ok(());
    }
    let markers = list_markers(conn)?;
    let now = crate::project::db::now_str();
    for m in markers {
        let frame = m.timeline_frame.max(0);
        if frame > part_start_frame && frame < part_end_frame {
            conn.execute(
                "DELETE FROM story_markers WHERE marker_id = ?1",
                params![m.marker_id],
            )?;
        } else if frame >= part_end_frame {
            let new_frame = (frame - span).max(0);
            let new_sec = round3(frame_to_seconds(new_frame, timeline_fps));
            let tc = marker_timecode(new_sec, timeline_fps);
            conn.execute(
                "UPDATE story_markers
                 SET timeline_frame = ?1, timeline_sec = ?2, tc = ?3, updated_at = ?4
                 WHERE marker_id = ?5",
                params![new_frame, new_sec, tc, now, m.marker_id],
            )?;
        }
    }
    Ok(())
}

pub fn part_timeline_window_frames(
    parts: &[StoryPartRow],
    part_id: &str,
    timeline_fps: f64,
) -> Option<(i64, i64)> {
    let mut cursor = 0;
    for part in parts {
        let span = part_span_frames(part, timeline_fps).max(0);
        if part.part_id == part_id {
            return Some((cursor, cursor + span));
        }
        cursor += span;
    }
    None
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

pub fn local_to_timeline_frame(
    parts: &[StoryPartRow],
    part_id: &str,
    local_frame: i64,
    timeline_fps: f64,
) -> Result<i64, String> {
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let (start, end) = part_timeline_window_frames(parts, part_id, timeline_fps)
        .ok_or_else(|| format!("part not found: {part_id}"))?;
    let span = (end - start).max(0);
    let local = local_frame.max(0);
    let clamped = if span > 0 { local.min(span) } else { 0 };
    Ok(start + clamped)
}

pub fn resolve_marker_timeline_frame(
    parts: &[StoryPartRow],
    timeline_frame: Option<i64>,
    part_id: Option<&str>,
    local_frame: Option<i64>,
    timeline_fps: f64,
) -> Result<(i64, String, Option<i64>), String> {
    if let Some(frame) = timeline_frame {
        if frame < 0 {
            return Err("timeline_frame must be >= 0".into());
        }
        let origin_part = part_id.unwrap_or("").trim().to_string();
        return Ok((frame, origin_part, local_frame));
    }
    let part_id = part_id
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "timeline_frame or part_id required".to_string())?;
    let local = local_frame.unwrap_or(0);
    let global = local_to_timeline_frame(parts, part_id, local, timeline_fps)?;
    Ok((global, part_id.trim().to_string(), Some(local)))
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

pub fn delete_markers_for_part(
    conn: &Connection,
    part_id: &str,
    timeline_fps: f64,
    parts_before_delete: &[StoryPartRow],
) -> rusqlite::Result<()> {
    if let Some((start, end)) =
        part_timeline_window_frames(parts_before_delete, part_id, timeline_fps)
    {
        shift_markers_after_part_removal_frames(conn, start, end, timeline_fps)?;
    }
    Ok(())
}

pub fn ensure_materialized_slots(conn: &Connection, timeline_fps: f64) -> rusqlite::Result<()> {
    let _ = list_parts(conn)?;
    recompute_marker_slots(conn, timeline_fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        super::super::db::ensure_schema(&conn).unwrap();
        conn
    }

    fn insert_part(conn: &Connection, part_id: &str, sort_index: i64, duration_frames: i64) {
        let now = crate::project::db::now_str();
        conn.execute(
            "INSERT INTO story_parts
                (part_id, kind, sort_index, clip_id, fps, in_frame, out_frame,
                 duration_frames, created_at, updated_at)
             VALUES (?1, 'tonovi', ?2, ?3, 50.0, 0, ?4, ?4, ?5, ?5)",
            params![
                part_id,
                sort_index,
                format!("clip_{part_id}"),
                duration_frames,
                now
            ],
        )
        .unwrap();
    }

    #[test]
    fn materializes_start_and_end_markers_as_program_boundaries() {
        let conn = setup_conn();
        insert_part(&conn, "a", 0, 50);
        insert_part(&conn, "b", 1, 30);

        recompute_marker_slots(&conn, 50.0).unwrap();

        let markers = list_markers(&conn).unwrap();
        assert_eq!(markers.len(), 2);
        assert!(markers
            .iter()
            .any(|m| m.timeline_frame == 0 && m.system_role == SYSTEM_MARKER_START));
        assert!(markers
            .iter()
            .any(|m| m.timeline_frame == 80 && m.system_role == SYSTEM_MARKER_END));

        let slots = list_marker_slots(&conn).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].start_frame, 0);
        assert_eq!(slots[0].end_frame, 80);
        assert_eq!(slots[0].duration_frames, 80);
    }

    #[test]
    fn end_marker_moves_with_program_duration() {
        let conn = setup_conn();
        insert_part(&conn, "a", 0, 50);
        recompute_marker_slots(&conn, 50.0).unwrap();
        let end_id = list_markers(&conn)
            .unwrap()
            .into_iter()
            .find(|m| m.system_role == SYSTEM_MARKER_END)
            .unwrap()
            .marker_id;

        conn.execute(
            "UPDATE story_parts SET out_frame = 90, duration_frames = 90 WHERE part_id = 'a'",
            [],
        )
        .unwrap();
        recompute_marker_slots(&conn, 50.0).unwrap();

        let markers = list_markers(&conn).unwrap();
        assert_eq!(
            markers
                .iter()
                .filter(|m| m.system_role == SYSTEM_MARKER_END)
                .count(),
            1
        );
        let end = markers
            .iter()
            .find(|m| m.system_role == SYSTEM_MARKER_END)
            .unwrap();
        assert_eq!(end.marker_id, end_id);
        assert_eq!(end.timeline_frame, 90);
        assert!(!markers.iter().any(|m| m.timeline_frame == 50));

        let slots = list_marker_slots(&conn).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].start_frame, 0);
        assert_eq!(slots[0].end_frame, 90);
    }

    #[test]
    fn internal_marker_splits_slots_between_system_boundaries() {
        let conn = setup_conn();
        insert_part(&conn, "a", 0, 100);
        recompute_marker_slots(&conn, 50.0).unwrap();

        create_marker_frame(&conn, 40, Some("M1"), Some("a"), Some(40), 50.0).unwrap();

        let markers = list_markers(&conn).unwrap();
        assert_eq!(markers.len(), 3);
        assert!(markers.iter().any(|m| m.timeline_frame == 40));
        let slots = list_marker_slots(&conn).unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!((slots[0].start_frame, slots[0].end_frame), (0, 40));
        assert_eq!((slots[1].start_frame, slots[1].end_frame), (40, 100));
    }
}
