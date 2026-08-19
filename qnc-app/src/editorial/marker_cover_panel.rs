//! Shared editorial marker/cover controls.
//!
//! UI-only module: it returns typed actions and leaves the screen owner in charge of
//! API mutations and playback state.

use eframe::egui::{self, RichText, Vec2};

use crate::qnc_theme::{self, MUTED, TEXT};

const COMPACT_CTRL_H: f32 = 20.0;

pub(crate) struct MarkerCoverInput<'a> {
    pub leading_label: Option<&'a str>,
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub tc: &'a dyn Fn(f64) -> String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MarkerCoverAction {
    None,
    AddMarker,
    CreateCover,
    OverwriteCover,
}

pub(crate) fn show(ui: &mut egui::Ui, input: MarkerCoverInput<'_>) -> MarkerCoverAction {
    let mut action = MarkerCoverAction::None;

    ui.horizontal(|ui| {
        if let Some(label) = input.leading_label {
            ui.label(RichText::new(label).color(MUTED).small());
            ui.label(RichText::new("-").color(MUTED).small());
        }
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
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), COMPACT_CTRL_H),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.spacing_mut().button_padding = Vec2::new(6.0, 1.0);
                if compact_action_btn(ui, "Overwrite").clicked() {
                    action = MarkerCoverAction::OverwriteCover;
                }
                if compact_action_btn(ui, "Cover slot").clicked() {
                    action = MarkerCoverAction::CreateCover;
                }
                if compact_action_btn(ui, "M marker").clicked() {
                    action = MarkerCoverAction::AddMarker;
                }
            },
        );
    });

    action
}

fn compact_action_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let t = qnc_theme::current(ui);
    ui.add(
        egui::Button::new(RichText::new(label).color(t.text).size(qnc_theme::FONT_UI))
            .min_size(Vec2::new(0.0, COMPACT_CTRL_H))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, t.border)),
    )
}
