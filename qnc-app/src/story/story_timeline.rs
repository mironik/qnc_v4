//! Native Story timeline helpers.
//!
//! Pure frame calculations over the host timeline model. No UI, playback I/O
//! or state mutation.

use crate::api::{TimelineModel, TimelineSegment};

pub(super) fn active_segment_frame(
    timeline: Option<&TimelineModel>,
    virtual_frame: i64,
) -> Option<&TimelineSegment> {
    let timeline = timeline?;
    timeline.segments.iter().find(|segment| {
        let start = segment.global_start_frame;
        let end = segment.global_end_frame.max(start + 1);
        virtual_frame >= start && virtual_frame < end
    })
}

pub(super) fn local_frame_in_part(
    timeline: Option<&TimelineModel>,
    part_id: &str,
    virtual_frame: i64,
) -> Option<i64> {
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return None;
    }

    let timeline = timeline?;
    let segment = timeline
        .segments
        .iter()
        .find(|segment| segment.part_id == part_id)?;
    let start = segment.global_start_frame;
    let end = segment.global_end_frame.max(start + 1);
    let span = (end - start).max(0);
    let local = (virtual_frame - start).max(0);
    Some(if span > 0 { local.min(span) } else { 0 })
}
