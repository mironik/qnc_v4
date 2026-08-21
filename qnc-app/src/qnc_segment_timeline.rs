//! QNC segment timeline - neutral segment/program adapter.
//!
//! This component is not a Story form. It adapts segment/program owner data to
//! the shared `QncTimeline` layer contract, so Source, Segment and Program use
//! one timeline paint/hit-test implementation with different enabled layers.
//!
//! Durable meaning comes from DB/API snapshots. Runtime position comes from the
//! broadcast/player carrier. This module never derives story state and never
//! talks to playback directly.
//!
//! Segment timeline cheat sheet:
//! - Broadcast player owns play/pause/seek execution and the runtime clock.
//! - DB/API playlist owns segment, marker slot, marker and cover ranges.
//! - Segment rows are local UI projections of the same program axis.
//! - Program overview is the final/global UI projection of that same axis.
//! - Intents from this component are only program-frame or layer selections.
//! - This component never decides playback, next segment, source media, or timebase.

use eframe::egui::{self, Vec2};

use crate::qnc_timeline::{
    ExpandedAudio, LayerFlags, QncTimeline, TimelineCoverSpan, TimelineFocusPaint,
    TimelineInteract, TimelineMarkerPin, TimelineSlotSpan, TimelineVirtualSpan,
};

mod css {
    use eframe::egui::Color32;

    pub const ROW_GAP: f32 = 3.0;

    pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0f, 0x19);
    pub const LINE: Color32 = Color32::from_rgb(55, 65, 81);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentAudioExpansion {
    #[default]
    None,
    A1,
    A2,
}

impl SegmentAudioExpansion {
    pub fn toggle(self, lane: Self) -> Self {
        if matches!(lane, Self::None) {
            Self::None
        } else if self == lane {
            Self::None
        } else {
            lane
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentLayerFlags {
    pub carrier: bool,
    pub audio_a1: bool,
    pub audio_a2: bool,
    pub base_video: bool,
    pub covers: bool,
    pub markers: bool,
    pub marker_slots: bool,
    pub in_out: bool,
    pub shot_range: bool,
    pub playhead: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineSegment<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_base_video: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineCover<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineMarkerSlot<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_cover: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineMarker<'a> {
    pub id: &'a str,
    pub frame: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineProgramRow {
    pub index: usize,
    pub count: usize,
    pub start_frame: i64,
    pub end_frame: i64,
}

impl SegmentTimelineProgramRow {
    pub fn start_frame(self) -> i64 {
        self.start_frame.max(0)
    }

    pub fn end_frame(self) -> i64 {
        let start = self.start_frame();
        self.end_frame.max(start + 1)
    }

    pub fn duration_frames(self) -> i64 {
        self.end_frame() - self.start_frame()
    }

    pub fn is_last(self) -> bool {
        self.index + 1 == self.count
    }

    pub fn program_frame_from_local(self, local_frame: i64) -> i64 {
        self.start_frame() + local_frame.clamp(0, self.duration_frames())
    }

    pub fn local_playhead_frame(self, program_frame: i64) -> Option<i64> {
        let start = self.start_frame();
        let end = self.end_frame();
        let in_row = program_frame >= start
            && (program_frame < end || self.is_last() && program_frame == end);
        in_row.then(|| (program_frame - start).clamp(0, self.duration_frames()))
    }

    pub fn local_range(
        self,
        program_start_frame: i64,
        program_end_frame: i64,
    ) -> Option<(i64, i64)> {
        if program_end_frame <= program_start_frame {
            return None;
        }
        let start = program_start_frame.max(self.start_frame());
        let end = program_end_frame.min(self.end_frame());
        (end > start).then_some((start - self.start_frame(), end - self.start_frame()))
    }

    pub fn local_marker_frame(self, program_frame: i64) -> Option<i64> {
        let frame = program_frame.max(0);
        if frame < self.start_frame() || frame > self.end_frame() {
            return None;
        }
        let at_start = frame == self.start_frame();
        let at_end = frame == self.end_frame();
        if at_start && self.index != 0 {
            return None;
        }
        if at_end && !self.is_last() {
            return Some(self.duration_frames());
        }
        Some(frame - self.start_frame())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineProgramSegment<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_base_video: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineProgramCover<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineProgramMarkerSlot<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_cover: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SegmentTimelineProgramMarker<'a> {
    pub id: &'a str,
    pub frame: i64,
}

pub struct SegmentTimelineProgramInput<'a> {
    pub playhead_program_frame: i64,
    pub segments: &'a [SegmentTimelineProgramSegment<'a>],
    pub covers: &'a [SegmentTimelineProgramCover<'a>],
    pub marker_slots: &'a [SegmentTimelineProgramMarkerSlot<'a>],
    pub markers: &'a [SegmentTimelineProgramMarker<'a>],
    pub waveform_duration_frames: i64,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub expanded_audio: SegmentAudioExpansion,
    pub show_lane_labels: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentTimelineProgramIntent {
    None,
    /// UI request for a program-frame position. The playback owner decides what
    /// to do with it; this component does not play or seek media.
    CueProgramFrame(i64),
    ToggleAudioExpand(SegmentAudioExpansion),
    SelectMarkerSlot(String),
    SelectCover(String),
    SelectMarker {
        marker_id: String,
        program_frame: i64,
    },
}

pub fn segment_layers() -> SegmentLayerFlags {
    SegmentLayerFlags {
        carrier: true,
        audio_a1: true,
        audio_a2: true,
        base_video: true,
        covers: true,
        markers: true,
        marker_slots: true,
        in_out: false,
        shot_range: false,
        playhead: true,
    }
}

/// Segment-stack UI: one row per segment, but every row is a local projection
/// of the same continuous program timeline.
pub fn show_program(
    ui: &mut egui::Ui,
    input: SegmentTimelineProgramInput<'_>,
) -> SegmentTimelineProgramIntent {
    let layers = segment_layers();
    let mut out = SegmentTimelineProgramIntent::None;
    if input.segments.is_empty() {
        return out;
    }
    let rows = program_segment_rows(input.segments);
    let waveform_duration = input.waveform_duration_frames.max(1);
    let count = rows.len();

    let width = ui.available_width();
    let total_h = program_content_height(count, layers, input.expanded_audio);
    egui::Frame::NONE
        .fill(css::BG)
        .stroke(egui::Stroke::new(1.0, css::LINE))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(width, total_h));
            ui.set_max_height(total_h);
            ui.spacing_mut().item_spacing = Vec2::new(0.0, css::ROW_GAP);

            for row in rows {
                let row_intent = paint_program_row(
                    ui,
                    layers,
                    row,
                    input.segments,
                    input.playhead_program_frame,
                    input.covers,
                    input.marker_slots,
                    input.markers,
                    waveform_duration,
                    input.a1_peaks,
                    input.a2_peaks,
                    input.expanded_audio,
                    row.index == 0 && input.show_lane_labels,
                );
                merge_program_intent(&mut out, row_intent);
                if row.index + 1 < count {
                    ui.add_space(css::ROW_GAP);
                }
            }
        });

    out
}

/// Final/program UI: one compact overview row for the whole playlist.
/// This is still only a passive projection of the broadcast-player position.
pub fn show_program_overview(
    ui: &mut egui::Ui,
    input: SegmentTimelineProgramInput<'_>,
) -> SegmentTimelineProgramIntent {
    let layers = segment_layers();
    let mut out = SegmentTimelineProgramIntent::None;
    if input.segments.is_empty() {
        return out;
    }
    let program_duration = program_duration_frames(&input);
    let waveform_duration = input.waveform_duration_frames.max(1);
    let rows = program_visual_rows(program_duration);
    let count = rows.len();

    let width = ui.available_width();
    let total_h = program_content_height(count, layers, input.expanded_audio);
    egui::Frame::NONE
        .fill(css::BG)
        .stroke(egui::Stroke::new(1.0, css::LINE))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(width, total_h));
            ui.set_max_height(total_h);
            ui.spacing_mut().item_spacing = Vec2::new(0.0, css::ROW_GAP);

            for row in rows {
                let row_intent = paint_program_row(
                    ui,
                    layers,
                    row,
                    input.segments,
                    input.playhead_program_frame,
                    input.covers,
                    input.marker_slots,
                    input.markers,
                    waveform_duration,
                    input.a1_peaks,
                    input.a2_peaks,
                    input.expanded_audio,
                    row.index == 0 && input.show_lane_labels,
                );
                merge_program_intent(&mut out, row_intent);
                if row.index + 1 < count {
                    ui.add_space(css::ROW_GAP);
                }
            }
        });

    out
}

fn paint_program_row(
    ui: &mut egui::Ui,
    layers: SegmentLayerFlags,
    row: SegmentTimelineProgramRow,
    segments: &[SegmentTimelineProgramSegment<'_>],
    playhead_program_frame: i64,
    covers: &[SegmentTimelineProgramCover<'_>],
    marker_slots: &[SegmentTimelineProgramMarkerSlot<'_>],
    markers: &[SegmentTimelineProgramMarker<'_>],
    program_duration_frames: i64,
    program_a1_peaks: &[f32],
    program_a2_peaks: &[f32],
    expanded_audio: SegmentAudioExpansion,
    show_lane_labels: bool,
) -> SegmentTimelineProgramIntent {
    let duration_frames = row.duration_frames().max(1);
    let local_playhead = row.local_playhead_frame(playhead_program_frame);
    let local_covers = local_covers_for_row(row, covers);
    let local_slots = local_marker_slots_for_row(row, marker_slots);
    let local_markers = local_markers_for_row(row, markers);
    let local_segments = local_segments_for_row(row, segments);
    let virtual_spans = timeline_virtual_spans(&local_segments);
    let cover_spans = timeline_cover_spans(&local_covers);
    let slot_spans = timeline_slot_spans(&local_slots);
    let marker_pins = timeline_marker_pins(&local_markers);
    let timeline_layers = timeline_layers_for_row(layers, local_playhead);
    let a1_peaks = local_peaks_for_row(row, program_duration_frames, program_a1_peaks);
    let a2_peaks = local_peaks_for_row(row, program_duration_frames, program_a2_peaks);

    let interact = QncTimeline {
        layers: timeline_layers,
        duration_frames,
        playhead_frame: local_playhead.unwrap_or(0),
        shot_in_frame: 0,
        shot_out_frame: duration_frames,
        draft_in_frame: 0,
        draft_out_frame: duration_frames,
        video_background: None,
        focus: TimelineFocusPaint::Playhead,
        show_lane_labels,
        expanded_audio: timeline_expanded_audio(expanded_audio),
        a1_peaks: &a1_peaks,
        a2_peaks: &a2_peaks,
        a3_peaks: &[],
        a4_peaks: &[],
        virtual_spans: &virtual_spans,
        covers: &cover_spans,
        marker_slots: &slot_spans,
        markers: &marker_pins,
        base_video_blank: false,
    }
    .show(ui);

    program_intent_from_timeline_interact(interact, row, markers)
}

fn local_peaks_for_row(
    row: SegmentTimelineProgramRow,
    program_duration_frames: i64,
    program_peaks: &[f32],
) -> Vec<f32> {
    if program_peaks.is_empty() {
        return Vec::new();
    }
    let row_duration = row.duration_frames().max(1) as usize;
    let bucket_count = row_duration.clamp(24, program_peaks.len().max(24));
    let mut out = vec![0.0; bucket_count];
    let program_duration = program_duration_frames.max(1);
    let row_start = row.start_frame();
    let row_end = row.end_frame();
    for bucket in 0..bucket_count {
        let local_start = bucket as f64 * row.duration_frames() as f64 / bucket_count as f64;
        let local_end = (bucket + 1) as f64 * row.duration_frames() as f64 / bucket_count as f64;
        let program_start = row_start as f64 + local_start;
        let program_end = (row_start as f64 + local_end).min(row_end as f64);
        out[bucket] = max_program_peak(program_peaks, program_duration, program_start, program_end);
    }
    out
}

fn max_program_peak(
    program_peaks: &[f32],
    program_duration_frames: i64,
    program_start_frame: f64,
    program_end_frame: f64,
) -> f32 {
    let duration = program_duration_frames.max(1) as f64;
    let peak_len = program_peaks.len();
    let start = ((program_start_frame.max(0.0) / duration) * peak_len as f64)
        .floor()
        .clamp(0.0, peak_len as f64) as usize;
    let end = ((program_end_frame.max(program_start_frame + 1.0) / duration) * peak_len as f64)
        .ceil()
        .clamp(0.0, peak_len as f64) as usize;
    let end = end.max(start + 1).min(peak_len);
    program_peaks[start..end]
        .iter()
        .copied()
        .fold(0.0, f32::max)
}

fn program_duration_frames(input: &SegmentTimelineProgramInput<'_>) -> i64 {
    input
        .segments
        .iter()
        .map(|segment| segment.end_frame.max(segment.start_frame))
        .chain(
            input
                .covers
                .iter()
                .map(|cover| cover.end_frame.max(cover.start_frame)),
        )
        .chain(
            input
                .marker_slots
                .iter()
                .map(|slot| slot.end_frame.max(slot.start_frame)),
        )
        .chain(input.markers.iter().map(|marker| marker.frame.max(0)))
        .chain(std::iter::once(input.playhead_program_frame.max(0)))
        .max()
        .unwrap_or(1)
        .max(1)
}

fn program_visual_rows(duration_frames: i64) -> Vec<SegmentTimelineProgramRow> {
    vec![SegmentTimelineProgramRow {
        index: 0,
        count: 1,
        start_frame: 0,
        end_frame: duration_frames.max(1),
    }]
}

fn program_segment_rows(
    segments: &[SegmentTimelineProgramSegment<'_>],
) -> Vec<SegmentTimelineProgramRow> {
    let count = segments.len();
    segments
        .iter()
        .enumerate()
        .map(|(index, segment)| SegmentTimelineProgramRow {
            index,
            count,
            start_frame: segment.start_frame.max(0),
            end_frame: segment.end_frame.max(segment.start_frame + 1),
        })
        .collect()
}

fn merge_program_intent(
    out: &mut SegmentTimelineProgramIntent,
    row_intent: SegmentTimelineProgramIntent,
) {
    if matches!(out, SegmentTimelineProgramIntent::None)
        && !matches!(row_intent, SegmentTimelineProgramIntent::None)
    {
        *out = row_intent;
    }
}

fn program_content_height(
    row_count: usize,
    layers: SegmentLayerFlags,
    expanded_audio: SegmentAudioExpansion,
) -> f32 {
    if row_count == 0 {
        return 0.0;
    }
    let row_h = content_height(layers, expanded_audio);
    row_h * row_count as f32 + css::ROW_GAP * (row_count.saturating_sub(1) as f32)
}

fn local_covers_for_row<'a>(
    row: SegmentTimelineProgramRow,
    covers: &'a [SegmentTimelineProgramCover<'a>],
) -> Vec<SegmentTimelineCover<'a>> {
    covers
        .iter()
        .filter_map(|cover| {
            let (start_frame, end_frame) = row.local_range(
                cover.start_frame.max(0),
                cover.end_frame.max(cover.start_frame),
            )?;
            Some(SegmentTimelineCover {
                id: cover.id,
                start_frame,
                end_frame,
                selected: cover.selected,
            })
        })
        .collect()
}

fn local_marker_slots_for_row<'a>(
    row: SegmentTimelineProgramRow,
    slots: &'a [SegmentTimelineProgramMarkerSlot<'a>],
) -> Vec<SegmentTimelineMarkerSlot<'a>> {
    slots
        .iter()
        .filter_map(|slot| {
            let (start_frame, end_frame) = row.local_range(
                slot.start_frame.max(0),
                slot.end_frame.max(slot.start_frame),
            )?;
            Some(SegmentTimelineMarkerSlot {
                id: slot.id,
                start_frame,
                end_frame,
                has_cover: slot.has_cover,
                selected: slot.selected,
            })
        })
        .collect()
}

fn local_markers_for_row<'a>(
    row: SegmentTimelineProgramRow,
    markers: &'a [SegmentTimelineProgramMarker<'a>],
) -> Vec<SegmentTimelineMarker<'a>> {
    markers
        .iter()
        .filter_map(|marker| {
            let frame = row.local_marker_frame(marker.frame)?;
            Some(SegmentTimelineMarker {
                id: marker.id,
                frame,
            })
        })
        .collect()
}

fn local_segments_for_row<'a>(
    row: SegmentTimelineProgramRow,
    segments: &'a [SegmentTimelineProgramSegment<'a>],
) -> Vec<SegmentTimelineSegment<'a>> {
    segments
        .iter()
        .filter_map(|segment| {
            let (start_frame, end_frame) = row.local_range(
                segment.start_frame.max(0),
                segment.end_frame.max(segment.start_frame),
            )?;
            Some(SegmentTimelineSegment {
                id: segment.id,
                kind: segment.kind,
                start_frame,
                end_frame,
                has_base_video: segment.has_base_video,
                selected: segment.selected,
            })
        })
        .collect()
}

fn timeline_layers(layers: SegmentLayerFlags) -> LayerFlags {
    LayerFlags {
        carrier: layers.carrier,
        audio_a1: layers.audio_a1,
        audio_a2: layers.audio_a2,
        audio_a3: false,
        audio_a4: false,
        base_video: layers.base_video,
        shot_range: layers.shot_range,
        covers: layers.covers,
        markers: layers.markers,
        marker_slots: layers.marker_slots,
        in_out: layers.in_out,
        playhead: layers.playhead,
    }
}

fn timeline_layers_for_row(layers: SegmentLayerFlags, local_playhead: Option<i64>) -> LayerFlags {
    let mut timeline_layers = timeline_layers(layers);
    timeline_layers.playhead = timeline_layers.playhead && local_playhead.is_some();
    timeline_layers
}

fn timeline_expanded_audio(expanded_audio: SegmentAudioExpansion) -> ExpandedAudio {
    match expanded_audio {
        SegmentAudioExpansion::None => ExpandedAudio::None,
        SegmentAudioExpansion::A1 => ExpandedAudio::A1,
        SegmentAudioExpansion::A2 => ExpandedAudio::A2,
    }
}

fn segment_expanded_audio(expanded_audio: ExpandedAudio) -> SegmentAudioExpansion {
    match expanded_audio {
        ExpandedAudio::A1 => SegmentAudioExpansion::A1,
        ExpandedAudio::A2 => SegmentAudioExpansion::A2,
        ExpandedAudio::None | ExpandedAudio::A3 | ExpandedAudio::A4 => SegmentAudioExpansion::None,
    }
}

fn timeline_virtual_spans<'a>(
    segments: &'a [SegmentTimelineSegment<'a>],
) -> Vec<TimelineVirtualSpan<'a>> {
    segments
        .iter()
        .map(|segment| TimelineVirtualSpan {
            id: segment.id,
            label: if segment.kind.trim().is_empty() {
                segment.id
            } else {
                segment.kind
            },
            start_frame: segment.start_frame,
            end_frame: segment.end_frame,
            has_base_video: segment.has_base_video,
            selected: segment.selected,
        })
        .collect()
}

fn timeline_cover_spans<'a>(covers: &'a [SegmentTimelineCover<'a>]) -> Vec<TimelineCoverSpan<'a>> {
    covers
        .iter()
        .map(|cover| TimelineCoverSpan {
            id: cover.id,
            start_frame: cover.start_frame,
            end_frame: cover.end_frame,
            selected: cover.selected,
        })
        .collect()
}

fn timeline_slot_spans<'a>(
    slots: &'a [SegmentTimelineMarkerSlot<'a>],
) -> Vec<TimelineSlotSpan<'a>> {
    slots
        .iter()
        .map(|slot| TimelineSlotSpan {
            id: slot.id,
            start_frame: slot.start_frame,
            end_frame: slot.end_frame,
            has_cover: slot.has_cover,
            selected: slot.selected,
        })
        .collect()
}

fn timeline_marker_pins<'a>(
    markers: &'a [SegmentTimelineMarker<'a>],
) -> Vec<TimelineMarkerPin<'a>> {
    markers
        .iter()
        .map(|marker| TimelineMarkerPin {
            id: marker.id,
            timeline_frame: marker.frame,
        })
        .collect()
}

fn program_intent_from_timeline_interact(
    interact: TimelineInteract,
    row: SegmentTimelineProgramRow,
    markers: &[SegmentTimelineProgramMarker<'_>],
) -> SegmentTimelineProgramIntent {
    if interact.row_start_click {
        return SegmentTimelineProgramIntent::CueProgramFrame(row.start_frame());
    }
    if let Some(lane) = interact.expand_click {
        return SegmentTimelineProgramIntent::ToggleAudioExpand(segment_expanded_audio(lane));
    }
    if let Some(marker_id) = interact.select_marker {
        let program_frame = markers
            .iter()
            .find(|marker| marker.id == marker_id)
            .map(|marker| marker.frame.max(0))
            .unwrap_or_else(|| row.start_frame())
            .clamp(row.start_frame(), row.end_frame());
        return SegmentTimelineProgramIntent::SelectMarker {
            marker_id,
            program_frame,
        };
    }
    if let Some(cover_id) = interact.select_cover {
        return SegmentTimelineProgramIntent::SelectCover(cover_id);
    }
    if let Some(slot_id) = interact.select_marker_slot {
        return SegmentTimelineProgramIntent::SelectMarkerSlot(slot_id);
    }
    if let Some(frame) = interact.seek_frame {
        return SegmentTimelineProgramIntent::CueProgramFrame(row.program_frame_from_local(frame));
    }
    SegmentTimelineProgramIntent::None
}

fn content_height(layers: SegmentLayerFlags, expanded_audio: SegmentAudioExpansion) -> f32 {
    QncTimeline {
        layers: timeline_layers(layers),
        duration_frames: 1,
        playhead_frame: 0,
        shot_in_frame: 0,
        shot_out_frame: 1,
        draft_in_frame: 0,
        draft_out_frame: 1,
        video_background: None,
        focus: TimelineFocusPaint::Playhead,
        show_lane_labels: true,
        expanded_audio: timeline_expanded_audio(expanded_audio),
        a1_peaks: &[],
        a2_peaks: &[],
        a3_peaks: &[],
        a4_peaks: &[],
        virtual_spans: &[],
        covers: &[],
        marker_slots: &[],
        markers: &[],
        base_video_blank: false,
    }
    .content_height()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_layers_keep_marker_slots_independent_from_in_out() {
        let layers = segment_layers();

        assert!(layers.carrier);
        assert!(layers.audio_a1);
        assert!(layers.audio_a2);
        assert!(layers.covers);
        assert!(layers.markers);
        assert!(layers.marker_slots);
        assert!(!layers.in_out);
        assert!(!layers.shot_range);
    }

    #[test]
    fn segment_audio_expansion_toggles_like_shared_timeline() {
        assert_eq!(
            SegmentAudioExpansion::None.toggle(SegmentAudioExpansion::A1),
            SegmentAudioExpansion::A1
        );
        assert_eq!(
            SegmentAudioExpansion::A1.toggle(SegmentAudioExpansion::A1),
            SegmentAudioExpansion::None
        );
        assert_eq!(
            SegmentAudioExpansion::A1.toggle(SegmentAudioExpansion::A2),
            SegmentAudioExpansion::A2
        );
    }

    #[test]
    fn row_adapter_draws_playhead_only_on_active_projection() {
        let layers = segment_layers();

        assert!(timeline_layers_for_row(layers, Some(0)).playhead);
        assert!(!timeline_layers_for_row(layers, None).playhead);
    }

    #[test]
    fn program_row_projects_continuous_wrap_frame_to_local_segment_rows() {
        let first = SegmentTimelineProgramRow {
            index: 0,
            count: 3,
            start_frame: 0,
            end_frame: 50,
        };
        let second = SegmentTimelineProgramRow {
            index: 1,
            count: 3,
            start_frame: 50,
            end_frame: 100,
        };
        let last = SegmentTimelineProgramRow {
            index: 2,
            count: 3,
            start_frame: 100,
            end_frame: 125,
        };

        assert_eq!(first.local_playhead_frame(0), Some(0));
        assert_eq!(first.local_playhead_frame(49), Some(49));
        assert_eq!(first.local_playhead_frame(50), None);
        assert_eq!(second.local_playhead_frame(50), Some(0));
        assert_eq!(second.local_playhead_frame(75), Some(25));
        assert_eq!(second.local_playhead_frame(100), None);
        assert_eq!(last.local_playhead_frame(125), Some(25));
    }

    #[test]
    fn program_row_maps_local_seek_to_continuous_wrap_frame() {
        let row = SegmentTimelineProgramRow {
            index: 1,
            count: 3,
            start_frame: 50,
            end_frame: 100,
        };

        assert_eq!(row.program_frame_from_local(0), 50);
        assert_eq!(row.program_frame_from_local(12), 62);
        assert_eq!(row.program_frame_from_local(200), 100);
    }

    #[test]
    fn program_visual_rows_are_not_playlist_item_rows() {
        let segments = [
            SegmentTimelineProgramSegment {
                id: "virtual_a",
                kind: "tonovi",
                start_frame: 0,
                end_frame: 50,
                has_base_video: true,
                selected: false,
            },
            SegmentTimelineProgramSegment {
                id: "virtual_b",
                kind: "offovi",
                start_frame: 50,
                end_frame: 90,
                has_base_video: false,
                selected: true,
            },
        ];
        let input = SegmentTimelineProgramInput {
            playhead_program_frame: 55,
            segments: &segments,
            covers: &[],
            marker_slots: &[],
            markers: &[],
            waveform_duration_frames: 90,
            a1_peaks: &[],
            a2_peaks: &[],
            expanded_audio: SegmentAudioExpansion::None,
            show_lane_labels: true,
        };

        let rows = program_visual_rows(program_duration_frames(&input));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].start_frame(), 0);
        assert_eq!(rows[0].end_frame(), 90);
        assert_eq!(rows[0].local_playhead_frame(55), Some(55));
    }

    #[test]
    fn program_segment_rows_are_local_ui_projections_of_playlist_items() {
        let segments = [
            SegmentTimelineProgramSegment {
                id: "virtual_a",
                kind: "tonovi",
                start_frame: 0,
                end_frame: 50,
                has_base_video: true,
                selected: false,
            },
            SegmentTimelineProgramSegment {
                id: "virtual_b",
                kind: "offovi",
                start_frame: 50,
                end_frame: 90,
                has_base_video: false,
                selected: true,
            },
        ];

        let rows = program_segment_rows(&segments);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].start_frame(), 0);
        assert_eq!(rows[0].end_frame(), 50);
        assert_eq!(rows[1].start_frame(), 50);
        assert_eq!(rows[1].end_frame(), 90);
        assert_eq!(rows[1].program_frame_from_local(5), 55);
    }

    #[test]
    fn playlist_items_are_ranges_inside_the_same_visual_row() {
        let row = SegmentTimelineProgramRow {
            index: 0,
            count: 1,
            start_frame: 0,
            end_frame: 90,
        };
        let segments = [
            SegmentTimelineProgramSegment {
                id: "virtual_a",
                kind: "tonovi",
                start_frame: 0,
                end_frame: 50,
                has_base_video: true,
                selected: false,
            },
            SegmentTimelineProgramSegment {
                id: "virtual_b",
                kind: "offovi",
                start_frame: 50,
                end_frame: 90,
                has_base_video: false,
                selected: true,
            },
        ];

        let local = local_segments_for_row(row, &segments);

        assert_eq!(local.len(), 2);
        assert_eq!(local[0].start_frame, 0);
        assert_eq!(local[0].end_frame, 50);
        assert_eq!(local[1].start_frame, 50);
        assert_eq!(local[1].end_frame, 90);
        assert!(local[1].selected);
    }

    #[test]
    fn segment_adapter_maps_program_ranges_to_qnc_timeline_spans() {
        let row = SegmentTimelineProgramRow {
            index: 0,
            count: 1,
            start_frame: 0,
            end_frame: 90,
        };
        let segments = [
            SegmentTimelineProgramSegment {
                id: "virtual_a",
                kind: "tonovi",
                start_frame: 0,
                end_frame: 50,
                has_base_video: true,
                selected: false,
            },
            SegmentTimelineProgramSegment {
                id: "virtual_b",
                kind: "",
                start_frame: 50,
                end_frame: 90,
                has_base_video: false,
                selected: true,
            },
        ];

        let local = local_segments_for_row(row, &segments);
        let spans = timeline_virtual_spans(&local);

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].id, "virtual_a");
        assert_eq!(spans[0].label, "tonovi");
        assert_eq!(spans[0].start_frame, 0);
        assert_eq!(spans[0].end_frame, 50);
        assert!(spans[0].has_base_video);
        assert_eq!(spans[1].id, "virtual_b");
        assert_eq!(spans[1].label, "virtual_b");
        assert_eq!(spans[1].start_frame, 50);
        assert_eq!(spans[1].end_frame, 90);
        assert!(!spans[1].has_base_video);
        assert!(spans[1].selected);
    }

    #[test]
    fn program_row_maps_global_cover_slot_ranges_to_local_row_ranges() {
        let row = SegmentTimelineProgramRow {
            index: 1,
            count: 3,
            start_frame: 50,
            end_frame: 100,
        };

        assert_eq!(row.local_range(40, 70), Some((0, 20)));
        assert_eq!(row.local_range(70, 130), Some((20, 50)));
        assert_eq!(row.local_range(0, 20), None);
    }

    #[test]
    fn program_row_keeps_only_first_start_and_last_end_marker_boundaries() {
        let first = SegmentTimelineProgramRow {
            index: 0,
            count: 3,
            start_frame: 0,
            end_frame: 50,
        };
        let middle = SegmentTimelineProgramRow {
            index: 1,
            count: 3,
            start_frame: 50,
            end_frame: 100,
        };
        let last = SegmentTimelineProgramRow {
            index: 2,
            count: 3,
            start_frame: 100,
            end_frame: 125,
        };

        assert_eq!(first.local_marker_frame(0), Some(0));
        assert_eq!(first.local_marker_frame(50), Some(50));
        assert_eq!(middle.local_marker_frame(50), None);
        assert_eq!(middle.local_marker_frame(100), Some(50));
        assert_eq!(last.local_marker_frame(100), None);
        assert_eq!(last.local_marker_frame(125), Some(25));
    }

    #[test]
    fn program_intents_return_wrap_program_frames() {
        let row = SegmentTimelineProgramRow {
            index: 1,
            count: 3,
            start_frame: 50,
            end_frame: 100,
        };
        let markers = [SegmentTimelineProgramMarker {
            id: "m_mid",
            frame: 80,
        }];

        assert_eq!(
            program_intent_from_timeline_interact(
                TimelineInteract {
                    seek_frame: Some(12),
                    ..TimelineInteract::default()
                },
                row,
                &markers,
            ),
            SegmentTimelineProgramIntent::CueProgramFrame(62)
        );
        assert_eq!(
            program_intent_from_timeline_interact(
                TimelineInteract {
                    row_start_click: true,
                    ..TimelineInteract::default()
                },
                row,
                &markers,
            ),
            SegmentTimelineProgramIntent::CueProgramFrame(50)
        );
        assert_eq!(
            program_intent_from_timeline_interact(
                TimelineInteract {
                    select_marker: Some("m_mid".into()),
                    ..TimelineInteract::default()
                },
                row,
                &markers,
            ),
            SegmentTimelineProgramIntent::SelectMarker {
                marker_id: "m_mid".into(),
                program_frame: 80,
            }
        );
    }
}
