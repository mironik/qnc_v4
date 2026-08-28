//! Neutral remote facade for the app broadcast player.
//!
//! The form-facing API is kept stable for the existing UI, but execution is
//! delegated to the modular v2 player crates: core transport, FFmpeg media
//! adapter, audio output, and monitor bridge. Forms provide intent only; the
//! runtime owns frame position and transport state.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};
use qnc_media_ffmpeg::{
    probe_media_input_runtime_with_toolchain, FfmpegAudioDecodeOptions, FfmpegAudioOutput,
    FfmpegDecodeOptions, FfmpegDecodePolicy as AdapterDecodePolicy, FfmpegHardwareDecode,
    FfmpegMediaInput, FfmpegSourceOpen, FfmpegSourceRegistry, FfmpegToolchain, FfmpegVideoDecode,
    FfmpegVideoPayload, FfmpegVideoPrefetchRule,
};
use qnc_player_core::{
    AudioFormat, AudioFramePacket, AudioOutputAdapter, BroadcastEngineError,
    BroadcastEngineErrorKind, BroadcastEvent, BroadcastPlaybackRequest,
    BroadcastPlayerProtocolCommand, BroadcastPlayerProtocolEvent, ClockTick, ColorSpace,
    DecodedVideoFrame, EngineFrameRequest, EngineSourceHandle, FieldMode,
    FrameNumber as CoreFrameNumber, FramePresenter, FrameRange as CoreFrameRange,
    SourceOpenAdapter, SourceRuntime, Timebase as CoreTimebase, TransportEngine, TransportStatus,
    VideoDecodeAdapter, VideoFormat,
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
use qnc_player_runtime::{BroadcastPlayerRuntime, PlayerRuntimeCommand};
use serde_json::Value;

use crate::player_contract::{
    BroadcastHostSourceRef, BroadcastSourceKind, BroadcastSourceTimebase, FrameNumber,
};

type FfmpegPlayerAudioOutput = AudioOutputWithSink<
    FfmpegAudioOutput,
    AudioPacketTelemetry<AvSyncAudioPacketSink<FfmpegAudioSink>>,
>;

type PlayerVideoPresenter = AvSyncFramePresenter<OutputFrameTelemetry<FfmpegFramePresenter>>;

type PlayerMonitorPresenter = MonitorFramePresenter<PlayerVideoPresenter, PlayerMonitorFrameMapper>;

type PlayerTransport = TransportEngine<
    PlayerInputOpen,
    PlayerInputVideoDecode,
    PlayerInputAudioOutput,
    PlayerFramePresenter,
>;

type PlayerRuntime = BroadcastPlayerRuntime<PlayerTransport>;

const SOURCE_VIDEO_PREFETCH_FRAMES: u16 = 24;
const SOURCE_VIDEO_CACHE_FRAMES: usize = 96;
const SOURCE_AUDIO_PREFETCH_FRAMES: u16 = 32;
const SOURCE_AUDIO_CACHE_FRAMES: usize = 192;
const SOURCE_DECODE_BURST_FRAMES: usize = 6;
const PLAYLIST_VIDEO_PREFETCH_FRAMES: u16 = 16;
const PLAYLIST_VIDEO_CACHE_FRAMES: usize = 48;
const PLAYLIST_AUDIO_PREFETCH_FRAMES: u16 = 16;
const PLAYLIST_AUDIO_CACHE_FRAMES: usize = 96;
const PLAYLIST_DECODE_BURST_FRAMES: usize = 4;
const PLAYLIST_SOURCE_WARMUP_LOOKAHEAD_FRAMES: CoreFrameNumber = 120;
const PLAYLIST_SOURCE_WARMUP_MAX_SOURCES: usize = 2;
const PLAYLIST_PREVIEW_WIDTH: u32 = 640;
const PLAYLIST_PREVIEW_HEIGHT: u32 = 360;
const QNC_PLAYER_HWACCEL_ENV: &str = "QNC_PLAYER_HWACCEL";

/// UI open payload - media identity for the modular player runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastPlayerOpenRequest {
    pub source_ref: BroadcastHostSourceRef,
    pub media_input: String,
    pub source_fps: f64,
    pub source_timebase: BroadcastSourceTimebase,
    pub has_audio: bool,
    pub audio_channels: u8,
    pub start_source_frame: FrameNumber,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastProgramOpenRequest {
    pub program_id: String,
    pub project_id: String,
    pub timeline_fps: f64,
    pub duration_frames: i64,
    pub start_program_frame: FrameNumber,
    pub preview_video_resolution: BroadcastProgramPreviewVideoResolution,
    /// Streaming playlist input. This is one playable program input; do not
    /// rebuild this as a top-level list of independently played takes.
    pub items: Vec<BroadcastProgramItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastProgramPreviewVideoResolution {
    FastPreview,
    SourceRaster,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastProgramItem {
    pub item_id: String,
    pub record_in_frame: FrameNumber,
    pub record_out_frame: FrameNumber,
    pub sources: Vec<BroadcastProgramSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastProgramSource {
    pub source_ref: BroadcastHostSourceRef,
    pub media_input: String,
    pub source_fps: f64,
    pub source_timebase: BroadcastSourceTimebase,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u8,
    pub audio_output_channel: Option<u8>,
}

pub const PROGRAM_AUDIO_OUTPUT_CH1: u8 = 0;
pub const PROGRAM_AUDIO_OUTPUT_CH2: u8 = 1;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum PlayerCommand {
    Open(BroadcastPlayerOpenRequest),
    PrepareProgram(BroadcastProgramOpenRequest),
    OpenProgram(BroadcastProgramOpenRequest),
    PlayProgram(BroadcastProgramOpenRequest),
    Play,
    Pause,
    TogglePlay,
    SeekFrame {
        frame: FrameNumber,
        still: bool,
        coalesce: bool,
    },
    Stop,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum PlayerEvent {
    /// Program loaded — timeline paint bounds (once per open).
    SourceReady {
        fps: f64,
        duration_frames: i64,
        in_frame: i64,
        out_frame: i64,
        /// Probed from file (`progressive` / upper-first / lower-first).
        field_mode: FieldMode,
    },
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
    BoundaryReached {
        source_frame: FrameNumber,
    },
    ProgramPrepared {
        request: BroadcastProgramOpenRequest,
    },
    ProgramPrepareFailed {
        request: BroadcastProgramOpenRequest,
        error: String,
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

enum PlayerInputOpen {
    Ffmpeg(FfmpegSourceOpen),
    Playlist(PlaylistInputOpen),
}

impl SourceOpenAdapter for PlayerInputOpen {
    fn open_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<EngineSourceHandle, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.open_source(source, source_revision),
            Self::Playlist(inner) => inner.open_source(source, source_revision),
        }
    }

    fn close_source(&mut self, source_id: &str) -> Result<(), BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.close_source(source_id),
            Self::Playlist(inner) => inner.close_source(source_id),
        }
    }
}

enum PlayerInputVideoDecode {
    Ffmpeg(FfmpegVideoDecode),
    Playlist(PlaylistInputVideoDecode),
}

impl VideoDecodeAdapter for PlayerInputVideoDecode {
    type VideoFrame = FfmpegVideoPayload;

    fn prepare_video(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.prepare_video(source),
            Self::Playlist(inner) => inner.prepare_video(source),
        }
    }

    fn decode_video_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.decode_video_frame(request),
            Self::Playlist(inner) => inner.decode_video_frame(request),
        }
    }
}

enum PlayerInputAudioOutput {
    Ffmpeg(FfmpegPlayerAudioOutput),
    Playlist(PlaylistInputAudioOutput),
}

impl AudioOutputAdapter for PlayerInputAudioOutput {
    type AudioPacket = Vec<u8>;

    fn prepare_audio(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.prepare_audio(source),
            Self::Playlist(inner) => inner.prepare_audio(source),
        }
    }

    fn render_audio_for_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.render_audio_for_frame(request),
            Self::Playlist(inner) => inner.render_audio_for_frame(request),
        }
    }

    fn submit_audio_packet(
        &mut self,
        packet: AudioFramePacket<Self::AudioPacket>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.submit_audio_packet(packet),
            Self::Playlist(inner) => inner.submit_audio_packet(packet),
        }
    }

    fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Ffmpeg(inner) => inner.stop_audio(),
            Self::Playlist(inner) => inner.stop_audio(),
        }
    }
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
    video_formats: BTreeMap<String, VideoFormat>,
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
        let video_format = frame
            .video_format
            .clone()
            .or_else(|| self.video_formats.get(&source_id).cloned())
            .unwrap_or_else(|| self.fallback_video_format.clone());
        qnc_player_monitor_bridge::MonitorFrameBuffer::new(
            Some(source_id),
            frame.frame,
            video_format,
            MonitorPixelLayout::Rgb24,
            frame.payload.bytes.clone(),
        )
        .map_err(Into::into)
    }
}

struct PlayerRuntimeSession {
    runtime: PlayerRuntime,
    monitor: SharedPlayerMonitor,
    event_bridge: MonitorEventBridge,
    source: SourceRuntime,
    range: CoreFrameRange,
    startup_warnings: Vec<String>,
    last_frame_revision: u64,
}

#[derive(Clone, Debug)]
struct PlayerProgramState {
    timebase: CoreTimebase,
    duration_frames: CoreFrameNumber,
    items: Vec<PlayerProgramItem>,
}

#[derive(Clone, Debug)]
struct PlayerProgramItem {
    spec: BroadcastProgramItem,
    sources: Vec<PlayerProgramSource>,
}

#[derive(Clone, Debug)]
struct PlayerProgramSource {
    spec: BroadcastProgramSource,
    record_in_frame: CoreFrameNumber,
    record_out_frame: CoreFrameNumber,
    source: SourceRuntime,
    range: CoreFrameRange,
}

#[derive(Clone, Debug)]
struct ProgramFrameResult {
    video: Option<PlayerProgramSource>,
    audio_buses: Vec<ProgramAudioBus>,
}

#[derive(Clone, Debug)]
struct ProgramAudioBus {
    output_channel: u8,
    source: PlayerProgramSource,
}

impl ProgramFrameResult {
    fn audio_bus(&self, output_channel: u8) -> Option<&PlayerProgramSource> {
        self.audio_buses
            .iter()
            .find(|bus| bus.output_channel == output_channel)
            .map(|bus| &bus.source)
    }
}

struct PlaylistInputOpen {
    source: SourceRuntime,
}

impl SourceOpenAdapter for PlaylistInputOpen {
    fn open_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<EngineSourceHandle, BroadcastEngineError> {
        if source.source_id != self.source.source_id {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                format!("playlist source not registered: {}", source.source_id),
            )
            .with_source_id(source.source_id.clone()));
        }
        Ok(EngineSourceHandle::from_source_runtime(
            &self.source,
            source_revision,
        ))
    }

    fn close_source(&mut self, _source_id: &str) -> Result<(), BroadcastEngineError> {
        Ok(())
    }
}

struct PlaylistInputVideoDecode {
    source_id: String,
    video_format: VideoFormat,
    program: PlayerProgramState,
    prepared_sources: BTreeSet<String>,
    inner: FfmpegVideoDecode,
}

impl VideoDecodeAdapter for PlaylistInputVideoDecode {
    type VideoFrame = FfmpegVideoPayload;

    fn prepare_video(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.require_playlist_source(source.source_id.as_str())?;
        let program_sources = self
            .program
            .media_sources_matching(PlayerProgramSource::has_video)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        for program_source in program_sources {
            let _ = self.ensure_video_source_prepared(&program_source)?;
        }
        Ok(Vec::new())
    }

    fn decode_video_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError> {
        self.require_playlist_source(request.source_id.as_str())?;
        let result = self.program.frame_result(request.frame);
        let Some(program_source) = result.video.as_ref() else {
            return self.black_frame(request.frame);
        };
        self.request_upcoming_video_warmups(request.frame, &result)?;

        let _ = self.ensure_video_source_prepared(program_source)?;
        let source_frame = program_source.source_frame_for_program_frame(request.frame);
        let source_request = EngineFrameRequest::new(&source_handle(program_source), source_frame)?;
        let mut frame = self.inner.decode_video_frame(source_request)?;
        frame.source_id = self.source_id.clone();
        frame.frame = request.frame;
        frame.video_format = Some(self.video_format.clone());
        frame.payload.frame = request.frame;
        Ok(frame)
    }
}

impl PlaylistInputVideoDecode {
    fn ensure_video_source_prepared(
        &mut self,
        source: &PlayerProgramSource,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if self.prepared_sources.contains(&source.source.source_id) {
            return Ok(Vec::new());
        }
        let handle = EngineSourceHandle::from_source_runtime(&source.source, None);
        let events = self.inner.prepare_video(&handle)?;
        self.prepared_sources
            .insert(source.source.source_id.clone());
        Ok(events)
    }

    fn request_upcoming_video_warmups(
        &mut self,
        program_frame: CoreFrameNumber,
        result: &ProgramFrameResult,
    ) -> Result<(), BroadcastEngineError> {
        let active_sources = result
            .video
            .as_ref()
            .map(|source| BTreeSet::from([source.source.source_id.clone()]))
            .unwrap_or_default();
        for point in playlist_upcoming_start_points(
            &self.program,
            program_frame,
            PLAYLIST_SOURCE_WARMUP_LOOKAHEAD_FRAMES,
            PLAYLIST_SOURCE_WARMUP_MAX_SOURCES,
            PlayerProgramSource::has_video,
        ) {
            if active_sources.contains(&point.source.source.source_id) {
                continue;
            }
            let _ = self.ensure_video_source_prepared(&point.source)?;
            let source_frame = point
                .source
                .source_frame_for_program_frame(point.program_frame);
            let request = EngineFrameRequest::new(&source_handle(&point.source), source_frame)?;
            self.inner.request_stream_warmup(request)?;
        }
        Ok(())
    }

    fn require_playlist_source(&self, source_id: &str) -> Result<(), BroadcastEngineError> {
        if source_id == self.source_id {
            return Ok(());
        }
        Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::VideoDecode,
            format!("video request is not for playlist source: {source_id}"),
        )
        .with_source_id(source_id.to_string()))
    }

    fn black_frame(
        &self,
        frame: CoreFrameNumber,
    ) -> Result<DecodedVideoFrame<FfmpegVideoPayload>, BroadcastEngineError> {
        let byte_len = video_frame_byte_len(&self.video_format)?;
        Ok(DecodedVideoFrame {
            source_id: self.source_id.clone(),
            frame,
            video_format: Some(self.video_format.clone()),
            payload: FfmpegVideoPayload {
                frame,
                bytes: vec![0; byte_len],
            },
        })
    }
}

struct PlaylistInputAudioOutput {
    source_id: String,
    timebase: CoreTimebase,
    audio_format: AudioFormat,
    program: PlayerProgramState,
    prepared_sources: BTreeSet<String>,
    inner: FfmpegPlayerAudioOutput,
}

impl AudioOutputAdapter for PlaylistInputAudioOutput {
    type AudioPacket = Vec<u8>;

    fn prepare_audio(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.require_playlist_source(source.source_id.as_str())?;
        let program_sources = self
            .program
            .media_sources_matching(PlayerProgramSource::has_audio)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        for program_source in program_sources {
            let _ = self.ensure_audio_source_prepared(&program_source)?;
        }
        Ok(Vec::new())
    }

    fn render_audio_for_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
        self.require_playlist_source(request.source_id.as_str())?;
        let result = self.program.frame_result(request.frame);
        if result.audio_buses.is_empty() {
            return self.silence_packet(request.frame);
        }
        self.request_upcoming_audio_warmups(request.frame, &result)?;
        let mut packet = self.silence_packet(request.frame)?;
        if let Some(program_source) = result.audio_bus(PROGRAM_AUDIO_OUTPUT_CH1) {
            let _ = self.ensure_audio_source_prepared(program_source)?;
            let source_frame = program_source.source_frame_for_program_frame(request.frame);
            let source_request =
                EngineFrameRequest::new(&source_handle(program_source), source_frame)?;
            let source_packet = self.inner.render_audio_for_frame(source_request)?;
            copy_pcm_s16le_channel(&source_packet, &mut packet, 0)?;
        }
        if let Some(program_source) = result.audio_bus(PROGRAM_AUDIO_OUTPUT_CH2) {
            let _ = self.ensure_audio_source_prepared(program_source)?;
            let source_frame = program_source.source_frame_for_program_frame(request.frame);
            let source_request =
                EngineFrameRequest::new(&source_handle(program_source), source_frame)?;
            let source_packet = self.inner.render_audio_for_frame(source_request)?;
            copy_pcm_s16le_channel(&source_packet, &mut packet, 1)?;
        }
        Ok(packet)
    }

    fn submit_audio_packet(
        &mut self,
        packet: AudioFramePacket<Self::AudioPacket>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.inner.submit_audio_packet(packet)
    }

    fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.inner.stop_audio()
    }
}

impl PlaylistInputAudioOutput {
    fn ensure_audio_source_prepared(
        &mut self,
        source: &PlayerProgramSource,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if self.prepared_sources.contains(&source.source.source_id) {
            return Ok(Vec::new());
        }
        let handle = EngineSourceHandle::from_source_runtime(&source.source, None);
        let events = self.inner.prepare_audio(&handle)?;
        self.prepared_sources
            .insert(source.source.source_id.clone());
        Ok(events)
    }

    fn request_upcoming_audio_warmups(
        &mut self,
        program_frame: CoreFrameNumber,
        result: &ProgramFrameResult,
    ) -> Result<(), BroadcastEngineError> {
        let active_sources = result
            .audio_buses
            .iter()
            .map(|bus| bus.source.source.source_id.clone())
            .collect::<BTreeSet<_>>();
        for point in playlist_upcoming_start_points(
            &self.program,
            program_frame,
            PLAYLIST_SOURCE_WARMUP_LOOKAHEAD_FRAMES,
            PLAYLIST_SOURCE_WARMUP_MAX_SOURCES,
            PlayerProgramSource::has_audio,
        ) {
            if active_sources.contains(&point.source.source.source_id) {
                continue;
            }
            let _ = self.ensure_audio_source_prepared(&point.source)?;
            let source_frame = point
                .source
                .source_frame_for_program_frame(point.program_frame);
            let request = EngineFrameRequest::new(&source_handle(&point.source), source_frame)?;
            self.inner.inner_mut().request_stream_warmup(request)?;
        }
        Ok(())
    }

    fn require_playlist_source(&self, source_id: &str) -> Result<(), BroadcastEngineError> {
        if source_id == self.source_id {
            return Ok(());
        }
        Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            format!("audio request is not for playlist source: {source_id}"),
        )
        .with_source_id(source_id.to_string()))
    }

    fn silence_packet(
        &self,
        frame: CoreFrameNumber,
    ) -> Result<AudioFramePacket<Vec<u8>>, BroadcastEngineError> {
        let byte_len = audio_packet_byte_len_for_frame(frame, &self.audio_format, self.timebase)?;
        Ok(AudioFramePacket {
            source_id: self.source_id.clone(),
            start_frame: frame,
            frame_count: 1,
            audio_format: Some(self.audio_format.clone()),
            payload: vec![0; byte_len],
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PlayerDecodePolicy {
    recommended_backend: Option<String>,
    video_prefetch_frames: Option<u16>,
    video_cache_frames: Option<usize>,
    audio_prefetch_frames: Option<u16>,
    audio_cache_frames: Option<usize>,
    video_prefetch_rules: Vec<FfmpegVideoPrefetchRule>,
}

impl PlayerDecodePolicy {
    fn from_runtime(runtime: &Value) -> Self {
        runtime
            .get("hardware_profile")
            .map(Self::from_hardware_profile)
            .unwrap_or_default()
    }

    fn from_hardware_profile(profile: &Value) -> Self {
        let media_decode = profile.get("media_decode");
        let recommended_backend = local_player_decode_backend();
        let video_prefetch_frames =
            media_decode.and_then(|media| positive_u16_field(media, "video_prefetch_frames"));
        let video_cache_frames =
            media_decode.and_then(|media| positive_usize_field(media, "video_cache_frames"));
        let audio_prefetch_frames =
            media_decode.and_then(|media| positive_u16_field(media, "audio_prefetch_frames"));
        let audio_cache_frames =
            media_decode.and_then(|media| positive_usize_field(media, "audio_cache_frames"));
        let video_prefetch_rules = media_decode
            .and_then(|media| media.get("video_prefetch_rules"))
            .and_then(Value::as_array)
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(video_prefetch_rule_from_json)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            recommended_backend,
            video_prefetch_frames,
            video_cache_frames,
            audio_prefetch_frames,
            audio_cache_frames,
            video_prefetch_rules,
        }
    }

    fn adapter_decode_policy(&self) -> AdapterDecodePolicy {
        self.video_prefetch_rules
            .iter()
            .cloned()
            .fold(AdapterDecodePolicy::fixed(), |policy, rule| {
                policy.with_video_prefetch_rule(rule)
            })
    }
}

pub struct PlayerRemote {
    runtime: Option<PlayerRuntimeSession>,
    program: Option<PlayerProgramState>,
    active_program_request: Option<BroadcastProgramOpenRequest>,
    prepared_program: Option<PreparedProgram>,
    pending_program_open: Option<PendingProgramOpen>,
    next_program_open_sequence: u64,
    pending_program_play: bool,
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
    loaded_program: Option<SourceReadyBounds>,
    source_ready_sent: bool,
}

type ProgramOpenResult = Result<ProgramRuntimeBuild, String>;

struct PreparedProgram {
    request: BroadcastProgramOpenRequest,
    build: ProgramRuntimeBuild,
}

struct PendingProgramOpen {
    sequence: u64,
    mode: PendingProgramMode,
    request: BroadcastProgramOpenRequest,
    rx: Receiver<ProgramOpenResult>,
    started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingProgramMode {
    Prepare,
    Open,
}

#[derive(Debug, Clone, Copy)]
struct SourceReadyBounds {
    fps: f64,
    duration_frames: i64,
    in_frame: i64,
    out_frame: i64,
    field_mode: FieldMode,
}

const STILL_SEEK_DEBOUNCE: Duration = Duration::from_millis(80);

impl Default for PlayerRemote {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerRemote {
    pub fn new() -> Self {
        Self {
            runtime: None,
            program: None,
            active_program_request: None,
            prepared_program: None,
            pending_program_open: None,
            next_program_open_sequence: 0,
            pending_program_play: false,
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
            loaded_program: None,
            source_ready_sent: false,
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

    #[allow(dead_code)]
    pub fn source_kind(&self) -> Option<BroadcastSourceKind> {
        self.identity.as_ref().map(|identity| identity.source_kind)
    }

    #[allow(dead_code)]
    pub fn source_sec(&self) -> f64 {
        self.source_sec
    }

    #[allow(dead_code)]
    pub fn source_frame(&self) -> FrameNumber {
        self.source_frame
    }

    #[allow(dead_code)]
    pub fn playing(&self) -> bool {
        self.playing
    }

    #[allow(dead_code)]
    pub fn has_source(&self) -> bool {
        self.runtime.is_some()
    }

    pub fn matches_source(&self, request: &BroadcastPlayerOpenRequest) -> bool {
        if self.program.is_some() {
            return false;
        }
        self.last_request.as_ref().is_some_and(|prev| {
            prev.source_ref == request.source_ref
                && prev.media_input == request.media_input
                && approx(prev.source_fps, request.source_fps)
                && prev.source_timebase == request.source_timebase
                && prev.has_audio == request.has_audio
                && prev.audio_channels == request.audio_channels
        })
    }

    pub fn configure_runtime_profile(&mut self, runtime: &Value) {
        self.decode_policy = PlayerDecodePolicy::from_runtime(runtime);
    }

    #[allow(dead_code)]
    pub fn set_display_frame(&mut self, frame: FrameNumber) {
        self.set_display_core_frame(old_to_core_frame(frame));
    }

    pub fn dispatch(&mut self, command: PlayerCommand, ctx: &egui::Context) {
        match command {
            PlayerCommand::Open(request) => self.open(request, ctx),
            PlayerCommand::PrepareProgram(request) => self.prepare_program(request, ctx),
            PlayerCommand::OpenProgram(request) => self.open_program(request, ctx),
            PlayerCommand::PlayProgram(request) => self.play_program(request, ctx),
            PlayerCommand::Play => self.play(ctx),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlay => {
                if self.pending_program_open.is_some() && self.pending_program_play {
                    self.pending_program_play = false;
                    self.status = "Opening program".into();
                    ctx.request_repaint();
                } else if self.playing {
                    self.pause();
                } else {
                    self.play(ctx);
                }
            }
            PlayerCommand::SeekFrame {
                frame,
                still,
                coalesce,
            } => {
                let _ = still;
                self.seek_core_frame(old_to_core_frame(frame), coalesce, ctx)
            }
            PlayerCommand::Stop => self.stop(),
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context) -> Vec<PlayerEvent> {
        self.poll_pending_program_open(ctx);
        self.flush_still(ctx);
        let mut out = self.drain_pending_player_events();

        let now = self.now_tick();
        if let Some(runtime) = self.runtime.as_mut() {
            if self.playing {
                runtime.runtime.tick(now);
            }
        }
        out.extend(self.drain_runtime_events());

        if self.playing {
            ctx.request_repaint();
        }

        out
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<PlayerEvent> {
        self.poll_pending_program_open(ctx);
        self.flush_still(ctx);
        let mut out = self.drain_pending_player_events();
        out.extend(self.drain_runtime_events());

        if self.playing {
            ctx.request_repaint();
        }

        out
    }

    fn drain_pending_player_events(&mut self) -> Vec<PlayerEvent> {
        let mut out = self.pending_events.drain(..).collect::<Vec<_>>();
        if let Some(err) = self.pending_error.take() {
            out.push(PlayerEvent::Error(err));
        }
        out
    }

    pub fn stop(&mut self) {
        self.pending_still = None;
        self.pending_program_open = None;
        self.pending_program_play = false;
        if self.runtime.is_some() {
            let events = self.dispatch_runtime_command(
                "app-stop",
                BroadcastPlayerProtocolCommand::Stop,
                self.now_tick(),
            );
            self.pending_events.extend(events);
        }
        self.runtime = None;
        self.program = None;
        self.active_program_request = None;
        self.prepared_program = None;
        self.identity = None;
        self.last_request = None;
        self.loaded_program = None;
        self.source_ready_sent = false;
        self.playing = false;
        self.active = false;
        self.status = "Stopped".into();
    }

    fn push_source_ready(&mut self, out: &mut Vec<PlayerEvent>) {
        if self.source_ready_sent {
            return;
        }
        let Some(bounds) = self.loaded_program else {
            return;
        };
        self.source_ready_sent = true;
        out.push(PlayerEvent::SourceReady {
            fps: bounds.fps,
            duration_frames: bounds.duration_frames,
            in_frame: bounds.in_frame,
            out_frame: bounds.out_frame,
            field_mode: bounds.field_mode,
        });
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

        self.pending_program_open = None;
        self.pending_program_play = false;
        self.prepared_program = None;
        self.program = None;
        self.active_program_request = None;
        self.pending_still = None;
        self.last_still_frame = None;
        self.playing = false;
        self.active = false;
        self.status = "Open".into();

        match build_runtime_session(&request, &self.decode_policy) {
            Ok(session) => {
                let start_frame = old_to_core_frame(request.start_source_frame);
                let start_frame = clamp_core_frame(start_frame, session.range);
                let startup_warnings = session.startup_warnings.clone();
                let playback_request = playback_request_from_source(
                    format!("app-open-{}", session.source.source_id),
                    session.source.clone(),
                    session.range,
                    start_frame,
                );
                match playback_request {
                    Ok(playback_request) => {
                        self.loaded_program = Some(source_ready_bounds_from_session(&session));
                        self.source_ready_sent = false;
                        self.runtime = Some(session);
                        self.identity = Some(identity_from_request(&request));
                        self.last_request = Some(request);
                        self.active = true;
                        self.set_display_core_frame(start_frame);
                        let mut events = self.dispatch_runtime_command(
                            "app-set-playback-request",
                            BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                                request: Box::new(playback_request),
                            },
                            self.now_tick(),
                        );
                        events.extend(
                            self.apply_protocol_events(
                                startup_warnings
                                    .into_iter()
                                    .map(|message| BroadcastPlayerProtocolEvent::DecodeWarning {
                                        message,
                                    })
                                    .collect(),
                            ),
                        );
                        self.pending_events.extend(events);
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

    fn prepare_program(&mut self, request: BroadcastProgramOpenRequest, ctx: &egui::Context) {
        if self.program_matches(&request)
            || self.prepared_program_matches(&request)
            || self.pending_program_matches(&request)
        {
            return;
        }
        self.start_program_build(request, PendingProgramMode::Prepare, ctx);
    }

    fn open_program(&mut self, request: BroadcastProgramOpenRequest, ctx: &egui::Context) {
        if self.program_matches(&request) && self.runtime.is_some() {
            self.active = true;
            self.status = "Ready".into();
            ctx.request_repaint();
            return;
        }
        if self.prepared_program_matches(&request) {
            let Some(prepared) = self.prepared_program.take() else {
                return;
            };
            crate::player_log::log_info("program-open", "open from prepared program");
            let _ = self.finish_program_open(request, prepared.build, ctx);
            return;
        }
        if let Some(pending) = self.pending_program_open.as_mut() {
            if same_program_request(&pending.request, &request) {
                pending.mode = PendingProgramMode::Open;
                pending.request = request;
                crate::player_log::log_info("program-open", "open waits for pending prepare");
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
        }
        self.start_program_build(request, PendingProgramMode::Open, ctx);
    }

    fn play_program(&mut self, request: BroadcastProgramOpenRequest, ctx: &egui::Context) {
        if self.program_matches(&request) && self.runtime.is_some() {
            self.active = true;
            self.status = "Ready".into();
            self.play(ctx);
            return;
        }
        if self.prepared_program_matches(&request) {
            let Some(prepared) = self.prepared_program.take() else {
                return;
            };
            crate::player_log::log_info("program-open", "play from prepared program");
            if self
                .finish_program_open(request, prepared.build, ctx)
                .is_ok()
            {
                self.play(ctx);
            }
            return;
        }
        if let Some(pending) = self.pending_program_open.as_mut() {
            if same_program_request(&pending.request, &request) {
                pending.mode = PendingProgramMode::Open;
                pending.request = request;
                self.pending_program_play = true;
                self.status = "Opening program".into();
                crate::player_log::log_info("program-open", "autoplay waits for pending program");
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
        }
        let request_for_match = request.clone();
        self.start_program_build(request, PendingProgramMode::Open, ctx);
        if self.pending_program_matches(&request_for_match) {
            self.pending_program_play = true;
            self.status = "Opening program".into();
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn start_program_build(
        &mut self,
        request: BroadcastProgramOpenRequest,
        mode: PendingProgramMode,
        ctx: &egui::Context,
    ) {
        self.pending_still = None;
        self.last_still_frame = None;
        self.pending_error = None;
        self.prepared_program = None;
        self.status = match mode {
            PendingProgramMode::Prepare => "Preparing program".into(),
            PendingProgramMode::Open => "Opening program".into(),
        };
        crate::player_log::log_info(
            "program-open",
            format!(
                "{mode:?} start items={} start_frame={}",
                request.items.len(),
                request.start_program_frame.0
            ),
        );
        if mode == PendingProgramMode::Open {
            self.playing = false;
            self.active = false;
            self.runtime = None;
            self.program = None;
            self.active_program_request = None;
            self.identity = None;
            self.last_request = None;
            self.loaded_program = None;
            self.source_ready_sent = false;
            self.pending_program_play = false;
        }

        let sequence = self.next_program_open_sequence;
        self.next_program_open_sequence = self.next_program_open_sequence.wrapping_add(1);
        let (tx, rx) = mpsc::channel();
        let worker_request = request.clone();
        let decode_policy = self.decode_policy.clone();
        let spawn_result = thread::Builder::new()
            .name(format!("qnc-program-open-{sequence}"))
            .spawn(move || {
                let result = build_program_runtime_session(&worker_request, &decode_policy);
                let _ = tx.send(result);
            });

        match spawn_result {
            Ok(_) => {
                self.pending_program_open = Some(PendingProgramOpen {
                    sequence,
                    mode,
                    request,
                    rx,
                    started_at: Instant::now(),
                });
                ctx.request_repaint_after(Duration::from_millis(16));
            }
            Err(err) => {
                self.fail_program_open(format!("Ne mogu otvoriti program worker: {err}"));
            }
        }
        ctx.request_repaint();
    }

    fn poll_pending_program_open(&mut self, ctx: &egui::Context) {
        let Some((sequence, result)) =
            self.pending_program_open
                .as_ref()
                .and_then(|pending| match pending.rx.try_recv() {
                    Ok(result) => Some((pending.sequence, result)),
                    Err(TryRecvError::Empty) => {
                        ctx.request_repaint_after(Duration::from_millis(16));
                        None
                    }
                    Err(TryRecvError::Disconnected) => Some((
                        pending.sequence,
                        Err("Program open worker je prekinut".to_string()),
                    )),
                })
        else {
            return;
        };
        let Some(pending) = self.pending_program_open.take() else {
            return;
        };
        if pending.sequence != sequence {
            return;
        }

        let should_play = self.pending_program_play;
        self.pending_program_play = false;
        let elapsed_ms = pending.started_at.elapsed().as_millis();
        crate::player_log::log_info(
            "program-open",
            format!("{:?} ready in {elapsed_ms} ms", pending.mode),
        );
        match result {
            Ok(build) => match pending.mode {
                PendingProgramMode::Prepare => {
                    let request = pending.request.clone();
                    self.prepared_program = Some(PreparedProgram {
                        request: request.clone(),
                        build,
                    });
                    if self.status == "Preparing program" {
                        self.status = "Program prepared".into();
                    }
                    self.pending_events
                        .push(PlayerEvent::ProgramPrepared { request });
                }
                PendingProgramMode::Open => {
                    if self
                        .finish_program_open(pending.request, build, ctx)
                        .is_ok()
                        && should_play
                    {
                        self.play(ctx);
                    }
                }
            },
            Err(err) => match pending.mode {
                PendingProgramMode::Prepare => self.fail_program_prepare(pending.request, err),
                PendingProgramMode::Open => self.fail_program_open(err),
            },
        }
        ctx.request_repaint();
    }

    fn finish_program_open(
        &mut self,
        request: BroadcastProgramOpenRequest,
        build: ProgramRuntimeBuild,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let start_frame =
            old_to_core_frame(request.start_program_frame).min(build.program.duration_frames - 1);
        let configured_start_frame = build.configured_start_frame;
        let startup_warnings = build.session.startup_warnings.clone();

        self.loaded_program = Some(source_ready_bounds_from_program(&build.program));
        self.source_ready_sent = false;
        self.identity = Some(identity_from_program_request(&request));
        self.runtime = Some(build.session);
        self.program = Some(build.program);
        self.active_program_request = Some(request);
        self.last_request = None;
        self.active = true;
        self.status = "Ready".into();
        self.set_display_program_frame(start_frame);
        let mut events = self.drain_runtime_events();
        if configured_start_frame != start_frame {
            events.extend(self.dispatch_runtime_command(
                "app-program-cue-frame",
                BroadcastPlayerProtocolCommand::CueFrame {
                    frame: start_frame,
                    present_frame: true,
                },
                self.now_tick(),
            ));
        }
        events.extend(
            self.apply_protocol_events(
                startup_warnings
                    .into_iter()
                    .map(|message| BroadcastPlayerProtocolEvent::DecodeWarning { message })
                    .collect(),
            ),
        );
        self.pending_events.extend(events);
        ctx.request_repaint();
        Ok(())
    }

    fn fail_program_open(&mut self, err: String) {
        self.runtime = None;
        self.program = None;
        self.active_program_request = None;
        self.identity = None;
        self.last_request = None;
        self.loaded_program = None;
        self.source_ready_sent = false;
        self.playing = false;
        self.active = false;
        self.status = err.clone();
        self.pending_error = Some(err);
    }

    fn fail_program_prepare(&mut self, request: BroadcastProgramOpenRequest, err: String) {
        self.prepared_program = None;
        self.pending_program_play = false;
        self.status = err.clone();
        crate::player_log::log_error("player-prepare", &err);
        self.pending_events.push(PlayerEvent::ProgramPrepareFailed {
            request,
            error: err,
        });
    }

    fn program_matches(&self, request: &BroadcastProgramOpenRequest) -> bool {
        self.runtime.is_some()
            && self.program.is_some()
            && self
                .active_program_request
                .as_ref()
                .is_some_and(|active| same_program_request(active, request))
    }

    fn prepared_program_matches(&self, request: &BroadcastProgramOpenRequest) -> bool {
        self.prepared_program
            .as_ref()
            .is_some_and(|prepared| same_program_request(&prepared.request, request))
    }

    fn pending_program_matches(&self, request: &BroadcastProgramOpenRequest) -> bool {
        self.pending_program_open
            .as_ref()
            .is_some_and(|pending| same_program_request(&pending.request, request))
    }

    fn play(&mut self, ctx: &egui::Context) {
        if self.pending_program_open.is_some() {
            self.pending_program_play = true;
            self.status = "Opening program".into();
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        self.commit_pending_still_before_play(ctx);
        if self.runtime.is_none() {
            let Some(request) = self.last_request.clone() else {
                self.status = "Odaberi source kadar prije play".into();
                self.pending_error = Some(self.status.clone());
                return;
            };
            self.open(request, ctx);
        }
        let now = self.now_tick();
        if self.runtime.is_some() {
            let events = self.dispatch_runtime_command(
                "app-play",
                BroadcastPlayerProtocolCommand::Play,
                now,
            );
            self.pending_events.extend(events);
            ctx.request_repaint();
        } else {
            self.status = "Source nije otvoren".into();
            self.pending_error = Some(self.status.clone());
        }
    }

    fn commit_pending_still_before_play(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.take_pending_still_for_play() else {
            return;
        };
        if self.runtime.is_some() {
            self.cue_still_frame(frame, ctx);
        }
    }

    fn take_pending_still_for_play(&mut self) -> Option<CoreFrameNumber> {
        self.pending_still.take().map(|(frame, _)| frame)
    }

    fn pause(&mut self) {
        self.pending_still = None;
        self.pending_program_play = false;
        if self.pending_program_open.is_some() {
            self.status = "Opening program".into();
            return;
        }
        if self.runtime.is_some() {
            let events = self.dispatch_runtime_command(
                "app-pause",
                BroadcastPlayerProtocolCommand::Pause,
                self.now_tick(),
            );
            self.pending_events.extend(events);
        }
        self.playing = false;
        self.status = "Paused".into();
    }

    fn seek_core_frame(&mut self, frame: CoreFrameNumber, coalesce: bool, ctx: &egui::Context) {
        if self.program.is_some() {
            self.seek_program_frame(frame, coalesce, ctx);
            return;
        }
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

    fn seek_program_frame(&mut self, frame: CoreFrameNumber, coalesce: bool, ctx: &egui::Context) {
        if self.playing {
            self.pause();
        }
        let frame = self.clamp_to_program_range(frame);
        self.set_display_program_frame(frame);
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
        if self.program.is_some() {
            self.cue_program_frame(frame, ctx);
            return;
        }
        let frame = self.clamp_to_runtime_range(frame);
        if self.last_still_frame == Some(frame) && !self.playing {
            return;
        }
        self.last_still_frame = Some(frame);
        if self.runtime.is_none() {
            self.status = "Nema source za seek".into();
            self.pending_error = Some(self.status.clone());
            return;
        };

        let events = self.dispatch_runtime_command(
            "app-cue-frame",
            BroadcastPlayerProtocolCommand::CueFrame {
                frame,
                present_frame: true,
            },
            self.now_tick(),
        );
        self.playing = false;
        self.active = true;
        self.status = "Ready".into();
        self.set_display_core_frame(frame);
        self.pending_events.extend(events);
        ctx.request_repaint();
    }

    fn cue_program_frame(&mut self, frame: CoreFrameNumber, ctx: &egui::Context) {
        let frame = self.clamp_to_program_range(frame);
        if self.last_still_frame == Some(frame) && !self.playing {
            return;
        }
        self.last_still_frame = Some(frame);
        let events = self.dispatch_runtime_command(
            "app-program-cue-frame",
            BroadcastPlayerProtocolCommand::CueFrame {
                frame,
                present_frame: true,
            },
            self.now_tick(),
        );
        self.playing = false;
        self.active = true;
        self.status = "Ready".into();
        self.set_display_program_frame(frame);
        self.pending_events.extend(events);
        ctx.request_repaint();
    }

    fn dispatch_runtime_command(
        &mut self,
        command_id: impl Into<String>,
        command: BroadcastPlayerProtocolCommand,
        now_tick: ClockTick,
    ) -> Vec<PlayerEvent> {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime
                .runtime
                .dispatch_at(PlayerRuntimeCommand::new(command_id, command), now_tick);
        }
        self.drain_runtime_events()
    }

    fn drain_runtime_events(&mut self) -> Vec<PlayerEvent> {
        let protocol_events = self
            .runtime
            .as_mut()
            .map(|runtime| runtime.runtime.drain_events())
            .unwrap_or_default();
        self.apply_protocol_events(protocol_events)
    }

    fn apply_protocol_events(
        &mut self,
        protocol_events: Vec<BroadcastPlayerProtocolEvent>,
    ) -> Vec<PlayerEvent> {
        let mut out = Vec::new();

        if let Some(runtime) = self.runtime.as_mut() {
            let _ = runtime.event_bridge.apply_events(&protocol_events);
        }

        for event in protocol_events {
            match event {
                BroadcastPlayerProtocolEvent::CarrierPositionChanged { frame, status, .. } => {
                    self.set_display_transport_frame(frame);
                    self.playing = status == TransportStatus::Playing;
                    self.active = true;
                    self.status = status_label(status).to_string();
                    self.push_source_ready(&mut out);
                    out.push(PlayerEvent::State {
                        source_frame: self.source_frame,
                        source_sec: self.source_sec,
                        playing: self.playing,
                        status: self.status.clone(),
                    });
                }
                BroadcastPlayerProtocolEvent::TransportStatusChanged { status } => {
                    // Playhead authority is transport carrier — never a stale monitor buffer.
                    self.sync_display_from_transport();
                    self.playing = status == TransportStatus::Playing;
                    self.active = status != TransportStatus::Empty;
                    self.status = status_label(status).to_string();
                    if matches!(status, TransportStatus::Ready | TransportStatus::Paused) {
                        self.push_source_ready(&mut out);
                    }
                    out.push(PlayerEvent::State {
                        source_frame: self.source_frame,
                        source_sec: self.source_sec,
                        playing: self.playing,
                        status: self.status.clone(),
                    });
                }
                BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame } => {
                    if self.program.is_some() {
                        let program_frame = self
                            .program
                            .as_ref()
                            .map(|program| frame.min(program.duration_frames))
                            .unwrap_or(frame);
                        self.set_display_program_frame(program_frame);
                        self.playing = false;
                        self.status = "Kraj programa".into();
                        out.push(PlayerEvent::BoundaryReached {
                            source_frame: core_to_old_frame(program_frame),
                        });
                    } else {
                        self.set_display_transport_frame(frame);
                        self.playing = false;
                        self.status = "Paused".into();
                        out.push(PlayerEvent::BoundaryReached {
                            source_frame: core_to_old_frame(frame),
                        });
                    }
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

    fn take_monitor_frame_event(&mut self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        let Some(runtime) = self.runtime.as_mut() else {
            return out;
        };
        let Ok(snapshot) = runtime.monitor.snapshot() else {
            return Vec::new();
        };
        if snapshot.frame_revision == runtime.last_frame_revision {
            return Vec::new();
        }
        let carrier = runtime.runtime.transport().state().carrier_frame;
        let Some(frame_buffer) = snapshot.last_frame_buffer else {
            runtime.last_frame_revision = snapshot.frame_revision;
            return Vec::new();
        };
        // Ignore stale/wrong monitor decode; transport remains playhead authority.
        if frame_buffer.frame != carrier {
            runtime.last_frame_revision = snapshot.frame_revision;
            return Vec::new();
        }
        runtime.last_frame_revision = snapshot.frame_revision;
        let image = ColorImage::from_rgb(
            [
                frame_buffer.video_format.width as usize,
                frame_buffer.video_format.height as usize,
            ],
            &frame_buffer.bytes,
        );
        self.set_display_transport_frame(carrier);
        self.push_source_ready(&mut out);
        out.push(PlayerEvent::Frame {
            image,
            source_frame: self.source_frame,
            source_sec: self.source_sec,
            playing: self.playing,
        });
        out
    }

    fn sync_display_from_transport(&mut self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let carrier = runtime.runtime.transport().state().carrier_frame;
        self.set_display_transport_frame(carrier);
    }

    fn set_display_core_frame(&mut self, frame: CoreFrameNumber) {
        self.source_frame = core_to_old_frame(frame);
        if let Some(runtime) = self.runtime.as_ref() {
            self.source_sec = seconds_at_frame(frame, runtime.source.timebase);
        }
    }

    fn set_display_transport_frame(&mut self, frame: CoreFrameNumber) {
        if self.program.is_some() {
            self.set_display_program_frame(frame);
        } else {
            self.set_display_core_frame(frame);
        }
    }

    fn set_display_program_frame(&mut self, frame: CoreFrameNumber) {
        let frame = self.clamp_to_program_range(frame);
        self.source_frame = core_to_old_frame(frame);
        if let Some(program) = self.program.as_ref() {
            self.source_sec = seconds_at_frame(frame, program.timebase);
        } else if let Some(runtime) = self.runtime.as_ref() {
            self.source_sec = seconds_at_frame(frame, runtime.source.timebase);
        }
    }

    fn clamp_to_runtime_range(&self, frame: CoreFrameNumber) -> CoreFrameNumber {
        if self.program.is_some() {
            return self.clamp_to_program_range(frame);
        }
        self.runtime
            .as_ref()
            .map(|runtime| clamp_core_frame(frame, runtime.range))
            .unwrap_or(frame)
    }

    fn clamp_to_program_range(&self, frame: CoreFrameNumber) -> CoreFrameNumber {
        let Some(program) = self.program.as_ref() else {
            return frame;
        };
        if program.duration_frames == 0 {
            return 0;
        }
        frame.min(program.duration_frames)
    }

    fn now_tick(&self) -> ClockTick {
        self.started_at.elapsed().as_nanos()
    }
}

fn build_runtime_session(
    request: &BroadcastPlayerOpenRequest,
    decode_policy: &PlayerDecodePolicy,
) -> Result<PlayerRuntimeSession, String> {
    let media_input = ffmpeg_media_input(&request.media_input, "media path")?;

    let source_id = source_id_from_request(request);
    let toolchain = FfmpegToolchain::default();
    let mut source =
        probe_media_input_runtime_with_toolchain(&media_input, source_id.clone(), &toolchain)
            .map_err(|error| error.to_string())?
            .source;
    source.timebase = core_timebase_from_source_timebase(request.source_timebase)?;
    let field_label = source
        .video_format
        .as_ref()
        .map(|vf| match vf.field_mode {
            FieldMode::Progressive => "progressive",
            FieldMode::InterlacedUpperFirst => "interlaced-tff",
            FieldMode::InterlacedLowerFirst => "interlaced-bff",
        })
        .unwrap_or("no-video");
    let (w, h) = source
        .video_format
        .as_ref()
        .map(|vf| (vf.width, vf.height))
        .unwrap_or((0, 0));
    crate::player_log::log_info(
        "probe",
        &format!(
            "file={} {}x{} timebase={}/{} field={} audio={}",
            request.media_input.trim(),
            w,
            h,
            source.timebase.frame_rate_num,
            source.timebase.frame_rate_den,
            field_label,
            source.audio_format.is_some()
        ),
    );

    if !request.has_audio {
        source.audio_format = None;
    } else if source.audio_format.is_none() {
        source.audio_format = Some(AudioFormat::new(
            48_000,
            request.audio_channels.max(1) as u16,
        )?);
    }

    let range = range_from_request(request, &source)?;
    let registry =
        FfmpegSourceRegistry::from_media_inputs(BTreeMap::from([(source_id, media_input)]));
    let (hardware_decode, hardware_warning) = hardware_decode_from_policy(decode_policy);
    let video_decode = FfmpegVideoDecode::with_options(
        registry.clone(),
        video_decode_options(toolchain.clone(), hardware_decode, decode_policy),
    );
    let audio_output = FfmpegAudioOutput::with_options(
        registry.clone(),
        audio_decode_options(toolchain, decode_policy),
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
        PlayerInputOpen::Ffmpeg(FfmpegSourceOpen::new(registry)),
        PlayerInputVideoDecode::Ffmpeg(video_decode),
        PlayerInputAudioOutput::Ffmpeg(audio),
        presenter,
    )
    .with_decode_burst_frames(SOURCE_DECODE_BURST_FRAMES);

    let session = PlayerRuntimeSession {
        runtime: BroadcastPlayerRuntime::new(transport),
        monitor,
        event_bridge,
        source: source.clone(),
        range,
        startup_warnings,
        last_frame_revision: 0,
    };

    Ok(session)
}

struct ProgramRuntimeBuild {
    session: PlayerRuntimeSession,
    program: PlayerProgramState,
    configured_start_frame: CoreFrameNumber,
}

fn build_program_runtime_session(
    request: &BroadcastProgramOpenRequest,
    decode_policy: &PlayerDecodePolicy,
) -> Result<ProgramRuntimeBuild, String> {
    let duration_frames = request.duration_frames.max(1) as CoreFrameNumber;
    let start_program_frame =
        old_to_core_frame(request.start_program_frame).min(duration_frames.saturating_sub(1));
    if request.items.is_empty() {
        return Err("Program nema playlist iteme".into());
    }

    let toolchain = FfmpegToolchain::default();
    let mut registry_inputs = BTreeMap::new();
    let mut probe_cache = BTreeMap::new();
    let mut items = Vec::new();
    let mut startup_warnings = Vec::new();
    let mut source_ids = ProgramSourceIdAllocator::default();

    for (item_index, item) in request.items.iter().enumerate() {
        let mut sources = Vec::new();
        for (source_index, source) in item.sources.iter().enumerate() {
            let source_id = source_ids.allocate(request, item, source, item_index, source_index);
            sources.push(build_program_source_runtime(
                item,
                source,
                source_id,
                &toolchain,
                &mut registry_inputs,
                &mut probe_cache,
            )?);
        }
        if sources.is_empty() {
            return Err(format!(
                "Program item nema playable media · {}",
                item.item_id
            ));
        }
        items.push(PlayerProgramItem {
            spec: item.clone(),
            sources,
        });
    }

    if items.is_empty() {
        return Err("Program nema validan playlist item".to_string());
    }
    let timebase = program_timebase_from_items(&items)
        .ok_or_else(|| "Program input nema valjan probe timebase".to_string())?;
    let program = PlayerProgramState {
        timebase,
        duration_frames,
        items,
    };
    let playlist_input = playlist_input_source(request, &program)?;
    let range = CoreFrameRange::new(0, playlist_input.duration_frames)
        .map_err(|error| error.to_string())?;
    let registry = FfmpegSourceRegistry::from_media_inputs(registry_inputs);
    let (hardware_decode, hardware_warning) = hardware_decode_from_policy(decode_policy);
    let audio_sink_result = build_audio_sink();
    startup_warnings.extend(
        [hardware_warning, audio_sink_result.1]
            .into_iter()
            .flatten(),
    );

    let playlist_video_format = playlist_input
        .video_format
        .clone()
        .unwrap_or(default_video_format()?);
    crate::player_log::log_monitor(
        "program-format",
        format!(
            "program_id={} mode={:?} output={}x{}",
            request.program_id,
            request.preview_video_resolution,
            playlist_video_format.width,
            playlist_video_format.height
        ),
    );
    let playlist_audio_format = playlist_input
        .audio_format
        .clone()
        .unwrap_or(AudioFormat::new(48_000, 2)?);
    let video_decode = FfmpegVideoDecode::with_options(
        registry.clone(),
        playlist_video_decode_options(
            toolchain.clone(),
            hardware_decode,
            decode_policy,
            playlist_video_format.clone(),
        ),
    );
    let audio_output = FfmpegAudioOutput::with_options(
        registry.clone(),
        playlist_audio_decode_options(toolchain, decode_policy),
    );
    let av_sync = AvSyncTelemetry::default();
    let audio = AudioOutputWithSink::new(
        audio_output,
        AudioPacketTelemetry::new(AvSyncAudioPacketSink::new(
            audio_sink_result.0,
            av_sync.clone(),
        )),
    );
    let monitor = SharedPlayerMonitor::default();
    let event_bridge = MonitorEventBridge::new(monitor.clone());
    let presenter = build_frame_presenter(&playlist_input, monitor.clone(), av_sync);
    let playlist_source_id = playlist_input.source_id.clone();
    let transport = TransportEngine::new(
        PlayerInputOpen::Playlist(PlaylistInputOpen {
            source: playlist_input.clone(),
        }),
        PlayerInputVideoDecode::Playlist(PlaylistInputVideoDecode {
            source_id: playlist_source_id.clone(),
            video_format: playlist_video_format,
            program: program.clone(),
            prepared_sources: BTreeSet::new(),
            inner: video_decode,
        }),
        PlayerInputAudioOutput::Playlist(PlaylistInputAudioOutput {
            source_id: playlist_source_id,
            timebase,
            audio_format: playlist_audio_format,
            program: program.clone(),
            prepared_sources: BTreeSet::new(),
            inner: audio,
        }),
        presenter,
    )
    .with_decode_burst_frames(PLAYLIST_DECODE_BURST_FRAMES);

    let mut session = PlayerRuntimeSession {
        runtime: BroadcastPlayerRuntime::new(transport),
        monitor,
        event_bridge,
        source: playlist_input,
        range,
        startup_warnings,
        last_frame_revision: 0,
    };
    configure_program_runtime_session(&mut session, start_program_frame)?;

    Ok(ProgramRuntimeBuild {
        session,
        program,
        configured_start_frame: start_program_frame,
    })
}

fn configure_program_runtime_session(
    session: &mut PlayerRuntimeSession,
    start_frame: CoreFrameNumber,
) -> Result<(), String> {
    let playback_request = playback_request_from_source(
        format!("app-program-open-{}", session.source.source_id),
        session.source.clone(),
        session.range,
        start_frame,
    )?;
    session.runtime.dispatch_at(
        PlayerRuntimeCommand::new(
            "app-program-set-playlist-input",
            BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                request: Box::new(playback_request),
            },
        ),
        0,
    );
    Ok(())
}

fn build_program_source_runtime(
    item: &BroadcastProgramItem,
    source_spec: &BroadcastProgramSource,
    source_id: String,
    toolchain: &FfmpegToolchain,
    registry_inputs: &mut BTreeMap<String, FfmpegMediaInput>,
    probe_cache: &mut BTreeMap<String, qnc_media_ffmpeg::FfmpegProbeReport>,
) -> Result<PlayerProgramSource, String> {
    let media_input = ffmpeg_media_input(&source_spec.media_input, "Program media")?;
    let mut source =
        probe_program_source_runtime(source_id.clone(), &media_input, toolchain, probe_cache)?
            .source;
    source.timebase = core_timebase_from_source_timebase(source_spec.source_timebase)?;
    if !source_spec.has_video {
        source.video_format = None;
    }
    if !source_spec.has_audio {
        source.audio_format = None;
    } else if source.audio_format.is_none() {
        source.audio_format = Some(AudioFormat::new(
            48_000,
            source_spec.audio_channels.max(1) as u16,
        )?);
    }
    if source.video_format.is_none() && source.audio_format.is_none() {
        return Err(format!(
            "Program media nema playable track · {}",
            item.item_id
        ));
    }

    let range = range_from_source_ref(&source_spec.source_ref, &source)?;
    let record_in_frame = old_to_core_frame(item.record_in_frame);
    let record_out_frame = old_to_core_frame(item.record_out_frame).max(record_in_frame + 1);
    registry_inputs.insert(source_id, media_input);
    Ok(PlayerProgramSource {
        spec: source_spec.clone(),
        record_in_frame,
        record_out_frame,
        source,
        range,
    })
}

fn core_timebase_from_source_timebase(
    source_timebase: BroadcastSourceTimebase,
) -> Result<CoreTimebase, String> {
    if !source_timebase.is_valid() {
        return Err("Program source nema valjan source timebase".into());
    }
    CoreTimebase::new(source_timebase.fps_num, source_timebase.fps_den)
        .map_err(|err| err.to_string())
}

fn ffmpeg_media_input(value: &str, label: &str) -> Result<FfmpegMediaInput, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{label} nema media input"));
    }
    Ok(FfmpegMediaInput::from_uri(value))
}

fn playlist_input_source(
    request: &BroadcastProgramOpenRequest,
    program: &PlayerProgramState,
) -> Result<SourceRuntime, String> {
    let mut source = SourceRuntime::new(
        playlist_input_id(request),
        program.duration_frames,
        program.timebase,
    )?;
    if let Some(video_format) = playlist_video_format(request, program) {
        source = source.with_video_format(video_format);
    }
    if let Some(audio_format) = playlist_audio_format(program)? {
        source = source.with_audio_format(audio_format);
    }
    if source.video_format.is_none() && source.audio_format.is_none() {
        return Err("Program input nema playable track".into());
    }
    Ok(source)
}

fn playlist_video_format(
    request: &BroadcastProgramOpenRequest,
    program: &PlayerProgramState,
) -> Option<VideoFormat> {
    let source_format = program
        .media_sources_matching(PlayerProgramSource::has_video)
        .iter()
        .find_map(|take| {
            take.source
                .video_format
                .as_ref()
                .map(|format| format.clone())
        })?;
    Some(match request.preview_video_resolution {
        BroadcastProgramPreviewVideoResolution::FastPreview => {
            playlist_preview_video_format(&source_format)
        }
        BroadcastProgramPreviewVideoResolution::SourceRaster => {
            playlist_source_raster_video_format(&source_format)
        }
    })
}

fn playlist_preview_video_format(source_format: &VideoFormat) -> VideoFormat {
    VideoFormat::new(
        PLAYLIST_PREVIEW_WIDTH,
        PLAYLIST_PREVIEW_HEIGHT,
        FieldMode::Progressive,
        source_format.color_space.clone(),
    )
    .unwrap_or_else(|_| source_format.clone())
}

fn playlist_source_raster_video_format(source_format: &VideoFormat) -> VideoFormat {
    VideoFormat::new(
        source_format.width,
        source_format.height,
        FieldMode::Progressive,
        source_format.color_space.clone(),
    )
    .unwrap_or_else(|_| source_format.clone())
}

fn playlist_audio_format(program: &PlayerProgramState) -> Result<Option<AudioFormat>, String> {
    let mut sample_rate_hz = 48_000;
    let mut channel_count = 0_u16;
    for format in program
        .media_sources_matching(PlayerProgramSource::has_audio)
        .iter()
        .filter_map(|take| take.source.audio_format.as_ref())
    {
        sample_rate_hz = format.sample_rate_hz;
        channel_count = channel_count.max(format.channel_count);
    }
    if channel_count == 0 {
        return Ok(None);
    }
    channel_count = channel_count.max(2);
    Ok(Some(AudioFormat::new(sample_rate_hz, channel_count)?))
}

fn program_timebase_from_items(items: &[PlayerProgramItem]) -> Option<CoreTimebase> {
    for item in items {
        if let Some(source) = item.sources.iter().find(|source| source.has_video()) {
            return Some(source.source.timebase);
        }
    }
    items
        .iter()
        .flat_map(|item| item.sources.iter())
        .find(|source| source.has_audio())
        .map(|source| source.source.timebase)
}

fn default_video_format() -> Result<VideoFormat, String> {
    VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709)
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
                video_formats: video_formats_from_sources(std::slice::from_ref(source)),
            },
        ));
    }
    PlayerFramePresenter::Plain(presenter)
}

fn video_formats_from_sources(sources: &[SourceRuntime]) -> BTreeMap<String, VideoFormat> {
    sources
        .iter()
        .filter_map(|source| {
            source
                .video_format
                .clone()
                .map(|format| (source.source_id.clone(), format))
        })
        .collect()
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

fn video_decode_options(
    toolchain: FfmpegToolchain,
    hardware_decode: FfmpegHardwareDecode,
    policy: &PlayerDecodePolicy,
) -> FfmpegDecodeOptions {
    let mut options = FfmpegDecodeOptions::software()
        .with_toolchain(toolchain)
        .with_hardware_decode(hardware_decode)
        .with_decode_policy(policy.adapter_decode_policy());
    let prefetch_frames = policy
        .video_prefetch_frames
        .unwrap_or(SOURCE_VIDEO_PREFETCH_FRAMES)
        .max(SOURCE_VIDEO_PREFETCH_FRAMES);
    let cache_frames = policy
        .video_cache_frames
        .unwrap_or(SOURCE_VIDEO_CACHE_FRAMES)
        .max(SOURCE_VIDEO_CACHE_FRAMES);
    options = options
        .with_video_prefetch_frames(prefetch_frames)
        .with_video_cache_frames(cache_frames);
    options
}

fn audio_decode_options(
    toolchain: FfmpegToolchain,
    policy: &PlayerDecodePolicy,
) -> FfmpegAudioDecodeOptions {
    let mut options = FfmpegAudioDecodeOptions::default().with_toolchain(toolchain);
    let prefetch_frames = policy
        .audio_prefetch_frames
        .unwrap_or(SOURCE_AUDIO_PREFETCH_FRAMES)
        .max(SOURCE_AUDIO_PREFETCH_FRAMES);
    let cache_frames = policy
        .audio_cache_frames
        .unwrap_or(SOURCE_AUDIO_CACHE_FRAMES)
        .max(SOURCE_AUDIO_CACHE_FRAMES);
    options = options
        .with_audio_prefetch_frames(prefetch_frames)
        .with_audio_cache_frames(cache_frames);
    options
}

fn playlist_video_decode_options(
    toolchain: FfmpegToolchain,
    hardware_decode: FfmpegHardwareDecode,
    policy: &PlayerDecodePolicy,
    output_format: VideoFormat,
) -> FfmpegDecodeOptions {
    let options = video_decode_options(toolchain, hardware_decode, policy);
    let prefetch_frames = policy
        .video_prefetch_frames
        .unwrap_or(PLAYLIST_VIDEO_PREFETCH_FRAMES)
        .max(PLAYLIST_VIDEO_PREFETCH_FRAMES);
    let cache_frames = policy
        .video_cache_frames
        .unwrap_or(PLAYLIST_VIDEO_CACHE_FRAMES)
        .max(PLAYLIST_VIDEO_CACHE_FRAMES);
    options
        .with_video_output_format(output_format)
        .with_video_prefetch_frames(prefetch_frames)
        .with_video_cache_frames(cache_frames)
}

fn playlist_audio_decode_options(
    toolchain: FfmpegToolchain,
    policy: &PlayerDecodePolicy,
) -> FfmpegAudioDecodeOptions {
    let options = audio_decode_options(toolchain, policy);
    let prefetch_frames = policy
        .audio_prefetch_frames
        .unwrap_or(PLAYLIST_AUDIO_PREFETCH_FRAMES)
        .max(PLAYLIST_AUDIO_PREFETCH_FRAMES);
    let cache_frames = policy
        .audio_cache_frames
        .unwrap_or(PLAYLIST_AUDIO_CACHE_FRAMES)
        .max(PLAYLIST_AUDIO_CACHE_FRAMES);
    options
        .with_audio_prefetch_frames(prefetch_frames)
        .with_audio_cache_frames(cache_frames)
}

fn hardware_decode_from_policy(
    policy: &PlayerDecodePolicy,
) -> (FfmpegHardwareDecode, Option<String>) {
    let Some(value) = policy.recommended_backend.as_deref() else {
        return (FfmpegHardwareDecode::Software, None);
    };
    hardware_decode_from_backend_label(value, QNC_PLAYER_HWACCEL_ENV)
}

fn local_player_decode_backend() -> Option<String> {
    env::var(QNC_PLAYER_HWACCEL_ENV)
        .ok()
        .and_then(|value| normalize_decode_backend(&value))
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

fn positive_u16_field(value: &Value, key: &str) -> Option<u16> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn positive_usize_field(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn non_empty_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn video_prefetch_rule_from_json(value: &Value) -> Option<FfmpegVideoPrefetchRule> {
    let mut rule = FfmpegVideoPrefetchRule::new(positive_u16_field(value, "min_prefetch_frames")?);
    if let Some(container) = non_empty_string_field(value, "container_contains") {
        rule = rule.when_container_contains(container);
    }
    if let Some(codec) = non_empty_string_field(value, "codec") {
        rule = rule.when_codec(codec);
    }
    if let Some(pixel_format) = non_empty_string_field(value, "pixel_format_contains") {
        rule = rule.when_pixel_format_contains(pixel_format);
    }
    if let Some(profile) = non_empty_string_field(value, "profile_contains") {
        rule = rule.when_profile_contains(profile);
    }
    Some(rule)
}

fn probe_program_source_runtime(
    source_id: String,
    media_input: &FfmpegMediaInput,
    toolchain: &FfmpegToolchain,
    probe_cache: &mut BTreeMap<String, qnc_media_ffmpeg::FfmpegProbeReport>,
) -> Result<qnc_media_ffmpeg::FfmpegProbeReport, String> {
    let cache_key = media_input_identity(media_input.label().as_str());
    if let Some(cached) = probe_cache.get(&cache_key) {
        let mut report = cached.clone();
        report.source.source_id = source_id;
        return Ok(report);
    }
    let report =
        probe_media_input_runtime_with_toolchain(media_input, source_id.clone(), toolchain)
            .map_err(|error| error.to_string())?;
    probe_cache.insert(cache_key, report.clone());
    Ok(report)
}

fn source_ready_bounds_from_session(session: &PlayerRuntimeSession) -> SourceReadyBounds {
    let timebase = session.source.timebase;
    let fps = f64::from(timebase.frame_rate_num) / f64::from(timebase.frame_rate_den);
    let field_mode = session
        .source
        .video_format
        .as_ref()
        .map(|vf| vf.field_mode)
        .unwrap_or(FieldMode::Progressive);
    SourceReadyBounds {
        fps,
        duration_frames: i64::try_from(session.source.duration_frames).unwrap_or(i64::MAX),
        in_frame: i64::try_from(session.range.start_frame).unwrap_or(i64::MAX),
        out_frame: i64::try_from(session.range.end_frame).unwrap_or(i64::MAX),
        field_mode,
    }
}

fn source_ready_bounds_from_program(program: &PlayerProgramState) -> SourceReadyBounds {
    let fps =
        f64::from(program.timebase.frame_rate_num) / f64::from(program.timebase.frame_rate_den);
    let field_mode = program
        .media_sources_matching(PlayerProgramSource::has_video)
        .iter()
        .find_map(|take| {
            take.source
                .video_format
                .as_ref()
                .map(|format| format.field_mode)
        })
        .unwrap_or(FieldMode::Progressive);
    SourceReadyBounds {
        fps,
        duration_frames: i64::try_from(program.duration_frames).unwrap_or(i64::MAX),
        in_frame: 0,
        out_frame: i64::try_from(program.duration_frames).unwrap_or(i64::MAX),
        field_mode,
    }
}

fn range_from_request(
    request: &BroadcastPlayerOpenRequest,
    source: &SourceRuntime,
) -> Result<CoreFrameRange, String> {
    let start = old_to_core_frame(request.source_ref.in_frame.unwrap_or(FrameNumber(0)))
        .min(source.duration_frames.saturating_sub(1));
    let mut end = old_to_core_frame(
        request
            .source_ref
            .out_frame
            .unwrap_or(request.source_ref.duration_frames),
    )
    .min(source.duration_frames);
    if end <= start {
        end = start.saturating_add(1).min(source.duration_frames);
    }
    CoreFrameRange::new(start, end)
}

fn range_from_source_ref(
    source_ref: &BroadcastHostSourceRef,
    source: &SourceRuntime,
) -> Result<CoreFrameRange, String> {
    let start = old_to_core_frame(source_ref.in_frame.unwrap_or(FrameNumber(0)))
        .min(source.duration_frames.saturating_sub(1));
    let mut end = old_to_core_frame(source_ref.out_frame.unwrap_or(source_ref.duration_frames))
        .min(source.duration_frames);
    if end <= start {
        end = start.saturating_add(1).min(source.duration_frames);
    }
    CoreFrameRange::new(start, end)
}

fn playback_request_from_source(
    request_id: impl Into<String>,
    source: SourceRuntime,
    range: CoreFrameRange,
    initial_frame: CoreFrameNumber,
) -> Result<BroadcastPlaybackRequest, String> {
    BroadcastPlaybackRequest::new(request_id, source)?
        .with_range(range)?
        .with_initial_frame(initial_frame)
}

fn identity_from_request(request: &BroadcastPlayerOpenRequest) -> LoadedSourceIdentity {
    LoadedSourceIdentity {
        project_id: request.source_ref.project_id.clone(),
        virtual_shot_id: request.source_ref.virtual_shot_id.clone(),
        clip_id: request.source_ref.clip_id.clone(),
        source_kind: source_kind(request.has_audio),
    }
}

fn identity_from_program_request(request: &BroadcastProgramOpenRequest) -> LoadedSourceIdentity {
    let has_video = request
        .items
        .iter()
        .flat_map(|item| item.sources.iter())
        .any(|source| source.has_video);
    let has_audio = request
        .items
        .iter()
        .flat_map(|item| item.sources.iter())
        .any(|source| source.has_audio);
    LoadedSourceIdentity {
        project_id: request.project_id.clone(),
        virtual_shot_id: request.program_id.clone(),
        clip_id: request.program_id.clone(),
        source_kind: if has_video && has_audio {
            BroadcastSourceKind::VideoAndAudio
        } else if has_video {
            BroadcastSourceKind::VideoOnly
        } else {
            BroadcastSourceKind::AudioOnly
        },
    }
}

fn same_program_request(
    left: &BroadcastProgramOpenRequest,
    right: &BroadcastProgramOpenRequest,
) -> bool {
    left.program_id == right.program_id
        && left.project_id == right.project_id
        && left.preview_video_resolution == right.preview_video_resolution
        && left.duration_frames == right.duration_frames
        && left.items == right.items
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

fn playlist_input_id(request: &BroadcastProgramOpenRequest) -> String {
    let id = [request.project_id.as_str(), request.program_id.as_str()]
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("::")
        .trim()
        .to_string();
    if id.is_empty() {
        "qnc-playlist-input".into()
    } else {
        format!("qnc-playlist-input::{id}")
    }
}

#[derive(Default)]
struct ProgramSourceIdAllocator {
    chains: BTreeMap<String, ProgramSourceChainState>,
}

#[derive(Clone, Copy)]
struct ProgramSourceChainState {
    chain_index: usize,
    record_out_frame: CoreFrameNumber,
    source_out_frame: CoreFrameNumber,
}

impl ProgramSourceIdAllocator {
    fn allocate(
        &mut self,
        request: &BroadcastProgramOpenRequest,
        item: &BroadcastProgramItem,
        source: &BroadcastProgramSource,
        item_index: usize,
        source_index: usize,
    ) -> String {
        let base_id = source_id_from_program_source(request, source, item_index, source_index);
        let key = program_source_media_track_key(request, source);
        let record_in_frame = old_to_core_frame(item.record_in_frame);
        let record_out_frame = old_to_core_frame(item.record_out_frame).max(record_in_frame + 1);
        let source_in_frame =
            old_to_core_frame(source.source_ref.in_frame.unwrap_or(FrameNumber(0)));
        let source_out_frame = old_to_core_frame(
            source
                .source_ref
                .out_frame
                .unwrap_or(source.source_ref.duration_frames),
        )
        .max(source_in_frame + 1);

        let chain_index = match self.chains.get(&key).copied() {
            Some(previous)
                if previous.record_out_frame == record_in_frame
                    && previous.source_out_frame == source_in_frame =>
            {
                previous.chain_index
            }
            Some(previous) => previous.chain_index.saturating_add(1),
            None => 0,
        };
        self.chains.insert(
            key,
            ProgramSourceChainState {
                chain_index,
                record_out_frame,
                source_out_frame,
            },
        );

        if chain_index == 0 {
            base_id
        } else {
            format!("{base_id}::chain-{chain_index}")
        }
    }
}

fn source_id_from_program_source(
    request: &BroadcastProgramOpenRequest,
    source: &BroadcastProgramSource,
    item_index: usize,
    source_index: usize,
) -> String {
    let id = program_source_media_track_key(request, source);
    if id.is_empty() {
        format!("qnc-playlist-input-{item_index}-{source_index}")
    } else {
        format!("qnc-playlist-input::{id}")
    }
}

fn program_source_media_track_key(
    request: &BroadcastProgramOpenRequest,
    source: &BroadcastProgramSource,
) -> String {
    let media_input = media_input_identity(source.media_input.as_str());
    let timebase = source_timebase_identity(source.source_timebase);
    let tracks = program_source_track_identity(source);
    [
        request.project_id.as_str(),
        source.source_ref.clip_id.as_str(),
        media_input.as_str(),
        timebase.as_str(),
        tracks.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join("::")
    .trim()
    .to_string()
}

fn program_source_track_identity(source: &BroadcastProgramSource) -> String {
    let video = if source.has_video { "v1" } else { "v0" };
    let audio = if source.has_audio { "a1" } else { "a0" };
    let channel = source
        .audio_output_channel
        .map(|channel| format!("ch{channel}"))
        .unwrap_or_else(|| "ch-none".into());
    format!("{video}:{audio}:{channel}")
}

fn media_input_identity(media_input: &str) -> String {
    media_input
        .trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn source_timebase_identity(timebase: BroadcastSourceTimebase) -> String {
    if timebase.is_valid() {
        format!("{}/{}", timebase.fps_num, timebase.fps_den)
    } else {
        "timebase-unknown".into()
    }
}

impl PlayerProgramState {
    #[cfg(test)]
    fn source_at_program_frame(
        &self,
        program_frame: CoreFrameNumber,
    ) -> Option<&PlayerProgramSource> {
        self.source_at_program_frame_matching(program_frame, |_| true)
    }

    fn frame_result(&self, program_frame: CoreFrameNumber) -> ProgramFrameResult {
        ProgramFrameResult {
            video: self.video_result_at_program_frame(program_frame).cloned(),
            audio_buses: self.audio_buses_at_program_frame(program_frame),
        }
    }

    fn video_result_at_program_frame(
        &self,
        program_frame: CoreFrameNumber,
    ) -> Option<&PlayerProgramSource> {
        self.item_at_program_frame(program_frame)?
            .sources
            .iter()
            .rev()
            .find(|source| source.has_video())
    }

    #[cfg(test)]
    fn audio_source_at_program_frame(
        &self,
        program_frame: CoreFrameNumber,
        output_channel: u8,
    ) -> Option<&PlayerProgramSource> {
        self.item_at_program_frame(program_frame)?
            .sources
            .iter()
            .find(|source| {
                source.has_audio() && source.spec.audio_output_channel == Some(output_channel)
            })
    }

    fn audio_buses_at_program_frame(&self, program_frame: CoreFrameNumber) -> Vec<ProgramAudioBus> {
        let mut buses = self
            .item_at_program_frame(program_frame)
            .map(|item| {
                item.sources
                    .iter()
                    .filter_map(|source| {
                        let output_channel = source.spec.audio_output_channel?;
                        source.has_audio().then(|| ProgramAudioBus {
                            output_channel,
                            source: source.clone(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        buses.sort_by_key(|bus| bus.output_channel);
        buses
    }

    #[cfg(test)]
    fn source_at_program_frame_matching(
        &self,
        program_frame: CoreFrameNumber,
        predicate: impl Fn(&PlayerProgramSource) -> bool,
    ) -> Option<&PlayerProgramSource> {
        let item = self.item_at_program_frame(program_frame)?;
        item.sources.iter().find(|source| predicate(source))
    }

    fn item_at_program_frame(&self, program_frame: CoreFrameNumber) -> Option<&PlayerProgramItem> {
        let frame = program_frame.min(self.duration_frames.saturating_sub(1));
        self.items.iter().find(|item| {
            let start = old_to_core_frame(item.spec.record_in_frame);
            let end = old_to_core_frame(item.spec.record_out_frame).max(start + 1);
            frame >= start && frame < end
        })
    }

    fn media_sources_matching(
        &self,
        predicate: fn(&PlayerProgramSource) -> bool,
    ) -> Vec<&PlayerProgramSource> {
        self.items
            .iter()
            .flat_map(|item| item.sources.iter())
            .filter(|source| predicate(source))
            .collect()
    }
}

#[derive(Clone)]
struct PlaylistStartPoint {
    source: PlayerProgramSource,
    program_frame: CoreFrameNumber,
}

fn playlist_upcoming_start_points(
    program: &PlayerProgramState,
    after_frame: CoreFrameNumber,
    lookahead_frames: CoreFrameNumber,
    max_sources: usize,
    has_track: fn(&PlayerProgramSource) -> bool,
) -> Vec<PlaylistStartPoint> {
    let horizon = after_frame.saturating_add(lookahead_frames);
    let mut seen_sources = BTreeSet::new();
    let mut points = Vec::new();
    for source in program.media_sources_matching(has_track) {
        if source.record_in_frame <= after_frame || source.record_in_frame > horizon {
            continue;
        }
        if !seen_sources.insert(source.source.source_id.clone()) {
            continue;
        }
        points.push(PlaylistStartPoint {
            source: source.clone(),
            program_frame: source.record_in_frame,
        });
    }
    points.sort_by_key(|point| (point.program_frame, point.source.source.source_id.clone()));
    points.truncate(max_sources.max(1));
    points
}

impl PlayerProgramSource {
    fn has_video(&self) -> bool {
        self.spec.has_video && self.source.video_format.is_some()
    }

    fn has_audio(&self) -> bool {
        self.spec.has_audio && self.source.audio_format.is_some()
    }

    fn source_frame_for_program_frame(&self, program_frame: CoreFrameNumber) -> CoreFrameNumber {
        let record_in = self.record_in_frame;
        let record_out = self.record_out_frame.max(record_in + 1);
        let source_start = self.range.start_frame;
        let source_end = self.range.end_frame.max(source_start + 1);
        let local_program_frame = program_frame
            .min(record_out.saturating_sub(1))
            .max(record_in)
            .saturating_sub(record_in);
        source_start
            .saturating_add(local_program_frame)
            .min(source_end.saturating_sub(1))
    }
}

fn source_handle(source: &PlayerProgramSource) -> EngineSourceHandle {
    EngineSourceHandle::from_source_runtime(&source.source, None)
}

fn video_frame_byte_len(video_format: &VideoFormat) -> Result<usize, BroadcastEngineError> {
    let width = usize::try_from(video_format.width).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::VideoDecode,
            "video width overflow",
        )
    })?;
    let height = usize::try_from(video_format.height).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::VideoDecode,
            "video height overflow",
        )
    })?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "video frame byte length overflow",
            )
        })
}

fn audio_packet_byte_len_for_frame(
    frame: CoreFrameNumber,
    audio_format: &AudioFormat,
    timebase: CoreTimebase,
) -> Result<usize, BroadcastEngineError> {
    let start_sample = audio_sample_at_frame(frame, audio_format.sample_rate_hz, timebase)?;
    let end_sample = audio_sample_at_frame(
        frame.saturating_add(1),
        audio_format.sample_rate_hz,
        timebase,
    )?;
    let samples = end_sample.saturating_sub(start_sample);
    let bytes = samples
        .checked_mul(u64::from(audio_format.channel_count))
        .and_then(|samples| samples.checked_mul(2))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio packet byte length overflow",
            )
        })?;
    usize::try_from(bytes).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio packet byte length exceeds platform size",
        )
    })
}

fn copy_pcm_s16le_channel(
    src: &AudioFramePacket<Vec<u8>>,
    dst: &mut AudioFramePacket<Vec<u8>>,
    dst_channel: usize,
) -> Result<(), BroadcastEngineError> {
    let src_channels = src
        .audio_format
        .as_ref()
        .map(|format| usize::from(format.channel_count))
        .unwrap_or(0);
    let dst_channels = dst
        .audio_format
        .as_ref()
        .map(|format| usize::from(format.channel_count))
        .unwrap_or(0);
    if src_channels == 0 || dst_channels == 0 || dst_channel >= dst_channels {
        return Ok(());
    }
    if src.payload.len() % 2 != 0 || dst.payload.len() % 2 != 0 {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "pcm_s16le payload has partial sample",
        ));
    }
    let src_frame_stride = src_channels * 2;
    let dst_frame_stride = dst_channels * 2;
    if src.payload.len() % src_frame_stride != 0 || dst.payload.len() % dst_frame_stride != 0 {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "pcm_s16le payload does not align to channel count",
        ));
    }
    let src_frames = src.payload.len() / src_frame_stride;
    let dst_frames = dst.payload.len() / dst_frame_stride;
    if src_frames == 0 || dst_frames == 0 {
        return Ok(());
    }
    if src_frames != dst_frames {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            format!(
                "playlist audio packet sample span mismatch: source={src_frames} destination={dst_frames}"
            ),
        )
        .with_source_id(src.source_id.clone())
        .with_frame(src.start_frame));
    }
    for frame in 0..dst_frames {
        let src_index = (frame * src_channels) * 2;
        let dst_index = (frame * dst_channels + dst_channel) * 2;
        dst.payload[dst_index..dst_index + 2]
            .copy_from_slice(&src.payload[src_index..src_index + 2]);
    }
    Ok(())
}

fn audio_sample_at_frame(
    frame: CoreFrameNumber,
    sample_rate_hz: u32,
    timebase: CoreTimebase,
) -> Result<u64, BroadcastEngineError> {
    let numerator = u128::from(frame)
        .checked_mul(u128::from(sample_rate_hz))
        .and_then(|value| value.checked_mul(u128::from(timebase.frame_rate_den)))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio sample calculation overflow",
            )
        })?;
    let denominator = u128::from(timebase.frame_rate_num);
    if denominator == 0 {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio sample timebase denominator is zero",
        ));
    }
    u64::try_from(numerator / denominator).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio sample value exceeds u64",
        )
    })
}

#[cfg(test)]
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
        Err("fps must be finite and > 0".into())
    }
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

    fn open_request(in_frame: i64, out_frame: i64) -> BroadcastPlayerOpenRequest {
        BroadcastPlayerOpenRequest {
            source_ref: BroadcastHostSourceRef::from_frame_fields(
                "project",
                "part_a",
                "",
                "clip_a",
                Some(FrameNumber(in_frame)),
                Some(FrameNumber(out_frame)),
                FrameNumber(250),
            )
            .unwrap(),
            media_input: "media.mov".into(),
            source_fps: 50.0,
            source_timebase: BroadcastSourceTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            has_audio: true,
            audio_channels: 2,
            start_source_frame: FrameNumber(in_frame),
        }
    }

    fn program_open_request() -> BroadcastProgramOpenRequest {
        BroadcastProgramOpenRequest {
            program_id: "story_a".into(),
            project_id: "project".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            start_program_frame: FrameNumber(0),
            preview_video_resolution: BroadcastProgramPreviewVideoResolution::FastPreview,
            items: Vec::new(),
        }
    }

    #[test]
    fn program_source_probe_cache_reuses_probe_with_requested_source_id() {
        let path = "C:/MEDIA/CLIP.MP4";
        let source = SourceRuntime::new("cached-source", 100, CoreTimebase::new(50, 1).unwrap())
            .unwrap()
            .with_video_format(
                VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709).unwrap(),
            )
            .with_audio_format(AudioFormat::new(48_000, 2).unwrap());
        let mut cache = BTreeMap::from([(
            media_input_identity(path),
            qnc_media_ffmpeg::FfmpegProbeReport {
                source,
                has_video: true,
                has_audio: true,
            },
        )]);
        let toolchain = FfmpegToolchain::new("ffmpeg", "ffprobe").unwrap();

        let report = probe_program_source_runtime(
            "playlist-source".into(),
            &FfmpegMediaInput::from_uri(path),
            &toolchain,
            &mut cache,
        )
        .unwrap();

        assert_eq!(report.source.source_id, "playlist-source");
        assert!(report.has_video);
        assert!(report.has_audio);
    }

    #[test]
    fn app_adapter_uses_contract_frames_for_runtime_range() {
        let source = SourceRuntime::new("src", 100, CoreTimebase::new(50, 1).unwrap()).unwrap();
        let source_ref = BroadcastHostSourceRef::from_frame_fields(
            "project",
            "shot",
            "root",
            "clip",
            Some(FrameNumber(10)),
            Some(FrameNumber(40)),
            FrameNumber(100),
        )
        .unwrap();
        let request = BroadcastPlayerOpenRequest {
            source_ref,
            media_input: "media.mov".into(),
            source_fps: 50.0,
            source_timebase: BroadcastSourceTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            has_audio: false,
            audio_channels: 0,
            start_source_frame: FrameNumber(0),
        };

        let range = range_from_request(&request, &source).unwrap();

        assert_eq!(range.start_frame, 10);
        assert_eq!(range.end_frame, 40);
    }

    #[test]
    fn matches_source_includes_source_range() {
        let mut remote = PlayerRemote::new();
        remote.last_request = Some(open_request(10, 40));

        assert!(remote.matches_source(&open_request(10, 40)));
        assert!(!remote.matches_source(&open_request(11, 40)));
        assert!(!remote.matches_source(&open_request(10, 41)));
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
    fn app_adapter_accepts_probed_integer_rates_without_locked_profile() {
        for (fps, expected) in [
            (30.0, CoreTimebase::new(30, 1).unwrap()),
            (50.0, CoreTimebase::new(50, 1).unwrap()),
            (60.0, CoreTimebase::new(60, 1).unwrap()),
        ] {
            assert_eq!(core_timebase_from_fps(fps).unwrap(), expected);
        }
    }

    #[test]
    fn app_adapter_builds_runtime_playback_request_with_initial_frame() {
        let source = SourceRuntime::new("src", 100, CoreTimebase::new(50, 1).unwrap()).unwrap();
        let range = CoreFrameRange::new(10, 20).unwrap();

        let request = playback_request_from_source("request-1", source, range, 14).unwrap();

        assert_eq!(request.execution_range, range);
        assert_eq!(request.initial_frame, 14);
    }

    #[test]
    fn play_path_takes_pending_still_frame_before_transport_play() {
        let mut remote = PlayerRemote::new();
        remote.pending_still = Some((37, Instant::now() + Duration::from_secs(1)));

        assert_eq!(remote.take_pending_still_for_play(), Some(37));
        assert!(remote.pending_still.is_none());
    }

    #[test]
    fn play_defers_while_program_open_is_pending() {
        let mut remote = PlayerRemote::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        remote.pending_program_open = Some(PendingProgramOpen {
            sequence: 7,
            mode: PendingProgramMode::Open,
            request: program_open_request(),
            rx,
            started_at: Instant::now(),
        });

        remote.play(&egui::Context::default());

        assert!(remote.pending_program_play);
        assert!(!remote.playing);
        assert!(remote.pending_error.is_none());
        assert_eq!(remote.status, "Opening program");
    }

    #[test]
    fn play_program_command_autostarts_after_pending_program_open() {
        let mut remote = PlayerRemote::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        remote.pending_program_open = Some(PendingProgramOpen {
            sequence: 7,
            mode: PendingProgramMode::Prepare,
            request: program_open_request(),
            rx,
            started_at: Instant::now(),
        });

        remote.dispatch(
            PlayerCommand::PlayProgram(program_open_request()),
            &egui::Context::default(),
        );

        assert!(remote.pending_program_play);
        assert_eq!(
            remote
                .pending_program_open
                .as_ref()
                .map(|pending| pending.mode),
            Some(PendingProgramMode::Open)
        );
        assert!(!remote.playing);
        assert!(remote.pending_error.is_none());
        assert_eq!(remote.status, "Opening program");
    }

    #[test]
    fn toggle_play_cancels_pending_program_autoplay() {
        let mut remote = PlayerRemote::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        remote.pending_program_open = Some(PendingProgramOpen {
            sequence: 7,
            mode: PendingProgramMode::Open,
            request: program_open_request(),
            rx,
            started_at: Instant::now(),
        });
        remote.pending_program_play = true;

        remote.dispatch(PlayerCommand::TogglePlay, &egui::Context::default());

        assert!(!remote.pending_program_play);
        assert_eq!(remote.status, "Opening program");
    }

    #[test]
    fn decode_policy_ignores_host_runtime_backend_for_client_player() {
        let runtime = serde_json::json!({
            "hardware_profile": {
                "media_decode": {
                    "recommended_backend": "d3d11va",
                    "video_prefetch_frames": 6,
                    "video_cache_frames": 40,
                    "audio_prefetch_frames": 5,
                    "audio_cache_frames": 80,
                    "video_prefetch_rules": [
                        {
                            "container_contains": "container_a",
                            "codec": "codec_a",
                            "pixel_format_contains": "pixel_format_a",
                            "min_prefetch_frames": 8
                        }
                    ]
                }
            }
        });

        let policy = PlayerDecodePolicy::from_runtime(&runtime);

        assert_eq!(policy.recommended_backend, None);
        assert_eq!(policy.video_prefetch_frames, Some(6));
        assert_eq!(policy.video_cache_frames, Some(40));
        assert_eq!(policy.audio_prefetch_frames, Some(5));
        assert_eq!(policy.audio_cache_frames, Some(80));
        assert_eq!(policy.video_prefetch_rules.len(), 1);
    }

    #[test]
    fn decode_policy_clamps_host_runtime_options_to_client_playback_floor() {
        let runtime = serde_json::json!({
            "hardware_profile": {
                "media_decode": {
                    "video_prefetch_frames": 6,
                    "video_cache_frames": 40,
                    "audio_prefetch_frames": 5,
                    "audio_cache_frames": 80,
                    "video_prefetch_rules": [
                        {
                            "codec": "codec_a",
                            "profile_contains": "profile_a",
                            "min_prefetch_frames": 8
                        }
                    ]
                }
            }
        });
        let policy = PlayerDecodePolicy::from_runtime(&runtime);
        let toolchain = FfmpegToolchain::new("ffmpeg", "ffprobe").unwrap();

        let video_options =
            video_decode_options(toolchain.clone(), FfmpegHardwareDecode::Software, &policy);
        let audio_options = audio_decode_options(toolchain, &policy);

        assert_eq!(
            video_options.video_prefetch_frames,
            SOURCE_VIDEO_PREFETCH_FRAMES
        );
        assert_eq!(video_options.video_cache_frames, SOURCE_VIDEO_CACHE_FRAMES);
        assert_eq!(video_options.decode_policy.video_prefetch_rules().len(), 1);
        assert_eq!(
            audio_options.audio_prefetch_frames,
            SOURCE_AUDIO_PREFETCH_FRAMES
        );
        assert_eq!(audio_options.audio_cache_frames, SOURCE_AUDIO_CACHE_FRAMES);
    }

    #[test]
    fn playlist_decode_options_keep_streaming_read_ahead_floor() {
        let toolchain = FfmpegToolchain::new("ffmpeg", "ffprobe").unwrap();
        let playlist_output =
            VideoFormat::new(640, 360, FieldMode::Progressive, ColorSpace::Rec709).unwrap();
        let low_policy = PlayerDecodePolicy {
            video_prefetch_frames: Some(2),
            video_cache_frames: Some(12),
            audio_prefetch_frames: Some(3),
            audio_cache_frames: Some(24),
            ..PlayerDecodePolicy::default()
        };

        let video_options = playlist_video_decode_options(
            toolchain.clone(),
            FfmpegHardwareDecode::Software,
            &low_policy,
            playlist_output,
        );
        let audio_options = playlist_audio_decode_options(toolchain, &low_policy);

        assert_eq!(
            video_options.video_prefetch_frames,
            PLAYLIST_VIDEO_PREFETCH_FRAMES
        );
        assert_eq!(
            video_options.video_cache_frames,
            PLAYLIST_VIDEO_CACHE_FRAMES
        );
        assert_eq!(
            video_options
                .video_output_format
                .as_ref()
                .map(|format| { (format.width, format.height, format.field_mode) }),
            Some((640, 360, FieldMode::Progressive))
        );
        assert_eq!(
            audio_options.audio_prefetch_frames,
            PLAYLIST_AUDIO_PREFETCH_FRAMES
        );
        assert_eq!(
            audio_options.audio_cache_frames,
            PLAYLIST_AUDIO_CACHE_FRAMES
        );
    }

    #[test]
    fn source_and_playlist_decode_bursts_are_separate() {
        assert_eq!(SOURCE_DECODE_BURST_FRAMES, 6);
        assert_eq!(PLAYLIST_DECODE_BURST_FRAMES, 4);
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
        assert_eq!(policy.video_prefetch_frames, None);
        assert!(policy.video_prefetch_rules.is_empty());
    }

    #[test]
    fn push_source_ready_emits_once_per_open() {
        let mut remote = PlayerRemote::new();
        remote.loaded_program = Some(SourceReadyBounds {
            fps: 50.0,
            duration_frames: 100,
            in_frame: 0,
            out_frame: 100,
            field_mode: FieldMode::Progressive,
        });
        let mut out = Vec::new();
        remote.push_source_ready(&mut out);
        remote.push_source_ready(&mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            PlayerEvent::SourceReady {
                duration_frames: 100,
                field_mode: FieldMode::Progressive,
                ..
            }
        ));
    }

    #[test]
    fn stop_clears_loaded_program_and_source_ready_flag() {
        let mut remote = PlayerRemote::new();
        remote.loaded_program = Some(SourceReadyBounds {
            fps: 50.0,
            duration_frames: 100,
            in_frame: 0,
            out_frame: 100,
            field_mode: FieldMode::Progressive,
        });
        remote.source_ready_sent = true;
        remote.stop();
        assert!(remote.loaded_program.is_none());
        assert!(!remote.source_ready_sent);
        assert!(!remote.has_source());
    }

    #[test]
    fn source_ready_bounds_maps_timebase_and_range() {
        let source = SourceRuntime::new("src", 250, CoreTimebase::new(50, 1).unwrap()).unwrap();
        let range = CoreFrameRange::new(10, 200).unwrap();
        let timebase = source.timebase;
        let fps = f64::from(timebase.frame_rate_num) / f64::from(timebase.frame_rate_den);
        let bounds = SourceReadyBounds {
            fps,
            duration_frames: i64::try_from(source.duration_frames).unwrap_or(i64::MAX),
            in_frame: i64::try_from(range.start_frame).unwrap_or(i64::MAX),
            out_frame: i64::try_from(range.end_frame).unwrap_or(i64::MAX),
            field_mode: FieldMode::Progressive,
        };
        assert!((bounds.fps - 50.0).abs() < 0.001);
        assert_eq!(bounds.duration_frames, 250);
        assert_eq!(bounds.in_frame, 10);
        assert_eq!(bounds.out_frame, 200);
        assert_eq!(bounds.field_mode, FieldMode::Progressive);
    }

    fn program_source(
        source_id: &str,
        record_in: i64,
        record_out: i64,
        source_in: i64,
        source_out: i64,
        clip_id: &str,
    ) -> PlayerProgramSource {
        program_source_with_fps(
            source_id, record_in, record_out, source_in, source_out, clip_id, 50.0,
        )
    }

    fn source_timebase_from_test_fps(source_fps: f64) -> BroadcastSourceTimebase {
        if approx(source_fps, 59.94) {
            BroadcastSourceTimebase {
                fps_num: 60_000,
                fps_den: 1_001,
            }
        } else {
            BroadcastSourceTimebase {
                fps_num: source_fps.round().max(1.0) as u32,
                fps_den: 1,
            }
        }
    }

    fn program_source_with_fps(
        source_id: &str,
        record_in: i64,
        record_out: i64,
        source_in: i64,
        source_out: i64,
        clip_id: &str,
        source_fps: f64,
    ) -> PlayerProgramSource {
        let source_ref = BroadcastHostSourceRef::from_frame_fields(
            "project",
            source_id,
            source_id,
            clip_id,
            Some(FrameNumber(source_in)),
            Some(FrameNumber(source_out)),
            FrameNumber(500),
        )
        .unwrap();
        PlayerProgramSource {
            spec: BroadcastProgramSource {
                source_ref,
                media_input: format!("C:/qnc/proxy/{clip_id}.mp4"),
                source_fps,
                source_timebase: source_timebase_from_test_fps(source_fps),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
                audio_output_channel: Some(PROGRAM_AUDIO_OUTPUT_CH1),
            },
            record_in_frame: record_in as CoreFrameNumber,
            record_out_frame: record_out as CoreFrameNumber,
            source: SourceRuntime::new(source_id, 500, core_timebase_from_fps(source_fps).unwrap())
                .unwrap()
                .with_video_format(
                    VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709)
                        .unwrap(),
                )
                .with_audio_format(AudioFormat::new(48_000, 2).unwrap()),
            range: CoreFrameRange::new(source_in as CoreFrameNumber, source_out as CoreFrameNumber)
                .unwrap(),
        }
    }

    fn audio_ch1_only(mut source: PlayerProgramSource) -> PlayerProgramSource {
        source.spec.has_video = false;
        source.spec.audio_output_channel = Some(PROGRAM_AUDIO_OUTPUT_CH1);
        source.source.video_format = None;
        source
    }

    fn audio_ch2(mut source: PlayerProgramSource) -> PlayerProgramSource {
        source.spec.audio_output_channel = Some(PROGRAM_AUDIO_OUTPUT_CH2);
        source
    }

    fn program_item(
        item_id: &str,
        record_in: i64,
        record_out: i64,
        sources: Vec<PlayerProgramSource>,
    ) -> PlayerProgramItem {
        PlayerProgramItem {
            spec: BroadcastProgramItem {
                item_id: item_id.into(),
                record_in_frame: FrameNumber(record_in),
                record_out_frame: FrameNumber(record_out),
                sources: sources.iter().map(|source| source.spec.clone()).collect(),
            },
            sources,
        }
    }

    fn mono_audio_packet(source_id: &str, samples: &[i16]) -> AudioFramePacket<Vec<u8>> {
        let mut payload = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            payload.extend(sample.to_le_bytes());
        }
        AudioFramePacket {
            source_id: source_id.into(),
            start_frame: 0,
            frame_count: 1,
            audio_format: Some(AudioFormat::new(48_000, 1).unwrap()),
            payload,
        }
    }

    fn stereo_program_packet(sample_frames: usize) -> AudioFramePacket<Vec<u8>> {
        AudioFramePacket {
            source_id: "program".into(),
            start_frame: 0,
            frame_count: 1,
            audio_format: Some(AudioFormat::new(48_000, 2).unwrap()),
            payload: vec![0; sample_frames * 2 * 2],
        }
    }

    fn pcm_channel_samples(packet: &AudioFramePacket<Vec<u8>>, channel: usize) -> Vec<i16> {
        let channels = packet
            .audio_format
            .as_ref()
            .map(|format| usize::from(format.channel_count))
            .unwrap_or(0);
        if channels == 0 || channel >= channels {
            return Vec::new();
        }
        packet
            .payload
            .chunks_exact(channels * 2)
            .map(|frame| {
                let index = channel * 2;
                i16::from_le_bytes([frame[index], frame[index + 1]])
            })
            .collect()
    }

    fn two_take_program_state() -> PlayerProgramState {
        PlayerProgramState {
            timebase: CoreTimebase::new(50, 1).unwrap(),
            duration_frames: 100,
            items: vec![
                program_item(
                    "item_a",
                    0,
                    50,
                    vec![program_source("part_a", 0, 50, 100, 150, "clip_a")],
                ),
                program_item(
                    "item_b",
                    50,
                    100,
                    vec![program_source("part_b", 50, 100, 200, 250, "clip_a")],
                ),
            ],
        }
    }

    #[test]
    fn program_source_maps_record_frame_to_source_frame() {
        let program = two_take_program_state();
        let second = program.video_result_at_program_frame(60).unwrap();

        assert_eq!(
            program.source_at_program_frame(60).map(|source| source
                .spec
                .source_ref
                .virtual_shot_id
                .as_str()),
            Some("part_b")
        );
        assert_eq!(second.source_frame_for_program_frame(60), 210);
    }

    #[test]
    fn flat_playlist_resolves_playable_video_result_and_audio_buses() {
        let program = PlayerProgramState {
            timebase: CoreTimebase::new(50, 1).unwrap(),
            duration_frames: 50,
            items: vec![
                program_item(
                    "item_base_pre",
                    0,
                    10,
                    vec![program_source("part_a_pre", 0, 10, 100, 110, "clip_a")],
                ),
                program_item(
                    "item_cover",
                    10,
                    20,
                    vec![
                        audio_ch1_only(program_source(
                            "part_a_under_cover",
                            10,
                            20,
                            110,
                            120,
                            "clip_a",
                        )),
                        audio_ch2(program_source("cover_a", 10, 20, 40, 50, "clip_b")),
                    ],
                ),
                program_item(
                    "item_base_post",
                    20,
                    50,
                    vec![program_source("part_a_post", 20, 50, 120, 150, "clip_a")],
                ),
            ],
        };
        let request = BroadcastProgramOpenRequest {
            program_id: "playlist".into(),
            project_id: "project".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            start_program_frame: FrameNumber(0),
            preview_video_resolution: BroadcastProgramPreviewVideoResolution::FastPreview,
            items: Vec::new(),
        };
        assert_eq!(
            playlist_video_format(&request, &program).map(|format| (format.width, format.height)),
            Some((PLAYLIST_PREVIEW_WIDTH, PLAYLIST_PREVIEW_HEIGHT))
        );

        assert_eq!(
            program
                .video_result_at_program_frame(12)
                .map(|source| source.spec.source_ref.virtual_shot_id.as_str()),
            Some("cover_a")
        );
        let cover_result = program.frame_result(12);
        assert_eq!(
            cover_result.video.as_ref().map(|source| source
                .spec
                .source_ref
                .virtual_shot_id
                .as_str()),
            Some("cover_a")
        );
        assert_eq!(cover_result.audio_buses.len(), 2);
        assert_eq!(
            cover_result.audio_buses[0].output_channel,
            PROGRAM_AUDIO_OUTPUT_CH1
        );
        assert_eq!(
            cover_result.audio_buses[0]
                .source
                .spec
                .source_ref
                .virtual_shot_id
                .as_str(),
            "part_a_under_cover"
        );
        assert_eq!(
            cover_result.audio_buses[1].output_channel,
            PROGRAM_AUDIO_OUTPUT_CH2
        );
        assert_eq!(
            cover_result.audio_buses[1]
                .source
                .spec
                .source_ref
                .virtual_shot_id
                .as_str(),
            "cover_a"
        );
        let cover_video_result = program.video_result_at_program_frame(19).unwrap();
        assert_eq!(
            cover_video_result.spec.source_ref.virtual_shot_id,
            "cover_a"
        );
        assert_eq!(cover_video_result.source_frame_for_program_frame(19), 49);
        assert_eq!(
            program
                .audio_source_at_program_frame(12, PROGRAM_AUDIO_OUTPUT_CH1)
                .map(|source| source.spec.source_ref.virtual_shot_id.as_str()),
            Some("part_a_under_cover")
        );
        assert_eq!(
            program
                .audio_source_at_program_frame(12, PROGRAM_AUDIO_OUTPUT_CH2)
                .map(|source| source.spec.source_ref.virtual_shot_id.as_str()),
            Some("cover_a")
        );
        assert_eq!(
            program
                .video_result_at_program_frame(20)
                .map(|source| source.spec.source_ref.virtual_shot_id.as_str()),
            Some("part_a_post")
        );
        let a1_cover_out = program
            .audio_source_at_program_frame(19, PROGRAM_AUDIO_OUTPUT_CH1)
            .unwrap();
        let a2_cover_out = program
            .audio_source_at_program_frame(19, PROGRAM_AUDIO_OUTPUT_CH2)
            .unwrap();
        let a1_after_cover = program
            .audio_source_at_program_frame(20, PROGRAM_AUDIO_OUTPUT_CH1)
            .unwrap();
        assert_eq!(a1_cover_out.source_frame_for_program_frame(19), 119);
        assert_eq!(a2_cover_out.source_frame_for_program_frame(19), 49);
        assert_eq!(a1_after_cover.source_frame_for_program_frame(20), 120);
        assert!(program
            .audio_source_at_program_frame(20, PROGRAM_AUDIO_OUTPUT_CH2)
            .is_none());
        let cover_out_result = program.frame_result(19);
        let after_cover_result = program.frame_result(20);
        assert_eq!(
            cover_out_result
                .video
                .as_ref()
                .unwrap()
                .source_frame_for_program_frame(19),
            49
        );
        assert_eq!(
            after_cover_result
                .video
                .as_ref()
                .unwrap()
                .source_frame_for_program_frame(20),
            120
        );
        assert_eq!(after_cover_result.audio_buses.len(), 1);
        assert_eq!(
            after_cover_result.audio_buses[0].output_channel,
            PROGRAM_AUDIO_OUTPUT_CH1
        );
    }

    #[test]
    fn pcm_channel_copy_preserves_existing_a1_when_adding_a2() {
        let format = AudioFormat::new(48_000, 2).unwrap();
        let mut dst = AudioFramePacket {
            source_id: "program".into(),
            start_frame: 0,
            frame_count: 1,
            audio_format: Some(format.clone()),
            payload: vec![1, 0, 0, 0, 2, 0, 0, 0],
        };
        let a2 = AudioFramePacket {
            source_id: "cover".into(),
            start_frame: 0,
            frame_count: 1,
            audio_format: Some(format),
            payload: vec![9, 0, 7, 0, 8, 0, 6, 0],
        };

        copy_pcm_s16le_channel(&a2, &mut dst, 1).unwrap();

        assert_eq!(dst.payload, vec![1, 0, 9, 0, 2, 0, 8, 0]);
    }

    #[test]
    fn pcm_channel_copy_rejects_source_destination_sample_span_mismatch() {
        let src = mono_audio_packet("source", &[10, 20]);
        let mut dst = stereo_program_packet(4);

        let err = copy_pcm_s16le_channel(&src, &mut dst, 1).unwrap_err();

        assert!(err.message.contains("sample span mismatch"));
        assert_eq!(pcm_channel_samples(&dst, 0), vec![0, 0, 0, 0]);
        assert_eq!(pcm_channel_samples(&dst, 1), vec![0, 0, 0, 0]);
    }

    #[test]
    fn program_source_keeps_mixed_fps_source_frames_contiguous() {
        let fast_source = program_source_with_fps("part_fast", 0, 50, 100, 150, "clip_fast", 59.94);
        assert_eq!(fast_source.source_frame_for_program_frame(0), 100);
        assert_eq!(fast_source.source_frame_for_program_frame(1), 101);
        assert_eq!(fast_source.source_frame_for_program_frame(49), 149);

        let alternate_source =
            program_source_with_fps("part_alt", 0, 50, 100, 150, "clip_alt", 50.0);
        assert_eq!(alternate_source.source_frame_for_program_frame(0), 100);
        assert_eq!(alternate_source.source_frame_for_program_frame(1), 101);
        assert_eq!(alternate_source.source_frame_for_program_frame(49), 149);
    }

    #[test]
    fn program_source_id_keeps_contiguous_base_across_overlay() {
        let request = BroadcastProgramOpenRequest {
            program_id: "playlist".into(),
            project_id: "project".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            start_program_frame: FrameNumber(0),
            preview_video_resolution: BroadcastProgramPreviewVideoResolution::FastPreview,
            items: Vec::new(),
        };
        let make_source = |shot_id: &str,
                           virtual_shot_id: &str,
                           media_input: &str,
                           in_frame: i64,
                           out_frame: i64|
         -> BroadcastProgramSource {
            BroadcastProgramSource {
                source_ref: BroadcastHostSourceRef::from_frame_fields(
                    "project",
                    shot_id,
                    virtual_shot_id,
                    "clip_a",
                    Some(FrameNumber(in_frame)),
                    Some(FrameNumber(out_frame)),
                    FrameNumber(500),
                )
                .unwrap(),
                media_input: media_input.into(),
                source_fps: 50.0,
                source_timebase: BroadcastSourceTimebase {
                    fps_num: 50,
                    fps_den: 1,
                },
                has_video: true,
                has_audio: true,
                audio_channels: 2,
                audio_output_channel: Some(PROGRAM_AUDIO_OUTPUT_CH1),
            }
        };
        let first = make_source("segment_a", "virtual_a", "C:/QNC/proxy/clip_a.mp4", 0, 50);
        let second = make_source(
            "segment_b",
            "virtual_b",
            "c:\\qnc\\proxy\\clip_a.mp4",
            50,
            100,
        );
        let third = make_source(
            "segment_c",
            "virtual_c",
            "C:/qnc/proxy/clip_b.mp4",
            100,
            150,
        );
        let mut fourth = make_source(
            "segment_d",
            "virtual_d",
            "C:/QNC/proxy/clip_a.mp4",
            150,
            200,
        );
        fourth.audio_output_channel = Some(PROGRAM_AUDIO_OUTPUT_CH2);
        let fifth = make_source(
            "segment_e",
            "virtual_e",
            "C:/QNC/proxy/clip_a.mp4",
            300,
            350,
        );
        let first_item = BroadcastProgramItem {
            item_id: "item_a".into(),
            record_in_frame: FrameNumber(0),
            record_out_frame: FrameNumber(50),
            sources: vec![first.clone()],
        };
        let second_item = BroadcastProgramItem {
            item_id: "item_b".into(),
            record_in_frame: FrameNumber(50),
            record_out_frame: FrameNumber(100),
            sources: vec![second.clone()],
        };
        let third_item = BroadcastProgramItem {
            item_id: "item_c".into(),
            record_in_frame: FrameNumber(100),
            record_out_frame: FrameNumber(150),
            sources: vec![third.clone()],
        };
        let fourth_item = BroadcastProgramItem {
            item_id: "item_d".into(),
            record_in_frame: FrameNumber(150),
            record_out_frame: FrameNumber(200),
            sources: vec![fourth.clone()],
        };
        let fifth_item = BroadcastProgramItem {
            item_id: "item_e".into(),
            record_in_frame: FrameNumber(200),
            record_out_frame: FrameNumber(250),
            sources: vec![fifth.clone()],
        };

        let mut allocator = ProgramSourceIdAllocator::default();
        let first_id = allocator.allocate(&request, &first_item, &first, 0, 0);
        let second_id = allocator.allocate(&request, &second_item, &second, 1, 0);
        let third_id = allocator.allocate(&request, &third_item, &third, 2, 0);
        let fourth_id = allocator.allocate(&request, &fourth_item, &fourth, 3, 0);
        let fifth_id = allocator.allocate(&request, &fifth_item, &fifth, 4, 0);

        assert_eq!(
            source_id_from_program_source(&request, &first, 0, 0),
            source_id_from_program_source(&request, &first, 0, 1)
        );
        assert_eq!(first_id, second_id);
        assert_ne!(first_id, third_id);
        assert_ne!(first_id, fourth_id);
        assert_ne!(first_id, fifth_id);

        let base_under_cover = make_source(
            "segment_a_under_cover",
            "virtual_a",
            "C:/QNC/proxy/clip_a.mp4",
            50,
            60,
        );
        let base_after_cover = make_source(
            "segment_a_post",
            "virtual_a",
            "C:/QNC/proxy/clip_a.mp4",
            60,
            100,
        );
        let base_under_cover_item = BroadcastProgramItem {
            item_id: "item_base_under_cover".into(),
            record_in_frame: FrameNumber(50),
            record_out_frame: FrameNumber(60),
            sources: vec![base_under_cover.clone()],
        };
        let base_after_cover_item = BroadcastProgramItem {
            item_id: "item_base_after_cover".into(),
            record_in_frame: FrameNumber(60),
            record_out_frame: FrameNumber(100),
            sources: vec![base_after_cover.clone()],
        };
        let mut overlay_allocator = ProgramSourceIdAllocator::default();
        let base_before_id = overlay_allocator.allocate(&request, &first_item, &first, 0, 0);
        let base_under_cover_id =
            overlay_allocator.allocate(&request, &base_under_cover_item, &base_under_cover, 1, 0);
        let base_after_cover_id =
            overlay_allocator.allocate(&request, &base_after_cover_item, &base_after_cover, 2, 0);
        assert_eq!(base_before_id, base_under_cover_id);
        assert_eq!(base_before_id, base_after_cover_id);
    }

    #[test]
    fn playlist_upcoming_start_points_are_lookahead_bounded() {
        let mut program = two_take_program_state();
        program.items.push(program_item(
            "item_c",
            100,
            150,
            vec![program_source("part_c", 100, 150, 20, 70, "clip_b")],
        ));

        let too_early =
            playlist_upcoming_start_points(&program, 60, 39, 2, PlayerProgramSource::has_video);
        assert!(too_early.is_empty());

        let points =
            playlist_upcoming_start_points(&program, 60, 40, 2, PlayerProgramSource::has_video);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].source.spec.source_ref.virtual_shot_id, "part_c");
        assert_eq!(points[0].program_frame, 100);
    }

    #[test]
    fn program_timebase_uses_source_probe_not_timeline_fps() {
        let items = vec![program_item(
            "item_a",
            0,
            60,
            vec![program_source_with_fps(
                "part_fast",
                0,
                60,
                100,
                160,
                "clip_fast",
                59.94,
            )],
        )];

        assert_eq!(
            program_timebase_from_items(&items),
            Some(CoreTimebase::new(60_000, 1_001).unwrap())
        );
    }

    #[test]
    fn playlist_input_source_uses_program_duration_and_formats() {
        let request = BroadcastProgramOpenRequest {
            program_id: "story_a".into(),
            project_id: "project".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            start_program_frame: FrameNumber(0),
            preview_video_resolution: BroadcastProgramPreviewVideoResolution::FastPreview,
            items: Vec::new(),
        };
        let program = two_take_program_state();

        let source = playlist_input_source(&request, &program).unwrap();

        assert_eq!(source.source_id, "qnc-playlist-input::project::story_a");
        assert_eq!(source.duration_frames, 100);
        assert_eq!(source.timebase, CoreTimebase::new(50, 1).unwrap());
        assert!(source.video_format.is_some());
        assert_eq!(source.audio_format.as_ref().unwrap().channel_count, 2);
    }

    #[test]
    fn playlist_input_source_can_use_source_raster_for_hires_preview() {
        let request = BroadcastProgramOpenRequest {
            program_id: "hires-preview:story_a".into(),
            project_id: "project".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            start_program_frame: FrameNumber(0),
            preview_video_resolution: BroadcastProgramPreviewVideoResolution::SourceRaster,
            items: Vec::new(),
        };
        let program = two_take_program_state();

        let source = playlist_input_source(&request, &program).unwrap();

        assert_eq!(
            source
                .video_format
                .as_ref()
                .map(|format| (format.width, format.height)),
            Some((1920, 1080))
        );
    }

    #[test]
    fn program_boundary_reports_playlist_end_only() {
        let mut remote = PlayerRemote::new();
        remote.program = Some(two_take_program_state());
        remote.playing = true;

        let events = remote.apply_protocol_events(vec![
            BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame: 100 },
        ]);

        assert!(matches!(
            events.as_slice(),
            [PlayerEvent::BoundaryReached {
                source_frame: FrameNumber(100)
            }]
        ));
        assert_eq!(remote.source_frame, FrameNumber(100));
        assert!(!remote.playing);
        assert_eq!(remote.status, "Kraj programa");
    }
}
