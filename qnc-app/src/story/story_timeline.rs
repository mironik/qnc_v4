//! Native Story timeline helpers.
//!
//! Pure calculations over the host timeline model: duration, active segment
//! lookup and local part time. No UI, playback I/O or state mutation.

use crate::api::{TimelineModel, TimelineSegment};

use super::ViewMode;

pub(super) fn duration(
    view_mode: ViewMode,
    timeline: Option<&TimelineModel>,
    source_in: f64,
    source_out: f64,
) -> f64 {
    match view_mode {
        ViewMode::Wrap => wrap_duration(timeline),
        ViewMode::Source => (source_out - source_in).max(0.0),
    }
}

pub(super) fn active_segment(
    timeline: Option<&TimelineModel>,
    virtual_sec: f64,
) -> Option<&TimelineSegment> {
    timeline?.segments.iter().find(|segment| {
        virtual_sec >= segment.global_start_sec
            && virtual_sec < segment.global_end_sec.max(segment.global_start_sec + 0.01)
    })
}

pub(super) fn local_sec_in_part(
    timeline: Option<&TimelineModel>,
    part_id: &str,
    virtual_sec: f64,
) -> Option<f64> {
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return None;
    }

    let segment = timeline?
        .segments
        .iter()
        .find(|segment| segment.part_id == part_id)?;
    let local = (virtual_sec - segment.global_start_sec).max(0.0);
    let span = (segment.global_end_sec - segment.global_start_sec).max(0.0);
    Some(if span > 0.0 { local.min(span) } else { 0.0 })
}

fn wrap_duration(timeline: Option<&TimelineModel>) -> f64 {
    timeline
        .map(|timeline| timeline.duration_sec.max(0.0))
        .unwrap_or(0.0)
}
