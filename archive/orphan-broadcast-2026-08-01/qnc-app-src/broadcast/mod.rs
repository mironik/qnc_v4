#![allow(dead_code)]
#![allow(unused_imports)]

pub mod asset;
pub mod audio;
pub mod av_sync;
pub mod backend;
pub mod broadcast_quality;
pub mod celluloid;
pub mod clock;
pub mod compositor;
pub mod diagnostics;
pub mod engine;
pub mod ffmpeg;
pub mod graph;
pub mod host_source;
pub mod hwaccel;
pub mod layers;
pub mod live_ffmpeg_integ;
pub mod mixer;
pub mod payload;
pub mod player;
pub mod player_log;
pub mod playout;
pub mod present;
pub mod presentation;
pub mod probe;
pub mod program;
pub mod render;
pub mod runtime;
pub mod schedule;
pub mod session;
pub mod source;
pub mod source_playback;
pub mod sync;
pub mod timebase;
pub mod timeline;
pub mod transport;
pub mod video;
pub mod window;
pub mod worker;

pub use asset::{
    BroadcastMediaAsset, BroadcastMediaAssetSeed, BroadcastMediaKind, BroadcastMediaLocation,
    BroadcastMediaResolver, BroadcastResolvedDecodeBackend, InMemoryMediaResolver,
    MediaRequirement, MediaResolveError, NullResolvedBroadcastBackend, ResolvedAudioDecodeRequest,
    ResolvedAudioSource, ResolvedFrameDecodePlan, ResolvedVideoDecodeRequest, ResolvedVideoSource,
};
pub use audio::{AudioFrameQueue, QueuedAudioFrame};
pub use av_sync::{
    audio_emit_range, av_queues_in_lockstep, carrier_decode_exhausted, clock_ahead_of_decode,
    decode_budget, decode_error_action, decode_frontier_for_stall, decode_newest_for_refill,
    play_start_ready, should_enter_underrun_stall, should_resume_after_stall,
    simulate_contiguous_emits, simulate_coupled_play_loop, simulate_engine_lifecycle,
    simulate_hold_policy_emits, simulate_stall_resume_cycles, soft_eos_tick_progress, AvSyncError,
    CoupledPlayTrace, DecodeErrorAction, LifecycleTrace, StallResumeTrace,
    MAX_DECODE_RECOVER_STREAK, PLAY_START_MIN_BUFFER_FRAMES, SOFT_EOS_TICKS,
    STALL_RESUME_BUFFER_FRAMES,
};
pub use backend::{
    AudioDecodeRequest, BroadcastDecodeBackend, DecodeEffectEvent, DecodeError, DecodeMarkerEvent,
    DecodedAudioBus, DecodedProgramFrame, DecodedVideoLayer, FrameDecodePlan, NullBroadcastBackend,
    VideoDecodeRequest,
};
pub use celluloid::CelluloidTrack;
pub use clock::{BroadcastMasterClock, ClockReference, ClockState};
pub use compositor::{
    VideoCompositeLayer, VideoCompositePlan, VideoCompositePlanError, VideoCompositeRole,
};
pub use diagnostics::{BroadcastPlayoutDiagnostics, PlayoutProblem, QueueSnapshot};
pub use engine::{probe_open_assets, BroadcastEngine, EngineEvent, EngineOpenRequest};
pub use ffmpeg::{FfmpegBroadcastBackend, FfmpegBroadcastConfig, FfmpegCommandSpec};
pub use graph::BroadcastProgramGraph;
pub use host_source::{BroadcastHostSourceError, BroadcastHostSourceRef};
pub use hwaccel::{configure_player_hwaccel_from_host_profile, DecodeHwaccel};
pub use layers::{
    AudioChannel, AudioMix, EffectKind, MarkerKind, ProgramLayer, ProgramLayerKind,
    ProgramLayerSource, ZPriority, MAX_AUDIO_CHANNELS,
};
pub use mixer::{AudioBusRole, AudioMixInput, AudioMixPlan, AudioMixPlanError};
pub use payload::{
    BroadcastAudioPayload, BroadcastAudioSampleFormat, BroadcastColorSpace, BroadcastPixelFormat,
    BroadcastScanMode, BroadcastVideoPayload, MediaPayloadError,
};
pub use player::{
    BroadcastPlaybackPump, BroadcastPlayerError, BroadcastPlayerErrorKind, BroadcastPlayerTick,
};
pub use playout::{
    BroadcastPlayoutFrame, BroadcastPlayoutSelector, PlayoutReadiness, PlayoutTiming, PlayoutVideo,
};
pub use present::{composite_video_layers, mix_audio_buses, PresentError};
pub use presentation::{
    BroadcastPresentationBatch, BroadcastPresentationPlan, PresentationPlanError,
    PresentationPlanErrorKind, PresentationPlanQueue,
};
pub use probe::{
    parse_ffprobe_json, BroadcastMediaProbeReport, FfprobeCommandSpec, FfprobeMediaProbe,
    MediaProbeError,
};
pub use program::{
    build_layered_program, build_source_timeline, source_spec_for_playback, strip_filmstrip,
    LayeredProgramInput, ProgramMarkerInput, ProgramOverlayInput,
};
pub use render::{
    AudioRenderBus, AudioRenderSource, BroadcastRenderPlan, EffectRenderEvent, MarkerRenderEvent,
    TimelineUnderlay, VideoRenderLayer, VideoRenderRole, VideoRenderSource,
};
pub use runtime::{BroadcastRuntimeDriver, BroadcastRuntimeTick};
pub use schedule::{BroadcastFrameScheduler, ScheduledProgramFrame};
pub use session::BroadcastPlaybackSession;
pub use source::{source_range_from_seconds, BroadcastSourceBuildError, BroadcastSourceRangeSpec};
pub use source_playback::fixture_source;
pub use sync::{sample_index_at_frame_offset, AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};
pub use timebase::{FrameNumber, FrameRange, Timebase, TimebaseParseError};
pub use timeline::{
    AudioLayerSourceSpec, AudioTrackSpec, FilmstripUnderlay, OverlayLayerSpec, TimelineMarkerSpec,
    UniversalTimelineSpec, VideoLayerSourceSpec, VirtualMediaRef,
};
pub use transport::{
    BroadcastTransport, EditBackend, FfmpegEditBackend, FfmpegPreviewBackend, NullPlayoutSink,
    PlayoutSink, PreviewBackend, ProgramHandle, TransportCommand, TransportEvent,
};
pub use video::{QueuedVideoFrame, VideoFrameQueue};
pub use window::{DecodeWindow, FrameDecodeBatch};
pub use worker::{BroadcastDecodeWorker, BroadcastResolvedDecodeWorker, DecodeQueueFill};

#[derive(Debug, Clone)]
pub struct BroadcastPlaybackSource {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub source_range: FrameRange,
    pub source_timebase: Timebase,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u8,
}

/// What media this playback source actually carries.
///
/// Decode / queue / mix expectations must branch on this — not on generic
/// “has a timeline”. Silence program buses are not media audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastSourceKind {
    /// Base video + media audio (source ch0→A1, ch1→A2, …; silence pads unused buses).
    VideoAndAudio,
    /// Picture only — session must not treat PCM as media clock/owner.
    VideoOnly,
    /// Audio-only (OFF/VO style) — no base video queue fill.
    AudioOnly,
}

impl BroadcastPlaybackSource {
    pub fn kind(&self) -> BroadcastSourceKind {
        match (self.has_video, self.has_audio) {
            (true, true) => BroadcastSourceKind::VideoAndAudio,
            (true, false) => BroadcastSourceKind::VideoOnly,
            (false, true) => BroadcastSourceKind::AudioOnly,
            (false, false) => {
                // Invalid for construction; treat as video-only blank for safety.
                BroadcastSourceKind::VideoOnly
            }
        }
    }

    pub fn expects_video_decode(&self) -> bool {
        self.has_video
    }

    /// Media audio decode (not program Silence layers).
    pub fn expects_media_audio_decode(&self) -> bool {
        self.has_audio
    }

    pub fn identity_matches(&self, other: &Self) -> bool {
        self.project_id == other.project_id
            && self.virtual_shot_id == other.virtual_shot_id
            && self.clip_id == other.clip_id
    }

    /// Program bus count A1..A4 when media audio is present (each bus is mono).
    /// At least A1+A2 so the dual-mono monitor always has L/R slots.
    pub fn normalized_audio_channels(&self) -> u8 {
        if !self.has_audio {
            0
        } else {
            self.audio_channels.max(2).min(MAX_AUDIO_CHANNELS)
        }
    }

    /// Native player always exposes at least A1+A2 mono buses (silence if needed).
    pub fn program_audio_buses(&self) -> u8 {
        if !self.has_audio {
            2
        } else {
            self.normalized_audio_channels()
                .max(2)
                .min(MAX_AUDIO_CHANNELS)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastBackendKind {
    Unbound,
    NativePtsDecoder,
}
