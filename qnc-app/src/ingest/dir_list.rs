//! `dir_list` **component** — Ingest disk/folder browser (left media body).
//!
//! Pure paint + hit-test. Orchestrator ([`super`]) maps
//! [`DirListAction`] → `fs_list` / ingest browse.

use eframe::egui::{self, RichText};

use crate::api::FsEntry;
use crate::qnc_theme::{self, MUTED, TEXT};
use crate::qnc_ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirBrowserKind {
    #[default]
    Local,
    Lan,
    Internet,
}

impl DirBrowserKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Računalo",
            Self::Lan => "LAN",
            Self::Internet => "Internet",
        }
    }
}

pub struct DirListInput<'a> {
    pub kind: DirBrowserKind,
    pub roots: bool,
    pub path: &'a str,
    pub parent: Option<&'a str>,
    pub entries: &'a [FsEntry],
    pub error: Option<&'a str>,
    pub busy: bool,
}

pub enum DirListAction {
    None,
    /// Switch Izvori tab (Local / LAN / Internet).
    SelectKind(DirBrowserKind),
    /// Enter folder or jump to roots (`path` empty).
    OpenPath(String),
    Confirm,
    Cancel,
}

pub fn show(ui: &mut egui::Ui, input: DirListInput<'_>) -> DirListAction {
    let mut action = DirListAction::None;

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Izvori")
                .size(qnc_theme::FONT_UI)
                .color(MUTED),
        );
        ui.add_space(12.0);
        for kind in [
            DirBrowserKind::Local,
            DirBrowserKind::Lan,
            DirBrowserKind::Internet,
        ] {
            let selected = input.kind == kind;
            if qnc_theme::link_tab(ui, kind.label(), selected).clicked() && input.kind != kind {
                action = DirListAction::SelectKind(kind);
            }
            ui.add_space(10.0);
        }
    });

    ui.add_space(qnc_ui::space::GAP);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let can_up =
            matches!(input.kind, DirBrowserKind::Local) && !input.roots && input.parent.is_some();
        if qnc_theme::text_link(ui, "↑ Gore", can_up).clicked() {
            action = DirListAction::OpenPath(input.parent.unwrap_or("").to_string());
        }
        let can_disks = matches!(input.kind, DirBrowserKind::Local);
        if qnc_theme::text_link(ui, "Diskovi", can_disks).clicked() {
            action = DirListAction::OpenPath(String::new());
        }
        let label = match input.kind {
            DirBrowserKind::Local if input.roots => "Računalo".into(),
            DirBrowserKind::Local if !input.path.is_empty() => short_path(input.path),
            DirBrowserKind::Lan => "LAN".into(),
            DirBrowserKind::Internet => "Internet".into(),
            _ => "—".into(),
        };
        ui.label(
            RichText::new(label)
                .monospace()
                .size(qnc_theme::FONT_UI)
                .color(TEXT),
        );
    });

    if let Some(err) = input.error {
        ui.add_space(4.0);
        ui.colored_label(egui::Color32::from_rgb(220, 100, 80), err);
    }

    ui.add_space(6.0);
    let footer_h = qnc_theme::CHROME_CTRL_H + 8.0;
    let tree_h = (ui.available_height() - footer_h).max(40.0);
    egui::ScrollArea::vertical()
        .id_salt("ingest_dir_list")
        .max_height(tree_h)
        .min_scrolled_height(tree_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match input.kind {
                DirBrowserKind::Local => {
                    if input.entries.is_empty() {
                        ui.label(
                            RichText::new(if input.roots {
                                "Nema diskova."
                            } else {
                                "Nema podmapa."
                            })
                            .size(qnc_theme::FONT_UI)
                            .color(MUTED),
                        );
                    }
                    for e in input.entries {
                        let name = if e.name.is_empty() {
                            e.path.as_str()
                        } else {
                            e.name.as_str()
                        };
                        let row = if input.roots {
                            format!("💾  {name}")
                        } else {
                            format!("📁  {name}")
                        };
                        if qnc_theme::text_link(ui, &row, true).clicked() {
                            action = DirListAction::OpenPath(e.path.clone());
                        }
                    }
                }
                DirBrowserKind::Lan => {
                    ui.label(
                        RichText::new("Nema konfiguriranih LAN izvora.")
                            .size(qnc_theme::FONT_UI)
                            .color(MUTED),
                    );
                }
                DirBrowserKind::Internet => {
                    ui.label(
                        RichText::new("Nema konfiguriranih Internet izvora.")
                            .size(qnc_theme::FONT_UI)
                            .color(MUTED),
                    );
                }
            }
        });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let can_ok = matches!(input.kind, DirBrowserKind::Local)
            && !input.roots
            && !input.path.is_empty()
            && !input.busy;
        ui.add_enabled_ui(can_ok, |ui| {
            if qnc_theme::primary_btn(ui, "U redu").clicked() {
                action = DirListAction::Confirm;
            }
        });
        if qnc_theme::action_btn(ui, "Odustani").clicked() {
            action = DirListAction::Cancel;
        }
    });

    action
}

fn short_path(path: &str) -> String {
    let mut p = path.trim().to_string();
    for prefix in ["\\\\?\\UNC\\", "\\\\?\\", "//?/"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            p = rest.to_string();
            break;
        }
    }
    if p.chars().count() <= 42 {
        return p;
    }
    let tail: String = p
        .chars()
        .rev()
        .take(36)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}
