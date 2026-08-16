//! Shared editorial marker/cover controls.
//!
//! UI-only module: it returns typed actions and leaves the screen owner in charge of
//! API mutations and playback state.

use eframe::egui::{self, RichText};

use crate::editorial::common::{action_btn, truncate};
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker};
use crate::qnc_theme::{MUTED, TEXT};

pub(crate) struct MarkerCoverInput<'a> {
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub marker_slots: &'a [MarkerSlot],
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub selected_slot_id: &'a str,
    pub selected_cover_id: &'a str,
    pub tc: &'a dyn Fn(f64) -> String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MarkerCoverAction {
    None,
    AddMarker,
    CreateCover,
    OverwriteCover,
    SelectSlot(String),
    SelectCover(String),
    DeleteCover(String),
    SeekMarkerFrame(i64),
    MoveMarker {
        marker_id: String,
        direction: String,
    },
    DeleteMarker(String),
}

pub(crate) fn show(ui: &mut egui::Ui, input: MarkerCoverInput<'_>) -> MarkerCoverAction {
    let mut action = MarkerCoverAction::None;

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("Playhead {}", (input.tc)(input.playhead_sec)))
                .color(TEXT)
                .small(),
        );
        ui.label(
            RichText::new(format!("frame {}", input.virtual_frame.max(0)))
                .color(MUTED)
                .small(),
        );
        if action_btn(ui, "M marker").clicked() {
            action = MarkerCoverAction::AddMarker;
        }
        if action_btn(ui, "Cover slot").clicked() {
            action = MarkerCoverAction::CreateCover;
        }
        if action_btn(ui, "Overwrite").clicked() {
            action = MarkerCoverAction::OverwriteCover;
        }
    });

    if !matches!(action, MarkerCoverAction::None) {
        return action;
    }

    action = marker_slots(ui, &input);
    if !matches!(action, MarkerCoverAction::None) {
        return action;
    }

    action = covers(ui, &input);
    if !matches!(action, MarkerCoverAction::None) {
        return action;
    }

    markers(ui, &input)
}

fn marker_slots(ui: &mut egui::Ui, input: &MarkerCoverInput<'_>) -> MarkerCoverAction {
    if input.marker_slots.is_empty() {
        return MarkerCoverAction::None;
    }

    let mut action = MarkerCoverAction::None;
    ui.add_space(6.0);
    ui.label(RichText::new("Marker slotovi").color(TEXT).small().strong());
    ui.horizontal_wrapped(|ui| {
        for slot in input.marker_slots {
            let id = slot.slot_id.clone();
            let active = !id.is_empty() && id == input.selected_slot_id;
            let cover_mark = if slot.has_cover { "●" } else { "○" };
            let label = if !slot.label.is_empty() {
                format!("{cover_mark} {}", slot.label)
            } else {
                format!(
                    "{cover_mark} {}–{}",
                    (input.tc)(slot.start_sec),
                    (input.tc)(slot.end_sec.max(slot.start_sec))
                )
            };
            if ui.selectable_label(active, label).clicked() {
                action = MarkerCoverAction::SelectSlot(id);
            }
        }
    });
    action
}

fn covers(ui: &mut egui::Ui, input: &MarkerCoverInput<'_>) -> MarkerCoverAction {
    if input.covers.is_empty() {
        return MarkerCoverAction::None;
    }

    let mut action = MarkerCoverAction::None;
    ui.add_space(6.0);
    ui.label(RichText::new("Coveri").color(TEXT).small().strong());
    egui::ScrollArea::horizontal()
        .id_salt("story_cover_strip")
        .max_height(38.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for cover in input.covers {
                    let id = cover.cover_id.clone();
                    let active = !id.is_empty() && id == input.selected_cover_id;
                    let title = if !cover.title.is_empty() {
                        cover.title.clone()
                    } else if !cover.clip_id.is_empty() {
                        cover.clip_id.clone()
                    } else {
                        id.clone()
                    };
                    let label = format!(
                        "{}  {}–{}",
                        truncate(&title, 18),
                        (input.tc)(cover.timeline_start_sec),
                        (input.tc)(cover.timeline_end_sec.max(cover.timeline_start_sec))
                    );
                    ui.horizontal(|ui| {
                        if ui.selectable_label(active, label).clicked() {
                            action = MarkerCoverAction::SelectCover(id.clone());
                        }
                        if ui
                            .add(egui::Button::new(RichText::new("Del").small()))
                            .on_hover_text("Obriši cover")
                            .clicked()
                        {
                            action = MarkerCoverAction::DeleteCover(id.clone());
                        }
                        ui.add_space(4.0);
                    });
                }
            });
        });
    action
}

fn markers(ui: &mut egui::Ui, input: &MarkerCoverInput<'_>) -> MarkerCoverAction {
    if input.markers.is_empty() {
        return MarkerCoverAction::None;
    }

    let mut action = MarkerCoverAction::None;
    ui.add_space(6.0);
    ui.label(RichText::new("Markeri").color(TEXT).small().strong());
    egui::ScrollArea::horizontal()
        .id_salt("story_marker_strip")
        .max_height(34.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for marker in input.markers {
                    let frame = marker.timeline_frame.max(0);
                    let label = if !marker.label.is_empty() {
                        format!("{} {}", (input.tc)(marker.timeline_sec), marker.label)
                    } else {
                        format!("{} M", (input.tc)(marker.timeline_sec))
                    };
                    let marker_id = marker.marker_id.clone();
                    let locked = frame == 0;
                    ui.horizontal(|ui| {
                        if ui.selectable_label(false, label).clicked() {
                            action = MarkerCoverAction::SeekMarkerFrame(frame);
                        }
                        if ui
                            .add_enabled(!locked, egui::Button::new(RichText::new("Up").small()))
                            .on_hover_text("Pomakni marker ranije")
                            .clicked()
                        {
                            action = MarkerCoverAction::MoveMarker {
                                marker_id: marker_id.clone(),
                                direction: "up".into(),
                            };
                        }
                        if ui
                            .add_enabled(!locked, egui::Button::new(RichText::new("Down").small()))
                            .on_hover_text("Pomakni marker kasnije")
                            .clicked()
                        {
                            action = MarkerCoverAction::MoveMarker {
                                marker_id: marker_id.clone(),
                                direction: "down".into(),
                            };
                        }
                        if ui
                            .add_enabled(!locked, egui::Button::new(RichText::new("Del").small()))
                            .on_hover_text("Obriši marker")
                            .clicked()
                        {
                            action = MarkerCoverAction::DeleteMarker(marker_id.clone());
                        }
                        ui.add_space(4.0);
                    });
                }
            });
        });
    action
}
