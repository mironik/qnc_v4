//! Thin wrapper — paint lives in `qnc_ui::preview` (Story reference).

use eframe::egui::{self, TextureHandle};

use crate::qnc_ui;

pub(super) struct PreviewMonitorInput<'a> {
    pub height: f32,
    pub texture: Option<&'a TextureHandle>,
}

pub(super) fn show(ui: &mut egui::Ui, input: PreviewMonitorInput<'_>) {
    qnc_ui::preview(
        ui,
        qnc_ui::PreviewInput {
            height: input.height,
            texture: input.texture,
            empty_label: "Odaberi klip",
            sense: egui::Sense::hover(),
        },
    );
}
