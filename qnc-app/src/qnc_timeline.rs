//! QNC-timeline — one native timeline component, symbolic layer paint.
//!
//! Form-agnostic: this component does not know Ingest, Story, Media Assist,
//! Wrap, or any workflow screen. Owners pass [`LayerFlags`] + paint data.
//!
//! Layers (docs/qnc-timeline.md):
//!   carrier = logical clock, not painted
//!   → audio A1..A4 → base video → pokrivalice → IN/OUT + playhead
//!
//! Layers are visual UI only. They do not play media.
//!
//! Virtual-shot grouping is owner data; this component only paints supplied spans.
//!
//! # M markers — not owned here
//!
//! M / slot logic lives outside this UI component. This component never derives
//! slots from markers.
//!
//! When the owner enables the **covers** (pokrivalice) layer, it may call that
//! owner logic and pass ready-made `covers` / `markers` / `marker_slots` for
//! paint. Without an active covers layer, M pins and slots are not painted.

use eframe::egui::{self, Color32, Vec2};

/// Layout + paint tokens — owned by this component (egui only).
pub mod css {
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
    pub const WAVE_A1: Color32 = Color32::from_rgb(16, 185, 129);
    pub const WAVE_A2: Color32 = Color32::from_rgb(107, 114, 128);
    pub const WAVE_A3: Color32 = Color32::from_rgb(75, 85, 99);
    pub const WAVE_A4: Color32 = Color32::from_rgb(55, 65, 81);
    pub const FOCUS: Color32 = Color32::from_rgb(255, 180, 60);
    pub const SLOT_SELECTED: Color32 = Color32::from_rgb(0x0f, 0x76, 0x6e);
    pub const PLAYHEAD: Color32 = Color32::from_rgb(0x4e, 0xc9, 0xb0);
    pub const MUTED: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
    pub const IO_HANDLE: Color32 = Color32::WHITE;
    pub fn io_dim() -> Color32 {
        Color32::from_black_alpha(133)
    }
    pub fn slot_selected_overlay() -> Color32 {
        Color32::from_rgba_unmultiplied(8, 96, 88, 96)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExpandedAudio {
    #[default]
    None,
    A1,
    A2,
    A3,
    A4,
}

impl ExpandedAudio {
    pub fn lane_h(self, lane: Self) -> f32 {
        if self == lane && !matches!(lane, Self::None) {
            css::AUDIO_EXPANDED_H
        } else {
            css::AUDIO_H
        }
    }

    #[allow(dead_code)]
    pub fn a1_h(self) -> f32 {
        self.lane_h(Self::A1)
    }

    #[allow(dead_code)]
    pub fn a2_h(self) -> f32 {
        self.lane_h(Self::A2)
    }

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

/// Per-layer visibility. Owner (outside this component) sets which layers paint.
///
/// `markers` / `marker_slots` are paint toggles that only apply when
/// [`LayerFlags::covers`] is active (owner invokes M logic then).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerFlags {
    pub carrier: bool,
    pub audio_a1: bool,
    pub audio_a2: bool,
    pub audio_a3: bool,
    pub audio_a4: bool,
    pub base_video: bool,
    /// Selected shot/source range — durable DB range, paint-only.
    pub shot_range: bool,
    /// Pokrivalice layer — gate for M pin / slot paint.
    pub covers: bool,
    /// Paint M pins (only if `covers`).
    pub markers: bool,
    /// Paint slot bands (only if `covers`); data from broadcast/owner.
    pub marker_slots: bool,
    /// Draft IN/OUT range for user marking; paint-only.
    pub in_out: bool,
    pub playhead: bool,
}

impl LayerFlags {
    /// Every layer defined and on — including A3/A4.
    pub const ALL: Self = Self {
        carrier: true,
        audio_a1: true,
        audio_a2: true,
        audio_a3: true,
        audio_a4: true,
        base_video: true,
        shot_range: true,
        covers: true,
        markers: true,
        marker_slots: true,
        in_out: true,
        playhead: true,
    };
}

impl Default for LayerFlags {
    /// A1–A4 are defined; A3/A4 stay off until explicitly enabled.
    fn default() -> Self {
        Self {
            audio_a3: false,
            audio_a4: false,
            ..Self::ALL
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineFocusPaint {
    #[default]
    Playhead,
    In,
    Out,
}

#[derive(Debug, Clone, Copy)]
pub struct TimelineVirtualSpan<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_base_video: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TimelineCoverSpan<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub selected: bool,
}

/// Slot band for paint — geometry from broadcast/owner, not derived here.
#[derive(Debug, Clone, Copy)]
pub struct TimelineSlotSpan<'a> {
    pub id: &'a str,
    pub start_frame: i64,
    pub end_frame: i64,
    pub has_cover: bool,
    pub selected: bool,
}

/// M pin for paint — position from owner projection to carrier frame.
#[derive(Debug, Clone, Copy)]
pub struct TimelineMarkerPin<'a> {
    pub id: &'a str,
    pub timeline_frame: i64,
}

/// Full QNC-timeline paint input.
pub struct QncTimeline<'a> {
    pub layers: LayerFlags,
    pub duration_frames: i64,
    pub playhead_frame: i64,
    pub shot_in_frame: i64,
    pub shot_out_frame: i64,
    pub draft_in_frame: i64,
    pub draft_out_frame: i64,
    pub video_background: Option<&'a dyn Fn(&mut egui::Ui, egui::Rect)>,
    pub focus: TimelineFocusPaint,
    pub show_lane_labels: bool,
    pub expanded_audio: ExpandedAudio,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub a3_peaks: &'a [f32],
    pub a4_peaks: &'a [f32],
    pub virtual_spans: &'a [TimelineVirtualSpan<'a>],
    pub covers: &'a [TimelineCoverSpan<'a>],
    /// Ready-made slots from owner (broadcast M→slot); painted only if covers on.
    pub marker_slots: &'a [TimelineSlotSpan<'a>],
    /// Ready-made M pins from owner; painted only if covers on.
    pub markers: &'a [TimelineMarkerPin<'a>],
    pub base_video_blank: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineInteract {
    pub seek_frame: Option<i64>,
    pub row_start_click: bool,
    pub expand_click: Option<ExpandedAudio>,
    pub select_virtual: Option<String>,
    pub select_cover: Option<String>,
    pub select_cover_frame: Option<i64>,
    pub select_marker_slot: Option<String>,
    pub select_marker_slot_frame: Option<i64>,
    pub select_marker: Option<String>,
}

struct AudioRowInteract {
    expand_click: bool,
    row_start_click: bool,
    seek_frame: Option<i64>,
}

impl QncTimeline<'_> {
    pub fn content_height(&self) -> f32 {
        let mut h = 0.0;
        let mut gaps = 0u32;
        let mut push = |row: f32| {
            if gaps > 0 {
                h += css::ROW_GAP;
            }
            h += row;
            gaps += 1;
        };
        // Visual order: A1 → V → A2 → A3 → A4 (A3/A4 only if enabled)
        if self.layers.audio_a1 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A1));
        }
        if self.video_stack_on() {
            push(self.video_stack_h());
        }
        if self.layers.audio_a2 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A2));
        }
        if self.layers.audio_a3 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A3));
        }
        if self.layers.audio_a4 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A4));
        }
        h
    }

    fn video_stack_on(&self) -> bool {
        self.layers.carrier
            || self.layers.base_video
            || self.layers.shot_range
            || self.layers.covers
            || self.layers.in_out
            || self.layers.playhead
    }

    fn video_stack_h(&self) -> f32 {
        css::VIDEO_H
    }

    pub fn show(&self, ui: &mut egui::Ui) -> TimelineInteract {
        let mut out = TimelineInteract::default();
        let width = ui.available_width();
        let total_h = self.content_height();
        let duration_frames = self.duration_frames.max(1);

        egui::Frame::NONE
            .fill(css::BG)
            .stroke(egui::Stroke::new(1.0, css::LINE))
            .show(ui, |ui| {
                ui.set_min_size(Vec2::new(width, total_h));
                ui.set_max_height(total_h);
                ui.spacing_mut().item_spacing = Vec2::new(0.0, css::ROW_GAP);

                // Visual order: A1 → V → A2 → A3 → A4 (A3/A4 only if enabled)
                if self.layers.audio_a1 {
                    merge_audio(
                        &mut out,
                        ExpandedAudio::A1,
                        self.paint_audio_row(
                            ui,
                            ExpandedAudio::A1,
                            "A1",
                            self.a1_peaks,
                            css::WAVE_A1,
                            css::AUDIO_PRIMARY_BG,
                            duration_frames,
                        ),
                    );
                }

                if self.video_stack_on() {
                    merge_interact(&mut out, self.paint_video_stack(ui, duration_frames));
                }

                if self.layers.audio_a2 {
                    merge_audio(
                        &mut out,
                        ExpandedAudio::A2,
                        self.paint_audio_row(
                            ui,
                            ExpandedAudio::A2,
                            "A2",
                            self.a2_peaks,
                            css::WAVE_A2,
                            css::AUDIO_SECONDARY_BG,
                            duration_frames,
                        ),
                    );
                }

                if self.layers.audio_a3 {
                    merge_audio(
                        &mut out,
                        ExpandedAudio::A3,
                        self.paint_audio_row(
                            ui,
                            ExpandedAudio::A3,
                            "A3",
                            self.a3_peaks,
                            css::WAVE_A3,
                            css::AUDIO_SECONDARY_BG,
                            duration_frames,
                        ),
                    );
                }

                if self.layers.audio_a4 {
                    merge_audio(
                        &mut out,
                        ExpandedAudio::A4,
                        self.paint_audio_row(
                            ui,
                            ExpandedAudio::A4,
                            "A4",
                            self.a4_peaks,
                            css::WAVE_A4,
                            css::AUDIO_SECONDARY_BG,
                            duration_frames,
                        ),
                    );
                }
            });

        out
    }

    fn track_inner(row: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(
            egui::pos2(row.left() + css::LABEL_COL_W, row.top()),
            row.max,
        )
    }

    fn paint_label(ui: &mut egui::Ui, row: egui::Rect, label: &str, expanded: bool) {
        let label_rect = Self::label_rect(row);
        let painter = ui.painter();
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
            if expanded { css::FOCUS } else { css::MUTED },
        );
    }

    fn label_rect(row: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_size(row.min, Vec2::new(css::LABEL_COL_W, row.height()))
    }

    fn paint_audio_row(
        &self,
        ui: &mut egui::Ui,
        lane: ExpandedAudio,
        label: &str,
        peaks: &[f32],
        wave_color: Color32,
        bg: Color32,
        duration_frames: i64,
    ) -> AudioRowInteract {
        let height = self.expanded_audio.lane_h(lane);
        let width = ui.available_width();
        let (row, response) =
            ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click_and_drag());
        let label_rect = Self::label_rect(row);
        let label_resp = ui.interact(
            label_rect,
            response.id.with(("audio_label", label)),
            egui::Sense::click(),
        );
        if label_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let paint_label = self.show_lane_labels.then_some(label).unwrap_or("");
        Self::paint_label(ui, row, paint_label, self.expanded_audio == lane);
        let inner = Self::track_inner(row);
        let painter = ui.painter();
        painter.rect_filled(inner, 0.0, bg);
        painter.rect_stroke(
            inner,
            0.0,
            egui::Stroke::new(1.0, css::LINE),
            egui::StrokeKind::Inside,
        );
        paint_peaks(painter, inner, peaks, wave_color);
        if self.layers.shot_range {
            paint_shot_range(
                painter,
                inner,
                duration_frames,
                self.shot_in_frame,
                self.shot_out_frame,
            );
        }
        if self.layers.in_out {
            paint_io_dim(
                painter,
                inner,
                duration_frames,
                self.draft_in_frame,
                self.draft_out_frame,
            );
            paint_io_handles(
                painter,
                inner,
                duration_frames,
                self.draft_in_frame,
                self.draft_out_frame,
                self.focus,
            );
        }
        if self.layers.playhead {
            paint_playhead(
                painter,
                inner,
                duration_frames,
                self.playhead_frame,
                self.focus,
            );
        }
        if self.show_lane_labels && label_resp.clicked() {
            return AudioRowInteract {
                expand_click: true,
                row_start_click: true,
                seek_frame: None,
            };
        }
        AudioRowInteract {
            expand_click: false,
            row_start_click: false,
            seek_frame: seek_from_pointer(&response, inner, duration_frames),
        }
    }

    /// V: video/control row. Filmstrip thumbnails are a separate UI background component.
    fn paint_video_stack(&self, ui: &mut egui::Ui, duration_frames: i64) -> TimelineInteract {
        let width = ui.available_width();
        let (row, response) = ui.allocate_exact_size(
            Vec2::new(width, self.video_stack_h()),
            egui::Sense::click_and_drag(),
        );
        let label_rect = Self::label_rect(row);
        let label_resp = ui.interact(
            label_rect,
            response.id.with("video_label"),
            egui::Sense::click(),
        );
        if label_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        Self::paint_label(
            ui,
            row,
            self.show_lane_labels.then_some("V").unwrap_or(""),
            false,
        );
        let inner = Self::track_inner(row);
        let track_rect = inner;
        // M pins/slots are cover-layer companions: paint only when covers is active.
        let covers_active = self.layers.covers;
        // `layers.carrier` = logical timecode clock only; never a visible yellow wash.

        // Pass 1 — underlay (slots only; images come next so playhead can sit above).
        {
            let painter = ui.painter();
            painter.rect_filled(inner, 0.0, css::VIDEO_BG);
            painter.rect_stroke(
                inner,
                0.0,
                egui::Stroke::new(1.0, css::LINE),
                egui::StrokeKind::Inside,
            );
            if let Some(paint_background) = self.video_background {
                paint_background(ui, track_rect);
            }
        }

        // Pass 2 — covers + chrome above thumbs (playhead last = never under film).
        {
            let painter = ui.painter();

            if covers_active && self.layers.marker_slots {
                paint_marker_slots(painter, track_rect, duration_frames, self.marker_slots);
            }

            if self.layers.base_video {
                if self.virtual_spans.is_empty() {
                    if !self.base_video_blank {
                        painter.rect_filled(
                            track_rect.shrink2(Vec2::new(0.0, 10.0)),
                            2.0,
                            css::WAVE_A1.linear_multiply(0.25),
                        );
                    }
                } else {
                    paint_virtual_spans(painter, track_rect, duration_frames, self.virtual_spans);
                }
            }

            if self.layers.shot_range {
                paint_shot_range(
                    painter,
                    inner,
                    duration_frames,
                    self.shot_in_frame,
                    self.shot_out_frame,
                );
            }

            if covers_active {
                paint_covers(painter, track_rect, duration_frames, self.covers);
                if self.layers.marker_slots {
                    paint_selected_marker_slots(
                        painter,
                        track_rect,
                        duration_frames,
                        self.marker_slots,
                    );
                }
                if self.layers.markers {
                    paint_markers(painter, track_rect, duration_frames, self.markers);
                }
            }
            if self.layers.in_out {
                paint_io_dim(
                    painter,
                    inner,
                    duration_frames,
                    self.draft_in_frame,
                    self.draft_out_frame,
                );
                paint_io_handles(
                    painter,
                    inner,
                    duration_frames,
                    self.draft_in_frame,
                    self.draft_out_frame,
                    self.focus,
                );
            }
            if self.layers.playhead {
                paint_playhead(
                    painter,
                    inner,
                    duration_frames,
                    self.playhead_frame,
                    self.focus,
                );
            }
        }

        if self.show_lane_labels && label_resp.clicked() {
            return TimelineInteract {
                row_start_click: true,
                ..TimelineInteract::default()
            };
        }

        pointer_interact(
            &response,
            inner,
            duration_frames,
            self.virtual_spans,
            self.covers,
            self.marker_slots,
            self.markers,
        )
    }
}

fn merge_interact(out: &mut TimelineInteract, update: TimelineInteract) {
    out.row_start_click |= update.row_start_click;
    if out.expand_click.is_none() {
        out.expand_click = update.expand_click;
    }
    if out.select_marker.is_none() {
        out.select_marker = update.select_marker;
    }
    if out.select_cover.is_none() {
        out.select_cover = update.select_cover;
        out.select_cover_frame = update.select_cover_frame;
    }
    if out.select_marker_slot.is_none() {
        out.select_marker_slot = update.select_marker_slot;
        out.select_marker_slot_frame = update.select_marker_slot_frame;
    }
    if out.select_virtual.is_none() {
        out.select_virtual = update.select_virtual;
    }
    if out.seek_frame.is_none() {
        out.seek_frame = update.seek_frame;
    }
}

fn merge_audio(out: &mut TimelineInteract, lane: ExpandedAudio, a: AudioRowInteract) {
    out.row_start_click |= a.row_start_click;
    if a.expand_click {
        out.expand_click = Some(lane);
    } else if out.seek_frame.is_none() {
        if let Some(frame) = a.seek_frame {
            out.seek_frame = Some(frame);
        }
    }
}

fn x_for_frame(area: egui::Rect, duration_frames: i64, frame: i64) -> f32 {
    let duration_frames = duration_frames.max(1);
    let t = (frame.clamp(0, duration_frames) as f32) / (duration_frames as f32);
    area.left() + t * area.width()
}

fn paint_covers(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    covers: &[TimelineCoverSpan<'_>],
) {
    for cover in covers {
        if cover.end_frame <= cover.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, cover.start_frame);
        let x1 = x_for_frame(area, duration_frames, cover.end_frame);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, area.top() + 2.0),
                egui::pos2(x1.max(x0 + 3.0), area.top() + area.height() * 0.42),
            ),
            2.0,
            if cover.selected {
                Color32::from_rgb(250, 204, 21)
            } else {
                Color32::from_rgb(234, 179, 8).linear_multiply(0.75)
            },
        );
    }
}

fn paint_virtual_spans(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    spans: &[TimelineVirtualSpan<'_>],
) {
    for span in spans {
        if span.end_frame <= span.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, span.start_frame);
        let x1 = x_for_frame(area, duration_frames, span.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 17.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 10.0),
        );
        let base = if span.has_base_video {
            css::WAVE_A1
        } else {
            css::WAVE_A2
        };
        painter.rect_filled(rect, 2.0, base.linear_multiply(0.24));
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(
                if span.selected { 1.8 } else { 1.0 },
                if span.selected { css::FOCUS } else { base },
            ),
            egui::StrokeKind::Inside,
        );
        if rect.width() >= 42.0 {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                span.label,
                egui::FontId::proportional(10.0),
                Color32::from_rgb(229, 231, 235),
            );
        }
    }
}

fn paint_marker_slots(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    slots: &[TimelineSlotSpan<'_>],
) {
    for slot in slots {
        if slot.end_frame <= slot.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, slot.start_frame);
        let x1 = x_for_frame(area, duration_frames, slot.end_frame);
        let slot_rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        );
        let color = if slot.selected {
            css::SLOT_SELECTED
        } else if slot.has_cover {
            Color32::from_rgb(250, 204, 21)
        } else {
            Color32::from_rgb(45, 212, 191)
        };
        painter.rect_filled(slot_rect, 1.0, color.linear_multiply(0.55));
        painter.rect_stroke(
            slot_rect,
            1.0,
            egui::Stroke::new(if slot.selected { 2.0 } else { 1.0 }, color),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_selected_marker_slots(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    slots: &[TimelineSlotSpan<'_>],
) {
    for slot in slots.iter().filter(|slot| slot.selected) {
        if slot.end_frame <= slot.start_frame {
            continue;
        }
        let x0 = x_for_frame(area, duration_frames, slot.start_frame);
        let x1 = x_for_frame(area, duration_frames, slot.end_frame);
        let slot_rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        );
        painter.rect_filled(slot_rect, 1.0, css::slot_selected_overlay());
        painter.rect_stroke(
            slot_rect,
            1.0,
            egui::Stroke::new(2.0, css::SLOT_SELECTED),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_markers(
    painter: &egui::Painter,
    area: egui::Rect,
    duration_frames: i64,
    markers: &[TimelineMarkerPin<'_>],
) {
    let pink = Color32::from_rgb(244, 114, 182);
    for m in markers {
        let x = x_for_frame(area, duration_frames, m.timeline_frame);
        painter.line_segment(
            [egui::pos2(x, area.top()), egui::pos2(x, area.bottom())],
            egui::Stroke::new(1.0, pink),
        );
        painter.text(
            egui::pos2(x + 3.0, area.top() + 2.0),
            egui::Align2::LEFT_TOP,
            "M",
            egui::FontId::proportional(9.0),
            pink,
        );
    }
}

fn pointer_interact(
    response: &egui::Response,
    inner: egui::Rect,
    duration_frames: i64,
    virtual_spans: &[TimelineVirtualSpan<'_>],
    covers: &[TimelineCoverSpan<'_>],
    slots: &[TimelineSlotSpan<'_>],
    markers: &[TimelineMarkerPin<'_>],
) -> TimelineInteract {
    let mut out = TimelineInteract::default();
    let Some(pos) = response.interact_pointer_pos() else {
        return out;
    };
    if !inner.expand(2.0).contains(pos) || inner.width() <= 0.0 {
        return out;
    }

    if response.clicked() {
        let clicked_frame = frame_from_pos(inner, duration_frames, pos);
        if let Some(id) = marker_hit(inner, duration_frames, pos, markers) {
            out.select_marker = Some(id.to_string());
            out.seek_frame = clicked_frame;
            return out;
        }
        if let Some(id) = cover_hit(inner, duration_frames, pos, covers) {
            out.select_cover = Some(id.to_string());
            out.select_cover_frame = clicked_frame;
            return out;
        }
        if let Some(id) = marker_slot_hit(inner, duration_frames, pos, slots) {
            out.select_marker_slot = Some(id.to_string());
            out.select_marker_slot_frame = clicked_frame;
            return out;
        }
        if let Some(id) = virtual_span_hit(inner, duration_frames, pos, virtual_spans) {
            out.select_virtual = Some(id.to_string());
            out.seek_frame = clicked_frame;
            return out;
        }
    }

    if response.clicked() || response.dragged() {
        out.seek_frame = frame_from_pos(inner, duration_frames, pos);
    }
    out
}

fn marker_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    markers: &'a [TimelineMarkerPin<'a>],
) -> Option<&'a str> {
    let tolerance = 5.0;
    markers.iter().find_map(|marker| {
        let x = x_for_frame(area, duration_frames, marker.timeline_frame);
        (!marker.id.trim().is_empty() && (pos.x - x).abs() <= tolerance).then_some(marker.id)
    })
}

fn cover_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    covers: &'a [TimelineCoverSpan<'a>],
) -> Option<&'a str> {
    covers.iter().rev().find_map(|cover| {
        if cover.end_frame <= cover.start_frame || cover.id.trim().is_empty() {
            return None;
        }
        let x0 = x_for_frame(area, duration_frames, cover.start_frame);
        let x1 = x_for_frame(area, duration_frames, cover.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.top() + area.height() * 0.42),
        )
        .expand(2.0);
        rect.contains(pos).then_some(cover.id)
    })
}

fn marker_slot_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    slots: &'a [TimelineSlotSpan<'a>],
) -> Option<&'a str> {
    slots.iter().find_map(|slot| {
        if slot.end_frame <= slot.start_frame || slot.id.trim().is_empty() {
            return None;
        }
        let x0 = x_for_frame(area, duration_frames, slot.start_frame);
        let x1 = x_for_frame(area, duration_frames, slot.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        )
        .expand(2.0);
        rect.contains(pos).then_some(slot.id)
    })
}

fn virtual_span_hit<'a>(
    area: egui::Rect,
    duration_frames: i64,
    pos: egui::Pos2,
    spans: &'a [TimelineVirtualSpan<'a>],
) -> Option<&'a str> {
    spans.iter().rev().find_map(|span| {
        if span.end_frame <= span.start_frame || span.id.trim().is_empty() {
            return None;
        }
        let x0 = x_for_frame(area, duration_frames, span.start_frame);
        let x1 = x_for_frame(area, duration_frames, span.end_frame);
        let rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 17.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 10.0),
        )
        .expand(2.0);
        rect.contains(pos).then_some(span.id)
    })
}

fn paint_shot_range(
    painter: &egui::Painter,
    inner: egui::Rect,
    duration_frames: i64,
    shot_in_frame: i64,
    shot_out_frame: i64,
) {
    if shot_out_frame <= shot_in_frame {
        return;
    }
    let in_x = x_for_frame(inner, duration_frames, shot_in_frame);
    let out_x = x_for_frame(inner, duration_frames, shot_out_frame);
    let range = egui::Rect::from_min_max(
        egui::pos2(in_x, inner.top() + 1.0),
        egui::pos2(out_x.max(in_x + 3.0), inner.bottom() - 1.0),
    );
    let color = Color32::from_rgb(96, 165, 250);
    painter.rect_stroke(
        range,
        1.0,
        egui::Stroke::new(1.5, color.linear_multiply(0.85)),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [
            egui::pos2(in_x, inner.top() + 1.0),
            egui::pos2(in_x, inner.bottom() - 1.0),
        ],
        egui::Stroke::new(2.0, color),
    );
    painter.line_segment(
        [
            egui::pos2(out_x, inner.top() + 1.0),
            egui::pos2(out_x, inner.bottom() - 1.0),
        ],
        egui::Stroke::new(2.0, color),
    );
}

fn paint_io_dim(
    painter: &egui::Painter,
    inner: egui::Rect,
    duration_frames: i64,
    draft_in_frame: i64,
    draft_out_frame: i64,
) {
    let in_x = x_for_frame(inner, duration_frames, draft_in_frame);
    let out_x = x_for_frame(inner, duration_frames, draft_out_frame);
    if in_x > inner.left() + 0.5 {
        painter.rect_filled(
            egui::Rect::from_min_max(inner.min, egui::pos2(in_x, inner.bottom())),
            0.0,
            css::io_dim(),
        );
    }
    if out_x < inner.right() - 0.5 {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(out_x, inner.top()), inner.max),
            0.0,
            css::io_dim(),
        );
    }
}

fn paint_io_handles(
    painter: &egui::Painter,
    inner: egui::Rect,
    duration_frames: i64,
    draft_in_frame: i64,
    draft_out_frame: i64,
    focus: TimelineFocusPaint,
) {
    let in_x = x_for_frame(inner, duration_frames, draft_in_frame);
    let out_x = x_for_frame(inner, duration_frames, draft_out_frame);
    let in_stroke = if focus == TimelineFocusPaint::In {
        egui::Stroke::new(3.0, css::FOCUS)
    } else {
        egui::Stroke::new(2.0, css::IO_HANDLE)
    };
    let out_stroke = if focus == TimelineFocusPaint::Out {
        egui::Stroke::new(3.0, css::FOCUS)
    } else {
        egui::Stroke::new(2.0, css::IO_HANDLE)
    };
    painter.line_segment(
        [
            egui::pos2(in_x, inner.top()),
            egui::pos2(in_x, inner.bottom()),
        ],
        in_stroke,
    );
    painter.line_segment(
        [
            egui::pos2(out_x, inner.top()),
            egui::pos2(out_x, inner.bottom()),
        ],
        out_stroke,
    );
}

fn paint_playhead(
    painter: &egui::Painter,
    inner: egui::Rect,
    duration_frames: i64,
    playhead_frame: i64,
    focus: TimelineFocusPaint,
) {
    let x = x_for_frame(inner, duration_frames, playhead_frame);
    let stroke = if focus == TimelineFocusPaint::Playhead {
        egui::Stroke::new(2.5, css::FOCUS)
    } else {
        egui::Stroke::new(1.5, css::PLAYHEAD)
    };
    painter.line_segment(
        [egui::pos2(x, inner.top()), egui::pos2(x, inner.bottom())],
        stroke,
    );
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
    if inner.width() <= 0.0 {
        return None;
    }
    frame_from_pos(inner, duration_frames, pos)
}

fn frame_from_pos(inner: egui::Rect, duration_frames: i64, pos: egui::Pos2) -> Option<i64> {
    if inner.width() <= 0.0 {
        return None;
    }
    let t = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
    let duration_frames = duration_frames.max(1);
    Some(((t * duration_frames as f64).round() as i64).clamp(0, duration_frames))
}

fn paint_peaks(painter: &egui::Painter, rect: egui::Rect, peaks: &[f32], color: Color32) {
    if peaks.is_empty() || rect.width() < 2.0 {
        return;
    }
    let mid = rect.center().y;
    let half = rect.height() * 0.48;
    let n = peaks.len();
    let bars = (rect.width().floor() as usize).max(1);
    for i in 0..bars {
        let start = i * n / bars;
        let end = ((i + 1) * n / bars).max(start + 1).min(n);
        let mut max_p = 0.0f32;
        for p in &peaks[start..end] {
            max_p = max_p.max(p.abs());
        }
        let x = rect.left() + i as f32 + 0.5;
        let amp = max_p.clamp(0.0, 1.0) * half;
        painter.line_segment(
            [egui::pos2(x, mid - amp), egui::pos2(x, mid + amp)],
            egui::Stroke::new(1.0, color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_defines_a3_a4_but_leaves_them_off() {
        let d = LayerFlags::default();
        assert!(d.audio_a1);
        assert!(d.audio_a2);
        assert!(d.shot_range);
        assert!(d.in_out);
        assert!(!d.audio_a3);
        assert!(!d.audio_a4);
        assert!(LayerFlags::ALL.audio_a3);
        assert!(LayerFlags::ALL.audio_a4);
    }

    #[test]
    fn virtual_span_hit_uses_shared_frame_geometry() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(100.0, 60.0));
        let spans = [
            TimelineVirtualSpan {
                id: "virtual_a",
                label: "tonovi",
                start_frame: 10,
                end_frame: 40,
                has_base_video: true,
                selected: false,
            },
            TimelineVirtualSpan {
                id: "virtual_b",
                label: "offovi",
                start_frame: 20,
                end_frame: 50,
                has_base_video: true,
                selected: true,
            },
        ];

        assert_eq!(
            virtual_span_hit(area, 100, egui::pos2(30.0, 30.0), &spans),
            Some("virtual_b")
        );
        assert_eq!(frame_from_pos(area, 100, egui::pos2(30.0, 30.0)), Some(30));
    }

    #[test]
    fn marker_slot_hit_uses_full_timeline_row_height() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(100.0, 60.0));
        let slots = [TimelineSlotSpan {
            id: "slot_mid",
            start_frame: 10,
            end_frame: 40,
            has_cover: false,
            selected: false,
        }];

        assert_eq!(
            marker_slot_hit(area, 100, egui::pos2(25.0, 6.0), &slots),
            Some("slot_mid")
        );
        assert_eq!(
            marker_slot_hit(area, 100, egui::pos2(25.0, 54.0), &slots),
            Some("slot_mid")
        );
    }
}
