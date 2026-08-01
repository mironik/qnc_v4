//! `template-picker` BLOCK — RADNI TOK dropdown used by `settings.rs`.
//!
//! Pure paint + hit-test. Owns no state; caller maps [`TemplatePickerAction`]
//! into `ProjectScreen` field mutations / `ProjectAction`.

use eframe::egui::{self, Color32, RichText, Sense};

use crate::api::TemplateRow;
use crate::qnc_form;
use crate::qnc_theme::{self, BORDER, MUTED, TEXT};

const HEAD_BORDER: Color32 = BORDER;
const ROW_BORDER: Color32 = BORDER;

/// Web `.qnc-pts-cards { max-height: 200px }`
const PICKER_CARDS_MAX_H: f32 = 200.0;

pub struct TemplatePickerInput<'a> {
    pub templates: &'a [TemplateRow],
    pub selected_id: &'a str,
    pub picker_open: bool,
    pub confirm_delete_template: Option<&'a str>,
    pub content_w: f32,
}

pub enum TemplatePickerAction {
    None,
    ToggleOpen,
    Select(String),
    /// Sets confirm UI.
    RequestDelete(String),
    ConfirmDelete(String),
    CancelDelete,
}

pub fn show(ui: &mut egui::Ui, input: TemplatePickerInput<'_>) -> TemplatePickerAction {
    let mut action = TemplatePickerAction::None;
    // Local mirror so a click this frame can immediately reveal/hide the
    // cards without waiting a frame (matches prior in-place toggle).
    let mut open = input.picker_open;

    if let Some(tid) = input.confirm_delete_template {
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(220, 120, 80), "Obrisati template?");
                    if qnc_form::btn(ui, "Da", false).clicked() {
                        action = TemplatePickerAction::ConfirmDelete(tid.to_string());
                    }
                    if qnc_form::btn(ui, "Ne", true).clicked() {
                        action = TemplatePickerAction::CancelDelete;
                    }
                });
            });
    }

    egui::Frame::NONE.show(ui, |ui| {
        ui.set_max_width(input.content_w);
        let summary = input
            .templates
            .iter()
            .find(|t| t.template_id == input.selected_id)
            .map(|t| t.name.as_str())
            .unwrap_or("—");

        ui.label(RichText::new("RADNI TOK").size(12.0).strong().color(MUTED));
        ui.add_space(6.0);
        let theme = qnc_theme::current(ui);
        let head = egui::Frame::NONE
            .fill(theme.raised)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_max_width(input.content_w);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(summary).size(14.0).strong().color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(if open { "▲" } else { "▼" })
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                });
            });
        if head.response.interact(Sense::click()).clicked() {
            open = !open;
            action = TemplatePickerAction::ToggleOpen;
        }

        if !open {
            return;
        }
        qnc_form::hline(ui, input.content_w, HEAD_BORDER);
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 4,
                bottom: 8,
            })
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("pts_templates")
                    .max_height(PICKER_CARDS_MAX_H)
                    .show(ui, |ui| {
                        ui.set_min_width(input.content_w.max(120.0));
                        if input.templates.is_empty() {
                            ui.label(RichText::new("Nema templatea.").color(MUTED));
                        }
                        for tmpl in input.templates {
                            let selected = tmpl.template_id == input.selected_id;
                            let t = qnc_theme::current(ui);
                            let fill = if selected {
                                t.surface
                            } else {
                                Color32::TRANSPARENT
                            };
                            let can_delete = !tmpl.is_system();
                            let mut delete_hit = false;
                            // HIT TEST: × and select-card MUST be separate responses —
                            // never Sense::click on a Frame that contains ×.
                            let card = egui::Frame::NONE
                                .fill(fill)
                                .inner_margin(egui::Margin::symmetric(0, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&tmpl.name).strong().color(TEXT));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if can_delete
                                                    && qnc_theme::text_link(ui, "×", true)
                                                        .on_hover_text("Obriši template")
                                                        .clicked()
                                                {
                                                    delete_hit = true;
                                                    action = TemplatePickerAction::RequestDelete(
                                                        tmpl.template_id.clone(),
                                                    );
                                                }
                                                if !tmpl.description.is_empty() {
                                                    ui.label(
                                                        RichText::new(&tmpl.description)
                                                            .size(12.0)
                                                            .color(MUTED),
                                                    );
                                                }
                                            },
                                        );
                                    });
                                });
                            if !delete_hit && card.response.interact(Sense::click()).clicked() {
                                // Caller closes the picker (picker_open = false) on Select.
                                action = TemplatePickerAction::Select(tmpl.template_id.clone());
                            }
                            qnc_form::hline(ui, input.content_w, ROW_BORDER);
                        }
                    });
            });
    });

    action
}
