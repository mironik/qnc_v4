//! Neutral frame math for a segmented editorial program.
//!
//! This module does not know Story, Media Assist, UI widgets, or playback I/O.
//! It only maps between a global program axis and source-frame ranges stored in
//! the editorial playlist.

use crate::api::{EditorialPlaylist, EditorialPlaylistCover, EditorialPlaylistSegment};
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
    pub streamable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SegmentProgramCover {
    pub cover_id: String,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub start_frame: i64,
    pub end_frame: i64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
    pub streamable: bool,
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
            covers: segment_program_covers(playlist, covers),
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

    #[cfg(test)]
    pub(crate) fn program_parts(&self) -> Vec<SegmentProgramPart<'_>> {
        self.segments
            .iter()
            .map(|segment| SegmentProgramPart {
                part_id: segment.part_id.as_str(),
                clip_id: segment.clip_id.as_str(),
                program_start_frame: segment.global_start_frame,
                program_end_frame: segment.global_end_frame,
                timeline_fps: self.timeline_fps,
                source_in_frame: segment.source_in_frame,
                source_out_frame: segment.source_out_frame,
                source_fps: segment.source_fps,
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn source_frame_for_program_frame(&self, program_frame: i64) -> Option<i64> {
        source_frame_for_program_frame(&self.program_parts(), program_frame)
    }

    #[cfg(test)]
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

fn segment_program_covers(
    playlist: Option<&EditorialPlaylist>,
    story_covers: &[StoryCover],
) -> Vec<SegmentProgramCover> {
    let playlist_covers: Vec<_> = playlist
        .into_iter()
        .flat_map(|playlist| playlist.segments.iter())
        .flat_map(|segment| segment.covers.iter())
        .map(SegmentProgramCover::from_playlist_cover)
        .collect();
    if !playlist_covers.is_empty() {
        return playlist_covers;
    }
    story_covers
        .iter()
        .map(SegmentProgramCover::from_story_cover)
        .collect()
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
            virtual_shot_id: segment.virtual_shot_id.clone(),
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

impl SegmentProgramCover {
    fn from_playlist_cover(cover: &EditorialPlaylistCover) -> Self {
        let start = cover.timeline_start_frame.max(0);
        let end = cover.timeline_end_frame.max(start + 1);
        let source_in = cover.source_in_frame.max(0);
        Self {
            cover_id: cover.cover_id.clone(),
            clip_id: cover.clip_id.clone(),
            virtual_shot_id: cover.virtual_shot_id.clone(),
            start_frame: start,
            end_frame: end,
            source_in_frame: source_in,
            source_out_frame: cover.source_out_frame.max(source_in + 1),
            source_fps: cover.source_fps,
            streamable: cover.streamable,
        }
    }

    fn from_story_cover(cover: &StoryCover) -> Self {
        let start = cover.timeline_start_frame.max(0);
        let end = cover.timeline_end_frame.max(start + 1);
        let source_in = cover.source_in_frame.max(0);
        let source_out = if cover.source_out_frame > cover.source_in_frame {
            cover.source_out_frame.max(source_in + 1)
        } else {
            source_in + (end - start).max(1)
        };
        Self {
            cover_id: cover.cover_id.clone(),
            clip_id: cover.clip_id.clone(),
            virtual_shot_id: cover.virtual_shot_id.clone(),
            start_frame: start,
            end_frame: end,
            source_in_frame: source_in,
            source_out_frame: source_out,
            source_fps: cover.source_fps,
            streamable: !cover.clip_id.trim().is_empty(),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SegmentProgramPart<'a> {
    pub part_id: &'a str,
    pub clip_id: &'a str,
    pub program_start_frame: i64,
    pub program_end_frame: i64,
    pub timeline_fps: f64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentFrameProjection {
    pub part_id: String,
    pub program_frame: i64,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentBoundaryTransition {
    pub part_id: String,
    pub program_frame: i64,
    pub source_frame: i64,
}

/// Active playback layer at a program frame — mirrors host `resolve_active_layer_frozen`.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentPlaybackLayerKind {
    PartVideo,
    CoverVideo,
    OffAudio,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SegmentPlaybackTarget {
    pub kind: SegmentPlaybackLayerKind,
    pub part_id: String,
    pub cover_id: String,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub program_frame: i64,
    pub local_program_frame: i64,
    pub source_frame: i64,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_fps: f64,
}

#[cfg(test)]
impl SegmentPlaybackTarget {
    #[allow(dead_code)]
    pub(crate) fn sync_key(&self) -> String {
        format!("{:?}|{}", self.kind, self.clip_id)
    }
}

/// One row of the program EDL at a record frame — no transport semantics.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramEdlFrame {
    pub kind: SegmentPlaybackLayerKind,
    pub program_frame: i64,
    pub part_id: String,
    pub cover_id: String,
    pub clip_id: String,
    pub source_frame: i64,
}

#[cfg(test)]
impl ProgramEdlFrame {
    #[allow(dead_code)]
    pub(crate) fn from_target(program_frame: i64, target: &SegmentPlaybackTarget) -> Self {
        Self {
            kind: target.kind.clone(),
            program_frame: program_frame.max(0),
            part_id: target.part_id.clone(),
            cover_id: target.cover_id.clone(),
            clip_id: target.clip_id.clone(),
            source_frame: target.source_frame.max(0),
        }
    }

    /// True at an edit (record) cut — clip change or non-contiguous source take.
    pub(crate) fn is_edl_cut_from(&self, previous: &Self) -> bool {
        if self.kind != previous.kind {
            return true;
        }
        if self.part_id != previous.part_id {
            return true;
        }
        if self.cover_id != previous.cover_id {
            return true;
        }
        if self.clip_id != previous.clip_id {
            return true;
        }
        false
    }
}

impl SegmentProgramModel {
    #[cfg(test)]
    pub(crate) fn active_cover_at_program_frame(
        &self,
        program_frame: i64,
    ) -> Option<&SegmentProgramCover> {
        let frame = program_frame.max(0);
        self.covers.iter().find(|cover| {
            frame >= cover.start_frame.max(0) && frame < cover.end_frame.max(cover.start_frame + 1)
        })
    }

    /// Resolve which clip/range the player should follow at `program_frame`.
    #[cfg(test)]
    pub(crate) fn resolve_playback_target(
        &self,
        program_frame: i64,
        _story_covers: &[StoryCover],
    ) -> Option<SegmentPlaybackTarget> {
        let segment = self.active_part_at_program_frame(program_frame)?;
        let local_program_frame = program_frame.saturating_sub(segment.global_start_frame.max(0));

        if let Some(cover) = self.active_cover_at_program_frame(program_frame) {
            let clip_id = cover.clip_id.trim();
            if !clip_id.is_empty() && cover.streamable {
                let cover_local = program_frame.saturating_sub(cover.start_frame.max(0));
                let source_in = cover.source_in_frame.max(0);
                let source_out = cover.source_out_frame.max(source_in + 1);
                return Some(SegmentPlaybackTarget {
                    kind: SegmentPlaybackLayerKind::CoverVideo,
                    part_id: segment.part_id.clone(),
                    cover_id: cover.cover_id.clone(),
                    clip_id: clip_id.to_string(),
                    virtual_shot_id: cover.virtual_shot_id.clone(),
                    program_frame: program_frame.max(0),
                    local_program_frame,
                    source_frame: source_frame_for_segment_local_frame(
                        self.timeline_fps,
                        cover.source_fps,
                        source_in,
                        source_out,
                        cover_local,
                    ),
                    source_in_frame: source_in,
                    source_out_frame: source_out,
                    source_fps: cover.source_fps,
                });
            }
        }

        if segment.kind.trim().eq_ignore_ascii_case("offovi") {
            let clip_id = segment.clip_id.trim();
            return Some(SegmentPlaybackTarget {
                kind: SegmentPlaybackLayerKind::OffAudio,
                part_id: segment.part_id.clone(),
                cover_id: String::new(),
                clip_id: clip_id.to_string(),
                virtual_shot_id: segment.virtual_shot_id.clone(),
                program_frame: program_frame.max(0),
                local_program_frame,
                source_frame: source_frame_for_segment_local_frame(
                    self.timeline_fps,
                    segment.source_fps,
                    segment.source_in_frame,
                    segment.source_out_frame,
                    local_program_frame,
                ),
                source_in_frame: segment.source_in_frame.max(0),
                source_out_frame: segment.source_out_frame.max(segment.source_in_frame + 1),
                source_fps: segment.source_fps,
            });
        }

        let source_frame =
            source_frame_for_program_frame(&self.program_parts(), program_frame.max(0))?;
        Some(SegmentPlaybackTarget {
            kind: SegmentPlaybackLayerKind::PartVideo,
            part_id: segment.part_id.clone(),
            cover_id: String::new(),
            clip_id: segment.clip_id.clone(),
            virtual_shot_id: segment.virtual_shot_id.clone(),
            program_frame: program_frame.max(0),
            local_program_frame,
            source_frame,
            source_in_frame: segment.source_in_frame.max(0),
            source_out_frame: segment.source_out_frame.max(segment.source_in_frame + 1),
            source_fps: segment.source_fps,
        })
    }
}

#[cfg(test)]
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
    Some(source_frame_for_segment_local_frame(
        part.timeline_fps,
        part.source_fps,
        part.source_in_frame,
        part.source_out_frame,
        local_frame,
    ))
}

#[cfg(test)]
fn source_frame_for_segment_local_frame(
    timeline_fps: f64,
    source_fps: f64,
    source_in_frame: i64,
    source_out_frame: i64,
    local_program_frame: i64,
) -> i64 {
    let source_in = source_in_frame.max(0);
    let source_out = source_out_frame.max(source_in + 1);
    let source_span = (source_out - source_in).max(1);
    let local_source_frame =
        source_offset_for_program_frame(local_program_frame, timeline_fps, source_fps)
            .clamp(0, source_span.saturating_sub(1));
    source_in + local_source_frame
}

#[cfg(test)]
fn source_offset_for_program_frame(
    local_program_frame: i64,
    _timeline_fps: f64,
    _source_fps: f64,
) -> i64 {
    local_program_frame.max(0)
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn boundary_transition_after_source_frame(
    parts: &[SegmentProgramPart<'_>],
    current_part_id: &str,
    open_clip_id: &str,
    source_frame: i64,
) -> Option<SegmentBoundaryTransition> {
    let current = part_by_id(parts, current_part_id)?;
    if !valid_part(current) {
        return None;
    }
    if !open_clip_id.trim().is_empty() && current.clip_id.trim() != open_clip_id.trim() {
        return None;
    }
    if source_frame.max(0) < current.source_out_frame.max(current.source_in_frame + 1) {
        return None;
    }
    let current_index = parts
        .iter()
        .position(|part| part.part_id == current.part_id)?;
    parts
        .iter()
        .skip(current_index + 1)
        .filter(|part| valid_part(part))
        .next()
        .map(|part| SegmentBoundaryTransition {
            part_id: part.part_id.to_string(),
            program_frame: program_start_frame(part),
            source_frame: part.source_in_frame.max(0),
        })
}

#[cfg(test)]
pub(crate) fn boundary_transition_imminent_after_source_frame(
    parts: &[SegmentProgramPart<'_>],
    current_part_id: &str,
    open_clip_id: &str,
    source_frame: i64,
    transport_playing: bool,
) -> Option<SegmentBoundaryTransition> {
    if !transport_playing {
        return None;
    }
    let current = part_by_id(parts, current_part_id)?;
    if !valid_part(current) {
        return None;
    }
    if !open_clip_id.trim().is_empty() && current.clip_id.trim() != open_clip_id.trim() {
        return None;
    }
    let out = current.source_out_frame.max(current.source_in_frame + 1);
    let frame = source_frame.max(0);
    // Last in-range frame before broadcast player pauses at OUT.
    if frame + 1 < out {
        return None;
    }
    let current_index = parts
        .iter()
        .position(|part| part.part_id == current.part_id)?;
    parts
        .iter()
        .skip(current_index + 1)
        .filter(|part| valid_part(part))
        .next()
        .map(|part| SegmentBoundaryTransition {
            part_id: part.part_id.to_string(),
            program_frame: program_start_frame(part),
            source_frame: part.source_in_frame.max(0),
        })
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn valid_part(part: &SegmentProgramPart<'_>) -> bool {
    !part.part_id.trim().is_empty()
        && !part.clip_id.trim().is_empty()
        && part.program_end_frame > part.program_start_frame
        && part.source_out_frame > part.source_in_frame
}

#[cfg(test)]
fn program_start_frame(part: &SegmentProgramPart<'_>) -> i64 {
    part.program_start_frame.max(0)
}

#[cfg(test)]
fn program_end_frame(part: &SegmentProgramPart<'_>) -> i64 {
    let start = program_start_frame(part);
    part.program_end_frame.max(start + 1)
}

#[cfg(test)]
fn program_span(part: &SegmentProgramPart<'_>) -> i64 {
    (program_end_frame(part) - program_start_frame(part)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylist, EditorialPlaylistSegment};
    use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};

    fn parts() -> Vec<SegmentProgramPart<'static>> {
        vec![
            SegmentProgramPart {
                part_id: "part_a",
                clip_id: "clip_a",
                program_start_frame: 0,
                program_end_frame: 50,
                timeline_fps: 50.0,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 50.0,
            },
            SegmentProgramPart {
                part_id: "part_b",
                clip_id: "clip_a",
                program_start_frame: 50,
                program_end_frame: 100,
                timeline_fps: 50.0,
                source_in_frame: 200,
                source_out_frame: 250,
                source_fps: 50.0,
            },
        ]
    }

    #[test]
    fn program_edl_frame_detects_edit_cut() {
        use super::ProgramEdlFrame;
        let prev = ProgramEdlFrame {
            kind: SegmentPlaybackLayerKind::PartVideo,
            program_frame: 49,
            part_id: "part_a".into(),
            cover_id: String::new(),
            clip_id: "clip_a".into(),
            source_frame: 149,
        };
        let at_cut = ProgramEdlFrame {
            kind: SegmentPlaybackLayerKind::PartVideo,
            program_frame: 50,
            part_id: "part_b".into(),
            cover_id: String::new(),
            clip_id: "clip_a".into(),
            source_frame: 200,
        };
        assert!(at_cut.is_edl_cut_from(&prev));

        let mixed_fps_step_inside_same_take = ProgramEdlFrame {
            kind: SegmentPlaybackLayerKind::PartVideo,
            program_frame: 50,
            part_id: "part_a".into(),
            cover_id: String::new(),
            clip_id: "clip_a".into(),
            source_frame: 151,
        };
        assert!(!mixed_fps_step_inside_same_take.is_edl_cut_from(&prev));
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
    fn resolve_playback_target_maps_tonovi_source_in_out() {
        let parts = parts();
        let model = SegmentProgramModel::from_playlist(
            Some(&EditorialPlaylist {
                project_id: "p".into(),
                timeline_fps: 50.0,
                duration_frames: 100,
                duration_sec: 4.0,
                segments: parts
                    .iter()
                    .enumerate()
                    .map(|(idx, part)| EditorialPlaylistSegment {
                        part_id: part.part_id.to_string(),
                        kind: "tonovi".into(),
                        clip_id: part.clip_id.to_string(),
                        global_start_frame: if idx == 0 { 0 } else { 50 },
                        global_end_frame: if idx == 0 { 50 } else { 100 },
                        duration_frames: 50,
                        source_in_frame: part.source_in_frame,
                        source_out_frame: part.source_out_frame,
                        source_fps: 50.0,
                        streamable: true,
                        ..EditorialPlaylistSegment::default()
                    })
                    .collect(),
            }),
            &[],
            &[],
            &[],
        );
        let target = model.resolve_playback_target(55, &[]).expect("part_b");
        assert_eq!(target.kind, SegmentPlaybackLayerKind::PartVideo);
        assert_eq!(target.part_id, "part_b");
        assert_eq!(target.source_frame, 205);
    }

    #[test]
    fn boundary_transition_imminent_detects_last_in_range_frame_while_playing() {
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
        assert!(boundary_transition_imminent_after_source_frame(
            &parts, "part_a", "clip_a", 148, true
        )
        .is_none());
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
    fn program_seek_maps_to_active_part_source_range() {
        let parts = parts();

        assert_eq!(source_frame_for_program_frame(&parts, 50), Some(200));
        assert_eq!(source_frame_for_program_frame(&parts, 99), Some(249));
    }

    #[test]
    fn program_seek_keeps_mixed_fps_source_frames_contiguous() {
        let parts = [SegmentProgramPart {
            part_id: "part_50fps",
            clip_id: "clip_fast",
            program_start_frame: 0,
            program_end_frame: 25,
            timeline_fps: 50.0,
            source_in_frame: 100,
            source_out_frame: 150,
            source_fps: 59.94,
        }];

        assert_eq!(source_frame_for_program_frame(&parts, 0), Some(100));
        assert_eq!(source_frame_for_program_frame(&parts, 1), Some(101));
        assert_eq!(source_frame_for_program_frame(&parts, 24), Some(124));
        assert_eq!(
            program_frame_for_source_frame(&parts, "part_50fps", "clip_fast", 124)
                .map(|projection| projection.program_frame),
            Some(24)
        );
    }

    #[test]
    fn cover_target_keeps_mixed_fps_source_frames_contiguous() {
        let playlist = EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            segments: vec![EditorialPlaylistSegment {
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
                covers: vec![EditorialPlaylistCover {
                    cover_id: "cover_fast".into(),
                    clip_id: "clip_b".into(),
                    virtual_shot_id: "vcover".into(),
                    timeline_start_frame: 10,
                    timeline_end_frame: 20,
                    source_in_frame: 40,
                    source_out_frame: 80,
                    source_fps: 50.0,
                    streamable: true,
                    source: Default::default(),
                }],
                ..EditorialPlaylistSegment::default()
            }],
            ..EditorialPlaylist::default()
        };
        let program = SegmentProgramModel::from_playlist(Some(&playlist), &[], &[], &[]);

        let target = program.resolve_playback_target(11, &[]).unwrap();

        assert_eq!(target.kind, SegmentPlaybackLayerKind::CoverVideo);
        assert_eq!(target.source_fps, 50.0);
        assert_eq!(target.source_frame, 41);
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
            timeline_fps: 50.0,
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
