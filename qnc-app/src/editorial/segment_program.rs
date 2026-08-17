//! Neutral frame math for a segmented editorial program.
//!
//! This module does not know Story, Media Assist, UI widgets, or playback I/O.
//! It only maps between a global program axis and source-frame ranges stored in
//! the editorial playlist.

use crate::api::{EditorialPlaylist, EditorialPlaylistSegment};
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};
use crate::frame_time::normalize_fps;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SegmentProgramModel {
    timeline_fps: f64,
    duration_frames: i64,
    duration_sec: f64,
    segments: Vec<SegmentProgramSegment>,
    covers: Vec<SegmentProgramCover>,
    marker_slots: Vec<SegmentProgramMarkerSlot>,
    markers: Vec<SegmentProgramMarker>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SegmentProgramSegment {
    pub part_id: String,
    pub kind: String,
    pub clip_id: String,
    pub global_start_frame: i64,
    pub global_end_frame: i64,
    pub duration_frames: i64,
    pub global_start_sec: f64,
    pub global_end_sec: f64,
    pub duration_sec: f64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
    pub streamable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentProgramCover {
    pub cover_id: String,
    pub start_frame: i64,
    pub end_frame: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentProgramMarkerSlot {
    pub slot_id: String,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_cover: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentProgramMarker {
    pub marker_id: String,
    pub frame: i64,
}

impl SegmentProgramModel {
    pub(crate) fn from_playlist(
        playlist: Option<&EditorialPlaylist>,
        marker_slots: &[MarkerSlot],
        covers: &[StoryCover],
        markers: &[StoryMarker],
    ) -> Self {
        let timeline_fps = playlist
            .map(|playlist| playlist.timeline_fps)
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .map(normalize_fps)
            .unwrap_or(0.0);
        let segments: Vec<SegmentProgramSegment> = playlist
            .map(|playlist| {
                playlist
                    .segments
                    .iter()
                    .map(SegmentProgramSegment::from_playlist_segment)
                    .collect()
            })
            .unwrap_or_default();
        let duration_frames = playlist
            .map(|playlist| playlist.duration_frames.max(0))
            .unwrap_or(0)
            .max(
                segments
                    .last()
                    .map(|segment| segment.global_end_frame.max(0))
                    .unwrap_or(0),
            );
        let duration_sec = playlist
            .map(|playlist| playlist.duration_sec.max(0.0))
            .unwrap_or(0.0);

        Self {
            timeline_fps,
            duration_frames,
            duration_sec,
            segments,
            covers: covers
                .iter()
                .map(|cover| SegmentProgramCover {
                    cover_id: cover.cover_id.clone(),
                    start_frame: cover.timeline_start_frame.max(0),
                    end_frame: cover.timeline_end_frame.max(cover.timeline_start_frame),
                })
                .collect(),
            marker_slots: marker_slots
                .iter()
                .map(|slot| SegmentProgramMarkerSlot {
                    slot_id: slot.slot_id.clone(),
                    start_frame: slot.start_frame.max(0),
                    end_frame: slot.end_frame.max(slot.start_frame),
                    has_cover: slot.has_cover,
                })
                .collect(),
            markers: markers
                .iter()
                .map(|marker| SegmentProgramMarker {
                    marker_id: marker.marker_id.clone(),
                    frame: marker.timeline_frame.max(0),
                })
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub(crate) fn timeline_fps(&self) -> Option<f64> {
        (self.timeline_fps.is_finite() && self.timeline_fps > 0.0).then_some(self.timeline_fps)
    }

    pub(crate) fn duration_frames(&self) -> i64 {
        self.duration_frames.max(0)
    }

    #[cfg(test)]
    pub(crate) fn duration_sec(&self) -> f64 {
        self.duration_sec.max(0.0)
    }

    pub(crate) fn segments(&self) -> &[SegmentProgramSegment] {
        &self.segments
    }

    #[cfg(test)]
    pub(crate) fn covers(&self) -> &[SegmentProgramCover] {
        &self.covers
    }

    pub(crate) fn marker_slots(&self) -> &[SegmentProgramMarkerSlot] {
        &self.marker_slots
    }

    #[cfg(test)]
    pub(crate) fn markers(&self) -> &[SegmentProgramMarker] {
        &self.markers
    }

    pub(crate) fn active_part_at_program_frame(
        &self,
        program_frame: i64,
    ) -> Option<&SegmentProgramSegment> {
        let frame = program_frame.max(0);
        self.segments.iter().find(|segment| {
            frame >= segment.global_start_frame.max(0)
                && frame < segment.global_end_frame.max(segment.global_start_frame + 1)
        })
    }

    pub(crate) fn program_parts(&self) -> Vec<SegmentProgramPart<'_>> {
        self.segments
            .iter()
            .map(|segment| SegmentProgramPart {
                part_id: segment.part_id.as_str(),
                clip_id: segment.clip_id.as_str(),
                program_start_frame: segment.global_start_frame,
                program_end_frame: segment.global_end_frame,
                source_in_frame: segment.source_in_frame,
                source_out_frame: segment.source_out_frame,
            })
            .collect()
    }

    pub(crate) fn source_frame_for_program_frame(&self, program_frame: i64) -> Option<i64> {
        source_frame_for_program_frame(&self.program_parts(), program_frame)
    }

    pub(crate) fn program_frame_for_source_frame(
        &self,
        selected_part_id: &str,
        open_clip_id: &str,
        source_frame: i64,
    ) -> Option<SegmentFrameProjection> {
        program_frame_for_source_frame(
            &self.program_parts(),
            selected_part_id,
            open_clip_id,
            source_frame,
        )
    }

    /// While playing, hand off on the last in-range source frame — before the player
    /// would cross a wrap-row cut. Wrap rows are UI-only; the program axis is continuous.
    pub(crate) fn boundary_transition_imminent_after_source_frame(
        &self,
        current_part_id: &str,
        open_clip_id: &str,
        source_frame: i64,
        playing: bool,
    ) -> Option<SegmentBoundaryTransition> {
        boundary_transition_imminent_after_source_frame(
            &self.program_parts(),
            current_part_id,
            open_clip_id,
            source_frame,
            playing,
        )
    }

    pub(crate) fn adjacent_part(
        &self,
        selected_part_id: &str,
        program_frame: i64,
        direction: i32,
    ) -> Option<&SegmentProgramSegment> {
        if self.segments.is_empty() || direction == 0 {
            return None;
        }
        let current_index = self
            .segments
            .iter()
            .position(|segment| {
                !selected_part_id.trim().is_empty() && segment.part_id == selected_part_id
            })
            .or_else(|| {
                let frame = program_frame.max(0);
                self.segments.iter().position(|segment| {
                    frame >= segment.global_start_frame.max(0)
                        && frame < segment.global_end_frame.max(segment.global_start_frame + 1)
                })
            })
            .unwrap_or(0);
        let next = if direction < 0 {
            current_index.saturating_sub(1)
        } else {
            (current_index + 1).min(self.segments.len().saturating_sub(1))
        };
        (next != current_index).then(|| &self.segments[next])
    }

    pub(crate) fn effective_marker_slot_id<'a>(&'a self, selected_slot_id: &'a str) -> &'a str {
        if !selected_slot_id.trim().is_empty() {
            return selected_slot_id;
        }
        self.first_empty_marker_slot()
            .map(|slot| slot.slot_id.as_str())
            .unwrap_or("")
    }

    pub(crate) fn first_empty_marker_slot(&self) -> Option<&SegmentProgramMarkerSlot> {
        self.marker_slots
            .iter()
            .find(|slot| !slot.has_cover && !slot.slot_id.trim().is_empty())
    }

    pub(crate) fn marker_slot_by_id(&self, slot_id: &str) -> Option<&SegmentProgramMarkerSlot> {
        let slot_id = slot_id.trim();
        (!slot_id.is_empty()).then_some(())?;
        self.marker_slots
            .iter()
            .find(|slot| slot.slot_id == slot_id)
    }

    pub(crate) fn adjacent_marker_slot(
        &self,
        selected_slot_id: &str,
        program_frame: i64,
        direction: i32,
    ) -> Option<&SegmentProgramMarkerSlot> {
        if self.marker_slots.is_empty() || direction == 0 {
            return None;
        }
        let current_index = self
            .marker_slots
            .iter()
            .position(|slot| {
                !selected_slot_id.trim().is_empty() && slot.slot_id == selected_slot_id
            })
            .or_else(|| {
                let frame = program_frame.max(0);
                self.marker_slots.iter().position(|slot| {
                    frame >= slot.start_frame.max(0) && frame < slot.end_frame.max(slot.start_frame)
                })
            })
            .or_else(|| {
                self.first_empty_marker_slot().and_then(|empty| {
                    self.marker_slots
                        .iter()
                        .position(|slot| slot.slot_id == empty.slot_id)
                })
            })
            .unwrap_or(0);
        let next = if direction < 0 {
            current_index.saturating_sub(1)
        } else {
            (current_index + 1).min(self.marker_slots.len().saturating_sub(1))
        };
        (next != current_index).then(|| &self.marker_slots[next])
    }
}

impl SegmentProgramSegment {
    fn from_playlist_segment(segment: &EditorialPlaylistSegment) -> Self {
        let start = segment.global_start_frame.max(0);
        let end = segment
            .global_end_frame
            .max(start + segment.duration_frames.max(0))
            .max(start + 1);
        let duration_frames = segment.duration_frames.max(end - start).max(1);
        Self {
            part_id: segment.part_id.clone(),
            kind: segment.kind.clone(),
            clip_id: segment.clip_id.clone(),
            global_start_frame: start,
            global_end_frame: end,
            duration_frames,
            global_start_sec: segment.global_start_sec.max(0.0),
            global_end_sec: segment.global_end_sec.max(segment.global_start_sec),
            duration_sec: segment.duration_sec.max(0.0),
            source_in_frame: segment.source_in_frame.max(0),
            source_out_frame: segment.source_out_frame.max(segment.source_in_frame + 1),
            source_fps: segment.source_fps,
            streamable: segment.streamable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentProgramPart<'a> {
    pub part_id: &'a str,
    pub clip_id: &'a str,
    pub program_start_frame: i64,
    pub program_end_frame: i64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentFrameProjection {
    pub part_id: String,
    pub program_frame: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentBoundaryTransition {
    pub part_id: String,
    pub program_frame: i64,
    pub source_frame: i64,
}

pub(crate) fn source_frame_for_program_frame(
    parts: &[SegmentProgramPart<'_>],
    program_frame: i64,
) -> Option<i64> {
    let part = active_part_at_program_frame(parts, program_frame)?;
    if !valid_part(part) {
        return None;
    }
    let local_frame = program_frame
        .max(0)
        .saturating_sub(program_start_frame(part).max(0));
    let span = source_span(part);
    Some(part.source_in_frame.max(0) + local_frame.clamp(0, span.saturating_sub(1)))
}

pub(crate) fn program_frame_for_source_frame(
    parts: &[SegmentProgramPart<'_>],
    selected_part_id: &str,
    open_clip_id: &str,
    source_frame: i64,
) -> Option<SegmentFrameProjection> {
    let source_frame = source_frame.max(0);
    part_by_id(parts, selected_part_id)
        .and_then(|part| project_source_frame(part, open_clip_id, source_frame))
        .or_else(|| {
            parts
                .iter()
                .find_map(|part| project_source_frame(part, open_clip_id, source_frame))
        })
}

pub(crate) fn boundary_transition_after_source_frame(
    parts: &[SegmentProgramPart<'_>],
    current_part_id: &str,
    open_clip_id: &str,
    source_frame: i64,
) -> Option<SegmentBoundaryTransition> {
    boundary_transition_imminent_after_source_frame(
        parts,
        current_part_id,
        open_clip_id,
        source_frame,
        false,
    )
}

pub(crate) fn boundary_transition_imminent_after_source_frame(
    parts: &[SegmentProgramPart<'_>],
    current_part_id: &str,
    open_clip_id: &str,
    source_frame: i64,
    playing: bool,
) -> Option<SegmentBoundaryTransition> {
    let current = part_by_id(parts, current_part_id)?;
    if !valid_part(current) {
        return None;
    }
    if !open_clip_id.trim().is_empty() && current.clip_id.trim() != open_clip_id.trim() {
        return None;
    }
    let out = current.source_out_frame.max(current.source_in_frame + 1);
    let frame = source_frame.max(0);
    let crossed_out = frame >= out;
    let imminent_while_playing = playing && frame + 1 >= out;
    if !crossed_out && !imminent_while_playing {
        return None;
    }
    next_valid_program_part(parts, current.part_id)
}

fn next_valid_program_part(
    parts: &[SegmentProgramPart<'_>],
    current_part_id: &str,
) -> Option<SegmentBoundaryTransition> {
    let current_index = parts
        .iter()
        .position(|part| part.part_id == current_part_id)?;
    parts
        .iter()
        .skip(current_index + 1)
        .find(|part| valid_part(part))
        .map(|part| SegmentBoundaryTransition {
            part_id: part.part_id.to_string(),
            program_frame: program_start_frame(part),
            source_frame: part.source_in_frame.max(0),
        })
}

fn project_source_frame(
    part: &SegmentProgramPart<'_>,
    open_clip_id: &str,
    source_frame: i64,
) -> Option<SegmentFrameProjection> {
    if !open_clip_id.trim().is_empty() && part.clip_id.trim() != open_clip_id.trim() {
        return None;
    }
    if !valid_part(part) {
        return None;
    }
    let inn = part.source_in_frame.max(0);
    let out = part.source_out_frame.max(inn + 1);
    if source_frame < inn || source_frame >= out {
        return None;
    }
    let local_source_frame = source_frame.saturating_sub(inn);
    let span = program_span(part);
    Some(SegmentFrameProjection {
        part_id: part.part_id.to_string(),
        program_frame: program_start_frame(part)
            + local_source_frame.clamp(0, span.saturating_sub(1)),
    })
}

fn active_part_at_program_frame<'a>(
    parts: &'a [SegmentProgramPart<'a>],
    program_frame: i64,
) -> Option<&'a SegmentProgramPart<'a>> {
    parts.iter().find(|part| {
        let start = program_start_frame(part);
        let end = program_end_frame(part);
        program_frame >= start && program_frame < end
    })
}

fn part_by_id<'a>(
    parts: &'a [SegmentProgramPart<'a>],
    part_id: &str,
) -> Option<&'a SegmentProgramPart<'a>> {
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return None;
    }
    parts.iter().find(|part| part.part_id == part_id)
}

fn valid_part(part: &SegmentProgramPart<'_>) -> bool {
    !part.part_id.trim().is_empty()
        && !part.clip_id.trim().is_empty()
        && part.program_end_frame > part.program_start_frame
        && part.source_out_frame > part.source_in_frame
}

fn source_span(part: &SegmentProgramPart<'_>) -> i64 {
    (part.source_out_frame - part.source_in_frame).max(1)
}

fn program_start_frame(part: &SegmentProgramPart<'_>) -> i64 {
    part.program_start_frame.max(0)
}

fn program_end_frame(part: &SegmentProgramPart<'_>) -> i64 {
    let start = program_start_frame(part);
    part.program_end_frame.max(start + 1)
}

fn program_span(part: &SegmentProgramPart<'_>) -> i64 {
    (program_end_frame(part) - program_start_frame(part)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};

    fn parts() -> Vec<SegmentProgramPart<'static>> {
        vec![
            SegmentProgramPart {
                part_id: "part_a",
                clip_id: "clip_a",
                program_start_frame: 0,
                program_end_frame: 50,
                source_in_frame: 100,
                source_out_frame: 150,
            },
            SegmentProgramPart {
                part_id: "part_b",
                clip_id: "clip_a",
                program_start_frame: 50,
                program_end_frame: 100,
                source_in_frame: 200,
                source_out_frame: 250,
            },
        ]
    }

    #[test]
    fn source_projection_treats_part_out_as_exclusive() {
        let parts = parts();

        assert_eq!(
            program_frame_for_source_frame(&parts, "part_a", "clip_a", 149),
            Some(SegmentFrameProjection {
                part_id: "part_a".into(),
                program_frame: 49,
            })
        );
        assert_eq!(
            program_frame_for_source_frame(&parts, "part_a", "clip_a", 150),
            None
        );
    }

    #[test]
    fn boundary_transition_uses_next_program_part_after_out_or_overrun() {
        let parts = parts();

        assert_eq!(
            boundary_transition_after_source_frame(&parts, "part_a", "clip_a", 150),
            Some(SegmentBoundaryTransition {
                part_id: "part_b".into(),
                program_frame: 50,
                source_frame: 200,
            })
        );
        assert_eq!(
            boundary_transition_after_source_frame(&parts, "part_a", "clip_a", 151),
            Some(SegmentBoundaryTransition {
                part_id: "part_b".into(),
                program_frame: 50,
                source_frame: 200,
            })
        );
    }

    #[test]
    fn boundary_transition_imminent_hands_off_on_last_in_range_frame_while_playing() {
        let parts = parts();

        assert_eq!(
            boundary_transition_imminent_after_source_frame(&parts, "part_a", "clip_a", 149, true),
            Some(SegmentBoundaryTransition {
                part_id: "part_b".into(),
                program_frame: 50,
                source_frame: 200,
            })
        );
        assert!(boundary_transition_imminent_after_source_frame(
            &parts, "part_a", "clip_a", 149, false
        )
        .is_none());
    }

    #[test]
    fn program_seek_maps_to_active_part_source_range() {
        let parts = parts();

        assert_eq!(source_frame_for_program_frame(&parts, 50), Some(200));
        assert_eq!(source_frame_for_program_frame(&parts, 99), Some(249));
    }

    #[test]
    fn program_model_uses_playlist_as_single_segment_axis() {
        let playlist = EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 90,
            duration_sec: 1.8,
            segments: vec![
                EditorialPlaylistSegment {
                    part_id: "part_a".into(),
                    kind: "tonovi".into(),
                    clip_id: "clip_a".into(),
                    global_start_frame: 0,
                    global_end_frame: 50,
                    duration_frames: 50,
                    source_in_frame: 100,
                    source_out_frame: 150,
                    source_fps: 50.0,
                    streamable: true,
                    ..EditorialPlaylistSegment::default()
                },
                EditorialPlaylistSegment {
                    part_id: "part_b".into(),
                    kind: "offovi".into(),
                    clip_id: "clip_b".into(),
                    global_start_frame: 50,
                    global_end_frame: 90,
                    duration_frames: 40,
                    source_in_frame: 20,
                    source_out_frame: 60,
                    source_fps: 50.0,
                    streamable: true,
                    ..EditorialPlaylistSegment::default()
                },
            ],
        };
        let slots = [MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 0,
            end_frame: 50,
            has_cover: false,
            ..MarkerSlot::default()
        }];
        let covers = [StoryCover {
            cover_id: "cover_a".into(),
            timeline_start_frame: 10,
            timeline_end_frame: 30,
            ..StoryCover::default()
        }];
        let markers = [StoryMarker {
            marker_id: "m0".into(),
            timeline_frame: 0,
            ..StoryMarker::default()
        }];

        let program =
            SegmentProgramModel::from_playlist(Some(&playlist), &slots, &covers, &markers);

        assert_eq!(program.timeline_fps(), Some(50.0));
        assert_eq!(program.duration_frames(), 90);
        assert_eq!(program.duration_sec(), 1.8);
        assert_eq!(
            program
                .active_part_at_program_frame(50)
                .map(|p| p.part_id.as_str()),
            Some("part_b")
        );
        assert_eq!(program.source_frame_for_program_frame(55), Some(25));
        assert_eq!(
            program
                .program_frame_for_source_frame("part_a", "clip_a", 120)
                .map(|projection| projection.program_frame),
            Some(20)
        );
        assert_eq!(program.effective_marker_slot_id(""), "slot_a");
        assert_eq!(program.covers()[0].cover_id, "cover_a");
        assert_eq!(program.markers()[0].marker_id, "m0");
    }

    #[test]
    fn program_model_steps_parts_and_marker_slots_without_form_state() {
        let playlist = EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 25.0,
            duration_frames: 100,
            segments: vec![
                EditorialPlaylistSegment {
                    part_id: "part_a".into(),
                    global_start_frame: 0,
                    global_end_frame: 40,
                    duration_frames: 40,
                    clip_id: "clip_a".into(),
                    source_in_frame: 0,
                    source_out_frame: 40,
                    ..EditorialPlaylistSegment::default()
                },
                EditorialPlaylistSegment {
                    part_id: "part_b".into(),
                    global_start_frame: 40,
                    global_end_frame: 100,
                    duration_frames: 60,
                    clip_id: "clip_a".into(),
                    source_in_frame: 100,
                    source_out_frame: 160,
                    ..EditorialPlaylistSegment::default()
                },
            ],
            ..EditorialPlaylist::default()
        };
        let slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 40,
                has_cover: true,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 40,
                end_frame: 100,
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];
        let program = SegmentProgramModel::from_playlist(Some(&playlist), &slots, &[], &[]);

        assert_eq!(
            program
                .adjacent_part("part_a", 0, 1)
                .map(|part| part.part_id.as_str()),
            Some("part_b")
        );
        assert_eq!(
            program
                .adjacent_marker_slot("", 10, 1)
                .map(|slot| slot.slot_id.as_str()),
            Some("slot_b")
        );
        assert_eq!(
            program
                .first_empty_marker_slot()
                .map(|slot| slot.slot_id.as_str()),
            Some("slot_b")
        );
    }
}
