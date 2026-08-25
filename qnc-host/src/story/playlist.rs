//! Raw editorial playlist — the montage result stored in source coordinates.
//!
//! This is not the Broadcast Player input. Preview playback derives a flat
//! playlist input from this structure, while export can later relink the same
//! source-frame ranges to full-resolution originals.
//!
//! Reads montage rows from SQLite; no ffprobe in the hot path.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::frame_time::{frame_to_seconds, is_valid_fps, seconds_to_frame};
use crate::project::db::{open_project, ProjectPaths};
use crate::project::ProjectDbBroker;

use super::covers::{list_covers, StoryCoverRow};
use super::db::{ensure_schema, list_parts, sync_story_part_source_fps, StoryPartRow};
use super::markers::{
    part_span_frames, part_span_seconds, timeline_duration_frames_from_parts, TIMELINE_EPS,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTimebase {
    pub fps_num: i64,
    pub fps_den: i64,
}

impl SourceTimebase {
    fn from_parts(fps_num: i64, fps_den: i64) -> Self {
        Self {
            fps_num: fps_num.max(0),
            fps_den: fps_den.max(1),
        }
    }

    fn is_valid(self) -> bool {
        self.fps_num > 0 && self.fps_den > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditorialCover {
    pub cover_id: String,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub title: String,
    pub timeline_start_frame: i64,
    pub timeline_end_frame: i64,
    pub timeline_start_sec: f64,
    pub timeline_end_sec: f64,
    pub local_start_frame: i64,
    pub local_end_frame: i64,
    pub local_start_sec: f64,
    pub local_end_sec: f64,
    pub slot_duration_frames: i64,
    pub slot_duration_sec: f64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
    pub source_timebase: SourceTimebase,
    pub source_in_sec: f64,
    pub source_out_sec: f64,
    /// When cover timeline starts before segment global start (ton under cover).
    pub source_offset_frames: i64,
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
    pub global_start_frame: i64,
    pub global_end_frame: i64,
    pub duration_frames: i64,
    pub global_start_sec: f64,
    pub global_end_sec: f64,
    pub duration_sec: f64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
    pub source_timebase: SourceTimebase,
    pub streamable: bool,
    pub source: StreamRef,
    pub covers: Vec<EditorialCover>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditorialPlaylist {
    pub project_id: String,
    pub timeline_fps: f64,
    pub duration_frames: i64,
    pub duration_sec: f64,
    pub segments: Vec<EditorialSegment>,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[derive(Debug, Clone, Copy)]
struct CoverPlaybackTrim {
    source_in_frame: i64,
    source_out_frame: i64,
    source_fps: f64,
}

fn cover_playback_trim(cover: &StoryCoverRow, fallback_fps: f64) -> CoverPlaybackTrim {
    let source_fps = if is_valid_fps(cover.source_fps) {
        cover.source_fps
    } else {
        fallback_fps
    };
    let (source_in_frame, source_out_frame) = if cover.source_out_frame > cover.source_in_frame {
        let source_in_frame = cover.source_in_frame.max(0);
        (
            source_in_frame,
            cover.source_out_frame.max(source_in_frame + 1),
        )
    } else if is_valid_fps(source_fps) {
        let in_sec = cover.in_seconds.unwrap_or(0.0).max(0.0);
        let out_sec = cover.out_seconds.unwrap_or(in_sec).max(in_sec);
        let in_frame = seconds_to_frame(in_sec, source_fps).max(0);
        (
            in_frame,
            seconds_to_frame(out_sec, source_fps).max(in_frame + 1),
        )
    } else {
        (0, 1)
    };
    CoverPlaybackTrim {
        source_in_frame,
        source_out_frame,
        source_fps,
    }
}

fn cover_is_streamable(cover: &StoryCoverRow) -> bool {
    !cover.cover_id.trim().is_empty() && !cover.virtual_shot_id.trim().is_empty()
}

fn map_covers_for_segment(
    covers: &[StoryCoverRow],
    part_start_frame: i64,
    part_end_frame: i64,
    span_frames: i64,
    timeline_fps: f64,
) -> Vec<EditorialCover> {
    let mut out = Vec::new();
    let part_start = frame_to_seconds(part_start_frame, timeline_fps);
    let part_end = frame_to_seconds(part_end_frame, timeline_fps);
    for cover in covers {
        let c_start_frame = if cover.timeline_start_frame > 0 {
            cover.timeline_start_frame
        } else {
            seconds_to_frame(cover.timeline_start_sec, timeline_fps)
        };
        let c_end_frame = if cover.timeline_end_frame > 0 {
            cover.timeline_end_frame
        } else {
            seconds_to_frame(cover.timeline_end_sec, timeline_fps)
        };
        let c_start = frame_to_seconds(c_start_frame, timeline_fps);
        let c_end = frame_to_seconds(c_end_frame, timeline_fps);
        if c_end <= part_start + TIMELINE_EPS || c_start >= part_end - TIMELINE_EPS {
            continue;
        }
        let local_start_frame = (c_start_frame - part_start_frame).max(0);
        let local_end_frame = (c_end_frame - part_start_frame).min(span_frames);
        if local_end_frame <= local_start_frame {
            continue;
        }
        let trim = cover_playback_trim(cover, timeline_fps);
        let source_in_frame = trim.source_in_frame;
        let source_out_frame = trim.source_out_frame.max(source_in_frame + 1);
        let slot_duration_frames = (local_end_frame - local_start_frame).max(0);
        let source_offset_frames = (part_start_frame - c_start_frame).max(0);
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
            timeline_start_frame: c_start_frame,
            timeline_end_frame: c_end_frame,
            timeline_start_sec: round3(c_start),
            timeline_end_sec: round3(c_end),
            local_start_frame,
            local_end_frame,
            local_start_sec: round3(frame_to_seconds(local_start_frame, timeline_fps)),
            local_end_sec: round3(frame_to_seconds(local_end_frame, timeline_fps)),
            slot_duration_frames,
            slot_duration_sec: round3(frame_to_seconds(
                (local_end_frame - local_start_frame).max(0),
                timeline_fps,
            )),
            source_in_frame,
            source_out_frame,
            source_fps: trim.source_fps,
            source_timebase: SourceTimebase::from_parts(cover.source_fps_num, cover.source_fps_den),
            source_in_sec: if is_valid_fps(trim.source_fps) {
                round3(frame_to_seconds(source_in_frame, trim.source_fps))
            } else {
                0.0
            },
            source_out_sec: if is_valid_fps(trim.source_fps) {
                round3(frame_to_seconds(source_out_frame, trim.source_fps))
            } else {
                0.0
            },
            source_offset_frames,
            source_offset_sec: round3((part_start - c_start).max(0.0)),
            streamable,
            stream_error,
            source: StreamRef::Cover {
                cover_id: cover.cover_id.clone(),
            },
            preload_lead_sec: DEFAULT_PRELOAD_LEAD_SEC,
        });
    }
    out.sort_by(|a, b| {
        a.local_start_sec
            .partial_cmp(&b.local_start_sec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn build_segments(
    parts: &[StoryPartRow],
    covers: &[StoryCoverRow],
    timeline_fps: f64,
) -> Vec<EditorialSegment> {
    let mut segments = Vec::new();
    let mut global_start_frame = 0;
    let mut global_start_sec = 0.0;
    for part in parts {
        let span_frames = part_span_frames(part);
        let global_end_frame = global_start_frame + span_frames;
        let span = part_span_seconds(part);
        let global_end = global_start_sec + span;
        let part_covers = map_covers_for_segment(
            covers,
            global_start_frame,
            global_end_frame,
            span_frames,
            timeline_fps,
        );
        let clip_id = part.clip_id.trim().to_string();
        segments.push(EditorialSegment {
            part_id: part.part_id.clone(),
            kind: part.kind.clone(),
            title: part.title.clone(),
            clip_id: clip_id.clone(),
            virtual_shot_id: part.virtual_shot_id.clone(),
            global_start_frame,
            global_end_frame,
            duration_frames: span_frames,
            global_start_sec: round3(global_start_sec),
            global_end_sec: round3(global_end),
            duration_sec: round3(span),
            source_in_frame: part.in_frame.max(0),
            source_out_frame: part.out_frame.max(part.in_frame + 1),
            source_fps: part.fps,
            source_timebase: SourceTimebase::from_parts(part.source_fps_num, part.source_fps_den),
            streamable: !clip_id.is_empty(),
            source: StreamRef::Part {
                part_id: part.part_id.clone(),
            },
            covers: part_covers,
        });
        global_start_frame = global_end_frame;
        global_start_sec = global_end;
    }
    segments
}

fn story_program_source_fps(parts: &[StoryPartRow]) -> f64 {
    parts
        .iter()
        .map(|part| part.fps)
        .find(|fps| is_valid_fps(*fps))
        .unwrap_or(0.0)
}

fn validate_source_timebases(segments: &[EditorialSegment]) -> Result<(), String> {
    for segment in segments {
        if segment.streamable && !segment.source_timebase.is_valid() {
            return Err(format!(
                "Segment '{}' nema originalni source timebase; ponovi import/probe.",
                segment.part_id
            ));
        }
        for cover in &segment.covers {
            if cover.streamable && !cover.source_timebase.is_valid() {
                return Err(format!(
                    "Pokrivalica '{}' nema originalni source timebase; ponovi import/probe.",
                    cover.cover_id
                ));
            }
        }
    }
    Ok(())
}

/// Build the canonical editorial playlist from project DB rows.
pub fn build_editorial_playlist_with_broker(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
) -> Result<EditorialPlaylist, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    project_db.with_project_write(pid, |conn| {
        build_editorial_playlist_from_conn(paths, pid, conn)
    })
}

#[allow(dead_code)]
pub fn build_editorial_playlist(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<EditorialPlaylist, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    build_editorial_playlist_from_conn(paths, pid, &conn)
}

fn build_editorial_playlist_from_conn(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<EditorialPlaylist, String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    sync_story_part_source_fps(paths, project_id, conn)?;
    let parts = list_parts(conn).map_err(|e| e.to_string())?;
    let timeline_fps = story_program_source_fps(&parts);
    let covers = list_covers(conn).map_err(|e| e.to_string())?;
    let segments = build_segments(&parts, &covers, timeline_fps);
    validate_source_timebases(&segments)?;
    let duration_frames = if segments.is_empty() {
        0
    } else {
        timeline_duration_frames_from_parts(&parts)
    };
    let duration_sec = if duration_frames <= 0 {
        0.0
    } else {
        round3(segments.iter().map(|segment| segment.duration_sec).sum())
    };
    Ok(EditorialPlaylist {
        project_id: project_id.to_string(),
        timeline_fps,
        duration_frames,
        duration_sec,
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::ProjectPaths;
    use crate::story::db::{
        create_cover, create_marker, create_part, ensure_schema, load_state, SegmentRangeInput,
    };

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
                 source_fps_num, source_fps_den,
                 in_frame, out_frame, duration_frames, timeline_duration_frames,
                 duration_label, duration_color_key, in_tc, out_tc, description, category_key,
                 created_at, updated_at)
             VALUES ('shot_a', 'clip_a', 'virtual', '', 0, '', 'manual', 'ok', 2.0, 1.0, 3.0,
                     25.0, 25.0, 25.0, 25, 1, 25, 75, 50, 50, '2:00', 'under_3',
                     '00:00:01:00', '00:00:03:00', 'Opis', 'manual_cut', 'epoch_1', 'epoch_1')",
            [],
        )
        .unwrap();
    }

    fn seed_virtual_shot_frames(
        paths: &ProjectPaths,
        project_id: &str,
        conn: &Connection,
        shot_id: &str,
        clip_id: &str,
        fps: f64,
        in_frame: i64,
        out_frame: i64,
    ) {
        crate::virtual_shots::db::ensure(paths, project_id, conn).unwrap();
        let duration_frames = (out_frame - in_frame).max(1);
        let in_seconds = frame_to_seconds(in_frame, fps);
        let out_seconds = frame_to_seconds(out_frame, fps);
        let duration_seconds = frame_to_seconds(duration_frames, fps);
        conn.execute(
            "INSERT INTO virtual_shots
                (shot_id, clip_id, kind, source_shot_id, locked, display_name, source, quality,
                 duration_seconds, in_seconds, out_seconds, fps, source_fps, timeline_fps,
                 source_fps_num, source_fps_den,
                 in_frame, out_frame, duration_frames, timeline_duration_frames,
                 duration_label, duration_color_key, in_tc, out_tc, description, category_key,
                 created_at, updated_at)
             VALUES (?1, ?2, 'virtual', '', 0, '', 'manual', 'ok', ?3, ?4, ?5,
                     ?6, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?11, 'frames', 'under_3',
                     '', '', 'Opis', 'manual_cut', 'epoch_1', 'epoch_1')",
            rusqlite::params![
                shot_id,
                clip_id,
                duration_seconds,
                in_seconds,
                out_seconds,
                fps,
                fps.round() as i64,
                1,
                in_frame,
                out_frame,
                duration_frames
            ],
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
    fn build_editorial_playlist_source_less_segment_without_timebase() {
        let base =
            std::env::temp_dir().join(format!("qnc_playlist_source_less_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_source_less";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        create_part(&paths, project_id, "tonovi", None, None, None, None).unwrap();

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.project_id, project_id);
        assert_eq!(plan.timeline_fps, 0.0);
        assert_eq!(plan.segments.len(), 1);
        assert!(!plan.segments[0].streamable);
        assert_eq!(plan.segments[0].source_fps, 0.0);
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

        create_part(
            &paths,
            project_id,
            "tonovi",
            Some("shot_a"),
            None,
            None,
            None,
        )
        .unwrap();
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
            SegmentRangeInput::default(),
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
    fn build_editorial_playlist_cover_source_range_uses_cover_source_fps() {
        let base = std::env::temp_dir().join(format!(
            "qnc_playlist_cover_source_fps_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_cover_source_fps";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        seed_virtual_shot_frames(&paths, project_id, &conn, "part_25", "clip_a", 25.0, 0, 50);
        seed_virtual_shot_frames(
            &paths, project_id, &conn, "cover_50", "clip_b", 50.0, 100, 200,
        );
        drop(conn);

        create_part(
            &paths,
            project_id,
            "tonovi",
            Some("part_25"),
            None,
            None,
            None,
        )
        .unwrap();
        create_marker(&paths, project_id, Some(1.0), None, Some("slot-end"), None).unwrap();
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
            Some("cover_50"),
            None,
            None,
            SegmentRangeInput::default(),
        )
        .unwrap();

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        let cover = &plan.segments[0].covers[0];

        assert_eq!(plan.timeline_fps, 25.0);
        assert_eq!(cover.clip_id, "clip_b");
        assert_eq!(cover.source_in_frame, 100);
        assert_eq!(cover.source_out_frame, 200);
        assert_eq!(cover.source_in_sec, 2.0);
        assert_eq!(cover.source_out_sec, 4.0);
        assert_eq!(
            cover.source_timebase,
            SourceTimebase {
                fps_num: 50,
                fps_den: 1
            }
        );
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

        create_part(
            &paths,
            project_id,
            "tonovi",
            Some("shot_a"),
            None,
            None,
            None,
        )
        .unwrap();
        create_part(
            &paths,
            project_id,
            "offovi",
            Some("shot_a"),
            None,
            None,
            None,
        )
        .unwrap();

        let plan = build_editorial_playlist(&paths, project_id).unwrap();
        assert_eq!(plan.segments.len(), 2);
        assert!(
            plan.segments[1].global_start_sec >= plan.segments[0].global_end_sec - TIMELINE_EPS
        );
        assert_eq!(plan.duration_sec, plan.segments[1].global_end_sec);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn build_editorial_playlist_mixed_source_fps_keeps_source_frame_counts() {
        let base =
            std::env::temp_dir().join(format!("qnc_playlist_mixed_fps_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "playlist_mixed_fps";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO story_parts
                (part_id, kind, sort_index, title, text, clip_id, virtual_shot_id,
                 in_tc, out_tc, in_seconds, out_seconds, fps, source_fps_num, source_fps_den,
                 in_frame, out_frame, duration_frames,
                 duration_label, duration_color_key, created_at, updated_at)
             VALUES ('part_25', 'tonovi', 0, '', '', 'clip_a', '',
                     '', '', 0, 1, 25, 25, 1, 0, 25, 25, '1:00', 'under_3', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO story_parts
                (part_id, kind, sort_index, title, text, clip_id, virtual_shot_id,
                 in_tc, out_tc, in_seconds, out_seconds, fps, source_fps_num, source_fps_den,
                 in_frame, out_frame, duration_frames,
                 duration_label, duration_color_key, created_at, updated_at)
             VALUES ('part_50', 'tonovi', 1, '', '', 'clip_b', '',
                     '', '', 0, 1, 50, 50, 1, 0, 50, 50, '1:00', 'under_3', 't', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        let plan = build_editorial_playlist(&paths, project_id).unwrap();

        assert_eq!(plan.timeline_fps, 25.0);
        assert_eq!(plan.duration_frames, 75);
        assert_eq!(plan.duration_sec, 2.0);
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].duration_frames, 25);
        assert_eq!(plan.segments[1].global_start_frame, 25);
        assert_eq!(plan.segments[1].global_end_frame, 75);
        assert_eq!(plan.segments[1].duration_frames, 50);
        assert_eq!(plan.segments[1].source_fps, 50.0);
        assert_eq!(
            plan.segments[1].source_timebase,
            SourceTimebase {
                fps_num: 50,
                fps_den: 1
            }
        );
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
                 in_tc, out_tc, in_seconds, out_seconds, fps, source_fps_num, source_fps_den,
                 in_frame, out_frame, duration_frames,
                 duration_label, duration_color_key, created_at, updated_at)
             VALUES ('part_manual', 'tonovi', 0, '', '', 'clip_x', '',
                     '', '', 0, 3, 25, 25, 1, 0, 75, 75, '3:00', 'under_3', 't', 't')",
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
