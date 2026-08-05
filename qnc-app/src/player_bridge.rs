//! Transitional: form model still mirrors player via Rx.
//! Target: components hold Tx/Rx only; this file shrinks away.

use eframe::egui::{self, ColorImage};

use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};
use crate::qnc_broadcast_player::{
    BroadcastPlayerOpenRequest, BroadcastPlayerRx, PlayerEvent, QncBroadcastPlayer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackCommand {
    SeekToPlayhead,
    PauseAndSeek,
    TogglePlay,
}

pub fn compact_playback_commands(
    raw: impl IntoIterator<Item = PlaybackCommand>,
    playing: bool,
) -> Vec<PlaybackCommand> {
    let mut seek_to = false;
    let mut pause_and_seek = false;
    let mut toggle = false;
    for command in raw {
        match command {
            PlaybackCommand::SeekToPlayhead => seek_to = true,
            PlaybackCommand::PauseAndSeek => pause_and_seek = true,
            PlaybackCommand::TogglePlay => toggle = true,
        }
    }

    let mut out = Vec::with_capacity(2);
    if pause_and_seek {
        out.push(PlaybackCommand::PauseAndSeek);
    } else if seek_to && !playing {
        out.push(PlaybackCommand::SeekToPlayhead);
    }
    if toggle {
        out.push(PlaybackCommand::TogglePlay);
    }
    out
}

fn should_cue_playhead_before_toggle(playing: bool) -> bool {
    !playing
}

pub fn should_apply_player_progress(
    player_frame: FrameNumber,
    pending_playhead_target: Option<FrameNumber>,
    playing: bool,
) -> bool {
    if playing {
        return true;
    }
    pending_playhead_target
        .map(|target| player_frame == target)
        .unwrap_or(true)
}

fn projected_playing_after_command(playing: bool, command: PlaybackCommand) -> bool {
    match command {
        PlaybackCommand::SeekToPlayhead | PlaybackCommand::PauseAndSeek => false,
        PlaybackCommand::TogglePlay => !playing,
    }
}

pub trait PlayerClient {
    fn drain_playback_commands(&mut self) -> Vec<PlaybackCommand>;
    fn clear_pending_seeks(&mut self);

    fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef>;
    fn playback_media_path(&self) -> Option<String>;
    fn playback_source_range_sec(&self) -> (f64, f64);
    fn playback_source_fps(&self) -> f64;
    fn playback_source_has_audio(&self) -> bool;
    fn playback_source_audio_channels(&self) -> u8;
    fn playback_source_frame(&self) -> FrameNumber;
    fn playback_is_playing(&self) -> bool;

    fn missing_source_message(&self) -> String;
    fn missing_path_message(&self) -> String;

    fn set_player_preview_active(&mut self, active: bool);
    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>);
    fn apply_player_frame(
        &mut self,
        image: ColorImage,
        source_frame: FrameNumber,
        source_sec: f64,
        playing: bool,
    );
    fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        source_sec: f64,
        playing: bool,
        status: impl Into<String>,
    );
    fn apply_player_error(&mut self, status: impl Into<String>);
}

fn open_request(
    client: &impl PlayerClient,
    _ctx: &egui::Context,
) -> Result<BroadcastPlayerOpenRequest, String> {
    let Some(source_ref) = client.playback_source_ref().cloned() else {
        return Err(client.missing_source_message());
    };
    let media_input = client
        .playback_media_path()
        .ok_or_else(|| client.missing_path_message())?;
    let (in_sec, out_sec) = client.playback_source_range_sec();
    let mut source_ref = source_ref;
    source_ref.in_seconds = Some(in_sec);
    source_ref.out_seconds = Some(out_sec);
    Ok(BroadcastPlayerOpenRequest {
        source_ref,
        media_input,
        source_fps: client.playback_source_fps(),
        has_audio: client.playback_source_has_audio(),
        audio_channels: client.playback_source_audio_channels(),
        start_source_frame: client.playback_source_frame(),
    })
}

/// Queue onto player TX. Player owns idempotent Open (same source = no restart).
pub fn handle_playback_commands(
    client: &mut impl PlayerClient,
    player: &QncBroadcastPlayer,
    ctx: &egui::Context,
) {
    let tx = player.tx();
    let mut projected_playing = client.playback_is_playing();
    for command in client.drain_playback_commands() {
        match command {
            PlaybackCommand::SeekToPlayhead | PlaybackCommand::PauseAndSeek => {
                let coalesce = matches!(command, PlaybackCommand::PauseAndSeek);
                match open_request(client, ctx) {
                    Ok(request) => {
                        let frame = request.start_source_frame;
                        let _ = tx.open(request);
                        let _ = tx.seek_frame(frame, true, coalesce);
                        projected_playing =
                            projected_playing_after_command(projected_playing, command);
                    }
                    Err(err) => client.apply_player_error(err),
                }
            }
            PlaybackCommand::TogglePlay => {
                let cue_playhead = should_cue_playhead_before_toggle(projected_playing);
                if !cue_playhead {
                    client.clear_pending_seeks();
                }
                crate::player_log::log_info("bridge", "TogglePlay / Space");
                match open_request(client, ctx) {
                    Ok(request) => {
                        let frame = request.start_source_frame;
                        let _ = tx.open(request);
                        if cue_playhead {
                            let _ = tx.goto_frame(frame);
                        }
                        let _ = tx.toggle_play();
                        projected_playing =
                            projected_playing_after_command(projected_playing, command);
                    }
                    Err(err) => {
                        crate::player_log::log_error("bridge", &err);
                        client.apply_player_error(err);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_keeps_pending_seek_before_toggle_when_paused() {
        let commands = compact_playback_commands(
            [PlaybackCommand::PauseAndSeek, PlaybackCommand::TogglePlay],
            false,
        );

        assert_eq!(
            commands,
            vec![PlaybackCommand::PauseAndSeek, PlaybackCommand::TogglePlay]
        );
    }

    #[test]
    fn compact_keeps_playing_toggle_as_pause_only_without_auto_seek() {
        let commands = compact_playback_commands(
            [PlaybackCommand::SeekToPlayhead, PlaybackCommand::TogglePlay],
            true,
        );

        assert_eq!(commands, vec![PlaybackCommand::TogglePlay]);
    }

    #[test]
    fn toggle_cues_playhead_only_when_starting_playback() {
        assert!(should_cue_playhead_before_toggle(false));
        assert!(!should_cue_playhead_before_toggle(true));
    }

    #[test]
    fn pause_seek_then_toggle_projects_as_start_from_target() {
        let mut projected = true;
        projected = projected_playing_after_command(projected, PlaybackCommand::PauseAndSeek);

        assert!(should_cue_playhead_before_toggle(projected));
        assert!(projected_playing_after_command(
            projected,
            PlaybackCommand::TogglePlay
        ));
    }

    #[test]
    fn paused_progress_does_not_override_pending_playhead_target_until_confirmed() {
        assert!(!should_apply_player_progress(
            FrameNumber(8),
            Some(FrameNumber(4)),
            false
        ));
        assert!(should_apply_player_progress(
            FrameNumber(4),
            Some(FrameNumber(4)),
            false
        ));
        assert!(should_apply_player_progress(
            FrameNumber(8),
            Some(FrameNumber(4)),
            true
        ));
        assert!(should_apply_player_progress(FrameNumber(8), None, false));
    }
}

/// Drain a subscriber RX into the form projection.
pub fn poll_player_remote(
    client: &mut impl PlayerClient,
    rx: &BroadcastPlayerRx,
    player: &QncBroadcastPlayer,
) {
    for event in rx.try_recv_all() {
        match event {
            PlayerEvent::Frame {
                image,
                source_frame,
                source_sec,
                playing,
            } => client.apply_player_frame(image, source_frame, source_sec, playing),
            PlayerEvent::State {
                source_frame,
                source_sec,
                playing,
                status,
            } => client.apply_player_state(source_frame, source_sec, playing, status),
            PlayerEvent::Error(err) => {
                crate::player_log::log_error("bridge-rx", &err);
                client.apply_player_error(err);
            }
            PlayerEvent::Stopped => {
                client.set_player_preview_active(false);
                client.apply_playback_command_state(false, "Stopped");
            }
        }
    }
    let snap = player.snapshot();
    client.set_player_preview_active(snap.active || snap.playing);
}
