//! QNC broadcast player — neutral egui component (TX/RX).
//!
//! Owns the modular player remote and monitor frame projection.
//! Command sources send Open/Play/Seek/Stop and display components subscribe to
//! events. The player does not know Story, Wrap, Ingest, or filmstrip.
//!
//! ```text
//! Command source ──Tx──► QncBroadcastPlayer ──Rx──► Display component
//!                    │
//!                    ├── modular player remote (core + FFmpeg adapter + output)
//!                    ├── preview texture
//!                    └── snapshot
//! ```

use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use serde_json::Value;

use crate::player_contract::{BroadcastSourceKind, FrameNumber};
use crate::player_remote::PlayerRemote;
use crate::qnc_ui;

pub use crate::player_remote::{
    BroadcastPlayerOpenRequest, BroadcastProgramOpenRequest, PlayerCommand, PlayerEvent,
    PlayerRemoteState,
};

/// Bounded RX capacity — drop-newest when a subscriber falls behind (Phase D).
const RX_CAPACITY: usize = 64;

pub mod css {
    use eframe::egui::Color32;

    pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0f, 0x19);
    pub const EMPTY: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
    pub const FRAME: Color32 = Color32::from_rgb(55, 65, 81);
}

/// Latest clock/transport peek (optional; prefer Rx for reactions).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlayerSnapshot {
    pub source_frame: FrameNumber,
    pub source_sec: f64,
    pub playing: bool,
    pub active: bool,
    pub has_source: bool,
    pub source_kind: Option<BroadcastSourceKind>,
    pub project_id: Option<String>,
    pub clip_id: Option<String>,
    pub virtual_shot_id: Option<String>,
    pub status: String,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            source_frame: FrameNumber(0),
            source_sec: 0.0,
            playing: false,
            active: false,
            has_source: false,
            source_kind: None,
            project_id: None,
            clip_id: None,
            virtual_shot_id: None,
            status: String::new(),
        }
    }
}

impl From<PlayerRemoteState> for PlayerSnapshot {
    fn from(s: PlayerRemoteState) -> Self {
        Self {
            source_frame: s.source_frame,
            source_sec: s.source_sec,
            playing: s.playing,
            active: s.active,
            has_source: s.has_source,
            source_kind: s.source_kind,
            project_id: s.project_id,
            clip_id: s.clip_id,
            virtual_shot_id: s.virtual_shot_id,
            status: s.status,
        }
    }
}

/// Cloneable command port — anyone may send; player does not know who.
#[derive(Clone)]
pub struct BroadcastPlayerTx {
    tx: Sender<PlayerCommand>,
}

impl BroadcastPlayerTx {
    pub fn send(&self, command: PlayerCommand) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "broadcast player TX closed".into())
    }

    pub fn open(&self, request: BroadcastPlayerOpenRequest) -> Result<(), String> {
        self.send(PlayerCommand::Open(request))
    }

    pub fn open_program(&self, request: BroadcastProgramOpenRequest) -> Result<(), String> {
        self.send(PlayerCommand::OpenProgram(request))
    }

    #[allow(dead_code)]
    pub fn play(&self) -> Result<(), String> {
        self.send(PlayerCommand::Play)
    }

    #[allow(dead_code)]
    pub fn pause(&self) -> Result<(), String> {
        self.send(PlayerCommand::Pause)
    }

    pub fn toggle_play(&self) -> Result<(), String> {
        self.send(PlayerCommand::TogglePlay)
    }

    #[allow(dead_code)]
    pub fn seek_frame(
        &self,
        frame: FrameNumber,
        still: bool,
        coalesce: bool,
    ) -> Result<(), String> {
        self.send(PlayerCommand::SeekFrame {
            frame,
            still,
            coalesce,
        })
    }

    #[allow(dead_code)]
    pub fn goto_frame(&self, frame: FrameNumber) -> Result<(), String> {
        self.seek_frame(frame, true, false)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.send(PlayerCommand::Stop)
    }
}

/// Event port — bounded queue. Call [`QncBroadcastPlayer::subscribe`] for more.
pub struct BroadcastPlayerRx {
    rx: Receiver<PlayerEvent>,
}

impl BroadcastPlayerRx {
    /// Placeholder before [`QncBroadcastPlayer::subscribe`] — drops all events.
    #[allow(dead_code)]
    pub fn disconnected() -> Self {
        let (_tx, rx) = mpsc::sync_channel(1);
        Self { rx }
    }

    /// Non-blocking drain of pending player events.
    pub fn try_recv_all(&self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    #[allow(dead_code)]
    pub fn try_recv(&self) -> Option<PlayerEvent> {
        self.rx.try_recv().ok()
    }
}

type EventFanout = Arc<Mutex<Vec<SyncSender<PlayerEvent>>>>;

/// App-owned player runtime: owns pump, texture, TX inbox, RX fan-out.
pub struct QncBroadcastPlayer {
    tx: BroadcastPlayerTx,
    cmd_rx: Receiver<PlayerCommand>,
    event_fanout: EventFanout,
    remote: PlayerRemote,
    texture: Option<TextureHandle>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
    transport_tick_gate: TransportTickGate,
}

#[derive(Debug, Default)]
struct TransportTickGate {
    last_ui_time: Option<f64>,
}

impl TransportTickGate {
    fn should_advance(&mut self, ui_time: f64) -> bool {
        if self
            .last_ui_time
            .is_some_and(|last| last.to_bits() == ui_time.to_bits())
        {
            return false;
        }
        self.last_ui_time = Some(ui_time);
        true
    }
}

impl Default for QncBroadcastPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl QncBroadcastPlayer {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        Self {
            tx: BroadcastPlayerTx { tx: cmd_tx },
            cmd_rx,
            event_fanout: Arc::new(Mutex::new(Vec::new())),
            remote: PlayerRemote::new(),
            texture: None,
            snapshot: Arc::new(Mutex::new(PlayerSnapshot::default())),
            transport_tick_gate: TransportTickGate::default(),
        }
    }

    pub fn tx(&self) -> BroadcastPlayerTx {
        self.tx.clone()
    }

    /// New event subscription (bounded). Each component that needs events calls this once.
    pub fn subscribe(&self) -> BroadcastPlayerRx {
        let (event_tx, event_rx) = mpsc::sync_channel(RX_CAPACITY);
        if let Ok(mut slots) = self.event_fanout.lock() {
            slots.push(event_tx);
        }
        BroadcastPlayerRx { rx: event_rx }
    }

    #[allow(dead_code)]
    pub fn texture(&self) -> Option<&TextureHandle> {
        self.texture.as_ref()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn matches_source(&self, request: &BroadcastPlayerOpenRequest) -> bool {
        self.remote.matches_source(request)
    }

    pub fn configure_runtime_profile(&mut self, runtime: &Value) {
        self.remote.configure_runtime_profile(runtime);
    }

    /// Shell tick: TX inbox → pump → texture + fan-out on RX subscribers.
    pub fn pump(&mut self, ctx: &egui::Context) {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(cmd) => self.remote.dispatch(cmd, ctx),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        let ui_time = ctx.input(|input| input.time);
        let events = if self.transport_tick_gate.should_advance(ui_time) {
            self.remote.tick(ctx)
        } else {
            self.remote.poll(ctx)
        };
        for event in events {
            match &event {
                PlayerEvent::Frame { image, .. } => {
                    self.upload_frame(ctx, image.clone());
                }
                PlayerEvent::Stopped => {
                    crate::player_log::log_info("player", "Stopped");
                    self.texture = None;
                }
                PlayerEvent::Error(err) => {
                    crate::player_log::log_error("player", err);
                }
                PlayerEvent::State {
                    status,
                    playing,
                    source_frame,
                    source_sec,
                    ..
                } => {
                    crate::player_log::log_state(
                        "player",
                        status,
                        *playing,
                        source_frame.0,
                        *source_sec,
                    );
                }
                PlayerEvent::BoundaryReached { source_frame } => {
                    crate::player_log::log_state("player", "Boundary", false, source_frame.0, 0.0);
                }
                PlayerEvent::SourceReady { .. } => {}
            }
            self.fanout_event(event);
        }

        if let Ok(mut slot) = self.snapshot.lock() {
            *slot = PlayerSnapshot::from(self.remote.state());
        }
    }

    fn fanout_event(&self, event: PlayerEvent) {
        let Ok(mut slots) = self.event_fanout.lock() else {
            return;
        };
        slots.retain(|tx| match tx.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    pub fn stop(&mut self) {
        let _ = self.tx.stop();
        self.remote.stop();
        self.texture = None;
        self.fanout_event(PlayerEvent::Stopped);
        if let Ok(mut slot) = self.snapshot.lock() {
            *slot = PlayerSnapshot::default();
        }
    }

    fn upload_frame(&mut self, ctx: &egui::Context, image: ColorImage) {
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("qnc_broadcast_player_frame", image, TextureOptions::LINEAR));
        }
        ctx.request_repaint();
    }

    pub fn show_monitor(&self, ui: &mut egui::Ui, height: f32, empty_label: &str) {
        let _ = (css::BG, css::EMPTY, css::FRAME);
        qnc_ui::preview(
            ui,
            qnc_ui::PreviewInput {
                height,
                texture: self.texture.as_ref(),
                empty_label,
                sense: egui::Sense::hover(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::TransportTickGate;

    #[test]
    fn transport_tick_gate_allows_one_transport_advance_per_ui_time() {
        let mut gate = TransportTickGate::default();

        assert!(gate.should_advance(1.0));
        assert!(!gate.should_advance(1.0));
        assert!(gate.should_advance(1.016));
    }
}
