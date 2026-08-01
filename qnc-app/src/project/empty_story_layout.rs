//! Project board — Story column shell + two body slots.
//!
//! Code names (not painted on form):
//! - `project_list` — left body (component owns **20px** pad)
//! - `setting_panel` — right body (owns its own **20px** content pad)
//!
//! Body faces keep shell `bg`. No extra outer pad here — panels own padding.

use eframe::egui::{self, Sense, Vec2};

use crate::qnc_theme;
use crate::qnc_ui::{self, ShellSide};

/// Project left column share (list side).
pub const LEFT_RATIO: f32 = 0.31;

/// Paint left | right bodies; `paint(side, inner_w, inner_h)` fills each slot.
pub fn project_board(ui: &mut egui::Ui, mut paint: impl FnMut(&mut egui::Ui, ShellSide, f32, f32)) {
    qnc_ui::column_shell(ui, LEFT_RATIO, |ui, m, side| {
        body_slot(ui, m.height.max(0.0), |ui, w, h| paint(ui, side, w, h));
    });
}

fn body_slot(ui: &mut egui::Ui, height: f32, mut fill: impl FnMut(&mut egui::Ui, f32, f32)) {
    let t = qnc_theme::current(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.bg);

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect);
            fill(ui, width, height);
        },
    );
}
