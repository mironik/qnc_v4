//! Project form — **layout board** (Lego plate) + named slots.
//!
//! This module defines *where* blocks sit. It does not paint domain UI.
//! [`super::settings`] only fills slots with components.
//!
//! Matches web `.qnc-pts-panel`:
//! `grid-template-rows: auto auto minmax(0, 1fr)`
//!
//! ```text
//! qnc_ui::project_workspace          ← outer board (workspace-split)
//! ├── LEFT  → project_list           ← list.rs
//! └── RIGHT → pts_panel              ← this file
//!       ├── HEAD   (auto)            title + subtitle
//!       ├── FIXED  (content height)  named slots top-down — never stretch
//!       │     ├── TemplatePicker
//!       │     ├── ProjectCreate
//!       │     ├── AiSettings
//!       │     ├── ProjectsRoot
//!       │     ├── ExportDirectory
//!       │     └── TemplateActions
//!       └── SCROLL (minmax 0,1fr)    always present
//!             ├── Advanced
//!             └── CustomTemplate     (only when create panel open)
//! ```

use eframe::egui::{self, Sense, Vec2};

use crate::qnc_form::{PAD_X, PAD_Y, SECTION_GAP};
use crate::qnc_theme::{self, BORDER};

/// Every paint region the board asks a filler to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtsSlot {
    Head,
    Fixed(PtsFixedSlot),
    Scroll(PtsScrollSlot),
}

/// FIXED-region slots (web `data-pts-section`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtsFixedSlot {
    TemplatePicker,
    ProjectCreate,
    AiSettings,
    ProjectsRoot,
    ExportDirectory,
    TemplateActions,
}

/// SCROLL-region slots (web order: advanced → custom_template).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtsScrollSlot {
    Advanced,
    CustomTemplate,
}

const FIXED_SLOTS: &[PtsFixedSlot] = &[
    PtsFixedSlot::TemplatePicker,
    PtsFixedSlot::ProjectCreate,
    PtsFixedSlot::AiSettings,
    PtsFixedSlot::ProjectsRoot,
    PtsFixedSlot::ExportDirectory,
    PtsFixedSlot::TemplateActions,
];

/// Geometry after the PTS board is allocated.
#[derive(Debug, Clone, Copy)]
pub struct PtsPanelMetrics {
    pub panel: egui::Rect,
    pub content_w: f32,
    pub head: egui::Rect,
    pub fixed: egui::Rect,
    pub scroll: egui::Rect,
}

/// Paint the PTS 3-row board and invoke **one filler call per named slot**.
///
/// - FIXED uses natural content height (no empty stretch gap).
/// - SCROLL always fills remaining panel height.
/// - `custom_template_open` only gates the CustomTemplate scroll slot.
pub fn pts_panel(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    custom_template_open: bool,
    mut fill: impl FnMut(PtsSlot, &mut egui::Ui, f32),
) -> PtsPanelMetrics {
    let t = qnc_theme::current(ui);
    let (panel, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter().rect_filled(panel, 0.0, t.bg);

    let content_w = (width - PAD_X * 2.0).max(120.0);
    let mut head_rect = egui::Rect::NOTHING;
    let mut fixed_rect = egui::Rect::NOTHING;
    let mut scroll_rect = egui::Rect::NOTHING;

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(panel)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(panel);
            ui.spacing_mut().item_spacing = Vec2::ZERO;

            // —— HEAD (auto) ——
            let head_top = ui.cursor().top();
            fill(PtsSlot::Head, ui, content_w);
            let head_bottom = ui.min_rect().bottom().max(ui.cursor().top());
            head_rect = egui::Rect::from_min_max(
                egui::pos2(panel.left(), head_top),
                egui::pos2(panel.right(), head_bottom),
            );
            ui.painter().hline(
                egui::Rangef::new(panel.left(), panel.right()),
                head_bottom,
                egui::Stroke::new(1.0, BORDER),
            );

            // —— FIXED (auto / content height — never pre-allocate empty budget) ——
            let fixed_top = ui.cursor().top();
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: PAD_X as i8,
                    right: PAD_X as i8,
                    top: PAD_Y as i8,
                    bottom: PAD_Y as i8,
                })
                .show(ui, |ui| {
                    ui.set_max_width(content_w);
                    ui.spacing_mut().item_spacing.y = SECTION_GAP;
                    for &slot in FIXED_SLOTS {
                        fill(PtsSlot::Fixed(slot), ui, content_w);
                    }
                });
            let fixed_bottom = ui.min_rect().bottom().max(ui.cursor().top());
            fixed_rect = egui::Rect::from_min_max(
                egui::pos2(panel.left(), fixed_top),
                egui::pos2(panel.right(), fixed_bottom),
            );
            ui.painter().hline(
                egui::Rangef::new(panel.left(), panel.right()),
                fixed_bottom,
                egui::Stroke::new(1.0, BORDER),
            );

            // —— SCROLL (minmax 0, 1fr) — always ——
            let scroll_top = fixed_bottom + 1.0;
            let area = egui::Rect::from_min_max(
                egui::pos2(panel.left(), scroll_top),
                egui::pos2(panel.right(), panel.bottom()),
            );
            scroll_rect = area;
            let scroll_h = area.height().max(48.0);
            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(area)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    ui.set_clip_rect(area);
                    egui::ScrollArea::vertical()
                        .id_salt("pts_scroll")
                        .max_height(scroll_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            egui::Frame::NONE
                                .inner_margin(egui::Margin {
                                    left: PAD_X as i8,
                                    right: PAD_X as i8,
                                    top: PAD_Y as i8,
                                    bottom: PAD_Y as i8,
                                })
                                .show(ui, |ui| {
                                    ui.set_max_width(content_w);
                                    ui.spacing_mut().item_spacing.y = SECTION_GAP;
                                    fill(PtsSlot::Scroll(PtsScrollSlot::Advanced), ui, content_w);
                                    if custom_template_open {
                                        fill(
                                            PtsSlot::Scroll(PtsScrollSlot::CustomTemplate),
                                            ui,
                                            content_w,
                                        );
                                    }
                                });
                        });
                },
            );
        },
    );

    PtsPanelMetrics {
        panel,
        content_w,
        head: head_rect,
        fixed: fixed_rect,
        scroll: scroll_rect,
    }
}
