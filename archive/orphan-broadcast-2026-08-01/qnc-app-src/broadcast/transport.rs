//! Broadcast transport — Edit / Preview / Playout backends share one time model.
//!
//! ```text
//! UniversalTimelineSpec + FrameNumber (carrier)
//!   ├─ EditBackend    — still / scrub (frame-accurate)
//!   ├─ PreviewBackend — continuous UI play (same FrameNumber)
//!   └─ PlayoutSink    — full-res live process (Null stub in Phase A)
//! ```
//!
//! UI does not own the clock. Forms send [`TransportCommand`]; backends return
//! [`TransportEvent`].
//!
//! Phase A: **one** [`BroadcastEngine`] (one ffmpeg worker + one rodio device).
//! Edit vs preview is a routing role — not two competing audio streams.
//! Phase B can split engines/processes without changing this command surface.

use eframe::egui::{self, ColorImage};

use super::asset::BroadcastMediaAsset;
use super::engine::{BroadcastEngine, EngineEvent, EngineOpenRequest};
use super::timebase::FrameNumber;
use super::timeline::UniversalTimelineSpec;
use super::BroadcastPlaybackSource;

/// Shared program identity — all backends speak the same carrier time.
#[derive(Debug, Clone)]
pub struct ProgramHandle {
    pub source: BroadcastPlaybackSource,
    pub timeline: UniversalTimelineSpec,
    pub assets: Vec<BroadcastMediaAsset>,
    pub carrier_start: FrameNumber,
    pub repaint: Option<egui::Context>,
}

impl ProgramHandle {
    pub fn into_engine_open(self) -> EngineOpenRequest {
        EngineOpenRequest {
            source: self.source,
            assets: self.assets,
            timeline: self.timeline,
            start_frame: self.carrier_start,
            repaint: self.repaint,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TransportCommand {
    Load(ProgramHandle),
    /// Edit/scrub truth — frame-accurate still.
    Goto {
        frame: FrameNumber,
    },
    Play {
        from: FrameNumber,
    },
    Pause,
    Stop,
}

#[derive(Debug, Clone)]
pub enum TransportEvent {
    Still {
        frame: FrameNumber,
        image: ColorImage,
        sec: f64,
    },
    PreviewFrame {
        frame: FrameNumber,
        image: ColorImage,
        sec: f64,
    },
    /// Reserved for a split audio path; Phase A audio stays inside the engine.
    #[allow(dead_code)]
    PreviewAudio {
        frame: FrameNumber,
        pcm: Vec<f32>,
        rate: u32,
        ch: u8,
    },
    State {
        frame: FrameNumber,
        sec: f64,
        playing: bool,
        status: String,
    },
    Error(String),
    Stopped,
}

/// Frame-accurate edit — IN/OUT/M, scrub. Never continuous as truth.
pub trait EditBackend {
    fn load(&mut self, program: ProgramHandle) -> Result<(), String>;
    fn still(&mut self, frame: FrameNumber) -> Result<(), String>;
    fn stop(&mut self);
    fn poll(&mut self) -> Vec<TransportEvent>;
}

/// Montage play — same FrameNumber, may be low-res continuous.
pub trait PreviewBackend {
    fn load(&mut self, program: ProgramHandle) -> Result<(), String>;
    fn play(&mut self, from: FrameNumber) -> Result<(), String>;
    fn pause(&mut self);
    fn stop(&mut self);
    fn poll(&mut self) -> Vec<TransportEvent>;
}

/// Live / air — own process later; UI never treats ColorImage as playout truth.
pub trait PlayoutSink {
    fn load(&mut self, program: ProgramHandle) -> Result<(), String>;
    fn play(&mut self, from: FrameNumber) -> Result<(), String>;
    fn pause(&mut self);
    fn stop(&mut self);
}

/// Phase A stub — no live process yet.
#[derive(Debug, Default)]
pub struct NullPlayoutSink;

impl PlayoutSink for NullPlayoutSink {
    fn load(&mut self, _program: ProgramHandle) -> Result<(), String> {
        Ok(())
    }

    fn play(&mut self, _from: FrameNumber) -> Result<(), String> {
        Ok(())
    }

    fn pause(&mut self) {}

    fn stop(&mut self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportRole {
    Edit,
    Preview,
}

/// Still/scrub commands against a shared engine (Phase A).
pub struct FfmpegEditBackend<'a> {
    engine: &'a BroadcastEngine,
}

impl<'a> FfmpegEditBackend<'a> {
    pub fn new(engine: &'a BroadcastEngine) -> Self {
        Self { engine }
    }
}

impl EditBackend for FfmpegEditBackend<'_> {
    fn load(&mut self, program: ProgramHandle) -> Result<(), String> {
        self.engine.open(program.into_engine_open());
        Ok(())
    }

    fn still(&mut self, frame: FrameNumber) -> Result<(), String> {
        self.engine.seek(frame, true);
        Ok(())
    }

    fn stop(&mut self) {
        self.engine.stop();
    }

    fn poll(&mut self) -> Vec<TransportEvent> {
        map_engine_events(self.engine.poll())
    }
}

/// Continuous play commands against a shared engine (Phase A).
pub struct FfmpegPreviewBackend<'a> {
    engine: &'a BroadcastEngine,
}

impl<'a> FfmpegPreviewBackend<'a> {
    pub fn new(engine: &'a BroadcastEngine) -> Self {
        Self { engine }
    }
}

impl PreviewBackend for FfmpegPreviewBackend<'_> {
    fn load(&mut self, program: ProgramHandle) -> Result<(), String> {
        self.engine.open(program.into_engine_open());
        Ok(())
    }

    fn play(&mut self, from: FrameNumber) -> Result<(), String> {
        // Anchor only when the playhead moved since last still; avoid double
        // seek+clear which stalls continuous pipes / audio.
        self.engine.seek(from, false);
        self.engine.play();
        Ok(())
    }

    fn pause(&mut self) {
        self.engine.pause();
    }

    fn stop(&mut self) {
        self.engine.stop();
    }

    fn poll(&mut self) -> Vec<TransportEvent> {
        map_engine_events(self.engine.poll())
    }
}

/// Thin router: scrub → edit, Space → preview, On Air → playout (later).
///
/// Phase A owns **one** [`BroadcastEngine`] so video + rodio stay coherent.
pub struct BroadcastTransport {
    engine: BroadcastEngine,
    playout: NullPlayoutSink,
    role: TransportRole,
    last_frame: FrameNumber,
    last_sec: f64,
}

impl Default for BroadcastTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastTransport {
    pub fn new() -> Self {
        Self {
            engine: BroadcastEngine::spawn(),
            playout: NullPlayoutSink,
            role: TransportRole::Edit,
            last_frame: FrameNumber(0),
            last_sec: 0.0,
        }
    }

    pub fn dispatch(&mut self, command: TransportCommand) -> Result<(), String> {
        match command {
            TransportCommand::Load(program) => {
                self.last_frame = program.carrier_start;
                self.last_sec = program
                    .timeline
                    .carrier
                    .timebase
                    .seconds_at_frame(program.carrier_start);
                self.role = TransportRole::Edit;
                FfmpegEditBackend::new(&self.engine).load(program.clone())?;
                let _ = self.playout.load(program);
                Ok(())
            }
            TransportCommand::Goto { frame } => {
                self.role = TransportRole::Edit;
                self.last_frame = frame;
                FfmpegEditBackend::new(&self.engine).still(frame)
            }
            TransportCommand::Play { from } => {
                self.role = TransportRole::Preview;
                // Re-anchor only if UI playhead moved without a Goto (rare).
                // Avoid seek+play on every Space — that clears continuous pipes
                // and audio before preroll.
                if from != self.last_frame {
                    self.engine.seek(from, false);
                }
                self.last_frame = from;
                self.engine.play();
                Ok(())
            }
            TransportCommand::Pause => {
                self.engine.pause();
                self.role = TransportRole::Edit;
                Ok(())
            }
            TransportCommand::Stop => {
                self.engine.stop();
                self.playout.stop();
                self.role = TransportRole::Edit;
                Ok(())
            }
        }
    }

    pub fn poll(&mut self) -> Vec<TransportEvent> {
        let events = map_engine_events(self.engine.poll());
        let mut out = Vec::with_capacity(events.len());
        for ev in events {
            match &ev {
                TransportEvent::Still { frame, sec, .. }
                | TransportEvent::PreviewFrame { frame, sec, .. } => {
                    self.last_frame = *frame;
                    self.last_sec = *sec;
                }
                TransportEvent::State {
                    frame,
                    sec,
                    playing,
                    ..
                } => {
                    self.last_frame = *frame;
                    self.last_sec = *sec;
                    if *playing {
                        self.role = TransportRole::Preview;
                    } else if self.role == TransportRole::Preview {
                        self.role = TransportRole::Edit;
                    }
                }
                _ => {}
            }
            out.push(ev);
        }
        out
    }

    pub fn last_frame(&self) -> FrameNumber {
        self.last_frame
    }

    pub fn last_sec(&self) -> f64 {
        self.last_sec
    }

    pub fn playing(&self) -> bool {
        self.role == TransportRole::Preview
    }
}

fn map_engine_events(events: Vec<EngineEvent>) -> Vec<TransportEvent> {
    events
        .into_iter()
        .map(|ev| match ev {
            EngineEvent::Frame {
                image,
                source_frame,
                source_sec,
                playing,
            } => {
                if playing {
                    TransportEvent::PreviewFrame {
                        frame: source_frame,
                        image,
                        sec: source_sec,
                    }
                } else {
                    TransportEvent::Still {
                        frame: source_frame,
                        image,
                        sec: source_sec,
                    }
                }
            }
            EngineEvent::State {
                source_frame,
                source_sec,
                playing,
                status,
            } => TransportEvent::State {
                frame: source_frame,
                sec: source_sec,
                playing,
                status,
            },
            EngineEvent::Error(err) => TransportEvent::Error(err),
            EngineEvent::Stopped => TransportEvent::Stopped,
        })
        .collect()
}
