//! Story builders for the shared QNC-timeline contract (`crate::timeline_model`).

use rusqlite::Connection;

use crate::frame_time::is_valid_fps;
use crate::project::db::{open_project, ProjectPaths};
use crate::project::ProjectDbBroker;
use crate::timeline_model::{
    build_source_timeline_model as shared_source_model, wrap_segment_io_pins, SegmentSchema,
    TimelineApplication, TimelineCover, TimelineMarkerSlot, TimelineModel, TimelinePin,
    TimelineSegment, TIMELINE_ROWS,
};

use super::covers::{list_covers, StoryCoverRow};
use super::db::{ensure_schema, list_parts, StoryPartRow};
use super::markers::{
    list_marker_slots_rows, list_markers_rows, part_span_frames, part_span_seconds,
    timeline_duration_frames_from_parts,
};

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Wrap application from Story SQLite (parts + covers + M markers).
pub fn build_wrap_timeline_model_with_broker(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
) -> Result<TimelineModel, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    project_db.with_project_write(pid, |conn| {
        build_wrap_timeline_model_from_conn(paths, pid, conn)
    })
}

#[allow(dead_code)]
pub fn build_wrap_timeline_model(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<TimelineModel, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    build_wrap_timeline_model_from_conn(paths, pid, &conn)
}

fn build_wrap_timeline_model_from_conn(
    _paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<TimelineModel, String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    let parts = list_parts(conn).map_err(|e| e.to_string())?;
    let timeline_fps = story_program_source_fps(&parts).unwrap_or(0.0);
    let covers = list_covers(conn).map_err(|e| e.to_string())?;
    let markers = list_markers_rows(conn).map_err(|e| e.to_string())?;
    let slots = list_marker_slots_rows(conn).map_err(|e| e.to_string())?;

    let segments = build_segments(&parts, &covers);
    let duration_frames = if segments.is_empty() {
        0
    } else {
        timeline_duration_frames_from_parts(&parts)
    };
    let duration_sec = round3(parts.iter().map(part_span_seconds).sum());

    Ok(TimelineModel {
        project_id: project_id.to_string(),
        application: TimelineApplication::Wrap,
        timeline_fps,
        duration_frames,
        duration_sec,
        rows: TIMELINE_ROWS.iter().map(|s| (*s).to_string()).collect(),
        io_pins: wrap_segment_io_pins(&segments),
        segments,
        markers: markers
            .into_iter()
            .map(|m| TimelinePin {
                timeline_frame: m.timeline_frame,
                id: m.marker_id,
                kind: "marker".into(),
                timeline_sec: m.timeline_sec,
                label: if m.label.is_empty() {
                    "M".into()
                } else {
                    m.label
                },
            })
            .collect(),
        marker_slots: slots
            .into_iter()
            .map(|s| TimelineMarkerSlot {
                slot_id: s.slot_id,
                start_frame: s.start_frame,
                end_frame: s.end_frame,
                start_sec: s.start_sec,
                end_sec: s.end_sec,
                start_marker_id: s.start_marker_id,
                end_marker_id: s.end_marker_id,
            })
            .collect(),
    })
}

pub fn build_source_timeline_model(
    project_id: &str,
    clip_id: &str,
    duration_sec: f64,
    in_sec: f64,
    out_sec: f64,
    timeline_fps: f64,
) -> Result<TimelineModel, String> {
    shared_source_model(
        project_id,
        clip_id,
        duration_sec,
        in_sec,
        out_sec,
        timeline_fps,
    )
}

fn build_segments(parts: &[StoryPartRow], covers: &[StoryCoverRow]) -> Vec<TimelineSegment> {
    let mut segments = Vec::new();
    let mut global_start_frame = 0;
    let mut global_start_sec = 0.0;
    for part in parts {
        let span_frames = part_span_frames(part);
        let global_end_frame = global_start_frame + span_frames;
        let span = part_span_seconds(part);
        let global_end = global_start_sec + span;
        let schema = SegmentSchema::from_kind(&part.kind);
        let part_covers = if schema.allows_covers() {
            map_covers_for_segment(covers, global_start_frame, global_end_frame)
        } else {
            vec![]
        };
        let clip_id = part.clip_id.trim().to_string();
        segments.push(TimelineSegment {
            part_id: part.part_id.clone(),
            kind: part.kind.clone(),
            schema,
            clip_id: clip_id.clone(),
            global_start_frame,
            global_end_frame,
            duration_frames: span_frames,
            global_start_sec: round3(global_start_sec),
            global_end_sec: round3(global_end),
            duration_sec: round3(span),
            streamable: !clip_id.is_empty(),
            rows: schema.rows().iter().map(|s| (*s).to_string()).collect(),
            emulsion: schema.emulsion().iter().map(|s| (*s).to_string()).collect(),
            covers: part_covers,
        });
        global_start_frame = global_end_frame;
        global_start_sec = global_end;
    }
    segments
}

fn map_covers_for_segment(
    covers: &[StoryCoverRow],
    part_start_frame: i64,
    part_end_frame: i64,
) -> Vec<TimelineCover> {
    let mut out = Vec::new();
    for cover in covers {
        let c_start_frame = cover.timeline_start_frame.max(0);
        let c_end_frame = cover.timeline_end_frame.max(c_start_frame);
        if c_end_frame <= part_start_frame || c_start_frame >= part_end_frame {
            continue;
        }
        out.push(TimelineCover {
            cover_id: cover.cover_id.clone(),
            clip_id: cover.clip_id.clone(),
            timeline_start_frame: c_start_frame,
            timeline_end_frame: c_end_frame,
            timeline_start_sec: cover.timeline_start_sec,
            timeline_end_sec: cover.timeline_end_sec,
            streamable: !cover.virtual_shot_id.trim().is_empty(),
        });
    }
    out
}

fn story_program_source_fps(parts: &[StoryPartRow]) -> Option<f64> {
    parts
        .iter()
        .map(|part| part.fps)
        .find(|fps| is_valid_fps(*fps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_via_story_is_io_only() {
        let model = build_source_timeline_model("p", "c1", 5.0, 1.0, 4.0, 50.0).unwrap();
        assert_eq!(model.application, TimelineApplication::Source);
        assert_eq!(model.timeline_fps, 50.0);
        assert!(model.markers.is_empty());
        assert_eq!(model.io_pins.len(), 2);
    }
}
