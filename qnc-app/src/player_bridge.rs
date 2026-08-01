//! Transitional: form model still mirrors player via Rx.
//! Target: components hold Tx/Rx only; this file shrinks away.

use eframe::egui::{self, ColorImage};

use crate::player_contract::BroadcastHostSourceRef;
use crate::qnc_broadcast_player::{
    BroadcastPlayerOpenRequest, BroadcastPlayerRx, PlayerEvent, QncBroadcastPlayer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackCommand {
    SeekToPlayhead,
    PauseAndSeek,
    TogglePlay,
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
    fn playback_source_sec(&self) -> f64;

    fn missing_source_message(&self) -> String;
    fn missing_path_message(&self) -> String;

    fn set_native_preview_active(&mut self, active: bool);
    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>);
    fn apply_native_player_frame(&mut self, image: ColorImage, source_sec: f64, playing: bool);
    fn apply_native_player_state(
        &mut self,
        source_sec: f64,
        playing: bool,
        status: impl Into<String>,
    );
    fn apply_native_player_error(&mut self, status: impl Into<String>);
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
        start_source_sec: client.playback_source_sec(),
    })
}

/// Queue onto player TX. Player owns idempotent Open (same source = no restart).
pub fn handle_playback_commands(
    client: &mut impl PlayerClient,
    player: &QncBroadcastPlayer,
    ctx: &egui::Context,
) {
    let tx = player.tx();
    for command in client.drain_playback_commands() {
        match command {
            PlaybackCommand::SeekToPlayhead | PlaybackCommand::PauseAndSeek => {
                let coalesce = matches!(command, PlaybackCommand::PauseAndSeek);
                match open_request(client, ctx) {
                    Ok(request) => {
                        let sec = request.start_source_sec;
                        let _ = tx.open(request);
                        let _ = tx.seek_sec(sec, true, coalesce);
                    }
                    Err(err) => client.apply_native_player_error(err),
                }
            }
            PlaybackCommand::TogglePlay => {
                client.clear_pending_seeks();
                crate::player_log::log_info("bridge", "TogglePlay / Space");
                match open_request(client, ctx) {
                    Ok(request) => {
                        let _ = tx.open(request);
                        let _ = tx.toggle_play();
                    }
                    Err(err) => {
                        crate::player_log::log_error("bridge", &err);
                        client.apply_native_player_error(err);
                    }
                }
            }
        }
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
                source_sec,
                playing,
                ..
            } => client.apply_native_player_frame(image, source_sec, playing),
            PlayerEvent::State {
                source_sec,
                playing,
                status,
                ..
            } => client.apply_native_player_state(source_sec, playing, status),
            PlayerEvent::Error(err) => {
                crate::player_log::log_error("bridge-rx", &err);
                client.apply_native_player_error(err);
            }
            PlayerEvent::Stopped => {
                client.set_native_preview_active(false);
                client.apply_playback_command_state(false, "Stopped");
            }
        }
    }
    let snap = player.snapshot();
    client.set_native_preview_active(snap.active || snap.playing);
}
