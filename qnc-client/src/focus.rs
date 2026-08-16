//! Timeline focus: same ←/→ command; focus chooses the target.

use crate::api::TimelineModel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    Playhead,
    In,
    Out,
    Marker { id: String },
    Slot { id: String },
}

impl FocusTarget {
    pub fn label(&self) -> String {
        match self {
            FocusTarget::Playhead => "playhead".into(),
            FocusTarget::In => "IN".into(),
            FocusTarget::Out => "OUT".into(),
            FocusTarget::Marker { id } => format!("M:{id}"),
            FocusTarget::Slot { id } => format!("slot:{id}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelineFocus {
    pub target: FocusTarget,
}

impl Default for TimelineFocus {
    fn default() -> Self {
        Self {
            target: FocusTarget::Playhead,
        }
    }
}

impl TimelineFocus {
    pub fn clear(&mut self) {
        self.target = FocusTarget::Playhead;
    }

    pub fn is_playhead(&self) -> bool {
        matches!(self.target, FocusTarget::Playhead)
    }

    pub fn select_in(&mut self) {
        self.target = FocusTarget::In;
    }

    pub fn select_out(&mut self) {
        self.target = FocusTarget::Out;
    }

    pub fn select_marker(&mut self, id: impl Into<String>) {
        self.target = FocusTarget::Marker { id: id.into() };
    }

    pub fn select_slot(&mut self, id: impl Into<String>) {
        self.target = FocusTarget::Slot { id: id.into() };
    }

    pub fn focus_next(&mut self, chain: &[FocusTarget]) {
        if chain.is_empty() {
            self.clear();
            return;
        }
        let idx = chain.iter().position(|t| t == &self.target);
        let next = match idx {
            Some(i) => (i + 1) % chain.len(),
            None => 0,
        };
        self.target = chain[next].clone();
    }

    pub fn focus_prev(&mut self, chain: &[FocusTarget]) {
        if chain.is_empty() {
            self.clear();
            return;
        }
        let idx = chain.iter().position(|t| t == &self.target);
        let prev = match idx {
            Some(0) | None => chain.len() - 1,
            Some(i) => i - 1,
        };
        self.target = chain[prev].clone();
    }
}

pub fn normalized_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 1.0 {
        fps
    } else {
        25.0
    }
}

pub fn seconds_to_frame(seconds: f64, fps: f64) -> i64 {
    (seconds.max(0.0) * normalized_fps(fps)).round() as i64
}

pub fn frame_to_seconds(frame: i64, fps: f64) -> f64 {
    frame.max(0) as f64 / normalized_fps(fps)
}

/// 1 frame in seconds, for display labels only.
#[allow(dead_code)]
pub fn frame_sec(fps: f64) -> f64 {
    1.0 / normalized_fps(fps)
}

pub fn duration_frames_from_timeline(model: Option<&TimelineModel>) -> i64 {
    let fps = fps_from_timeline(model);
    model
        .map(|m| {
            if m.duration_frames > 0 {
                m.duration_frames
            } else {
                seconds_to_frame(m.duration_sec.max(0.0), fps)
            }
        })
        .unwrap_or(0)
        .max(0)
}

pub fn timeline_pin_frame(pin_frame: i64, pin_sec: f64, fps: f64) -> i64 {
    if pin_frame > 0 {
        pin_frame
    } else {
        seconds_to_frame(pin_sec.max(0.0), fps)
    }
}

pub fn timeline_span_frames(
    start_frame: i64,
    end_frame: i64,
    start_sec: f64,
    end_sec: f64,
    fps: f64,
) -> (i64, i64) {
    let start = if start_frame > 0 {
        start_frame
    } else {
        seconds_to_frame(start_sec.max(0.0), fps)
    };
    let end = if end_frame > start {
        end_frame
    } else {
        seconds_to_frame(end_sec.max(start_sec), fps).max(start + 1)
    };
    (start.max(0), end.max(start.max(0) + 1))
}

pub fn fps_from_timeline(model: Option<&TimelineModel>) -> f64 {
    model
        .map(|m| m.timeline_fps)
        .filter(|f| f.is_finite() && *f > 1.0)
        .unwrap_or(25.0)
}

/// Build Tab cycle: playhead → IN → OUT → markers → slots.
pub fn focus_chain(
    has_in: bool,
    has_out: bool,
    marker_ids: impl IntoIterator<Item = String>,
    slot_ids: impl IntoIterator<Item = String>,
) -> Vec<FocusTarget> {
    let mut chain = vec![FocusTarget::Playhead];
    if has_in {
        chain.push(FocusTarget::In);
    }
    if has_out {
        chain.push(FocusTarget::Out);
    }
    for id in marker_ids {
        if !id.is_empty() {
            chain.push(FocusTarget::Marker { id });
        }
    }
    for id in slot_ids {
        if !id.is_empty() {
            chain.push(FocusTarget::Slot { id });
        }
    }
    chain
}

/// Always include IN/OUT in chain for source editing (even before first mark).
#[allow(dead_code)]
pub fn source_focus_chain(marker_ids: impl IntoIterator<Item = String>) -> Vec<FocusTarget> {
    focus_chain(true, true, marker_ids, std::iter::empty())
}
