use std::collections::BTreeMap;
use std::env;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use qnc_broadcast_player::{
    AudioFormat, AudioFramePacket, AudioOutputAdapter, BroadcastEngineError,
    BroadcastEngineErrorKind, BroadcastEvent, ColorSpace, DecodedVideoFrame, EngineFrameRequest,
    EngineSourceHandle, FieldMode, FramePresenter, SourceOpenAdapter, SourceRuntime, Timebase,
    VideoDecodeAdapter, VideoFormat,
};

const RGB24_BYTES_PER_PIXEL: usize = 3;
const PCM_S16LE_BYTES_PER_SAMPLE: usize = 2;
const DEFAULT_VIDEO_PREFETCH_FRAMES: u16 = 1;
const DEFAULT_SYNCHRONOUS_CACHE_FRAMES: u64 = 1;
const DEFAULT_VIDEO_CACHE_FRAMES: usize = 32;
const DEFAULT_VIDEO_CACHE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_AUDIO_PREFETCH_FRAMES: u16 = 1;
const DEFAULT_AUDIO_CACHE_FRAMES: usize = 64;
const DEFAULT_AUDIO_CACHE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_FFMPEG_READ_TIMEOUT: Duration = Duration::from_millis(10_000);
const DEFAULT_FFMPEG_TOOL_TIMEOUT: Duration = Duration::from_millis(10_000);
const DEFAULT_FFPROBE_TIMEOUT: Duration = Duration::from_millis(10_000);
const FFMPEG_CHILD_TERMINATE_WAIT: Duration = Duration::from_millis(5);
const FFMPEG_CHILD_TERMINATE_POLL: Duration = Duration::from_millis(1);
const FFMPEG_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const FFMPEG_INPUT_SEEK_PREROLL_FRAMES: u64 = 25;
const DEFAULT_FFMPEG_BINARY: &str = "ffmpeg";
const DEFAULT_FFPROBE_BINARY: &str = "ffprobe";
const QNC_FFMPEG_ENV: &str = "QNC_FFMPEG";
const QNC_FFPROBE_ENV: &str = "QNC_FFPROBE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FfmpegStreamCacheConfig {
    max_cache_frames: usize,
    max_cache_bytes: usize,
    read_timeout: Duration,
}

#[derive(Clone, Copy, Debug)]
struct FfmpegVideoCacheRequest<'a> {
    toolchain: &'a FfmpegToolchain,
    hardware_decode: &'a FfmpegHardwareDecode,
    timebase: Timebase,
    cache_config: FfmpegStreamCacheConfig,
}

#[derive(Clone, Copy, Debug)]
struct FfmpegVideoStreamRequest<'a> {
    toolchain: &'a FfmpegToolchain,
    hardware_decode: &'a FfmpegHardwareDecode,
    timebase: Timebase,
    frame_byte_len: usize,
    read_ahead_frames: usize,
    read_timeout: Duration,
}

struct FfmpegBoundedCommandRequest<'a> {
    kind: BroadcastEngineErrorKind,
    process_label: &'a str,
    command_label: &'a str,
    timeout: Duration,
    reaper_name: &'a str,
    source_id: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FfmpegFrameSeek {
    input_seek_frame: u64,
    relative_start_frame: u64,
    input_seek_position: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegToolchain {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl Default for FfmpegToolchain {
    fn default() -> Self {
        Self::from_env()
    }
}

impl FfmpegToolchain {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Result<Self, String> {
        let toolchain = Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        };
        toolchain.validate()?;
        Ok(toolchain)
    }

    pub fn from_env() -> Self {
        Self {
            ffmpeg: tool_path_from_env(QNC_FFMPEG_ENV, DEFAULT_FFMPEG_BINARY),
            ffprobe: tool_path_from_env(QNC_FFPROBE_ENV, DEFAULT_FFPROBE_BINARY),
        }
    }

    pub fn ffmpeg(&self) -> &Path {
        &self.ffmpeg
    }

    pub fn ffprobe(&self) -> &Path {
        &self.ffprobe
    }

    fn validate(&self) -> Result<(), String> {
        validate_tool_path(&self.ffmpeg, "ffmpeg")?;
        validate_tool_path(&self.ffprobe, "ffprobe")
    }
}

fn tool_path_from_env(env_key: &str, default_binary: &str) -> PathBuf {
    env::var_os(env_key)
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_binary))
}

fn validate_tool_path(path: &Path, label: &str) -> Result<(), String> {
    if path.as_os_str().to_string_lossy().trim().is_empty() {
        return Err(format!("{label} path must not be blank"));
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct FfmpegSourceRegistry {
    source_paths: BTreeMap<String, PathBuf>,
}

impl FfmpegSourceRegistry {
    pub fn new(source_paths: BTreeMap<String, PathBuf>) -> Self {
        Self { source_paths }
    }

    pub fn single(source_id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::new(BTreeMap::from([(source_id.into(), path.into())]))
    }

    fn source_path(&self, source_id: &str) -> Result<&Path, BroadcastEngineError> {
        self.source_paths
            .get(source_id)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::SourceOpen,
                    format!("source path not registered: {source_id}"),
                )
                .with_source_id(source_id.to_string())
            })
    }
}

#[derive(Clone, Debug)]
pub struct FfmpegSourceOpen {
    registry: FfmpegSourceRegistry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegProbeReport {
    pub source: SourceRuntime,
    pub has_video: bool,
    pub has_audio: bool,
}

pub fn probe_source_runtime(
    path: &Path,
    source_id: impl Into<String>,
    timebase_hint: Option<Timebase>,
) -> Result<FfmpegProbeReport, BroadcastEngineError> {
    let toolchain = FfmpegToolchain::default();
    probe_source_runtime_with_toolchain(path, source_id, timebase_hint, &toolchain)
}

pub fn probe_source_runtime_with_toolchain(
    path: &Path,
    source_id: impl Into<String>,
    timebase_hint: Option<Timebase>,
    toolchain: &FfmpegToolchain,
) -> Result<FfmpegProbeReport, BroadcastEngineError> {
    let source_id = source_id.into();
    let video_probe = probe_video_runtime(path, &source_id, toolchain)?;
    let audio_probe = probe_audio_runtime(path, &source_id, toolchain)?;
    if video_probe.is_none() && audio_probe.is_none() {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "source has no playable FFmpeg video or audio stream",
        )
        .with_source_id(source_id));
    }

    let timebase = timebase_hint
        .or_else(|| video_probe.as_ref().map(|probe| probe.timebase))
        .unwrap_or(Timebase::new(25, 1).map_err(contract_error)?);
    let duration_frames = if let Some(video_probe) = &video_probe {
        video_probe.duration_frames
    } else {
        let audio_probe = audio_probe.as_ref().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "audio probe is missing",
            )
            .with_source_id(source_id.clone())
        })?;
        duration_frames_from_audio_samples(
            audio_probe.duration_samples,
            audio_probe.audio_format.sample_rate_hz,
            timebase,
        )?
    };

    let mut source =
        SourceRuntime::new(source_id, duration_frames, timebase).map_err(contract_error)?;
    if let Some(video_probe) = video_probe.clone() {
        source = source.with_video_format(video_probe.video_format);
    }
    if let Some(audio_probe) = audio_probe.clone() {
        source = source.with_audio_format(audio_probe.audio_format);
    }
    Ok(FfmpegProbeReport {
        source,
        has_video: video_probe.is_some(),
        has_audio: audio_probe.is_some(),
    })
}

impl FfmpegSourceOpen {
    pub fn new(registry: FfmpegSourceRegistry) -> Self {
        Self { registry }
    }
}

impl SourceOpenAdapter for FfmpegSourceOpen {
    fn open_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<EngineSourceHandle, BroadcastEngineError> {
        self.registry.source_path(&source.source_id)?;

        if source.video_format.is_none() && source.audio_format.is_none() {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                "source has no playable tracks",
            )
            .with_source_id(source.source_id.clone()));
        }
        Ok(EngineSourceHandle::from_source_runtime(
            source,
            source_revision,
        ))
    }

    fn close_source(&mut self, _source_id: &str) -> Result<(), BroadcastEngineError> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct FfmpegVideoDecode {
    registry: FfmpegSourceRegistry,
    options: FfmpegDecodeOptions,
    sessions: BTreeMap<String, FfmpegVideoSession>,
}

impl FfmpegVideoDecode {
    pub fn new(registry: FfmpegSourceRegistry) -> Self {
        Self::with_options(registry, FfmpegDecodeOptions::default())
    }

    pub fn with_options(registry: FfmpegSourceRegistry, options: FfmpegDecodeOptions) -> Self {
        Self {
            registry,
            options,
            sessions: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegDecodeOptions {
    pub toolchain: FfmpegToolchain,
    pub hardware_decode: FfmpegHardwareDecode,
    pub video_prefetch_frames: u16,
    pub video_cache_frames: usize,
    pub video_cache_bytes: usize,
    pub read_timeout: Duration,
}

impl Default for FfmpegDecodeOptions {
    fn default() -> Self {
        Self::software()
    }
}

impl FfmpegDecodeOptions {
    pub fn software() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
            hardware_decode: FfmpegHardwareDecode::Software,
            video_prefetch_frames: DEFAULT_VIDEO_PREFETCH_FRAMES,
            video_cache_frames: DEFAULT_VIDEO_CACHE_FRAMES,
            video_cache_bytes: DEFAULT_VIDEO_CACHE_BYTES,
            read_timeout: DEFAULT_FFMPEG_READ_TIMEOUT,
        }
    }

    pub fn with_toolchain(mut self, toolchain: FfmpegToolchain) -> Self {
        self.toolchain = toolchain;
        self
    }

    pub fn with_hardware_decode(mut self, hardware_decode: FfmpegHardwareDecode) -> Self {
        self.hardware_decode = hardware_decode;
        self
    }

    pub fn with_video_prefetch_frames(mut self, video_prefetch_frames: u16) -> Self {
        self.video_prefetch_frames = video_prefetch_frames.max(1);
        self
    }

    pub fn with_video_cache_frames(mut self, video_cache_frames: usize) -> Self {
        self.video_cache_frames = video_cache_frames.max(1);
        self
    }

    pub fn with_video_cache_bytes(mut self, video_cache_bytes: usize) -> Self {
        self.video_cache_bytes = video_cache_bytes.max(1);
        self
    }

    pub fn with_read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = read_timeout.max(Duration::from_millis(1));
        self
    }
}

#[derive(Debug)]
struct FfmpegVideoSession {
    video_format: VideoFormat,
    duration_frames: u64,
    cache: BTreeMap<u64, FfmpegVideoPayload>,
    stream: Option<FfmpegVideoStream>,
}

impl FfmpegVideoSession {
    fn new(source: &EngineSourceHandle) -> Result<Self, BroadcastEngineError> {
        let video_format = source.video_format.clone().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                "video format is required for FFmpeg video session",
            )
            .with_source_id(source.source_id.clone())
        })?;
        Ok(Self {
            video_format,
            duration_frames: source.duration_frames,
            cache: BTreeMap::new(),
            stream: None,
        })
    }

    fn cached_frame(&self, frame: u64) -> Option<FfmpegVideoPayload> {
        self.cache.get(&frame).cloned()
    }

    fn payload_for_request(&self, request_frame: u64) -> Option<FfmpegVideoPayload> {
        let decode_frame = self.decode_frame_for_request(request_frame);
        let mut payload = self.cached_frame(decode_frame)?;
        payload.frame = request_frame;
        Some(payload)
    }

    fn decode_frame_for_request(&self, request_frame: u64) -> u64 {
        request_frame.min(self.last_decodable_frame())
    }

    fn last_decodable_frame(&self) -> u64 {
        self.duration_frames.saturating_sub(1)
    }

    fn prefetch_end_frame(&self, start_frame: u64, prefetch_frames: u16) -> u64 {
        start_frame
            .saturating_add(u64::from(prefetch_frames.max(1)))
            .saturating_sub(1)
            .min(self.last_decodable_frame())
    }

    fn cache_decoded_frames(
        &mut self,
        decoded_frames: Vec<FfmpegVideoPayload>,
        max_cache_frames: usize,
        max_cache_bytes: usize,
    ) -> Result<(), BroadcastEngineError> {
        let frame_byte_len = self.frame_byte_len()?;
        if decoded_frames.is_empty() {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "decoded video frame payload list is empty",
            ));
        }

        for payload in decoded_frames {
            if payload.bytes.len() != frame_byte_len {
                return Err(BroadcastEngineError::new(
                    BroadcastEngineErrorKind::VideoDecode,
                    "decoded video payload byte size does not match video format",
                )
                .with_frame(payload.frame));
            }
            self.cache.insert(payload.frame, payload);
        }
        self.trim_cache(max_cache_frames, max_cache_bytes);
        Ok(())
    }

    fn cache_streamed_frames(
        &mut self,
        path: &Path,
        start_frame: u64,
        end_frame: u64,
        request: FfmpegVideoCacheRequest<'_>,
    ) -> Result<(), BroadcastEngineError> {
        let frame_byte_len = self.frame_byte_len()?;
        if !self.can_reuse_stream_for(start_frame) {
            let read_ahead_frames = frame_span_len(start_frame, end_frame)?;
            let stream_request = FfmpegVideoStreamRequest {
                toolchain: request.toolchain,
                hardware_decode: request.hardware_decode,
                timebase: request.timebase,
                frame_byte_len,
                read_ahead_frames,
                read_timeout: request.cache_config.read_timeout,
            };
            self.stream = Some(FfmpegVideoStream::spawn(path, start_frame, stream_request)?);
        }
        let stream = self.stream.as_mut().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "ffmpeg video stream is not available",
            )
            .with_frame(start_frame)
        })?;
        let synchronous_end_frame = synchronous_cache_end_frame(start_frame, end_frame);
        let decoded_frames = match stream.read_frames_until(synchronous_end_frame) {
            Ok(decoded_frames) => decoded_frames,
            Err(error) => {
                self.stream = None;
                return Err(error);
            }
        };
        self.cache_decoded_frames(
            decoded_frames,
            request.cache_config.max_cache_frames,
            request.cache_config.max_cache_bytes,
        )?;
        self.cache_ready_streamed_frames(
            end_frame,
            request.cache_config.max_cache_frames,
            request.cache_config.max_cache_bytes,
        )
    }

    fn cache_ready_streamed_frames(
        &mut self,
        end_frame: u64,
        max_cache_frames: usize,
        max_cache_bytes: usize,
    ) -> Result<(), BroadcastEngineError> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        let decoded_frames = match stream.read_ready_frames_until(end_frame) {
            Ok(decoded_frames) => decoded_frames,
            Err(_error) => {
                self.stream = None;
                return Ok(());
            }
        };
        if decoded_frames.is_empty() {
            return Ok(());
        }
        self.cache_decoded_frames(decoded_frames, max_cache_frames, max_cache_bytes)
    }

    fn frame_byte_len(&self) -> Result<usize, BroadcastEngineError> {
        let width = usize::try_from(self.video_format.width).map_err(|_| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "video width is outside platform usize range",
            )
        })?;
        let height = usize::try_from(self.video_format.height).map_err(|_| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "video height is outside platform usize range",
            )
        })?;
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(RGB24_BYTES_PER_PIXEL))
            .ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::VideoDecode,
                    "video frame byte size overflow",
                )
            })
    }

    fn trim_cache(&mut self, max_cache_frames: usize, max_cache_bytes: usize) {
        let max_cache_frames = max_cache_frames.max(1);
        let max_cache_bytes = max_cache_bytes.max(1);
        while self.cache.len() > max_cache_frames || self.cache_byte_len() > max_cache_bytes {
            let Some(oldest_frame) = self.cache.keys().next().copied() else {
                break;
            };
            self.cache.remove(&oldest_frame);
        }
    }

    fn cache_byte_len(&self) -> usize {
        self.cache.values().map(|payload| payload.bytes.len()).sum()
    }

    fn can_reuse_stream_for(&self, start_frame: u64) -> bool {
        stream_next_frame(self.stream.as_ref()) == Some(start_frame)
    }
}

#[derive(Debug)]
struct FfmpegVideoStream {
    child: Option<Child>,
    reader: FfmpegContinuousPipeReader,
    next_frame: u64,
    read_timeout: Duration,
}

impl FfmpegVideoStream {
    fn spawn(
        path: &Path,
        start_frame: u64,
        request: FfmpegVideoStreamRequest<'_>,
    ) -> Result<Self, BroadcastEngineError> {
        let seek = ffmpeg_frame_seek(
            start_frame,
            request.timebase,
            BroadcastEngineErrorKind::VideoDecode,
        )?;
        let filter = format!("select=gte(n\\,{})", seek.relative_start_frame);
        let mut command = Command::new(request.toolchain.ffmpeg());
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        for arg in request.hardware_decode.ffmpeg_input_args() {
            command.arg(arg);
        }
        if seek.input_seek_frame > 0 {
            command.args(["-ss", &seek.input_seek_position]);
        }
        let mut child = command
            .arg("-i")
            .arg(path)
            .args([
                "-map", "0:v:0", "-an", "-vf", &filter, "-vsync", "0", "-f", "rawvideo",
                "-pix_fmt", "rgb24", "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| engine_error(BroadcastEngineErrorKind::VideoDecode, err))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "ffmpeg video stream stdout was not captured",
            )
            .with_frame(start_frame)
        })?;
        Ok(Self {
            child: Some(child),
            reader: FfmpegContinuousPipeReader::new(
                stdout,
                request.frame_byte_len,
                request.read_ahead_frames,
            ),
            next_frame: start_frame,
            read_timeout: request.read_timeout,
        })
    }

    fn read_frames_until(
        &mut self,
        end_frame: u64,
    ) -> Result<Vec<FfmpegVideoPayload>, BroadcastEngineError> {
        let mut frames = Vec::new();
        while self.next_frame <= end_frame {
            let frame = self.next_frame;
            let bytes = self.read_next_frame(frame)?;
            frames.push(FfmpegVideoPayload { frame, bytes });
            self.next_frame = self.next_frame.saturating_add(1);
        }
        Ok(frames)
    }

    fn read_next_frame(&mut self, frame: u64) -> Result<Vec<u8>, BroadcastEngineError> {
        match self.reader.read_next(self.read_timeout) {
            Ok(bytes) => Ok(bytes),
            Err(FfmpegPipeReadFailure::Read(err)) => {
                let message = if let Some(child) = self.child.as_mut() {
                    ffmpeg_stream_read_error_message(
                        child,
                        &err,
                        "ffmpeg video stream ended before requested frame",
                    )
                } else {
                    "ffmpeg video stream ended before requested frame; ffmpeg child is already closed"
                        .to_string()
                };
                Err(
                    BroadcastEngineError::new(BroadcastEngineErrorKind::VideoDecode, message)
                        .with_frame(frame),
                )
            }
            Err(FfmpegPipeReadFailure::Timeout) => {
                let message = if let Some(child) = self.child.as_mut() {
                    ffmpeg_stream_timeout_message(
                        child,
                        self.read_timeout,
                        "ffmpeg video stream read timed out",
                    )
                } else {
                    "ffmpeg video stream read timed out; ffmpeg child is already closed".to_string()
                };
                self.reader.close_and_join();
                Err(
                    BroadcastEngineError::new(BroadcastEngineErrorKind::VideoDecode, message)
                        .with_frame(frame),
                )
            }
            Err(FfmpegPipeReadFailure::Disconnected(message)) => Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                message,
            )
            .with_frame(frame)),
        }
    }

    fn read_ready_frames_until(
        &mut self,
        end_frame: u64,
    ) -> Result<Vec<FfmpegVideoPayload>, BroadcastEngineError> {
        let mut frames = Vec::new();
        while self.next_frame <= end_frame {
            let frame = self.next_frame;
            let Some(bytes) = self.try_read_next_frame(frame)? else {
                break;
            };
            frames.push(FfmpegVideoPayload { frame, bytes });
            self.next_frame = self.next_frame.saturating_add(1);
        }
        Ok(frames)
    }

    fn try_read_next_frame(&mut self, frame: u64) -> Result<Option<Vec<u8>>, BroadcastEngineError> {
        match self.reader.try_read_next() {
            Ok(bytes) => Ok(bytes),
            Err(FfmpegPipeReadFailure::Read(err)) => {
                let message = if let Some(child) = self.child.as_mut() {
                    ffmpeg_stream_read_error_message(
                        child,
                        &err,
                        "ffmpeg video stream ended before requested frame",
                    )
                } else {
                    "ffmpeg video stream ended before requested frame; ffmpeg child is already closed"
                        .to_string()
                };
                Err(
                    BroadcastEngineError::new(BroadcastEngineErrorKind::VideoDecode, message)
                        .with_frame(frame),
                )
            }
            Err(FfmpegPipeReadFailure::Timeout) => Ok(None),
            Err(FfmpegPipeReadFailure::Disconnected(message)) => Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                message,
            )
            .with_frame(frame)),
        }
    }
}

impl Drop for FfmpegVideoStream {
    fn drop(&mut self) {
        self.reader.disconnect();
        if let Some(child) = self.child.take() {
            let _ = terminate_child_bounded(child, "ffmpeg", "qnc-ffmpeg-video-reaper");
        }
        self.reader.join();
    }
}

fn stream_next_frame(stream: Option<&FfmpegVideoStream>) -> Option<u64> {
    stream.map(|stream| stream.next_frame)
}

fn frame_span_len(start_frame: u64, end_frame: u64) -> Result<usize, BroadcastEngineError> {
    prefetch_span_len(
        start_frame,
        end_frame,
        BroadcastEngineErrorKind::VideoDecode,
    )
}

fn audio_frame_span_len(start_frame: u64, end_frame: u64) -> Result<usize, BroadcastEngineError> {
    prefetch_span_len(
        start_frame,
        end_frame,
        BroadcastEngineErrorKind::AudioOutput,
    )
}

fn prefetch_span_len(
    start_frame: u64,
    end_frame: u64,
    error_kind: BroadcastEngineErrorKind,
) -> Result<usize, BroadcastEngineError> {
    if end_frame < start_frame {
        return Err(
            BroadcastEngineError::new(error_kind, "prefetch frame span is invalid")
                .with_frame(start_frame),
        );
    }
    usize::try_from(end_frame.saturating_sub(start_frame).saturating_add(1)).map_err(|_| {
        BroadcastEngineError::new(
            error_kind,
            "prefetch frame span is outside platform usize range",
        )
        .with_frame(start_frame)
    })
}

fn synchronous_cache_end_frame(start_frame: u64, prefetch_end_frame: u64) -> u64 {
    start_frame
        .saturating_add(DEFAULT_SYNCHRONOUS_CACHE_FRAMES)
        .saturating_sub(1)
        .min(prefetch_end_frame)
}

#[derive(Debug)]
struct FfmpegContinuousPipeReader {
    payload_rx: Option<Receiver<Result<Vec<u8>, FfmpegPipeReadError>>>,
    handle: Option<JoinHandle<()>>,
}

impl FfmpegContinuousPipeReader {
    fn new(stdout: ChildStdout, byte_len: usize, queue_capacity: usize) -> Self {
        let (payload_tx, payload_rx) =
            sync_channel::<Result<Vec<u8>, FfmpegPipeReadError>>(queue_capacity.max(1));
        let handle =
            thread::spawn(move || read_ffmpeg_fixed_size_pipe(stdout, byte_len, payload_tx));
        Self {
            payload_rx: Some(payload_rx),
            handle: Some(handle),
        }
    }

    fn read_next(&mut self, timeout: Duration) -> Result<Vec<u8>, FfmpegPipeReadFailure> {
        let payload_rx = self.payload_rx.as_ref().ok_or_else(|| {
            FfmpegPipeReadFailure::Disconnected("ffmpeg pipe reader is closed".to_string())
        })?;
        match payload_rx.recv_timeout(timeout) {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(err)) => Err(FfmpegPipeReadFailure::Read(err)),
            Err(RecvTimeoutError::Timeout) => Err(FfmpegPipeReadFailure::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(FfmpegPipeReadFailure::Disconnected(
                "ffmpeg pipe reader stopped before returning payload".to_string(),
            )),
        }
    }

    fn try_read_next(&mut self) -> Result<Option<Vec<u8>>, FfmpegPipeReadFailure> {
        let payload_rx = self.payload_rx.as_ref().ok_or_else(|| {
            FfmpegPipeReadFailure::Disconnected("ffmpeg pipe reader is closed".to_string())
        })?;
        match payload_rx.try_recv() {
            Ok(Ok(bytes)) => Ok(Some(bytes)),
            Ok(Err(err)) => Err(FfmpegPipeReadFailure::Read(err)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(FfmpegPipeReadFailure::Disconnected(
                "ffmpeg pipe reader stopped before returning payload".to_string(),
            )),
        }
    }

    fn close_and_join(&mut self) {
        self.disconnect();
        self.join();
    }

    fn disconnect(&mut self) {
        self.payload_rx.take();
    }

    fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FfmpegPipeReadError {
    kind: ErrorKind,
    message: String,
}

impl From<std::io::Error> for FfmpegPipeReadError {
    fn from(err: std::io::Error) -> Self {
        Self {
            kind: err.kind(),
            message: err.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FfmpegPipeReadFailure {
    Read(FfmpegPipeReadError),
    Timeout,
    Disconnected(String),
}

fn read_ffmpeg_fixed_size_pipe<R: Read>(
    mut reader: R,
    byte_len: usize,
    payload_tx: SyncSender<Result<Vec<u8>, FfmpegPipeReadError>>,
) {
    loop {
        let mut bytes = vec![0; byte_len];
        let result = reader
            .read_exact(&mut bytes)
            .map(|()| bytes)
            .map_err(Into::into);
        let finished = result.is_err();
        if payload_tx.send(result).is_err() || finished {
            break;
        }
    }
}

fn read_ffmpeg_audio_packets<R: Read>(
    mut reader: R,
    start_frame: u64,
    audio_format: AudioFormat,
    timebase: Timebase,
    payload_tx: SyncSender<Result<FfmpegAudioPayload, BroadcastEngineError>>,
) {
    let mut frame = start_frame;
    loop {
        let byte_len = match audio_packet_byte_len_for_frame(frame, &audio_format, timebase) {
            Ok(byte_len) => byte_len,
            Err(error) => {
                let _ = payload_tx.send(Err(error.with_frame(frame)));
                break;
            }
        };
        let mut bytes = vec![0; byte_len];
        let result = reader
            .read_exact(&mut bytes)
            .map(|()| FfmpegAudioPayload { frame, bytes })
            .map_err(|err| ffmpeg_audio_packet_read_error(frame, err));
        let finished = result.is_err();
        if payload_tx.send(result).is_err() || finished {
            break;
        }
        frame = frame.saturating_add(1);
    }
}

fn ffmpeg_audio_packet_read_error(frame: u64, err: std::io::Error) -> BroadcastEngineError {
    let message = if err.kind() == ErrorKind::UnexpectedEof {
        "ffmpeg audio stream ended before requested frame".to_string()
    } else {
        err.to_string()
    };
    BroadcastEngineError::new(BroadcastEngineErrorKind::AudioOutput, message).with_frame(frame)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FfmpegHardwareDecode {
    #[default]
    Software,
    Auto,
    Backend(String),
}

pub fn available_hardware_decode_backends() -> Result<Vec<String>, BroadcastEngineError> {
    let toolchain = FfmpegToolchain::default();
    available_hardware_decode_backends_with_toolchain(&toolchain)
}

pub fn available_hardware_decode_backends_with_toolchain(
    toolchain: &FfmpegToolchain,
) -> Result<Vec<String>, BroadcastEngineError> {
    let mut command = Command::new(toolchain.ffmpeg());
    command.args(["-hide_banner", "-hwaccels"]);
    let output = run_bounded_process_command(
        command,
        FfmpegBoundedCommandRequest {
            kind: BroadcastEngineErrorKind::SourceOpen,
            process_label: "ffmpeg",
            command_label: "ffmpeg hwaccels",
            timeout: DEFAULT_FFMPEG_TOOL_TIMEOUT,
            reaper_name: "qnc-ffmpeg-tool-reaper",
            source_id: None,
        },
    )?;
    if !output.status.success() {
        return Err(stderr_error(
            BroadcastEngineErrorKind::SourceOpen,
            &output.stderr,
        ));
    }
    Ok(parse_ffmpeg_hwaccels(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

impl FfmpegHardwareDecode {
    pub fn backend(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("hardware decode backend must not be blank".to_string());
        }
        Ok(Self::Backend(name))
    }

    fn ffmpeg_input_args(&self) -> Vec<String> {
        match self {
            FfmpegHardwareDecode::Software => Vec::new(),
            FfmpegHardwareDecode::Auto => {
                vec!["-hwaccel".to_string(), "auto".to_string()]
            }
            FfmpegHardwareDecode::Backend(name) => {
                vec!["-hwaccel".to_string(), name.clone()]
            }
        }
    }
}

fn parse_ffmpeg_hwaccels(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.ends_with(':'))
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegVideoPayload {
    pub frame: u64,
    pub bytes: Vec<u8>,
}

impl FfmpegVideoDecode {
    fn cached_payload(&self, source_id: &str, frame: u64) -> Option<FfmpegVideoPayload> {
        self.sessions
            .get(source_id)
            .and_then(|session| session.payload_for_request(frame))
    }

    fn decode_span(
        &self,
        request: &EngineFrameRequest,
    ) -> Result<(u64, u64), BroadcastEngineError> {
        let session = self.video_session(&request.source_id)?;
        let start_frame = session.decode_frame_for_request(request.frame);
        let end_frame = session.prefetch_end_frame(start_frame, self.options.video_prefetch_frames);
        Ok((start_frame, end_frame))
    }

    fn cache_streamed_payload(
        &mut self,
        request: &EngineFrameRequest,
        start_frame: u64,
        end_frame: u64,
        path: &Path,
    ) -> Result<(), BroadcastEngineError> {
        let max_cache_frames = self.options.video_cache_frames;
        let max_cache_bytes = self.options.video_cache_bytes;
        let hardware_decode = self.options.hardware_decode.clone();
        let toolchain = self.options.toolchain.clone();
        let cache_config = FfmpegStreamCacheConfig {
            max_cache_frames,
            max_cache_bytes,
            read_timeout: self.options.read_timeout,
        };
        let stream_request = FfmpegVideoCacheRequest {
            toolchain: &toolchain,
            hardware_decode: &hardware_decode,
            timebase: request.timebase,
            cache_config,
        };
        self.video_session_mut(&request.source_id)?
            .cache_streamed_frames(path, start_frame, end_frame, stream_request)
            .map_err(|error| error.with_source_id(request.source_id.clone()))
    }

    fn cache_ready_streamed_payload(
        &mut self,
        request: &EngineFrameRequest,
    ) -> Result<(), BroadcastEngineError> {
        let max_cache_frames = self.options.video_cache_frames;
        let max_cache_bytes = self.options.video_cache_bytes;
        let end_frame = {
            let session = self.video_session(&request.source_id)?;
            let start_frame = session.decode_frame_for_request(request.frame);
            session.prefetch_end_frame(start_frame, self.options.video_prefetch_frames)
        };
        self.video_session_mut(&request.source_id)?
            .cache_ready_streamed_frames(end_frame, max_cache_frames, max_cache_bytes)
            .map_err(|error| error.with_source_id(request.source_id.clone()))
    }

    fn video_session(&self, source_id: &str) -> Result<&FfmpegVideoSession, BroadcastEngineError> {
        self.sessions.get(source_id).ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                format!("video session is not prepared: {source_id}"),
            )
            .with_source_id(source_id.to_string())
        })
    }

    fn video_session_mut(
        &mut self,
        source_id: &str,
    ) -> Result<&mut FfmpegVideoSession, BroadcastEngineError> {
        self.sessions.get_mut(source_id).ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                format!("video session is not prepared: {source_id}"),
            )
            .with_source_id(source_id.to_string())
        })
    }
}

impl VideoDecodeAdapter for FfmpegVideoDecode {
    type VideoFrame = FfmpegVideoPayload;

    fn prepare_video(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if source.video_format.is_none() {
            return Ok(Vec::new());
        }
        self.registry.source_path(&source.source_id)?;
        self.sessions
            .insert(source.source_id.clone(), FfmpegVideoSession::new(source)?);
        Ok(Vec::new())
    }

    fn decode_video_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError> {
        self.cache_ready_streamed_payload(&request)?;
        if let Some(payload) = self.cached_payload(&request.source_id, request.frame) {
            return Ok(decoded_video_frame(request, payload));
        }

        let path = self.registry.source_path(&request.source_id)?.to_path_buf();
        let (decode_start_frame, prefetch_end_frame) = self.decode_span(&request)?;
        self.cache_streamed_payload(&request, decode_start_frame, prefetch_end_frame, &path)?;
        let payload = self
            .cached_payload(&request.source_id, request.frame)
            .ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::VideoDecode,
                    "requested frame was not present in decoded prefetch payload",
                )
                .with_source_id(request.source_id.clone())
                .with_frame(request.frame)
            })?;
        Ok(decoded_video_frame(request, payload))
    }
}

#[derive(Debug)]
pub struct FfmpegAudioOutput {
    registry: FfmpegSourceRegistry,
    options: FfmpegAudioDecodeOptions,
    sessions: BTreeMap<String, FfmpegAudioSession>,
}

impl FfmpegAudioOutput {
    pub fn new(registry: FfmpegSourceRegistry) -> Self {
        Self::with_options(registry, FfmpegAudioDecodeOptions::default())
    }

    pub fn with_options(registry: FfmpegSourceRegistry, options: FfmpegAudioDecodeOptions) -> Self {
        Self {
            registry,
            options,
            sessions: BTreeMap::new(),
        }
    }

    fn cached_packet(&self, source_id: &str, frame: u64) -> Option<FfmpegAudioPayload> {
        self.sessions
            .get(source_id)
            .and_then(|session| session.payload_for_request(frame))
    }

    fn decode_span(
        &self,
        request: &EngineFrameRequest,
    ) -> Result<(u64, u64), BroadcastEngineError> {
        let session = self.audio_session(&request.source_id)?;
        let start_frame = session.decode_frame_for_request(request.frame);
        let end_frame = session.prefetch_end_frame(start_frame, self.options.audio_prefetch_frames);
        Ok((start_frame, end_frame))
    }

    fn cache_streamed_packet(
        &mut self,
        request: &EngineFrameRequest,
        start_frame: u64,
        end_frame: u64,
        path: &Path,
    ) -> Result<(), BroadcastEngineError> {
        let max_cache_frames = self.options.audio_cache_frames;
        let max_cache_bytes = self.options.audio_cache_bytes;
        let cache_config = FfmpegStreamCacheConfig {
            max_cache_frames,
            max_cache_bytes,
            read_timeout: self.options.read_timeout,
        };
        let toolchain = self.options.toolchain.clone();
        self.audio_session_mut(&request.source_id)?
            .cache_streamed_packets(
                path,
                &toolchain,
                request.timebase,
                start_frame,
                end_frame,
                cache_config,
            )
            .map_err(|error| error.with_source_id(request.source_id.clone()))
    }

    fn cache_ready_streamed_packet(
        &mut self,
        request: &EngineFrameRequest,
    ) -> Result<(), BroadcastEngineError> {
        let max_cache_frames = self.options.audio_cache_frames;
        let max_cache_bytes = self.options.audio_cache_bytes;
        let end_frame = {
            let session = self.audio_session(&request.source_id)?;
            let start_frame = session.decode_frame_for_request(request.frame);
            session.prefetch_end_frame(start_frame, self.options.audio_prefetch_frames)
        };
        self.audio_session_mut(&request.source_id)?
            .cache_ready_streamed_packets(end_frame, max_cache_frames, max_cache_bytes)
            .map_err(|error| error.with_source_id(request.source_id.clone()))
    }

    fn audio_session(&self, source_id: &str) -> Result<&FfmpegAudioSession, BroadcastEngineError> {
        self.sessions.get(source_id).ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                format!("audio session is not prepared: {source_id}"),
            )
            .with_source_id(source_id.to_string())
        })
    }

    fn audio_session_mut(
        &mut self,
        source_id: &str,
    ) -> Result<&mut FfmpegAudioSession, BroadcastEngineError> {
        self.sessions.get_mut(source_id).ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                format!("audio session is not prepared: {source_id}"),
            )
            .with_source_id(source_id.to_string())
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegAudioDecodeOptions {
    pub toolchain: FfmpegToolchain,
    pub audio_prefetch_frames: u16,
    pub audio_cache_frames: usize,
    pub audio_cache_bytes: usize,
    pub read_timeout: Duration,
}

impl Default for FfmpegAudioDecodeOptions {
    fn default() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
            audio_prefetch_frames: DEFAULT_AUDIO_PREFETCH_FRAMES,
            audio_cache_frames: DEFAULT_AUDIO_CACHE_FRAMES,
            audio_cache_bytes: DEFAULT_AUDIO_CACHE_BYTES,
            read_timeout: DEFAULT_FFMPEG_READ_TIMEOUT,
        }
    }
}

impl FfmpegAudioDecodeOptions {
    pub fn with_toolchain(mut self, toolchain: FfmpegToolchain) -> Self {
        self.toolchain = toolchain;
        self
    }

    pub fn with_audio_prefetch_frames(mut self, audio_prefetch_frames: u16) -> Self {
        self.audio_prefetch_frames = audio_prefetch_frames.max(1);
        self
    }

    pub fn with_audio_cache_frames(mut self, audio_cache_frames: usize) -> Self {
        self.audio_cache_frames = audio_cache_frames.max(1);
        self
    }

    pub fn with_audio_cache_bytes(mut self, audio_cache_bytes: usize) -> Self {
        self.audio_cache_bytes = audio_cache_bytes.max(1);
        self
    }

    pub fn with_read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = read_timeout.max(Duration::from_millis(1));
        self
    }
}

#[derive(Debug)]
struct FfmpegAudioSession {
    audio_format: AudioFormat,
    duration_frames: u64,
    cache: BTreeMap<u64, FfmpegAudioPayload>,
    stream: Option<FfmpegAudioStream>,
}

impl FfmpegAudioSession {
    fn new(source: &EngineSourceHandle) -> Result<Self, BroadcastEngineError> {
        let audio_format = source.audio_format.clone().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                "audio format is required for FFmpeg audio session",
            )
            .with_source_id(source.source_id.clone())
        })?;
        Ok(Self {
            audio_format,
            duration_frames: source.duration_frames,
            cache: BTreeMap::new(),
            stream: None,
        })
    }

    fn payload_for_request(&self, request_frame: u64) -> Option<FfmpegAudioPayload> {
        let decode_frame = self.decode_frame_for_request(request_frame);
        let mut payload = self.cache.get(&decode_frame)?.clone();
        payload.frame = request_frame;
        Some(payload)
    }

    fn decode_frame_for_request(&self, request_frame: u64) -> u64 {
        request_frame.min(self.last_decodable_frame())
    }

    fn last_decodable_frame(&self) -> u64 {
        self.duration_frames.saturating_sub(1)
    }

    fn prefetch_end_frame(&self, start_frame: u64, prefetch_frames: u16) -> u64 {
        start_frame
            .saturating_add(u64::from(prefetch_frames.max(1)))
            .saturating_sub(1)
            .min(self.last_decodable_frame())
    }

    fn cache_streamed_packets(
        &mut self,
        path: &Path,
        toolchain: &FfmpegToolchain,
        timebase: Timebase,
        start_frame: u64,
        end_frame: u64,
        cache_config: FfmpegStreamCacheConfig,
    ) -> Result<(), BroadcastEngineError> {
        if !self.can_reuse_stream_for(start_frame) {
            let read_ahead_packets = audio_frame_span_len(start_frame, end_frame)?;
            self.stream = Some(FfmpegAudioStream::spawn(
                path,
                toolchain,
                &self.audio_format,
                timebase,
                start_frame,
                read_ahead_packets,
                cache_config.read_timeout,
            )?);
        }
        let stream = self.stream.as_mut().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "ffmpeg audio stream is not available",
            )
            .with_frame(start_frame)
        })?;
        let synchronous_end_frame = synchronous_cache_end_frame(start_frame, end_frame);
        let read_result = match stream.read_packets_until(synchronous_end_frame) {
            Ok(read_result) => read_result,
            Err(error) => {
                self.stream = None;
                return Err(error);
            }
        };
        if read_result.stream_ended {
            self.stream = None;
        }
        self.cache_decoded_packets(
            read_result.packets,
            cache_config.max_cache_frames,
            cache_config.max_cache_bytes,
        )?;
        self.cache_ready_streamed_packets(
            end_frame,
            cache_config.max_cache_frames,
            cache_config.max_cache_bytes,
        )
    }

    fn cache_ready_streamed_packets(
        &mut self,
        end_frame: u64,
        max_cache_frames: usize,
        max_cache_bytes: usize,
    ) -> Result<(), BroadcastEngineError> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        let read_result = match stream.read_ready_packets_until(end_frame) {
            Ok(read_result) => read_result,
            Err(_error) => {
                self.stream = None;
                return Ok(());
            }
        };
        if read_result.stream_ended {
            self.stream = None;
        }
        if read_result.packets.is_empty() {
            return Ok(());
        }
        self.cache_decoded_packets(read_result.packets, max_cache_frames, max_cache_bytes)
    }

    fn cache_decoded_packets(
        &mut self,
        decoded_packets: Vec<FfmpegAudioPayload>,
        max_cache_frames: usize,
        max_cache_bytes: usize,
    ) -> Result<(), BroadcastEngineError> {
        if decoded_packets.is_empty() {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "decoded audio packet list is empty",
            ));
        }
        for payload in decoded_packets {
            if payload.bytes.is_empty() {
                return Err(BroadcastEngineError::new(
                    BroadcastEngineErrorKind::AudioOutput,
                    "decoded audio packet is empty",
                )
                .with_frame(payload.frame));
            }
            self.cache.insert(payload.frame, payload);
        }
        self.trim_cache(max_cache_frames, max_cache_bytes);
        Ok(())
    }

    fn trim_cache(&mut self, max_cache_frames: usize, max_cache_bytes: usize) {
        let max_cache_frames = max_cache_frames.max(1);
        let max_cache_bytes = max_cache_bytes.max(1);
        while self.cache.len() > max_cache_frames || self.cache_byte_len() > max_cache_bytes {
            let Some(oldest_frame) = self.cache.keys().next().copied() else {
                break;
            };
            self.cache.remove(&oldest_frame);
        }
    }

    fn cache_byte_len(&self) -> usize {
        self.cache.values().map(|payload| payload.bytes.len()).sum()
    }

    fn can_reuse_stream_for(&self, start_frame: u64) -> bool {
        audio_stream_next_frame(self.stream.as_ref()) == Some(start_frame)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FfmpegAudioPayload {
    frame: u64,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct FfmpegAudioReadResult {
    packets: Vec<FfmpegAudioPayload>,
    stream_ended: bool,
}

#[derive(Debug)]
struct FfmpegAudioPipeReader {
    payload_rx: Option<Receiver<Result<FfmpegAudioPayload, BroadcastEngineError>>>,
    handle: Option<JoinHandle<()>>,
}

impl FfmpegAudioPipeReader {
    fn new(
        stdout: ChildStdout,
        start_frame: u64,
        audio_format: AudioFormat,
        timebase: Timebase,
        queue_capacity: usize,
    ) -> Self {
        let (payload_tx, payload_rx) =
            sync_channel::<Result<FfmpegAudioPayload, BroadcastEngineError>>(queue_capacity.max(1));
        let handle = thread::spawn(move || {
            read_ffmpeg_audio_packets(stdout, start_frame, audio_format, timebase, payload_tx)
        });
        Self {
            payload_rx: Some(payload_rx),
            handle: Some(handle),
        }
    }

    fn read_next(
        &mut self,
        timeout: Duration,
    ) -> Result<FfmpegAudioPayload, FfmpegAudioPipeReadFailure> {
        let payload_rx = self.payload_rx.as_ref().ok_or_else(|| {
            FfmpegAudioPipeReadFailure::Disconnected(
                "ffmpeg audio pipe reader is closed".to_string(),
            )
        })?;
        match payload_rx.recv_timeout(timeout) {
            Ok(Ok(packet)) => Ok(packet),
            Ok(Err(error)) => Err(FfmpegAudioPipeReadFailure::Read(error)),
            Err(RecvTimeoutError::Timeout) => Err(FfmpegAudioPipeReadFailure::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(FfmpegAudioPipeReadFailure::Disconnected(
                "ffmpeg audio pipe reader stopped before returning payload".to_string(),
            )),
        }
    }

    fn try_read_next(&mut self) -> Result<Option<FfmpegAudioPayload>, FfmpegAudioPipeReadFailure> {
        let payload_rx = self.payload_rx.as_ref().ok_or_else(|| {
            FfmpegAudioPipeReadFailure::Disconnected(
                "ffmpeg audio pipe reader is closed".to_string(),
            )
        })?;
        match payload_rx.try_recv() {
            Ok(Ok(packet)) => Ok(Some(packet)),
            Ok(Err(error)) => Err(FfmpegAudioPipeReadFailure::Read(error)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(FfmpegAudioPipeReadFailure::Disconnected(
                "ffmpeg audio pipe reader stopped before returning payload".to_string(),
            )),
        }
    }

    fn close_and_join(&mut self) {
        self.disconnect();
        self.join();
    }

    fn disconnect(&mut self) {
        self.payload_rx.take();
    }

    fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn detach(&mut self) {
        self.handle.take();
    }
}

#[derive(Debug)]
enum FfmpegAudioPipeReadFailure {
    Read(BroadcastEngineError),
    Timeout,
    Disconnected(String),
}

#[derive(Debug)]
struct FfmpegAudioStream {
    child: Option<Child>,
    reader: FfmpegAudioPipeReader,
    next_frame: u64,
    read_timeout: Duration,
}

impl FfmpegAudioStream {
    fn spawn(
        path: &Path,
        toolchain: &FfmpegToolchain,
        audio_format: &AudioFormat,
        timebase: Timebase,
        start_frame: u64,
        read_ahead_packets: usize,
        read_timeout: Duration,
    ) -> Result<Self, BroadcastEngineError> {
        let seek = ffmpeg_frame_seek(start_frame, timebase, BroadcastEngineErrorKind::AudioOutput)?;
        let seek_start_sample =
            audio_sample_at_frame(seek.input_seek_frame, audio_format.sample_rate_hz, timebase)?;
        let start_sample =
            audio_sample_at_frame(start_frame, audio_format.sample_rate_hz, timebase)?;
        let relative_start_sample = start_sample.saturating_sub(seek_start_sample);
        let filter = format!("atrim=start_sample={relative_start_sample}");
        let mut command = Command::new(toolchain.ffmpeg());
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        if seek.input_seek_frame > 0 {
            command.args(["-ss", &seek.input_seek_position]);
        }
        let mut child = command
            .arg("-i")
            .arg(path)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                &filter,
                "-f",
                "s16le",
                "-ac",
                &audio_format.channel_count.to_string(),
                "-ar",
                &audio_format.sample_rate_hz.to_string(),
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| engine_error(BroadcastEngineErrorKind::AudioOutput, err))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "ffmpeg audio stream stdout was not captured",
            )
            .with_frame(start_frame)
        })?;
        Ok(Self {
            child: Some(child),
            reader: FfmpegAudioPipeReader::new(
                stdout,
                start_frame,
                audio_format.clone(),
                timebase,
                read_ahead_packets,
            ),
            next_frame: start_frame,
            read_timeout,
        })
    }

    fn read_packets_until(
        &mut self,
        end_frame: u64,
    ) -> Result<FfmpegAudioReadResult, BroadcastEngineError> {
        let mut packets = Vec::new();
        let mut stream_ended = false;
        while self.next_frame <= end_frame {
            match self.read_next_packet() {
                Ok(packet) => {
                    self.next_frame = packet.frame.saturating_add(1);
                    packets.push(packet);
                }
                Err(error)
                    if error.kind == BroadcastEngineErrorKind::AudioOutput
                        && error
                            .message
                            .starts_with("ffmpeg audio stream ended before requested frame")
                        && !packets.is_empty() =>
                {
                    stream_ended = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(FfmpegAudioReadResult {
            packets,
            stream_ended,
        })
    }

    fn read_ready_packets_until(
        &mut self,
        end_frame: u64,
    ) -> Result<FfmpegAudioReadResult, BroadcastEngineError> {
        let mut packets = Vec::new();
        while self.next_frame <= end_frame {
            match self.try_read_next_packet()? {
                Some(packet) => {
                    self.next_frame = packet.frame.saturating_add(1);
                    packets.push(packet);
                }
                None => break,
            }
        }
        Ok(FfmpegAudioReadResult {
            packets,
            stream_ended: false,
        })
    }

    fn read_next_packet(&mut self) -> Result<FfmpegAudioPayload, BroadcastEngineError> {
        match self.reader.read_next(self.read_timeout) {
            Ok(packet) => Ok(packet),
            Err(FfmpegAudioPipeReadFailure::Read(error)) => Err(error),
            Err(FfmpegAudioPipeReadFailure::Timeout) => {
                let frame = self.next_frame;
                let message = if let Some(child) = self.child.as_mut() {
                    ffmpeg_stream_timeout_message(
                        child,
                        self.read_timeout,
                        "ffmpeg audio stream read timed out",
                    )
                } else {
                    "ffmpeg audio stream read timed out; ffmpeg child is already closed".to_string()
                };
                self.reader.close_and_join();
                Err(
                    BroadcastEngineError::new(BroadcastEngineErrorKind::AudioOutput, message)
                        .with_frame(frame),
                )
            }
            Err(FfmpegAudioPipeReadFailure::Disconnected(message)) => Err(
                BroadcastEngineError::new(BroadcastEngineErrorKind::AudioOutput, message)
                    .with_frame(self.next_frame),
            ),
        }
    }

    fn try_read_next_packet(&mut self) -> Result<Option<FfmpegAudioPayload>, BroadcastEngineError> {
        match self.reader.try_read_next() {
            Ok(packet) => Ok(packet),
            Err(FfmpegAudioPipeReadFailure::Read(error)) => Err(error),
            Err(FfmpegAudioPipeReadFailure::Timeout) => Ok(None),
            Err(FfmpegAudioPipeReadFailure::Disconnected(message)) => Err(
                BroadcastEngineError::new(BroadcastEngineErrorKind::AudioOutput, message)
                    .with_frame(self.next_frame),
            ),
        }
    }
}

impl Drop for FfmpegAudioStream {
    fn drop(&mut self) {
        self.reader.disconnect();
        if let Some(child) = self.child.take() {
            let _ = terminate_child_bounded(child, "ffmpeg", "qnc-ffmpeg-audio-reaper");
        }
        self.reader.detach();
    }
}

fn audio_stream_next_frame(stream: Option<&FfmpegAudioStream>) -> Option<u64> {
    stream.map(|stream| stream.next_frame)
}

fn wait_for_child_exit(
    child: &mut Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if started_at.elapsed() >= timeout => return Ok(None),
            Ok(None) => thread::sleep(FFMPEG_CHILD_TERMINATE_POLL),
            Err(err) => return Err(err),
        }
    }
}

fn terminate_child_bounded(mut child: Child, process_label: &str, reaper_name: &str) -> String {
    match child.try_wait() {
        Ok(Some(status)) => return format!("{process_label} exited with {status}"),
        Ok(None) => {}
        Err(status_err) => {
            let kill_message = match child.kill() {
                Ok(()) => format!("{process_label} kill requested"),
                Err(kill_err) => format!("{process_label} kill failed: {kill_err}"),
            };
            let cleanup = spawn_child_reaper(child, reaper_name);
            return format!(
                "{process_label} status unavailable: {status_err}; {kill_message}; {cleanup}"
            );
        }
    }

    match child.kill() {
        Ok(()) => match wait_for_child_exit(&mut child, FFMPEG_CHILD_TERMINATE_WAIT) {
            Ok(Some(status)) => format!("{process_label} killed with {status}"),
            Ok(None) => {
                let cleanup = spawn_child_reaper(child, reaper_name);
                format!("{process_label} kill requested; {cleanup}")
            }
            Err(wait_err) => {
                let cleanup = spawn_child_reaper(child, reaper_name);
                format!("{process_label} killed; wait failed: {wait_err}; {cleanup}")
            }
        },
        Err(kill_err) => match wait_for_child_exit(&mut child, FFMPEG_CHILD_TERMINATE_WAIT) {
            Ok(Some(status)) => {
                format!("{process_label} kill failed: {kill_err}; exited with {status}")
            }
            Ok(None) => {
                let cleanup = spawn_child_reaper(child, reaper_name);
                format!("{process_label} kill failed: {kill_err}; {cleanup}")
            }
            Err(wait_err) => {
                let cleanup = spawn_child_reaper(child, reaper_name);
                format!(
                    "{process_label} kill failed: {kill_err}; wait failed: {wait_err}; {cleanup}"
                )
            }
        },
    }
}

fn request_child_termination_bounded(child: &mut Child, process_label: &str) -> String {
    match child.try_wait() {
        Ok(Some(status)) => return format!("{process_label} exited with {status}"),
        Ok(None) => {}
        Err(status_err) => {
            let kill_message = match child.kill() {
                Ok(()) => format!("{process_label} kill requested"),
                Err(kill_err) => format!("{process_label} kill failed: {kill_err}"),
            };
            return format!("{process_label} status unavailable: {status_err}; {kill_message}");
        }
    }

    match child.kill() {
        Ok(()) => match wait_for_child_exit(child, FFMPEG_CHILD_TERMINATE_WAIT) {
            Ok(Some(status)) => format!("{process_label} killed with {status}"),
            Ok(None) => format!("{process_label} kill requested; wait deferred"),
            Err(wait_err) => format!("{process_label} killed; wait failed: {wait_err}"),
        },
        Err(kill_err) => match wait_for_child_exit(child, FFMPEG_CHILD_TERMINATE_WAIT) {
            Ok(Some(status)) => {
                format!("{process_label} kill failed: {kill_err}; exited with {status}")
            }
            Ok(None) => format!("{process_label} kill failed: {kill_err}; wait deferred"),
            Err(wait_err) => {
                format!("{process_label} kill failed: {kill_err}; wait failed: {wait_err}")
            }
        },
    }
}

fn spawn_child_reaper(mut child: Child, reaper_name: &str) -> String {
    match thread::Builder::new()
        .name(reaper_name.to_string())
        .spawn(move || {
            let _ = child.wait();
        }) {
        Ok(_handle) => "wait deferred to reaper".to_string(),
        Err(spawn_err) => format!("reaper spawn failed: {spawn_err}"),
    }
}

fn ffmpeg_stream_read_error_message(
    child: &mut Child,
    err: &FfmpegPipeReadError,
    eof_message: &str,
) -> String {
    let base = if err.kind == ErrorKind::UnexpectedEof {
        eof_message.to_string()
    } else {
        err.message.clone()
    };
    match child.try_wait() {
        Ok(Some(status)) => format!("{base}; ffmpeg exited with {status}"),
        Ok(None) => base,
        Err(status_err) => format!("{base}; ffmpeg status unavailable: {status_err}"),
    }
}

fn ffmpeg_stream_timeout_message(
    child: &mut Child,
    read_timeout: Duration,
    context: &str,
) -> String {
    let timeout_ms = read_timeout.as_millis();
    match child.try_wait() {
        Ok(Some(status)) => format!("{context} after {timeout_ms} ms; ffmpeg exited with {status}"),
        Ok(None) => {
            let cleanup = request_child_termination_bounded(child, "ffmpeg");
            format!("{context} after {timeout_ms} ms; {cleanup}")
        }
        Err(status_err) => {
            let cleanup = request_child_termination_bounded(child, "ffmpeg");
            format!(
                "{context} after {timeout_ms} ms; ffmpeg status unavailable: {status_err}; {cleanup}"
            )
        }
    }
}

fn audio_packet_byte_len_for_frame(
    frame: u64,
    audio_format: &AudioFormat,
    timebase: Timebase,
) -> Result<usize, BroadcastEngineError> {
    let (start_sample, end_sample) =
        audio_sample_span_for_frame(frame, audio_format.sample_rate_hz, timebase)?;
    let sample_count = end_sample.checked_sub(start_sample).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio sample span underflow",
        )
    })?;
    let channel_count = usize::from(audio_format.channel_count);
    usize::try_from(sample_count)
        .ok()
        .and_then(|samples| samples.checked_mul(channel_count))
        .and_then(|samples| samples.checked_mul(PCM_S16LE_BYTES_PER_SAMPLE))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio packet byte size overflow",
            )
            .with_frame(frame)
        })
}

fn audio_packet(
    request: EngineFrameRequest,
    audio_format: AudioFormat,
    payload: FfmpegAudioPayload,
) -> AudioFramePacket<Vec<u8>> {
    AudioFramePacket {
        source_id: request.source_id,
        start_frame: request.frame,
        frame_count: 1,
        audio_format: Some(audio_format),
        payload: payload.bytes,
    }
}

impl AudioOutputAdapter for FfmpegAudioOutput {
    type AudioPacket = Vec<u8>;

    fn prepare_audio(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if source.audio_format.is_none() {
            return Ok(Vec::new());
        }
        self.registry.source_path(&source.source_id)?;
        self.sessions
            .insert(source.source_id.clone(), FfmpegAudioSession::new(source)?);
        Ok(Vec::new())
    }

    fn render_audio_for_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
        self.cache_ready_streamed_packet(&request)?;
        if let Some(payload) = self.cached_packet(&request.source_id, request.frame) {
            let audio_format = self.audio_session(&request.source_id)?.audio_format.clone();
            return Ok(audio_packet(request, audio_format, payload));
        }

        let path = self.registry.source_path(&request.source_id)?.to_path_buf();
        let (decode_start_frame, prefetch_end_frame) = self.decode_span(&request)?;
        self.cache_streamed_packet(&request, decode_start_frame, prefetch_end_frame, &path)?;
        let payload = self
            .cached_packet(&request.source_id, request.frame)
            .ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::AudioOutput,
                    "requested audio packet was not present in decoded prefetch payload",
                )
                .with_source_id(request.source_id.clone())
                .with_frame(request.frame)
            })?;
        let audio_format = self.audio_session(&request.source_id)?.audio_format.clone();
        Ok(audio_packet(request, audio_format, payload))
    }

    fn submit_audio_packet(
        &mut self,
        packet: AudioFramePacket<Self::AudioPacket>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if packet.payload.is_empty() {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "decoded audio packet is empty",
            )
            .with_source_id(packet.source_id)
            .with_frame(packet.start_frame));
        }
        Ok(vec![BroadcastEvent::AudioLevelChanged {
            track_id: "ffmpeg-monitor".to_string(),
            peak_dbfs_x100: -1200,
        }])
    }

    fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        for session in self.sessions.values_mut() {
            session.stream = None;
        }
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug, Default)]
pub struct HeadlessFramePresenter;

impl FramePresenter for HeadlessFramePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if frame.payload.bytes.is_empty() || frame.payload.frame != frame.frame {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoPresent,
                "decoded video payload is invalid",
            )
            .with_source_id(frame.source_id)
            .with_frame(frame.frame));
        }
        Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
    }
}

fn decoded_video_frame(
    request: EngineFrameRequest,
    payload: FfmpegVideoPayload,
) -> DecodedVideoFrame<FfmpegVideoPayload> {
    DecodedVideoFrame {
        source_id: request.source_id,
        frame: request.frame,
        video_format: None,
        payload,
    }
}

fn engine_error(kind: BroadcastEngineErrorKind, err: std::io::Error) -> BroadcastEngineError {
    BroadcastEngineError::new(kind, err.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FfmpegVideoProbe {
    video_format: VideoFormat,
    timebase: Timebase,
    duration_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FfmpegAudioProbe {
    audio_format: AudioFormat,
    duration_samples: u64,
}

fn probe_video_runtime(
    path: &Path,
    source_id: &str,
    toolchain: &FfmpegToolchain,
) -> Result<Option<FfmpegVideoProbe>, BroadcastEngineError> {
    let Some(values) = probe_key_values(
        path,
        toolchain,
        "v:0",
        "stream=width,height,field_order,color_space,r_frame_rate,avg_frame_rate,nb_frames,duration_ts,time_base",
        BroadcastEngineErrorKind::SourceOpen,
        source_id,
    )?
    else {
        return Ok(None);
    };

    let width = parse_u32_probe_field(&values, "width", source_id)?;
    let height = parse_u32_probe_field(&values, "height", source_id)?;
    let timebase = parse_probe_timebase(&values, source_id)?;
    let duration_frames = parse_duration_frames(&values, timebase, source_id)?;
    let video_format = VideoFormat::new(
        width,
        height,
        parse_field_mode(values.get("field_order")),
        parse_color_space(values.get("color_space")),
    )
    .map_err(contract_error)?;

    Ok(Some(FfmpegVideoProbe {
        video_format,
        timebase,
        duration_frames,
    }))
}

fn probe_audio_runtime(
    path: &Path,
    source_id: &str,
    toolchain: &FfmpegToolchain,
) -> Result<Option<FfmpegAudioProbe>, BroadcastEngineError> {
    let Some(values) = probe_key_values(
        path,
        toolchain,
        "a:0",
        "stream=sample_rate,channels,duration_ts,time_base",
        BroadcastEngineErrorKind::SourceOpen,
        source_id,
    )?
    else {
        return Ok(None);
    };

    let sample_rate_hz = parse_u32_probe_field(&values, "sample_rate", source_id)?;
    let channel_count = parse_u16_probe_field(&values, "channels", source_id)?;
    let audio_format = AudioFormat::new(sample_rate_hz, channel_count).map_err(contract_error)?;
    let duration_samples = parse_audio_duration_samples(&values, sample_rate_hz, source_id)?;

    Ok(Some(FfmpegAudioProbe {
        audio_format,
        duration_samples,
    }))
}

fn probe_key_values(
    path: &Path,
    toolchain: &FfmpegToolchain,
    stream_selector: &str,
    entries: &str,
    kind: BroadcastEngineErrorKind,
    source_id: &str,
) -> Result<Option<BTreeMap<String, String>>, BroadcastEngineError> {
    let mut command = Command::new(toolchain.ffprobe());
    command.args([
        "-v",
        "error",
        "-select_streams",
        stream_selector,
        "-show_entries",
        entries,
        "-of",
        "default=noprint_wrappers=1:nokey=0",
    ]);
    command.arg(path);
    let output = run_probe_command(command, kind, source_id)?;
    if !output.status.success() {
        return Err(stderr_error(kind, &output.stderr).with_source_id(source_id.to_string()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !value.trim().is_empty() && value.trim() != "N/A" {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    if values.is_empty() {
        Ok(None)
    } else {
        Ok(Some(values))
    }
}

fn run_probe_command(
    command: Command,
    kind: BroadcastEngineErrorKind,
    source_id: &str,
) -> Result<std::process::Output, BroadcastEngineError> {
    run_bounded_process_command(
        command,
        FfmpegBoundedCommandRequest {
            kind,
            process_label: "ffprobe",
            command_label: "ffprobe",
            timeout: DEFAULT_FFPROBE_TIMEOUT,
            reaper_name: "qnc-ffprobe-reaper",
            source_id: Some(source_id),
        },
    )
}

fn run_bounded_process_command(
    mut command: Command,
    request: FfmpegBoundedCommandRequest<'_>,
) -> Result<std::process::Output, BroadcastEngineError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| attach_source_id(engine_error(request.kind, err), request.source_id))?;
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return child.wait_with_output().map_err(|err| {
                    attach_source_id(engine_error(request.kind, err), request.source_id)
                });
            }
            Ok(None) if started_at.elapsed() >= request.timeout => {
                let cleanup =
                    terminate_child_bounded(child, request.process_label, request.reaper_name);
                let timeout_ms = request.timeout.as_millis();
                let message = format!(
                    "{} timed out after {timeout_ms} ms; {cleanup}",
                    request.command_label
                );
                return Err(attach_source_id(
                    BroadcastEngineError::new(request.kind, message),
                    request.source_id,
                ));
            }
            Ok(None) => thread::sleep(FFMPEG_PROCESS_POLL_INTERVAL),
            Err(err) => {
                let cleanup =
                    terminate_child_bounded(child, request.process_label, request.reaper_name);
                let message = format!(
                    "{} status unavailable: {err}; {cleanup}",
                    request.command_label
                );
                return Err(attach_source_id(
                    BroadcastEngineError::new(request.kind, message),
                    request.source_id,
                ));
            }
        }
    }
}

fn attach_source_id(error: BroadcastEngineError, source_id: Option<&str>) -> BroadcastEngineError {
    if let Some(source_id) = source_id {
        error.with_source_id(source_id.to_string())
    } else {
        error
    }
}

fn parse_duration_frames(
    values: &BTreeMap<String, String>,
    timebase: Timebase,
    source_id: &str,
) -> Result<u64, BroadcastEngineError> {
    if let Some(frames) =
        parse_optional_u64_probe_field(values, "nb_frames").filter(|frames| *frames > 0)
    {
        return Ok(frames);
    }
    let duration_ts = parse_optional_u64_probe_field(values, "duration_ts").ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "video probe did not return duration_ts or nb_frames",
        )
        .with_source_id(source_id.to_string())
    })?;
    let stream_time_base = values.get("time_base").ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "video probe did not return time_base",
        )
        .with_source_id(source_id.to_string())
    })?;
    let (time_base_num, time_base_den) = parse_u32_ratio(stream_time_base).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "video probe returned invalid time_base",
        )
        .with_source_id(source_id.to_string())
    })?;
    duration_frames_from_video_ticks(duration_ts, time_base_num, time_base_den, timebase)
        .map_err(|err| err.with_source_id(source_id.to_string()))
}

fn duration_frames_from_video_ticks(
    duration_ts: u64,
    time_base_num: u32,
    time_base_den: u32,
    timebase: Timebase,
) -> Result<u64, BroadcastEngineError> {
    let numerator = u128::from(duration_ts)
        .checked_mul(u128::from(time_base_num))
        .and_then(|value| value.checked_mul(u128::from(timebase.frame_rate_num)))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "video duration frame mapping overflow",
            )
        })?;
    let denominator = u128::from(time_base_den)
        .checked_mul(u128::from(timebase.frame_rate_den))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "video duration frame denominator overflow",
            )
        })?;
    let frames = ceil_div_u128(numerator, denominator).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "video duration frame denominator is zero",
        )
    })?;
    u64::try_from(frames)
        .map_err(|_| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "video duration frame count is outside u64 range",
            )
        })
        .and_then(|frames| {
            if frames == 0 {
                Err(BroadcastEngineError::new(
                    BroadcastEngineErrorKind::SourceOpen,
                    "video duration frame count is zero",
                ))
            } else {
                Ok(frames)
            }
        })
}

fn parse_probe_timebase(
    values: &BTreeMap<String, String>,
    source_id: &str,
) -> Result<Timebase, BroadcastEngineError> {
    let rate = values
        .get("r_frame_rate")
        .or_else(|| values.get("avg_frame_rate"))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "video probe did not return frame rate",
            )
            .with_source_id(source_id.to_string())
        })?;
    let (num, den) = parse_u32_ratio(rate).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "video probe returned invalid frame rate",
        )
        .with_source_id(source_id.to_string())
    })?;
    Timebase::new(num, den).map_err(contract_error)
}

fn parse_audio_duration_samples(
    values: &BTreeMap<String, String>,
    sample_rate_hz: u32,
    source_id: &str,
) -> Result<u64, BroadcastEngineError> {
    let duration_ts = parse_optional_u64_probe_field(values, "duration_ts").ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio probe did not return duration_ts",
        )
        .with_source_id(source_id.to_string())
    })?;
    let time_base = values.get("time_base").ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio probe did not return time_base",
        )
        .with_source_id(source_id.to_string())
    })?;
    let (time_base_num, time_base_den) = parse_u32_ratio(time_base).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio probe returned invalid time_base",
        )
        .with_source_id(source_id.to_string())
    })?;
    let numerator = u128::from(duration_ts)
        .checked_mul(u128::from(time_base_num))
        .and_then(|value| value.checked_mul(u128::from(sample_rate_hz)))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "audio duration sample mapping overflow",
            )
            .with_source_id(source_id.to_string())
        })?;
    let samples = numerator / u128::from(time_base_den);
    u64::try_from(samples).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio duration sample count is outside u64 range",
        )
        .with_source_id(source_id.to_string())
    })
}

fn duration_frames_from_audio_samples(
    sample_count: u64,
    sample_rate_hz: u32,
    timebase: Timebase,
) -> Result<u64, BroadcastEngineError> {
    let numerator = u128::from(sample_count)
        .checked_mul(u128::from(timebase.frame_rate_num))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "audio duration frame mapping overflow",
            )
        })?;
    let denominator = u128::from(sample_rate_hz)
        .checked_mul(u128::from(timebase.frame_rate_den))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                "audio duration frame denominator overflow",
            )
        })?;
    let frames = ceil_div_u128(numerator, denominator).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio duration frame denominator is zero",
        )
    })?;
    u64::try_from(frames).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::SourceOpen,
            "audio duration frame count is outside u64 range",
        )
    })
}

fn parse_u32_probe_field(
    values: &BTreeMap<String, String>,
    key: &str,
    source_id: &str,
) -> Result<u32, BroadcastEngineError> {
    values
        .get(key)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                format!("probe did not return valid {key}"),
            )
            .with_source_id(source_id.to_string())
        })
}

fn parse_u16_probe_field(
    values: &BTreeMap<String, String>,
    key: &str,
    source_id: &str,
) -> Result<u16, BroadcastEngineError> {
    values
        .get(key)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::SourceOpen,
                format!("probe did not return valid {key}"),
            )
            .with_source_id(source_id.to_string())
        })
}

fn parse_optional_u64_probe_field(values: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    values.get(key).and_then(|value| value.parse::<u64>().ok())
}

fn parse_u32_ratio(value: &str) -> Option<(u32, u32)> {
    let (num, den) = value.split_once('/')?;
    let num = num.parse::<u32>().ok()?;
    let den = den.parse::<u32>().ok()?;
    if num == 0 || den == 0 {
        return None;
    }
    Some((num, den))
}

fn parse_field_mode(value: Option<&String>) -> FieldMode {
    match value.map(String::as_str) {
        Some("tt") | Some("tb") => FieldMode::InterlacedUpperFirst,
        Some("bb") | Some("bt") => FieldMode::InterlacedLowerFirst,
        _ => FieldMode::Progressive,
    }
}

fn parse_color_space(value: Option<&String>) -> ColorSpace {
    match value.map(String::as_str) {
        Some("bt709") => ColorSpace::Rec709,
        Some("bt2020nc") | Some("bt2020c") => ColorSpace::Rec2020,
        Some("rgb") => ColorSpace::Srgb,
        Some(custom) => ColorSpace::Custom(custom.to_string()),
        None => ColorSpace::Rec709,
    }
}

fn ceil_div_u128(numerator: u128, denominator: u128) -> Option<u128> {
    if denominator == 0 {
        return None;
    }
    numerator
        .checked_add(denominator.saturating_sub(1))?
        .checked_div(denominator)
}

fn contract_error(message: impl Into<String>) -> BroadcastEngineError {
    BroadcastEngineError::new(BroadcastEngineErrorKind::Contract, message)
}

fn ffmpeg_frame_seek(
    start_frame: u64,
    timebase: Timebase,
    error_kind: BroadcastEngineErrorKind,
) -> Result<FfmpegFrameSeek, BroadcastEngineError> {
    let input_seek_frame = start_frame.saturating_sub(FFMPEG_INPUT_SEEK_PREROLL_FRAMES);
    Ok(FfmpegFrameSeek {
        input_seek_frame,
        relative_start_frame: start_frame.saturating_sub(input_seek_frame),
        input_seek_position: ffmpeg_frame_position(input_seek_frame, timebase, error_kind)?,
    })
}

fn ffmpeg_frame_position(
    frame: u64,
    timebase: Timebase,
    error_kind: BroadcastEngineErrorKind,
) -> Result<String, BroadcastEngineError> {
    let numerator = u128::from(frame)
        .checked_mul(u128::from(timebase.frame_rate_den))
        .ok_or_else(|| BroadcastEngineError::new(error_kind, "ffmpeg seek position overflow"))?;
    let denominator = u128::from(timebase.frame_rate_num);
    if denominator == 0 {
        return Err(BroadcastEngineError::new(
            error_kind,
            "ffmpeg seek timebase denominator is zero",
        ));
    }
    let whole = numerator / denominator;
    let fraction = numerator % denominator;
    let micros = fraction
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_div(denominator))
        .ok_or_else(|| BroadcastEngineError::new(error_kind, "ffmpeg seek fraction overflow"))?;
    Ok(format!("{whole}.{micros:06}"))
}

fn audio_sample_span_for_frame(
    frame: u64,
    sample_rate_hz: u32,
    timebase: Timebase,
) -> Result<(u64, u64), BroadcastEngineError> {
    let start_sample = audio_sample_at_frame(frame, sample_rate_hz, timebase)?;
    let next_frame = frame.checked_add(1).ok_or_else(|| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "frame number overflow while mapping audio samples",
        )
    })?;
    let end_sample = audio_sample_at_frame(next_frame, sample_rate_hz, timebase)?;
    if end_sample <= start_sample {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio sample span is empty",
        ));
    }
    Ok((start_sample, end_sample))
}

fn audio_sample_at_frame(
    frame: u64,
    sample_rate_hz: u32,
    timebase: Timebase,
) -> Result<u64, BroadcastEngineError> {
    let numerator = u128::from(frame)
        .checked_mul(u128::from(sample_rate_hz))
        .and_then(|value| value.checked_mul(u128::from(timebase.frame_rate_den)))
        .ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio sample mapping overflow",
            )
        })?;
    let value = numerator / u128::from(timebase.frame_rate_num);
    u64::try_from(value).map_err(|_| {
        BroadcastEngineError::new(
            BroadcastEngineErrorKind::AudioOutput,
            "audio sample mapping is outside u64 range",
        )
    })
}

fn stderr_error(kind: BroadcastEngineErrorKind, stderr: &[u8]) -> BroadcastEngineError {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    BroadcastEngineError::new(
        kind,
        if message.is_empty() {
            "ffmpeg command failed".to_string()
        } else {
            message
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qnc_broadcast_player::{ColorSpace, FieldMode};

    #[test]
    fn ffmpeg_toolchain_accepts_explicit_binary_paths() {
        let toolchain = FfmpegToolchain::new("tools/ffmpeg-custom", "tools/ffprobe-custom")
            .expect("toolchain paths should be valid");

        assert_eq!(toolchain.ffmpeg(), Path::new("tools/ffmpeg-custom"));
        assert_eq!(toolchain.ffprobe(), Path::new("tools/ffprobe-custom"));
    }

    #[test]
    fn ffmpeg_toolchain_rejects_blank_binary_paths() {
        let err = FfmpegToolchain::new(" ", "tools/ffprobe-custom").unwrap_err();
        assert!(err.contains("ffmpeg path must not be blank"));

        let err = FfmpegToolchain::new("tools/ffmpeg-custom", "").unwrap_err();
        assert!(err.contains("ffprobe path must not be blank"));
    }

    #[test]
    fn decode_options_carry_explicit_toolchain() {
        let toolchain = FfmpegToolchain::new("tools/ffmpeg-custom", "tools/ffprobe-custom")
            .expect("toolchain paths should be valid");

        let video_options = FfmpegDecodeOptions::software().with_toolchain(toolchain.clone());
        let audio_options = FfmpegAudioDecodeOptions::default().with_toolchain(toolchain.clone());

        assert_eq!(video_options.toolchain, toolchain);
        assert_eq!(
            audio_options.toolchain.ffmpeg(),
            Path::new("tools/ffmpeg-custom")
        );
        assert_eq!(
            audio_options.toolchain.ffprobe(),
            Path::new("tools/ffprobe-custom")
        );
    }

    #[test]
    fn hardware_decode_defaults_to_software_without_ffmpeg_args() {
        let options = FfmpegDecodeOptions::default();

        assert_eq!(options.hardware_decode, FfmpegHardwareDecode::Software);
        assert_eq!(options.video_prefetch_frames, DEFAULT_VIDEO_PREFETCH_FRAMES);
        assert_eq!(options.video_cache_frames, DEFAULT_VIDEO_CACHE_FRAMES);
        assert_eq!(options.video_cache_bytes, DEFAULT_VIDEO_CACHE_BYTES);
        assert!(options.hardware_decode.ffmpeg_input_args().is_empty());
    }

    #[test]
    fn decode_options_clamp_prefetch_and_cache_to_one() {
        let options = FfmpegDecodeOptions::software()
            .with_video_prefetch_frames(0)
            .with_video_cache_frames(0)
            .with_video_cache_bytes(0);

        assert_eq!(options.video_prefetch_frames, 1);
        assert_eq!(options.video_cache_frames, 1);
        assert_eq!(options.video_cache_bytes, 1);
    }

    #[test]
    fn hardware_decode_auto_builds_ffmpeg_input_args() {
        let options =
            FfmpegDecodeOptions::software().with_hardware_decode(FfmpegHardwareDecode::Auto);

        assert_eq!(
            options.hardware_decode.ffmpeg_input_args(),
            vec!["-hwaccel".to_string(), "auto".to_string()]
        );
    }

    #[test]
    fn hardware_decode_backend_builds_ffmpeg_input_args() {
        let hardware_decode = FfmpegHardwareDecode::backend("cuda").unwrap();

        assert_eq!(
            hardware_decode.ffmpeg_input_args(),
            vec!["-hwaccel".to_string(), "cuda".to_string()]
        );
    }

    #[test]
    fn hardware_decode_rejects_blank_backend() {
        let err = FfmpegHardwareDecode::backend(" ").unwrap_err();

        assert!(err.contains("must not be blank"));
    }

    #[test]
    fn ffmpeg_frame_seek_uses_preroll_and_relative_frame() {
        let seek = ffmpeg_frame_seek(
            3_000,
            Timebase::new(50, 1).unwrap(),
            BroadcastEngineErrorKind::VideoDecode,
        )
        .unwrap();

        assert_eq!(seek.input_seek_frame, 2_975);
        assert_eq!(seek.relative_start_frame, 25);
        assert_eq!(seek.input_seek_position, "59.500000");
    }

    #[test]
    fn ffmpeg_frame_position_handles_fractional_broadcast_timebase() {
        let position = ffmpeg_frame_position(
            30,
            Timebase::new(30_000, 1_001).unwrap(),
            BroadcastEngineErrorKind::VideoDecode,
        )
        .unwrap();

        assert_eq!(position, "1.001000");
    }

    #[test]
    fn parse_ffmpeg_hwaccels_skips_header_line() {
        let backends =
            parse_ffmpeg_hwaccels("Hardware acceleration methods:\r\ncuda\r\nd3d11va\r\n\r\n");

        assert_eq!(backends, vec!["cuda".to_string(), "d3d11va".to_string()]);
    }

    #[test]
    fn cache_miss_pulls_only_small_start_window_synchronously() {
        assert_eq!(synchronous_cache_end_frame(5_000, 5_007), 5_000);
        assert_eq!(synchronous_cache_end_frame(5_000, 5_000), 5_000);
    }

    #[test]
    fn video_session_caches_decoded_rgb_frames_and_trims_oldest() {
        let mut session = video_session(2, 1, 20);
        let frames = vec![
            FfmpegVideoPayload {
                frame: 10,
                bytes: vec![10; 6],
            },
            FfmpegVideoPayload {
                frame: 11,
                bytes: vec![11; 6],
            },
            FfmpegVideoPayload {
                frame: 12,
                bytes: vec![12; 6],
            },
            FfmpegVideoPayload {
                frame: 13,
                bytes: vec![13; 6],
            },
        ];

        session.cache_decoded_frames(frames, 3, usize::MAX).unwrap();

        assert!(session.cached_frame(10).is_none());
        assert_eq!(session.cached_frame(11).unwrap().bytes, vec![11; 6]);
        assert_eq!(session.cached_frame(12).unwrap().bytes, vec![12; 6]);
        assert_eq!(session.cached_frame(13).unwrap().bytes, vec![13; 6]);
    }

    #[test]
    fn video_session_trims_cache_by_byte_budget() {
        let mut session = video_session(2, 1, 20);
        let frames = vec![
            FfmpegVideoPayload {
                frame: 10,
                bytes: vec![10; 6],
            },
            FfmpegVideoPayload {
                frame: 11,
                bytes: vec![11; 6],
            },
            FfmpegVideoPayload {
                frame: 12,
                bytes: vec![12; 6],
            },
        ];

        session.cache_decoded_frames(frames, 8, 12).unwrap();

        assert!(session.cached_frame(10).is_none());
        assert_eq!(session.cached_frame(11).unwrap().bytes, vec![11; 6]);
        assert_eq!(session.cached_frame(12).unwrap().bytes, vec![12; 6]);
        assert_eq!(session.cache_byte_len(), 12);
    }

    #[test]
    fn video_session_ready_stream_cache_without_stream_is_noop() {
        let mut session = video_session(2, 1, 20);

        session
            .cache_ready_streamed_frames(12, 8, usize::MAX)
            .unwrap();

        assert!(session.cached_frame(12).is_none());
        assert_eq!(session.cache_byte_len(), 0);
    }

    #[test]
    fn video_session_labels_boundary_payload_with_requested_frame() {
        let mut session = video_session(2, 1, 20);

        session
            .cache_decoded_frames(
                vec![FfmpegVideoPayload {
                    frame: 19,
                    bytes: vec![7; 6],
                }],
                8,
                usize::MAX,
            )
            .unwrap();

        let payload = session.payload_for_request(20).unwrap();
        assert_eq!(payload.frame, 20);
        assert_eq!(payload.bytes, vec![7; 6]);
    }

    #[test]
    fn video_session_rejects_payload_size_mismatch() {
        let mut session = video_session(2, 1, 20);

        let err = session
            .cache_decoded_frames(
                vec![FfmpegVideoPayload {
                    frame: 0,
                    bytes: vec![1, 2, 3, 4, 5],
                }],
                8,
                usize::MAX,
            )
            .unwrap_err();

        assert_eq!(err.kind, BroadcastEngineErrorKind::VideoDecode);
        assert!(err.to_string().contains("byte size"));
    }

    #[test]
    fn video_session_prefetch_end_stays_inside_decodable_frames() {
        let session = video_session(2, 1, 20);

        assert_eq!(session.decode_frame_for_request(20), 19);
        assert_eq!(session.prefetch_end_frame(18, 8), 19);
        assert_eq!(session.prefetch_end_frame(4, 1), 4);
    }

    #[test]
    fn video_frame_span_len_counts_inclusive_frame_window() {
        assert_eq!(frame_span_len(10, 17).unwrap(), 8);
        assert_eq!(frame_span_len(4, 4).unwrap(), 1);
    }

    #[test]
    fn video_frame_span_len_rejects_reversed_window() {
        let err = frame_span_len(8, 7).unwrap_err();

        assert_eq!(err.kind, BroadcastEngineErrorKind::VideoDecode);
        assert!(err.to_string().contains("span is invalid"));
    }

    #[test]
    fn fixed_size_pipe_reader_emits_bounded_video_chunks() {
        let (payload_tx, payload_rx) = sync_channel::<Result<Vec<u8>, FfmpegPipeReadError>>(3);

        read_ffmpeg_fixed_size_pipe(std::io::Cursor::new(vec![1, 2, 3, 4]), 2, payload_tx);

        assert_eq!(payload_rx.recv().unwrap().unwrap(), vec![1, 2]);
        assert_eq!(payload_rx.recv().unwrap().unwrap(), vec![3, 4]);
        assert_eq!(
            payload_rx.recv().unwrap().unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn audio_decode_options_clamp_prefetch_and_cache_to_one() {
        let options = FfmpegAudioDecodeOptions::default()
            .with_audio_prefetch_frames(0)
            .with_audio_cache_frames(0)
            .with_audio_cache_bytes(0);

        assert_eq!(options.audio_prefetch_frames, 1);
        assert_eq!(options.audio_cache_frames, 1);
        assert_eq!(options.audio_cache_bytes, 1);
    }

    #[test]
    fn audio_session_maps_source_boundary_to_last_decodable_frame() {
        let output = audio_session(20);

        assert_eq!(output.decode_frame_for_request(19), 19);
        assert_eq!(output.decode_frame_for_request(20), 19);
    }

    #[test]
    fn audio_session_labels_boundary_payload_with_requested_frame() {
        let mut session = audio_session(20);

        session
            .cache_decoded_packets(
                vec![FfmpegAudioPayload {
                    frame: 19,
                    bytes: vec![7; 10],
                }],
                8,
                usize::MAX,
            )
            .unwrap();

        let payload = session.payload_for_request(20).unwrap();
        assert_eq!(payload.frame, 20);
        assert_eq!(payload.bytes, vec![7; 10]);
    }

    #[test]
    fn audio_session_trims_cache_by_byte_budget() {
        let mut session = audio_session(20);

        session
            .cache_decoded_packets(
                vec![
                    FfmpegAudioPayload {
                        frame: 10,
                        bytes: vec![10; 4],
                    },
                    FfmpegAudioPayload {
                        frame: 11,
                        bytes: vec![11; 4],
                    },
                    FfmpegAudioPayload {
                        frame: 12,
                        bytes: vec![12; 4],
                    },
                ],
                8,
                8,
            )
            .unwrap();

        assert!(session.payload_for_request(10).is_none());
        assert_eq!(session.payload_for_request(11).unwrap().bytes, vec![11; 4]);
        assert_eq!(session.payload_for_request(12).unwrap().bytes, vec![12; 4]);
        assert_eq!(session.cache_byte_len(), 8);
    }

    #[test]
    fn audio_session_ready_stream_cache_without_stream_is_noop() {
        let mut session = audio_session(20);

        session
            .cache_ready_streamed_packets(12, 8, usize::MAX)
            .unwrap();

        assert!(session.payload_for_request(12).is_none());
        assert_eq!(session.cache_byte_len(), 0);
    }

    #[test]
    fn audio_sample_span_is_exact_for_25_timebase() {
        let timebase = Timebase::new(25, 1).unwrap();

        assert_eq!(
            audio_sample_span_for_frame(0, 48_000, timebase).unwrap(),
            (0, 1920)
        );
        assert_eq!(
            audio_sample_span_for_frame(1, 48_000, timebase).unwrap(),
            (1920, 3840)
        );
    }

    #[test]
    fn audio_sample_span_is_exact_for_30_and_60_timebase() {
        let timebase_30 = Timebase::new(30, 1).unwrap();
        let timebase_60 = Timebase::new(60, 1).unwrap();

        assert_eq!(
            audio_sample_span_for_frame(0, 48_000, timebase_30).unwrap(),
            (0, 1600)
        );
        assert_eq!(
            audio_sample_span_for_frame(1, 48_000, timebase_30).unwrap(),
            (1600, 3200)
        );
        assert_eq!(
            audio_sample_span_for_frame(0, 48_000, timebase_60).unwrap(),
            (0, 800)
        );
        assert_eq!(
            audio_sample_span_for_frame(1, 48_000, timebase_60).unwrap(),
            (800, 1600)
        );
    }

    #[test]
    fn audio_packet_byte_len_uses_integer_frame_sample_span() {
        let audio_format = AudioFormat::new(48_000, 2).unwrap();
        let timebase = Timebase::new(25, 1).unwrap();

        assert_eq!(
            audio_packet_byte_len_for_frame(0, &audio_format, timebase).unwrap(),
            7680
        );
    }

    #[test]
    fn audio_sample_span_distributes_2997_fractional_remainder() {
        let timebase = Timebase::new(30_000, 1001).unwrap();

        assert_eq!(
            audio_sample_span_for_frame(0, 48_000, timebase).unwrap(),
            (0, 1601)
        );
        assert_eq!(
            audio_sample_span_for_frame(1, 48_000, timebase).unwrap(),
            (1601, 3203)
        );
        assert_eq!(
            audio_sample_span_for_frame(4, 48_000, timebase).unwrap(),
            (6406, 8008)
        );
    }

    #[test]
    fn audio_sample_span_distributes_5994_fractional_remainder() {
        let timebase = Timebase::new(60_000, 1001).unwrap();

        assert_eq!(
            audio_sample_span_for_frame(0, 48_000, timebase).unwrap(),
            (0, 800)
        );
        assert_eq!(
            audio_sample_span_for_frame(1, 48_000, timebase).unwrap(),
            (800, 1601)
        );
        assert_eq!(
            audio_sample_span_for_frame(9, 48_000, timebase).unwrap(),
            (7207, 8008)
        );
    }

    #[test]
    fn parse_duration_frames_prefers_declared_frame_count() {
        let mut values = BTreeMap::new();
        values.insert("nb_frames".to_string(), "890".to_string());
        values.insert("duration_ts".to_string(), "1".to_string());
        values.insert("time_base".to_string(), "1/50".to_string());

        let frames = parse_duration_frames(&values, Timebase::new(50, 1).unwrap(), "src").unwrap();

        assert_eq!(frames, 890);
    }

    #[test]
    fn parse_duration_frames_maps_probe_ticks_to_frames() {
        let mut values = BTreeMap::new();
        values.insert("duration_ts".to_string(), "890".to_string());
        values.insert("time_base".to_string(), "1/50".to_string());

        let frames = parse_duration_frames(&values, Timebase::new(50, 1).unwrap(), "src").unwrap();

        assert_eq!(frames, 890);
    }

    #[test]
    fn parse_duration_frames_maps_mp4_ticks_to_frames() {
        let mut values = BTreeMap::new();
        values.insert("duration_ts".to_string(), "890000".to_string());
        values.insert("time_base".to_string(), "1/50000".to_string());

        let frames = parse_duration_frames(&values, Timebase::new(50, 1).unwrap(), "src").unwrap();

        assert_eq!(frames, 890);
    }

    #[test]
    fn audio_sample_spans_accumulate_without_drift_for_broadcast_timebases() {
        for (timebase, frames) in [
            (Timebase::new(25, 1).unwrap(), 1_000),
            (Timebase::new(50, 1).unwrap(), 1_000),
            (Timebase::new(30, 1).unwrap(), 1_000),
            (Timebase::new(60, 1).unwrap(), 1_000),
            (Timebase::new(30_000, 1001).unwrap(), 1_001),
            (Timebase::new(60_000, 1001).unwrap(), 1_001),
        ] {
            assert_contiguous_audio_sample_spans(timebase, 48_000, frames);
        }
    }

    fn video_session(width: u32, height: u32, duration_frames: u64) -> FfmpegVideoSession {
        let source = EngineSourceHandle {
            source_id: "src".to_string(),
            source_revision: None,
            duration_frames,
            timebase: Timebase::new(25, 1).unwrap(),
            video_format: Some(
                VideoFormat::new(width, height, FieldMode::Progressive, ColorSpace::Rec709)
                    .unwrap(),
            ),
            audio_format: None,
        };
        FfmpegVideoSession::new(&source).unwrap()
    }

    fn assert_contiguous_audio_sample_spans(timebase: Timebase, sample_rate_hz: u32, frames: u64) {
        let mut previous_end = 0;
        let mut accumulated_samples = 0;
        for frame in 0..frames {
            let (start, end) =
                audio_sample_span_for_frame(frame, sample_rate_hz, timebase).unwrap();
            assert_eq!(start, previous_end, "gap before frame {frame}");
            accumulated_samples += end - start;
            previous_end = end;
        }
        assert_eq!(
            accumulated_samples,
            audio_sample_at_frame(frames, sample_rate_hz, timebase).unwrap()
        );
    }

    fn audio_session(duration_frames: u64) -> FfmpegAudioSession {
        let source = EngineSourceHandle {
            source_id: "src".to_string(),
            source_revision: None,
            duration_frames,
            timebase: Timebase::new(25, 1).unwrap(),
            video_format: None,
            audio_format: Some(AudioFormat::new(48_000, 1).unwrap()),
        };
        FfmpegAudioSession::new(&source).unwrap()
    }
}
