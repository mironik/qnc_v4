//! Shared editorial Segmenti panel.
//!
//! Uses standalone [`crate::qnc_segment_timeline`] for paint.
//! Editorial segment list stays in this panel — not inside the timeline component.
//! Segment rows are only projections of one continuous playlist input timeline.

use eframe::egui::{self, RichText, Vec2};

use crate::editorial::marker_cover_panel;
use crate::editorial::segment_program::{SegmentProgramMarkerSlot, SegmentProgramModel};
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};
use crate::qnc_segment_timeline::{
    self, SegmentAudioExpansion, SegmentTimelineProgramCover, SegmentTimelineProgramInput,
    SegmentTimelineProgramIntent, SegmentTimelineProgramMarker, SegmentTimelineProgramMarkerSlot,
    SegmentTimelineProgramSegment,
};
use crate::qnc_theme::current;

pub(crate) struct SegmentPanelInput<'a> {
    pub height: f32,
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub program: &'a SegmentProgramModel,
    pub marker_slots: &'a [MarkerSlot],
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub selected_slot_id: &'a str,
    pub selected_cover_id: &'a str,
    pub tc: &'a dyn Fn(f64) -> String,
    pub tc_frame: &'a dyn Fn(i64) -> String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SegmentPanelAction {
    None,
    SeekTimelineFrame(i64),
    MarkerCover(marker_cover_panel::MarkerCoverAction),
    SelectMarkerSlot(String),
    SelectCover(String),
    SelectMarker { marker_id: String, frame: i64 },
}

pub(crate) fn show(ui: &mut egui::Ui, input: SegmentPanelInput<'_>) -> SegmentPanelAction {
    let mut action = SegmentPanelAction::None;
    let t = current(ui);

    egui::Frame::NONE
        .fill(t.surface)
        .stroke(egui::Stroke::new(1.0, t.border))
        .inner_margin(10.0)
        .show(ui, |ui| {
            let effective_selected_slot_id = input
                .program
                .effective_marker_slot_id(input.selected_slot_id);
            ui.set_min_size(Vec2::new(ui.available_width(), input.height));
            ui.label(RichText::new("Segmenti").color(t.text).strong());
            ui.add_space(8.0);

            if input.program.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(input.height * 0.35);
                    ui.label(
                        RichText::new("Nema segmenata — dodaj ton i off segment").color(t.muted),
                    );
                });
                return;
            }

            let stack_height = (input.height - 320.0).max(140.0);
            let timeline_covers = segment_timeline_covers(input.covers, input.selected_cover_id);
            let timeline_slots =
                segment_timeline_slots(input.program.marker_slots(), effective_selected_slot_id);
            let timeline_markers = segment_timeline_markers(input.markers);
            let timeline_segments = segment_timeline_segments(input.program, input.virtual_frame);
            // Segment stack: local row projections only. Playback remains outside
            // this panel; returned frame is a global program-frame request.
            egui::ScrollArea::vertical()
                .max_height(stack_height)
                .show(ui, |ui| {
                    let timeline_intent = qnc_segment_timeline::show_program(
                        ui,
                        SegmentTimelineProgramInput {
                            playhead_program_frame: input.virtual_frame,
                            segments: &timeline_segments,
                            covers: &timeline_covers,
                            marker_slots: &timeline_slots,
                            markers: &timeline_markers,
                            expanded_audio: SegmentAudioExpansion::None,
                            show_lane_labels: true,
                        },
                    );
                    if matches!(action, SegmentPanelAction::None) {
                        action = segment_action_from_timeline_intent(timeline_intent);
                    }
                });

            ui.add_space(8.0);
            let marker_action = marker_cover_panel::show(
                ui,
                marker_cover_panel::MarkerCoverInput {
                    virtual_frame: input.virtual_frame,
                    playhead_sec: input.playhead_sec,
                    marker_slots: input.marker_slots,
                    covers: input.covers,
                    markers: input.markers,
                    selected_slot_id: effective_selected_slot_id,
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
            ui.label(RichText::new("Redoslijed segmenata").color(t.muted).small());
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .show(ui, |ui| {
                    let active_part_id = input
                        .program
                        .active_part_at_program_frame(input.virtual_frame)
                        .map(|segment| segment.part_id.as_str());
                    for seg in input.program.segments() {
                        let active = active_part_id.is_some_and(|id| id == seg.part_id.as_str());
                        let label = format!(
                            "{}  {}–{}",
                            if seg.kind.is_empty() {
                                "segment"
                            } else {
                                seg.kind.as_str()
                            },
                            (input.tc_frame)(seg.global_start_frame),
                            (input.tc_frame)(seg.global_end_frame.max(seg.global_start_frame))
                        );
                        if ui
                            .selectable_label(active, RichText::new(label).size(12.0))
                            .clicked()
                        {
                            action = segment_row_click_action(seg.global_start_frame);
                        }
                    }
                });

            ui.add_space(8.0);
            ui.label(RichText::new("Playlist input").color(t.muted).small());
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
                    expanded_audio: SegmentAudioExpansion::None,
                    show_lane_labels: true,
                },
            );
            if matches!(action, SegmentPanelAction::None) {
                action = segment_action_from_timeline_intent(overview_intent);
            }
        });

    action
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
        SegmentTimelineProgramIntent::SelectMarkerSlot(slot_id) => {
            SegmentPanelAction::SelectMarkerSlot(slot_id)
        }
        SegmentTimelineProgramIntent::SelectCover(cover_id) => {
            SegmentPanelAction::SelectCover(cover_id)
        }
        SegmentTimelineProgramIntent::SelectMarker {
            marker_id,
            program_frame,
        } => SegmentPanelAction::SelectMarker {
            marker_id,
            frame: program_frame,
        },
    }
}

fn segment_row_click_action(global_start_frame: i64) -> SegmentPanelAction {
    SegmentPanelAction::SeekTimelineFrame(global_start_frame.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylist, EditorialPlaylistSegment};

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
    fn segment_row_click_seeks_same_continuous_program_timeline() {
        assert_eq!(
            segment_row_click_action(50),
            SegmentPanelAction::SeekTimelineFrame(50)
        );
        assert_eq!(
            segment_row_click_action(-10),
            SegmentPanelAction::SeekTimelineFrame(0)
        );
    }

    #[test]
    fn timeline_layer_intents_preserve_db_ids_and_program_frames() {
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::SelectMarkerSlot(
                "slot_a".into()
            ),),
            SegmentPanelAction::SelectMarkerSlot("slot_a".into())
        );
        assert_eq!(
            segment_action_from_timeline_intent(SegmentTimelineProgramIntent::SelectCover(
                "cover_a".into()
            ),),
            SegmentPanelAction::SelectCover("cover_a".into())
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
