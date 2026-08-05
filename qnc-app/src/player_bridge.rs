//! Form → [`PlaybackStack`] bridge (open metadata + command queue only).
//! Playhead authority lives in [`CarrierSync`] — not in forms.

use eframe::egui::{self, ColorImage};

use crate::playback_stack::PlaybackStack;
use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};
use crate::qnc_broadcast_player::{BroadcastPlayerOpenRequest, BroadcastPlayerRx, PlayerEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackCommand {
    /// Discrete seek (IO, marks, timeline click).
    CueFrame(FrameNumber),
    /// Scrub drag — coalesced still.
    ScrubFrame(FrameNumber),
    TogglePlay,
}

pub fn compact_playback_commands(
    raw: impl IntoIterator<Item = PlaybackCommand>,
    playing: bool,
) -> Vec<PlaybackCommand> {
    let mut cue = None;
    let mut scrub = None;
    let mut toggle = false;
    for command in raw {
        match command {
            PlaybackCommand::CueFrame(frame) => cue = Some(frame),
            PlaybackCommand::ScrubFrame(frame) => scrub = Some(frame),
            PlaybackCommand::TogglePlay => toggle = true,
        }
    }

    let mut out = Vec::with_capacity(2);
    if let Some(frame) = scrub {
        out.push(PlaybackCommand::ScrubFrame(frame));
    } else if let Some(frame) = cue {
        if !playing {
            out.push(PlaybackCommand::CueFrame(frame));
        }
    }
    if toggle {
        out.push(PlaybackCommand::TogglePlay);
    }
    out
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
    fn playback_is_playing(&self) -> bool;

    fn missing_source_message(&self) -> String;
    fn missing_path_message(&self) -> String;

    fn set_player_preview_active(&mut self, active: bool);
    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>);
    fn apply_player_frame(&mut self, image: ColorImage, source_sec: f64, playing: bool);
    fn apply_player_state(&mut self, source_sec: f64, playing: bool, status: impl Into<String>);
    fn apply_player_error(&mut self, status: impl Into<String>);
}

/// Build open request from form source metadata (no playhead — carrier owns that).
pub fn build_open_request(client: &impl PlayerClient) -> Result<BroadcastPlayerOpenRequest, String> {
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
        start_source_frame: FrameNumber(0),
    })
}

/// Drain form queue → shared playback stack (carrier + player).
pub fn handle_playback_commands(
    client: &mut impl PlayerClient,
    stack: &mut PlaybackStack,
    _ctx: &egui::Context,
) {
    for command in client.drain_playback_commands() {
        match command {
            PlaybackCommand::CueFrame(frame) => {
                match build_open_request(client) {
                    Ok(request) => {
                        if let Err(err) = stack.ensure_open(request) {
                            client.apply_player_error(err);
                            continue;
                        }
                        if !stack.cue_frame(frame.0) {
                            client.apply_player_error("Timeline nije spreman — pričekaj SourceReady");
                        }
                    }
                    Err(err) => client.apply_player_error(err),
                }
            }
            PlaybackCommand::ScrubFrame(frame) => {
                match build_open_request(client) {
                    Ok(request) => {
                        if let Err(err) = stack.ensure_open(request) {
                            client.apply_player_error(err);
                            continue;
                        }
                        let _ = stack.scrub_frame(frame.0);
                    }
                    Err(err) => client.apply_player_error(err),
                }
            }
            PlaybackCommand::TogglePlay => {
                client.clear_pending_seeks();
                match build_open_request(client) {
                    Ok(request) => {
                        if let Err(err) = stack.ensure_open(request) {
                            client.apply_player_error(err);
                            continue;
                        }
                        if let Err(err) = stack.toggle_play() {
                            client.apply_player_error(err);
                        }
                    }
                    Err(err) => client.apply_player_error(err),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_keeps_scrub_before_toggle_when_paused() {
        let commands = compact_playback_commands(
            [
                PlaybackCommand::ScrubFrame(FrameNumber(50)),
                PlaybackCommand::TogglePlay,
            ],
            false,
        );
        assert_eq!(
            commands,
            vec![
                PlaybackCommand::ScrubFrame(FrameNumber(50)),
                PlaybackCommand::TogglePlay,
            ]
        );
    }

    #[test]
    fn compact_skips_cue_when_playing_toggle_only() {
        let commands = compact_playback_commands(
            [
                PlaybackCommand::CueFrame(FrameNumber(10)),
                PlaybackCommand::TogglePlay,
            ],
            true,
        );
        assert_eq!(commands, vec![PlaybackCommand::TogglePlay]);
    }
}

/// Apply already-drained RX events — transport flags (+ optional editorial mirror in client).
pub fn apply_player_events(
    client: &mut impl PlayerClient,
    events: &[PlayerEvent],
    player: &crate::qnc_broadcast_player::QncBroadcastPlayer,
) {
    for event in events {
        match event {
            PlayerEvent::Frame {
                image,
                source_sec,
                playing,
                ..
            } => client.apply_player_frame(image.clone(), *source_sec, *playing),
            PlayerEvent::State {
                source_sec,
                playing,
                status,
                ..
            } => client.apply_player_state(*source_sec, *playing, status.clone()),
            PlayerEvent::Error(err) => {
                crate::player_log::log_error("bridge-rx", &err);
                client.apply_player_error(err);
            }
            PlayerEvent::Stopped => {
                client.set_player_preview_active(false);
                client.apply_playback_command_state(false, "Stopped");
            }
            PlayerEvent::SourceReady { .. } => {}
        }
    }
    let snap = player.snapshot();
    client.set_player_preview_active(snap.active || snap.playing);
}

pub fn poll_player_remote(
    client: &mut impl PlayerClient,
    rx: &BroadcastPlayerRx,
    player: &crate::qnc_broadcast_player::QncBroadcastPlayer,
) {
    apply_player_events(client, &rx.try_recv_all(), player);
}
