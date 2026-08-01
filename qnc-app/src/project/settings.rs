//! `project-template-settings` — fills named [`super::layout::PtsSlot`]s.
//!
//! Does **not** invent layout. [`layout::pts_panel`] owns the board;
//! this module only dispatches each slot to a paint block.

use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use serde_json::{json, Value};

use crate::project_pts::{self, INPUT_FORMATS};
use crate::qnc_form::{
    self, PathRowAction, FIELD_GAP_X, FIELD_GAP_Y, LABEL_FS, PAD_X, PAD_Y, SECTION_GAP,
};
use crate::qnc_location_browser::{self, LocationBrowserAction, LocationBrowserInput};
use crate::qnc_theme::{self, MUTED, TEXT};

use super::ai;
use super::create;
use super::layout::{self, PtsFixedSlot, PtsScrollSlot, PtsSlot};
use super::screen::{ProjectAction, ProjectScreen};
use super::template_picker::{self, TemplatePickerAction, TemplatePickerInput};

const TITLE: Color32 = TEXT;
const LABEL: Color32 = MUTED;
/// Same contract as `project_list::PANEL_PAD` — content inset inside setting_panel.
const PANEL_PAD: f32 = 20.0;

/// Right panel: layout board + one component per named slot.
pub fn show(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    screen: &mut ProjectScreen,
) -> ProjectAction {
    let mut action = ProjectAction::None;
    let custom_open = screen.template_create_open;

    let (panel_rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(panel_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(panel_rect);
            egui::Frame::NONE
                .inner_margin(egui::Margin::same(PANEL_PAD as i8))
                .show(ui, |ui| {
                    let inner_w = ui.available_width().max(40.0);
                    let inner_h = ui.available_height().max(40.0);
                    let _metrics = layout::pts_panel(
                        ui,
                        inner_w,
                        inner_h,
                        custom_open,
                        |slot, ui, content_w| match slot {
                            PtsSlot::Head => {
                                qnc_theme::panel_title_row(ui, true, |ui| {
                                    ui.label(
                                        RichText::new("Postavke")
                                            .size(qnc_theme::FONT_UI)
                                            .strong()
                                            .color(TITLE),
                                    );
                                });
                                egui::Frame::NONE
                                    .inner_margin(egui::Margin {
                                        left: PAD_X as i8,
                                        right: PAD_X as i8,
                                        top: 0,
                                        bottom: PAD_Y as i8,
                                    })
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(
                                                "Odaberi radni tok i pregledaj postavke projekta.",
                                            )
                                            .size(12.0)
                                            .color(MUTED),
                                        );
                                    });
                            }
                            PtsSlot::Fixed(PtsFixedSlot::TemplatePicker) => {
                                ui_radni_tok_picker(screen, ui, content_w, &mut action);
                            }
                            PtsSlot::Fixed(PtsFixedSlot::ProjectCreate) => {
                                let can_create = !screen.new_name.trim().is_empty()
                                    && !screen.selected_template_id.is_empty();
                                let a = create::show(
                                    ui,
                                    create::ProjectCreateInput {
                                        content_w,
                                        name: &mut screen.new_name,
                                        can_create,
                                    },
                                );
                                if !matches!(a, ProjectAction::None) {
                                    action = a;
                                }
                            }
                            PtsSlot::Fixed(PtsFixedSlot::AiSettings) => {
                                let eff = screen.effective().cloned().unwrap_or(Value::Null);
                                let a = ai::show(
                                    ui,
                                    ai::AiSettingsInput {
                                        content_w,
                                        effective: &eff,
                                    },
                                );
                                if !matches!(a, ProjectAction::None) {
                                    action = a;
                                }
                            }
                            PtsSlot::Fixed(PtsFixedSlot::ProjectsRoot) => {
                                match qnc_form::path_row(
                                    ui,
                                    content_w,
                                    "Lokacija projekata:",
                                    &mut screen.projects_root_draft,
                                ) {
                                    PathRowAction::Pick => {
                                        action = ProjectAction::ToggleProjectsRootBrowser
                                    }
                                    PathRowAction::Commit(path) => {
                                        action = ProjectAction::SetSettingsPath(
                                            "storage.projects_root".into(),
                                            Value::String(
                                                qnc_location_browser::clean_location_path(&path),
                                            ),
                                        );
                                    }
                                    PathRowAction::None => {}
                                }
                                if screen.projects_root_browser_open {
                                    ui.add_space(6.0);
                                    let browser_action = qnc_location_browser::show(
                                        ui,
                                        LocationBrowserInput {
                                            id_salt: "project_root",
                                            kind: screen.projects_root_browser_kind,
                                            roots: screen.projects_root_browser_roots,
                                            path: &screen.projects_root_browser_path,
                                            parent: screen.projects_root_browser_parent.as_deref(),
                                            entries: &screen.projects_root_browser_entries,
                                            error: screen.projects_root_browser_error.as_deref(),
                                            busy: false,
                                            confirm_label: "U redu",
                                            max_tree_height: Some(170.0),
                                        },
                                    );
                                    match browser_action {
                                        LocationBrowserAction::None => {}
                                        LocationBrowserAction::SelectKind(kind) => {
                                            action = ProjectAction::SelectProjectsRootKind(kind);
                                        }
                                        LocationBrowserAction::OpenPath(path) => {
                                            action = ProjectAction::OpenProjectsRootPath(path);
                                        }
                                        LocationBrowserAction::Confirm => {
                                            action = ProjectAction::ConfirmProjectsRootBrowser;
                                        }
                                        LocationBrowserAction::Cancel => {
                                            action = ProjectAction::CancelProjectsRootBrowser;
                                        }
                                    }
                                }
                            }
                            PtsSlot::Fixed(PtsFixedSlot::ExportDirectory) => {
                                match qnc_form::path_row(
                                    ui,
                                    content_w,
                                    "Export direktorij:",
                                    &mut screen.export_dir_draft,
                                ) {
                                    PathRowAction::Pick => {
                                        action = ProjectAction::ToggleExportDirBrowser
                                    }
                                    PathRowAction::Commit(path) => {
                                        action = ProjectAction::SetSettingsPath(
                                            "export.directory".into(),
                                            Value::String(
                                                qnc_location_browser::clean_location_path(&path),
                                            ),
                                        );
                                    }
                                    PathRowAction::None => {}
                                }
                                if screen.export_dir_browser_open {
                                    ui.add_space(6.0);
                                    let browser_action = qnc_location_browser::show(
                                        ui,
                                        LocationBrowserInput {
                                            id_salt: "export_dir",
                                            kind: screen.export_dir_browser_kind,
                                            roots: screen.export_dir_browser_roots,
                                            path: &screen.export_dir_browser_path,
                                            parent: screen.export_dir_browser_parent.as_deref(),
                                            entries: &screen.export_dir_browser_entries,
                                            error: screen.export_dir_browser_error.as_deref(),
                                            busy: false,
                                            confirm_label: "U redu",
                                            max_tree_height: Some(150.0),
                                        },
                                    );
                                    match browser_action {
                                        LocationBrowserAction::None => {}
                                        LocationBrowserAction::SelectKind(kind) => {
                                            action = ProjectAction::SelectExportDirKind(kind);
                                        }
                                        LocationBrowserAction::OpenPath(path) => {
                                            action = ProjectAction::OpenExportDirPath(path);
                                        }
                                        LocationBrowserAction::Confirm => {
                                            action = ProjectAction::ConfirmExportDirBrowser;
                                        }
                                        LocationBrowserAction::Cancel => {
                                            action = ProjectAction::CancelExportDirBrowser;
                                        }
                                    }
                                }
                            }
                            PtsSlot::Fixed(PtsFixedSlot::TemplateActions) => {
                                qnc_form::trailing_actions(ui, content_w, |ui| {
                                    if qnc_form::btn(ui, "Novi template", false).clicked() {
                                        action = ProjectAction::SetTemplateCreateOpen(true);
                                    }
                                    if screen.template_create_open
                                        && qnc_form::btn(ui, "Odustani", true).clicked()
                                    {
                                        action = ProjectAction::SetTemplateCreateOpen(false);
                                    }
                                });
                            }
                            PtsSlot::Scroll(PtsScrollSlot::Advanced) => {
                                ui_advanced(screen, ui, content_w, &mut action);
                            }
                            PtsSlot::Scroll(PtsScrollSlot::CustomTemplate) => {
                                ui_custom_template(screen, ui, content_w, &mut action);
                                let can_save = !screen.template_draft_name.trim().is_empty()
                                    && !screen.selected_template_id.is_empty();
                                qnc_form::trailing_actions(ui, content_w, |ui| {
                                    if qnc_form::btn_sized(
                                        ui,
                                        "Spremi novi template",
                                        false,
                                        can_save,
                                    )
                                    .clicked()
                                    {
                                        action = ProjectAction::SaveCustomTemplate;
                                    }
                                });
                                ui.add_space(12.0);
                            }
                        },
                    );
                });
        },
    );

    action
}

fn ui_radni_tok_picker(
    screen: &mut ProjectScreen,
    ui: &mut egui::Ui,
    content_w: f32,
    action: &mut ProjectAction,
) {
    let confirm_id = screen.confirm_delete_template.clone();
    let input = TemplatePickerInput {
        templates: &screen.templates,
        selected_id: &screen.selected_template_id,
        picker_open: screen.picker_open,
        confirm_delete_template: confirm_id.as_deref(),
        content_w,
    };
    match template_picker::show(ui, input) {
        TemplatePickerAction::None => {}
        TemplatePickerAction::ToggleOpen => screen.picker_open = !screen.picker_open,
        TemplatePickerAction::Select(id) => {
            *action = ProjectAction::SelectTemplate(id);
            screen.picker_open = false;
        }
        TemplatePickerAction::RequestDelete(id) => screen.confirm_delete_template = Some(id),
        TemplatePickerAction::ConfirmDelete(id) => {
            *action = ProjectAction::DeleteTemplate(id);
            screen.confirm_delete_template = None;
        }
        TemplatePickerAction::CancelDelete => screen.confirm_delete_template = None,
    }
}

fn ui_advanced(
    screen: &mut ProjectScreen,
    ui: &mut egui::Ui,
    col_w: f32,
    action: &mut ProjectAction,
) {
    let eff = screen.effective().cloned().unwrap_or(Value::Null);

    // Advanced — toggle + recessed groups (same as Segmenti panels).
    ui.set_max_width(col_w);
    let theme = qnc_theme::current(ui);
    let sum = egui::Frame::NONE
        .fill(theme.raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .inner_margin(egui::Margin::symmetric(8, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Advanced postavke")
                        .size(13.0)
                        .strong()
                        .color(TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(if screen.advanced_open { "▲" } else { "▼" })
                            .size(11.0)
                            .color(MUTED),
                    );
                });
            });
        });
    if sum.response.interact(Sense::click()).clicked() {
        screen.advanced_open = !screen.advanced_open;
    }
    if !screen.advanced_open {
        return;
    }

    ui.spacing_mut().item_spacing.y = SECTION_GAP;
    let inner_w = col_w;

    // —— INPUT SOURCE ——
    qnc_form::section(ui, inner_w, |ui| {
        qnc_form::group_title(ui, "Input source");
        let mut mode = ProjectScreen::path_str(&eff, &["input", "mode"]);
        if mode.is_empty() {
            mode = "auto".into();
        }
        mode = if mode.eq_ignore_ascii_case("manual") {
            "manual".into()
        } else {
            "auto".into()
        };
        let mut cells: Vec<FieldCell<'_>> = vec![
            FieldCell::Combo {
                label: "Mode",
                id: "pts_input_mode",
                value: mode.clone(),
                options: &[("auto", "AUTO"), ("manual", "Ručno")],
                on_pick: FieldPick::MergeInputMode,
            },
            FieldCell::Combo {
                label: "Ingest profil",
                id: "pts_ingest_profile",
                value: ProjectScreen::path_str(&eff, &["storage", "ingest_profile"])
                    .pipe_default("field"),
                options: project_pts::INGEST_PROFILES,
                on_pick: FieldPick::Path("storage.ingest_profile"),
            },
            FieldCell::Combo {
                label: "Proxy policy",
                id: "pts_proxy_policy",
                value: ProjectScreen::path_str(&eff, &["storage", "proxy_policy"])
                    .pipe_default("generate_if_missing"),
                options: project_pts::PROXY_POLICIES,
                on_pick: FieldPick::Path("storage.proxy_policy"),
            },
        ];
        // Manual fields painted below if needed — first paint main grid
        let main_n = cells.len();
        let _ = main_n;
        field_grid(ui, ui.available_width(), &mut cells, action);

        if mode == "manual" {
            ui.add_space(FIELD_GAP_Y);
            let mut manual: Vec<FieldCell<'_>> = vec![
                FieldCell::Combo {
                    label: "Format",
                    id: "pts_input_format",
                    value: ProjectScreen::path_str(&eff, &["input", "format"])
                        .pipe_default("HD 1080p50"),
                    options: INPUT_FORMATS,
                    on_pick: FieldPick::Path("input.format"),
                },
                FieldCell::Fps {
                    label: "Frame rate",
                    id: "pts_input_fps",
                    path: "input.fps",
                    value: project_pts::fps_display(&eff, &["input", "fps"], 50.0),
                },
                FieldCell::Int {
                    label: "Width",
                    path: "input.width",
                    value: project_pts::path_i64(&eff, &["input", "width"], 1920),
                },
                FieldCell::Int {
                    label: "Height",
                    path: "input.height",
                    value: project_pts::path_i64(&eff, &["input", "height"], 1080),
                },
                FieldCell::Combo {
                    label: "Field order",
                    id: "pts_input_field",
                    value: ProjectScreen::path_str(&eff, &["input", "field_order"])
                        .pipe_default("progressive"),
                    options: project_pts::FIELD_ORDER,
                    on_pick: FieldPick::Path("input.field_order"),
                },
                FieldCell::Combo {
                    label: "Color space",
                    id: "pts_input_cs",
                    value: ProjectScreen::path_str(&eff, &["input", "color_space"])
                        .pipe_default("rec709"),
                    options: project_pts::COLOR_SPACE,
                    on_pick: FieldPick::Path("input.color_space"),
                },
            ];
            field_grid(ui, ui.available_width(), &mut manual, action);
        }
    });

    // —— EXPORT MODE ——
    qnc_form::section(ui, inner_w, |ui| {
        qnc_form::group_title(ui, "Export");
        let export_mode = project_pts::normalize_export_mode(&ProjectScreen::path_str(
            &eff,
            &["export", "default_mode"],
        ));
        let mut cells = vec![
            FieldCell::Combo {
                label: "Mode",
                id: "pts_export_mode",
                value: export_mode,
                options: project_pts::EXPORT_MODES,
                on_pick: FieldPick::Path("export.default_mode"),
            },
            FieldCell::Combo {
                label: "Original policy",
                id: "pts_orig_policy",
                value: ProjectScreen::path_str(&eff, &["storage", "original_policy"])
                    .pipe_default("link_when_available"),
                options: project_pts::ORIGINAL_POLICIES,
                on_pick: FieldPick::Path("storage.original_policy"),
            },
        ];
        field_grid(ui, ui.available_width(), &mut cells, action);
    });

    // —— EXPORT FORMAT / PRESET ——
    qnc_form::section(ui, inner_w, |ui| {
        qnc_form::group_title(ui, "Export format");
        let mut preset = ProjectScreen::path_str(&eff, &["export", "preset"]);
        if preset.is_empty() {
            preset = "xdcam_hd422_50i".into();
        }
        let mut preset_opts: Vec<(String, String)> = project_pts::builtin_export_presets()
            .into_iter()
            .map(|(id, name, _)| (id.to_string(), name.to_string()))
            .collect();
        for (id, name, _) in project_pts::custom_export_presets(&eff) {
            preset_opts.push((id, name));
        }
        preset_opts.push(("manual".into(), "Ručno".into()));
        let preset_owned = preset_opts.clone();
        let preset_refs: Vec<(&str, &str)> = preset_owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();

        // Single-column preset select (web field-grid with one cell)
        let grid_w = ui.available_width();
        let cell_w = field_cell_width(grid_w, 1);
        let mut p = preset.clone();
        field_combo_cell(
            ui,
            cell_w,
            "Preset",
            "pts_export_preset",
            &mut p,
            &preset_refs,
            |v| *action = ProjectAction::ApplyExportPreset(v),
        );

        if preset == "manual" {
            ui.add_space(FIELD_GAP_Y);
            let mut manual: Vec<FieldCell<'_>> = vec![
                FieldCell::Combo {
                    label: "Format",
                    id: "pts_export_format",
                    value: ProjectScreen::path_str(&eff, &["export", "format"])
                        .pipe_default("HD 1080i50"),
                    options: INPUT_FORMATS,
                    on_pick: FieldPick::Path("export.format"),
                },
                FieldCell::Fps {
                    label: "Frame rate",
                    id: "pts_export_fps",
                    path: "export.fps",
                    value: project_pts::fps_display(&eff, &["export", "fps"], 25.0),
                },
                FieldCell::Int {
                    label: "Width",
                    path: "export.width",
                    value: project_pts::path_i64(&eff, &["export", "width"], 1920),
                },
                FieldCell::Int {
                    label: "Height",
                    path: "export.height",
                    value: project_pts::path_i64(&eff, &["export", "height"], 1080),
                },
                FieldCell::Combo {
                    label: "Field order",
                    id: "pts_export_field",
                    value: ProjectScreen::path_str(&eff, &["export", "field_order"])
                        .pipe_default("upper_first"),
                    options: project_pts::FIELD_ORDER,
                    on_pick: FieldPick::Path("export.field_order"),
                },
                FieldCell::Combo {
                    label: "Color space",
                    id: "pts_export_cs",
                    value: ProjectScreen::path_str(&eff, &["export", "color_space"])
                        .pipe_default("rec709"),
                    options: project_pts::COLOR_SPACE,
                    on_pick: FieldPick::Path("export.color_space"),
                },
                FieldCell::Combo {
                    label: "Container",
                    id: "pts_export_container",
                    value: ProjectScreen::path_str(&eff, &["export", "container"])
                        .pipe_default("mxf_op1a"),
                    options: project_pts::CONTAINERS,
                    on_pick: FieldPick::Path("export.container"),
                },
                FieldCell::Combo {
                    label: "Video codec",
                    id: "pts_export_codec",
                    value: ProjectScreen::path_str(&eff, &["export", "video_codec"])
                        .pipe_default("mpeg2_422_50mbit"),
                    options: project_pts::VIDEO_CODECS,
                    on_pick: FieldPick::Path("export.video_codec"),
                },
                FieldCell::Fps {
                    label: "Audio",
                    id: "pts_export_arate",
                    path: "export.audio_sample_rate",
                    value: project_pts::path_i64(&eff, &["export", "audio_sample_rate"], 48000)
                        .to_string(),
                },
                FieldCell::Fps {
                    label: "Channels",
                    id: "pts_export_ach",
                    path: "export.audio_channels",
                    value: project_pts::path_i64(&eff, &["export", "audio_channels"], 2)
                        .to_string(),
                },
            ];
            field_grid(ui, grid_w, &mut manual, action);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut screen.export_preset_draft_name)
                        .desired_width((grid_w - 160.0).max(120.0))
                        .hint_text("Naziv novog preseta"),
                );
                if qnc_form::btn_sized(
                    ui,
                    "Spremi u template",
                    false,
                    !screen.export_preset_draft_name.trim().is_empty(),
                )
                .clicked()
                {
                    *action = ProjectAction::SaveExportPreset;
                }
            });
        }
    });

    // —— WORKFLOW TABS ——
    qnc_form::section(ui, inner_w, |ui| {
        qnc_form::group_title(ui, "Plugin tabovi u workflowu");
        let tabs = eff
            .get("workspace")
            .and_then(|w| w.get("tabs"))
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let has = |id: &str| tabs.iter().any(|v| v.as_str() == Some(id));
        let module_rows: Vec<(String, String)> = if screen.modules.is_empty() {
            [
                ("ingest", "Ingest"),
                ("media_assist", "Media Assist"),
                ("storyboard", "Story"),
            ]
            .into_iter()
            .map(|(a, b)| (a.into(), b.into()))
            .collect()
        } else {
            screen
                .modules
                .iter()
                .filter(|m| {
                    let key = m.tab_key();
                    key != "project" && m.enabled.unwrap_or(true)
                })
                .map(|m| (m.tab_key().to_string(), m.display_label().to_string()))
                .collect()
        };
        ui.spacing_mut().item_spacing.y = 6.0;
        for (id, label) in module_rows {
            let mut on = has(&id);
            if ui
                .checkbox(&mut on, RichText::new(label).size(13.0).color(TEXT))
                .changed()
            {
                *action = ProjectAction::ToggleWorkflowTab(id, on);
            }
        }
    });
}

fn ui_custom_template(
    screen: &mut ProjectScreen,
    ui: &mut egui::Ui,
    col_w: f32,
    action: &mut ProjectAction,
) {
    let eff = screen.effective().cloned().unwrap_or(Value::Null);
    qnc_form::section(ui, col_w, |ui| {
        qnc_form::group_title(ui, "Novi template");
        let base = screen
            .templates
            .iter()
            .find(|t| t.template_id == screen.selected_template_id)
            .map(|t| t.name.as_str())
            .unwrap_or("—");
        ui.label(
            RichText::new(format!("Baza: {base}"))
                .size(12.0)
                .color(MUTED),
        );
        ui.add_space(8.0);
        let w = (ui.available_width()).max(80.0);
        ui.add(
            egui::TextEdit::singleline(&mut screen.template_draft_name)
                .desired_width(w)
                .hint_text("Naziv novog templatea"),
        );
        ui.add(
            egui::TextEdit::multiline(&mut screen.template_draft_description)
                .desired_width(w)
                .desired_rows(2)
                .hint_text("Kratki opis"),
        );
        ui.add_space(6.0);
        let mut kbd = ProjectScreen::path_str(&eff, &["keyboard_shortcuts", "active_preset"]);
        if kbd.is_empty() {
            kbd = "default".into();
        }
        let presets = if screen.keyboard_presets.is_empty() {
            vec![
                ("default".into(), "QNC".into()),
                ("resolve".into(), "DaVinci Resolve".into()),
                ("premiere".into(), "Adobe Premiere Pro".into()),
                ("finalcut".into(), "Final Cut Pro 11".into()),
                ("edius".into(), "Grass Valley EDIUS".into()),
                ("avid".into(), "Avid Media Composer".into()),
            ]
        } else {
            screen
                .keyboard_presets
                .iter()
                .map(|p| {
                    (
                        p.id.clone(),
                        if p.name.is_empty() {
                            p.id.clone()
                        } else {
                            p.name.clone()
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let preset_refs: Vec<(&str, &str)> = presets
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let cell_w = field_cell_width(w, 1);
        field_combo_cell(
            ui,
            cell_w,
            "Tipkovnica",
            "pts_kbd_preset",
            &mut kbd,
            &preset_refs,
            |v| {
                *action = ProjectAction::SetSettingsPath(
                    "keyboard_shortcuts.active_preset".into(),
                    Value::String(v),
                );
            },
        );
    });
}

trait PipeDefault {
    fn pipe_default(self, default: &str) -> String;
}

impl PipeDefault for String {
    fn pipe_default(self, default: &str) -> String {
        if self.is_empty() {
            default.into()
        } else {
            self
        }
    }
}

#[derive(Clone, Copy)]
enum FieldPick {
    Path(&'static str),
    MergeInputMode,
}

enum FieldCell<'a> {
    Combo {
        label: &'a str,
        id: &'a str,
        value: String,
        options: &'a [(&'a str, &'a str)],
        on_pick: FieldPick,
    },
    Fps {
        label: &'a str,
        id: &'a str,
        path: &'a str,
        value: String,
    },
    Int {
        label: &'a str,
        path: &'a str,
        value: i64,
    },
}

fn field_cell_width(grid_w: f32, cols: usize) -> f32 {
    qnc_form::field_cell_width(grid_w, cols)
}

fn field_grid_cols(grid_w: f32) -> usize {
    qnc_form::field_grid_cols(grid_w)
}

fn field_grid(
    ui: &mut egui::Ui,
    grid_w: f32,
    cells: &mut [FieldCell<'_>],
    action: &mut ProjectAction,
) {
    let cols = field_grid_cols(grid_w);
    let cell_w = field_cell_width(grid_w, cols);
    let mut i = 0;
    while i < cells.len() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = FIELD_GAP_X;
            for _ in 0..cols {
                if i >= cells.len() {
                    break;
                }
                match &mut cells[i] {
                    FieldCell::Combo {
                        label,
                        id,
                        value,
                        options,
                        on_pick,
                    } => {
                        let pick = on_pick;
                        field_combo_cell(ui, cell_w, label, id, value, options, |v| match pick {
                            FieldPick::Path(path) => {
                                *action = ProjectAction::SetSettingsPath(
                                    (*path).into(),
                                    Value::String(v),
                                );
                            }
                            FieldPick::MergeInputMode => {
                                *action = ProjectAction::MergeSettingsOverride(json!({
                                    "input": { "mode": v }
                                }));
                            }
                        });
                    }
                    FieldCell::Fps {
                        label,
                        id,
                        path,
                        value,
                    } => {
                        field_fps_cell(ui, cell_w, label, id, path, value, action);
                    }
                    FieldCell::Int { label, path, value } => {
                        field_int_cell(ui, cell_w, label, path, *value, action);
                    }
                }
                i += 1;
            }
        });
        ui.add_space(FIELD_GAP_Y);
    }
}

/// Web fieldSelect: label above, full-width control.
fn field_combo_cell(
    ui: &mut egui::Ui,
    cell_w: f32,
    label: &str,
    id: &str,
    value: &mut String,
    options: &[(&str, &str)],
    on_change: impl FnOnce(String),
) {
    let before = value.clone();
    let display = options
        .iter()
        .find(|(v, _)| *v == value.as_str())
        .map(|(_, l)| (*l).to_string())
        .unwrap_or_else(|| value.clone());
    ui.allocate_ui_with_layout(
        Vec2::new(cell_w, 52.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(cell_w);
            ui.label(RichText::new(label).size(LABEL_FS).color(LABEL));
            ui.add_space(6.0);
            egui::ComboBox::from_id_salt(id)
                .selected_text(display)
                .width(cell_w)
                .show_ui(ui, |ui| {
                    for (v, l) in options {
                        ui.selectable_value(value, (*v).to_string(), *l);
                    }
                });
        },
    );
    if *value != before {
        on_change(value.clone());
    }
}

fn field_fps_cell(
    ui: &mut egui::Ui,
    cell_w: f32,
    label: &str,
    id: &str,
    path: &str,
    value: &mut String,
    action: &mut ProjectAction,
) {
    let opts: Vec<(&str, &str)> = if path.contains("audio_sample_rate") {
        project_pts::AUDIO_RATES.iter().map(|o| (*o, *o)).collect()
    } else if path.contains("audio_channels") {
        project_pts::AUDIO_CHANNELS
            .iter()
            .map(|o| (*o, *o))
            .collect()
    } else {
        project_pts::FPS_OPTIONS.iter().map(|o| (*o, *o)).collect()
    };
    let before = value.clone();
    field_combo_cell(ui, cell_w, label, id, value, &opts, |_| {});
    if *value != before {
        if let Ok(n) = value.parse::<f64>() {
            if path.contains("audio") || n.fract().abs() < f64::EPSILON {
                *action = ProjectAction::SetSettingsPath(path.into(), json!(n.round() as i64));
            } else {
                *action = ProjectAction::SetSettingsPath(path.into(), json!(n));
            }
        }
    }
}

fn field_int_cell(
    ui: &mut egui::Ui,
    cell_w: f32,
    label: &str,
    path: &str,
    current: i64,
    action: &mut ProjectAction,
) {
    let mut text = current.to_string();
    let before = text.clone();
    ui.allocate_ui_with_layout(
        Vec2::new(cell_w, 52.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(cell_w);
            ui.label(RichText::new(label).size(LABEL_FS).color(LABEL));
            ui.add_space(6.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(cell_w)
                    .hint_text("0"),
            );
            if resp.lost_focus() && text != before {
                if let Ok(n) = text.parse::<i64>() {
                    *action = ProjectAction::SetSettingsPath(path.into(), json!(n));
                }
            }
        },
    );
}
