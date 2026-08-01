//! Live broadcast engine — owns [`BroadcastPlaybackPump`] on a worker thread.
//!
//! Neutral product decode path: program spec in, RGBA + PCM out. Forms own
//! how they build the program; this engine does not.

use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, Sink};

use super::asset::{BroadcastMediaAsset, InMemoryMediaResolver};
use super::av_sync::{
    audio_emit_range, carrier_decode_exhausted, decode_error_action, decode_frontier_for_stall,
    play_start_ready, should_resume_after_stall, soft_eos_tick_progress, DecodeErrorAction,
    MAX_DECODE_RECOVER_STREAK, PLAY_START_MIN_BUFFER_FRAMES, SOFT_EOS_TICKS,
    STALL_RESUME_BUFFER_FRAMES,
};
use super::backend::DecodedProgramFrame;
use super::clock::{ClockReference, ClockState};
use super::ffmpeg::{FfmpegBroadcastBackend, FfmpegBroadcastConfig};
use super::payload::{BroadcastAudioPayload, BroadcastVideoPayload};
use super::player::BroadcastPlaybackPump;
use super::player_log;
use super::playout::{BroadcastPlayoutFrame, PlayoutVideo};
use super::present::{composite_video_layers, diagnostics_status_hint, mix_audio_buses};
use super::probe::FfprobeMediaProbe;
use super::program::{source_spec_for_playback, strip_filmstrip};
use super::timebase::FrameNumber;
use super::{BroadcastPlaybackSource, UniversalTimelineSpec};

const QUEUE_CAPACITY: usize = 24;
const LOOKAHEAD_FRAMES: usize = 8;
/// Warm queues before starting the clock — decode is capped while Playing.
const PLAY_PREROLL_FRAMES: usize = 12;

#[derive(Debug, Clone)]
pub struct EngineOpenRequest {
    pub source: BroadcastPlaybackSource,
    pub assets: Vec<BroadcastMediaAsset>,
    pub timeline: UniversalTimelineSpec,
    pub start_frame: FrameNumber,
    pub repaint: Option<egui::Context>,
}

#[derive(Debug)]
enum EngineCommand {
    Open(EngineOpenRequest),
    Play,
    Pause,
    Seek { frame: FrameNumber, still: bool },
    Stop,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Frame {
        image: ColorImage,
        source_frame: FrameNumber,
        source_sec: f64,
        playing: bool,
    },
    State {
        source_frame: FrameNumber,
        source_sec: f64,
        playing: bool,
        status: String,
    },
    Error(String),
    Stopped,
}

struct EngineAudio {
    _stream: OutputStream,
    sink: Sink,
}

impl EngineAudio {
    fn new() -> Option<Self> {
        let (stream, handle) = OutputStream::try_default().ok()?;
        let sink = Sink::try_new(&handle).ok()?;
        sink.pause();
        Some(Self {
            _stream: stream,
            sink,
        })
    }

    fn append(&self, sample_rate: u32, channels: u8, samples: Vec<f32>) {
        if samples.is_empty() || channels == 0 {
            return;
        }
        self.sink
            .append(SamplesBuffer::new(channels as u16, sample_rate, samples));
        self.sink.play();
    }

    fn play(&self) {
        self.sink.play();
    }

    fn pause(&self) {
        self.sink.pause();
    }

    fn stop(&self) {
        self.sink.stop();
    }
}

type LivePump = BroadcastPlaybackPump<FfmpegBroadcastBackend, InMemoryMediaResolver>;

struct EngineRuntime {
    pump: LivePump,
    audio: Option<EngineAudio>,
    repaint: Option<egui::Context>,
    last_presented: Option<FrameNumber>,
    last_audio_frame: Option<FrameNumber>,
    last_status: Option<String>,
    /// Consecutive ticks sitting on the last carrier frame (avoid instant EOS).
    eos_ticks: u32,
    /// Decode hit EOF / cannot refill while stalled near end — finish cleanly.
    soft_eos: bool,
    /// Consecutive tick_present decode failures while Playing.
    decode_fail_streak: u32,
    /// Rodio missing — picture may still play; surfaced once on open/play.
    audio_output_missing: bool,
    width: u32,
    height: u32,
}

/// Handle used by [`crate::player_remote::PlayerRemote`].
pub struct BroadcastEngine {
    cmd_tx: Sender<EngineCommand>,
    event_rx: Receiver<EngineEvent>,
}

impl BroadcastEngine {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::sync_channel(64);
        thread::Builder::new()
            .name("qnc-broadcast-engine".into())
            .spawn(move || engine_loop(cmd_rx, event_tx))
            .expect("broadcast engine thread");
        Self { cmd_tx, event_rx }
    }

    pub fn open(&self, request: EngineOpenRequest) {
        let _ = self.cmd_tx.send(EngineCommand::Open(request));
    }

    pub fn play(&self) {
        let _ = self.cmd_tx.send(EngineCommand::Play);
    }

    pub fn pause(&self) {
        let _ = self.cmd_tx.send(EngineCommand::Pause);
    }

    pub fn seek(&self, frame: FrameNumber, still: bool) {
        let _ = self.cmd_tx.send(EngineCommand::Seek { frame, still });
    }

    pub fn stop(&self) {
        let _ = self.cmd_tx.send(EngineCommand::Stop);
    }

    pub fn poll(&self) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

impl Default for BroadcastEngine {
    fn default() -> Self {
        Self::spawn()
    }
}

fn engine_loop(cmd_rx: Receiver<EngineCommand>, event_tx: SyncSender<EngineEvent>) {
    let mut runtime: Option<EngineRuntime> = None;
    let mut playing = false;

    loop {
        // Drain commands without blocking forever when idle.
        // While playing with a healthy buffer, wake near the next frame — not every 1ms.
        let wait = if playing {
            runtime
                .as_ref()
                .and_then(|rt| rt.pump.next_frame_deadline(Instant::now()))
                .map(|d| {
                    d.min(Duration::from_millis(12))
                        .max(Duration::from_millis(1))
                })
                .unwrap_or(Duration::from_millis(2))
        } else {
            Duration::from_millis(50)
        };
        let mut cmds = Vec::new();
        match cmd_rx.recv_timeout(wait) {
            Ok(cmd) => cmds.push(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            cmds.push(cmd);
        }

        for cmd in cmds {
            match cmd {
                EngineCommand::Open(request) => {
                    playing = false;
                    match open_runtime(request) {
                        Ok(mut rt) => {
                            let now = Instant::now();
                            let frame = rt.pump.current_frame(now);
                            // Open/still cue = single-frame. Play flips continuous back on.
                            rt.pump.backend_mut().set_continuous(false);
                            rt.pump.seek(frame, now);
                            match tick_present(&mut rt, now, false, true, &event_tx) {
                                Ok(()) => {
                                    let sec = rt.pump.seconds_at_carrier_frame(frame);
                                    let _ = event_tx.send(EngineEvent::State {
                                        source_frame: frame,
                                        source_sec: sec,
                                        playing: false,
                                        status: "Ready".into(),
                                    });
                                    player_log::log_state("engine", "Ready", false, frame.0, sec);
                                    if rt.audio_output_missing
                                        && rt.pump.source().expects_media_audio_decode()
                                    {
                                        let msg = "Audio output unavailable (rodio) — picture-only";
                                        player_log::log_error("engine", msg);
                                        let _ = event_tx.send(EngineEvent::Error(msg.into()));
                                    }
                                }
                                Err(err) => {
                                    player_log::log_error("engine", format!("Open still: {err}"));
                                    let _ = event_tx.send(EngineEvent::Error(err));
                                }
                            }
                            runtime = Some(rt);
                        }
                        Err(err) => {
                            runtime = None;
                            player_log::log_error("engine", format!("Open: {err}"));
                            let _ = event_tx.send(EngineEvent::Error(err));
                        }
                    }
                }
                EngineCommand::Play => {
                    if let Some(rt) = runtime.as_mut() {
                        let now = Instant::now();
                        let frame = rt.pump.current_frame(now);
                        player_log::log_info(
                            "engine",
                            format!("Play from frame {} (preroll…)", frame.0),
                        );
                        rt.pump.backend_mut().clear_streams();
                        rt.pump.backend_mut().set_continuous(true);
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.stop();
                        }
                        rt.last_presented = None;
                        rt.last_audio_frame = None;
                        rt.eos_ticks = 0;
                        rt.soft_eos = false;
                        rt.decode_fail_streak = 0;
                        // Warm continuous pipes + queues without emitting UI frames.
                        // Do not start the clock until A/V lockstep cushion exists —
                        // otherwise the first Playing ticks stall forever ("zapne").
                        rt.pump.pause(now);
                        rt.pump.seek(frame, now);
                        let max_warm = PLAY_PREROLL_FRAMES + 48;
                        for _ in 0..max_warm {
                            let t = Instant::now();
                            if let Err(err) = tick_present(rt, t, false, false, &event_tx) {
                                player_log::log_error("engine", format!("Play preroll: {err}"));
                                let _ = event_tx.try_send(EngineEvent::Error(err));
                                break;
                            }
                            let source = rt.pump.source();
                            if play_start_ready(
                                frame,
                                source.expects_video_decode(),
                                source.expects_media_audio_decode(),
                                rt.pump.newest_video_frame(),
                                rt.pump.newest_audio_frame(),
                                PLAY_START_MIN_BUFFER_FRAMES,
                            ) {
                                break;
                            }
                        }
                        rt.last_presented = None;
                        rt.last_audio_frame = None;
                        rt.eos_ticks = 0;
                        rt.soft_eos = false;
                        rt.decode_fail_streak = 0;
                        let now = Instant::now();
                        rt.pump.play_from_frame(frame, now);
                        playing = true;
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.play();
                        } else if rt.audio_output_missing
                            && rt.pump.source().expects_media_audio_decode()
                        {
                            let msg = "Audio output unavailable (rodio) — picture-only play";
                            player_log::log_error("engine", msg);
                            let _ = event_tx.try_send(EngineEvent::Error(msg.into()));
                        }
                        let sec = rt.pump.seconds_at_carrier_frame(frame);
                        let _ = event_tx.try_send(EngineEvent::State {
                            source_frame: frame,
                            source_sec: sec,
                            playing: true,
                            status: "Playing".into(),
                        });
                        player_log::log_state("engine", "Playing", true, frame.0, sec);
                        if let Some(ctx) = &rt.repaint {
                            ctx.request_repaint();
                        }
                    }
                }
                EngineCommand::Pause => {
                    playing = false;
                    if let Some(rt) = runtime.as_mut() {
                        let now = Instant::now();
                        rt.pump.pause(now);
                        rt.pump.backend_mut().set_continuous(false);
                        rt.pump.backend_mut().clear_streams();
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.pause();
                        }
                        let frame = rt.pump.current_frame(now);
                        let sec = rt.pump.seconds_at_carrier_frame(frame);
                        let _ = event_tx.try_send(EngineEvent::State {
                            source_frame: frame,
                            source_sec: sec,
                            playing: false,
                            status: "Paused".into(),
                        });
                    }
                }
                EngineCommand::Seek { frame, still } => {
                    playing = false;
                    if let Some(rt) = runtime.as_mut() {
                        let now = Instant::now();
                        rt.pump.backend_mut().set_continuous(false);
                        rt.pump.backend_mut().clear_streams();
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.stop();
                        }
                        rt.last_presented = None;
                        rt.last_audio_frame = None;
                        rt.pump.pause(now);
                        rt.pump.seek(frame, now);
                        if still {
                            if let Err(err) = tick_present(rt, now, false, true, &event_tx) {
                                let _ = event_tx.try_send(EngineEvent::Error(err));
                            }
                        }
                        let sec = rt.pump.seconds_at_carrier_frame(frame);
                        let _ = event_tx.try_send(EngineEvent::State {
                            source_frame: frame,
                            source_sec: sec,
                            playing: false,
                            status: "Seek".into(),
                        });
                    }
                }
                EngineCommand::Stop => {
                    playing = false;
                    if let Some(mut rt) = runtime.take() {
                        rt.pump.stop();
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.stop();
                        }
                    }
                    let _ = event_tx.send(EngineEvent::Stopped);
                }
            }
        }

        if playing {
            if let Some(rt) = runtime.as_mut() {
                let now = Instant::now();
                if rt.pump.clock_state() != ClockState::Playing {
                    playing = false;
                    continue;
                }
                // Do NOT re-anchor the clock when decode lags — that snaps A/V
                // backward while rodio keeps playing and causes joint jumps.
                match tick_present(rt, now, true, true, &event_tx) {
                    Ok(()) => {
                        rt.decode_fail_streak = 0;
                        let frame = rt.pump.current_frame(Instant::now());
                        let end = rt.pump.carrier_range().end_exclusive;
                        let last = FrameNumber(end.0.saturating_sub(1));
                        // Soft EOS: last carrier frame, or stalled because decode hit EOF.
                        if soft_eos_tick_progress(
                            frame,
                            last,
                            rt.pump.is_clock_stalled(),
                            rt.soft_eos,
                        ) {
                            rt.eos_ticks = rt.eos_ticks.saturating_add(1);
                        } else {
                            rt.eos_ticks = 0;
                        }
                        if rt.eos_ticks >= SOFT_EOS_TICKS {
                            playing = false;
                            // Virtual-clip / source IN — not leave playhead on last frame.
                            rewind_to_carrier_in(rt, &event_tx, "Ready (EOS → IN)");
                        } else if let Some(ctx) = &rt.repaint {
                            ctx.request_repaint();
                        }
                    }
                    Err(err) => {
                        rt.decode_fail_streak = rt.decode_fail_streak.saturating_add(1);
                        let frame = rt.pump.current_frame(Instant::now());
                        let end = rt.pump.carrier_range().end_exclusive;
                        let last = end.0.saturating_sub(1);
                        let near_end = frame.0 + STALL_RESUME_BUFFER_FRAMES >= last;
                        let action = decode_error_action(
                            rt.decode_fail_streak,
                            near_end,
                            MAX_DECODE_RECOVER_STREAK,
                        );
                        player_log::log_error(
                            "engine",
                            format!(
                                "decode fail streak={} near_end={near_end} action={action:?}: {err}",
                                rt.decode_fail_streak
                            ),
                        );
                        // Soft recover: drop broken continuous pipes, freeze clock.
                        rt.pump.backend_mut().clear_streams();
                        rt.pump.backend_mut().set_continuous(true);
                        rt.pump.stall_at(frame);
                        if let Some(audio) = rt.audio.as_ref() {
                            audio.pause();
                        }
                        match action {
                            DecodeErrorAction::Recover => {
                                let sec = rt.pump.seconds_at_carrier_frame(frame);
                                let status = format!(
                                    "Decode recover {}/{}",
                                    rt.decode_fail_streak, MAX_DECODE_RECOVER_STREAK
                                );
                                let _ = event_tx.try_send(EngineEvent::State {
                                    source_frame: frame,
                                    source_sec: sec,
                                    playing: true,
                                    status: status.clone(),
                                });
                                player_log::log_state("engine", &status, true, frame.0, sec);
                            }
                            DecodeErrorAction::SoftEos => {
                                rt.soft_eos = true;
                                rt.eos_ticks = rt.eos_ticks.saturating_add(1);
                                let sec = rt.pump.seconds_at_carrier_frame(frame);
                                if rt.eos_ticks >= SOFT_EOS_TICKS {
                                    playing = false;
                                    rewind_to_carrier_in(rt, &event_tx, "Ready (EOF → IN)");
                                } else {
                                    let _ = event_tx.try_send(EngineEvent::State {
                                        source_frame: frame,
                                        source_sec: sec,
                                        playing: true,
                                        status: "Decode EOF — finishing".into(),
                                    });
                                }
                            }
                            DecodeErrorAction::Fatal => {
                                playing = false;
                                rt.decode_fail_streak = 0;
                                rt.pump.pause(Instant::now());
                                let _ = event_tx.try_send(EngineEvent::Error(err));
                            }
                        }
                    }
                }
            } else {
                playing = false;
            }
        }
    }
}

fn open_runtime(request: EngineOpenRequest) -> Result<EngineRuntime, String> {
    let mut resolver = InMemoryMediaResolver::new();
    for asset in request.assets {
        resolver = resolver.with_asset(asset);
    }
    let backend =
        FfmpegBroadcastBackend::new(FfmpegBroadcastConfig::default()).with_continuous(true);
    let width = backend.config().output_width;
    let height = backend.config().output_height;
    let pump = BroadcastPlaybackPump::from_timeline(
        request.source,
        request.timeline,
        backend,
        resolver,
        QUEUE_CAPACITY,
        LOOKAHEAD_FRAMES,
        ClockReference::InternalMonotonic,
    );
    let audio = EngineAudio::new();
    let audio_output_missing = audio.is_none();
    let mut runtime = EngineRuntime {
        pump,
        audio,
        repaint: request.repaint,
        last_presented: None,
        last_audio_frame: None,
        last_status: None,
        eos_ticks: 0,
        soft_eos: false,
        decode_fail_streak: 0,
        audio_output_missing,
        width,
        height,
    };
    let now = Instant::now();
    runtime.pump.seek(request.start_frame, now);
    Ok(runtime)
}

/// Soft EOS / EOF: pause and cue a still at carrier IN (virtual-clip start).
fn rewind_to_carrier_in(rt: &mut EngineRuntime, event_tx: &SyncSender<EngineEvent>, status: &str) {
    let now = Instant::now();
    let in_frame = rt.pump.carrier_range().start;
    rt.eos_ticks = 0;
    rt.soft_eos = false;
    rt.decode_fail_streak = 0;
    rt.pump.pause(now);
    rt.pump.backend_mut().set_continuous(false);
    rt.pump.backend_mut().clear_streams();
    if let Some(audio) = rt.audio.as_ref() {
        audio.stop();
    }
    rt.last_presented = None;
    rt.last_audio_frame = None;
    rt.pump.seek(in_frame, now);
    if let Err(err) = tick_present(rt, now, false, true, event_tx) {
        player_log::log_error("engine", format!("EOS rewind still: {err}"));
    }
    let sec = rt.pump.seconds_at_carrier_frame(in_frame);
    let _ = event_tx.try_send(EngineEvent::State {
        source_frame: in_frame,
        source_sec: sec,
        playing: false,
        status: status.into(),
    });
    player_log::log_state("engine", status, false, in_frame.0, sec);
    if let Some(ctx) = &rt.repaint {
        ctx.request_repaint();
    }
}

fn tick_present(
    rt: &mut EngineRuntime,
    now: Instant,
    playing: bool,
    emit: bool,
    event_tx: &SyncSender<EngineEvent>,
) -> Result<(), String> {
    // While Playing: never let the wall clock skip past decoded frames.
    // Skipping chops PCM (unintelligible speech) and jumps the picture.
    // Stall at the decode frontier, decode the next frame, then resume.
    let last_frame = {
        let end = rt.pump.carrier_range().end_exclusive;
        FrameNumber(end.0.saturating_sub(1))
    };

    if playing && emit {
        let source = rt.pump.source();
        if let Some(newest) = decode_frontier_for_stall(
            source.expects_video_decode(),
            source.expects_media_audio_decode(),
            rt.pump.newest_video_frame(),
            rt.pump.newest_audio_frame(),
        ) {
            let master = rt.pump.current_frame(now);
            if !rt.pump.is_clock_stalled() && master.0 > newest.0 {
                rt.pump.stall_at(newest);
                if let Some(audio) = rt.audio.as_ref() {
                    audio.pause();
                }
            }
        }
        emit_from_queues(rt, now, true, event_tx)?;
    }

    let tick = rt.pump.tick(now).map_err(|err| err.message)?;
    let decoded_n = tick.decoded.video_frames + tick.decoded.audio_frames;

    if playing && emit && rt.pump.is_clock_stalled() {
        let source = rt.pump.source();
        let newest = decode_frontier_for_stall(
            source.expects_video_decode(),
            source.expects_media_audio_decode(),
            rt.pump.newest_video_frame(),
            rt.pump.newest_audio_frame(),
        );
        let held = rt.pump.current_frame(Instant::now());
        // Stalled + no new frames while near carrier end ⇒ EOF soft EOS
        // (never wait forever for a resume cushion that cannot arrive).
        let near_end = newest
            .map(|n| n.0 + STALL_RESUME_BUFFER_FRAMES >= last_frame.0)
            .unwrap_or(true);
        let exhausted = carrier_decode_exhausted(newest, last_frame, 1, decoded_n);
        if exhausted && near_end {
            rt.soft_eos = true;
        } else if let Some(newest) = newest {
            if should_resume_after_stall(held, newest, STALL_RESUME_BUFFER_FRAMES) {
                let resume_at = Instant::now();
                rt.pump.resume_clock(resume_at);
                rt.soft_eos = false;
                if let Some(audio) = rt.audio.as_ref() {
                    audio.play();
                }
            }
        }
    }

    if !emit {
        return Ok(());
    }

    if let Some(hint) = diagnostics_status_hint(tick.diagnostics.problem) {
        if rt.last_status.as_deref() != Some(hint.as_str()) {
            rt.last_status = Some(hint.clone());
            let frame = rt.pump.current_frame(Instant::now());
            let sec = rt.pump.seconds_at_carrier_frame(frame);
            let _ = event_tx.try_send(EngineEvent::State {
                source_frame: frame,
                source_sec: sec,
                playing,
                status: hint,
            });
        }
    } else {
        rt.last_status = None;
    }

    // Still / scrub: queues were empty before decode — present after tick.
    if !playing {
        emit_playout_video(rt, tick.playout.as_ref(), false, event_tx)?;
    }
    Ok(())
}

fn emit_from_queues(
    rt: &mut EngineRuntime,
    now: Instant,
    playing: bool,
    event_tx: &SyncSender<EngineEvent>,
) -> Result<(), String> {
    // Contiguous PCM: emit every carrier frame from last+1..=min(master, newest).
    // Hold/exact-match discarded intermediates on clock jumps → crackle / garbled speech.
    if playing && !rt.pump.is_clock_stalled() && rt.pump.source().expects_media_audio_decode() {
        let master = rt.pump.current_frame(now);
        let newest = rt.pump.newest_audio_frame();
        let batch =
            audio_emit_range(rt.last_audio_frame, master, newest).map_err(|err| err.message)?;
        for frame in batch {
            let Some(audio_frame) = rt.pump.take_audio_frame(frame) else {
                // Gap in queue — stop this tick; stall frontier should pin master.
                break;
            };
            if let Ok((rate, ch, samples)) = mix_audio_buses(&audio_frame.payload) {
                if let Some(audio) = rt.audio.as_ref() {
                    audio.append(rate, ch, samples);
                }
                rt.last_audio_frame = Some(frame);
            }
        }
    }

    let playout = rt.pump.playout_now(now);
    emit_playout_video(rt, playout.as_ref(), playing, event_tx)
}

type LiveProgramFrame = DecodedProgramFrame<BroadcastVideoPayload, BroadcastAudioPayload>;

fn emit_playout_video(
    rt: &mut EngineRuntime,
    playout: Option<&BroadcastPlayoutFrame<LiveProgramFrame>>,
    playing: bool,
    event_tx: &SyncSender<EngineEvent>,
) -> Result<(), String> {
    let Some(playout) = playout else {
        return Ok(());
    };

    let program = match &playout.video {
        PlayoutVideo::Ready(queued) => &queued.payload,
        PlayoutVideo::HoldPrevious(_) | PlayoutVideo::Missing | PlayoutVideo::NoVideoExpected => {
            return Ok(());
        }
    };

    if rt.last_presented == Some(program.source_frame) {
        return Ok(());
    }

    let image =
        composite_video_layers(&program.video, rt.width, rt.height).map_err(|err| err.message)?;

    rt.last_presented = Some(program.source_frame);
    let source_sec = rt.pump.seconds_at_carrier_frame(program.source_frame);
    let _ = event_tx.try_send(EngineEvent::Frame {
        image,
        source_frame: program.source_frame,
        source_sec,
        playing,
    });
    Ok(())
}

/// Probe media + build default source open payload for the engine.
pub fn probe_open_assets(
    source_ref: &super::BroadcastHostSourceRef,
    media_input: &str,
    fallback_fps: f64,
    has_audio_hint: bool,
    audio_channels_hint: u8,
) -> Result<
    (
        BroadcastPlaybackSource,
        Vec<BroadcastMediaAsset>,
        UniversalTimelineSpec,
    ),
    String,
> {
    if media_input.trim().is_empty() {
        return Err("broadcast engine: media path missing".into());
    }
    let seed = if media_input.starts_with("http://") || media_input.starts_with("https://") {
        source_ref.proxy_url_seed(media_input.to_string())
    } else {
        source_ref.proxy_local_seed(std::path::PathBuf::from(media_input))
    };

    let probe = FfprobeMediaProbe::default();
    let asset = match probe.probe_asset_seed(seed.clone()) {
        Ok(asset) => asset,
        Err(_) => {
            // Degraded: UI FPS fallback (not broadcast truth).
            let tb =
                super::Timebase::try_from_source_fps(fallback_fps).map_err(|err| err.message)?;
            seed.with_probe_report(super::BroadcastMediaProbeReport {
                source_timebase: tb,
                has_video: true,
                has_audio: has_audio_hint,
                audio_channels: if has_audio_hint {
                    audio_channels_hint.max(2).min(4)
                } else {
                    0
                },
                audio_stream_count: if has_audio_hint { 1 } else { 0 },
                video_width: None,
                video_height: None,
            })
        }
    };

    let source = source_ref
        .playback_source_from_asset(&asset)
        .map_err(|err| err.message)?;
    let timeline = strip_filmstrip(source_spec_for_playback(&source));
    Ok((source, vec![asset], timeline))
}
