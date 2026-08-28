//! `project_list` **component** — left Project panel.
//!
//! Pure paint + hit-test. Orchestrator ([`super::screen`]) maps
//! [`ProjectListAction`] → host / form actions.
//!
//! Contract:
//! - **20px** panel pad (title, table, footer all inside)
//! - Invisible 2-col table: name | plain ×
//! - Selected row only: surface fill
//! - × → Da/Ne popup above that ×

use eframe::egui::{self, RichText, Sense, Vec2};

use crate::api::ProjectRow;
use crate::qnc_theme::{self, FONT_UI, MUTED, TEXT};

/// Inset of all panel content (title, table, footer).
pub const PANEL_PAD: f32 = 20.0;
/// Left inset for name text inside each row.
const TEXT_PAD_L: f32 = 20.0;
/// Space under the "Projekti" title before the table.
const BELOW_TITLE_PAD: f32 = 40.0;
const ROW_H: f32 = 60.0;
const ROW_GAP: f32 = 10.0;
const DEL_COL_W: f32 = 28.0;
const COL_GAP: f32 = 8.0;
const CONFIRM_BTN_W: f32 = 28.0;
const CONFIRM_BTN_H: f32 = 22.0;
const CONFIRM_GAP: f32 = 8.0;
const CONFIRM_MARGIN_X: f32 = 8.0;
const CONFIRM_MARGIN_Y: f32 = 6.0;
const CONFIRM_OVERLAY_GAP: f32 = 2.0;

pub struct ProjectListInput<'a> {
    pub width: f32,
    pub height: f32,
    pub projects: &'a [ProjectRow],
    pub selected_index: Option<usize>,
    pub active_project_id: &'a str,
    pub confirm_delete: bool,
    pub status: &'a str,
}

#[allow(dead_code)]
pub enum ProjectListAction {
    None,
    Select(usize),
    Open(usize),
    RequestDelete(usize),
    ConfirmDelete,
    CancelDelete,
}

pub fn show(ui: &mut egui::Ui, input: ProjectListInput<'_>) -> ProjectListAction {
    let mut action = ProjectListAction::None;
    let t = qnc_theme::current(ui);
    let (panel_rect, _) =
        ui.allocate_exact_size(Vec2::new(input.width, input.height), Sense::hover());
    ui.painter().rect_filled(panel_rect, 0.0, t.bg);

    let content = egui::Rect::from_min_max(
        egui::pos2(panel_rect.left() + PANEL_PAD, panel_rect.top() + PANEL_PAD),
        egui::pos2(
            panel_rect.right() - PANEL_PAD,
            panel_rect.bottom() - PANEL_PAD,
        ),
    );
    let content_w = content.width().max(40.0);

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(content);
            ui.spacing_mut().item_spacing = Vec2::ZERO;

            let title_h = qnc_theme::CHROME_ROW_H;
            let (title_rect, _) =
                ui.allocate_exact_size(Vec2::new(content_w, title_h), Sense::hover());
            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(title_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.label(RichText::new("Projekti").size(FONT_UI).strong().color(TEXT));
                },
            );
            ui.painter().hline(
                title_rect.x_range(),
                title_rect.bottom() - 0.5,
                egui::Stroke::new(1.0, t.border),
            );

            ui.add_space(BELOW_TITLE_PAD);

            let foot_h = qnc_theme::CHROME_ROW_H;
            let table_top = ui.cursor().top();
            let table_bottom = (content.bottom() - foot_h).max(table_top + 40.0);
            let table_rect = egui::Rect::from_min_max(
                egui::pos2(content.left(), table_top),
                egui::pos2(content.right(), table_bottom),
            );
            let table_w = table_rect.width().max(40.0);
            let foot_rect = egui::Rect::from_min_size(
                egui::pos2(content.left(), table_bottom),
                Vec2::new(content_w, foot_h),
            );

            let mut confirm_anchor: Option<egui::Rect> = None;

            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(table_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    ui.set_clip_rect(table_rect);
                    ui.set_max_width(table_w);
                    egui::ScrollArea::vertical()
                        .id_salt("project_list")
                        .max_height(table_rect.height())
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_max_width(table_w);
                            if input.projects.is_empty() {
                                ui.label(
                                    RichText::new("Nema projekata.").size(FONT_UI).color(MUTED),
                                );
                                return;
                            }

                            for (i, p) in input.projects.iter().enumerate() {
                                let selected = input.selected_index == Some(i);
                                let (row_rect, _) = ui
                                    .allocate_exact_size(Vec2::new(table_w, ROW_H), Sense::hover());
                                let del_rect = egui::Rect::from_min_size(
                                    egui::pos2(row_rect.right() - DEL_COL_W, row_rect.top()),
                                    Vec2::new(DEL_COL_W, ROW_H),
                                );
                                let del_button_rect = egui::Rect::from_center_size(
                                    del_rect.center(),
                                    Vec2::new(DEL_COL_W, CONFIRM_BTN_H),
                                );
                                let select_rect = egui::Rect::from_min_max(
                                    row_rect.min,
                                    egui::pos2(del_rect.left() - COL_GAP, row_rect.bottom()),
                                );
                                let name_rect = egui::Rect::from_min_max(
                                    egui::pos2(row_rect.left() + TEXT_PAD_L, row_rect.top()),
                                    select_rect.right_bottom(),
                                );
                                let select = ui.interact(
                                    select_rect,
                                    ui.id().with(("project_list_row_select", i)),
                                    Sense::click(),
                                );
                                if selected || select.hovered() {
                                    let fill = if selected {
                                        t.surface
                                    } else {
                                        t.surface.linear_multiply(0.55)
                                    };
                                    ui.painter().rect_filled(row_rect, 0.0, fill);
                                }

                                ui.allocate_new_ui(
                                    egui::UiBuilder::new()
                                        .max_rect(name_rect)
                                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                    |ui| {
                                        ui.set_clip_rect(name_rect);
                                        ui.set_max_width(name_rect.width().max(0.0));
                                        let mut label =
                                            RichText::new(&p.name).size(FONT_UI).color(TEXT);
                                        if p.project_id == input.active_project_id {
                                            label = label.strong();
                                        }
                                        ui.add(
                                            egui::Label::new(label)
                                                .truncate()
                                                .sense(Sense::hover())
                                                .selectable(false),
                                        );
                                    },
                                );

                                let del = ui
                                    .allocate_new_ui(
                                        egui::UiBuilder::new().max_rect(del_button_rect).layout(
                                            egui::Layout::centered_and_justified(
                                                egui::Direction::LeftToRight,
                                            ),
                                        ),
                                        |ui| {
                                            ui.set_clip_rect(del_button_rect);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new("×").size(FONT_UI).color(MUTED),
                                                )
                                                .sense(Sense::click())
                                                .selectable(false),
                                            )
                                        },
                                    )
                                    .inner;

                                if selected && input.confirm_delete {
                                    confirm_anchor = Some(del_button_rect);
                                }

                                if del.clicked() {
                                    action = ProjectListAction::RequestDelete(i);
                                } else if select.clicked() {
                                    action = ProjectListAction::Open(i);
                                }

                                if i + 1 < input.projects.len() {
                                    ui.add_space(ROW_GAP);
                                }
                            }
                        });
                },
            );

            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(foot_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.set_clip_rect(foot_rect);
                    ui.label(RichText::new(input.status).size(FONT_UI).color(MUTED));
                },
            );

            if let Some(anchor) = confirm_anchor {
                egui::Area::new(egui::Id::new("project_list_del_confirm"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(confirm_popup_pos(anchor))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .inner_margin(egui::Margin::symmetric(
                                CONFIRM_MARGIN_X as i8,
                                CONFIRM_MARGIN_Y as i8,
                            ))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = CONFIRM_GAP;
                                    if ui
                                        .add_sized(
                                            Vec2::new(CONFIRM_BTN_W, CONFIRM_BTN_H),
                                            egui::Label::new(
                                                RichText::new("Da").size(FONT_UI).color(TEXT),
                                            )
                                            .sense(Sense::click())
                                            .selectable(false),
                                        )
                                        .clicked()
                                    {
                                        action = ProjectListAction::ConfirmDelete;
                                    }
                                    if ui
                                        .add_sized(
                                            Vec2::new(CONFIRM_BTN_W, CONFIRM_BTN_H),
                                            egui::Label::new(
                                                RichText::new("Ne").size(FONT_UI).color(MUTED),
                                            )
                                            .sense(Sense::click())
                                            .selectable(false),
                                        )
                                        .clicked()
                                    {
                                        action = ProjectListAction::CancelDelete;
                                    }
                                });
                            });
                    });
            }
        },
    );

    action
}

fn confirm_popup_pos(anchor: egui::Rect) -> egui::Pos2 {
    let popup_h = CONFIRM_BTN_H + (CONFIRM_MARGIN_Y * 2.0);
    let ne_center_x = CONFIRM_MARGIN_X + CONFIRM_BTN_W + CONFIRM_GAP + (CONFIRM_BTN_W * 0.5);
    egui::pos2(
        anchor.center().x - ne_center_x,
        anchor.top() - popup_h - CONFIRM_OVERLAY_GAP,
    )
}
