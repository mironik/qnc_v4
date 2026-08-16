//! Native QNC-timeline paint — one universal path; applications via schema + snapshot.
//! Source: IO pins only. Wrap: IO + M markers + yellow covers.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::api::{SegmentSchema, TimelineApplication, TimelineModel, TimelinePin};
use crate::focus::{frame_to_seconds, timeline_pin_frame, timeline_span_frames, FocusTarget};

const ROW_H: f32 = 28.0;
const LABEL_W: f32 = 44.0;
const PAD: f32 = 6.0;

fn color_a1() -> Color32 {
    Color32::from_rgb(32, 92, 110)
}
fn color_a2() -> Color32 {
    Color32::from_rgb(28, 78, 96)
}
fn color_v_tone() -> Color32 {
    Color32::from_rgb(120, 36, 78) // magenta emulsion
}
fn color_v_off() -> Color32 {
    Color32::from_rgb(40, 40, 44)
}
fn color_v_source() -> Color32 {
    Color32::from_rgb(96, 40, 72)
}
fn color_cover() -> Color32 {
    Color32::from_rgb(196, 168, 48) // yellow emulsion
}
fn color_io_pin() -> Color32 {
    Color32::from_rgb(220, 220, 230)
}
fn color_m_pin() -> Color32 {
    Color32::from_rgb(120, 200, 255) // cyan timebase
}
fn color_playhead() -> Color32 {
    Color32::from_rgb(240, 240, 240)
}
fn color_grid() -> Color32 {
    Color32::from_rgb(55, 55, 58)
}

fn color_focus() -> Color32 {
    Color32::from_rgb(255, 180, 60)
}

/// Draw Kodak A1/V/A2 timeline. Returns seek frame if user clicked the ruler.
pub fn paint_timeline(
    ui: &mut Ui,
    model: &TimelineModel,
    virtual_frame: i64,
    focus: Option<&FocusTarget>,
) -> Option<i64> {
    let fps = if model.timeline_fps.is_finite() && model.timeline_fps > 1.0 {
        model.timeline_fps
    } else {
        25.0
    };
    let duration_frames = if model.duration_frames > 0 {
        model.duration_frames
    } else {
        ((model.duration_sec.max(0.0) * fps).round() as i64).max(1)
    }
    .max(1);
    let duration_sec = frame_to_seconds(duration_frames, fps);
    let available = ui.available_width();
    let height = ROW_H * 3.0 + PAD * 4.0 + 18.0;
    let (response, painter) = ui.allocate_painter(Vec2::new(available, height), Sense::click());
    let rect = response.rect;
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + LABEL_W, rect.top() + PAD),
        Pos2::new(rect.right() - PAD, rect.bottom() - PAD - 14.0),
    );
    let track_w = track.width().max(1.0);

    painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 22, 24));

    let rows = [
        ("A1", color_a1()),
        ("V", color_v_tone()),
        ("A2", color_a2()),
    ];
    for (i, (label, base)) in rows.iter().enumerate() {
        let y0 = track.top() + i as f32 * (ROW_H + PAD);
        let row = Rect::from_min_size(Pos2::new(track.left(), y0), Vec2::new(track_w, ROW_H));
        let label_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, y0),
            Vec2::new(LABEL_W - 8.0, ROW_H),
        );
        painter.text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::FontId::proportional(13.0),
            Color32::from_rgb(200, 200, 200),
        );
        painter.rect_filled(row, 2.0, color_grid());

        for seg in &model.segments {
            let (seg_start, seg_end) = timeline_span_frames(
                seg.global_start_frame,
                seg.global_end_frame,
                seg.global_start_sec,
                seg.global_end_sec,
                fps,
            );
            let x0 = track.left() + (seg_start as f32 / duration_frames as f32) * track_w;
            let x1 = track.left() + (seg_end as f32 / duration_frames as f32) * track_w;
            let seg_rect = Rect::from_min_max(
                Pos2::new(x0, row.top() + 2.0),
                Pos2::new(x1.max(x0 + 2.0), row.bottom() - 2.0),
            );
            let fill = match (i, seg.schema) {
                (0, _) | (2, _) => *base,
                (1, SegmentSchema::Off) => color_v_off(),
                (1, SegmentSchema::Source) => color_v_source(),
                (1, _) => color_v_tone(),
                _ => *base,
            };
            painter.rect_filled(seg_rect, 2.0, fill);

            // Yellow covers — wrap only (schema allows; source snapshot keeps covers empty).
            if i == 1 && seg.schema != SegmentSchema::Source {
                for cover in &seg.covers {
                    let (cover_start, cover_end) = timeline_span_frames(
                        cover.timeline_start_frame,
                        cover.timeline_end_frame,
                        cover.timeline_start_sec,
                        cover.timeline_end_sec,
                        fps,
                    );
                    let cx0 =
                        track.left() + (cover_start as f32 / duration_frames as f32) * track_w;
                    let cx1 = track.left() + (cover_end as f32 / duration_frames as f32) * track_w;
                    let cover_rect = Rect::from_min_max(
                        Pos2::new(cx0, row.top() + 6.0),
                        Pos2::new(cx1.max(cx0 + 2.0), row.bottom() - 6.0),
                    );
                    painter.rect_filled(cover_rect, 1.0, color_cover());
                }
            }
        }
    }

    // Marker slots (M–M) — wrap only.
    if model.application != TimelineApplication::Source {
        let v_y0 = track.top() + 1.0 * (ROW_H + PAD);
        for slot in &model.marker_slots {
            let (slot_start, slot_end) = timeline_span_frames(
                slot.start_frame,
                slot.end_frame,
                slot.start_sec,
                slot.end_sec,
                fps,
            );
            let sx0 = track.left() + (slot_start as f32 / duration_frames as f32) * track_w;
            let sx1 = track.left() + (slot_end as f32 / duration_frames as f32) * track_w;
            let focused = matches!(focus, Some(FocusTarget::Slot { id }) if id == &slot.slot_id);
            painter.rect_stroke(
                Rect::from_min_max(
                    Pos2::new(sx0, v_y0 + 2.0),
                    Pos2::new(sx1.max(sx0 + 1.0), v_y0 + ROW_H - 2.0),
                ),
                1.0,
                Stroke::new(
                    if focused { 2.5 } else { 1.0 },
                    if focused {
                        color_focus()
                    } else {
                        Color32::from_rgb(70, 110, 140)
                    },
                ),
                egui::StrokeKind::Middle,
            );
        }
    }

    paint_pins(
        &painter,
        track,
        track_w,
        duration_frames,
        fps,
        &model.io_pins,
        color_io_pin(),
        focus,
    );
    if model.application != TimelineApplication::Source {
        paint_pins(
            &painter,
            track,
            track_w,
            duration_frames,
            fps,
            &model.markers,
            color_m_pin(),
            focus,
        );
    }

    // Playhead
    let px = track.left()
        + (virtual_frame.max(0) as f32 / duration_frames as f32).clamp(0.0, 1.0) * track_w;
    let ph_stroke = if matches!(focus, Some(FocusTarget::Playhead)) {
        Stroke::new(3.0, color_focus())
    } else {
        Stroke::new(2.0, color_playhead())
    };
    painter.line_segment(
        [
            Pos2::new(px, track.top() - 2.0),
            Pos2::new(px, track.bottom() + 2.0),
        ],
        ph_stroke,
    );

    // Time ruler
    painter.text(
        Pos2::new(track.left(), rect.bottom() - 12.0),
        egui::Align2::LEFT_CENTER,
        format!("0 / {duration_sec:.1}s"),
        egui::FontId::proportional(11.0),
        Color32::from_rgb(140, 140, 140),
    );
    painter.text(
        Pos2::new(px, rect.bottom() - 12.0),
        egui::Align2::CENTER_CENTER,
        format!("{:.2}s", frame_to_seconds(virtual_frame, fps)),
        egui::FontId::proportional(11.0),
        Color32::from_rgb(220, 220, 220),
    );

    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if pos.x >= track.left() && pos.x <= track.right() {
                let frame =
                    (((pos.x - track.left()) / track_w) * duration_frames as f32).round() as i64;
                return Some(frame.clamp(0, duration_frames));
            }
        }
    }
    None
}

fn paint_pins(
    painter: &egui::Painter,
    track: Rect,
    track_w: f32,
    duration_frames: i64,
    fps: f64,
    pins: &[TimelinePin],
    color: Color32,
    focus: Option<&FocusTarget>,
) {
    for pin in pins {
        let focused = match focus {
            Some(FocusTarget::In) => pin.kind.eq_ignore_ascii_case("in"),
            Some(FocusTarget::Out) => pin.kind.eq_ignore_ascii_case("out"),
            Some(FocusTarget::Marker { id }) => pin.id == *id,
            _ => false,
        };
        let stroke = if focused {
            Stroke::new(3.0, color_focus())
        } else {
            Stroke::new(1.5, color)
        };
        let frame = timeline_pin_frame(pin.timeline_frame, pin.timeline_sec, fps);
        let x = track.left()
            + (frame.max(0) as f32 / duration_frames.max(1) as f32).clamp(0.0, 1.0) * track_w;
        painter.line_segment(
            [Pos2::new(x, track.top()), Pos2::new(x, track.bottom())],
            stroke,
        );
        let tag = if pin.label.is_empty() {
            match pin.kind.as_str() {
                "in" => "I",
                "out" => "O",
                _ => "M",
            }
        } else {
            pin.label.as_str()
        };
        painter.text(
            Pos2::new(x, track.top() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            tag,
            egui::FontId::proportional(10.0),
            if focused { color_focus() } else { color },
        );
    }
}
