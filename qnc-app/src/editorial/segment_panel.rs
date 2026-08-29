//! Shared editorial Segmenti panel.
//!
//! Uses standalone [`crate::qnc_segment_timeline`] for paint.
//! Editorial segment list stays in this panel — not inside the timeline component.
//! Segment rows are only projections of one continuous playlist input timeline.

use eframe::egui::{self, RichText, Vec2};

use crate::editorial::marker_cover_panel;
use crate::editorial::segment_program::{SegmentProgramMarkerSlot, SegmentProgramModel};
use crate::editorial::types::{StoryCover, StoryMarker};
use crate::qnc_segment_timeline::{
    self, SegmentAudioExpansion, SegmentTimelineProgramCover, SegmentTimelineProgramInput,
    SegmentTimelineProgramIntent, SegmentTimelineProgramMarker, SegmentTimelineProgramMarkerSlot,
    SegmentTimelineProgramSegment,
};
use crate::qnc_theme::current;

const PANEL_MARGIN: f32 = 10.0;
const BOTTOM_GAP: f32 = 4.0;
const PLAYLIST_INPUT_H: f32 = 128.0;

pub(crate) struct SegmentPanelInput<'a> {
    pub height: f32,
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub program: &'a SegmentProgramModel,
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub selected_slot_id: &'a str,
    pub selected_cover_id: &'a str,
    pub sync_cover_enabled: bool,
    pub tc: &'a dyn Fn(f64) -> String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SegmentPanelAction {
    None,
    SeekTimelineFrame(i64),
    MarkerCover(marker_cover_panel::MarkerCoverAction),
    SelectMarkerSlot { slot_id: String, frame: i64 },
    SelectCover { cover_id: String, frame: i64 },
    SelectMarker { marker_id: String, frame: i64 },
}

pub(crate) fn show(ui: &mut egui::Ui, input: SegmentPanelInput<'_>) -> SegmentPanelAction {
    let mut action = SegmentPanelAction::None;
    let t = current(ui);
    let stack_audio_expansion_id = egui::Id::new("story_segment_stack_audio_expansion");
    let playlist_audio_expansion_id = egui::Id::new("story_playlist_input_audio_expansion");
    let panel_size = Vec2::new(
        ui.available_width(),
        input.height.min(ui.available_height()).max(0.0),
    );
    let (panel_rect, _) = ui.allocate_exact_size(panel_size, egui::Sense::hover());
    ui.painter().rect_filled(panel_rect, 0.0, t.surface);
    ui.painter().rect_stroke(
        panel_rect,
        0.0,
        egui::Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );

    let content_rect = panel_rect.shrink(PANEL_MARGIN);
    if content_rect.is_positive() {
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.set_clip_rect(content_rect);
                let effective_selected_slot_id = input
                    .program
                    .effective_marker_slot_id(input.selected_slot_id);
                ui.set_min_size(content_rect.size());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Segmenti").color(t.text).strong());
                    if !input.program.is_empty() {
                        let timing = segment_panel_timing(input.program, input.virtual_frame);
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            right_header_timing_group(
                                ui,
                                &time_from_frames(&input, timing.segment_frames),
                                &(input.tc)(input.playhead_sec),
                                timing.playhead_frame,
                                &time_from_frames(&input, timing.total_frames),
                            );
                        });
                    }
                });
                ui.add_space(8.0);

                if input.program.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.35);
                        ui.label(
                            RichText::new("Nema segmenata — dodaj ton i off segment")
                                .color(t.muted),
                        );
                    });
                    return;
                }

                let timeline_covers =
                    segment_timeline_covers(input.covers, input.selected_cover_id);
                let timeline_slots = segment_timeline_slots(
                    input.program.marker_slots(),
                    effective_selected_slot_id,
                );
                let timeline_markers = segment_timeline_markers(input.markers);
                let timeline_segments =
                    segment_timeline_segments(input.program, input.virtual_frame);

                let body_size = Vec2::new(ui.available_width(), ui.available_height().max(0.0));
                let (body_rect, _) = ui.allocate_exact_size(body_size, egui::Sense::hover());
                let playlist_h = PLAYLIST_INPUT_H.min(body_rect.height());
                let playlist_rect = egui::Rect::from_min_max(
                    egui::pos2(body_rect.left(), body_rect.bottom() - playlist_h),
                    body_rect.right_bottom(),
                );
                let stack_bottom = (playlist_rect.top() - BOTTOM_GAP).max(body_rect.top());
                let stack_rect = egui::Rect::from_min_max(
                    body_rect.left_top(),
                    egui::pos2(body_rect.right(), stack_bottom),
                );

                // Segment stack: local row projections only. Playback remains outside
                // this panel; returned frame is a global program-frame request.
                if stack_rect.height() > 0.0 {
                    ui.allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(stack_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |ui| {
                            ui.set_clip_rect(stack_rect);
                            egui::ScrollArea::vertical()
                                .id_salt("story_segment_stack_scroll")
                                .max_height(stack_rect.height())
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_min_width(stack_rect.width());
                                    let timeline_intent = qnc_segment_timeline::show_program(
                                        ui,
                                        SegmentTimelineProgramInput {
                                            playhead_program_frame: input.virtual_frame,
                                            segments: &timeline_segments,
                                            covers: &timeline_covers,
                                            marker_slots: &timeline_slots,
                                            markers: &timeline_markers,
                                            waveform_duration_frames: input
                                                .program
                                                .duration_frames(),
                                            a1_peaks: input.a1_peaks,
                                            a2_peaks: input.a2_peaks,
                                            expanded_audio: audio_expansion(
                                                ui.ctx(),
                                                stack_audio_expansion_id,
                                            ),
                                            show_lane_labels: true,
                                        },
                                    );
                                    apply_timeline_intent(
                                        ui.ctx(),
                                        stack_audio_expansion_id,
                                        timeline_intent,
                                        &mut action,
                                    );
                                });
                        },
                    );
                }

                if playlist_rect.height() > 0.0 {
                    ui.allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(playlist_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |ui| {
                            ui.set_clip_rect(playlist_rect);
                            ui.set_min_width(playlist_rect.width());
                            ui.separator();
                            let marker_action = marker_cover_panel::show(
                                ui,
                                marker_cover_panel::MarkerCoverInput {
                                    leading_label: None,
                                    virtual_frame: input.virtual_frame,
                                    playhead_sec: input.playhead_sec,
                                    tc: input.tc,
                                    show_playhead: false,
                                    sync_cover_enabled: input.sync_cover_enabled,
                                },
                            );
                            if !matches!(marker_action, marker_cover_panel::MarkerCoverAction::None)
                                && matches!(action, SegmentPanelAction::None)
                            {
                                action = SegmentPanelAction::MarkerCover(marker_action);
                            }
                            ui.add_space(2.0);
                            // Final playlist-input timeline: one overview of the DB playlist,
                            // still only a graphic projection of broadcast-player progress.
                            let overview_intent = qnc_segment_timeline::show_program_overview(
                                ui,
                                SegmentTimelineProgramInput {
                                    playhead_program_frame: input.virtual_frame,
                                    segments: &timeline_segments,
                                    covers: &timeline_covers,
                                    marker_slots: &timeline_slots,
                                    markers: &timeline_markers,
                                    waveform_duration_frames: input.program.duration_frames(),
                                    a1_peaks: input.a1_peaks,
                                    a2_peaks: input.a2_peaks,
                                    expanded_audio: audio_expansion(
                                        ui.ctx(),
                                        playlist_audio_expansion_id,
                                    ),
                                    show_lane_labels: true,
                                },
                            );
                            apply_timeline_intent(
                                ui.ctx(),
                                playlist_audio_expansion_id,
                                overview_intent,
                                &mut action,
                            );
                        },
                    );
                }
            },
        );
    }

    action
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SegmentPanelTiming {
    segment_frames: i64,
    playhead_frame: i64,
    total_frames: i64,
}

fn segment_panel_timing(program: &SegmentProgramModel, playhead_frame: i64) -> SegmentPanelTiming {
    let playhead_frame = playhead_frame.max(0);
    let segment_frames = program
        .active_part_at_program_frame(playhead_frame)
        .or_else(|| {
            program
                .segments()
                .last()
                .filter(|_| playhead_frame >= program.duration_frames())
        })
        .map(|segment| segment.duration_frames.max(0))
        .unwrap_or(0);
    SegmentPanelTiming {
        segment_frames,
        playhead_frame,
        total_frames: program.duration_frames(),
    }
}

fn time_from_frames(input: &SegmentPanelInput<'_>, frames: i64) -> String {
    input
        .program
        .timeline_fps()
        .map(|fps| (input.tc)(frames.max(0) as f64 / fps))
        .unwrap_or_else(|| "--:--:--:--".into())
}

fn right_header_timing_group(
    ui: &mut egui::Ui,
    segment_duration: &str,
    playhead: &str,
    frame: i64,
    duration: &str,
) {
    let t = current(ui);
    let segment_color = t.accent;
    let playhead_color = t.focus;
    header_value(ui, duration, egui::Color32::from_rgb(255, 120, 120));
    header_label(ui, "Trajanje");
    ui.add_space(18.0);
    header_value(ui, &frame.max(0).to_string(), playhead_color);
    header_label(ui, "frame");
    header_value(ui, playhead, playhead_color);
    header_label(ui, "Playhead");
    ui.add_space(18.0);
    header_value(ui, segment_duration, segment_color);
    header_label(ui, "Segment");
}

fn header_label(ui: &mut egui::Ui, label: &str) {
    let t = current(ui);
    ui.label(RichText::new(label).color(t.muted).size(17.0).strong());
}

fn header_value(ui: &mut egui::Ui, value: &str, color: egui::Color32) {
    ui.label(RichText::new(value).color(color).size(22.0).strong());
}

fn audio_expansion(ctx: &egui::Context, id: egui::Id) -> SegmentAudioExpansion {
    ctx.data_mut(|data| {
        data.get_persisted::<SegmentAudioExpansion>(id)
            .unwrap_or_default()
    })
}

fn set_audio_expansion(ctx: &egui::Context, id: egui::Id, expanded: SegmentAudioExpansion) {
    ctx.data_mut(|data| data.insert_persisted(id, expanded));
}

fn apply_timeline_intent(
    ctx: &egui::Context,
    expanded_id: egui::Id,
    intent: SegmentTimelineProgramIntent,
    action: &mut SegmentPanelAction,
) {
    if let SegmentTimelineProgramIntent::ToggleAudioExpand(lane) = intent {
        let expanded = audio_expansion(ctx, expanded_id).toggle(lane);
        set_audio_expansion(ctx, expanded_id, expanded);
        return;
    }
    if matches!(action, SegmentPanelAction::None) {
        *action = segment_action_from_timeline_intent(intent);
    }
}

fn segment_timeline_covers<'a>(
    covers: &'a [StoryCover],
    selected_cover_id: &str,
) -> Vec<SegmentTimelineProgramCover<'a>> {
    covers
        .iter()
        .map(|cover| SegmentTimelineProgramCover {
            id: cover.cover_id.as_str(),
            start_frame: cover.timeline_start_frame.max(0),
            end_frame: cover.timeline_end_frame.max(cover.timeline_start_frame),
            selected: !cover.cover_id.is_empty() && cover.cover_id == selected_cover_id,
        })
        .collect()
}

fn segment_timeline_segments<'a>(
    program: &'a SegmentProgramModel,
    virtual_frame: i64,
) -> Vec<SegmentTimelineProgramSegment<'a>> {
    let active_part_id = program
        .active_part_at_program_frame(virtual_frame)
        .or_else(|| {
            program
                .segments()
                .last()
                .filter(|_| virtual_frame >= program.duration_frames())
        })
        .map(|segment| segment.part_id.as_str());
    program
        .segments()
        .iter()
        .map(|seg| SegmentTimelineProgramSegment {
            id: seg.part_id.as_str(),
            kind: seg.kind.as_str(),
            start_frame: seg.global_start_frame,
            end_frame: seg.global_end_frame,
            has_base_video: !seg.kind.trim().eq_ignore_ascii_case("offovi"),
            selected: active_part_id.is_some_and(|id| id == seg.part_id.as_str()),
        })
        .collect()
}

fn segment_timeline_slots<'a>(
    marker_slots: &'a [SegmentProgramMarkerSlot],
    selected_slot_id: &str,
) -> Vec<SegmentTimelineProgramMarkerSlot<'a>> {
    marker_slots
        .iter()
        .map(|slot| SegmentTimelineProgramMarkerSlot {
            id: slot.slot_id.as_str(),
            start_frame: slot.start_frame.max(0),
            end_frame: slot.end_frame.max(slot.start_frame),
            has_cover: slot.has_cover,
            selected: !slot.slot_id.is_empty() && slot.slot_id == selected_slot_id,
        })
        .collect()
}

fn segment_timeline_markers<'a>(
    markers: &'a [StoryMarker],
) -> Vec<SegmentTimelineProgramMarker<'a>> {
    markers
        .iter()
        .map(|marker| SegmentTimelineProgramMarker {
            id: marker.marker_id.as_str(),
            frame: marker.timeline_frame.max(0),
        })
        .collect()
}

fn segment_action_from_timeline_intent(intent: SegmentTimelineProgramIntent) -> SegmentPanelAction {
    match intent {
        SegmentTimelineProgramIntent::None | SegmentTimelineProgramIntent::ToggleAudioExpand(_) => {
            SegmentPanelAction::None
        }
        SegmentTimelineProgramIntent::CueProgramFrame(program_frame) => {
            SegmentPanelAction::SeekTimelineFrame(program_frame)
        }
        SegmentTimelineProgramIntent::SelectMarkerSlot {
            slot_id,
            program_frame,
        } => SegmentPanelAction::SelectMarkerSlot {
            slot_id,
            frame: program_frame,
        },
        SegmentTimelineProgramIntent::SelectCover {
            cover_id,
            program_frame,
        } => SegmentPanelAction::SelectCover {
            cover_id,
            frame: program_frame,
        },
        SegmentTimelineProgramIntent::SelectMarker {
            marker_id,
            program_frame,
        } => SegmentPanelAction::SelectMarker {
            marker_id,
            frame: program_frame,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylist, EditorialPlaylistSegment};
    use crate::editorial::types::MarkerSlot;

    fn program_fixture() -> SegmentProgramModel {
        SegmentProgramModel::from_playlist(
            Some(&EditorialPlaylist {
                project_id: "p".into(),
                timeline_fps: 50.0,
                duration_frames: 90,
                segments: vec![
                    EditorialPlaylistSegment {
                        part_id: "p1".into(),
                        kind: "tonovi".into(),
                        global_start_frame: 0,
                        global_end_frame: 50,
                        duration_frames: 50,
                        clip_id: "clip_a".into(),
                        source_in_frame: 0,
                        source_out_frame: 50,
                        ..EditorialPlaylistSegment::default()
                    },
                    EditorialPlaylistSegment {
                        part_id: "p2".into(),
                        kind: "offovi".into(),
                        global_start_frame: 50,
                        global_end_frame: 90,
                        duration_frames: 40,
                        clip_id: "clip_b".into(),
                        source_in_frame: 10,
                        source_out_frame: 50,
                        ..EditorialPlaylistSegment::default()
                    },
                ],
                ..EditorialPlaylist::default()
            }),
            &[],
            &[],
            &[],
        )
    }

    #[test]
    fn first_empty_slot_is_effective_when_none_selected() {
        let slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                has_cover: true,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];
        let program = SegmentProgramModel::from_playlist(None, &slots, &[], &[]);

        assert_eq!(program.effective_marker_slot_id(""), "slot_b");
        assert_eq!(program.effective_marker_slot_id("slot_a"), "slot_a");
    }

    #[test]
    fn segment_panel_converts_program_model_to_program_timeline_models() {
        let program = program_fixture();
        let covers = vec![StoryCover {
            cover_id: "cover_a".into(),
            timeline_start_frame: 10,
            timeline_end_frame: 30,
            ..StoryCover::default()
        }];
        let markers = vec![StoryMarker {
            marker_id: "m_start".into(),
            timeline_frame: 0,
            ..StoryMarker::default()
        }];
        let slots_for_program = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 0,
            end_frame: 50,
            ..MarkerSlot::default()
        }];
        let program_with_slots =
            SegmentProgramModel::from_playlist(None, &slots_for_program, &[], &[]);
        let program_segments = segment_timeline_segments(&program, 55);

        assert_eq!(program_segments.len(), 2);
        assert_eq!(program_segments[0].start_frame, 0);
        assert_eq!(program_segments[1].start_frame, 50);
        assert_eq!(program_segments[1].end_frame, 90);
        assert!(program_segments[1].selected);
        assert!(!program_segments[1].has_base_video);
        assert_eq!(
            segment_timeline_covers(&covers, "cover_a")[0].start_frame,
            10
        );
        assert!(segment_timeline_covers(&covers, "cover_a")[0].selected);
        assert_eq!(
            segment_timeline_slots(program_with_slots.marker_slots(), "slot_a")[0].end_frame,
            50
        );
        assert!(segment_timeline_slots(program_with_slots.marker_slots(), "slot_a")[0].selected);
        assert_eq!(segment_timeline_markers(&markers)[0].frame, 0);
    }

    #[test]
    fn segment_panel_projects_program_end_to_last_segment() {
        let program = program_fixture();
        let program_segments = segment_timeline_segments(&program, program.duration_frames());

        assert_eq!(program.duration_frames(), 90);
        assert_eq!(program_segments.len(), 2);
        assert!(!program_segments[0].selected);
        assert!(program_segments[1].selected);
    }

    #[test]
    fn segment_panel_timing_uses_active_segment_and_total_program() {
        let program = program_fixture();

        assert_eq!(
            segment_panel_timing(&program, 55),
            SegmentPanelTiming {
                segment_frames: 40,
                playhead_frame: 55,
                total_frames: 90,
            }
        );
        assert_eq!(
            segment_panel_timing(&program, program.duration_frames()),
            SegmentPanelTiming {
                segment_frames: 40,
                playhead_frame: 90,
                total_frames: 90,
            }
        );
    }

    #[test]
    fn timeline_layer_intents_preserve_db_ids_and_program_frames() {
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::SelectMarkerSlot {
                slot_id: "slot_a".into(),
                program_frame: 44,
            }),
            SegmentPanelAction::SelectMarkerSlot {
                slot_id: "slot_a".into(),
                frame: 44,
            }
        );
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::SelectCover {
                cover_id: "cover_a".into(),
                program_frame: 22,
            }),
            SegmentPanelAction::SelectCover {
                cover_id: "cover_a".into(),
                frame: 22,
            }
        );
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::SelectMarker {
                marker_id: "m_mid".into(),
                program_frame: 80,
            }),
            SegmentPanelAction::SelectMarker {
                marker_id: "m_mid".into(),
                frame: 80,
            }
        );
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::CueProgramFrame(112)),
            SegmentPanelAction::SeekTimelineFrame(112)
        );
    }

    #[test]
    fn segment_marker_models_keep_global_program_frames() {
        let markers = [
            StoryMarker {
                marker_id: "m_start".into(),
                timeline_frame: 0,
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m_mid".into(),
                timeline_frame: 50,
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m_end".into(),
                timeline_frame: 100,
                ..StoryMarker::default()
            },
        ];

        assert_eq!(
            segment_timeline_markers(&markers)
                .iter()
                .map(|marker| (marker.id, marker.frame))
                .collect::<Vec<_>>(),
            vec![("m_start", 0), ("m_mid", 50), ("m_end", 100)]
        );
    }
}
