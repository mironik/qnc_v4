#![forbid(unsafe_code)]

pub mod export_profile;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub type ServiceResult<T> = Result<T, ServiceError>;

pub const JOB_TYPE_FILMSTRIP: &str = "filmstrip";
pub const JOB_SOURCE_FILMSTRIP: &str = "filmstrip";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProfile {
    Light,
    LocalAi,
    Enterprise,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameTimebase {
    pub fps_num: u32,
    pub fps_den: u32,
}

impl FrameTimebase {
    pub fn new(fps_num: u32, fps_den: u32) -> ServiceResult<Self> {
        if fps_num == 0 || fps_den == 0 {
            return Err(ServiceError::new(
                "invalid_timebase",
                "Frame timebase must come from probe metadata and cannot be zero.",
            ));
        }

        Ok(Self { fps_num, fps_den })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRange {
    pub source_in: i64,
    pub source_out: i64,
    pub timebase: FrameTimebase,
}

impl FrameRange {
    pub fn frame_len(&self) -> i64 {
        (self.source_out - self.source_in).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.source_out <= self.source_in
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Progressive,
    InterlacedTopFieldFirst,
    InterlacedBottomFieldFirst,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRef {
    pub clip_id: String,
    pub locator: MediaLocator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaLocator {
    LocalPath { path: PathBuf },
    IntranetPath { uri: String },
    ManagedAsset { asset_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProbe {
    pub width: u32,
    pub height: u32,
    pub duration_sec: Option<f64>,
    pub timebase: FrameTimebase,
    pub scan_mode: ScanMode,
    pub codec: String,
    pub field_order: String,
    pub frame_count: Option<i64>,
    pub duration_frames: Option<i64>,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioProbeRequest {
    pub input: MediaRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioProbe {
    pub duration_sec: Option<f64>,
    pub codec: String,
    pub has_audio: bool,
    pub audio_channels: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: PathBuf,
    pub media_type: String,
    pub render_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameExtractRequest {
    pub input: MediaRef,
    pub frame: i64,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosterExtractRequest {
    pub input: MediaRef,
    pub output_path: PathBuf,
    pub seek_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmstripRequest {
    pub input: MediaRef,
    pub frame_count: usize,
    pub seek_seconds: Vec<f64>,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmstripFrameArtifact {
    pub index: usize,
    pub seek_sec: f64,
    pub artifact: ArtifactRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmstripJobFrame {
    pub index: usize,
    pub seek_sec: f64,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmstripJobPayload {
    pub media_path: PathBuf,
    pub duration_sec: f64,
    pub frames: Vec<FilmstripJobFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmstripJobResult {
    pub duration_sec: f64,
    pub frames: Vec<FilmstripFrameArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyBuildRequest {
    pub input: MediaRef,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioWrapRequest {
    pub input: MediaRef,
    pub output_path: PathBuf,
    pub fps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformRequest {
    pub input: MediaRef,
    pub range: Option<FrameRange>,
    pub peak_buckets: usize,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformPeaks {
    pub a1_peaks: Vec<f32>,
    pub a2_peaks: Vec<f32>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractRangeRequest {
    pub input: MediaRef,
    pub range: FrameRange,
    pub output_path: PathBuf,
}

#[async_trait]
pub trait MediaProcessor: Send + Sync {
    async fn probe(&self, input: &MediaRef) -> ServiceResult<MediaProbe>;
    async fn probe_audio(&self, request: AudioProbeRequest) -> ServiceResult<AudioProbe>;
    async fn extract_frame(&self, request: FrameExtractRequest) -> ServiceResult<ArtifactRef>;
    async fn extract_poster(&self, request: PosterExtractRequest) -> ServiceResult<ArtifactRef>;
    async fn build_filmstrip(
        &self,
        request: FilmstripRequest,
    ) -> ServiceResult<Vec<FilmstripFrameArtifact>>;
    async fn build_proxy(&self, request: ProxyBuildRequest) -> ServiceResult<ArtifactRef>;
    async fn build_audio_wrap(&self, request: AudioWrapRequest) -> ServiceResult<ArtifactRef>;
    async fn build_waveform(&self, request: WaveformRequest) -> ServiceResult<WaveformPeaks>;
    async fn extract_range(&self, request: ExtractRangeRequest) -> ServiceResult<ArtifactRef>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionRequest {
    pub input: MediaRef,
    pub range: Option<FrameRange>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub clip_id: String,
    pub language: Option<String>,
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub source_in: i64,
    pub source_out: i64,
    pub text: String,
}

#[async_trait]
pub trait TranscriptionEngine: Send + Sync {
    async fn transcribe(&self, request: TranscriptionRequest) -> ServiceResult<Transcript>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDocument {
    pub clip_id: String,
    pub source_in: i64,
    pub source_out: i64,
    pub title: Option<String>,
    pub body: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub clip_id: String,
    pub source_in: i64,
    pub source_out: i64,
    pub score: f32,
    pub text: String,
    pub metadata: serde_json::Value,
}

#[async_trait]
pub trait SearchEngine: Send + Sync {
    async fn index_document(&self, document: SearchDocument) -> ServiceResult<()>;
    async fn remove_clip(&self, clip_id: &str) -> ServiceResult<()>;
    async fn search(&self, request: SearchRequest) -> ServiceResult<Vec<SearchHit>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequest {
    pub task: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResponse {
    pub output: serde_json::Value,
}

#[async_trait]
pub trait AIOrchestrator: Send + Sync {
    async fn run(&self, request: AiRequest) -> ServiceResult<AiResponse>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobClaimRequest {
    pub worker_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub max_jobs: Option<usize>,
    #[serde(default)]
    pub lease_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLease {
    pub job_id: String,
    pub project_id: String,
    pub job_type: String,
    pub source_id: String,
    pub clip_id: String,
    pub worker_id: String,
    pub lease_id: String,
    pub lease_until_unix_ms: u64,
    pub attempts: i64,
    pub queued_at: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGenerateJobPayload {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub asset_status: String,
    pub card_locked: bool,
    pub original_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyGenerateJobResult {
    pub output_path: PathBuf,
    #[serde(default)]
    pub probe: Option<MediaProbe>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobClaimResponse {
    pub jobs: Vec<JobLease>,
    pub playback_active: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHeartbeatRequest {
    pub worker_id: String,
    pub project_id: String,
    pub lease_id: String,
    #[serde(default)]
    pub job_ids: Vec<String>,
    #[serde(default)]
    pub lease_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHeartbeatResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
    pub lease_until_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompleteRequest {
    pub worker_id: String,
    pub project_id: String,
    pub lease_id: String,
    pub job_id: String,
    #[serde(default)]
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFailRequest {
    pub worker_id: String,
    pub project_id: String,
    pub lease_id: String,
    pub job_id: String,
    pub error: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobAck {
    pub accepted: bool,
    pub job_id: String,
    pub message: Option<String>,
}

#[async_trait]
pub trait JobService: Send + Sync {
    async fn claim(&self, request: JobClaimRequest) -> ServiceResult<JobClaimResponse>;
    async fn heartbeat(&self, request: JobHeartbeatRequest) -> ServiceResult<JobHeartbeatResponse>;
    async fn complete(&self, request: JobCompleteRequest) -> ServiceResult<JobAck>;
    async fn fail(&self, request: JobFailRequest) -> ServiceResult<JobAck>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportJobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub project_id: String,
    pub playlist: serde_json::Value,
    pub project_settings: serde_json::Value,
    pub export_settings: serde_json::Value,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportJob {
    pub job_id: String,
    pub state: ExportJobState,
    pub artifacts: Vec<ArtifactRef>,
    pub message: Option<String>,
}

#[async_trait]
pub trait ExportEngine: Send + Sync {
    async fn submit(&self, request: ExportRequest) -> ServiceResult<ExportJob>;
    async fn status(&self, job_id: &str) -> ServiceResult<ExportJob>;
    async fn cancel(&self, job_id: &str) -> ServiceResult<()>;
}

#[derive(Clone)]
pub struct ServiceRegistry {
    pub media: Arc<dyn MediaProcessor>,
    pub transcription: Arc<dyn TranscriptionEngine>,
    pub search: Arc<dyn SearchEngine>,
    pub ai: Arc<dyn AIOrchestrator>,
    pub export: Arc<dyn ExportEngine>,
}
