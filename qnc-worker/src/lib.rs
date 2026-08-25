#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use qnc_service_contracts::{
    AudioWrapJobArtifact, AudioWrapJobPayload, AudioWrapJobResult, FilmstripJobPayload,
    FilmstripJobResult, JobAck, JobClaimRequest, JobClaimResponse, JobCompleteRequest,
    JobFailRequest, JobHeartbeatRequest, JobHeartbeatResponse, JobLease, MediaProbeJobPayload,
    MediaProbeJobResult, PosterJobPayload, PosterJobResult, ProxyGenerateJobPayload,
    ProxyGenerateJobResult, WaveformJobPayload, WaveformJobResult, WorkerPlacement,
    JOB_TYPE_AUDIO_WRAP, JOB_TYPE_FILMSTRIP, JOB_TYPE_MEDIA_PROBE, JOB_TYPE_THUMB_PROXY,
    JOB_TYPE_WAVEFORM,
};
use serde::{de::DeserializeOwned, Serialize};
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

pub trait JobHandler: Send + Sync {
    fn job_type(&self) -> &'static str;
    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError>;
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
            let result = handler.run(job);
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

pub fn detect_worker_placement(host_url: &str) -> WorkerPlacement {
    let host = host_from_url(host_url).unwrap_or_default();
    if host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
    {
        WorkerPlacement::LocalWorkstation
    } else {
        WorkerPlacement::IntranetSharedMedia
    }
}

fn host_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .trim();
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host.to_string());
    }
    Some(
        authority
            .rsplit_once('@')
            .map(|(_, rest)| rest)
            .unwrap_or(authority)
            .split(':')
            .next()
            .unwrap_or(authority)
            .to_string(),
    )
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
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

        fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
            thread::sleep(Duration::from_millis(700));
            Ok(json!({
                "status": "ok",
                "job_type": job.job_type,
            }))
        }
    }

    #[test]
    fn worker_placement_detects_local_and_intranet_hosts() {
        assert_eq!(
            detect_worker_placement("http://127.0.0.1:8001"),
            WorkerPlacement::LocalWorkstation
        );
        assert_eq!(
            detect_worker_placement("http://localhost:8001"),
            WorkerPlacement::LocalWorkstation
        );
        assert_eq!(
            detect_worker_placement("http://192.168.1.20:8001"),
            WorkerPlacement::IntranetSharedMedia
        );
        assert_eq!(
            detect_worker_placement("http://qnc-host.local:8001"),
            WorkerPlacement::IntranetSharedMedia
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

        let result = handler.run(&job).unwrap();
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

        let result = handler.run(&job).unwrap();
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

        let result = handler.run(&job).unwrap();
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

        let result = handler.run(&job).unwrap();
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

        let result = handler.run(&job).unwrap();
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

        let result = handler.run(&job).unwrap();
        let decoded: MediaProbeJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.probe.width, 1920);
        assert_eq!(decoded.probe.timebase.fps_num, 50);
        assert_eq!(decoded.probe.duration_sec, Some(12.5));
    }
}
