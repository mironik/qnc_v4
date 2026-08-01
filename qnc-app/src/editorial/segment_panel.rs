//! Shared editorial Segmenti panel.
//!
//! Uses standalone [`crate::qnc_timeline::QncTimeline`] for paint.
//! Editorial segment/part list stays in this panel — not inside the timeline component.

use eframe::egui::{self, RichText, Vec2};

use crate::api::TimelineSegment;
use crate::editorial::marker_cover_panel;
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};
use crate::qnc_theme::current;
use crate::qnc_timeline::{
    ExpandedAudio, LayerFlags, QncTimeline, TimelineCoverSpan, TimelineFocusPaint,
    TimelineMarkerPin, TimelineSlotSpan,
};

pub(crate) struct SegmentPanelInput<'a> {
    pub height: f32,
    pub duration_sec: f64,
    pub fps: f64,
    pub virtual_sec: f64,
    pub segments: &'a [TimelineSegment],
    pub active_part_id: Option<&'a str>,
    pub marker_slots: &'a [MarkerSlot],
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub selected_slot_id: &'a str,
    pub selected_cover_id: &'a str,
    pub tc: &'a dyn Fn(f64) -> String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SegmentPanelAction {
    None,
    SeekTimeline(f64),
    MarkerCover(marker_cover_panel::MarkerCoverAction),
    SelectSegment { part_id: String, start_sec: f64 },
}

fn wrap_layers() -> LayerFlags {
    LayerFlags {
        carrier: true,
        audio_a1: true,
        audio_a2: true,
        audio_a3: false,
        audio_a4: false,
        base_video: true,
        covers: true,
        markers: true,
        marker_slots: true,
        in_out: false,
        playhead: true,
    }
}

pub(crate) fn show(ui: &mut egui::Ui, input: SegmentPanelInput<'_>) -> SegmentPanelAction {
    let mut action = SegmentPanelAction::None;
    let t = current(ui);

    egui::Frame::NONE
        .fill(t.surface)
        .stroke(egui::Stroke::new(1.0, t.border))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), input.height));
            ui.label(RichText::new("Segmenti").color(t.text).strong());
            ui.add_space(8.0);

            if input.segments.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(input.height * 0.35);
                    ui.label(
                        RichText::new("Nema segmenata — dodaj ton i off segment").color(t.muted),
                    );
                });
                return;
            }

            let duration = input.duration_sec.max(0.1);
            let covers: Vec<TimelineCoverSpan<'_>> = input
                .covers
                .iter()
                .map(|c| TimelineCoverSpan {
                    id: c.cover_id.as_str(),
                    start_sec: c.timeline_start_sec,
                    end_sec: c.timeline_end_sec,
                    selected: !c.cover_id.is_empty() && c.cover_id == input.selected_cover_id,
                })
                .collect();
            let marker_slots: Vec<TimelineSlotSpan<'_>> = input
                .marker_slots
                .iter()
                .map(|s| TimelineSlotSpan {
                    id: s.slot_id.as_str(),
                    start_sec: s.start_sec,
                    end_sec: s.end_sec,
                    has_cover: s.has_cover,
                    selected: !s.slot_id.is_empty() && s.slot_id == input.selected_slot_id,
                })
                .collect();
            let markers: Vec<TimelineMarkerPin> = input
                .markers
                .iter()
                .map(|m| TimelineMarkerPin {
                    timeline_sec: m.timeline_sec,
                })
                .collect();

            let interact = QncTimeline {
                layers: wrap_layers(),
                duration_sec: duration,
                playhead_sec: input.virtual_sec,
                source_in: 0.0,
                source_out: duration,
                video_background: None,
                focus: TimelineFocusPaint::Playhead,
                expanded_audio: ExpandedAudio::None,
                a1_peaks: &[],
                a2_peaks: &[],
                a3_peaks: &[],
                a4_peaks: &[],
                covers: &covers,
                marker_slots: &marker_slots,
                markers: &markers,
                base_video_blank: false,
            }
            .show(ui);

            if let Some(sec) = interact.seek_sec {
                action = SegmentPanelAction::SeekTimeline(sec);
            }

            ui.add_space(8.0);
            let marker_action = marker_cover_panel::show(
                ui,
                marker_cover_panel::MarkerCoverInput {
                    virtual_sec: input.virtual_sec,
                    fps: input.fps,
                    marker_slots: input.marker_slots,
                    covers: input.covers,
                    markers: input.markers,
                    selected_slot_id: input.selected_slot_id,
                    selected_cover_id: input.selected_cover_id,
                    tc: input.tc,
                },
            );
            if !matches!(marker_action, marker_cover_panel::MarkerCoverAction::None)
                && matches!(action, SegmentPanelAction::None)
            {
                action = SegmentPanelAction::MarkerCover(marker_action);
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Parts").color(t.muted).small());
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .show(ui, |ui| {
                    for seg in input.segments {
                        let active = input
                            .active_part_id
                            .is_some_and(|id| id == seg.part_id.as_str());
                        let label = format!(
                            "{}  {}–{}",
                            if seg.kind.is_empty() {
                                "part"
                            } else {
                                seg.kind.as_str()
                            },
                            (input.tc)(seg.global_start_sec),
                            (input.tc)(seg.global_end_sec)
                        );
                        if ui
                            .selectable_label(active, RichText::new(label).size(12.0))
                            .clicked()
                        {
                            action = SegmentPanelAction::SelectSegment {
                                part_id: seg.part_id.clone(),
                                start_sec: seg.global_start_sec,
                            };
                        }
                    }
                });
        });

    action
}
