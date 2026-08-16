//! Form → [`PlaybackStack`] bridge (open metadata + event projection only).
//! Playhead authority lives in [`CarrierSync`] — not in forms.

use eframe::egui::ColorImage;

use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};
use crate::qnc_broadcast_player::{BroadcastPlayerOpenRequest, BroadcastPlayerRx, PlayerEvent};

pub trait PlayerClient {
    fn playback_source_ref(&self) -> Option<BroadcastHostSourceRef>;
    fn playback_media_path(&self) -> Option<String>;
    fn playback_source_fps(&self) -> f64;
    fn playback_source_has_audio(&self) -> bool;
    fn playback_source_audio_channels(&self) -> u8;

    fn missing_source_message(&self) -> String;
    fn missing_path_message(&self) -> String;

    fn set_player_preview_active(&mut self, active: bool);
    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>);
    fn apply_player_frame(&mut self, image: ColorImage, source_frame: FrameNumber, playing: bool);
    fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        playing: bool,
        status: impl Into<String>,
    );
    fn apply_player_error(&mut self, status: impl Into<String>);
}

/// Build open request from form source metadata (no playhead — carrier owns that).
pub fn build_open_request(
    client: &impl PlayerClient,
) -> Result<BroadcastPlayerOpenRequest, String> {
    let Some(source_ref) = client.playback_source_ref() else {
        return Err(client.missing_source_message());
    };
    let media_input = client
        .playback_media_path()
        .ok_or_else(|| client.missing_path_message())?;
    Ok(BroadcastPlayerOpenRequest {
        source_ref,
        media_input,
        source_fps: client.playback_source_fps(),
        has_audio: client.playback_source_has_audio(),
        audio_channels: client.playback_source_audio_channels(),
        start_source_frame: FrameNumber(0),
    })
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
                source_frame,
                playing,
                ..
            } => client.apply_player_frame(image.clone(), *source_frame, *playing),
            PlayerEvent::State {
                source_frame,
                playing,
                status,
                ..
            } => client.apply_player_state(*source_frame, *playing, status.clone()),
            PlayerEvent::BoundaryReached { source_frame } => {
                client.apply_player_state(*source_frame, false, "Paused")
            }
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

#[allow(dead_code)]
pub fn poll_player_remote(
    client: &mut impl PlayerClient,
    rx: &BroadcastPlayerRx,
    player: &crate::qnc_broadcast_player::QncBroadcastPlayer,
) {
    apply_player_events(client, &rx.try_recv_all(), player);
}
