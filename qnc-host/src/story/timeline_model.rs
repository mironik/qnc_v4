//! Story builders for the shared QNC-timeline contract (`crate::timeline_model`).

use crate::project::db::{open_project, project_timeline_fps, ProjectPaths};
use crate::timeline_model::{
    build_source_timeline_model as shared_source_model, wrap_segment_io_pins, SegmentSchema,
    TimelineApplication, TimelineCover, TimelineMarkerSlot, TimelineModel, TimelinePin,
    TimelineSegment, TIMELINE_ROWS,
};

use super::covers::{list_covers, StoryCoverRow};
use super::db::{ensure_schema, list_parts, StoryPartRow};
use super::markers::{
    list_marker_slots_rows, list_markers_rows, part_span_seconds, timeline_duration_from_parts,
    TIMELINE_EPS,
};

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Wrap application from Story SQLite (parts + covers + M markers).
pub fn build_wrap_timeline_model(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<TimelineModel, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_schema(&conn).map_err(|e| e.to_string())?;
    let timeline_fps = project_timeline_fps(paths, pid);
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let covers = list_covers(&conn).map_err(|e| e.to_string())?;
    let markers = list_markers_rows(&conn).map_err(|e| e.to_string())?;
    let slots = list_marker_slots_rows(&conn).map_err(|e| e.to_string())?;

    let segments = build_segments(&parts, &covers);
    let duration_sec = if segments.is_empty() {
        0.0
    } else {
        timeline_duration_from_parts(&parts)
    };

    Ok(TimelineModel {
        project_id: pid.to_string(),
        application: TimelineApplication::Wrap,
        timeline_fps,
        duration_sec,
        rows: TIMELINE_ROWS.iter().map(|s| (*s).to_string()).collect(),
        io_pins: wrap_segment_io_pins(&segments),
        segments,
        markers: markers
            .into_iter()
            .map(|m| TimelinePin {
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
) -> TimelineModel {
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
    let mut global_start = 0.0;
    for part in parts {
        let span = part_span_seconds(part);
        let global_end = round3(global_start + span);
        let schema = SegmentSchema::from_kind(&part.kind);
        let part_covers = if schema.allows_covers() {
            map_covers_for_segment(covers, global_start, global_end)
        } else {
            vec![]
        };
        let clip_id = part.clip_id.trim().to_string();
        segments.push(TimelineSegment {
            part_id: part.part_id.clone(),
            kind: part.kind.clone(),
            schema,
            clip_id: clip_id.clone(),
            global_start_sec: round3(global_start),
            global_end_sec: global_end,
            duration_sec: round3(span),
            streamable: !clip_id.is_empty(),
            rows: schema.rows().iter().map(|s| (*s).to_string()).collect(),
            emulsion: schema.emulsion().iter().map(|s| (*s).to_string()).collect(),
            covers: part_covers,
        });
        global_start = global_end;
    }
    segments
}

fn map_covers_for_segment(
    covers: &[StoryCoverRow],
    part_start: f64,
    part_end: f64,
) -> Vec<TimelineCover> {
    let mut out = Vec::new();
    for cover in covers {
        let c_start = cover.timeline_start_sec;
        let c_end = cover.timeline_end_sec;
        if c_end <= part_start + TIMELINE_EPS || c_start >= part_end - TIMELINE_EPS {
            continue;
        }
        out.push(TimelineCover {
            cover_id: cover.cover_id.clone(),
            clip_id: cover.clip_id.clone(),
            timeline_start_sec: c_start,
            timeline_end_sec: c_end,
            streamable: !cover.virtual_shot_id.trim().is_empty(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_via_story_is_io_only() {
        let model = build_source_timeline_model("p", "c1", 5.0, 1.0, 4.0, 25.0);
        assert_eq!(model.application, TimelineApplication::Source);
        assert!(model.markers.is_empty());
        assert_eq!(model.io_pins.len(), 2);
    }
}
