//! QNC-timeline — one native timeline component, symbolic layer paint.
//!
//! Layers (docs/qnc-timeline.md):
//!   carrier = logical clock, not painted
//!   → audio A1..A4 → base video → pokrivalice → IN/OUT + playhead
//!
//! Visibility is per-layer via [`LayerFlags`]. Named presets come later.
//! Editorial segments/parts are owner data — not part of this component.
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
    pub const FOCUS: Color32 = Color32::from_rgb(255, 180, 60);
    pub const PLAYHEAD: Color32 = Color32::from_rgb(0x4e, 0xc9, 0xb0);
    pub const MUTED: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
    pub const IO_HANDLE: Color32 = Color32::WHITE;
    pub fn io_dim() -> Color32 {
        Color32::from_black_alpha(133)
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

    pub fn a1_h(self) -> f32 {
        self.lane_h(Self::A1)
    }

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

/// Per-layer visibility. Owners compose presets from these later.
///
/// `markers` / `marker_slots` are paint toggles that only apply when
/// [`LayerFlags::covers`] is active (owner invokes broadcast M logic then).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerFlags {
    pub carrier: bool,
    pub audio_a1: bool,
    pub audio_a2: bool,
    pub audio_a3: bool,
    pub audio_a4: bool,
    pub base_video: bool,
    /// Pokrivalice layer — gate for M pin / slot paint.
    pub covers: bool,
    /// Paint M pins (only if `covers`).
    pub markers: bool,
    /// Paint slot bands (only if `covers`); data from broadcast/owner.
    pub marker_slots: bool,
    pub in_out: bool,
    pub playhead: bool,
}

impl LayerFlags {
    /// Complete QNC-timeline — every layer available.
    pub const ALL: Self = Self {
        carrier: true,
        audio_a1: true,
        audio_a2: true,
        audio_a3: true,
        audio_a4: true,
        base_video: true,
        covers: true,
        markers: true,
        marker_slots: true,
        in_out: true,
        playhead: true,
    };
}

impl Default for LayerFlags {
    fn default() -> Self {
        Self::ALL
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
pub struct TimelineCoverSpan<'a> {
    pub id: &'a str,
    pub start_sec: f64,
    pub end_sec: f64,
    pub selected: bool,
}

/// Slot band for paint — geometry from broadcast/owner, not derived here.
#[derive(Debug, Clone, Copy)]
pub struct TimelineSlotSpan<'a> {
    pub id: &'a str,
    pub start_sec: f64,
    pub end_sec: f64,
    pub has_cover: bool,
    pub selected: bool,
}

/// M pin for paint — position from owner projection to seconds.
#[derive(Debug, Clone, Copy)]
pub struct TimelineMarkerPin {
    pub timeline_sec: f64,
}

/// Full QNC-timeline paint input.
pub struct QncTimeline<'a> {
    pub layers: LayerFlags,
    pub duration_sec: f64,
    pub playhead_sec: f64,
    pub source_in: f64,
    pub source_out: f64,
    pub video_background: Option<&'a dyn Fn(&mut egui::Ui, egui::Rect)>,
    pub focus: TimelineFocusPaint,
    pub expanded_audio: ExpandedAudio,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub a3_peaks: &'a [f32],
    pub a4_peaks: &'a [f32],
    pub covers: &'a [TimelineCoverSpan<'a>],
    /// Ready-made slots from owner (broadcast M→slot); painted only if covers on.
    pub marker_slots: &'a [TimelineSlotSpan<'a>],
    /// Ready-made M pins from owner; painted only if covers on.
    pub markers: &'a [TimelineMarkerPin],
    pub base_video_blank: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineInteract {
    pub seek_sec: Option<f64>,
    pub expand_click: Option<ExpandedAudio>,
}

struct AudioRowInteract {
    expand_click: bool,
    seek_sec: Option<f64>,
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
        // Visual order: A1 → V → A2
        if self.layers.audio_a1 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A1));
        }
        if self.video_stack_on() {
            push(self.video_stack_h());
        }
        if self.layers.audio_a2 {
            push(self.expanded_audio.lane_h(ExpandedAudio::A2));
        }
        h
    }

    fn video_stack_on(&self) -> bool {
        self.layers.carrier
            || self.layers.base_video
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
        let dur = self.duration_sec.max(0.04);

        egui::Frame::NONE
            .fill(css::BG)
            .stroke(egui::Stroke::new(1.0, css::LINE))
            .show(ui, |ui| {
                ui.set_min_size(Vec2::new(width, total_h));
                ui.set_max_height(total_h);
                ui.spacing_mut().item_spacing = Vec2::new(0.0, css::ROW_GAP);

                // Visual order: A1 → V → A2
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
                            dur,
                        ),
                    );
                }

                if self.video_stack_on() {
                    if let Some(sec) = self.paint_video_stack(ui, dur) {
                        if out.seek_sec.is_none() {
                            out.seek_sec = Some(sec);
                        }
                    }
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
                            dur,
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
        let label_rect =
            egui::Rect::from_min_size(row.min, Vec2::new(css::LABEL_COL_W, row.height()));
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

    fn paint_audio_row(
        &self,
        ui: &mut egui::Ui,
        lane: ExpandedAudio,
        label: &str,
        peaks: &[f32],
        wave_color: Color32,
        bg: Color32,
        dur: f64,
    ) -> AudioRowInteract {
        let height = self.expanded_audio.lane_h(lane);
        let width = ui.available_width();
        let (row, response) =
            ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click_and_drag());
        let label_rect =
            egui::Rect::from_min_size(row.min, Vec2::new(css::LABEL_COL_W, row.height()));
        let label_resp = ui.interact(
            label_rect,
            ui.id().with(("audio_label", label)),
            egui::Sense::click(),
        );
        if label_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        Self::paint_label(ui, row, label, self.expanded_audio == lane);
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
        if self.layers.in_out {
            paint_io_dim(painter, inner, dur, self.source_in, self.source_out);
            paint_io_handles(
                painter,
                inner,
                dur,
                self.source_in,
                self.source_out,
                self.focus,
            );
        }
        if self.layers.playhead {
            paint_playhead(painter, inner, dur, self.playhead_sec, self.focus);
        }
        if label_resp.clicked() {
            return AudioRowInteract {
                expand_click: true,
                seek_sec: None,
            };
        }
        AudioRowInteract {
            expand_click: false,
            seek_sec: seek_from_pointer(&response, inner, dur),
        }
    }

    /// V: video/control row. Filmstrip thumbnails are a separate UI background component.
    fn paint_video_stack(&self, ui: &mut egui::Ui, dur: f64) -> Option<f64> {
        let width = ui.available_width();
        let (row, response) = ui.allocate_exact_size(
            Vec2::new(width, self.video_stack_h()),
            egui::Sense::click_and_drag(),
        );
        Self::paint_label(ui, row, "V", false);
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
                paint_marker_slots(painter, track_rect, dur, self.marker_slots);
            }

            if self.layers.base_video && !self.base_video_blank {
                painter.rect_filled(
                    track_rect.shrink2(Vec2::new(0.0, 10.0)),
                    2.0,
                    css::WAVE_A1.linear_multiply(0.25),
                );
            }

            if covers_active {
                paint_covers(painter, track_rect, dur, self.covers);
                if self.layers.markers {
                    paint_markers(painter, track_rect, dur, self.markers);
                }
            }
            if self.layers.in_out {
                paint_io_dim(painter, inner, dur, self.source_in, self.source_out);
                paint_io_handles(
                    painter,
                    inner,
                    dur,
                    self.source_in,
                    self.source_out,
                    self.focus,
                );
            }
            if self.layers.playhead {
                paint_playhead(painter, inner, dur, self.playhead_sec, self.focus);
            }
        }

        seek_from_pointer(&response, inner, dur)
    }
}

fn merge_audio(out: &mut TimelineInteract, lane: ExpandedAudio, a: AudioRowInteract) {
    if a.expand_click {
        out.expand_click = Some(lane);
    } else if out.seek_sec.is_none() {
        if let Some(sec) = a.seek_sec {
            out.seek_sec = Some(sec);
        }
    }
}

fn paint_covers(
    painter: &egui::Painter,
    area: egui::Rect,
    dur: f64,
    covers: &[TimelineCoverSpan<'_>],
) {
    let x_at = |sec: f64| area.left() + (sec / dur).clamp(0.0, 1.0) as f32 * area.width();
    for cover in covers {
        if cover.end_sec <= cover.start_sec {
            continue;
        }
        let x0 = x_at(cover.start_sec);
        let x1 = x_at(cover.end_sec);
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

fn paint_marker_slots(
    painter: &egui::Painter,
    area: egui::Rect,
    dur: f64,
    slots: &[TimelineSlotSpan<'_>],
) {
    let x_at = |sec: f64| area.left() + (sec / dur).clamp(0.0, 1.0) as f32 * area.width();
    for slot in slots {
        if slot.end_sec <= slot.start_sec {
            continue;
        }
        let x0 = x_at(slot.start_sec);
        let x1 = x_at(slot.end_sec);
        let slot_rect = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 2.0),
            egui::pos2(x1.max(x0 + 3.0), area.bottom() - 2.0),
        );
        let color = if slot.has_cover {
            Color32::from_rgb(250, 204, 21)
        } else if slot.selected {
            css::PLAYHEAD
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

fn paint_markers(
    painter: &egui::Painter,
    area: egui::Rect,
    dur: f64,
    markers: &[TimelineMarkerPin],
) {
    let x_at = |sec: f64| area.left() + (sec / dur).clamp(0.0, 1.0) as f32 * area.width();
    let pink = Color32::from_rgb(244, 114, 182);
    for m in markers {
        let x = x_at(m.timeline_sec);
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

fn paint_io_dim(
    painter: &egui::Painter,
    inner: egui::Rect,
    dur: f64,
    source_in: f64,
    source_out: f64,
) {
    let in_x = inner.left() + (source_in / dur).clamp(0.0, 1.0) as f32 * inner.width();
    let out_x = inner.left() + (source_out / dur).clamp(0.0, 1.0) as f32 * inner.width();
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
    dur: f64,
    source_in: f64,
    source_out: f64,
    focus: TimelineFocusPaint,
) {
    let in_x = inner.left() + (source_in / dur).clamp(0.0, 1.0) as f32 * inner.width();
    let out_x = inner.left() + (source_out / dur).clamp(0.0, 1.0) as f32 * inner.width();
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
    dur: f64,
    playhead_sec: f64,
    focus: TimelineFocusPaint,
) {
    let x = inner.left() + (playhead_sec / dur).clamp(0.0, 1.0) as f32 * inner.width();
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

fn seek_from_pointer(response: &egui::Response, inner: egui::Rect, duration: f64) -> Option<f64> {
    if !(response.clicked() || response.dragged()) {
        return None;
    }
    let pos = response.interact_pointer_pos()?;
    if !inner.expand(2.0).contains(pos) {
        return None;
    }
    let t = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
    Some(t * duration.max(0.04))
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
