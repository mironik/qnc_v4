//! QNC segment timeline - neutral segment/program timeline component.
//!
//! This component is not a Story form and does not wrap `qnc_timeline`.
//! It represents one continuous wrap/program timeline that can visually wrap
//! into multiple rows, like one text document displayed across editor lines.
//! It owns only segment-timeline UI geometry and paint:
//! carrier/progress, A1, V/segment base, A2, M pins, M-M slots, covers,
//! and playhead projection.
//!
//! Durable meaning comes from DB/API snapshots. Runtime position comes from the
//! broadcast/player carrier. This module never derives story state and never
//! talks to playback directly.

use eframe::egui::{self, Color32, Vec2};

mod css {
    use eframe::egui::Color32;

    pub const LABEL_COL_W: f32 = 28.0;
    pub const VIDEO_H: f32 = 64.0;
    pub const AUDIO_H: f32 = 15.0;
    pub const AUDIO_EXPANDED_H: f32 = 52.0;
    pub const ROW_GAP: f32 = 3.0;

    pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0f, 0x19);
    pub const VIDEO_BG: Color32 = Color32::from_rgb(0x11, 0x18, 0x27);
    pub const AUDIO_PRIMARY_BG: Color32 = Color32::from_rgb(0x11, 0x18, 0x27);
    pub const AUDIO_SECONDARY_BG: Color32 = Color32::from_rgb(0x0f, 0x17, 0x2a);
    pub const LABEL_BG: Color32 = Color32::from_rgb(0x1f, 0x29, 0x37);
    pub const LINE: Color32 = Color32::from_rgb(55, 65, 81);
    pub const SEGMENT_TON: Color32 = Color32::from_rgb(16, 185, 129);
    pub const SEGMENT_OFF: Color32 = Color32::from_rgb(107, 114, 128);
    pub const COVER: Color32 = Color32::from_rgb(234, 179, 8);
    pub const FOCUS: Color32 = Color32::from_rgb(255, 180, 60);
    pub const PLAYHEAD: Color32 = Color32::from_rgb(0x4e, 0xc9, 0xb0);
    pub const MUTED: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
    pub const MARKER: Color32 = Color32::from_rgb(244, 114, 182);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentAudioExpansion {
    #[default]
    None,
    A1,
    A2,
}

impl SegmentAudioExpansion {
    fn lane_h(self, lane: Self) -> f32 {
        if self == lane && !matches!(lane, Self::None) {
            css::AUDIO_EXPANDED_H
        } else {
            css::AUDIO_H
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
    pub expanded_audio: SegmentAudioExpansion,
    pub show_lane_labels: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentTimelineIntent {
    None,
    CueFrame(i64),
    ToggleAudioExpand(SegmentAudioExpansion),
    SelectMarkerSlot(String),
    SelectCover(String),
    SelectMarker(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentTimelineProgramIntent {
    None,
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

pub fn show_program(
    ui: &mut egui::Ui,
    input: SegmentTimelineProgramInput<'_>,
) -> SegmentTimelineProgramIntent {
    let layers = segment_layers();
    let mut out = SegmentTimelineProgramIntent::None;
    let count = input.segments.len();
    if count == 0 {
        return out;
    }

    let width = ui.available_width();
    let total_h = program_content_height(count, layers, input.expanded_audio);
    egui::Frame::NONE
        .fill(css::BG)
        .stroke(egui::Stroke::new(1.0, css::LINE))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(width, total_h));
            ui.set_max_height(total_h);
            ui.spacing_mut().item_spacing = Vec2::new(0.0, css::ROW_GAP);

            for (index, segment) in input.segments.iter().copied().enumerate() {
                let row = SegmentTimelineProgramRow {
                    index,
                    count,
                    start_frame: segment.start_frame,
                    end_frame: segment.end_frame,
                };
                let row_intent = paint_program_row(
                    ui,
                    layers,
                    row,
                    segment,
                    input.playhead_program_frame,
                    input.covers,
                    input.marker_slots,
                    input.markers,
                    input.expanded_audio,
                    index == 0 && input.show_lane_labels,
                );
                merge_program_intent(&mut out, row_intent);
                if index + 1 < count {
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
    segment: SegmentTimelineProgramSegment<'_>,
    playhead_program_frame: i64,
    covers: &[SegmentTimelineProgramCover<'_>],
    marker_slots: &[SegmentTimelineProgramMarkerSlot<'_>],
    markers: &[SegmentTimelineProgramMarker<'_>],
    expanded_audio: SegmentAudioExpansion,
    show_lane_labels: bool,
) -> SegmentTimelineProgramIntent {
    let duration_frames = row.duration_frames().max(1);
    let local_playhead = row.local_playhead_frame(playhead_program_frame);
    let local_covers = local_covers_for_row(row, covers);
    let local_slots = local_marker_slots_for_row(row, marker_slots);
    let local_markers = local_markers_for_row(row, markers);
    let local_segment = SegmentTimelineSegment {
        id: segment.id,
        kind: segment.kind,
        start_frame: 0,
        end_frame: duration_frames,
        has_base_video: segment.has_base_video,
        selected: segment.selected,
    };
    let mut out = SegmentTimelineProgramIntent::None;

    if layers.audio_a1 {
        let local_intent = paint_audio_row(
            ui,
            "A1",
            SegmentAudioExpansion::A1,
            expanded_audio,
            duration_frames,
            css::AUDIO_PRIMARY_BG,
            show_lane_labels,
            row.index,
        );
        merge_program_intent(
            &mut out,
            program_intent_from_local_intent(local_intent, row, markers),
        );
    }

    if layers.carrier || layers.base_video || layers.covers || layers.playhead {
        let local_intent = paint_video_stack(
            ui,
            duration_frames,
            local_playhead.unwrap_or(0),
            local_playhead.is_some(),
            local_segment,
            &local_covers,
            &local_slots,
            &local_markers,
            show_lane_labels,
        );
        merge_program_intent(
            &mut out,
            program_intent_from_local_intent(local_intent, row, markers),
        );
    }

    if layers.audio_a2 {
        let local_intent = paint_audio_row(
            ui,
            "A2",
            SegmentAudioExpansion::A2,
            expanded_audio,
            duration_frames,
            css::AUDIO_SECONDARY_BG,
            show_lane_labels,
            row.index,
        );
        merge_program_intent(
            &mut out,
            program_intent_from_local_intent(local_intent, row, markers),
        );
    }

    out
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

fn program_intent_from_local_intent(
    intent: SegmentTimelineIntent,
    row: SegmentTimelineProgramRow,
    markers: &[SegmentTimelineProgramMarker<'_>],
) -> SegmentTimelineProgramIntent {
    match intent {
        SegmentTimelineIntent::None => SegmentTimelineProgramIntent::None,
        SegmentTimelineIntent::CueFrame(local_frame) => {
            SegmentTimelineProgramIntent::CueProgramFrame(row.program_frame_from_local(local_frame))
        }
        SegmentTimelineIntent::ToggleAudioExpand(lane) => {
            SegmentTimelineProgramIntent::ToggleAudioExpand(lane)
        }
        SegmentTimelineIntent::SelectMarkerSlot(slot_id) => {
            SegmentTimelineProgramIntent::SelectMarkerSlot(slot_id)
        }
        SegmentTimelineIntent::SelectCover(cover_id) => {
            SegmentTimelineProgramIntent::SelectCover(cover_id)
        }
        SegmentTimelineIntent::SelectMarker(marker_id) => {
            let program_frame = markers
                .iter()
                .find(|marker| marker.id == marker_id)
                .map(|marker| marker.frame.max(0))
                .unwrap_or_else(|| row.start_frame())
                .clamp(row.start_frame(), row.end_frame());
            SegmentTimelineProgramIntent::SelectMarker {
                marker_id,
                program_frame,
            }
        }
    }
}

fn content_height(layers: SegmentLayerFlags, expanded_audio: SegmentAudioExpansion) -> f32 {
    let mut h = 0.0;
    let mut rows = 0u32;
    let mut push = |row_h: f32| {
        if rows > 0 {
            h += css::ROW_GAP;
        }
        h += row_h;
        rows += 1;
    };
    if layers.audio_a1 {
        push(expanded_audio.lane_h(SegmentAudioExpansion::A1));
    }
    if layers.carrier || layers.base_video || layers.covers || layers.playhead {
        push(css::VIDEO_H);
    }
    if layers.audio_a2 {
        push(expanded_audio.lane_h(SegmentAudioExpansion::A2));
    }
    h
}

fn paint_audio_row(
    ui: &mut egui::Ui,
    label: &str,
    lane: SegmentAudioExpansion,
    expanded_audio: SegmentAudioExpansion,
    duration_frames: i64,
    bg: Color32,
    show_lane_labels: bool,
    id_salt: usize,
) -> SegmentTimelineIntent {
    let height = expanded_audio.lane_h(lane);
    let (row, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    let label_rect = label_rect(row);
    let label_resp = ui.interact(
        label_rect,
        ui.id().with(("segment_audio_label", label, id_salt)),
        egui::Sense::click(),
    );
    if label_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    paint_label(
        ui.painter(),
        row,
        show_lane_labels.then_some(label).unwrap_or(""),
        show_lane_labels && expanded_audio == lane,
    );
    let inner = track_inner(row);
    ui.painter().rect_filled(inner, 0.0, bg);
    ui.painter().rect_stroke(
        inner,
        0.0,
        egui::Stroke::new(1.0, css::LINE),
        egui::StrokeKind::Inside,
    );
    paint_audio_midline(ui.painter(), inner);
    if show_lane_labels && label_resp.clicked() {
        return SegmentTimelineIntent::ToggleAudioExpand(lane);
    }
    seek_from_pointer(&response, inner, duration_frames)
        .map(SegmentTimelineIntent::CueFrame)
        .unwrap_or(SegmentTimelineIntent::None)
}

fn paint_video_stack(
    ui: &mut egui::Ui,
    duration_frames: i64,
    playhead_frame: i64,
    show_playhead: bool,
    segment: SegmentTimelineSegment<'_>,
    covers: &[SegmentTimelineCover<'_>],
    marker_slots: &[SegmentTimelineMarkerSlot<'_>],
    markers: &[SegmentTimelineMarker<'_>],
    show_lane_labels: bool,
) -> SegmentTimelineIntent {
    let (row, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), css::VIDEO_H),
        egui::Sense::click_and_drag(),
    );
    paint_label(
        ui.painter(),
        row,
        show_lane_labels.then_some("V").unwrap_or(""),
        false,
    );
    let inner = track_inner(row);
    let painter = ui.painter();

    painter.rect_filled(inner, 0.0, css::VIDEO_BG);
    painter.rect_stroke(
        inner,
        0.0,
        egui::Stroke::new(1.0, css::LINE),
        egui::StrokeKind::Inside,
    );

    paint_marker_slots(painter, inner, duration_frames, marker_slots);
    paint_segment_range(painter, inner, duration_frames, segment);
    paint_covers(painter, inner, duration_frames, covers);
    paint_markers(painter, inner, duration_frames, markers);
    if show_playhead {
        paint_playhead(painter, inner, duration_frames, playhead_frame);
    }

    pointer_intent(
        &response,
        inner,
        duration_frames,
        covers,
        marker_slots,
        markers,
    )
}

fn label_rect(row: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(row.min, Vec2::new(css::LABEL_COL_W, row.height()))
}

fn track_inner(row: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(row.left() + css::LABEL_COL_W, row.top()),
        row.max,
    )
}

fn paint_label(painter: &egui::Painter, row: egui::Rect, label: &str, selected: bool) {
    let label_rect = label_rect(row);
    painter.rect_filled(label_rect, 0.0, css::LABEL_BG);
    painter.vline(
        label_rect.right(),
        label_rect.y_range(),
        egui::Stroke::new(1.0, css::LINE),
    );
    painter.text(
        label_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(9.0),
        if selected { css::FOCUS } else { css::MUTED },
    );
}

fn paint_audio_midline(painter: &egui::Painter, area: egui::Rect) {
    painter.hline(
        area.x_range(),
        area.center().y,
        egui::Stroke::new(1.0, css::LINE.linear_multiply(0.8)),
    );
}

fn clamp_frame(frame: i64, duration_frames: i64) -> i64 {
    frame.clamp(0, duration_frames.max(1))
}

fn x_for_frame(area: egui::Rect, duration_frames: i64, frame: i64) -> f32 {
    let duration_frames = duration_frames.max(1);
    let t = (frame.clamp(0, duration_frames) as f32) / (duration_frames as f32);
    area.left() + t * area.width()
}

fn paint_segment_range(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    segment: SegmentTimelineSegment<'_>,
) {
    let start = clamp_frame(segment.start_frame, duration_frames);
    let end = clamp_frame(segment.end_frame.max(segment.start_frame), duration_frames);
    if end <= start {
        return;
    }
    let x0 = x_for_frame(area, duration_frames, start);
    let x1 = x_for_frame(area, duration_frames, end);
    let rect = egui::Rect::from_min_max(
        egui::pos2(x0, area.top() + 17.0),
        egui::pos2(x1.max(x0 + 3.0), area.bottom() - 10.0),
    );
    let base = if segment.has_base_video {
        css::SEGMENT_TON
    } else {
        css::SEGMENT_OFF
    };
    painter.rect_filled(rect, 2.0, base.linear_multiply(0.24));
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(
            if segment.selected { 1.8 } else { 1.0 },
            if segment.selected { css::FOCUS } else { base },
        ),
        egui::StrokeKind::Inside,
    );

    let label = if segment.kind.trim().is_empty() {
        segment.id
    } else {
        segment.kind
    };
    if rect.width() >= 42.0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(10.0),
            Color32::from_rgb(229, 231, 235),
        );
    }
}

fn paint_marker_slots(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    slots: &[SegmentTimelineMarkerSlot<'_>],
) {
    for slot in slots {
        if slot.end_frame <= slot.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, slot.start_frame);
        let x1 = x_for_frame(area, duration_frames, slot.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        );
        let color = if slot.has_cover {
            css::COVER
        } else if slot.selected {
            css::PLAYHEAD
        } else {
            Color32::from_rgb(45, 212, 191)
        };
        painter.rect_filled(rect, 1.0, color.linear_multiply(0.38));
        painter.rect_stroke(
            rect,
            1.0,
            egui::Stroke::new(if slot.selected { 2.0 } else { 1.0 }, color),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_covers(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    covers: &[SegmentTimelineCover<'_>],
) {
    for cover in covers {
        if cover.end_frame <= cover.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, cover.start_frame);
        let x1 = x_for_frame(area, duration_frames, cover.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 4.0),
            egui::pos2(x1.max(x0 + 3.0), area.top() + area.height() * 0.34),
        );
        painter.rect_filled(
            rect,
            2.0,
            if cover.selected {
                css::FOCUS
            } else {
                css::COVER.linear_multiply(0.85)
            },
        );
    }
}

fn paint_markers(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    markers: &[SegmentTimelineMarker<'_>],
) {
    for marker in markers {
        let x = x_for_frame(area, duration_frames, marker.frame);
        painter.line_segment(
            [egui::pos2(x, area.top()), egui::pos2(x, area.bottom())],
            egui::Stroke::new(1.0, css::MARKER),
        );
        painter.text(
            egui::pos2(x + 3.0, area.top() + 2.0),
            egui::Align2::LEFT_TOP,
            "M",
            egui::FontId::proportional(9.0),
            css::MARKER,
        );
    }
}

fn paint_playhead(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    playhead_frame: i64,
) {
    let x = x_for_frame(area, duration_frames, playhead_frame);
    painter.line_segment(
        [egui::pos2(x, area.top()), egui::pos2(x, area.bottom())],
        egui::Stroke::new(2.5, css::FOCUS),
    );
}

fn pointer_intent(
    response: &egui::Response,
    inner: egui::Rect,
    duration_frames: i64,
    covers: &[SegmentTimelineCover<'_>],
    slots: &[SegmentTimelineMarkerSlot<'_>],
    markers: &[SegmentTimelineMarker<'_>],
) -> SegmentTimelineIntent {
    let Some(pos) = response.interact_pointer_pos() else {
        return SegmentTimelineIntent::None;
    };
    if !inner.expand(2.0).contains(pos) || inner.width() <= 0.0 {
        return SegmentTimelineIntent::None;
    }

    if response.clicked() {
        if let Some(id) = marker_hit(inner, duration_frames, pos, markers) {
            return SegmentTimelineIntent::SelectMarker(id.to_string());
        }
        if let Some(id) = cover_hit(inner, duration_frames, pos, covers) {
            return SegmentTimelineIntent::SelectCover(id.to_string());
        }
        if let Some(id) = marker_slot_hit(inner, duration_frames, pos, slots) {
            return SegmentTimelineIntent::SelectMarkerSlot(id.to_string());
        }
    }

    if response.clicked() || response.dragged() {
        return frame_from_pos(inner, duration_frames, pos)
            .map(SegmentTimelineIntent::CueFrame)
            .unwrap_or(SegmentTimelineIntent::None);
    }

    SegmentTimelineIntent::None
}

fn marker_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    markers: &'a [SegmentTimelineMarker<'a>],
) -> Option<&'a str> {
    let tolerance = 5.0;
    markers.iter().find_map(|marker| {
        let x = x_for_frame(area, duration_frames, marker.frame);
        (!marker.id.trim().is_empty() && (pos.x - x).abs() <= tolerance).then_some(marker.id)
    })
}

fn cover_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    covers: &'a [SegmentTimelineCover<'a>],
) -> Option<&'a str> {
    covers.iter().rev().find_map(|cover| {
        if cover.end_frame <= cover.start_frame || cover.id.trim().is_empty() {
            return None;
        }
        let x0 = x_for_frame(area, duration_frames, cover.start_frame);
        let x1 = x_for_frame(area, duration_frames, cover.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 4.0),
            egui::pos2(x1.max(x0 + 3.0), area.top() + area.height() * 0.34),
        )
        .expand(2.0);
        rect.contains(pos).then_some(cover.id)
    })
}

fn marker_slot_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    slots: &'a [SegmentTimelineMarkerSlot<'a>],
) -> Option<&'a str> {
    slots.iter().find_map(|slot| {
        if slot.end_frame <= slot.start_frame || slot.id.trim().is_empty() {
            return None;
        }
        let x0 = x_for_frame(area, duration_frames, slot.start_frame);
        let x1 = x_for_frame(area, duration_frames, slot.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + area.height() * 0.34),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        )
        .expand(2.0);
        rect.contains(pos).then_some(slot.id)
    })
}

fn frame_from_pos(inner: egui::Rect, duration_frames: i64, pos: egui::Pos2) -> Option<i64> {
    if inner.width() <= 0.0 {
        return None;
    }
    let t = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
    let duration_frames = duration_frames.max(1);
    Some(((t * duration_frames as f64).round() as i64).clamp(0, duration_frames))
}

fn seek_from_pointer(
    response: &egui::Response,
    inner: egui::Rect,
    duration_frames: i64,
) -> Option<i64> {
    if !(response.clicked() || response.dragged()) {
        return None;
    }
    let pos = response.interact_pointer_pos()?;
    if !inner.expand(2.0).contains(pos) {
        return None;
    }
    frame_from_pos(inner, duration_frames, pos)
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
    fn clamp_frame_uses_frame_carrier_bounds() {
        assert_eq!(clamp_frame(-4, 100), 0);
        assert_eq!(clamp_frame(25, 100), 25);
        assert_eq!(clamp_frame(200, 100), 100);
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
            program_intent_from_local_intent(SegmentTimelineIntent::CueFrame(12), row, &markers),
            SegmentTimelineProgramIntent::CueProgramFrame(62)
        );
        assert_eq!(
            program_intent_from_local_intent(
                SegmentTimelineIntent::SelectMarker("m_mid".into()),
                row,
                &markers,
            ),
            SegmentTimelineProgramIntent::SelectMarker {
                marker_id: "m_mid".into(),
                program_frame: 80,
            }
        );
    }

    #[test]
    fn hit_tests_timeline_layers_by_id() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(100.0, 60.0));
        let markers = vec![SegmentTimelineMarker {
            id: "m1",
            frame: 25,
        }];
        let covers = vec![SegmentTimelineCover {
            id: "cover_a",
            start_frame: 10,
            end_frame: 40,
            selected: false,
        }];
        let slots = vec![SegmentTimelineMarkerSlot {
            id: "slot_a",
            start_frame: 0,
            end_frame: 50,
            has_cover: false,
            selected: false,
        }];

        assert_eq!(
            marker_hit(area, 100, egui::pos2(25.0, 30.0), &markers),
            Some("m1")
        );
        assert_eq!(
            cover_hit(area, 100, egui::pos2(20.0, 8.0), &covers),
            Some("cover_a")
        );
        assert_eq!(
            marker_slot_hit(area, 100, egui::pos2(20.0, 42.0), &slots),
            Some("slot_a")
        );
    }
}
