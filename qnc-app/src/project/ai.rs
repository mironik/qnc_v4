//! `ai_settings` BLOCK — FIXED slot: AI checkboxes.

use eframe::egui;
use serde_json::Value;

use crate::qnc_form;

use super::screen::{ProjectAction, ProjectScreen};

pub struct AiSettingsInput<'a> {
    pub content_w: f32,
    pub effective: &'a Value,
}

pub fn show(ui: &mut egui::Ui, input: AiSettingsInput<'_>) -> ProjectAction {
    let mut action = ProjectAction::None;
    qnc_form::section(ui, input.content_w, |ui| {
        qnc_form::group_title(ui, "AI");
        ui.spacing_mut().item_spacing.y = 8.0;
        ai_check(
            ui,
            &mut action,
            "ai.enabled",
            ProjectScreen::path_bool(input.effective, &["ai", "enabled"], false),
            "AI analiza kadrova i virtualni kadrovi",
        );
        ai_check(
            ui,
            &mut action,
            "ai.coverage_suggestions",
            ProjectScreen::path_bool(input.effective, &["ai", "coverage_suggestions"], true),
            "Coverage suggestions",
        );
        ai_check(
            ui,
            &mut action,
            "ai.transcription_enabled",
            ProjectScreen::path_bool(input.effective, &["ai", "transcription_enabled"], false),
            "Transkripcija u Media tabu",
        );
    });
    action
}

fn ai_check(ui: &mut egui::Ui, action: &mut ProjectAction, path: &str, checked: bool, label: &str) {
    let mut v = checked;
    if ui.checkbox(&mut v, label).changed() {
        *action = ProjectAction::SetSettingsPath(path.into(), Value::Bool(v));
    }
}
