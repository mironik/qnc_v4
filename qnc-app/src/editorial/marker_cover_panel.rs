//! Shared editorial marker/cover controls.
//!
//! UI-only module: it returns typed actions and leaves the screen owner in charge of
//! API mutations and playback state.

use eframe::egui::{self, Pos2, Rect, RichText, Sense, Vec2};

use crate::qnc_theme::{self, MUTED, TEXT};

const COMPACT_CTRL_H: f32 = 22.0;
const EDIT_ACTIONS_W: f32 = 260.0;
const EDIT_ACTION_GAP: f32 = 6.0;
const TRANSPORT_BTN_W: f32 = 30.0;
const TRANSPORT_GAP: f32 = 5.0;
const TRANSPORT_CONTROLS_W: f32 = TRANSPORT_BTN_W * 7.0 + TRANSPORT_GAP * 6.0;

pub(crate) struct MarkerCoverInput<'a> {
    pub leading_label: Option<&'a str>,
    pub virtual_frame: i64,
    pub playhead_sec: f64,
    pub tc: &'a dyn Fn(f64) -> String,
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
        let (row_rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), COMPACT_CTRL_H),
            Sense::hover(),
        );
        show_transport_controls(ui, row_rect, &mut action);
        show_edit_actions(ui, row_rect, &mut action);
    });

    action
}

fn show_transport_controls(ui: &mut egui::Ui, row_rect: Rect, action: &mut MarkerCoverAction) {
    let free_right = (row_rect.max.x - EDIT_ACTIONS_W - 12.0).max(row_rect.min.x);
    let free_w = free_right - row_rect.min.x;
    if free_w < TRANSPORT_CONTROLS_W {
        return;
    }
    let x = row_rect.min.x + (free_w - TRANSPORT_CONTROLS_W) * 0.5;
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
}

fn show_edit_actions(ui: &mut egui::Ui, row_rect: Rect, action: &mut MarkerCoverAction) {
    let x = (row_rect.max.x - EDIT_ACTIONS_W).max(row_rect.min.x);
    let rect = Rect::from_min_max(Pos2::new(x, row_rect.min.y), row_rect.max);
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = EDIT_ACTION_GAP;
            ui.spacing_mut().button_padding = Vec2::new(6.0, 1.0);
            if compact_action_btn(ui, "Overwrite").clicked() {
                *action = MarkerCoverAction::OverwriteCover;
            }
            if compact_action_btn(ui, "Cover slot").clicked() {
                *action = MarkerCoverAction::CreateCover;
            }
            if compact_action_btn(ui, "M marker").clicked() {
                *action = MarkerCoverAction::AddMarker;
            }
        },
    );
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
