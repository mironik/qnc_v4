//! Universal source dock chrome around standalone `QncTimeline` progress view.

use eframe::egui::{self, RichText};

use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_theme::{self, MUTED, TEXT};
use crate::qnc_timeline::{ExpandedAudio, LayerFlags, QncTimeline, TimelineFocusPaint};
use crate::qnc_timeline_progress::{
    self, TimelineProgressInput, TimelineProgressIntent, TimelineProgressModel,
};

pub struct SourceDockInput<'a> {
    pub clip_label: &'a str,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    /// Frame-based paint model — from [`crate::playback_stack::PlaybackStack`], not form seconds.
    pub timeline_model: TimelineProgressModel,
    pub focus: TimelineFocusPaint,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub frames: &'a [FilmFrame],
    pub tc_frame: &'a dyn Fn(i64) -> String,
    /// Clip name + IN/OUT row above the timeline.
    pub show_header: bool,
    /// Edit buttons (virtual / parts).
    pub show_edit_actions: bool,
    /// Import / selection actions.
    pub show_import_actions: bool,
    pub archive_original: bool,
    pub ai_mining: bool,
    pub import_enabled: bool,
    /// Number of imported clips that need explicit poster generation consent.
    pub proxy_poster_approval_count: usize,
    /// e.g. "8 uvezeno · 10/81" — shown in ingest header (right side).
    pub ingest_status: &'a str,
    /// Which A1/A2 lane is expanded (web kodak toggle).
    pub expanded_audio: ExpandedAudio,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceDockAction {
    None,
    CueFrame(i64),
    /// Toggle expand for A1 or A2 (same lane collapses).
    ToggleAudioExpand(ExpandedAudio),
    SaveVirtualShot,
    CreatePart(&'static str),
    CreateCover,
    ImportSelected,
    SelectAll,
    ClearSelection,
    Reload,
    SetArchive(bool),
    SetAiMining(bool),
    ApproveProxyPosters,
}

/// Horizontal inset shared by workspace columns and the bottom timeline dock.
pub const SHELL_MARGIN_X: i8 = 8;
const HEADER_TIMELINE_GAP_H: f32 = 4.0;

/// Owner layer mask for source dock (presets later — not a second timeline type).
fn source_layers() -> LayerFlags {
    LayerFlags {
        carrier: true,
        audio_a1: true,
        audio_a2: true,
        audio_a3: false,
        audio_a4: false,
        base_video: false,
        shot_range: true,
        covers: false,
        markers: false,
        marker_slots: false,
        in_out: true,
        playhead: true,
    }
}

/// Exact dock height so timeline fits fully; upper CentralPanel flex-shrinks.
pub fn dock_height(expanded_audio: ExpandedAudio, show_header: bool) -> f32 {
    let track = QncTimeline {
        layers: source_layers(),
        duration_frames: 1,
        playhead_frame: 0,
        shot_in_frame: 0,
        shot_out_frame: 1,
        draft_in_frame: 0,
        draft_out_frame: 1,
        video_background: None,
        focus: TimelineFocusPaint::Playhead,
        show_lane_labels: true,
        expanded_audio,
        a1_peaks: &[],
        a2_peaks: &[],
        a3_peaks: &[],
        a4_peaks: &[],
        virtual_spans: &[],
        covers: &[],
        marker_slots: &[],
        markers: &[],
        base_video_blank: false,
    }
    .content_height()
        + 2.0; // Frame stroke outside content
    if show_header {
        track + qnc_theme::CHROME_ROW_H + HEADER_TIMELINE_GAP_H
    } else {
        track
    }
}

pub fn show(ui: &mut egui::Ui, input: SourceDockInput<'_>) -> SourceDockAction {
    let mut action = SourceDockAction::None;
    let mut archive = input.archive_original;
    let mut ai_mining = input.ai_mining;
    let tl_colors = qnc_theme::current(ui).timeline();
    let source_in_frame = input.source_in_frame.max(0);
    let source_out_frame = input.source_out_frame.max(source_in_frame);
    let source_duration_frames = (source_out_frame - source_in_frame).max(0);

    egui::Frame::NONE
        .fill(tl_colors.bg)
        .inner_margin(egui::Margin {
            left: SHELL_MARGIN_X,
            right: SHELL_MARGIN_X,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if input.show_header {
                qnc_theme::chrome_row(ui, true, |ui| {
                    if input.show_edit_actions {
                        // RTL first: action buttons keep hit-targets; labels take leftover.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if qnc_theme::action_btn(ui, "Pokrivalice").clicked() {
                                action = SourceDockAction::CreateCover;
                            }
                            if qnc_theme::action_btn(ui, "Voice over").clicked() {
                                action = SourceDockAction::CreatePart("offovi");
                            }
                            if qnc_theme::action_btn(ui, "Talking Head").clicked() {
                                action = SourceDockAction::CreatePart("tonovi");
                            }
                            if qnc_theme::action_btn(ui, "Add virtual clip").clicked() {
                                action = SourceDockAction::SaveVirtualShot;
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(input.clip_label)
                                            .color(TEXT)
                                            .strong()
                                            .size(qnc_theme::FONT_UI),
                                    );
                                    ui.add_space(10.0);
                                    timecode_label(
                                        ui,
                                        "IN",
                                        (input.tc_frame)(source_in_frame),
                                        input.focus == TimelineFocusPaint::In,
                                    );
                                    timecode_label(
                                        ui,
                                        "OUT",
                                        (input.tc_frame)(source_out_frame),
                                        input.focus == TimelineFocusPaint::Out,
                                    );
                                    timecode_label(
                                        ui,
                                        "Trajanje",
                                        (input.tc_frame)(source_duration_frames),
                                        false,
                                    );
                                },
                            );
                        });
                    } else if input.show_import_actions {
                        ui.label(
                            RichText::new(input.clip_label)
                                .color(TEXT)
                                .strong()
                                .size(qnc_theme::FONT_UI),
                        );
                        ui.add_space(10.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if !input.ingest_status.is_empty() {
                                ui.label(
                                    RichText::new(input.ingest_status)
                                        .size(qnc_theme::FONT_UI)
                                        .color(MUTED),
                                );
                            }
                            if qnc_theme::action_btn(ui, "Osvježi").clicked() {
                                action = SourceDockAction::Reload;
                            }
                            if input.proxy_poster_approval_count > 0 {
                                let label = format!(
                                    "Generiraj postere ({})",
                                    input.proxy_poster_approval_count
                                );
                                if qnc_theme::action_btn(ui, &label)
                                    .on_hover_text("Odobri generiranje postera iz proxya")
                                    .clicked()
                                {
                                    action = SourceDockAction::ApproveProxyPosters;
                                }
                                ui.label(
                                    RichText::new("Nema postera na kartici")
                                        .size(qnc_theme::FONT_UI)
                                        .color(MUTED),
                                );
                            }
                            let uvezi = ui.add_enabled_ui(input.import_enabled, |ui| {
                                qnc_theme::primary_btn(ui, "Uvezi")
                            });
                            if uvezi.inner.clicked() {
                                action = SourceDockAction::ImportSelected;
                            }
                            if qnc_theme::action_btn(ui, "Odaberi sve").clicked() {
                                action = SourceDockAction::SelectAll;
                            }
                            if qnc_theme::action_btn(ui, "Očisti").clicked() {
                                action = SourceDockAction::ClearSelection;
                            }
                            if ui
                                .checkbox(
                                    &mut archive,
                                    RichText::new("Kopiraj original").size(qnc_theme::FONT_UI),
                                )
                                .changed()
                            {
                                action = SourceDockAction::SetArchive(archive);
                            }
                            if ui
                                .checkbox(
                                    &mut ai_mining,
                                    RichText::new("AI mining").size(qnc_theme::FONT_UI),
                                )
                                .changed()
                            {
                                action = SourceDockAction::SetAiMining(ai_mining);
                            }
                        });
                    } else {
                        ui.label(
                            RichText::new(input.clip_label)
                                .color(TEXT)
                                .strong()
                                .size(qnc_theme::FONT_UI),
                        );
                        ui.add_space(10.0);
                        timecode_label(
                            ui,
                            "IN",
                            (input.tc_frame)(source_in_frame),
                            input.focus == TimelineFocusPaint::In,
                        );
                        timecode_label(
                            ui,
                            "OUT",
                            (input.tc_frame)(source_out_frame),
                            input.focus == TimelineFocusPaint::Out,
                        );
                        timecode_label(
                            ui,
                            "Trajanje",
                            (input.tc_frame)(source_duration_frames),
                            false,
                        );
                    }
                });
                ui.add_space(HEADER_TIMELINE_GAP_H);
            }

            let filmstrip_background = |ui: &mut egui::Ui, rect: egui::Rect| {
                crate::qnc_filmstrip_background::paint(ui, rect, input.frames);
            };
            let video_background = if input.frames.is_empty() {
                None
            } else {
                Some(&filmstrip_background as &dyn Fn(&mut egui::Ui, egui::Rect))
            };

            let model = input.timeline_model;
            let intent = qnc_timeline_progress::show(
                ui,
                TimelineProgressInput {
                    model,
                    layers: source_layers(),
                    video_background,
                    focus: input.focus,
                    expanded_audio: input.expanded_audio,
                    a1_peaks: input.a1_peaks,
                    a2_peaks: input.a2_peaks,
                    a3_peaks: &[],
                    a4_peaks: &[],
                    covers: &[],
                    marker_slots: &[],
                    markers: &[],
                    base_video_blank: false,
                },
            );
            if matches!(action, SourceDockAction::None) {
                match intent {
                    TimelineProgressIntent::ToggleAudioExpand(lane) => {
                        action = SourceDockAction::ToggleAudioExpand(lane);
                    }
                    TimelineProgressIntent::CueFrame(frame) => {
                        action = SourceDockAction::CueFrame(frame);
                    }
                    TimelineProgressIntent::None => {}
                }
            }
        });

    action
}

fn timecode_label(ui: &mut egui::Ui, label: &str, value: String, focused: bool) {
    qnc_theme::timecode_label(ui, label, &value, focused);
}
