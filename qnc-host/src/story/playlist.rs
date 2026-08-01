//! Canonical editorial playlist — single projection for play, export XML, export EDL.
//!
//! Reads montage rows from SQLite; no ffprobe in the hot path.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::project::db::{open_project, project_timeline_fps, ProjectPaths};

use super::covers::{list_covers, StoryCoverRow};
use super::db::{ensure_schema, list_parts, StoryPartRow};
use super::markers::{
    cover_slot_duration_sec, part_span_seconds, timeline_duration_from_parts, TIMELINE_EPS,
};

/// Seconds before a cover slot to begin stream preload (playback worker, phase 4).
pub const DEFAULT_PRELOAD_LEAD_SEC: f64 = 6.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamRef {
    Part { part_id: String },
    Cover { cover_id: String },
    VirtualShot { virtual_shot_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditorialCover {
    pub cover_id: String,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub title: String,
    pub timeline_start_sec: f64,
    pub timeline_end_sec: f64,
    pub local_start_sec: f64,
    pub local_end_sec: f64,
    pub slot_duration_sec: f64,
    pub source_in_sec: f64,
    pub source_out_sec: f64,
    /// When cover timeline starts before segment global start (ton under cover).
    pub source_offset_sec: f64,
    pub streamable: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stream_error: String,
    pub source: StreamRef,
    pub preload_lead_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditorialSegment {
    pub part_id: String,
    pub kind: String,
    pub title: String,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub global_start_sec: f64,
    pub global_end_sec: f64,
    pub duration_sec: f64,
    pub streamable: bool,
    pub source: StreamRef,
    pub covers: Vec<EditorialCover>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditorialPlaylist {
    pub project_id: String,
    pub timeline_fps: f64,
    pub duration_sec: f64,
    pub segments: Vec<EditorialSegment>,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn cover_playback_trim(conn: &Connection, cover: &StoryCoverRow) -> (f64, f64, f64) {
    let in_s = cover.in_seconds.unwrap_or(0.0).max(0.0);
    let raw_out = cover.out_seconds.unwrap_or(in_s).max(in_s);
    let source_span = raw_out - in_s;
    let slot_dur = cover_slot_duration_sec(conn, cover)
        .unwrap_or_else(|_| (cover.timeline_end_sec - cover.timeline_start_sec).max(0.0));
    let playback_span = if slot_dur > TIMELINE_EPS {
        source_span.min(slot_dur)
    } else {
        source_span
    };
    (in_s, in_s + playback_span.max(0.0), slot_dur)
}

fn cover_is_streamable(cover: &StoryCoverRow) -> bool {
    !cover.cover_id.trim().is_empty() && !cover.virtual_shot_id.trim().is_empty()
}

fn map_covers_for_segment(
    conn: &Connection,
    covers: &[StoryCoverRow],
    part_start: f64,
    part_end: f64,
    span: f64,
) -> Vec<EditorialCover> {
    let mut out = Vec::new();
    for cover in covers {
        let c_start = cover.timeline_start_sec;
        let c_end = cover.timeline_end_sec;
        if c_end <= part_start + TIMELINE_EPS || c_start >= part_end - TIMELINE_EPS {
            continue;
        }
        let local_start = round3((c_start - part_start).max(0.0));
        let local_end = round3((c_end - part_start).min(span));
        if local_end <= local_start + TIMELINE_EPS {
            continue;
        }
        let (source_in, source_out, slot_duration) = cover_playback_trim(conn, cover);
        let streamable = cover_is_streamable(cover);
        let stream_error = if streamable {
            String::new()
        } else if cover.virtual_shot_id.trim().is_empty() {
            "missing virtual_shot_id".into()
        } else {
            "missing cover_id".into()
        };
        out.push(EditorialCover {
            cover_id: cover.cover_id.clone(),
            clip_id: cover.clip_id.clone(),
            virtual_shot_id: cover.virtual_shot_id.clone(),
            title: cover.title.clone(),
            timeline_start_sec: round3(c_start),
            timeline_end_sec: round3(c_end),
            local_start_sec: local_start,
            local_end_sec: local_end,
            slot_duration_sec: round3(local_end - local_start),
            source_in_sec: round3(source_in),
            source_out_sec: round3(source_out),
            source_offset_sec: round3((part_start - c_start).max(0.0)),
            streamable,
            stream_error,
            source: StreamRef::Cover {
                cover_id: cover.cover_id.clone(),
            },
            preload_lead_sec: DEFAULT_PRELOAD_LEAD_SEC,
        });
        let _ = slot_duration;
    }
    out.sort_by(|a, b| {
        a.local_start_sec
            .partial_cmp(&b.local_start_sec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn build_segments(
    conn: &Connection,
    parts: &[StoryPartRow],
    covers: &[StoryCoverRow],
) -> Vec<EditorialSegment> {
    let mut segments = Vec::new();
    let mut global_start = 0.0;
    for part in parts {
        let span = part_span_seconds(part);
        let global_end = round3(global_start + span);
        let part_covers = map_covers_for_segment(conn, covers, global_start, global_end, span);
        let clip_id = part.clip_id.trim().to_string();
        segments.push(EditorialSegment {
            part_id: part.part_id.clone(),
            kind: part.kind.clone(),
            title: part.title.clone(),
            clip_id: clip_id.clone(),
            virtual_shot_id: part.virtual_shot_id.clone(),
            global_start_sec: round3(global_start),
            global_end_sec: global_end,
            duration_sec: round3(span),
            streamable: !clip_id.is_empty(),
            source: StreamRef::Part {
                part_id: part.part_id.clone(),
            },
            covers: part_covers,
        });
        global_start = global_end;
    }
    segments
}

/// Build the canonical editorial playlist from project DB rows.
pub fn build_editorial_playlist(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<EditorialPlaylist, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_schema(&conn).map_err(|e| e.to_string())?;
    let timeline_fps = project_timeline_fps(paths, pid);
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let covers = list_covers(&conn).map_err(|e| e.to_string())?;
    let segments = build_segments(&conn, &parts, &covers);
    let duration_sec = if segments.is_empty() {
        0.0
    } else {
        round3(timeline_duration_from_parts(&parts))
    };
    Ok(EditorialPlaylist {
        project_id: pid.to_string(),
        timeline_fps,
        duration_sec,
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::ProjectPaths;
    use crate::story::db::{create_cover, create_marker, create_part, ensure_schema, load_state};

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn seed_virtual_shot(paths: &ProjectPaths, project_id: &str, conn: &Connection) {
        crate::virtual_shots::db::ensure(paths, project_id, conn).unwrap();
        conn.execute(
            "INSERT INTO virtual_shots
                (shot_id, clip_id, kind, source_shot_id, locked, display_name, source, quality,
                 duration_seconds, in_seconds, out_seconds, fps, source_fps, timeline_fps,
                 in_frame, out_frame, duration_frames, timeline_duration_frames,
                 duration_label, duration_color_key, in_tc, out_tc, description, category_key,
                 created_at, updated_at)
             VALUES ('shot_a', 'clip_a', 'derived', '', 0, '', 'manual', 'ok', 2.0, 1.0, 3.0,
                     25.0, 25.0, 25.0, 25, 75, 50, 50, '2:00', 'under_3',
                     '00:00:01:00', '00:00:03:00', 'Opis', 'manual_cut', 'epoch_1', 'epoch_1')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn build_editorial_playlist_empty_project() {
        let base = std::env::temp_dir().join(format!("qnc_playlist_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_empty";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.project_id, project_id);
        assert!(plan.segments.is_empty());
        assert_eq!(plan.duration_sec, 0.0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn build_editorial_playlist_ton_segment_with_cover() {
        let base = std::env::temp_dir().join(format!("qnc_playlist_cover_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_cover";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        seed_virtual_shot(&paths, project_id, &conn);
        drop(conn);

        create_part(&paths, project_id, "tonovi", Some("shot_a")).unwrap();
        create_marker(&paths, project_id, Some(1.5), None, Some("slot-end"), None).unwrap();
        let state = load_state(&paths, project_id).unwrap();
        let slot_id = state
            .get("marker_slots")
            .and_then(|v| v.as_array())
            .and_then(|slots| slots.first())
            .and_then(|s| s.get("slot_id"))
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        create_cover(
            &paths,
            project_id,
            &slot_id,
            None,
            Some("shot_a"),
            None,
            None,
        )
        .unwrap();

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.segments.len(), 1);
        let seg = &plan.segments[0];
        assert_eq!(seg.global_start_sec, 0.0);
        assert!(seg.duration_sec > 0.0);
        assert_eq!(seg.global_end_sec, seg.duration_sec);
        assert!(seg.streamable);
        assert_eq!(
            seg.source,
            StreamRef::Part {
                part_id: seg.part_id.clone()
            }
        );
        assert!(
            !seg.covers.is_empty(),
            "expected at least one cover in slot"
        );
        let cover = &seg.covers[0];
        assert!(cover.streamable);
        assert_eq!(
            cover.source,
            StreamRef::Cover {
                cover_id: cover.cover_id.clone()
            }
        );
        assert!(cover.local_end_sec > cover.local_start_sec);
        assert_eq!(cover.preload_lead_sec, DEFAULT_PRELOAD_LEAD_SEC);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn build_editorial_playlist_two_segments_global_offsets() {
        let base = std::env::temp_dir().join(format!("qnc_playlist_two_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_two";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        seed_virtual_shot(&paths, project_id, &conn);
        drop(conn);

        create_part(&paths, project_id, "tonovi", Some("shot_a")).unwrap();
        create_part(&paths, project_id, "offovi", Some("shot_a")).unwrap();

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.segments.len(), 2);
        assert!(
            plan.segments[1].global_start_sec >= plan.segments[0].global_end_sec - TIMELINE_EPS
        );
        assert_eq!(plan.duration_sec, plan.segments[1].global_end_sec);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn build_editorial_playlist_cover_without_virtual_shot_not_streamable() {
        let base =
            std::env::temp_dir().join(format!("qnc_playlist_nostream_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_nostream";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO story_parts
                (part_id, kind, sort_index, title, text, clip_id, virtual_shot_id,
                 in_tc, out_tc, in_seconds, out_seconds, fps,
                 in_frame, out_frame, duration_frames,
                 duration_label, duration_color_key, created_at, updated_at)
             VALUES ('part_manual', 'tonovi', 0, '', '', 'clip_x', '',
                     '', '', 0, 3, 25, 0, 75, 75, '3:00', 'under_3', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO story_covers
                (cover_id, timeline_start_sec, timeline_end_sec, slot_signature, slot_index,
                 clip_id, virtual_shot_id, title, note, in_tc, out_tc, in_seconds, out_seconds,
                 sort_index, created_at, updated_at)
             VALUES ('cover_bad', 0.5, 1.5, '', 0, 'clip_x', '', 'Bad', '', '', '', 0, 1,
                     0, 't', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].covers.len(), 1);
        let cover = &plan.segments[0].covers[0];
        assert!(!cover.streamable);
        assert!(cover.stream_error.contains("virtual_shot_id"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
