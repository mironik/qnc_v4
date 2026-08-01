//! Neutral remote facade for the app broadcast player.
//!
//! The form-facing API is kept stable for the existing UI, but execution is
//! delegated to the modular v2 player crates: core transport, FFmpeg media
//! adapter, audio output, and monitor bridge. Forms provide intent only; the
//! runtime owns frame position and transport state.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};
use qnc_media_ffmpeg::{
    probe_source_runtime_with_toolchain, FfmpegAudioDecodeOptions, FfmpegAudioOutput,
    FfmpegDecodeOptions, FfmpegHardwareDecode, FfmpegSourceOpen, FfmpegSourceRegistry,
    FfmpegToolchain, FfmpegVideoDecode, FfmpegVideoPayload,
};
use qnc_player_core::{
    map_broadcast_player_protocol_event, AudioFormat, BroadcastEngineError, BroadcastEvent,
    BroadcastPlayerProtocolEvent, ClockTick, ColorSpace, DecodedVideoFrame, FieldMode,
    FrameNumber as CoreFrameNumber, FramePresenter, FrameRange as CoreFrameRange, SourceRuntime,
    Timebase as CoreTimebase, TransportEngine, TransportStatus, VideoFormat,
};
use qnc_player_monitor::MonitorPixelLayout;
use qnc_player_monitor_bridge::{
    MonitorBridgeError, MonitorEventBridge, MonitorFrameMapper, MonitorFramePresenter,
    SharedPlayerMonitor,
};
use qnc_player_output::{
    AudioOutputWithSink, AudioPacketTelemetry, AvSyncAudioPacketSink, AvSyncFramePresenter,
    AvSyncTelemetry, FfmpegAudioSink, FfmpegFramePresenter, OutputFrameTelemetry,
};
use serde_json::Value;

use crate::player_contract::{BroadcastHostSourceRef, BroadcastSourceKind, FrameNumber};

type PlayerAudioOutput = AudioOutputWithSink<
    FfmpegAudioOutput,
    AudioPacketTelemetry<AvSyncAudioPacketSink<FfmpegAudioSink>>,
>;

type PlayerVideoPresenter = AvSyncFramePresenter<OutputFrameTelemetry<FfmpegFramePresenter>>;

type PlayerMonitorPresenter = MonitorFramePresenter<PlayerVideoPresenter, PlayerMonitorFrameMapper>;

type PlayerTransport =
    TransportEngine<FfmpegSourceOpen, FfmpegVideoDecode, PlayerAudioOutput, PlayerFramePresenter>;

/// UI open payload - media identity for the modular player runtime.
#[derive(Debug, Clone)]
pub struct BroadcastPlayerOpenRequest {
    pub source_ref: BroadcastHostSourceRef,
    pub media_input: String,
    pub source_fps: f64,
    pub has_audio: bool,
    pub audio_channels: u8,
    pub start_source_sec: f64,
}

#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Open(BroadcastPlayerOpenRequest),
    Play,
    Pause,
    TogglePlay,
    Seek {
        source_sec: f64,
        still: bool,
        coalesce: bool,
    },
    SeekFrame {
        frame: FrameNumber,
        still: bool,
        coalesce: bool,
    },
    Stop,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
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

#[derive(Debug, Clone)]
pub struct PlayerRemoteState {
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

#[derive(Clone, Debug)]
struct LoadedSourceIdentity {
    project_id: String,
    virtual_shot_id: String,
    clip_id: String,
    source_kind: BroadcastSourceKind,
}

#[derive(Debug)]
enum PlayerFramePresenter {
    Plain(PlayerVideoPresenter),
    Monitor(PlayerMonitorPresenter),
}

impl FramePresenter for PlayerFramePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Plain(presenter) => presenter.present_frame(frame),
            Self::Monitor(presenter) => presenter.present_frame(frame),
        }
    }
}

#[derive(Clone, Debug)]
struct PlayerMonitorFrameMapper {
    fallback_source_id: String,
    fallback_video_format: VideoFormat,
}

impl MonitorFrameMapper<FfmpegVideoPayload> for PlayerMonitorFrameMapper {
    fn map_frame(
        &mut self,
        frame: &DecodedVideoFrame<FfmpegVideoPayload>,
    ) -> Result<qnc_player_monitor_bridge::MonitorFrameBuffer, MonitorBridgeError> {
        let source_id = if frame.source_id.trim().is_empty() {
            self.fallback_source_id.clone()
        } else {
            frame.source_id.clone()
        };
        qnc_player_monitor_bridge::MonitorFrameBuffer::new(
            Some(source_id),
            frame.frame,
            frame
                .video_format
                .clone()
                .unwrap_or_else(|| self.fallback_video_format.clone()),
            MonitorPixelLayout::Rgb24,
            frame.payload.bytes.clone(),
        )
        .map_err(Into::into)
    }
}

struct PlayerRuntimeSession {
    transport: PlayerTransport,
    monitor: SharedPlayerMonitor,
    event_bridge: MonitorEventBridge,
    source: SourceRuntime,
    range: CoreFrameRange,
    startup_warnings: Vec<String>,
    last_frame_revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PlayerDecodePolicy {
    recommended_backend: Option<String>,
}

impl PlayerDecodePolicy {
    fn from_runtime(runtime: &Value) -> Self {
        runtime
            .get("hardware_profile")
            .map(Self::from_hardware_profile)
            .unwrap_or_default()
    }

    fn from_hardware_profile(profile: &Value) -> Self {
        let recommended_backend = profile
            .get("media_decode")
            .and_then(|media| media.get("recommended_backend"))
            .and_then(Value::as_str)
            .and_then(normalize_decode_backend);
        Self {
            recommended_backend,
        }
    }
}

pub struct PlayerRemote {
    runtime: Option<PlayerRuntimeSession>,
    last_request: Option<BroadcastPlayerOpenRequest>,
    identity: Option<LoadedSourceIdentity>,
    decode_policy: PlayerDecodePolicy,
    source_frame: FrameNumber,
    source_sec: f64,
    playing: bool,
    active: bool,
    status: String,
    pending_error: Option<String>,
    pending_events: Vec<PlayerEvent>,
    pending_still: Option<(CoreFrameNumber, Instant)>,
    last_still_frame: Option<CoreFrameNumber>,
    started_at: Instant,
}

const STILL_SEEK_DEBOUNCE: Duration = Duration::from_millis(80);
const MAX_CATCHUP_FRAMES: usize = 4;
const VIDEO_PREFETCH_FRAMES: u16 = 8;
const VIDEO_CACHE_FRAMES: usize = 32;
const AUDIO_PREFETCH_FRAMES: u16 = 8;
const AUDIO_CACHE_FRAMES: usize = 96;

impl Default for PlayerRemote {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerRemote {
    pub fn new() -> Self {
        Self {
            runtime: None,
            last_request: None,
            identity: None,
            decode_policy: PlayerDecodePolicy::default(),
            source_frame: FrameNumber(0),
            source_sec: 0.0,
            playing: false,
            active: false,
            status: String::new(),
            pending_error: None,
            pending_events: Vec::new(),
            pending_still: None,
            last_still_frame: None,
            started_at: Instant::now(),
        }
    }

    pub fn state(&self) -> PlayerRemoteState {
        PlayerRemoteState {
            source_frame: self.source_frame,
            source_sec: self.source_sec,
            playing: self.playing,
            active: self.active,
            has_source: self.runtime.is_some(),
            source_kind: self.identity.as_ref().map(|identity| identity.source_kind),
            project_id: self
                .identity
                .as_ref()
                .map(|identity| identity.project_id.clone()),
            clip_id: self
                .identity
                .as_ref()
                .map(|identity| identity.clip_id.clone()),
            virtual_shot_id: self
                .identity
                .as_ref()
                .map(|identity| identity.virtual_shot_id.clone()),
            status: self.status.clone(),
        }
    }

    pub fn source_kind(&self) -> Option<BroadcastSourceKind> {
        self.identity.as_ref().map(|identity| identity.source_kind)
    }

    pub fn source_sec(&self) -> f64 {
        self.source_sec
    }

    pub fn source_frame(&self) -> FrameNumber {
        self.source_frame
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn has_source(&self) -> bool {
        self.runtime.is_some()
    }

    pub fn matches_source(&self, request: &BroadcastPlayerOpenRequest) -> bool {
        self.last_request.as_ref().is_some_and(|prev| {
            prev.source_ref.project_id == request.source_ref.project_id
                && prev.source_ref.clip_id == request.source_ref.clip_id
                && prev.source_ref.virtual_shot_id == request.source_ref.virtual_shot_id
                && prev.media_input == request.media_input
        })
    }

    pub fn configure_runtime_profile(&mut self, runtime: &Value) {
        self.decode_policy = PlayerDecodePolicy::from_runtime(runtime);
    }

    pub fn set_display_sec(&mut self, source_sec: f64) {
        let frame = self.frame_at_seconds(source_sec.max(0.0));
        self.set_display_core_frame(frame);
    }

    pub fn set_display_frame(&mut self, frame: FrameNumber) {
        self.set_display_core_frame(old_to_core_frame(frame));
    }

    pub fn dispatch(&mut self, command: PlayerCommand, ctx: &egui::Context) {
        match command {
            PlayerCommand::Open(request) => self.open(request, ctx),
            PlayerCommand::Play => self.play(ctx),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlay => {
                if self.playing {
                    self.pause();
                } else {
                    self.play(ctx);
                }
            }
            PlayerCommand::Seek {
                source_sec,
                coalesce,
                ..
            } => self.seek_core_frame(self.frame_at_seconds(source_sec), coalesce, ctx),
            PlayerCommand::SeekFrame {
                frame, coalesce, ..
            } => self.seek_core_frame(old_to_core_frame(frame), coalesce, ctx),
            PlayerCommand::Stop => self.stop(),
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context) -> Vec<PlayerEvent> {
        self.flush_still(ctx);
        let mut out = self.pending_events.drain(..).collect::<Vec<_>>();
        if let Some(err) = self.pending_error.take() {
            out.push(PlayerEvent::Error(err));
        }

        let now = self.now_tick();
        let events = match self.runtime.as_mut() {
            Some(runtime) if self.playing => runtime.transport.tick(now),
            _ => Ok(Vec::new()),
        };
        match events {
            Ok(events) => out.extend(self.apply_core_events(events)),
            Err(error) => out.extend(self.apply_core_error(error)),
        }

        if self.playing {
            ctx.request_repaint();
        }
        out
    }

    pub fn stop(&mut self) {
        self.pending_still = None;
        if let Some(runtime) = self.runtime.as_mut() {
            let events = runtime.transport.stop();
            match events {
                Ok(events) => {
                    let applied = self.apply_core_events(events);
                    self.pending_events.extend(applied);
                }
                Err(error) => {
                    let applied = self.apply_core_error(error);
                    self.pending_events.extend(applied);
                }
            }
        }
        self.runtime = None;
        self.identity = None;
        self.last_request = None;
        self.playing = false;
        self.active = false;
        self.status = "Stopped".into();
    }

    fn open(&mut self, request: BroadcastPlayerOpenRequest, ctx: &egui::Context) {
        if self.matches_source(&request) && self.runtime.is_some() && self.pending_error.is_none() {
            self.last_request = Some(request);
            self.active = true;
            if self.status.is_empty() || self.status == "Stopped" {
                self.status = "Ready".into();
            }
            return;
        }

        self.pending_still = None;
        self.last_still_frame = None;
        self.playing = false;
        self.active = false;
        self.status = "Open".into();

        match build_runtime_session(&request, &self.decode_policy) {
            Ok(mut session) => {
                let start_frame =
                    frame_at_seconds(request.start_source_sec, session.source.timebase);
                let start_frame = clamp_core_frame(start_frame, session.range);
                match session
                    .transport
                    .sync_range_runtime(Some(session.range), start_frame, true)
                {
                    Ok(events) => {
                        self.runtime = Some(session);
                        self.identity = Some(identity_from_request(&request));
                        self.last_request = Some(request);
                        self.active = true;
                        self.set_display_core_frame(start_frame);
                        self.queue_core_events(events);
                    }
                    Err(error) => {
                        self.runtime = Some(session);
                        self.last_request = Some(request);
                        self.queue_core_error(error);
                    }
                }
            }
            Err(err) => {
                self.runtime = None;
                self.identity = None;
                self.last_request = None;
                self.playing = false;
                self.active = false;
                self.status = err.clone();
                self.pending_error = Some(err);
            }
        }
        ctx.request_repaint();
    }

    fn play(&mut self, ctx: &egui::Context) {
        self.pending_still = None;
        if self.runtime.is_none() {
            let Some(request) = self.last_request.clone() else {
                self.status = "Odaberi source kadar prije play".into();
                self.pending_error = Some(self.status.clone());
                return;
            };
            self.open(request, ctx);
        }
        let now = self.now_tick();
        match self.runtime.as_mut() {
            Some(runtime) => match runtime.transport.play(now) {
                Ok(events) => {
                    self.playing = true;
                    self.active = true;
                    self.status = "Playing".into();
                    self.queue_core_events(events);
                    ctx.request_repaint();
                }
                Err(error) => self.queue_core_error(error),
            },
            None => {
                self.status = "Source nije otvoren".into();
                self.pending_error = Some(self.status.clone());
            }
        }
    }

    fn pause(&mut self) {
        self.pending_still = None;
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.transport.pause() {
                Ok(events) => self.queue_core_events(events),
                Err(error) => self.queue_core_error(error),
            }
        }
        self.playing = false;
        self.status = "Paused".into();
    }

    fn seek_core_frame(&mut self, frame: CoreFrameNumber, coalesce: bool, ctx: &egui::Context) {
        if self.playing {
            self.pause();
        }
        let frame = self.clamp_to_runtime_range(frame);
        self.set_display_core_frame(frame);
        if coalesce {
            self.pending_still = Some((frame, Instant::now() + STILL_SEEK_DEBOUNCE));
            ctx.request_repaint_after(STILL_SEEK_DEBOUNCE + Duration::from_millis(10));
        } else {
            self.pending_still = None;
            self.cue_still_frame(frame, ctx);
        }
    }

    fn flush_still(&mut self, ctx: &egui::Context) {
        let Some((frame, at)) = self.pending_still else {
            return;
        };
        if self.playing {
            self.pending_still = None;
            return;
        }
        let now = Instant::now();
        if now < at {
            ctx.request_repaint_after(at - now);
            return;
        }
        self.pending_still = None;
        self.cue_still_frame(frame, ctx);
    }

    fn cue_still_frame(&mut self, frame: CoreFrameNumber, ctx: &egui::Context) {
        let frame = self.clamp_to_runtime_range(frame);
        if self.last_still_frame == Some(frame) && !self.playing {
            return;
        }
        self.last_still_frame = Some(frame);
        let Some(runtime) = self.runtime.as_mut() else {
            self.status = "Nema source za seek".into();
            self.pending_error = Some(self.status.clone());
            return;
        };
        match runtime
            .transport
            .sync_range_runtime(Some(runtime.range), frame, true)
        {
            Ok(events) => {
                self.playing = false;
                self.active = true;
                self.status = "Ready".into();
                self.set_display_core_frame(frame);
                self.queue_core_events(events);
                ctx.request_repaint();
            }
            Err(error) => self.queue_core_error(error),
        }
    }

    fn queue_core_events(&mut self, events: Vec<BroadcastEvent>) {
        let applied = self.apply_core_events(events);
        self.pending_events.extend(applied);
    }

    fn queue_core_error(&mut self, error: BroadcastEngineError) {
        let applied = self.apply_core_error(error);
        self.pending_events.extend(applied);
    }

    fn apply_core_events(&mut self, events: Vec<BroadcastEvent>) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        let protocol_events = events
            .iter()
            .filter_map(|event| map_broadcast_player_protocol_event(event).ok())
            .collect::<Vec<_>>();

        if let Some(runtime) = self.runtime.as_mut() {
            let _ = runtime.event_bridge.apply_events(&protocol_events);
        }

        for event in protocol_events {
            match event {
                BroadcastPlayerProtocolEvent::CarrierPositionChanged { frame, status, .. } => {
                    self.set_display_core_frame(frame);
                    self.playing = status == TransportStatus::Playing;
                    self.active = true;
                    self.status = status_label(status).to_string();
                    out.push(PlayerEvent::State {
                        source_frame: self.source_frame,
                        source_sec: self.source_sec,
                        playing: self.playing,
                        status: self.status.clone(),
                    });
                }
                BroadcastPlayerProtocolEvent::TransportStatusChanged { status } => {
                    self.playing = status == TransportStatus::Playing;
                    self.active = status != TransportStatus::Empty;
                    self.status = status_label(status).to_string();
                    out.push(PlayerEvent::State {
                        source_frame: self.source_frame,
                        source_sec: self.source_sec,
                        playing: self.playing,
                        status: self.status.clone(),
                    });
                }
                BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame } => {
                    self.set_display_core_frame(frame);
                    self.playing = false;
                    self.status = "Paused".into();
                }
                BroadcastPlayerProtocolEvent::SourceFailed { reason, .. }
                | BroadcastPlayerProtocolEvent::PlaybackError { message: reason }
                | BroadcastPlayerProtocolEvent::DecodeWarning { message: reason } => {
                    self.status = reason.clone();
                    self.playing = false;
                    out.push(PlayerEvent::Error(reason));
                }
                _ => {}
            }
        }

        out.extend(self.take_monitor_frame_event());
        out
    }

    fn apply_core_error(&mut self, error: BroadcastEngineError) -> Vec<PlayerEvent> {
        self.playing = false;
        self.status = error.to_string();
        vec![PlayerEvent::Error(self.status.clone())]
    }

    fn take_monitor_frame_event(&mut self) -> Vec<PlayerEvent> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Vec::new();
        };
        let Ok(snapshot) = runtime.monitor.snapshot() else {
            return Vec::new();
        };
        if snapshot.frame_revision == runtime.last_frame_revision {
            return Vec::new();
        }
        runtime.last_frame_revision = snapshot.frame_revision;
        let Some(frame_buffer) = snapshot.last_frame_buffer else {
            return Vec::new();
        };
        let image = ColorImage::from_rgb(
            [
                frame_buffer.video_format.width as usize,
                frame_buffer.video_format.height as usize,
            ],
            &frame_buffer.bytes,
        );
        self.set_display_core_frame(frame_buffer.frame);
        vec![PlayerEvent::Frame {
            image,
            source_frame: self.source_frame,
            source_sec: self.source_sec,
            playing: self.playing,
        }]
    }

    fn frame_at_seconds(&self, seconds: f64) -> CoreFrameNumber {
        let timebase = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.source.timebase)
            .or_else(|| {
                self.last_request
                    .as_ref()
                    .and_then(|request| core_timebase_from_fps(request.source_fps).ok())
            })
            .unwrap_or(CoreTimebase {
                frame_rate_num: 25,
                frame_rate_den: 1,
            });
        frame_at_seconds(seconds, timebase)
    }

    fn set_display_core_frame(&mut self, frame: CoreFrameNumber) {
        self.source_frame = core_to_old_frame(frame);
        if let Some(runtime) = self.runtime.as_ref() {
            self.source_sec = seconds_at_frame(frame, runtime.source.timebase);
        }
    }

    fn clamp_to_runtime_range(&self, frame: CoreFrameNumber) -> CoreFrameNumber {
        self.runtime
            .as_ref()
            .map(|runtime| clamp_core_frame(frame, runtime.range))
            .unwrap_or(frame)
    }

    fn now_tick(&self) -> ClockTick {
        self.started_at.elapsed().as_nanos()
    }
}

fn build_runtime_session(
    request: &BroadcastPlayerOpenRequest,
    decode_policy: &PlayerDecodePolicy,
) -> Result<PlayerRuntimeSession, String> {
    let media_path = PathBuf::from(request.media_input.trim());
    if media_path.as_os_str().is_empty() {
        return Err("media path is empty".into());
    }

    let source_id = source_id_from_request(request);
    let timebase_hint = core_timebase_from_fps(request.source_fps).ok();
    let toolchain = FfmpegToolchain::default();
    let mut source = probe_source_runtime_with_toolchain(
        &media_path,
        source_id.clone(),
        timebase_hint,
        &toolchain,
    )
    .or_else(|_| fallback_source_runtime(request, source_id.clone()))?
    .source;

    if !request.has_audio {
        source.audio_format = None;
    } else if source.audio_format.is_none() {
        source.audio_format = Some(AudioFormat::new(
            48_000,
            request.audio_channels.max(1) as u16,
        )?);
    }

    let range = range_from_request(request, &source)?;
    let registry = FfmpegSourceRegistry::new(BTreeMap::from([(source_id, media_path)]));
    let (hardware_decode, hardware_warning) = hardware_decode_from_policy(decode_policy);
    let video_decode = FfmpegVideoDecode::with_options(
        registry.clone(),
        FfmpegDecodeOptions::software()
            .with_toolchain(toolchain.clone())
            .with_hardware_decode(hardware_decode)
            .with_video_prefetch_frames(VIDEO_PREFETCH_FRAMES)
            .with_video_cache_frames(VIDEO_CACHE_FRAMES),
    );
    let audio_output = FfmpegAudioOutput::with_options(
        registry.clone(),
        FfmpegAudioDecodeOptions::default()
            .with_toolchain(toolchain)
            .with_audio_prefetch_frames(AUDIO_PREFETCH_FRAMES)
            .with_audio_cache_frames(AUDIO_CACHE_FRAMES),
    );
    let av_sync = AvSyncTelemetry::default();
    let (audio_sink, audio_device_warning) = build_audio_sink();
    let startup_warnings = [hardware_warning, audio_device_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let audio = AudioOutputWithSink::new(
        audio_output,
        AudioPacketTelemetry::new(AvSyncAudioPacketSink::new(audio_sink, av_sync.clone())),
    );
    let monitor = SharedPlayerMonitor::default();
    let event_bridge = MonitorEventBridge::new(monitor.clone());
    let presenter = build_frame_presenter(&source, monitor.clone(), av_sync);
    let transport = TransportEngine::new(
        FfmpegSourceOpen::new(registry),
        video_decode,
        audio,
        presenter,
    )
    .with_max_catchup_frames(MAX_CATCHUP_FRAMES);

    let mut session = PlayerRuntimeSession {
        transport,
        monitor,
        event_bridge,
        source: source.clone(),
        range,
        startup_warnings,
        last_frame_revision: 0,
    };

    let mut events = session
        .transport
        .set_active_source(&source, None)
        .map_err(|error| error.to_string())?;
    for warning in session.startup_warnings.clone() {
        events.push(BroadcastEvent::DecodeWarning { message: warning });
    }
    let protocol_events = events
        .iter()
        .filter_map(|event| map_broadcast_player_protocol_event(event).ok())
        .collect::<Vec<_>>();
    session
        .event_bridge
        .apply_events(&protocol_events)
        .map_err(|error| error.to_string())?;
    Ok(session)
}

fn build_frame_presenter(
    source: &SourceRuntime,
    monitor: SharedPlayerMonitor,
    av_sync: AvSyncTelemetry,
) -> PlayerFramePresenter {
    let presenter = AvSyncFramePresenter::new(
        OutputFrameTelemetry::new(FfmpegFramePresenter::event_only()),
        av_sync,
    );
    if let Some(video_format) = source.video_format.clone() {
        return PlayerFramePresenter::Monitor(MonitorFramePresenter::new(
            presenter,
            monitor,
            PlayerMonitorFrameMapper {
                fallback_source_id: source.source_id.clone(),
                fallback_video_format: video_format,
            },
        ));
    }
    PlayerFramePresenter::Plain(presenter)
}

fn build_audio_sink() -> (FfmpegAudioSink, Option<String>) {
    match FfmpegAudioSink::audio_device() {
        Ok(sink) => (sink, None),
        Err(error) => (
            FfmpegAudioSink::none(),
            Some(format!("Audio device unavailable: {error}")),
        ),
    }
}

fn hardware_decode_from_policy(
    policy: &PlayerDecodePolicy,
) -> (FfmpegHardwareDecode, Option<String>) {
    if let Some(value) = env::var("QNC_PLAYER_HWACCEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return hardware_decode_from_backend_label(&value, "QNC_PLAYER_HWACCEL");
    }

    let Some(value) = policy.recommended_backend.as_deref() else {
        return (FfmpegHardwareDecode::Software, None);
    };
    hardware_decode_from_backend_label(value, "hardware_profile.media_decode")
}

fn hardware_decode_from_backend_label(
    value: &str,
    source: &str,
) -> (FfmpegHardwareDecode, Option<String>) {
    if normalize_decode_backend(value).is_none() {
        return (FfmpegHardwareDecode::Software, None);
    }
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        return (FfmpegHardwareDecode::Auto, None);
    }
    match FfmpegHardwareDecode::backend(value) {
        Ok(backend) => (backend, None),
        Err(error) => (
            FfmpegHardwareDecode::Software,
            Some(format!("Hardware decode disabled from {source}: {error}")),
        ),
    }
}

fn normalize_decode_backend(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() || matches!(value.as_str(), "none" | "software" | "off" | "sw") {
        return None;
    }
    Some(value)
}

fn fallback_source_runtime(
    request: &BroadcastPlayerOpenRequest,
    source_id: String,
) -> Result<qnc_media_ffmpeg::FfmpegProbeReport, String> {
    let timebase = core_timebase_from_fps(request.source_fps)?;
    let duration_frames =
        frame_at_seconds(request.source_ref.duration_sec.max(1.0), timebase).max(1);
    let mut source = SourceRuntime::new(source_id, duration_frames, timebase)?;
    source = source.with_video_format(
        VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709)
            .map_err(|error| error.to_string())?,
    );
    if request.has_audio {
        source = source.with_audio_format(AudioFormat::new(
            48_000,
            request.audio_channels.max(1) as u16,
        )?);
    }
    Ok(qnc_media_ffmpeg::FfmpegProbeReport {
        source,
        has_video: true,
        has_audio: request.has_audio,
    })
}

fn range_from_request(
    request: &BroadcastPlayerOpenRequest,
    source: &SourceRuntime,
) -> Result<CoreFrameRange, String> {
    let in_sec = request.source_ref.in_seconds.unwrap_or(0.0).max(0.0);
    let out_sec = request
        .source_ref
        .out_seconds
        .filter(|value| *value > in_sec)
        .unwrap_or_else(|| request.source_ref.duration_sec.max(in_sec + 0.04));
    let start =
        frame_at_seconds(in_sec, source.timebase).min(source.duration_frames.saturating_sub(1));
    let mut end = frame_at_seconds(out_sec, source.timebase).min(source.duration_frames);
    if end <= start {
        end = start.saturating_add(1).min(source.duration_frames);
    }
    CoreFrameRange::new(start, end)
}

fn identity_from_request(request: &BroadcastPlayerOpenRequest) -> LoadedSourceIdentity {
    LoadedSourceIdentity {
        project_id: request.source_ref.project_id.clone(),
        virtual_shot_id: request.source_ref.virtual_shot_id.clone(),
        clip_id: request.source_ref.clip_id.clone(),
        source_kind: source_kind(request.has_audio),
    }
}

fn source_kind(has_audio: bool) -> BroadcastSourceKind {
    if has_audio {
        BroadcastSourceKind::VideoAndAudio
    } else {
        BroadcastSourceKind::VideoOnly
    }
}

fn source_id_from_request(request: &BroadcastPlayerOpenRequest) -> String {
    let id = [
        request.source_ref.project_id.as_str(),
        request.source_ref.virtual_shot_id.as_str(),
        request.source_ref.clip_id.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join("::");
    if id.is_empty() {
        "qnc-source".into()
    } else {
        id
    }
}

fn core_timebase_from_fps(fps: f64) -> Result<CoreTimebase, String> {
    if approx(fps, 23.976) {
        CoreTimebase::new(24_000, 1_001)
    } else if approx(fps, 29.97) {
        CoreTimebase::new(30_000, 1_001)
    } else if approx(fps, 59.94) {
        CoreTimebase::new(60_000, 1_001)
    } else if fps.is_finite() && fps > 0.0 {
        CoreTimebase::new(fps.round().max(1.0) as u32, 1)
    } else {
        CoreTimebase::new(25, 1)
    }
}

fn frame_at_seconds(seconds: f64, timebase: CoreTimebase) -> CoreFrameNumber {
    let frames =
        seconds.max(0.0) * f64::from(timebase.frame_rate_num) / f64::from(timebase.frame_rate_den);
    frames.round().max(0.0) as CoreFrameNumber
}

fn seconds_at_frame(frame: CoreFrameNumber, timebase: CoreTimebase) -> f64 {
    frame as f64 * f64::from(timebase.frame_rate_den) / f64::from(timebase.frame_rate_num)
}

fn clamp_core_frame(frame: CoreFrameNumber, range: CoreFrameRange) -> CoreFrameNumber {
    frame.clamp(range.start_frame, range.end_frame)
}

fn old_to_core_frame(frame: FrameNumber) -> CoreFrameNumber {
    frame.0.max(0) as CoreFrameNumber
}

fn core_to_old_frame(frame: CoreFrameNumber) -> FrameNumber {
    FrameNumber(i64::try_from(frame).unwrap_or(i64::MAX))
}

fn status_label(status: TransportStatus) -> &'static str {
    match status {
        TransportStatus::Empty => "Empty",
        TransportStatus::Ready => "Ready",
        TransportStatus::Playing => "Playing",
        TransportStatus::Paused => "Paused",
        TransportStatus::Stopped => "Stopped",
    }
}

fn approx(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.01
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_adapter_converts_seconds_to_core_frames() {
        let tb = CoreTimebase::new(25, 1).unwrap();

        assert_eq!(frame_at_seconds(0.04, tb), 1);
        assert_eq!(frame_at_seconds(1.0, tb), 25);
    }

    #[test]
    fn app_adapter_recognizes_fractional_broadcast_rates() {
        assert_eq!(
            core_timebase_from_fps(29.97).unwrap(),
            CoreTimebase::new(30_000, 1_001).unwrap()
        );
        assert_eq!(
            core_timebase_from_fps(59.94).unwrap(),
            CoreTimebase::new(60_000, 1_001).unwrap()
        );
    }

    #[test]
    fn decode_policy_reads_runtime_hardware_profile_backend() {
        let runtime = serde_json::json!({
            "hardware_profile": {
                "media_decode": {
                    "recommended_backend": "d3d11va"
                }
            }
        });

        let policy = PlayerDecodePolicy::from_runtime(&runtime);

        assert_eq!(policy.recommended_backend.as_deref(), Some("d3d11va"));
    }

    #[test]
    fn decode_policy_treats_software_profile_as_no_hardware_backend() {
        let runtime = serde_json::json!({
            "hardware_profile": {
                "media_decode": {
                    "recommended_backend": "software"
                }
            }
        });

        let policy = PlayerDecodePolicy::from_runtime(&runtime);

        assert_eq!(policy.recommended_backend, None);
    }
}
