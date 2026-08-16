//! Native Story source editor — thin wrapper over shared `qnc_source_dock`.

use eframe::egui;

use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_source_dock::{self, SourceDockAction, SourceDockInput};
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::qnc_timeline_progress::TimelineProgressModel;

pub(super) struct SourceEditorInput<'a> {
    pub clip_label: &'a str,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub timeline_model: TimelineProgressModel,
    pub focus: TimelineFocusPaint,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub frames: &'a [FilmFrame],
    pub tc_frame: &'a dyn Fn(i64) -> String,
    pub expanded_audio: ExpandedAudio,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum SourceEditorAction {
    None,
    SaveVirtualShot,
    CreatePart(&'static str),
    CreateCover,
    CueFrame(i64),
    ToggleAudioExpand(ExpandedAudio),
}

pub(super) fn dock_height(expanded_audio: ExpandedAudio) -> f32 {
    qnc_source_dock::dock_height(expanded_audio, true)
}

pub(super) fn show(ui: &mut egui::Ui, input: SourceEditorInput<'_>) -> SourceEditorAction {
    match qnc_source_dock::show(
        ui,
        SourceDockInput {
            clip_label: input.clip_label,
            source_in_frame: input.source_in_frame,
            source_out_frame: input.source_out_frame,
            timeline_model: input.timeline_model,
            focus: input.focus,
            a1_peaks: input.a1_peaks,
            a2_peaks: input.a2_peaks,
            frames: input.frames,
            tc_frame: input.tc_frame,
            show_header: true,
            show_edit_actions: true,
            show_import_actions: false,
            archive_original: false,
            ai_mining: false,
            import_enabled: false,
            ingest_status: "",
            expanded_audio: input.expanded_audio,
        },
    ) {
        SourceDockAction::None => SourceEditorAction::None,
        SourceDockAction::CueFrame(frame) => SourceEditorAction::CueFrame(frame),
        SourceDockAction::ToggleAudioExpand(lane) => SourceEditorAction::ToggleAudioExpand(lane),
        SourceDockAction::SaveVirtualShot => SourceEditorAction::SaveVirtualShot,
        SourceDockAction::CreatePart(kind) => SourceEditorAction::CreatePart(kind),
        SourceDockAction::CreateCover => SourceEditorAction::CreateCover,
        // Ingest-only actions — ignore in Story/Media Assist wrappers.
        SourceDockAction::ImportSelected
        | SourceDockAction::SelectAll
        | SourceDockAction::ClearSelection
        | SourceDockAction::Reload
        | SourceDockAction::SetArchive(_)
        | SourceDockAction::SetAiMining(_) => SourceEditorAction::None,
    }
}
