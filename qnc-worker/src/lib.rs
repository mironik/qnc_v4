#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use qnc_service_contracts::{
    AudioWrapJobArtifact, AudioWrapJobPayload, AudioWrapJobResult, ExportHiResJobPayload,
    ExportHiResJobResult, ExportHiResPlaylistItem, ExportHiResPlaylistSource, FilmstripJobPayload,
    FilmstripJobResult, FrameTimebase, JobAck, JobClaimRequest, JobClaimResponse,
    JobCompleteRequest, JobFailRequest, JobHeartbeatRequest, JobHeartbeatResponse, JobLease,
    MediaProbe, MediaProbeJobPayload, MediaProbeJobResult, PosterJobPayload, PosterJobResult,
    ProxyGenerateJobPayload, ProxyGenerateJobResult, ScanMode, WaveformJobPayload,
    WaveformJobResult, WorkerPlacement, JOB_TYPE_AUDIO_WRAP, JOB_TYPE_EXPORT_HIRES,
    JOB_TYPE_FILMSTRIP, JOB_TYPE_MEDIA_PROBE, JOB_TYPE_THUMB_PROXY, JOB_TYPE_WAVEFORM,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_HOST_URL: &str = "http://127.0.0.1:8001";
pub const DEFAULT_POLL_MS: u64 = 500;
pub const DEFAULT_LEASE_MS: u64 = 30_000;
pub const SMOKE_JOB_TYPE: &str = "qnc_worker_smoke";
pub const PROXY_GENERATE_JOB_TYPE: &str = "proxy_generate";
pub const FILMSTRIP_JOB_TYPE: &str = JOB_TYPE_FILMSTRIP;
pub const WAVEFORM_JOB_TYPE: &str = JOB_TYPE_WAVEFORM;
pub const THUMB_PROXY_JOB_TYPE: &str = JOB_TYPE_THUMB_PROXY;
pub const AUDIO_WRAP_JOB_TYPE: &str = JOB_TYPE_AUDIO_WRAP;
pub const MEDIA_PROBE_JOB_TYPE: &str = JOB_TYPE_MEDIA_PROBE;
pub const EXPORT_HIRES_JOB_TYPE: &str = JOB_TYPE_EXPORT_HIRES;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub placement: WorkerPlacement,
    pub requested_capabilities: Vec<String>,
    pub poll_interval: Duration,
    pub lease_ms: u64,
}

impl WorkerConfig {
    pub fn new(
        worker_id: impl Into<String>,
        requested_capabilities: Vec<String>,
        poll_ms: u64,
        lease_ms: u64,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            placement: WorkerPlacement::LocalWorkstation,
            requested_capabilities,
            poll_interval: Duration::from_millis(poll_ms.max(50)),
            lease_ms: lease_ms.max(5_000),
        }
    }

    pub fn with_placement(mut self, placement: WorkerPlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn claim_capabilities(&self, registry: &HandlerRegistry) -> Vec<String> {
        let supported = registry.capabilities();
        if self.requested_capabilities.is_empty() {
            return supported;
        }
        let requested: HashSet<String> = self
            .requested_capabilities
            .iter()
            .map(|value| normalize_job_type(value))
            .filter(|value| !value.is_empty())
            .collect();
        supported
            .into_iter()
            .filter(|capability| requested.contains(capability))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTick {
    pub playback_active: bool,
    pub claimed: usize,
    pub completed: usize,
    pub failed: usize,
    pub idle_reason: Option<String>,
}

impl WorkerTick {
    fn idle(reason: impl Into<String>) -> Self {
        Self {
            playback_active: false,
            claimed: 0,
            completed: 0,
            failed: 0,
            idle_reason: Some(reason.into()),
        }
    }
}

pub trait WorkerHost: Send + Sync {
    fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String>;
    fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String>;
    fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String>;
    fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String>;
}

pub struct JobExecutionContext<'a> {
    pub host: &'a dyn WorkerHost,
}

pub trait JobHandler: Send + Sync {
    fn job_type(&self) -> &'static str;
    fn run(
        &self,
        job: &JobLease,
        context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError>;
}

pub trait ProxyBuilder: Send + Sync {
    fn build_proxy(
        &self,
        payload: ProxyGenerateJobPayload,
    ) -> Result<ProxyGenerateJobResult, JobHandlerError>;
}

pub trait FilmstripBuilder: Send + Sync {
    fn build_filmstrip(
        &self,
        payload: FilmstripJobPayload,
    ) -> Result<FilmstripJobResult, JobHandlerError>;
}

pub trait WaveformBuilder: Send + Sync {
    fn build_waveform(
        &self,
        payload: WaveformJobPayload,
    ) -> Result<WaveformJobResult, JobHandlerError>;
}

pub trait PosterBuilder: Send + Sync {
    fn build_poster(&self, payload: PosterJobPayload) -> Result<PosterJobResult, JobHandlerError>;
}

pub trait AudioWrapBuilder: Send + Sync {
    fn build_audio_wrap(
        &self,
        payload: AudioWrapJobPayload,
    ) -> Result<AudioWrapJobResult, JobHandlerError>;
}

pub trait MediaProbeBuilder: Send + Sync {
    fn probe_media(
        &self,
        payload: MediaProbeJobPayload,
    ) -> Result<MediaProbeJobResult, JobHandlerError>;
}

pub trait HiResExporter: Send + Sync {
    fn export_hires(
        &self,
        payload: ExportHiResJobPayload,
    ) -> Result<ExportHiResJobResult, JobHandlerError>;

    fn export_hires_with_context(
        &self,
        payload: ExportHiResJobPayload,
        _job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<ExportHiResJobResult, JobHandlerError> {
        self.export_hires(payload)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobHandlerError {
    pub message: String,
    pub retryable: bool,
}

impl JobHandlerError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

pub struct HandlerRegistry {
    handlers: HashMap<String, Box<dyn JobHandler>>,
}

impl HandlerRegistry {
    pub fn empty() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn with_builtin_handlers() -> Self {
        let mut registry = Self::empty();
        registry.register(SmokeJobHandler);
        registry.register(ProxyGenerateJobHandler::new(LocalFfmpegProxyBuilder));
        registry.register(FilmstripJobHandler::new(LocalFfmpegFilmstripBuilder));
        registry.register(WaveformJobHandler::new(LocalFfmpegWaveformBuilder));
        registry.register(PosterJobHandler::new(LocalFfmpegPosterBuilder));
        registry.register(AudioWrapJobHandler::new(LocalFfmpegAudioWrapBuilder));
        registry.register(MediaProbeJobHandler::new(LocalFfmpegMediaProbeBuilder));
        registry.register(ExportHiResJobHandler::new(LocalFfmpegHiResRenderer));
        registry
    }

    pub fn register<H>(&mut self, handler: H)
    where
        H: JobHandler + 'static,
    {
        self.handlers
            .insert(normalize_job_type(handler.job_type()), Box::new(handler));
    }

    pub fn capabilities(&self) -> Vec<String> {
        let mut values: Vec<String> = self.handlers.keys().cloned().collect();
        values.sort();
        values
    }

    fn get(&self, job_type: &str) -> Option<&dyn JobHandler> {
        self.handlers
            .get(&normalize_job_type(job_type))
            .map(|handler| handler.as_ref())
    }
}

pub struct Worker<H: WorkerHost> {
    config: WorkerConfig,
    host: H,
    registry: HandlerRegistry,
}

impl<H: WorkerHost> Worker<H> {
    pub fn new(config: WorkerConfig, host: H, registry: HandlerRegistry) -> Self {
        Self {
            config,
            host,
            registry,
        }
    }

    pub fn run_once(&self) -> Result<WorkerTick, String> {
        let capabilities = self.config.claim_capabilities(&self.registry);
        if capabilities.is_empty() {
            return Ok(WorkerTick::idle("no_executable_capabilities"));
        }

        let claim = self.host.claim(&JobClaimRequest {
            worker_id: self.config.worker_id.clone(),
            placement: Some(self.config.placement),
            project_id: None,
            capabilities,
            max_jobs: Some(1),
            lease_ms: Some(self.config.lease_ms),
        })?;

        if claim.playback_active {
            return Ok(WorkerTick {
                playback_active: true,
                claimed: 0,
                completed: 0,
                failed: 0,
                idle_reason: claim.message.or_else(|| Some("playback_active".into())),
            });
        }
        if claim.jobs.is_empty() {
            return Ok(WorkerTick {
                playback_active: false,
                claimed: 0,
                completed: 0,
                failed: 0,
                idle_reason: claim.message.or_else(|| Some("no_jobs".into())),
            });
        }

        let mut completed = 0usize;
        let mut failed = 0usize;
        for job in &claim.jobs {
            match self.run_claimed_job(job) {
                Ok(()) => completed += 1,
                Err(_) => failed += 1,
            }
        }

        Ok(WorkerTick {
            playback_active: false,
            claimed: claim.jobs.len(),
            completed,
            failed,
            idle_reason: None,
        })
    }

    fn run_claimed_job(&self, job: &JobLease) -> Result<(), String> {
        let Some(handler) = self.registry.get(&job.job_type) else {
            return Err(format!(
                "No handler registered for job_type={}",
                job.job_type
            ));
        };

        let heartbeat_request = JobHeartbeatRequest {
            worker_id: self.config.worker_id.clone(),
            project_id: job.project_id.clone(),
            lease_id: job.lease_id.clone(),
            job_ids: vec![job.job_id.clone()],
            lease_ms: Some(self.config.lease_ms),
        };
        ensure_heartbeat_accepted(&self.host.heartbeat(&heartbeat_request)?, &job.job_id)?;

        let (stop_heartbeat_tx, stop_heartbeat_rx) = mpsc::channel();
        let heartbeat_interval = heartbeat_interval(self.config.lease_ms);
        let run_result = thread::scope(|scope| {
            let host = &self.host;
            let request = heartbeat_request.clone();
            let job_id = job.job_id.clone();
            let heartbeat_thread = scope.spawn(move || {
                run_heartbeat_loop(host, request, job_id, heartbeat_interval, stop_heartbeat_rx);
            });
            let context = JobExecutionContext { host: &self.host };
            let result = handler.run(job, &context);
            let _ = stop_heartbeat_tx.send(());
            let _ = heartbeat_thread.join();
            result
        });

        match run_result {
            Ok(result) => {
                let ack = self.host.complete(&JobCompleteRequest {
                    worker_id: self.config.worker_id.clone(),
                    project_id: job.project_id.clone(),
                    lease_id: job.lease_id.clone(),
                    job_id: job.job_id.clone(),
                    result,
                })?;
                if ack.accepted {
                    Ok(())
                } else {
                    Err(ack.message.unwrap_or_else(|| "complete rejected".into()))
                }
            }
            Err(error) => {
                let _ = self.host.fail(&JobFailRequest {
                    worker_id: self.config.worker_id.clone(),
                    project_id: job.project_id.clone(),
                    lease_id: job.lease_id.clone(),
                    job_id: job.job_id.clone(),
                    error: error.message.clone(),
                    retryable: error.retryable,
                });
                Err(error.message)
            }
        }
    }
}

fn heartbeat_interval(lease_ms: u64) -> Duration {
    Duration::from_millis((lease_ms / 3).clamp(500, 10_000))
}

fn run_heartbeat_loop<H: WorkerHost>(
    host: &H,
    request: JobHeartbeatRequest,
    job_id: String,
    interval: Duration,
    stop: mpsc::Receiver<()>,
) {
    loop {
        match stop.recv_timeout(interval) {
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        match host.heartbeat(&request) {
            Ok(response) => {
                if ensure_heartbeat_accepted(&response, &job_id).is_err() {
                    return;
                }
            }
            Err(_) => {}
        }
    }
}

fn ensure_heartbeat_accepted(response: &JobHeartbeatResponse, job_id: &str) -> Result<(), String> {
    if response.accepted.iter().any(|id| id == job_id) {
        Ok(())
    } else {
        Err(format!("Lease heartbeat rejected for job_id={job_id}"))
    }
}

#[derive(Debug, Clone)]
pub struct HttpJobClient {
    base_url: String,
    agent: ureq::Agent,
}

impl HttpJobClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(30))
                .build(),
        }
    }

    fn absolute(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else if rel.starts_with('/') {
            format!("{}{rel}", self.base_url)
        } else {
            format!("{}/{rel}", self.base_url)
        }
    }

    fn post_json<T, R>(&self, rel: &str, payload: &T) -> Result<R, String>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let url = self.absolute(rel);
        self.agent
            .post(&url)
            .send_json(serde_json::to_value(payload).map_err(|e| e.to_string())?)
            .map_err(|error| format!("POST {rel}: {error}"))?
            .into_json()
            .map_err(|error| format!("POST {rel} json: {error}"))
    }
}

impl WorkerHost for HttpJobClient {
    fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String> {
        self.post_json("/api/jobs/claim", request)
    }

    fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String> {
        self.post_json("/api/jobs/heartbeat", request)
    }

    fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String> {
        self.post_json("/api/jobs/complete", request)
    }

    fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String> {
        self.post_json("/api/jobs/fail", request)
    }
}

#[derive(Debug, Clone)]
struct SmokeJobHandler;

impl JobHandler for SmokeJobHandler {
    fn job_type(&self) -> &'static str {
        SMOKE_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        Ok(json!({
            "status": "ok",
            "job_type": job.job_type,
            "clip_id": job.clip_id,
        }))
    }
}

pub struct ProxyGenerateJobHandler<B: ProxyBuilder> {
    builder: B,
}

impl<B: ProxyBuilder> ProxyGenerateJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for ProxyGenerateJobHandler<B>
where
    B: ProxyBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        PROXY_GENERATE_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: ProxyGenerateJobPayload = serde_json::from_value(job.payload.clone())
            .map_err(|error| JobHandlerError::fatal(format!("invalid proxy payload: {error}")))?;
        let result = self.builder.build_proxy(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid proxy result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegProxyBuilder;

impl ProxyBuilder for LocalFfmpegProxyBuilder {
    fn build_proxy(
        &self,
        payload: ProxyGenerateJobPayload,
    ) -> Result<ProxyGenerateJobResult, JobHandlerError> {
        let probe = qnc_media_ffmpeg::proxy::generate_field_proxy(
            &payload.source_path,
            &payload.output_path,
        )
        .map_err(JobHandlerError::retryable)?;
        Ok(ProxyGenerateJobResult {
            output_path: payload.output_path,
            probe: Some(probe),
        })
    }
}

pub struct FilmstripJobHandler<B: FilmstripBuilder> {
    builder: B,
}

impl<B: FilmstripBuilder> FilmstripJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for FilmstripJobHandler<B>
where
    B: FilmstripBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        FILMSTRIP_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: FilmstripJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::fatal(format!("invalid filmstrip payload: {error}"))
            })?;
        let result = self.builder.build_filmstrip(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid filmstrip result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegFilmstripBuilder;

impl FilmstripBuilder for LocalFfmpegFilmstripBuilder {
    fn build_filmstrip(
        &self,
        payload: FilmstripJobPayload,
    ) -> Result<FilmstripJobResult, JobHandlerError> {
        let requested = payload.frames.len();
        let frames = qnc_media_ffmpeg::filmstrip::build_filmstrip_frame_artifacts_at_paths(
            &payload.media_path,
            &payload.frames,
        )
        .map_err(JobHandlerError::retryable)?;
        if frames.len() != requested {
            return Err(JobHandlerError::retryable(format!(
                "filmstrip incomplete: {}/{} frames",
                frames.len(),
                requested
            )));
        }
        Ok(FilmstripJobResult {
            duration_sec: payload.duration_sec,
            frames,
        })
    }
}

pub struct WaveformJobHandler<B: WaveformBuilder> {
    builder: B,
}

impl<B: WaveformBuilder> WaveformJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for WaveformJobHandler<B>
where
    B: WaveformBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        WAVEFORM_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: WaveformJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::fatal(format!("invalid waveform payload: {error}"))
            })?;
        let result = self.builder.build_waveform(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid waveform result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegWaveformBuilder;

impl WaveformBuilder for LocalFfmpegWaveformBuilder {
    fn build_waveform(
        &self,
        payload: WaveformJobPayload,
    ) -> Result<WaveformJobResult, JobHandlerError> {
        qnc_media_ffmpeg::waveform::build_waveform_peaks(
            &payload.media_path,
            payload.sample_rate_hz,
            payload.peak_buckets,
        )
        .map_err(JobHandlerError::retryable)
    }
}

pub struct PosterJobHandler<B: PosterBuilder> {
    builder: B,
}

impl<B: PosterBuilder> PosterJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for PosterJobHandler<B>
where
    B: PosterBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        THUMB_PROXY_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: PosterJobPayload = serde_json::from_value(job.payload.clone())
            .map_err(|error| JobHandlerError::fatal(format!("invalid poster payload: {error}")))?;
        let result = self.builder.build_poster(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid poster result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegPosterBuilder;

impl PosterBuilder for LocalFfmpegPosterBuilder {
    fn build_poster(&self, payload: PosterJobPayload) -> Result<PosterJobResult, JobHandlerError> {
        qnc_media_ffmpeg::poster::extract_poster_jpeg_at_seek(
            &payload.media_path,
            &payload.output_path,
            payload.seek_sec,
        )
        .map_err(JobHandlerError::retryable)?;
        Ok(PosterJobResult {
            output_path: payload.output_path,
        })
    }
}

pub struct AudioWrapJobHandler<B: AudioWrapBuilder> {
    builder: B,
}

impl<B: AudioWrapBuilder> AudioWrapJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for AudioWrapJobHandler<B>
where
    B: AudioWrapBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        AUDIO_WRAP_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: AudioWrapJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::fatal(format!("invalid audio_wrap payload: {error}"))
            })?;
        let result = self.builder.build_audio_wrap(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid audio_wrap result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegAudioWrapBuilder;

impl AudioWrapBuilder for LocalFfmpegAudioWrapBuilder {
    fn build_audio_wrap(
        &self,
        payload: AudioWrapJobPayload,
    ) -> Result<AudioWrapJobResult, JobHandlerError> {
        let mut wraps = Vec::with_capacity(payload.wraps.len());
        for item in payload.wraps {
            qnc_media_ffmpeg::audio_wrap::wrap_audio_with_timecode(
                &payload.media_path,
                &item.output_path,
                item.fps,
            )
            .map_err(JobHandlerError::retryable)?;
            let probe = qnc_media_ffmpeg::proxy::probe_media(&item.output_path).ok();
            wraps.push(AudioWrapJobArtifact {
                fps: item.fps,
                output_path: item.output_path,
                probe,
            });
        }
        Ok(AudioWrapJobResult { wraps })
    }
}

pub struct MediaProbeJobHandler<B: MediaProbeBuilder> {
    builder: B,
}

impl<B: MediaProbeBuilder> MediaProbeJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for MediaProbeJobHandler<B>
where
    B: MediaProbeBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        MEDIA_PROBE_JOB_TYPE
    }

    fn run(
        &self,
        job: &JobLease,
        _context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let payload: MediaProbeJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::fatal(format!("invalid media_probe payload: {error}"))
            })?;
        let result = self.builder.probe_media(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid media_probe result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegMediaProbeBuilder;

impl MediaProbeBuilder for LocalFfmpegMediaProbeBuilder {
    fn probe_media(
        &self,
        payload: MediaProbeJobPayload,
    ) -> Result<MediaProbeJobResult, JobHandlerError> {
        let probe = qnc_media_ffmpeg::proxy::probe_media(&payload.media_path)
            .map_err(JobHandlerError::retryable)?;
        Ok(MediaProbeJobResult { probe })
    }
}

pub struct ExportHiResJobHandler<B: HiResExporter> {
    job_type: &'static str,
    exporter: B,
}

impl<B: HiResExporter> ExportHiResJobHandler<B> {
    pub fn new(exporter: B) -> Self {
        Self::for_job_type(EXPORT_HIRES_JOB_TYPE, exporter)
    }

    pub fn for_job_type(job_type: &'static str, exporter: B) -> Self {
        Self { job_type, exporter }
    }
}

impl<B> JobHandler for ExportHiResJobHandler<B>
where
    B: HiResExporter + 'static,
{
    fn job_type(&self) -> &'static str {
        self.job_type
    }

    fn run(
        &self,
        job: &JobLease,
        context: &JobExecutionContext<'_>,
    ) -> Result<Value, JobHandlerError> {
        let job_type = self.job_type;
        let payload: ExportHiResJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::fatal(format!("invalid {job_type} payload: {error}"))
            })?;
        let result = self
            .exporter
            .export_hires_with_context(payload, job, context)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid {job_type} result: {error}")))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFfmpegHiResRenderer;

impl HiResExporter for LocalFfmpegHiResRenderer {
    fn export_hires(
        &self,
        payload: ExportHiResJobPayload,
    ) -> Result<ExportHiResJobResult, JobHandlerError> {
        export_hires_reencode(payload)
    }
}

const EXPORT_HIRES_RENDER_MODE: &str = "reencode_source_reference_quality_a1_a2_mono";
const EXPORT_HIRES_VIDEO_PRESET: &str = "ultrafast";

#[derive(Debug, Clone)]
struct ExportRenderPreset {
    width: u32,
    height: u32,
    timebase: FrameTimebase,
    scan_mode: ScanMode,
    video_codec: String,
    video_profile: String,
    pixel_format: String,
    video_bitrate_bps: u64,
    audio_codec: String,
    audio_sample_rate_hz: u32,
    audio_bitrate_bps: Option<u64>,
    container_extension: String,
    gop_frames: u32,
}

#[derive(Debug, Clone, Copy)]
struct RenderItemSources<'a> {
    video: Option<&'a ExportHiResPlaylistSource>,
    a1: Option<&'a ExportHiResPlaylistSource>,
    a2: Option<&'a ExportHiResPlaylistSource>,
}

#[derive(Debug, Clone, Copy)]
struct AddedMediaInput<'a> {
    index: usize,
    source: &'a ExportHiResPlaylistSource,
}

fn export_hires_reencode(
    payload: ExportHiResJobPayload,
) -> Result<ExportHiResJobResult, JobHandlerError> {
    if payload.project_id.trim().is_empty() || payload.export_id.trim().is_empty() {
        return Err(JobHandlerError::fatal(
            "export_hires payload nema project_id/export_id",
        ));
    }
    if payload.items.is_empty() {
        return Err(JobHandlerError::fatal(
            "export_hires payload nema flat playlist iteme",
        ));
    }
    let output_path = payload.output_path.clone();
    let item_count = payload.items.len();
    let source_count = payload.items.iter().map(|item| item.sources.len()).sum();
    let output_parent = output_path.parent().ok_or_else(|| {
        JobHandlerError::fatal(format!(
            "export_hires output nema direktorij: {}",
            output_path.display()
        ))
    })?;
    fs::create_dir_all(output_parent).map_err(|error| {
        JobHandlerError::fatal(format!("export_hires output direktorij: {error}"))
    })?;

    let work_dir = output_parent
        .join(".qnc_export_work")
        .join(safe_path_token(&payload.export_id));
    fs::create_dir_all(&work_dir)
        .map_err(|error| JobHandlerError::fatal(format!("export_hires work dir: {error}")))?;

    let toolchain = qnc_media_ffmpeg::FfmpegToolchain::default();
    let preset = export_render_preset(&payload, &toolchain)?;
    validate_export_timebases(&payload, preset.timebase)?;

    let manifest_path = work_dir.join("manifest.json");
    let manifest = json!({
        "mode": EXPORT_HIRES_RENDER_MODE,
        "project_id": &payload.project_id,
        "export_id": &payload.export_id,
        "timeline_timebase": payload.timeline_timebase,
        "reference_timebase": preset.timebase,
        "reference_width": preset.width,
        "reference_height": preset.height,
        "reference_scan_mode": preset.scan_mode,
        "reference_video_codec": preset.video_codec,
        "reference_video_profile": preset.video_profile,
        "reference_pixel_format": preset.pixel_format,
        "reference_video_bitrate_bps": preset.video_bitrate_bps,
        "video_encoder_preset": EXPORT_HIRES_VIDEO_PRESET,
        "audio_layout": "A1 mono + A2 mono",
        "reference_audio_codec": preset.audio_codec,
        "audio_sample_rate_hz": preset.audio_sample_rate_hz,
        "reference_audio_bitrate_bps": preset.audio_bitrate_bps,
        "container_extension": preset.container_extension,
        "gop_frames": preset.gop_frames,
        "duration_frames": payload.duration_frames,
        "output_path": &output_path,
        "items": &payload.items,
    });
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| JobHandlerError::fatal(format!("export_hires manifest json: {error}")))?;
    fs::write(&manifest_path, manifest_json)
        .map_err(|error| JobHandlerError::fatal(format!("export_hires manifest write: {error}")))?;

    let mut rendered_items = Vec::with_capacity(payload.items.len());
    for (index, item) in payload.items.iter().enumerate() {
        rendered_items.push(render_export_item(
            item, index, &work_dir, &preset, &toolchain,
        )?);
    }

    let concat_path = work_dir.join("rendered_items.ffconcat");
    fs::write(&concat_path, ffconcat_files_text(&rendered_items)).map_err(|error| {
        JobHandlerError::fatal(format!("export_hires concat manifest write: {error}"))
    })?;
    let mut final_cmd = Command::new(toolchain.ffmpeg());
    final_cmd
        .arg("-hide_banner")
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&concat_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a:0")
        .arg("-map")
        .arg("0:a:1")
        .arg("-c")
        .arg("copy");
    append_container_args(&mut final_cmd, &preset.container_extension);
    let status = final_cmd
        .arg(&output_path)
        .output()
        .map_err(|error| JobHandlerError::retryable(format!("ffmpeg export_hires: {error}")))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(JobHandlerError::fatal(format!(
            "ffmpeg export_hires failed: {}",
            truncate_message(&stderr, 600)
        )));
    }
    let output_ok = output_path
        .metadata()
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    if !output_ok {
        return Err(JobHandlerError::fatal(format!(
            "export_hires output je prazan: {}",
            output_path.display()
        )));
    }

    Ok(ExportHiResJobResult {
        output_path,
        manifest_path: Some(manifest_path),
        item_count,
        source_count,
        mode: EXPORT_HIRES_RENDER_MODE.into(),
    })
}

fn export_render_preset(
    payload: &ExportHiResJobPayload,
    toolchain: &qnc_media_ffmpeg::FfmpegToolchain,
) -> Result<ExportRenderPreset, JobHandlerError> {
    let container_extension = export_container_extension(&payload.output_path)?;
    export_render_preset_for_container(payload, toolchain, container_extension)
}

fn export_render_preset_for_container(
    payload: &ExportHiResJobPayload,
    toolchain: &qnc_media_ffmpeg::FfmpegToolchain,
    container_extension: String,
) -> Result<ExportRenderPreset, JobHandlerError> {
    let reference = payload
        .items
        .iter()
        .flat_map(|item| item.sources.iter())
        .find(|source| source.has_video)
        .ok_or_else(|| JobHandlerError::fatal("Export HI-res nema video source za render."))?;
    if !reference.original_path.is_file() {
        return Err(JobHandlerError::fatal(format!(
            "Export HI-res original ne postoji: {}",
            reference.original_path.display()
        )));
    }
    let probe =
        qnc_media_ffmpeg::proxy::probe_media(&reference.original_path).map_err(|error| {
            JobHandlerError::fatal(format!(
                "Export HI-res ne može probati referentni original {}: {error}",
                reference.original_path.display()
            ))
        })?;
    let quality = probe_source_quality(&reference.original_path, toolchain)?;
    render_preset_from_probe(probe, quality, container_extension)
}

fn render_preset_from_probe(
    probe: MediaProbe,
    quality: SourceQualityProbe,
    container_extension: String,
) -> Result<ExportRenderPreset, JobHandlerError> {
    if !probe.has_video || probe.width == 0 || probe.height == 0 {
        return Err(JobHandlerError::fatal(
            "Export HI-res referentni original nema valjan video raster.",
        ));
    }
    validate_timebase(probe.timebase, "Export HI-res referentni original")?;
    if quality.pixel_format.trim().is_empty() {
        return Err(JobHandlerError::fatal(
            "Export HI-res referentni original nema probe pixel format.",
        ));
    }
    let video_bitrate_bps = quality.video_bitrate_bps.ok_or_else(|| {
        JobHandlerError::fatal(
            "Export HI-res referentni original nema probe video bitrate za source-quality export.",
        )
    })?;
    let audio_sample_rate_hz = quality.audio_sample_rate_hz.ok_or_else(|| {
        JobHandlerError::fatal(
            "Export HI-res referentni original nema probe audio sample-rate za source-quality export.",
        )
    })?;
    let audio_codec = export_audio_encoder(&quality.audio_codec)?;
    Ok(ExportRenderPreset {
        width: probe.width,
        height: probe.height,
        timebase: probe.timebase,
        scan_mode: probe.scan_mode,
        video_codec: quality.video_codec,
        video_profile: quality.video_profile,
        pixel_format: quality.pixel_format,
        video_bitrate_bps,
        audio_codec,
        audio_sample_rate_hz,
        audio_bitrate_bps: quality.audio_bitrate_bps,
        container_extension,
        gop_frames: gop_frames_for_mxf(probe.timebase),
    })
}

fn validate_export_timebases(
    payload: &ExportHiResJobPayload,
    reference_timebase: FrameTimebase,
) -> Result<(), JobHandlerError> {
    validate_timebase(payload.timeline_timebase, "Export HI-res timeline")?;
    if payload.timeline_timebase != reference_timebase {
        return Err(JobHandlerError::fatal(format!(
            "Export HI-res zasad ne miješa timebase: timeline {} != source {}.",
            fps_arg(payload.timeline_timebase),
            fps_arg(reference_timebase)
        )));
    }
    for source in payload.items.iter().flat_map(|item| item.sources.iter()) {
        if source.has_video || source.has_audio {
            validate_timebase(source.source_timebase, &source.source_id)?;
            if source.source_timebase != reference_timebase {
                return Err(JobHandlerError::fatal(format!(
                    "Export HI-res zasad ne miješa source timebase: {} ima {}, referenca je {}.",
                    source.source_id,
                    fps_arg(source.source_timebase),
                    fps_arg(reference_timebase)
                )));
            }
        }
    }
    Ok(())
}

fn validate_timebase(timebase: FrameTimebase, label: &str) -> Result<(), JobHandlerError> {
    if timebase.fps_num == 0 || timebase.fps_den == 0 {
        return Err(JobHandlerError::fatal(format!(
            "{label} nema probe timebase."
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceQualityProbe {
    video_codec: String,
    video_profile: String,
    pixel_format: String,
    video_bitrate_bps: Option<u64>,
    audio_codec: String,
    audio_sample_rate_hz: Option<u32>,
    audio_bitrate_bps: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FfprobeQualityReport {
    #[serde(default)]
    streams: Vec<FfprobeQualityStream>,
    #[serde(default)]
    format: FfprobeQualityFormat,
}

#[derive(Debug, Deserialize, Default)]
struct FfprobeQualityFormat {
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FfprobeQualityStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    pix_fmt: Option<String>,
    bit_rate: Option<String>,
    sample_rate: Option<String>,
}

fn probe_source_quality(
    source: &Path,
    toolchain: &qnc_media_ffmpeg::FfmpegToolchain,
) -> Result<SourceQualityProbe, JobHandlerError> {
    let output = Command::new(toolchain.ffprobe())
        .arg("-hide_banner")
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("stream=codec_type,codec_name,profile,pix_fmt,bit_rate,sample_rate")
        .arg("-show_entries")
        .arg("format=bit_rate")
        .arg("-of")
        .arg("json")
        .arg(source)
        .output()
        .map_err(|error| JobHandlerError::retryable(format!("ffprobe source quality: {error}")))?;
    if !output.status.success() {
        return Err(JobHandlerError::fatal(format!(
            "ffprobe source quality failed: {}",
            truncate_message(&String::from_utf8_lossy(&output.stderr), 600)
        )));
    }
    let report: FfprobeQualityReport = serde_json::from_slice(&output.stdout)
        .map_err(|error| JobHandlerError::fatal(format!("ffprobe source quality json: {error}")))?;
    source_quality_from_report(report)
}

fn source_quality_from_report(
    report: FfprobeQualityReport,
) -> Result<SourceQualityProbe, JobHandlerError> {
    let video = report.streams.iter().find(|stream| {
        stream
            .codec_type
            .as_deref()
            .map(|kind| kind.eq_ignore_ascii_case("video"))
            .unwrap_or(false)
    });
    let Some(video) = video else {
        return Err(JobHandlerError::fatal(
            "Export HI-res referentni original nema video stream za source-quality export.",
        ));
    };

    let audio_streams: Vec<&FfprobeQualityStream> = report
        .streams
        .iter()
        .filter(|stream| {
            stream
                .codec_type
                .as_deref()
                .map(|kind| kind.eq_ignore_ascii_case("audio"))
                .unwrap_or(false)
        })
        .collect();
    let audio = audio_streams.first().copied();
    let audio_total_bitrate = audio_streams
        .iter()
        .filter_map(|stream| stream.bit_rate.as_deref().and_then(parse_u64_probe_value))
        .sum::<u64>();
    let container_bitrate = report
        .format
        .bit_rate
        .as_deref()
        .and_then(parse_u64_probe_value);
    let video_bitrate = video
        .bit_rate
        .as_deref()
        .and_then(parse_u64_probe_value)
        .or_else(|| {
            container_bitrate.and_then(|bitrate| {
                bitrate
                    .checked_sub(audio_total_bitrate)
                    .filter(|value| *value > 0)
            })
        });

    Ok(SourceQualityProbe {
        video_codec: video.codec_name.as_deref().unwrap_or("").trim().to_string(),
        video_profile: video.profile.as_deref().unwrap_or("").trim().to_string(),
        pixel_format: video.pix_fmt.as_deref().unwrap_or("").trim().to_string(),
        video_bitrate_bps: video_bitrate,
        audio_codec: audio
            .and_then(|stream| stream.codec_name.as_deref())
            .unwrap_or("")
            .trim()
            .to_string(),
        audio_sample_rate_hz: audio.and_then(|stream| {
            stream
                .sample_rate
                .as_deref()
                .and_then(parse_u64_probe_value)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
        }),
        audio_bitrate_bps: audio.and_then(|stream| {
            stream
                .bit_rate
                .as_deref()
                .and_then(parse_u64_probe_value)
                .filter(|value| *value > 0)
        }),
    })
}

fn parse_u64_probe_value(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

fn video_encoder(source_codec: &str) -> &'static str {
    match source_codec.trim().to_ascii_lowercase().as_str() {
        "h264" | "avc1" => "libx264",
        _ => "libx264",
    }
}

fn append_video_quality_args(cmd: &mut Command, preset: &ExportRenderPreset) {
    if let Some(profile) = x264_profile_arg(&preset.video_profile, &preset.pixel_format) {
        cmd.arg("-profile:v").arg(profile);
    }
    let bitrate = bitrate_arg(preset.video_bitrate_bps);
    let bufsize = bitrate_arg(preset.video_bitrate_bps.saturating_mul(2));
    cmd.arg("-b:v")
        .arg(&bitrate)
        .arg("-maxrate")
        .arg(&bitrate)
        .arg("-bufsize")
        .arg(bufsize);
    if preset.container_extension == "mxf" {
        let gop = preset.gop_frames.to_string();
        cmd.arg("-g").arg(&gop).arg("-keyint_min").arg(gop);
    }
}

fn x264_profile_arg(source_profile: &str, pixel_format: &str) -> Option<&'static str> {
    let profile = source_profile.trim().to_ascii_lowercase();
    let pixel = pixel_format.trim().to_ascii_lowercase();
    if profile.contains("4:2:2") || profile.contains("422") || pixel.contains("422") {
        Some("high422")
    } else if profile.contains("high 10") || profile.contains("high10") || pixel.contains("10") {
        Some("high10")
    } else if profile.contains("high") {
        Some("high")
    } else if profile.contains("main") {
        Some("main")
    } else if profile.contains("baseline") {
        Some("baseline")
    } else {
        None
    }
}

fn export_audio_encoder(source_codec: &str) -> Result<String, JobHandlerError> {
    let codec = source_codec.trim().to_ascii_lowercase();
    match codec.as_str() {
        "pcm_s24le" | "pcm_s16le" | "pcm_s32le" | "pcm_f32le" | "aac" => Ok(codec),
        "" => Err(JobHandlerError::fatal(
            "Export HI-res referentni original nema probe audio codec.",
        )),
        _ => Err(JobHandlerError::fatal(format!(
            "Export HI-res source-quality audio codec još nije podržan: {source_codec}"
        ))),
    }
}

fn append_audio_quality_args(cmd: &mut Command, preset: &ExportRenderPreset) {
    if preset.audio_codec == "aac" {
        if let Some(bit_rate) = preset.audio_bitrate_bps {
            cmd.arg("-b:a").arg(bitrate_arg(bit_rate));
        }
    }
}

fn bitrate_arg(bit_rate_bps: u64) -> String {
    let kbps = ((bit_rate_bps.saturating_add(999)) / 1000).max(1);
    format!("{kbps}k")
}

fn gop_frames_for_mxf(timebase: FrameTimebase) -> u32 {
    if timebase.fps_num == 0 || timebase.fps_den == 0 {
        return 50;
    }
    let rounded = (timebase.fps_num + (timebase.fps_den / 2)) / timebase.fps_den;
    rounded.clamp(1, 120)
}

fn export_container_extension(output_path: &Path) -> Result<String, JobHandlerError> {
    let extension = output_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            JobHandlerError::fatal(format!(
                "Export HI-res output nema source container ekstenziju: {}",
                output_path.display()
            ))
        })?;
    match extension.as_str() {
        "mxf" | "mov" | "mp4" => Ok(extension),
        other => Err(JobHandlerError::fatal(format!(
            "Export HI-res output container nije podržan: .{other}"
        ))),
    }
}

fn append_container_args(cmd: &mut Command, extension: &str) {
    match extension.trim().to_ascii_lowercase().as_str() {
        "mov" | "mp4" => {
            cmd.arg("-movflags")
                .arg("+faststart")
                .arg("-avoid_negative_ts")
                .arg("make_zero");
        }
        "mxf" => {
            cmd.arg("-avoid_negative_ts").arg("make_zero");
        }
        _ => {}
    }
}

fn render_export_item(
    item: &ExportHiResPlaylistItem,
    index: usize,
    work_dir: &Path,
    preset: &ExportRenderPreset,
    toolchain: &qnc_media_ffmpeg::FfmpegToolchain,
) -> Result<PathBuf, JobHandlerError> {
    if item.record_out_frame <= item.record_in_frame {
        return Err(JobHandlerError::fatal(format!(
            "export_hires item nema valjan record raspon: {}",
            item.item_id
        )));
    }
    let duration_frames = item.record_out_frame - item.record_in_frame;
    let duration = frame_position(duration_frames, preset.timebase);
    let sources = render_item_sources(item)?;
    validate_item_source(sources.video, duration_frames, "video")?;
    validate_item_source(sources.a1, duration_frames, "A1")?;
    validate_item_source(sources.a2, duration_frames, "A2")?;

    let mut cmd = Command::new(toolchain.ffmpeg());
    cmd.arg("-hide_banner").arg("-y");
    let mut input_index = 0usize;
    let mut media_inputs = Vec::new();
    let video_input = if let Some(source) = sources.video {
        add_or_reuse_source_input(
            &mut cmd,
            &mut media_inputs,
            &mut input_index,
            source,
            &duration,
        )
    } else {
        let index = input_index;
        cmd.arg("-f")
            .arg("lavfi")
            .arg("-t")
            .arg(&duration)
            .arg("-i")
            .arg(format!(
                "color=c=black:s={}x{}:r={}:d={}",
                preset.width,
                preset.height,
                fps_arg(preset.timebase),
                duration
            ));
        input_index += 1;
        index
    };
    let a1_input = if let Some(source) = sources.a1 {
        Some(add_or_reuse_source_input(
            &mut cmd,
            &mut media_inputs,
            &mut input_index,
            source,
            &duration,
        ))
    } else {
        None
    };
    let a2_input = if let Some(source) = sources.a2 {
        Some(add_or_reuse_source_input(
            &mut cmd,
            &mut media_inputs,
            &mut input_index,
            source,
            &duration,
        ))
    } else {
        None
    };

    let item_path = work_dir.join(format!("item_{index:05}.{}", preset.container_extension));
    cmd.arg("-filter_complex")
        .arg(export_item_filter(
            video_input,
            a1_input,
            a2_input,
            &duration,
            preset,
        ))
        .arg("-map")
        .arg("[vout]")
        .arg("-map")
        .arg("[a1out]")
        .arg("-map")
        .arg("[a2out]")
        .arg("-r")
        .arg(fps_arg(preset.timebase))
        .arg("-fps_mode")
        .arg("cfr")
        .arg("-c:v")
        .arg(video_encoder(&preset.video_codec))
        .arg("-preset")
        .arg(EXPORT_HIRES_VIDEO_PRESET);
    append_video_quality_args(&mut cmd, preset);
    cmd.arg("-pix_fmt")
        .arg(&preset.pixel_format)
        .arg("-c:a")
        .arg(&preset.audio_codec);
    append_audio_quality_args(&mut cmd, preset);
    cmd.arg("-ar:a")
        .arg(preset.audio_sample_rate_hz.to_string())
        .arg("-ac:a")
        .arg("1")
        .arg("-metadata:s:a:0")
        .arg("title=A1")
        .arg("-metadata:s:a:1")
        .arg("title=A2");
    append_scan_mode_args(&mut cmd, preset.scan_mode);
    cmd.arg("-t").arg(&duration).arg(&item_path);

    let status = cmd.output().map_err(|error| {
        JobHandlerError::retryable(format!(
            "ffmpeg export_hires item {}: {error}",
            item.item_id
        ))
    })?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(JobHandlerError::fatal(format!(
            "ffmpeg export_hires item {} failed: {}",
            item.item_id,
            truncate_message(&stderr, 600)
        )));
    }
    let output_ok = item_path
        .metadata()
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    if !output_ok {
        return Err(JobHandlerError::fatal(format!(
            "export_hires item output je prazan: {}",
            item_path.display()
        )));
    }
    Ok(item_path)
}

fn render_item_sources(
    item: &ExportHiResPlaylistItem,
) -> Result<RenderItemSources<'_>, JobHandlerError> {
    let video = item.sources.iter().filter(|source| source.has_video).last();
    let a1 = item
        .sources
        .iter()
        .filter(|source| source.has_audio && source.audio_output_channel.unwrap_or(0) == 0)
        .last();
    let a2 = item
        .sources
        .iter()
        .filter(|source| source.has_audio && source.audio_output_channel == Some(1))
        .last();
    if video.is_none() && a1.is_none() && a2.is_none() {
        return Err(JobHandlerError::fatal(format!(
            "export_hires item nema aktivni source: {}",
            item.item_id
        )));
    }
    Ok(RenderItemSources { video, a1, a2 })
}

fn validate_item_source(
    source: Option<&ExportHiResPlaylistSource>,
    duration_frames: i64,
    label: &str,
) -> Result<(), JobHandlerError> {
    let Some(source) = source else {
        return Ok(());
    };
    if source.source_out_frame <= source.source_in_frame {
        return Err(JobHandlerError::fatal(format!(
            "export_hires {label} source nema valjan frame raspon: {}",
            source.source_id
        )));
    }
    if source.source_out_frame - source.source_in_frame < duration_frames {
        return Err(JobHandlerError::fatal(format!(
            "export_hires {label} source je kraći od flat itema: {}",
            source.source_id
        )));
    }
    if !source.original_path.is_file() {
        return Err(JobHandlerError::fatal(format!(
            "export_hires original ne postoji: {}",
            source.original_path.display()
        )));
    }
    Ok(())
}

fn add_source_input(cmd: &mut Command, source: &ExportHiResPlaylistSource, duration: &str) {
    cmd.arg("-ss")
        .arg(frame_position(
            source.source_in_frame,
            source.source_timebase,
        ))
        .arg("-t")
        .arg(duration)
        .arg("-i")
        .arg(&source.original_path);
}

fn add_or_reuse_source_input<'a>(
    cmd: &mut Command,
    media_inputs: &mut Vec<AddedMediaInput<'a>>,
    next_input_index: &mut usize,
    source: &'a ExportHiResPlaylistSource,
    duration: &str,
) -> usize {
    if let Some(existing) = media_inputs
        .iter()
        .find(|existing| same_media_input(existing.source, source))
    {
        return existing.index;
    }
    let index = *next_input_index;
    add_source_input(cmd, source, duration);
    *next_input_index += 1;
    media_inputs.push(AddedMediaInput { index, source });
    index
}

fn same_media_input(a: &ExportHiResPlaylistSource, b: &ExportHiResPlaylistSource) -> bool {
    a.original_path == b.original_path
        && a.source_in_frame == b.source_in_frame
        && a.source_timebase == b.source_timebase
}

fn export_item_filter(
    video_input: usize,
    a1_input: Option<usize>,
    a2_input: Option<usize>,
    duration: &str,
    preset: &ExportRenderPreset,
) -> String {
    let scale_filter = scale_filter(preset);
    let mut filters = vec![format!(
        "[{video_input}:v:0]trim=duration={duration},setpts=PTS-STARTPTS,{scale_filter},pad={}:{}:(ow-iw)/2:(oh-ih)/2,setsar=1,format={}[vout]",
        preset.width, preset.height, preset.pixel_format
    )];
    filters.push(mono_audio_filter(
        a1_input,
        "a1out",
        duration,
        preset.audio_sample_rate_hz,
    ));
    filters.push(mono_audio_filter(
        a2_input,
        "a2out",
        duration,
        preset.audio_sample_rate_hz,
    ));
    filters.join(";")
}

fn scale_filter(preset: &ExportRenderPreset) -> String {
    if matches!(preset.scan_mode, ScanMode::Progressive | ScanMode::Unknown) {
        format!(
            "scale={}:{}:force_original_aspect_ratio=decrease",
            preset.width, preset.height
        )
    } else {
        format!(
            "scale={}:{}:force_original_aspect_ratio=decrease:interl=1",
            preset.width, preset.height
        )
    }
}

fn mono_audio_filter(
    input: Option<usize>,
    label: &str,
    duration: &str,
    sample_rate_hz: u32,
) -> String {
    if let Some(input) = input {
        return format!(
            "[{input}:a:0]atrim=duration={duration},asetpts=PTS-STARTPTS,aresample={sample_rate_hz},pan=mono|c0=c0[{label}]"
        );
    }
    format!("anullsrc=channel_layout=mono:sample_rate={sample_rate_hz}:d={duration}[{label}]")
}

fn append_scan_mode_args(cmd: &mut Command, scan_mode: ScanMode) {
    match scan_mode {
        ScanMode::InterlacedTopFieldFirst => {
            cmd.arg("-flags")
                .arg("+ildct+ilme")
                .arg("-top")
                .arg("1")
                .arg("-x264-params")
                .arg("tff=1");
        }
        ScanMode::InterlacedBottomFieldFirst => {
            cmd.arg("-flags")
                .arg("+ildct+ilme")
                .arg("-top")
                .arg("0")
                .arg("-x264-params")
                .arg("bff=1");
        }
        ScanMode::Progressive | ScanMode::Unknown => {}
    }
}

fn ffconcat_files_text(paths: &[PathBuf]) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    for path in paths {
        out.push_str("file '");
        out.push_str(&escape_ffconcat_path(path));
        out.push_str("'\n");
    }
    out
}

fn fps_arg(timebase: FrameTimebase) -> String {
    if timebase.fps_den == 1 {
        timebase.fps_num.to_string()
    } else {
        format!("{}/{}", timebase.fps_num, timebase.fps_den)
    }
}

fn frame_position(frame: i64, timebase: FrameTimebase) -> String {
    let num = frame.max(0) as i128 * timebase.fps_den as i128;
    let den = timebase.fps_num.max(1) as i128;
    let whole = num / den;
    let rem = num % den;
    let frac = rem * 1_000_000_000 / den;
    format!("{whole}.{frac:09}")
}

fn escape_ffconcat_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "'\\''")
}

fn safe_path_token(value: &str) -> String {
    let mut out: String = value
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out = "export".into();
    }
    out
}

fn truncate_message(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    format!("{}...", trimmed.chars().take(max_chars).collect::<String>())
}

fn normalize_job_type(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeHost {
        claims: Mutex<VecDeque<JobClaimResponse>>,
        claim_requests: Mutex<Vec<JobClaimRequest>>,
        heartbeats: Mutex<Vec<JobHeartbeatRequest>>,
        completes: Mutex<Vec<JobCompleteRequest>>,
        fails: Mutex<Vec<JobFailRequest>>,
    }

    impl FakeHost {
        fn with_claim(response: JobClaimResponse) -> Self {
            let mut claims = VecDeque::new();
            claims.push_back(response);
            Self {
                claims: Mutex::new(claims),
                ..Self::default()
            }
        }
    }

    impl WorkerHost for FakeHost {
        fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String> {
            self.claim_requests.lock().unwrap().push(request.clone());
            self.claims
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "no fake claim response".to_string())
        }

        fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String> {
            self.heartbeats.lock().unwrap().push(request.clone());
            Ok(JobHeartbeatResponse {
                accepted: request.job_ids.clone(),
                rejected: Vec::new(),
                lease_until_unix_ms: 123,
            })
        }

        fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String> {
            self.completes.lock().unwrap().push(request.clone());
            Ok(JobAck {
                accepted: true,
                job_id: request.job_id.clone(),
                message: None,
            })
        }

        fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String> {
            self.fails.lock().unwrap().push(request.clone());
            Ok(JobAck {
                accepted: true,
                job_id: request.job_id.clone(),
                message: None,
            })
        }
    }

    fn fake_execution_context(host: &FakeHost) -> JobExecutionContext<'_> {
        JobExecutionContext { host }
    }

    fn smoke_lease() -> JobLease {
        JobLease {
            job_id: "qnc_worker_smoke:worker:clip_a".into(),
            project_id: "project_a".into(),
            job_type: SMOKE_JOB_TYPE.into(),
            source_id: "worker".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: Some("epoch_1".into()),
            payload: json!({}),
        }
    }

    fn export_source(
        path: PathBuf,
        source_id: &str,
        has_video: bool,
        has_audio: bool,
        audio_output_channel: Option<u8>,
    ) -> ExportHiResPlaylistSource {
        ExportHiResPlaylistSource {
            source_id: source_id.into(),
            source_kind: source_id.into(),
            clip_id: "clip_a".into(),
            virtual_shot_id: "shot_a".into(),
            original_path: path,
            source_in_frame: 10,
            source_out_frame: 60,
            source_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            has_video,
            has_audio,
            audio_output_channel,
        }
    }

    struct FakeProxyBuilder;

    impl ProxyBuilder for FakeProxyBuilder {
        fn build_proxy(
            &self,
            payload: ProxyGenerateJobPayload,
        ) -> Result<ProxyGenerateJobResult, JobHandlerError> {
            Ok(ProxyGenerateJobResult {
                output_path: payload.output_path,
                probe: None,
            })
        }
    }

    struct FakeFilmstripBuilder;

    impl FilmstripBuilder for FakeFilmstripBuilder {
        fn build_filmstrip(
            &self,
            payload: FilmstripJobPayload,
        ) -> Result<FilmstripJobResult, JobHandlerError> {
            Ok(FilmstripJobResult {
                duration_sec: payload.duration_sec,
                frames: payload
                    .frames
                    .into_iter()
                    .map(|frame| qnc_service_contracts::FilmstripFrameArtifact {
                        index: frame.index,
                        seek_sec: frame.seek_sec,
                        artifact: qnc_service_contracts::ArtifactRef {
                            path: frame.output_path,
                            media_type: "image/jpeg".into(),
                            render_version: None,
                        },
                    })
                    .collect(),
            })
        }
    }

    struct FakeWaveformBuilder;

    impl WaveformBuilder for FakeWaveformBuilder {
        fn build_waveform(
            &self,
            payload: WaveformJobPayload,
        ) -> Result<WaveformJobResult, JobHandlerError> {
            Ok(WaveformJobResult {
                a1_peaks: vec![1.0; payload.peak_buckets.min(3)],
                a2_peaks: vec![0.5],
                warning: None,
            })
        }
    }

    struct FakePosterBuilder;

    impl PosterBuilder for FakePosterBuilder {
        fn build_poster(
            &self,
            payload: PosterJobPayload,
        ) -> Result<PosterJobResult, JobHandlerError> {
            Ok(PosterJobResult {
                output_path: payload.output_path,
            })
        }
    }

    struct FakeAudioWrapBuilder;

    impl AudioWrapBuilder for FakeAudioWrapBuilder {
        fn build_audio_wrap(
            &self,
            payload: AudioWrapJobPayload,
        ) -> Result<AudioWrapJobResult, JobHandlerError> {
            Ok(AudioWrapJobResult {
                wraps: payload
                    .wraps
                    .into_iter()
                    .map(|item| AudioWrapJobArtifact {
                        fps: item.fps,
                        output_path: item.output_path,
                        probe: None,
                    })
                    .collect(),
            })
        }
    }

    struct FakeMediaProbeBuilder;

    impl MediaProbeBuilder for FakeMediaProbeBuilder {
        fn probe_media(
            &self,
            _payload: MediaProbeJobPayload,
        ) -> Result<MediaProbeJobResult, JobHandlerError> {
            Ok(MediaProbeJobResult {
                probe: qnc_service_contracts::MediaProbe {
                    width: 1920,
                    height: 1080,
                    duration_sec: Some(12.5),
                    timebase: qnc_service_contracts::FrameTimebase {
                        fps_num: 50,
                        fps_den: 1,
                    },
                    scan_mode: qnc_service_contracts::ScanMode::Progressive,
                    codec: "h264".into(),
                    field_order: "progressive".into(),
                    frame_count: Some(625),
                    duration_frames: Some(625),
                    has_video: true,
                    has_audio: true,
                    audio_channels: 2,
                },
            })
        }
    }

    struct SlowJobHandler;

    impl JobHandler for SlowJobHandler {
        fn job_type(&self) -> &'static str {
            "slow_job"
        }

        fn run(
            &self,
            job: &JobLease,
            _context: &JobExecutionContext<'_>,
        ) -> Result<Value, JobHandlerError> {
            thread::sleep(Duration::from_millis(700));
            Ok(json!({
                "status": "ok",
                "job_type": job.job_type,
            }))
        }
    }

    #[test]
    fn export_hires_selects_cover_video_and_separate_mono_audio_sources() {
        let base = PathBuf::from("C:/media/base.mxf");
        let cover = PathBuf::from("C:/media/cover.mxf");
        let item = ExportHiResPlaylistItem {
            item_id: "item:0-50".into(),
            record_in_frame: 0,
            record_out_frame: 50,
            sources: vec![
                export_source(base.clone(), "base_video", true, false, None),
                export_source(base, "base_a1", false, true, Some(0)),
                export_source(cover, "cover", true, true, Some(1)),
            ],
        };

        let sources = render_item_sources(&item).unwrap();

        assert_eq!(sources.video.unwrap().source_id, "cover");
        assert_eq!(sources.a1.unwrap().source_id, "base_a1");
        assert_eq!(sources.a2.unwrap().source_id, "cover");
    }

    #[test]
    fn export_hires_filter_outputs_a1_and_a2_as_separate_mono_streams() {
        let preset = ExportRenderPreset {
            width: 1920,
            height: 1080,
            timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            scan_mode: ScanMode::Progressive,
            video_codec: "h264".into(),
            video_profile: "High 4:2:2".into(),
            pixel_format: "yuv422p10le".into(),
            video_bitrate_bps: 56_000_000,
            audio_codec: "pcm_s24le".into(),
            audio_sample_rate_hz: 48_000,
            audio_bitrate_bps: Some(1_152_000),
            container_extension: "mxf".into(),
            gop_frames: 50,
        };
        let filter = export_item_filter(0, Some(1), None, "1.000000000", &preset);

        assert!(filter.contains("[a1out]"));
        assert!(filter.contains("[a2out]"));
        assert!(filter.contains("pan=mono|c0=c0[a1out]"));
        assert!(filter.contains("format=yuv422p10le[vout]"));
        assert!(filter.contains("anullsrc=channel_layout=mono:sample_rate=48000"));
        assert!(!filter.contains("amerge"));
        assert!(!filter.contains("stereo"));
    }

    #[test]
    fn export_hires_reuses_same_media_input_for_video_and_audio() {
        let path = PathBuf::from("C:/card/ClipA.MXF");
        let video = export_source(path.clone(), "base_video", true, false, None);
        let audio = export_source(path, "base_audio", false, true, Some(0));

        assert!(same_media_input(&video, &audio));
    }

    #[test]
    fn export_hires_uses_deadline_encoder_preset() {
        assert_eq!(EXPORT_HIRES_VIDEO_PRESET, "ultrafast");
    }

    #[test]
    fn export_hires_quality_probe_derives_video_bitrate_from_container() {
        let report: FfprobeQualityReport = serde_json::from_value(json!({
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "profile": "High 4:2:2",
                    "pix_fmt": "yuv422p10le"
                },
                {
                    "codec_type": "audio",
                    "codec_name": "pcm_s24le",
                    "sample_rate": "48000",
                    "bit_rate": "1152000"
                },
                {
                    "codec_type": "audio",
                    "codec_name": "pcm_s24le",
                    "sample_rate": "48000",
                    "bit_rate": "1152000"
                }
            ],
            "format": { "bit_rate": "61245026" }
        }))
        .unwrap();

        let quality = source_quality_from_report(report).unwrap();

        assert_eq!(quality.video_codec, "h264");
        assert_eq!(quality.video_profile, "High 4:2:2");
        assert_eq!(quality.pixel_format, "yuv422p10le");
        assert_eq!(quality.video_bitrate_bps, Some(58_941_026));
        assert_eq!(quality.audio_codec, "pcm_s24le");
        assert_eq!(quality.audio_sample_rate_hz, Some(48_000));
        assert_eq!(quality.audio_bitrate_bps, Some(1_152_000));
    }

    #[test]
    fn export_hires_container_extension_follows_output_path() {
        assert_eq!(
            export_container_extension(Path::new("C:/exports/story.MXF")).unwrap(),
            "mxf"
        );
        assert_eq!(
            export_container_extension(Path::new("C:/exports/story.mov")).unwrap(),
            "mov"
        );
        assert!(export_container_extension(Path::new("C:/exports/story")).is_err());
    }

    #[test]
    fn export_hires_mxf_gop_tracks_source_fps() {
        assert_eq!(
            gop_frames_for_mxf(FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            }),
            50
        );
        assert_eq!(
            gop_frames_for_mxf(FrameTimebase {
                fps_num: 60000,
                fps_den: 1001,
            }),
            60
        );
    }

    #[test]
    fn export_hires_frame_position_uses_exact_timebase() {
        assert_eq!(
            frame_position(
                50,
                FrameTimebase {
                    fps_num: 50,
                    fps_den: 1,
                },
            ),
            "1.000000000"
        );
        assert_eq!(
            frame_position(
                30,
                FrameTimebase {
                    fps_num: 30000,
                    fps_den: 1001,
                },
            ),
            "1.001000000"
        );
    }

    #[test]
    fn requested_capabilities_are_limited_to_registered_handlers() {
        let config = WorkerConfig::new(
            "worker_a",
            vec!["proxy_generate".into(), SMOKE_JOB_TYPE.into()],
            DEFAULT_POLL_MS,
            DEFAULT_LEASE_MS,
        );
        let caps = config.claim_capabilities(&HandlerRegistry::with_builtin_handlers());
        assert_eq!(
            caps,
            vec![
                PROXY_GENERATE_JOB_TYPE.to_string(),
                SMOKE_JOB_TYPE.to_string()
            ]
        );
    }

    #[test]
    fn run_once_does_not_call_host_without_executable_capability() {
        let config = WorkerConfig::new(
            "worker_a",
            vec!["unknown_job_type".into()],
            DEFAULT_POLL_MS,
            DEFAULT_LEASE_MS,
        );
        let host = FakeHost::default();
        let worker = Worker::new(config, host, HandlerRegistry::with_builtin_handlers());

        let tick = worker.run_once().unwrap();
        assert_eq!(tick, WorkerTick::idle("no_executable_capabilities"));
        assert!(worker.host.claim_requests.lock().unwrap().is_empty());
    }

    #[test]
    fn run_once_heartbeats_and_completes_smoke_job() {
        let host = FakeHost::with_claim(JobClaimResponse {
            jobs: vec![smoke_lease()],
            playback_active: false,
            message: None,
        });
        let config = WorkerConfig::new("worker_a", vec![], DEFAULT_POLL_MS, DEFAULT_LEASE_MS);
        let worker = Worker::new(config, host, HandlerRegistry::with_builtin_handlers());

        let tick = worker.run_once().unwrap();
        assert_eq!(
            tick,
            WorkerTick {
                playback_active: false,
                claimed: 1,
                completed: 1,
                failed: 0,
                idle_reason: None,
            }
        );
        assert_eq!(worker.host.heartbeats.lock().unwrap().len(), 1);
        let claims = worker.host.claim_requests.lock().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].placement, Some(WorkerPlacement::LocalWorkstation));
        let completes = worker.host.completes.lock().unwrap();
        assert_eq!(completes.len(), 1);
        assert_eq!(completes[0].job_id, "qnc_worker_smoke:worker:clip_a");
        assert_eq!(completes[0].result["status"], "ok");
    }

    #[test]
    fn run_once_renews_heartbeat_while_job_runs() {
        let host = FakeHost::with_claim(JobClaimResponse {
            jobs: vec![JobLease {
                job_id: "slow_job:worker:clip_a".into(),
                project_id: "project_a".into(),
                job_type: "slow_job".into(),
                source_id: "worker".into(),
                clip_id: "clip_a".into(),
                worker_id: "worker_a".into(),
                lease_id: "lease_a".into(),
                lease_until_unix_ms: 123,
                attempts: 1,
                queued_at: None,
                payload: json!({}),
            }],
            playback_active: false,
            message: None,
        });
        let mut registry = HandlerRegistry::empty();
        registry.register(SlowJobHandler);
        let config = WorkerConfig {
            worker_id: "worker_a".into(),
            placement: WorkerPlacement::LocalWorkstation,
            requested_capabilities: vec!["slow_job".into()],
            poll_interval: Duration::from_millis(50),
            lease_ms: 600,
        };
        let worker = Worker::new(config, host, registry);

        let tick = worker.run_once().unwrap();

        assert_eq!(tick.completed, 1);
        assert!(
            worker.host.heartbeats.lock().unwrap().len() >= 2,
            "long jobs must renew the lease while the handler is running"
        );
    }

    #[test]
    fn proxy_generate_handler_roundtrips_payload_and_result() {
        let handler = ProxyGenerateJobHandler::new(FakeProxyBuilder);
        let output_path = PathBuf::from("C:/qnc/project/proxy/clip_a.mxf");
        let payload = ProxyGenerateJobPayload {
            source_path: PathBuf::from("C:/card/ClipA.MXF"),
            output_path: output_path.clone(),
            asset_status: "ready".into(),
            card_locked: false,
            original_path: Some(PathBuf::from("C:/card/ClipA.MXF")),
        };
        let job = JobLease {
            job_id: "proxy_generate:card:clip_a".into(),
            project_id: "project_a".into(),
            job_type: PROXY_GENERATE_JOB_TYPE.into(),
            source_id: "card".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: ProxyGenerateJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.output_path, output_path);
        assert!(decoded.probe.is_none());
    }

    #[test]
    fn filmstrip_handler_roundtrips_payload_and_result() {
        let handler = FilmstripJobHandler::new(FakeFilmstripBuilder);
        let output_path = PathBuf::from("C:/qnc/project/filmstrip/clip_a/000_0_00.jpg");
        let payload = FilmstripJobPayload {
            media_path: PathBuf::from("C:/qnc/project/proxy/clip_a.mp4"),
            duration_sec: 13.0,
            frames: vec![qnc_service_contracts::FilmstripJobFrame {
                index: 0,
                seek_sec: 0.0,
                output_path: output_path.clone(),
            }],
        };
        let job = JobLease {
            job_id: "filmstrip:filmstrip:clip_a".into(),
            project_id: "project_a".into(),
            job_type: FILMSTRIP_JOB_TYPE.into(),
            source_id: "filmstrip".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: FilmstripJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.duration_sec, 13.0);
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].artifact.path, output_path);
    }

    #[test]
    fn waveform_handler_roundtrips_payload_and_result() {
        let handler = WaveformJobHandler::new(FakeWaveformBuilder);
        let payload = WaveformJobPayload {
            media_path: PathBuf::from("C:/qnc/project/proxy/clip_a.mp4"),
            peak_buckets: 3,
            sample_rate_hz: 8_000,
        };
        let job = JobLease {
            job_id: "waveform:waveform:clip_a".into(),
            project_id: "project_a".into(),
            job_type: WAVEFORM_JOB_TYPE.into(),
            source_id: "waveform".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: WaveformJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.a1_peaks, vec![1.0, 1.0, 1.0]);
        assert_eq!(decoded.a2_peaks, vec![0.5]);
        assert!(decoded.warning.is_none());
    }

    #[test]
    fn poster_handler_roundtrips_payload_and_result() {
        let handler = PosterJobHandler::new(FakePosterBuilder);
        let output_path = PathBuf::from("C:/qnc/project/ingest/thumbnails/clip_a/poster.jpg");
        let payload = PosterJobPayload {
            media_path: PathBuf::from("C:/qnc/project/proxy/clip_a.mp4"),
            output_path: output_path.clone(),
            seek_sec: 0.0,
        };
        let job = JobLease {
            job_id: "thumb_proxy:card:clip_a".into(),
            project_id: "project_a".into(),
            job_type: THUMB_PROXY_JOB_TYPE.into(),
            source_id: "card".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: PosterJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.output_path, output_path);
    }

    #[test]
    fn audio_wrap_handler_roundtrips_payload_and_result() {
        let handler = AudioWrapJobHandler::new(FakeAudioWrapBuilder);
        let output_path = PathBuf::from("C:/qnc/project/proxy/vo_a_50.mp4");
        let payload = AudioWrapJobPayload {
            media_path: PathBuf::from("C:/qnc/project/audio/vo_a.wav"),
            wraps: vec![qnc_service_contracts::AudioWrapJobItem {
                fps: 50.0,
                output_path: output_path.clone(),
            }],
        };
        let job = JobLease {
            job_id: "audio_wrap:voice:vo_a".into(),
            project_id: "project_a".into(),
            job_type: AUDIO_WRAP_JOB_TYPE.into(),
            source_id: "voice".into(),
            clip_id: "vo_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: AudioWrapJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.wraps.len(), 1);
        assert_eq!(decoded.wraps[0].fps, 50.0);
        assert_eq!(decoded.wraps[0].output_path, output_path);
    }

    #[test]
    fn media_probe_handler_roundtrips_payload_and_result() {
        let handler = MediaProbeJobHandler::new(FakeMediaProbeBuilder);
        let payload = MediaProbeJobPayload {
            media_path: PathBuf::from("C:/qnc/project/proxy/clip_a.mp4"),
        };
        let job = JobLease {
            job_id: "media_probe:card:clip_a".into(),
            project_id: "project_a".into(),
            job_type: MEDIA_PROBE_JOB_TYPE.into(),
            source_id: "card".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let host = FakeHost::default();
        let context = fake_execution_context(&host);
        let result = handler.run(&job, &context).unwrap();
        let decoded: MediaProbeJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.probe.width, 1920);
        assert_eq!(decoded.probe.timebase.fps_num, 50);
        assert_eq!(decoded.probe.duration_sec, Some(12.5));
    }
}
