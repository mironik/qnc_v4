//! `project_create` BLOCK — FIXED slot: naziv + Novi projekt.

use eframe::egui;

use crate::qnc_form;

use super::screen::ProjectAction;

pub struct ProjectCreateInput<'a> {
    pub content_w: f32,
    pub name: &'a mut String,
    pub can_create: bool,
}

pub fn show(ui: &mut egui::Ui, input: ProjectCreateInput<'_>) -> ProjectAction {
    let mut action = ProjectAction::None;
    qnc_form::inline_row(
        ui,
        input.content_w,
        "Naziv projekta:",
        |ui| {
            ui.add(
                egui::TextEdit::singleline(input.name)
                    .desired_width(ui.available_width().max(40.0))
                    .hint_text("Naziv novog projekta"),
            );
        },
        |ui| {
            if qnc_form::btn_sized(ui, "Novi projekt", false, input.can_create).clicked() {
                action = ProjectAction::Create;
            }
        },
    );
    action
}
