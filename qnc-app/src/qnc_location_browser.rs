//! `qnc_location_browser` — neutral location/source browser component.
//!
//! This is a standalone UI component based on the Ingest directory browser
//! pattern. It does not know which form uses it. The caller provides a
//! filesystem/source snapshot and maps the returned action to its own workflow.

use eframe::egui::{self, RichText, Vec2};

use crate::api::FsEntry;
use crate::qnc_theme::{self, MUTED, TEXT};
use crate::qnc_ui;

const UP_COL_W: f32 = 78.0;
const DISKS_COL_W: f32 = 76.0;
const TREE_INDENT_W: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationSourceKind {
    #[default]
    Local,
    Lan,
    Internet,
}

impl LocationSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Računalo",
            Self::Lan => "LAN",
            Self::Internet => "Internet",
        }
    }
}

pub struct LocationBrowserInput<'a> {
    pub id_salt: &'a str,
    pub kind: LocationSourceKind,
    pub roots: bool,
    pub path: &'a str,
    pub parent: Option<&'a str>,
    pub entries: &'a [FsEntry],
    pub error: Option<&'a str>,
    pub busy: bool,
    pub confirm_label: &'a str,
    pub max_tree_height: Option<f32>,
}

pub enum LocationBrowserAction {
    None,
    SelectKind(LocationSourceKind),
    OpenPath(String),
    Confirm,
    Cancel,
}

pub fn show(ui: &mut egui::Ui, input: LocationBrowserInput<'_>) -> LocationBrowserAction {
    let mut action = LocationBrowserAction::None;

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Izvori")
                .size(qnc_theme::FONT_UI)
                .color(MUTED),
        );
        ui.add_space(12.0);
        for kind in [
            LocationSourceKind::Local,
            LocationSourceKind::Lan,
            LocationSourceKind::Internet,
        ] {
            let selected = input.kind == kind;
            if qnc_theme::link_tab(ui, kind.label(), selected).clicked() && !selected {
                action = LocationBrowserAction::SelectKind(kind);
            }
            ui.add_space(10.0);
        }
    });

    ui.add_space(qnc_ui::space::GAP);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let can_up = matches!(input.kind, LocationSourceKind::Local)
            && !input.roots
            && input.parent.is_some();
        if fixed_text_link(ui, "↑ Gore", can_up, UP_COL_W).clicked() {
            action = LocationBrowserAction::OpenPath(input.parent.unwrap_or("").to_string());
        }
        let can_disks = matches!(input.kind, LocationSourceKind::Local);
        if fixed_text_link(ui, "Diskovi", can_disks, DISKS_COL_W).clicked() {
            action = LocationBrowserAction::OpenPath(String::new());
        }
        show_location_breadcrumb(ui, &input, &mut action);
    });

    if let Some(err) = input.error {
        ui.add_space(4.0);
        ui.colored_label(egui::Color32::from_rgb(220, 100, 80), err);
    }

    ui.add_space(6.0);
    let footer_h = qnc_theme::CHROME_CTRL_H + 8.0;
    let available_tree_h = (ui.available_height() - footer_h).max(40.0);
    let tree_h = input
        .max_tree_height
        .map(|max_h| available_tree_h.min(max_h).max(40.0))
        .unwrap_or(available_tree_h);
    egui::ScrollArea::vertical()
        .id_salt(format!("{}_location_browser", input.id_salt))
        .max_height(tree_h)
        .min_scrolled_height(tree_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match input.kind {
                LocationSourceKind::Local => {
                    show_local_tree(ui, &input, &mut action);
                }
                LocationSourceKind::Lan => {
                    ui.label(
                        RichText::new("Nema konfiguriranih LAN izvora.")
                            .size(qnc_theme::FONT_UI)
                            .color(MUTED),
                    );
                }
                LocationSourceKind::Internet => {
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
        let can_confirm = matches!(input.kind, LocationSourceKind::Local)
            && !input.roots
            && !input.path.trim().is_empty()
            && !input.busy;
        ui.add_enabled_ui(can_confirm, |ui| {
            if qnc_theme::primary_btn(ui, input.confirm_label).clicked() {
                action = LocationBrowserAction::Confirm;
            }
        });
        if qnc_theme::action_btn(ui, "Odustani").clicked() {
            action = LocationBrowserAction::Cancel;
        }
    });

    action
}

fn fixed_text_link(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::new(width, qnc_theme::CHROME_CTRL_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| qnc_theme::text_link(ui, label, enabled),
    )
    .inner
}

fn show_local_tree(
    ui: &mut egui::Ui,
    input: &LocationBrowserInput<'_>,
    action: &mut LocationBrowserAction,
) {
    if input.entries.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(if input.roots {
                UP_COL_W
            } else {
                UP_COL_W + DISKS_COL_W
            });
            ui.label(
                RichText::new(if input.roots {
                    "Nema diskova."
                } else {
                    "Nema podmapa."
                })
                .size(qnc_theme::FONT_UI)
                .color(MUTED),
            );
        });
        return;
    }

    if input.roots {
        for entry in input.entries {
            if location_tree_row(ui, UP_COL_W, "💾", &entry_display_name(entry, true)) {
                *action = LocationBrowserAction::OpenPath(clean_location_path(&entry.path));
            }
        }
        return;
    }

    ui.horizontal(|ui| {
        ui.add_space(UP_COL_W + DISKS_COL_W);
        ui.label(
            RichText::new("▾")
                .monospace()
                .size(qnc_theme::FONT_UI)
                .color(MUTED),
        );
        ui.label(
            RichText::new(path_leaf(input.path))
                .size(qnc_theme::FONT_UI)
                .color(TEXT),
        );
    });
    for entry in input.entries {
        if location_tree_row(
            ui,
            UP_COL_W + DISKS_COL_W + TREE_INDENT_W,
            "📁",
            &entry_display_name(entry, false),
        ) {
            *action = LocationBrowserAction::OpenPath(clean_location_path(&entry.path));
        }
    }
}

fn location_tree_row(ui: &mut egui::Ui, offset: f32, icon: &str, label: &str) -> bool {
    ui.horizontal(|ui| {
        ui.add_space(offset);
        qnc_theme::text_link(ui, &format!("{icon}  {label}"), true).clicked()
    })
    .inner
}

fn entry_display_name(entry: &FsEntry, roots: bool) -> String {
    if !entry.name.trim().is_empty() {
        return clean_location_path(&entry.name);
    }
    if roots {
        clean_location_path(&entry.path)
    } else {
        path_leaf(&entry.path)
    }
}

fn path_leaf(path: &str) -> String {
    let clean = clean_location_path(path);
    let trimmed = clean.trim_end_matches(|c| c == '\\' || c == '/');
    trimmed
        .rsplit(|c| c == '\\' || c == '/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

pub fn clean_location_path(path: &str) -> String {
    let p = path.trim();
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else if let Some(rest) = p.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = p.strip_prefix("//?/") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

fn location_label(input: &LocationBrowserInput<'_>) -> String {
    match input.kind {
        LocationSourceKind::Local if input.roots => "Računalo".into(),
        LocationSourceKind::Local if !input.path.trim().is_empty() => short_path(input.path),
        LocationSourceKind::Lan => "LAN".into(),
        LocationSourceKind::Internet => "Internet".into(),
        _ => "—".into(),
    }
}

fn show_location_breadcrumb(
    ui: &mut egui::Ui,
    input: &LocationBrowserInput<'_>,
    action: &mut LocationBrowserAction,
) {
    if !matches!(input.kind, LocationSourceKind::Local) || input.roots {
        ui.label(
            RichText::new(location_label(input))
                .monospace()
                .size(qnc_theme::FONT_UI)
                .color(TEXT),
        );
        return;
    }

    let parts = breadcrumb_parts(input.path);
    if parts.is_empty() {
        ui.label(
            RichText::new(short_path(input.path))
                .monospace()
                .size(qnc_theme::FONT_UI)
                .color(TEXT),
        );
        return;
    }

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, (label, path)) in parts.iter().enumerate() {
            if index > 0 {
                ui.label(
                    RichText::new("\\")
                        .monospace()
                        .size(qnc_theme::FONT_UI)
                        .color(MUTED),
                );
            }
            if qnc_theme::text_link(ui, label, true).clicked() {
                *action = LocationBrowserAction::OpenPath(path.clone());
            }
        }
    });
}

fn breadcrumb_parts(path: &str) -> Vec<(String, String)> {
    let clean = clean_location_path(path);
    if clean.is_empty() {
        return Vec::new();
    }

    if is_windows_drive_rooted(&clean) {
        let drive = clean[..2].to_string();
        let mut out = vec![(drive.clone(), format!("{drive}\\"))];
        let rest = clean[3..].trim_matches(|c| c == '\\' || c == '/');
        let mut current = format!("{drive}\\");
        for part in rest
            .split(|c| c == '\\' || c == '/')
            .filter(|p| !p.is_empty())
        {
            if !current.ends_with('\\') {
                current.push('\\');
            }
            current.push_str(part);
            out.push((part.to_string(), current.clone()));
        }
        return out;
    }

    if clean.starts_with("\\\\") {
        let mut out = Vec::new();
        let mut current = String::from("\\\\");
        for part in clean
            .trim_start_matches('\\')
            .split('\\')
            .filter(|p| !p.is_empty())
        {
            if current != "\\\\" {
                current.push('\\');
            }
            current.push_str(part);
            out.push((part.to_string(), current.clone()));
        }
        return out;
    }

    if clean.starts_with('/') {
        let mut out = vec![("/".to_string(), "/".to_string())];
        let mut current = String::from("/");
        for part in clean
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
        {
            if !current.ends_with('/') {
                current.push('/');
            }
            current.push_str(part);
            out.push((part.to_string(), current.clone()));
        }
        return out;
    }

    let mut out = Vec::new();
    let mut current = String::new();
    for part in clean
        .split(|c| c == '\\' || c == '/')
        .filter(|p| !p.is_empty())
    {
        if !current.is_empty() {
            current.push('\\');
        }
        current.push_str(part);
        out.push((part.to_string(), current.clone()));
    }
    out
}

fn is_windows_drive_rooted(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn short_path(path: &str) -> String {
    let p = clean_location_path(path);
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
