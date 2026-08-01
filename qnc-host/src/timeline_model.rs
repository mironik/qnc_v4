//! QNC-timeline contract — universal Kodak timeline; applications via schema + snapshot.
//!
//! Product path: Story API + `qnc-client` paint. Not web kodak. Not QStory web.
//!
//! - Virtual source (`application: source`): IN/OUT only; empty M/covers.
//! - Wrap (`application: wrap`): IO + M markers + covers (+ future editorial).

use serde::{Deserialize, Serialize};

/// UI / native row order (Kodak perforacije → classic NLE rows).
pub const TIMELINE_ROWS: [&str; 3] = ["audio-1", "video", "audio-2"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineApplication {
    Wrap,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSchema {
    Off,
    Ton,
    Source,
}

impl SegmentSchema {
    pub fn from_kind(kind: &str) -> Self {
        if kind.trim().eq_ignore_ascii_case("offovi") {
            Self::Off
        } else if kind.trim().eq_ignore_ascii_case("source")
            || kind.trim().eq_ignore_ascii_case("clip")
        {
            Self::Source
        } else {
            Self::Ton
        }
    }

    pub fn rows(self) -> &'static [&'static str] {
        &TIMELINE_ROWS
    }

    /// Source has no yellow emulsion (covers are not editorial truth).
    pub fn emulsion(self) -> &'static [&'static str] {
        match self {
            Self::Source => &["cyan", "magenta"],
            _ => &["cyan", "magenta", "yellow"],
        }
    }

    pub fn allows_covers(self) -> bool {
        !matches!(self, Self::Source)
    }

    #[allow(dead_code)]
    pub fn allows_m_markers(self) -> bool {
        !matches!(self, Self::Source)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineCover {
    pub cover_id: String,
    pub clip_id: String,
    pub timeline_start_sec: f64,
    pub timeline_end_sec: f64,
    pub streamable: bool,
}

/// Pin on the timeline ruler: IN/OUT (source + wrap) or M marker (wrap only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelinePin {
    pub id: String,
    /// `in` | `out` | `marker`
    pub kind: String,
    pub timeline_sec: f64,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineMarkerSlot {
    pub slot_id: String,
    pub start_sec: f64,
    pub end_sec: f64,
    #[serde(default)]
    pub start_marker_id: String,
    #[serde(default)]
    pub end_marker_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineSegment {
    pub part_id: String,
    pub kind: String,
    pub schema: SegmentSchema,
    pub clip_id: String,
    pub global_start_sec: f64,
    pub global_end_sec: f64,
    pub duration_sec: f64,
    pub streamable: bool,
    pub rows: Vec<String>,
    pub emulsion: Vec<String>,
    /// Empty when `schema == Source`.
    pub covers: Vec<TimelineCover>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineModel {
    pub project_id: String,
    pub application: TimelineApplication,
    pub timeline_fps: f64,
    pub duration_sec: f64,
    /// Always A1 → V → A2.
    pub rows: Vec<String>,
    pub segments: Vec<TimelineSegment>,
    /// I/O pins (filled for source and wrap).
    #[serde(default)]
    pub io_pins: Vec<TimelinePin>,
    /// M markers — empty for source application.
    #[serde(default)]
    pub markers: Vec<TimelinePin>,
    /// M–M slots — empty for source application.
    #[serde(default)]
    pub marker_slots: Vec<TimelineMarkerSlot>,
}

/// Virtual-source application: one Source segment, IO pins only, empty M/covers.
pub fn build_source_timeline_model(
    project_id: &str,
    clip_id: &str,
    duration_sec: f64,
    in_sec: f64,
    out_sec: f64,
    timeline_fps: f64,
) -> TimelineModel {
    let duration = duration_sec.max(0.0);
    let in_s = in_sec.clamp(0.0, duration);
    let mut out_s = out_sec.clamp(0.0, duration);
    if out_s < in_s {
        out_s = in_s;
    }
    let schema = SegmentSchema::Source;
    let clip = clip_id.trim().to_string();
    TimelineModel {
        project_id: project_id.trim().to_string(),
        application: TimelineApplication::Source,
        timeline_fps: if timeline_fps > 0.0 {
            timeline_fps
        } else {
            25.0
        },
        duration_sec: duration,
        rows: TIMELINE_ROWS.iter().map(|s| (*s).to_string()).collect(),
        segments: vec![TimelineSegment {
            part_id: format!("source:{clip}"),
            kind: "source".into(),
            schema,
            clip_id: clip.clone(),
            global_start_sec: 0.0,
            global_end_sec: duration,
            duration_sec: duration,
            streamable: !clip.is_empty(),
            rows: schema.rows().iter().map(|s| (*s).to_string()).collect(),
            emulsion: schema.emulsion().iter().map(|s| (*s).to_string()).collect(),
            covers: vec![],
        }],
        io_pins: vec![
            TimelinePin {
                id: format!("{clip}:in"),
                kind: "in".into(),
                timeline_sec: in_s,
                label: "I".into(),
            },
            TimelinePin {
                id: format!("{clip}:out"),
                kind: "out".into(),
                timeline_sec: out_s,
                label: "O".into(),
            },
        ],
        markers: vec![],
        marker_slots: vec![],
    }
}

pub fn wrap_segment_io_pins(segments: &[TimelineSegment]) -> Vec<TimelinePin> {
    let mut pins = Vec::new();
    for seg in segments {
        pins.push(TimelinePin {
            id: format!("{}:in", seg.part_id),
            kind: "in".into(),
            timeline_sec: seg.global_start_sec,
            label: "I".into(),
        });
        pins.push(TimelinePin {
            id: format!("{}:out", seg.part_id),
            kind: "out".into(),
            timeline_sec: seg.global_end_sec,
            label: "O".into(),
        });
    }
    pins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kodak_rows_are_a1_v_a2() {
        assert_eq!(TIMELINE_ROWS, ["audio-1", "video", "audio-2"]);
    }

    #[test]
    fn source_schema_drops_yellow() {
        let schema = SegmentSchema::Source;
        assert!(!schema.emulsion().contains(&"yellow"));
        assert!(!schema.allows_covers());
        assert!(!schema.allows_m_markers());
    }

    #[test]
    fn source_model_has_io_only() {
        let model = build_source_timeline_model("p1", "clip_a", 10.0, 2.0, 8.0, 25.0);
        assert_eq!(model.application, TimelineApplication::Source);
        assert!(model.segments[0].covers.is_empty());
        assert!(model.markers.is_empty());
        assert!(model.marker_slots.is_empty());
        assert_eq!(model.io_pins.len(), 2);
        assert_eq!(model.io_pins[0].timeline_sec, 2.0);
        assert_eq!(model.io_pins[1].timeline_sec, 8.0);
    }
}
