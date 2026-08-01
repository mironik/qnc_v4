//! Legacy continuous-pipe player — **not on the product path**.
//!
//! Product decode owner is [`crate::broadcast::BroadcastTransport`] /
//! [`crate::broadcast::BroadcastPlaybackPump`]. This module remains for
//! reference and unit tests around seek relativity; do not wire it into UI.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, Sink};

use crate::broadcast::{
    BroadcastHostSourceRef, BroadcastMasterClock, BroadcastMediaProbeReport,
    BroadcastPlaybackSource, ClockReference, DecodeHwaccel, FrameNumber, Timebase,
};

// Re-export for any legacy callers; product path uses `broadcast::configure_…`.
#[allow(unused_imports)]
pub use crate::broadcast::configure_player_hwaccel_from_host_profile;

const PREVIEW_WIDTH: u32 = 960;
const PREVIEW_HEIGHT: u32 = 540;
/// Still / goto cue — smaller than play stream for snappy click-to-playhead.
const STILL_WIDTH: u32 = 640;
const STILL_HEIGHT: u32 = 360;
const VIDEO_QUEUE_CAPACITY: usize = 18;
const VIDEO_START_PREROLL_FRAMES: usize = 1;
const AUDIO_CHUNK_FRAMES: usize = 2_048;
const AUDIO_QUEUE_CHUNKS: usize = 8;
const BROADCAST_AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;
const STARTUP_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

fn player_hwaccel() -> DecodeHwaccel {
    crate::broadcast::hwaccel::player_hwaccel()
}

fn push_hwaccel_args(args: &mut Vec<String>) {
    crate::broadcast::hwaccel::push_hwaccel_args(args, player_hwaccel());
}

#[derive(Debug, Clone)]
pub struct NativePlayerSourceRequest {
    pub source_ref: BroadcastHostSourceRef,
    /// Local absolute media path (preferred). HTTP URL only for true remote.
    pub media_input: String,
    pub source_fps: f64,
    pub has_audio: bool,
    pub audio_channels: u8,
    pub start_source_sec: f64,
    pub repaint: Option<egui::Context>,
}

#[derive(Debug, Clone)]
pub struct NativePlayerFrame {
    pub image: ColorImage,
    pub source_frame: FrameNumber,
    pub source_sec: f64,
    pub playing: bool,
}

#[derive(Debug, Clone)]
pub struct NativePlayerState {
    pub source_frame: FrameNumber,
    pub source_sec: f64,
    pub playing: bool,
}

#[derive(Debug, Clone)]
pub enum NativePlayerEvent {
    Loaded {
        source: BroadcastPlaybackSource,
        frame: NativePlayerFrame,
    },
    Started {
        source: BroadcastPlaybackSource,
        state: NativePlayerState,
    },
    Frame(NativePlayerFrame),
    State(NativePlayerState),
    Error(String),
    Stopped,
}

pub struct NativePlayer {
    tx: Sender<NativePlayerCommand>,
    event_tx: Sender<NativePlayerMessage>,
    rx: Receiver<NativePlayerMessage>,
    next_request_id: u64,
    latest_request_id: u64,
    active: bool,
    source: Option<BroadcastPlaybackSource>,
    /// Last accepted source request — used by seek so Story never owns decode.
    last_request: Option<NativePlayerSourceRequest>,
    /// Cancel + kill handle for in-flight still cue ffmpeg.
    cue_cancel: Arc<AtomicBool>,
    cue_child: Arc<Mutex<Option<Child>>>,
}

enum NativePlayerCommand {
    Start {
        request_id: u64,
        request: NativePlayerSourceRequest,
    },
    Pause {
        request_id: u64,
    },
    Stop {
        request_id: u64,
    },
    StopForCue {
        request_id: u64,
    },
}

#[derive(Debug, Clone)]
enum NativePlayerMessage {
    Loaded {
        request_id: u64,
        source: BroadcastPlaybackSource,
        frame: NativePlayerFrame,
    },
    Started {
        request_id: u64,
        source: BroadcastPlaybackSource,
        state: NativePlayerState,
    },
    Frame {
        request_id: u64,
        frame: NativePlayerFrame,
    },
    State {
        request_id: u64,
        state: NativePlayerState,
    },
    Error {
        request_id: u64,
        message: String,
    },
    Stopped {
        request_id: u64,
    },
}

impl NativePlayer {
    pub fn new() -> Self {
        let (tx_cmd, rx_cmd) = mpsc::channel();
        let (tx_evt, rx_evt) = mpsc::channel();
        let worker_tx = tx_evt.clone();
        let _ = thread::Builder::new()
            .name("qnc-native-player".into())
            .spawn(move || native_player_worker(rx_cmd, worker_tx));
        Self {
            tx: tx_cmd,
            event_tx: tx_evt,
            rx: rx_evt,
            next_request_id: 0,
            latest_request_id: 0,
            active: false,
            source: None,
            last_request: None,
            cue_cancel: Arc::new(AtomicBool::new(false)),
            cue_child: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&mut self, request: NativePlayerSourceRequest) -> Result<(), String> {
        self.cancel_cue_job();
        self.last_request = Some(request.clone());
        let request_id = self.alloc_request_id();
        self.tx
            .send(NativePlayerCommand::Start {
                request_id,
                request,
            })
            .map_err(|err| format!("native player command: {err}"))?;
        self.active = true;
        Ok(())
    }

    /// Seek to request.start_source_sec. Playing → restart decode. Paused → still frame.
    /// Story/timeline only issues this command; they do not own the clock.
    pub fn seek(&mut self, request: NativePlayerSourceRequest) -> Result<(), String> {
        if self.active {
            self.start(request)
        } else {
            self.cue(request)
        }
    }

    pub fn cue(&mut self, request: NativePlayerSourceRequest) -> Result<(), String> {
        self.cancel_cue_job();
        self.last_request = Some(request.clone());
        let request_id = self.alloc_request_id();
        self.tx
            .send(NativePlayerCommand::StopForCue { request_id })
            .map_err(|err| format!("native player command: {err}"))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        self.cue_cancel = Arc::clone(&cancel);
        self.cue_child = Arc::clone(&child_slot);
        let tx = self.event_tx.clone();
        thread::Builder::new()
            .name("qnc-native-preview-cue".into())
            .spawn(move || {
                let message = match load_preview_frame(request, &cancel, &child_slot) {
                    Ok((source, frame)) => NativePlayerMessage::Loaded {
                        request_id,
                        source,
                        frame,
                    },
                    Err(message) if message == "cancelled" => {
                        return;
                    }
                    Err(message) => NativePlayerMessage::Error {
                        request_id,
                        message,
                    },
                };
                let _ = tx.send(message);
            })
            .map_err(|err| format!("native preview cue thread: {err}"))?;
        self.active = false;
        Ok(())
    }

    fn cancel_cue_job(&mut self) {
        self.cue_cancel.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.cue_child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    pub fn pause(&mut self) {
        self.cancel_cue_job();
        let request_id = self.alloc_request_id();
        let _ = self.tx.send(NativePlayerCommand::Pause { request_id });
        self.active = false;
    }

    pub fn stop(&mut self) {
        self.cancel_cue_job();
        let request_id = self.alloc_request_id();
        let _ = self.tx.send(NativePlayerCommand::Stop { request_id });
        self.active = false;
        self.source = None;
        self.last_request = None;
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn poll(&mut self) -> Vec<NativePlayerEvent> {
        let mut out = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            if message.request_id() != self.latest_request_id {
                continue;
            }
            let event = message.into_event();
            match &event {
                NativePlayerEvent::Loaded { source, .. } => {
                    self.active = false;
                    self.source = Some(source.clone());
                }
                NativePlayerEvent::Started { source, .. } => {
                    self.active = true;
                    self.source = Some(source.clone());
                }
                NativePlayerEvent::Frame(frame) => {
                    self.active = frame.playing;
                }
                NativePlayerEvent::State(state) => {
                    self.active = state.playing;
                }
                NativePlayerEvent::Error(_) | NativePlayerEvent::Stopped => {
                    self.active = false;
                }
            }
            out.push(event);
        }
        out
    }

    fn alloc_request_id(&mut self) -> u64 {
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.latest_request_id = self.next_request_id;
        self.next_request_id
    }
}

impl Default for NativePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePlayerMessage {
    fn request_id(&self) -> u64 {
        match self {
            Self::Loaded { request_id, .. }
            | Self::Started { request_id, .. }
            | Self::Frame { request_id, .. }
            | Self::State { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Stopped { request_id } => *request_id,
        }
    }

    fn into_event(self) -> NativePlayerEvent {
        match self {
            Self::Loaded { source, frame, .. } => NativePlayerEvent::Loaded { source, frame },
            Self::Started { source, state, .. } => NativePlayerEvent::Started { source, state },
            Self::Frame { frame, .. } => NativePlayerEvent::Frame(frame),
            Self::State { state, .. } => NativePlayerEvent::State(state),
            Self::Error { message, .. } => NativePlayerEvent::Error(message),
            Self::Stopped { .. } => NativePlayerEvent::Stopped,
        }
    }
}

struct PlayerRuntime {
    request_id: u64,
    source: BroadcastPlaybackSource,
    clock: BroadcastMasterClock,
    start_frame: FrameNumber,
    clock_started: bool,
    started_at: Instant,
    video_rx: Receiver<DecodedVideoFrame>,
    audio_rx: Option<Receiver<DecodedAudioChunk>>,
    decode: NativeDecodeHandle,
    audio: Option<NativeAudioOutput>,
    video_queue: VecDeque<DecodedVideoFrame>,
    last_presented: Option<FrameNumber>,
    repaint: Option<egui::Context>,
    playing: bool,
}

fn native_player_worker(rx: Receiver<NativePlayerCommand>, tx: Sender<NativePlayerMessage>) {
    let mut runtime: Option<PlayerRuntime> = None;

    loop {
        if runtime.as_ref().map(|rt| rt.playing).unwrap_or(false) {
            while let Ok(cmd) = rx.try_recv() {
                handle_command(cmd, &mut runtime, &tx);
            }
            if let Some(rt) = runtime.as_mut() {
                tick_runtime(rt, &tx);
                let sleep = rt
                    .clock
                    .next_frame_deadline(Instant::now())
                    .unwrap_or_else(|| Duration::from_millis(10))
                    .min(Duration::from_millis(10));
                thread::sleep(sleep);
            }
        } else {
            match rx.recv() {
                Ok(cmd) => handle_command(cmd, &mut runtime, &tx),
                Err(_) => return,
            }
        }
    }
}

fn handle_command(
    cmd: NativePlayerCommand,
    runtime: &mut Option<PlayerRuntime>,
    tx: &Sender<NativePlayerMessage>,
) {
    match cmd {
        NativePlayerCommand::Start {
            request_id,
            request,
        } => {
            stop_runtime(runtime);
            match PlayerRuntime::start(request_id, request) {
                Ok(next) => {
                    let now = Instant::now();
                    let state = runtime_state(&next, now);
                    let source = next.source.clone();
                    let _ = tx.send(NativePlayerMessage::Started {
                        request_id,
                        source,
                        state,
                    });
                    *runtime = Some(next);
                }
                Err(err) => {
                    *runtime = None;
                    let _ = tx.send(NativePlayerMessage::Error {
                        request_id,
                        message: err,
                    });
                }
            }
        }
        NativePlayerCommand::Pause { request_id } | NativePlayerCommand::Stop { request_id } => {
            stop_runtime(runtime);
            let _ = tx.send(NativePlayerMessage::Stopped { request_id });
        }
        NativePlayerCommand::StopForCue { request_id } => {
            let _ = request_id;
            stop_runtime(runtime);
        }
    }
}

fn stop_runtime(runtime: &mut Option<PlayerRuntime>) {
    if let Some(mut rt) = runtime.take() {
        rt.playing = false;
        rt.decode.stop();
        if let Some(audio) = rt.audio.as_ref() {
            audio.stop();
        }
    }
}

fn load_preview_frame(
    request: NativePlayerSourceRequest,
    cancel: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<(BroadcastPlaybackSource, NativePlayerFrame), String> {
    if request.media_input.trim().is_empty() {
        return Err("native player: media path missing".into());
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let source = resolve_player_source(&request)?;
    let source_frame = source.source_range.clamp(
        source
            .source_timebase
            .frame_at_seconds(request.start_source_sec),
    );
    let decoded = decode_preview_frame(
        &request.media_input,
        &source,
        source_frame,
        cancel,
        child_slot,
    )?;
    let image = rgba_to_color_image(&decoded)?;
    let frame = NativePlayerFrame {
        image,
        source_frame,
        source_sec: source.source_timebase.seconds_at_frame(source_frame),
        playing: false,
    };
    if let Some(ctx) = request.repaint {
        ctx.request_repaint();
    }
    Ok((source, frame))
}

impl PlayerRuntime {
    fn start(request_id: u64, request: NativePlayerSourceRequest) -> Result<Self, String> {
        if request.media_input.trim().is_empty() {
            return Err("native player: media path missing".into());
        }
        let source = resolve_player_source(&request)?;
        let start_frame = source.source_range.clamp(
            source
                .source_timebase
                .frame_at_seconds(request.start_source_sec),
        );
        let clock = BroadcastMasterClock::new(
            source.source_timebase,
            source.source_range,
            ClockReference::InternalMonotonic,
        );

        let audio = if source.program_audio_buses() > 0 {
            NativeAudioOutput::new().ok()
        } else {
            None
        };
        let (video_tx, video_rx) = mpsc::sync_channel(VIDEO_QUEUE_CAPACITY);
        let (audio_tx, audio_rx) = if audio.is_some() {
            let (tx, rx) = mpsc::sync_channel(AUDIO_QUEUE_CHUNKS);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let decode = NativeDecodeHandle::spawn(
            &request.media_input,
            &source,
            start_frame,
            video_tx,
            audio_tx,
        )?;

        Ok(Self {
            request_id,
            source,
            clock,
            start_frame,
            clock_started: false,
            started_at: Instant::now(),
            video_rx,
            audio_rx,
            decode,
            audio,
            video_queue: VecDeque::with_capacity(VIDEO_QUEUE_CAPACITY),
            last_presented: None,
            repaint: request.repaint,
            playing: true,
        })
    }
}

fn decode_preview_frame(
    media_input: &str,
    source: &BroadcastPlaybackSource,
    source_frame: FrameNumber,
    cancel: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
) -> Result<DecodedVideoFrame, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let spec = preview_video_command(media_input, source, source_frame);
    let mut child = spawn_ffmpeg(&spec)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "native preview ffmpeg: missing stdout".to_string())?;
    if let Ok(mut guard) = child_slot.lock() {
        *guard = Some(child);
    } else {
        let _ = child.kill();
        return Err("native preview ffmpeg: child lock poisoned".into());
    }

    let frame_bytes = still_frame_bytes()?;
    let mut buf = vec![0_u8; frame_bytes];
    let mut filled = 0_usize;
    while filled < frame_bytes {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(mut guard) = child_slot.lock() {
                if let Some(mut child) = guard.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            return Err("cancelled".into());
        }
        match stdout.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                if let Ok(mut guard) = child_slot.lock() {
                    if let Some(mut child) = guard.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                return Err(format!("native preview ffmpeg read: {err}"));
            }
        }
    }

    let status = if let Ok(mut guard) = child_slot.lock() {
        match guard.take() {
            Some(mut child) => child
                .wait()
                .map_err(|err| format!("native preview ffmpeg wait: {err}"))?,
            None => {
                return Err("cancelled".into());
            }
        }
    } else {
        return Err("native preview ffmpeg: child lock poisoned".into());
    };

    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    if !status.success() {
        return Err(format!("native preview ffmpeg failed ({status})"));
    }
    if filled < frame_bytes {
        return Err(format!(
            "native preview ffmpeg: decoded {filled} bytes, expected at least {frame_bytes}"
        ));
    }
    Ok(DecodedVideoFrame {
        source_frame,
        width: STILL_WIDTH,
        height: STILL_HEIGHT,
        rgba: buf,
    })
}

fn tick_runtime(rt: &mut PlayerRuntime, tx: &Sender<NativePlayerMessage>) {
    drain_video_decoder(rt);
    drain_audio_decoder(rt);

    let now = Instant::now();
    if !rt.clock_started {
        let remaining_frames = (rt.source.source_range.end_exclusive.0 - rt.start_frame.0).max(1);
        let preroll_frames = (remaining_frames as usize).min(VIDEO_START_PREROLL_FRAMES);
        if rt.video_queue.len() < preroll_frames
            && now.saturating_duration_since(rt.started_at) < STARTUP_FRAME_TIMEOUT
        {
            let _ = tx.send(NativePlayerMessage::State {
                request_id: rt.request_id,
                state: NativePlayerState {
                    source_frame: rt.start_frame,
                    source_sec: rt.source.source_timebase.seconds_at_frame(rt.start_frame),
                    playing: rt.playing,
                },
            });
            return;
        }
        let Some(frame) = rt.video_queue.pop_front() else {
            if now.saturating_duration_since(rt.started_at) >= STARTUP_FRAME_TIMEOUT {
                rt.playing = false;
                rt.decode.stop();
                if let Some(audio) = rt.audio.as_ref() {
                    audio.stop();
                }
                let _ = tx.send(NativePlayerMessage::Error {
                    request_id: rt.request_id,
                    message: "native player: decoder did not return first video frame".into(),
                });
            } else {
                let _ = tx.send(NativePlayerMessage::State {
                    request_id: rt.request_id,
                    state: NativePlayerState {
                        source_frame: rt.start_frame,
                        source_sec: rt.source.source_timebase.seconds_at_frame(rt.start_frame),
                        playing: rt.playing,
                    },
                });
            }
            return;
        };
        rt.clock.play_from(frame.source_frame, now);
        rt.clock_started = true;
        if let Some(audio) = rt.audio.as_ref() {
            audio.play();
        }
        present_video_frame(rt, frame, tx);
        return;
    }

    let current_frame = rt.clock.current_frame(now);
    let Some(frame) = video_frame_for_clock(&mut rt.video_queue, current_frame) else {
        let _ = tx.send(NativePlayerMessage::State {
            request_id: rt.request_id,
            state: runtime_state(rt, now),
        });
        return;
    };

    if rt.last_presented == Some(frame.source_frame) {
        return;
    }
    present_video_frame(rt, frame, tx);
}

fn present_video_frame(
    rt: &mut PlayerRuntime,
    frame: DecodedVideoFrame,
    tx: &Sender<NativePlayerMessage>,
) {
    let source_sec = rt
        .source
        .source_timebase
        .seconds_at_frame(frame.source_frame);
    let image = match rgba_to_color_image(&frame) {
        Ok(image) => image,
        Err(err) => {
            rt.playing = false;
            rt.decode.stop();
            let _ = tx.send(NativePlayerMessage::Error {
                request_id: rt.request_id,
                message: err,
            });
            return;
        }
    };
    rt.last_presented = Some(frame.source_frame);
    let playing = frame.source_frame.0 < rt.source.source_range.end_exclusive.0 - 1;
    rt.playing = playing;
    let _ = tx.send(NativePlayerMessage::Frame {
        request_id: rt.request_id,
        frame: NativePlayerFrame {
            image,
            source_frame: frame.source_frame,
            source_sec,
            playing,
        },
    });
    if let Some(ctx) = &rt.repaint {
        ctx.request_repaint();
    }
    if !playing {
        rt.decode.stop();
        if let Some(audio) = rt.audio.as_ref() {
            audio.stop();
        }
        let _ = tx.send(NativePlayerMessage::Stopped {
            request_id: rt.request_id,
        });
    }
}

fn runtime_state(rt: &PlayerRuntime, now: Instant) -> NativePlayerState {
    let frame = rt.clock.current_frame(now);
    NativePlayerState {
        source_frame: frame,
        source_sec: rt.source.source_timebase.seconds_at_frame(frame),
        playing: rt.playing,
    }
}

fn resolve_player_source(
    request: &NativePlayerSourceRequest,
) -> Result<BroadcastPlaybackSource, String> {
    let timebase = Timebase::try_from_source_fps(request.source_fps)
        .map_err(|err| format!("native player source fps: {}", err.message))?;
    // Program buses: A1+A2 minimum, A3/A4 optional (never a single mono bus).
    let audio_buses = if request.has_audio {
        request.audio_channels.max(2).min(4)
    } else {
        2
    };
    let seed = if media_input_is_remote(&request.media_input) {
        request
            .source_ref
            .proxy_url_seed(request.media_input.to_string())
    } else {
        request
            .source_ref
            .proxy_local_seed(std::path::PathBuf::from(&request.media_input))
    };
    let asset = seed.with_probe_report(BroadcastMediaProbeReport {
        source_timebase: timebase,
        has_video: true,
        has_audio: request.has_audio,
        audio_channels: audio_buses,
        audio_stream_count: if request.has_audio { 1 } else { 0 },
        video_width: None,
        video_height: None,
    });
    request
        .source_ref
        .playback_source_from_asset(&asset)
        .map_err(|err| err.message)
}

fn drain_video_decoder(rt: &mut PlayerRuntime) {
    drain_video_decoder_queue(&rt.video_rx, &mut rt.video_queue);
}

fn drain_video_decoder_queue(
    rx: &Receiver<DecodedVideoFrame>,
    queue: &mut VecDeque<DecodedVideoFrame>,
) {
    while queue.len() < VIDEO_QUEUE_CAPACITY {
        let Ok(frame) = rx.try_recv() else {
            break;
        };
        queue.push_back(frame);
    }
}

fn drain_audio_decoder(rt: &mut PlayerRuntime) {
    let (Some(rx), Some(audio)) = (rt.audio_rx.as_ref(), rt.audio.as_ref()) else {
        return;
    };
    while audio.queued_chunks() < AUDIO_QUEUE_CHUNKS {
        match rx.try_recv() {
            Ok(chunk) => audio.append(chunk),
            Err(_) => break,
        }
    }
}

fn video_frame_for_clock(
    queue: &mut VecDeque<DecodedVideoFrame>,
    master_frame: FrameNumber,
) -> Option<DecodedVideoFrame> {
    while queue.len() > 1 {
        let Some(next) = queue.get(1) else {
            break;
        };
        if next.source_frame <= master_frame {
            queue.pop_front();
        } else {
            break;
        }
    }
    queue
        .front()
        .filter(|frame| frame.source_frame <= master_frame)
        .cloned()
}

#[derive(Debug, Clone)]
struct DecodedVideoFrame {
    source_frame: FrameNumber,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Debug, Clone)]
struct DecodedAudioChunk {
    sample_rate_hz: u32,
    channels: u8,
    samples: Vec<f32>,
}

struct NativeAudioOutput {
    _stream: OutputStream,
    sink: Sink,
}

impl NativeAudioOutput {
    fn new() -> Result<Self, String> {
        let (stream, handle) =
            OutputStream::try_default().map_err(|err| format!("native audio output: {err}"))?;
        let sink = Sink::try_new(&handle).map_err(|err| format!("native audio sink: {err}"))?;
        sink.pause();
        Ok(Self {
            _stream: stream,
            sink,
        })
    }

    fn append(&self, chunk: DecodedAudioChunk) {
        if chunk.samples.is_empty() || chunk.channels == 0 {
            return;
        }
        self.sink.append(SamplesBuffer::new(
            chunk.channels as u16,
            chunk.sample_rate_hz,
            chunk.samples,
        ));
        self.sink.play();
    }

    fn queued_chunks(&self) -> usize {
        self.sink.len()
    }

    fn play(&self) {
        self.sink.play();
    }

    fn stop(&self) {
        self.sink.stop();
    }
}

struct NativeDecodeHandle {
    video: DecoderProcess,
    audio: Option<DecoderProcess>,
}

impl NativeDecodeHandle {
    fn spawn(
        media_input: &str,
        source: &BroadcastPlaybackSource,
        start_frame: FrameNumber,
        video_tx: SyncSender<DecodedVideoFrame>,
        audio_tx: Option<SyncSender<DecodedAudioChunk>>,
    ) -> Result<Self, String> {
        let relative_seek_sec = relative_seek_seconds(source, start_frame);
        let duration_sec = remaining_seconds(source, start_frame);
        let video = spawn_video_decoder(
            media_input,
            source.source_timebase,
            start_frame,
            source.source_range.end_exclusive,
            relative_seek_sec,
            duration_sec,
            video_tx,
        )?;
        let audio = match audio_tx {
            Some(tx) => {
                // Always decode into the program bus count (A1+A2 min, up to A4).
                let buses = source.program_audio_buses();
                spawn_audio_decoder(media_input, buses, relative_seek_sec, duration_sec, tx).ok()
            }
            _ => None,
        };
        Ok(Self { video, audio })
    }

    fn stop(&mut self) {
        self.video.stop();
        if let Some(audio) = self.audio.as_mut() {
            audio.stop();
        }
    }
}

impl Drop for NativeDecodeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct DecoderProcess {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl DecoderProcess {
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

fn spawn_video_decoder(
    media_input: &str,
    timebase: Timebase,
    start_frame: FrameNumber,
    end_exclusive: FrameNumber,
    relative_seek_sec: f64,
    duration_sec: f64,
    tx: SyncSender<DecodedVideoFrame>,
) -> Result<DecoderProcess, String> {
    let spec = video_command(media_input, timebase, relative_seek_sec, duration_sec);
    let mut child = spawn_ffmpeg(&spec)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "native video decoder: stdout missing".to_string())?;
    let process = DecoderProcess {
        stop: Arc::new(AtomicBool::new(false)),
        child: Arc::new(Mutex::new(Some(child))),
    };
    let worker = process.clone();
    thread::Builder::new()
        .name("qnc-native-video-decode".into())
        .spawn(move || {
            let Ok(frame_bytes) = preview_frame_bytes() else {
                wait_child(&worker.child);
                return;
            };
            let mut source_frame = start_frame;
            while source_frame < end_exclusive && !worker.stop.load(Ordering::SeqCst) {
                let mut rgba = vec![0_u8; frame_bytes];
                match stdout.read_exact(&mut rgba) {
                    Ok(()) => {
                        if tx
                            .send(DecodedVideoFrame {
                                source_frame,
                                width: PREVIEW_WIDTH,
                                height: PREVIEW_HEIGHT,
                                rgba,
                            })
                            .is_err()
                        {
                            break;
                        }
                        source_frame = FrameNumber(source_frame.0 + 1);
                    }
                    Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(_) => break,
                }
            }
            wait_child(&worker.child);
        })
        .map_err(|err| format!("native video decoder thread: {err}"))?;
    Ok(process)
}

fn spawn_audio_decoder(
    media_input: &str,
    channels: u8,
    relative_seek_sec: f64,
    duration_sec: f64,
    tx: SyncSender<DecodedAudioChunk>,
) -> Result<DecoderProcess, String> {
    let spec = audio_command(media_input, channels, relative_seek_sec, duration_sec);
    let mut child = spawn_ffmpeg(&spec)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "native audio decoder: stdout missing".to_string())?;
    let process = DecoderProcess {
        stop: Arc::new(AtomicBool::new(false)),
        child: Arc::new(Mutex::new(Some(child))),
    };
    let worker = process.clone();
    thread::Builder::new()
        .name("qnc-native-audio-decode".into())
        .spawn(move || {
            let chunk_samples = AUDIO_CHUNK_FRAMES.saturating_mul(channels as usize);
            let chunk_bytes = chunk_samples.saturating_mul(std::mem::size_of::<f32>());
            while !worker.stop.load(Ordering::SeqCst) {
                let mut bytes = vec![0_u8; chunk_bytes];
                match stdout.read_exact(&mut bytes) {
                    Ok(()) => {
                        let samples = bytes
                            .chunks_exact(4)
                            .map(|chunk| {
                                f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                            })
                            .collect::<Vec<_>>();
                        if tx
                            .send(DecodedAudioChunk {
                                sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                                channels,
                                samples,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(_) => break,
                }
            }
            wait_child(&worker.child);
        })
        .map_err(|err| format!("native audio decoder thread: {err}"))?;
    Ok(process)
}

#[derive(Debug, Clone, PartialEq)]
struct NativeFfmpegCommand {
    program: String,
    args: Vec<String>,
}

fn video_command(
    media_input: &str,
    timebase: Timebase,
    relative_seek_sec: f64,
    duration_sec: f64,
) -> NativeFfmpegCommand {
    let vf = format!(
        "fps={}/{},scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format=rgba",
        timebase.num,
        timebase.den,
        PREVIEW_WIDTH,
        PREVIEW_HEIGHT,
        PREVIEW_WIDTH,
        PREVIEW_HEIGHT
    );
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
    ];
    push_hwaccel_args(&mut args);
    // Broadcast: input seek (-ss before -i) for snappy scrub/play start.
    args.extend([
        "-ss".into(),
        format_seconds(relative_seek_sec),
        "-i".into(),
        media_input.to_string(),
        "-t".into(),
        format_seconds(duration_sec),
        "-an".into(),
        "-vf".into(),
        vf,
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    NativeFfmpegCommand {
        program: ffmpeg_program(),
        args,
    }
}

fn preview_video_command(
    media_input: &str,
    source: &BroadcastPlaybackSource,
    source_frame: FrameNumber,
) -> NativeFfmpegCommand {
    let relative_seek_sec = relative_seek_seconds(source, source_frame);
    let vf = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format=rgba",
        STILL_WIDTH, STILL_HEIGHT, STILL_WIDTH, STILL_HEIGHT
    );
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        // Fast keyframe seek for goto / scrub (accuracy traded for snappiness).
        "-fflags".into(),
        "+fastseek".into(),
    ];
    push_hwaccel_args(&mut args);
    args.extend([
        "-ss".into(),
        format_seconds(relative_seek_sec),
        "-i".into(),
        media_input.to_string(),
        "-frames:v".into(),
        "1".into(),
        "-an".into(),
        "-vf".into(),
        vf,
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    NativeFfmpegCommand {
        program: ffmpeg_program(),
        args,
    }
}

fn audio_command(
    media_input: &str,
    channels: u8,
    relative_seek_sec: f64,
    duration_sec: f64,
) -> NativeFfmpegCommand {
    NativeFfmpegCommand {
        program: ffmpeg_program(),
        args: vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-ss".into(),
            format_seconds(relative_seek_sec),
            "-i".into(),
            media_input.to_string(),
            "-t".into(),
            format_seconds(duration_sec),
            "-vn".into(),
            "-f".into(),
            "f32le".into(),
            "-acodec".into(),
            "pcm_f32le".into(),
            "-ar".into(),
            BROADCAST_AUDIO_SAMPLE_RATE_HZ.to_string(),
            "-ac".into(),
            // Program bus count: A1+A2 minimum, A3/A4 optional.
            channels.clamp(2, 4).to_string(),
            "pipe:1".into(),
        ],
    }
}

fn spawn_ffmpeg(spec: &NativeFfmpegCommand) -> Result<Child, String> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map_err(|err| format!("native ffmpeg start: {err}"))
}

fn ffmpeg_failure(context: &str, status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        format!("{context} failed ({status})")
    } else {
        format!("{context} failed ({status}): {message}")
    }
}

fn preview_frame_bytes() -> Result<usize, String> {
    (PREVIEW_WIDTH as usize)
        .checked_mul(PREVIEW_HEIGHT as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native player: frame size overflow".to_string())
}

fn still_frame_bytes() -> Result<usize, String> {
    (STILL_WIDTH as usize)
        .checked_mul(STILL_HEIGHT as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native player: still frame size overflow".to_string())
}

fn wait_child(child: &Arc<Mutex<Option<Child>>>) {
    let child = child.lock().ok().and_then(|mut guard| guard.take());
    if let Some(mut child) = child {
        let _ = child.wait();
    }
}

fn relative_seek_seconds(source: &BroadcastPlaybackSource, start_frame: FrameNumber) -> f64 {
    let frame_offset =
        (source.source_range.clamp(start_frame).0 - source.source_range.start.0).max(0);
    source
        .source_timebase
        .seconds_at_frame(FrameNumber(frame_offset))
}

fn media_input_is_remote(media_input: &str) -> bool {
    let s = media_input.trim();
    s.starts_with("http://") || s.starts_with("https://")
}

fn remaining_seconds(source: &BroadcastPlaybackSource, start_frame: FrameNumber) -> f64 {
    let start = source.source_range.clamp(start_frame);
    let frames = (source.source_range.end_exclusive.0 - start.0).max(1);
    source.source_timebase.seconds_at_frame(FrameNumber(frames))
}

fn rgba_to_color_image(frame: &DecodedVideoFrame) -> Result<ColorImage, String> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native player: frame size overflow".to_string())?;
    if frame.rgba.len() != expected {
        return Err(format!(
            "native player: video frame has {} bytes, expected {expected}",
            frame.rgba.len()
        ));
    }
    Ok(ColorImage::from_rgba_unmultiplied(
        [width, height],
        &frame.rgba,
    ))
}

fn format_seconds(seconds: f64) -> String {
    format!("{:.6}", seconds.max(0.0))
}

fn ffmpeg_program() -> String {
    std::env::var("QNC_FFMPEG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ffmpeg".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::{BroadcastMediaProbeReport, FrameRange};

    fn source() -> BroadcastPlaybackSource {
        let source_ref = BroadcastHostSourceRef::from_story_fields(
            "project",
            "shot",
            "",
            "clip",
            Some(5.0),
            Some(8.0),
            20.0,
        )
        .unwrap();
        let asset = source_ref
            .proxy_url_seed("http://127.0.0.1/media")
            .with_probe_report(BroadcastMediaProbeReport {
                source_timebase: Timebase::from_source_rate(25, 1).unwrap(),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
                audio_stream_count: 1,
                video_width: Some(1920),
                video_height: Some(1080),
            });
        source_ref.playback_source_from_asset(&asset).unwrap()
    }

    #[test]
    fn player_seek_is_relative_to_virtual_source_start() {
        let source = source();
        assert_eq!(
            source.source_range,
            FrameRange::new(FrameNumber(125), FrameNumber(200))
        );
        assert!((relative_seek_seconds(&source, FrameNumber(150)) - 1.0).abs() < 0.000_001);
        assert!((remaining_seconds(&source, FrameNumber(150)) - 2.0).abs() < 0.000_001);
    }

    #[test]
    fn video_command_uses_source_timebase_not_hardcoded_fps() {
        let cmd = video_command(
            "http://127.0.0.1/stream",
            Timebase::from_source_rate(30_000, 1_001).unwrap(),
            1.25,
            3.0,
        );
        let args = cmd.args.join(" ");
        assert!(args.contains("fps=30000/1001"));
        assert!(args.contains("1.250000"));
    }

    #[test]
    fn startup_clock_waits_for_first_decoded_frame() {
        let source = source();
        let mut clock = BroadcastMasterClock::new(
            source.source_timebase,
            source.source_range,
            ClockReference::InternalMonotonic,
        );

        let t0 = Instant::now();
        let first_frame = FrameNumber(150);
        clock.play_from(first_frame, t0 + Duration::from_secs(2));

        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(2)),
            first_frame
        );
    }

    #[test]
    fn preview_command_decodes_one_client_frame() {
        let source = source();
        let cmd = preview_video_command("http://127.0.0.1/stream", &source, FrameNumber(150));
        let args = cmd.args.join(" ");

        assert!(args.contains("-frames:v 1"));
        assert!(args.contains("1.000000"));
        assert!(args.contains("format=rgba"));
    }

    #[test]
    fn video_drain_preserves_early_frames_when_decoder_runs_ahead() {
        let (tx, rx) = mpsc::channel();
        for frame in 0..(VIDEO_QUEUE_CAPACITY + 5) {
            tx.send(DecodedVideoFrame {
                source_frame: FrameNumber(frame as i64),
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            })
            .unwrap();
        }
        let mut queue = VecDeque::new();

        drain_video_decoder_queue(&rx, &mut queue);

        assert_eq!(queue.len(), VIDEO_QUEUE_CAPACITY);
        assert_eq!(queue.front().unwrap().source_frame, FrameNumber(0));
        assert_eq!(
            queue.back().unwrap().source_frame,
            FrameNumber(VIDEO_QUEUE_CAPACITY as i64 - 1)
        );
    }

    #[test]
    fn rgba_payload_converts_to_egui_image() {
        let frame = DecodedVideoFrame {
            source_frame: FrameNumber(10),
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        };
        let image = rgba_to_color_image(&frame).unwrap();
        assert_eq!(image.size, [2, 1]);
        assert_eq!(image.pixels.len(), 2);
    }
}
