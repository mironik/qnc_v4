//! Shared editorial media pool.
//!
//! UI-only module for All / Virtual / Segment strips and compact transport
//! actions. Screen owners keep state mutations and host calls.
//! Head feature flags come from `composition::HeadFeatures` (orchestrator table).
//!
//! **One card grid** (`show_card_grid`) for Story / MA / Ingest — forms only
//! supply rows + features; layout never forks per screen.

use std::collections::HashMap;

use eframe::egui::{self, RichText, TextureHandle, Vec2};

use crate::api::IngestClip;
use crate::editorial::common::shot_id;
use crate::editorial::types::{LibraryTab, StoryPart, StoryShot};
use crate::qnc_media_card::{self, MediaCardInput};
use crate::qnc_theme::{self, current};
use crate::qnc_ui;

#[derive(Debug, Clone)]
pub(crate) enum MediaPoolAction {
    None,
    SwitchTab(LibraryTab),
    SelectShot(StoryShot),
    ToggleShotSelection(StoryShot),
    /// Ingest: card/thumb activation, independent from the checkbox hit zone.
    SelectClipId(String),
    /// Ingest: checkbox-only selection toggle.
    ToggleClipSelection(String),
    SelectPart(String),
    DeletePart(String),
    ReorderPart {
        part_id: String,
        direction: String,
    },
    TogglePlay,
    MarkIn,
    MarkOut,
    QuickCover,
    ExportCommit,
}

/// Built via `composition::HeadFeatures::to_pool_head` — do not hardcode flags in screens.
pub(crate) struct MediaPoolHeadInput {
    pub library_tab: LibraryTab,
    pub playing: bool,
    pub show_segment_tab: bool,
    pub show_export_xml: bool,
    pub show_quick_cover: bool,
}

pub(crate) struct MediaPoolStripInput<'a> {
    pub library_tab: LibraryTab,
    pub height: f32,
    pub selected_shot_id: &'a str,
    pub focused_shot_id: &'a str,
    pub panel_focused: bool,
    pub selected_clip_id: &'a str,
    pub all_clips: &'a [StoryShot],
    pub virtual_shots: &'a [StoryShot],
    pub parts: &'a [StoryPart],
    pub selected_part_id: &'a str,
    pub thumb_textures: &'a HashMap<String, TextureHandle>,
    pub tc: &'a dyn Fn(f64) -> String,
    /// Orchestrator chooses which card facets are on (Story ≠ MA ≠ Ingest).
    pub card_features: qnc_media_card::MediaCardFeatures,
}

/// One row in the shared media card grid (Story shot or Ingest clip).
pub(crate) struct MediaPoolCardRow<'a> {
    /// Click / focus identity (shot_id or clip_id).
    pub id: &'a str,
    /// Key into `thumb_textures` (usually clip_id).
    pub thumb_id: &'a str,
    pub title: &'a str,
    pub duration_sec: f64,
    pub duration_label: &'a str,
    pub import_status: &'a str,
    pub status_proxy: &'a str,
    pub status_original: &'a str,
    /// Selection check (Ingest multi-select / MA focus mirror).
    pub checked: bool,
}

pub(crate) struct MediaPoolCardGridInput<'a> {
    pub height: f32,
    pub selected_id: &'a str,
    pub focused_id: &'a str,
    pub panel_focused: bool,
    pub cards: &'a [MediaPoolCardRow<'a>],
    pub thumb_textures: &'a HashMap<String, TextureHandle>,
    pub tc: &'a dyn Fn(f64) -> String,
    pub card_features: qnc_media_card::MediaCardFeatures,
    pub empty_message: &'a str,
    pub id_salt: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaPoolCardGridAction {
    Activate(String),
    ToggleSelection(String),
}

pub(crate) fn show_head(ui: &mut egui::Ui, input: MediaPoolHeadInput) -> MediaPoolAction {
    let mut action = MediaPoolAction::None;
    qnc_theme::chrome_row(ui, true, |ui| {
        let tabs: &[LibraryTab] = if input.show_segment_tab {
            &[LibraryTab::All, LibraryTab::Virtual, LibraryTab::Segment]
        } else {
            &[LibraryTab::All, LibraryTab::Virtual]
        };
        for &tab in tabs {
            let label = match tab {
                LibraryTab::All => "All",
                LibraryTab::Virtual => "Virtual",
                LibraryTab::Segment => "Segment",
            };
            let active = input.library_tab == tab;
            if qnc_theme::link_tab(ui, label, active).clicked() {
                action = MediaPoolAction::SwitchTab(tab);
            }
            ui.add_space(10.0);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if input.show_export_xml && qnc_theme::transport_btn(ui, "Export XML").clicked() {
                action = MediaPoolAction::ExportCommit;
            }
            if input.show_quick_cover
                && qnc_theme::transport_btn(ui, "B")
                    .on_hover_text("Quick cover")
                    .clicked()
            {
                action = MediaPoolAction::QuickCover;
            }
            if qnc_theme::transport_btn(ui, "]")
                .on_hover_text("Mark OUT")
                .clicked()
            {
                action = MediaPoolAction::MarkOut;
            }
            if qnc_theme::transport_btn(ui, "[")
                .on_hover_text("Mark IN")
                .clicked()
            {
                action = MediaPoolAction::MarkIn;
            }
            let play = if input.playing { "||" } else { ">" };
            if qnc_theme::transport_btn(ui, play)
                .on_hover_text("Play / Pause")
                .clicked()
            {
                action = MediaPoolAction::TogglePlay;
            }
        });
    });
    action
}

pub(crate) fn show_strip(ui: &mut egui::Ui, input: MediaPoolStripInput<'_>) -> MediaPoolAction {
    let mut action = MediaPoolAction::None;
    // Same body chrome as Ingest strip / dir browser — `content_panel`, not a local Frame.
    qnc_ui::content_panel(ui, input.height, |ui| {
        action = if input.library_tab == LibraryTab::Segment {
            segment_cards(ui, &input)
        } else {
            shot_cards(ui, &input)
        };
    });
    action
}

/// Ingest right panel — same `content_panel` + `show_card_grid` as Story/MA strip.
pub(crate) fn show_ingest_strip(
    ui: &mut egui::Ui,
    height: f32,
    selected_clip_id: &str,
    clips: &[IngestClip],
    thumb_textures: &HashMap<String, TextureHandle>,
    tc: &dyn Fn(f64) -> String,
) -> MediaPoolAction {
    let mut action = MediaPoolAction::None;
    let rows: Vec<MediaPoolCardRow<'_>> = clips
        .iter()
        .map(|c| MediaPoolCardRow {
            id: c.clip_id.as_str(),
            thumb_id: c.clip_id.as_str(),
            title: if !c.name.is_empty() {
                c.name.as_str()
            } else {
                c.clip_id.as_str()
            },
            duration_sec: c.duration_sec,
            duration_label: "",
            import_status: c.import_status.as_str(),
            status_proxy: c.status_proxy.as_str(),
            status_original: c.status_original.as_str(),
            checked: c.selected,
        })
        .collect();

    qnc_ui::content_panel(ui, height, |ui| {
        if let Some(grid_action) = show_card_grid(
            ui,
            &MediaPoolCardGridInput {
                height: ui.available_height().max(0.0),
                selected_id: selected_clip_id,
                focused_id: selected_clip_id,
                panel_focused: false,
                cards: &rows,
                thumb_textures,
                tc,
                card_features: qnc_media_card::MediaCardFeatures::INGEST,
                empty_message: "Nema klipova — lijevo odaberi mapu → U redu.",
                id_salt: "ingest_media_grid",
            },
        ) {
            action = match grid_action {
                MediaPoolCardGridAction::Activate(id) => MediaPoolAction::SelectClipId(id),
                MediaPoolCardGridAction::ToggleSelection(id) => {
                    MediaPoolAction::ToggleClipSelection(id)
                }
            };
        }
    });
    action
}

/// Shared virtualized card grid — Story/MA strip and Ingest strip both use this.
pub(crate) fn show_card_grid(
    ui: &mut egui::Ui,
    input: &MediaPoolCardGridInput<'_>,
) -> Option<MediaPoolCardGridAction> {
    let mut clicked: Option<MediaPoolCardGridAction> = None;
    let cards = input.cards;
    let t = current(ui);
    let muted = t.muted;
    let available_w = ui.available_width().max(qnc_media_card::MIN_CARD_W);
    // Prefer live available height inside `content_panel`; fall back to input.
    let available_h = ui
        .available_height()
        .min(if input.height > 0.0 {
            input.height
        } else {
            f32::MAX
        })
        .max(qnc_media_card::MIN_CARD_H);
    let metrics = qnc_media_card::grid_metrics(available_w, cards.len());

    egui::ScrollArea::vertical()
        .id_salt(input.id_salt)
        .auto_shrink([false, false])
        .max_height(available_h)
        .show_viewport(ui, |ui, viewport| {
            ui.set_min_width(available_w);
            if cards.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.colored_label(muted, input.empty_message);
                });
                return;
            }

            let total_rows = cards.len().div_ceil(metrics.cols);
            let row_stride = metrics.card_h + metrics.gap;
            let first_row = (viewport.top() / row_stride).floor().max(0.0) as usize;
            let last_row = ((viewport.bottom() / row_stride).ceil() as usize + 1).min(total_rows);
            let focus_id = if input.panel_focused && !input.focused_id.trim().is_empty() {
                input.focused_id
            } else {
                input.selected_id
            };

            ui.add_space(first_row as f32 * row_stride);
            for row_idx in first_row..last_row {
                let start = row_idx * metrics.cols;
                let end = (start + metrics.cols).min(cards.len());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = metrics.gap;
                    for card in &cards[start..end] {
                        let focused = card.id == focus_id || card.thumb_id == focus_id;
                        let thumb = input.thumb_textures.get(card.thumb_id).cloned();
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(metrics.card_w, metrics.card_h),
                            egui::Sense::click(),
                        );
                        qnc_media_card::paint_media_card(
                            ui,
                            rect,
                            &MediaCardInput {
                                title: card.title,
                                duration_sec: card.duration_sec,
                                duration_label: card.duration_label,
                                import_status: card.import_status,
                                status_proxy: card.status_proxy,
                                status_original: card.status_original,
                                focused,
                                checked: card.checked,
                                features: input.card_features,
                                thumb: thumb.as_ref(),
                                tc: input.tc,
                            },
                        );
                        if resp.clicked() {
                            let is_checkbox_click = input.card_features.selection_check
                                && resp.interact_pointer_pos().is_some_and(|pos| {
                                    qnc_media_card::selection_check_hit_rect(rect).contains(pos)
                                });
                            clicked = Some(if is_checkbox_click {
                                MediaPoolCardGridAction::ToggleSelection(card.id.to_string())
                            } else {
                                MediaPoolCardGridAction::Activate(card.id.to_string())
                            });
                        }
                    }
                });
                ui.add_space(metrics.gap);
            }
            let rendered_rows = last_row.saturating_sub(first_row);
            let remaining_rows = total_rows.saturating_sub(first_row + rendered_rows);
            ui.add_space(remaining_rows as f32 * row_stride);
        });

    clicked
}

fn shot_cards(ui: &mut egui::Ui, input: &MediaPoolStripInput<'_>) -> MediaPoolAction {
    let shots: &[StoryShot] = match input.library_tab {
        LibraryTab::All => input.all_clips,
        LibraryTab::Virtual => input.virtual_shots,
        LibraryTab::Segment => &[],
    };

    let empty_message = match input.library_tab {
        LibraryTab::All => "Nema klipova — prvo Ingest import.",
        LibraryTab::Virtual => "Nema virtualnih — Spremi virtualni kadar.",
        LibraryTab::Segment => "",
    };

    let ids: Vec<String> = shots.iter().map(shot_id).collect();
    let rows: Vec<MediaPoolCardRow<'_>> = shots
        .iter()
        .enumerate()
        .map(|(i, shot)| {
            let sid = ids[i].as_str();
            let focused = sid == input.selected_shot_id
                || (!shot.clip_id.is_empty() && shot.clip_id == input.selected_clip_id);
            let title = if !shot.name.is_empty() {
                shot.name.as_str()
            } else if !shot.virtual_name.is_empty() {
                shot.virtual_name.as_str()
            } else {
                shot.clip_id.as_str()
            };
            MediaPoolCardRow {
                id: sid,
                thumb_id: if !shot.clip_id.is_empty() {
                    shot.clip_id.as_str()
                } else {
                    sid
                },
                title,
                duration_sec: shot.duration_sec,
                duration_label: shot.duration_label.as_str(),
                import_status: shot.import_status.as_str(),
                status_proxy: shot.status_proxy.as_str(),
                status_original: shot.status_original.as_str(),
                checked: focused,
            }
        })
        .collect();

    let mut action = MediaPoolAction::None;
    if let Some(grid_action) = show_card_grid(
        ui,
        &MediaPoolCardGridInput {
            height: ui.available_height().max(0.0),
            selected_id: input.selected_shot_id,
            focused_id: input.focused_shot_id,
            panel_focused: input.panel_focused,
            cards: &rows,
            thumb_textures: input.thumb_textures,
            tc: input.tc,
            card_features: input.card_features,
            empty_message,
            id_salt: "story_media_grid",
        },
    ) {
        let (id, checkbox_click) = match &grid_action {
            MediaPoolCardGridAction::Activate(id) => (id.as_str(), false),
            MediaPoolCardGridAction::ToggleSelection(id) => (id.as_str(), true),
        };
        if let Some(shot) = shots.iter().find(|s| shot_id(s) == id) {
            action = if checkbox_click {
                MediaPoolAction::ToggleShotSelection(shot.clone())
            } else {
                MediaPoolAction::SelectShot(shot.clone())
            };
        }
    }
    action
}

fn segment_cards(ui: &mut egui::Ui, input: &MediaPoolStripInput<'_>) -> MediaPoolAction {
    let mut action = MediaPoolAction::None;
    let t = current(ui);
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal(|ui| {
            if input.parts.is_empty() {
                ui.colored_label(
                    t.muted,
                    "Nema segmenata — označi source IN/OUT pa dodaj TON/OFF.",
                );
                return;
            }

            for part in input.parts {
                let selected = part.part_id == input.selected_part_id;
                let stroke = if selected {
                    egui::Stroke::new(2.0, t.select_red)
                } else {
                    egui::Stroke::new(1.0, t.border)
                };
                let resp = egui::Frame::NONE
                    .stroke(stroke)
                    .fill(t.raised)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_min_width(120.0);
                        ui.label(RichText::new(&part.kind).color(t.accent).strong());
                        ui.label(RichText::new(&part.part_id).color(t.text).small());
                        if !part.duration_label.is_empty() {
                            ui.label(RichText::new(&part.duration_label).color(t.muted).small());
                        }
                        if selected {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::Button::new(RichText::new("Up").small()))
                                    .on_hover_text("Pomakni segment ranije")
                                    .clicked()
                                {
                                    action = MediaPoolAction::ReorderPart {
                                        part_id: part.part_id.clone(),
                                        direction: "up".into(),
                                    };
                                }
                                if ui
                                    .add(egui::Button::new(RichText::new("Down").small()))
                                    .on_hover_text("Pomakni segment kasnije")
                                    .clicked()
                                {
                                    action = MediaPoolAction::ReorderPart {
                                        part_id: part.part_id.clone(),
                                        direction: "down".into(),
                                    };
                                }
                                if ui
                                    .add(egui::Button::new(RichText::new("Del").small()))
                                    .on_hover_text("Obriši segment")
                                    .clicked()
                                {
                                    action = MediaPoolAction::DeletePart(part.part_id.clone());
                                }
                            });
                        }
                    })
                    .response;
                if resp.clicked() && matches!(action, MediaPoolAction::None) {
                    action = MediaPoolAction::SelectPart(part.part_id.clone());
                }
            }
        });
    });
    action
}
