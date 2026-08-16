//! Shared **form elements** — Project (and any settings panel) uses this kit.
//!
//! Same layer as `chrome_row` / `action_btn`: screens compose, they do not invent
//! parallel padding, inline grids, or button styles.

use eframe::egui::{self, Color32, RichText, Vec2};

use crate::qnc_theme::{self, BORDER, MUTED};
use crate::qnc_ui;

/// Horizontal pad — matches chrome / content_panel.
pub const PAD_X: f32 = qnc_theme::CHROME_PAD_X as f32;
pub const PAD_Y: f32 = qnc_ui::space::GAP;
pub const INSET_X: f32 = qnc_theme::CHROME_PAD_X as f32;
pub const SECTION_GAP: f32 = qnc_ui::space::GAP;
pub const INLINE_LABEL_W: f32 = 168.0;
pub const INLINE_BTN_W: f32 = 120.0;
pub const INLINE_COL_GAP: f32 = qnc_ui::space::GAP;
pub const FIELD_MIN_W: f32 = 160.0;
pub const FIELD_GAP_X: f32 = qnc_ui::space::GAP;
pub const FIELD_GAP_Y: f32 = qnc_ui::space::GAP;
pub const LABEL_FS: f32 = 12.0;
pub const GROUP_TITLE_FS: f32 = 12.0;
pub const ROW_H: f32 = qnc_theme::CHROME_CTRL_H;

pub fn hline(ui: &mut egui::Ui, width: f32, color: Color32) {
    let y = ui.cursor().top();
    let x0 = ui.min_rect().left();
    ui.painter().hline(
        egui::Rangef::new(x0, x0 + width),
        y,
        egui::Stroke::new(1.0, color),
    );
    ui.add_space(1.0);
}

pub fn group_title(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(GROUP_TITLE_FS)
            .strong()
            .color(MUTED),
    );
    ui.add_space(SECTION_GAP);
}

/// Section divider — hairline only (no bordered / raised panel).
pub fn section(ui: &mut egui::Ui, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    hline(ui, width, BORDER);
    ui.add_space(10.0);
    ui.set_max_width(width);
    add_contents(ui);
}

/// Quiet ghost vs primary CTA — wraps `qnc_theme` buttons (no Project-only style).
pub fn btn(ui: &mut egui::Ui, label: &str, quiet: bool) -> egui::Response {
    btn_sized(ui, label, quiet, true)
}

pub fn btn_sized(ui: &mut egui::Ui, label: &str, quiet: bool, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        if quiet {
            qnc_theme::action_btn(ui, label)
        } else if label == "Novi projekt" || label.starts_with("Spremi") {
            qnc_theme::primary_btn(ui, label)
        } else {
            qnc_theme::action_btn(ui, label)
        }
    })
    .inner
}

/// Web `.qnc-pts-inline-row`: inset | label | 1fr | btn
pub fn inline_cols(content_w: f32) -> (f32, f32, f32) {
    let inner = (content_w - INSET_X * 2.0).max(0.0);
    let mut label_w = INLINE_LABEL_W;
    let mut btn_w = INLINE_BTN_W;
    let gaps = INLINE_COL_GAP * 2.0;
    if label_w + btn_w + gaps > inner {
        let scale = (inner - gaps).max(80.0) / (INLINE_LABEL_W + INLINE_BTN_W);
        label_w = (INLINE_LABEL_W * scale).max(88.0);
        btn_w = (INLINE_BTN_W * scale).max(96.0);
    }
    let mid = (inner - label_w - btn_w - gaps).max(48.0);
    (label_w, mid, btn_w)
}

pub fn inline_row(
    ui: &mut egui::Ui,
    content_w: f32,
    label: &str,
    mut content: impl FnMut(&mut egui::Ui),
    mut trailing: impl FnMut(&mut egui::Ui),
) {
    let h = ROW_H;
    let (label_w, mid, btn_w) = inline_cols(content_w);

    ui.horizontal(|ui| {
        ui.set_min_height(h);
        ui.set_max_width(content_w);
        ui.add_space(INSET_X);
        ui.add_sized(
            Vec2::new(label_w, h),
            egui::Label::new(RichText::new(label).size(LABEL_FS).strong().color(MUTED)),
        );
        ui.add_space(INLINE_COL_GAP);
        ui.allocate_ui_with_layout(
            Vec2::new(mid, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_max_width(mid);
                content(ui);
            },
        );
        ui.add_space(INLINE_COL_GAP);
        ui.allocate_ui_with_layout(
            Vec2::new(btn_w, h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_width(btn_w);
                ui.set_max_width(btn_w);
                trailing(ui);
            },
        );
    });
}

/// Actions aligned to the trailing column of `inline_row`.
pub fn trailing_actions(
    ui: &mut egui::Ui,
    content_w: f32,
    mut trailing: impl FnMut(&mut egui::Ui),
) {
    let h = ROW_H;
    let (label_w, mid, btn_w) = inline_cols(content_w);
    ui.horizontal(|ui| {
        ui.set_min_height(h);
        ui.set_max_width(content_w);
        ui.add_space(INSET_X + label_w + INLINE_COL_GAP + mid + INLINE_COL_GAP);
        ui.allocate_ui_with_layout(
            Vec2::new(btn_w, h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_width(btn_w);
                ui.set_max_width(btn_w);
                ui.spacing_mut().item_spacing.x = 8.0;
                trailing(ui);
            },
        );
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRowAction {
    None,
    Pick,
    Commit(String),
}

/// Editable path + Odaberi… — shared path field row.
pub fn path_row(
    ui: &mut egui::Ui,
    content_w: f32,
    label: &str,
    draft: &mut String,
) -> PathRowAction {
    let h = ROW_H;
    let (label_w, mid, btn_w) = inline_cols(content_w);
    let before = draft.clone();
    let mut commit = false;
    let mut do_pick = false;

    ui.horizontal(|ui| {
        ui.set_min_height(h);
        ui.set_max_width(content_w);
        ui.add_space(INSET_X);
        ui.add_sized(
            Vec2::new(label_w, h),
            egui::Label::new(RichText::new(label).size(LABEL_FS).strong().color(MUTED)),
        );
        ui.add_space(INLINE_COL_GAP);
        ui.allocate_ui_with_layout(
            Vec2::new(mid, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_max_width(mid);
                let resp = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(ui.available_width().max(40.0))
                        .hint_text("Putanja"),
                );
                if resp.lost_focus() && *draft != before {
                    commit = true;
                }
            },
        );
        ui.add_space(INLINE_COL_GAP);
        ui.allocate_ui_with_layout(
            Vec2::new(btn_w, h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_width(btn_w);
                ui.set_max_width(btn_w);
                if btn_sized(ui, "Odaberi…", false, true).clicked() {
                    do_pick = true;
                }
            },
        );
    });

    if do_pick {
        PathRowAction::Pick
    } else if commit {
        PathRowAction::Commit(draft.clone())
    } else {
        PathRowAction::None
    }
}

pub fn field_cell_width(grid_w: f32, cols: usize) -> f32 {
    let cols = cols.max(1) as f32;
    ((grid_w - FIELD_GAP_X * (cols - 1.0)) / cols).max(FIELD_MIN_W.min(grid_w))
}

pub fn field_grid_cols(grid_w: f32) -> usize {
    let n = ((grid_w + FIELD_GAP_X) / (FIELD_MIN_W + FIELD_GAP_X)).floor() as usize;
    n.max(1)
}
