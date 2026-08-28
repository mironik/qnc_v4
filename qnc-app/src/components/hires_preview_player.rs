use eframe::egui::{self, Color32, RichText};
use qnc_service_contracts::{
    ExportHiResPlaylistItem, ExportHiResPlaylistSource, PreviewHiResInputResponse,
};

use crate::playback_routing::PlaybackTransportIntent;
use crate::playback_stack::PlaybackStack;
use crate::player_contract::{BroadcastHostSourceRef, BroadcastSourceTimebase, FrameNumber};
use crate::player_remote::{
    BroadcastProgramItem, BroadcastProgramOpenRequest, BroadcastProgramPreviewVideoResolution,
    BroadcastProgramSource,
};
use crate::qnc_theme;

const HIRES_PREVIEW_PLAY_HINT: &str = "Space to play";

#[derive(Debug, Clone)]
pub(crate) struct HiResPreviewOpen {
    pub preview_id: String,
    pub request: BroadcastProgramOpenRequest,
}

#[derive(Debug, Default)]
pub(crate) struct HiResPreviewPlayerState {
    active: bool,
    input_ready: bool,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HiResPreviewPlayerAction {
    None,
    Close,
}

pub(crate) struct HiResPreviewPlayerComponent;

impl HiResPreviewPlayerState {
    pub(crate) fn active(&self) -> bool {
        self.active
    }
}

impl HiResPreviewPlayerComponent {
    pub(crate) fn build_open(
        input: &PreviewHiResInputResponse,
    ) -> Result<HiResPreviewOpen, String> {
        let timeline_timebase = BroadcastSourceTimebase {
            fps_num: input.timeline_timebase.fps_num,
            fps_den: input.timeline_timebase.fps_den,
        };
        let timeline_fps = timeline_timebase
            .fps()
            .ok_or_else(|| "Preview HI-res flat playlist nema valjan timebase".to_string())?;
        if input.items.is_empty() {
            return Err("Preview HI-res flat playlist je prazna".into());
        }
        let items = input
            .items
            .iter()
            .map(|item| program_item_from_flat_item(&input.project_id, item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HiResPreviewOpen {
            preview_id: input.preview_id.clone(),
            request: BroadcastProgramOpenRequest {
                program_id: format!("hires-preview:{}", input.preview_id.trim()),
                project_id: input.project_id.clone(),
                timeline_fps,
                duration_frames: input.duration_frames.max(1),
                start_program_frame: FrameNumber(0),
                preview_video_resolution: BroadcastProgramPreviewVideoResolution::SourceRaster,
                items,
            },
        })
    }

    pub(crate) fn build_play_intent(open: &HiResPreviewOpen) -> PlaybackTransportIntent {
        PlaybackTransportIntent::PlayProgram(open.request.clone())
    }

    pub(crate) fn open(
        state: &mut HiResPreviewPlayerState,
        ctx: &egui::Context,
        _open: &HiResPreviewOpen,
    ) {
        state.active = true;
        state.input_ready = true;
        state.message = HIRES_PREVIEW_PLAY_HINT.into();
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        ctx.request_repaint();
    }

    pub(crate) fn close(state: &mut HiResPreviewPlayerState, ctx: &egui::Context) {
        state.active = false;
        state.input_ready = false;
        state.message.clear();
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        ctx.request_repaint();
    }

    pub(crate) fn show(
        ctx: &egui::Context,
        playback: &PlaybackStack,
        state: &mut HiResPreviewPlayerState,
    ) -> HiResPreviewPlayerAction {
        if !state.active {
            return HiResPreviewPlayerAction::None;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            return HiResPreviewPlayerAction::Close;
        }

        let screen_rect = ctx.screen_rect();
        egui::Area::new(egui::Id::new("hires_preview_player.fullscreen"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(screen_rect.size());
                egui::Frame::NONE
                    .fill(Color32::BLACK)
                    .inner_margin(egui::Margin::same(0))
                    .show(ui, |ui| {
                        let monitor_h = ui.available_height().max(120.0);
                        let empty_label = if state.message.trim().is_empty() {
                            "Space to play"
                        } else {
                            state.message.as_str()
                        };
                        if state.input_ready {
                            playback.show_monitor(ui, monitor_h, empty_label);
                        } else {
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), monitor_h),
                                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                |ui| {
                                    ui.label(
                                        RichText::new(empty_label)
                                            .color(qnc_theme::MUTED)
                                            .size(qnc_theme::FONT_UI + 2.0),
                                    );
                                },
                            );
                        }
                    });
            });
        HiResPreviewPlayerAction::None
    }
}

fn program_item_from_flat_item(
    project_id: &str,
    item: &ExportHiResPlaylistItem,
) -> Result<BroadcastProgramItem, String> {
    let record_in_frame = item.record_in_frame.max(0);
    let record_out_frame = item.record_out_frame.max(record_in_frame + 1);
    let sources = item
        .sources
        .iter()
        .map(|source| program_source_from_flat_source(project_id, source))
        .collect::<Result<Vec<_>, _>>()?;
    if sources.is_empty() {
        return Err(format!(
            "Preview HI-res item nema source · {}",
            item.item_id
        ));
    }
    Ok(BroadcastProgramItem {
        item_id: item.item_id.clone(),
        record_in_frame: FrameNumber(record_in_frame),
        record_out_frame: FrameNumber(record_out_frame),
        sources,
    })
}

fn program_source_from_flat_source(
    project_id: &str,
    source: &ExportHiResPlaylistSource,
) -> Result<BroadcastProgramSource, String> {
    let source_timebase = BroadcastSourceTimebase {
        fps_num: source.source_timebase.fps_num,
        fps_den: source.source_timebase.fps_den,
    };
    let source_fps = source_timebase.fps().ok_or_else(|| {
        format!(
            "Preview HI-res source nema valjan timebase · {}",
            source.clip_id
        )
    })?;
    let source_in = source.source_in_frame.max(0);
    let source_out = source.source_out_frame.max(source_in + 1);
    let duration_frames = source_out.max(1);
    let media_input = source.original_path.to_string_lossy().trim().to_string();
    if media_input.is_empty() {
        return Err(format!(
            "Preview HI-res source nema original path · {}",
            source.clip_id
        ));
    }
    let source_ref = BroadcastHostSourceRef::from_frame_fields(
        project_id,
        &source.source_id,
        &source.virtual_shot_id,
        &source.clip_id,
        Some(FrameNumber(source_in)),
        Some(FrameNumber(source_out)),
        FrameNumber(duration_frames),
    )
    .map_err(|error| error.to_string())?;
    Ok(BroadcastProgramSource {
        source_ref,
        media_input,
        source_fps,
        source_timebase,
        has_video: source.has_video,
        has_audio: source.has_audio,
        audio_channels: if source.has_audio { 2 } else { 0 },
        audio_output_channel: source.audio_output_channel,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qnc_service_contracts::{FrameTimebase, PreviewHiResInputResponse};

    use super::*;

    fn flat_source(
        source_id: &str,
        kind: &str,
        clip_id: &str,
        path: &str,
        has_video: bool,
        has_audio: bool,
        audio_output_channel: Option<u8>,
    ) -> ExportHiResPlaylistSource {
        ExportHiResPlaylistSource {
            source_id: source_id.into(),
            source_kind: kind.into(),
            clip_id: clip_id.into(),
            virtual_shot_id: format!("{clip_id}_virtual"),
            original_path: PathBuf::from(path),
            source_in_frame: 10,
            source_out_frame: 60,
            source_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            has_video,
            has_audio,
            audio_output_channel,
        }
    }

    fn input_response() -> PreviewHiResInputResponse {
        PreviewHiResInputResponse {
            project_id: "project_a".into(),
            preview_id: "preview_a".into(),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 100,
            items: vec![ExportHiResPlaylistItem {
                item_id: "item:0-50".into(),
                record_in_frame: 0,
                record_out_frame: 50,
                sources: vec![
                    flat_source(
                        "part:p1:base_audio",
                        "base_audio",
                        "clip_a",
                        "G:/PRIVATE/XDROOT/Clip/base.MXF",
                        false,
                        true,
                        Some(0),
                    ),
                    flat_source(
                        "cover:c1",
                        "cover",
                        "clip_b",
                        "G:/PRIVATE/XDROOT/Clip/cover.MXF",
                        true,
                        true,
                        Some(1),
                    ),
                ],
            }],
            message: None,
        }
    }

    #[test]
    fn preview_hires_builds_broadcast_program_from_flat_playlist() {
        let open = HiResPreviewPlayerComponent::build_open(&input_response()).unwrap();
        let intent = HiResPreviewPlayerComponent::build_play_intent(&open);

        assert_eq!(open.preview_id, "preview_a");
        assert_eq!(open.request.project_id, "project_a");
        assert_eq!(open.request.program_id, "hires-preview:preview_a");
        assert_eq!(open.request.timeline_fps, 50.0);
        assert_eq!(open.request.duration_frames, 100);
        assert_eq!(
            open.request.preview_video_resolution,
            BroadcastProgramPreviewVideoResolution::SourceRaster
        );
        assert_eq!(open.request.items.len(), 1);
        assert_eq!(open.request.items[0].sources.len(), 2);
        assert_eq!(
            open.request.items[0].sources[0].media_input,
            "G:/PRIVATE/XDROOT/Clip/base.MXF"
        );
        assert_eq!(
            open.request.items[0].sources[1].media_input,
            "G:/PRIVATE/XDROOT/Clip/cover.MXF"
        );
        assert!(!open.request.items[0].sources[0].has_video);
        assert_eq!(
            open.request.items[0].sources[0].audio_output_channel,
            Some(0)
        );
        assert!(open.request.items[0].sources[1].has_video);
        assert_eq!(
            open.request.items[0].sources[1].audio_output_channel,
            Some(1)
        );
        assert!(matches!(intent, PlaybackTransportIntent::PlayProgram(_)));
    }

    #[test]
    fn preview_hires_rejects_empty_flat_playlist() {
        let mut input = input_response();
        input.items.clear();

        assert!(HiResPreviewPlayerComponent::build_open(&input).is_err());
    }

    #[test]
    fn preview_ready_switches_fullscreen_shell_to_program_monitor() {
        let ctx = egui::Context::default();
        let open = HiResPreviewPlayerComponent::build_open(&input_response()).unwrap();
        let mut state = HiResPreviewPlayerState::default();

        HiResPreviewPlayerComponent::open(&mut state, &ctx, &open);

        assert!(state.active());
        assert!(state.input_ready);
        assert_eq!(state.message, HIRES_PREVIEW_PLAY_HINT);
    }
}
