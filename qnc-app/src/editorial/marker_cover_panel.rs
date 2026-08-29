//! Shared editorial marker/cover controls.
//!
//! UI-only module: it returns typed actions and leaves the screen owner in charge of
//! API mutations and playback state.

use eframe::egui::{self, Pos2, Rect, RichText, Sense, Vec2};

use crate::qnc_theme::{self, MUTED, TEXT};

const COMPACT_CTRL_H: f32 = 22.0;
const EDIT_ACTIONS_W: f32 = 250.0;
const RIGHT_ACTIONS_W: f32 = 110.0;
const EDIT_ACTION_GAP: f32 = 6.0;
const CONTROL_GROUP_GAP: f32 = 10.0;
const TRANSPORT_BTN_W: f32 = 30.0;
const TRANSPORT_GAP: f32 = 5.0;
const TRANSPORT_CONTROLS_W: f32 = TRANSPORT_BTN_W * 7.0 + TRANSPORT_GAP * 6.0;

pub(crate) struct MarkerCoverInput<'a> {
    pub leading_label: Option<&'a str>,
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub tc: &'a dyn Fn(f64) -> String,
    pub show_playhead: bool,
    pub sync_cover_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MarkerCoverAction {
    None,
    PlaylistStart,
    PreviousSegment,
    PreviousMarkerSlot,
    PreviousMarker,
    NextMarker,
    NextMarkerSlot,
    NextSegment,
    AddMarker,
    CreateCover,
    OverwriteCover,
    ToggleSyncCover,
}

pub(crate) fn show(ui: &mut egui::Ui, input: MarkerCoverInput<'_>) -> MarkerCoverAction {
    let mut action = MarkerCoverAction::None;

    ui.horizontal(|ui| {
        if let Some(label) = input.leading_label {
            ui.label(RichText::new(label).color(MUTED).small());
            ui.label(RichText::new("-").color(MUTED).small());
        }
        if input.show_playhead {
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
            ui.add_space(8.0);
        }
        let (row_rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), COMPACT_CTRL_H),
            Sense::hover(),
        );
        if let Some(transport_rect) = show_transport_controls(ui, row_rect, &mut action) {
            show_edit_actions(ui, row_rect, transport_rect, &mut action);
            show_right_actions(
                ui,
                row_rect,
                transport_rect,
                input.sync_cover_enabled,
                &mut action,
            );
        }
    });

    action
}

fn show_transport_controls(
    ui: &mut egui::Ui,
    row_rect: Rect,
    action: &mut MarkerCoverAction,
) -> Option<Rect> {
    if row_rect.width() < TRANSPORT_CONTROLS_W {
        return None;
    }
    let x = row_rect.center().x - TRANSPORT_CONTROLS_W * 0.5;
    let rect = Rect::from_min_size(
        Pos2::new(x, row_rect.min.y),
        Vec2::new(TRANSPORT_CONTROLS_W, COMPACT_CTRL_H),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = TRANSPORT_GAP;
            if compact_icon_btn(ui, "⏮", "Prethodni segment").clicked() {
                *action = MarkerCoverAction::PreviousSegment;
            }
            if compact_slot_btn(ui, "Prethodni slot").clicked() {
                *action = MarkerCoverAction::PreviousMarkerSlot;
            }
            if compact_icon_btn(ui, "⚑", "Prethodni marker").clicked() {
                *action = MarkerCoverAction::PreviousMarker;
            }
            if compact_icon_btn(ui, "🏠", "Početak Playlist inputa").clicked() {
                *action = MarkerCoverAction::PlaylistStart;
            }
            if compact_icon_btn(ui, "⚑", "Sljedeći marker").clicked() {
                *action = MarkerCoverAction::NextMarker;
            }
            if compact_slot_btn(ui, "Sljedeći slot").clicked() {
                *action = MarkerCoverAction::NextMarkerSlot;
            }
            if compact_icon_btn(ui, "⏭", "Sljedeći segment").clicked() {
                *action = MarkerCoverAction::NextSegment;
            }
        },
    );
    Some(rect)
}

fn show_edit_actions(
    ui: &mut egui::Ui,
    row_rect: Rect,
    transport_rect: Rect,
    action: &mut MarkerCoverAction,
) {
    let max_x = transport_rect.min.x - CONTROL_GROUP_GAP;
    let available_w = max_x - row_rect.min.x;
    if available_w < 120.0 {
        return;
    }
    let width = available_w.min(EDIT_ACTIONS_W);
    let rect = Rect::from_min_max(
        Pos2::new(row_rect.min.x, row_rect.min.y),
        Pos2::new(row_rect.min.x + width, row_rect.max.y),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = EDIT_ACTION_GAP;
            ui.spacing_mut().button_padding = Vec2::new(6.0, 1.0);
            if compact_action_btn(ui, "M marker").clicked() {
                *action = MarkerCoverAction::AddMarker;
            }
            if compact_action_btn(ui, "Cover slot").clicked() {
                *action = MarkerCoverAction::CreateCover;
            }
            if compact_action_btn(ui, "Overwrite").clicked() {
                *action = MarkerCoverAction::OverwriteCover;
            }
        },
    );
}

fn show_right_actions(
    ui: &mut egui::Ui,
    row_rect: Rect,
    transport_rect: Rect,
    sync_cover_enabled: bool,
    action: &mut MarkerCoverAction,
) {
    let min_x = transport_rect.max.x + CONTROL_GROUP_GAP;
    let available_w = row_rect.max.x - min_x;
    if available_w < 110.0 {
        return;
    }
    let width = available_w.min(RIGHT_ACTIONS_W);
    let rect = Rect::from_min_max(
        Pos2::new(row_rect.max.x - width, row_rect.min.y),
        Pos2::new(row_rect.max.x, row_rect.max.y),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = EDIT_ACTION_GAP;
            ui.spacing_mut().button_padding = Vec2::new(6.0, 1.0);
            if compact_toggle_btn(ui, "Sync/B-roll", sync_cover_enabled).clicked() {
                *action = MarkerCoverAction::ToggleSyncCover;
            }
        },
    );
}

fn compact_action_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    compact_action_btn_state(ui, label, false)
}

fn compact_action_btn_state(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let t = qnc_theme::current(ui);
    let fill = if active {
        qnc_theme::ACCENT.gamma_multiply(0.45)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.add(
        egui::Button::new(RichText::new(label).color(t.text).size(qnc_theme::FONT_UI))
            .min_size(Vec2::new(0.0, COMPACT_CTRL_H))
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, t.border)),
    )
}

fn compact_toggle_btn(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let t = qnc_theme::current(ui);
    let fill = if active {
        qnc_theme::ACCENT.gamma_multiply(0.55)
    } else {
        egui::Color32::TRANSPARENT
    };
    let border = egui::Color32::from_gray(145);
    ui.add(
        egui::Button::new(RichText::new(label).color(t.text).size(qnc_theme::FONT_UI))
            .min_size(Vec2::new(0.0, COMPACT_CTRL_H))
            .fill(fill)
            .stroke(egui::Stroke::new(1.4, border)),
    )
    .on_hover_text("Sync pokrivalica")
}

fn compact_icon_btn(ui: &mut egui::Ui, icon: &str, tooltip: &str) -> egui::Response {
    let t = qnc_theme::current(ui);
    ui.add(
        egui::Button::new(RichText::new(icon).color(t.text).size(18.0))
            .min_size(Vec2::new(TRANSPORT_BTN_W, COMPACT_CTRL_H))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, t.border)),
    )
    .on_hover_text(tooltip)
}

fn compact_slot_btn(ui: &mut egui::Ui, tooltip: &str) -> egui::Response {
    let t = qnc_theme::current(ui);
    let response = ui.add(
        egui::Button::new(RichText::new("").size(18.0))
            .min_size(Vec2::new(TRANSPORT_BTN_W, COMPACT_CTRL_H))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, t.border)),
    );
    if ui.is_rect_visible(response.rect) {
        let color = if response.hovered() { t.text } else { t.muted };
        let slot_rect = Rect::from_center_size(response.rect.center(), Vec2::new(14.0, 5.0));
        ui.painter().rect_stroke(
            slot_rect,
            0.0,
            egui::Stroke::new(1.4, color),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(tooltip)
}
